//! x86_64 code generation via Cranelift.
//!
//! Maps C types to Cranelift IR types, generates function bodies,
//! handles the System V AMD64 ABI, and produces executable machine code.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use cranelift_codegen::ir::types::{I8, I16, I32, I64, F32, F64};
use cranelift_codegen::ir::{
    AbiParam, Block as ClifBlock, Function, InstBuilder, Signature,
    Type as ClifType, UserFuncName, Value,
};
use cranelift_codegen::isa::{self, CallConv};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};

use crate::ast::*;
use crate::sema::SemaContext;

/// Compiled function: machine code bytes + entry offset.
pub struct CompiledFunc {
    pub name: String,
    pub code: Vec<u8>,
}

/// Code generation context.
pub struct CodeGen<'a> {
    pub sema: &'a SemaContext,
    /// Variable counter for Cranelift.
    var_counter: u32,
    /// Map from C variable name to Cranelift Variable.
    var_map: BTreeMap<String, (Variable, ClifType)>,
    /// Break target block stack (for loops/switch).
    break_targets: Vec<ClifBlock>,
    /// Continue target block stack (for loops).
    continue_targets: Vec<ClifBlock>,
    /// String literals collected during codegen.
    pub string_literals: Vec<Vec<u8>>,
    /// Label blocks for goto.
    labels: BTreeMap<String, ClifBlock>,
}

impl<'a> CodeGen<'a> {
    pub fn new(sema: &'a SemaContext) -> Self {
        Self {
            sema,
            var_counter: 0,
            var_map: BTreeMap::new(),
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            string_literals: Vec::new(),
            labels: BTreeMap::new(),
        }
    }

    fn new_var(&mut self) -> Variable {
        let v = Variable::from_u32(self.var_counter);
        self.var_counter += 1;
        v
    }

    /// Map a C type to a Cranelift IR type.
    pub fn ctype_to_clif(ty: &CType) -> ClifType {
        match ty {
            CType::Void => I64, // void returns use i64 (convention)
            CType::Bool | CType::Char | CType::UChar => I8,
            CType::Short | CType::UShort => I16,
            CType::Int | CType::UInt | CType::Enum(_) => I32,
            CType::Long | CType::ULong | CType::LongLong | CType::ULongLong => I64,
            CType::Float => F32,
            CType::Double => F64,
            CType::Pointer(_) | CType::Array(_, _) | CType::FuncPtr { .. } => I64, // pointers are 64-bit
            CType::Struct(_) | CType::Union(_) => I64, // passed by pointer
            CType::TypedefName(_) => I64, // should be resolved
            CType::Const(inner) | CType::Volatile(inner) => Self::ctype_to_clif(inner),
        }
    }

    /// Compile a single function definition to machine code.
    pub fn compile_function(&mut self, func: &FuncDef) -> Result<CompiledFunc, String> {
        log::debug!("[cc] compiling function: {}", func.name);

        // Create ISA
        let mut flag_builder = settings::builder();
        flag_builder.set("opt_level", "speed").unwrap();
        let flags = settings::Flags::new(flag_builder);
        let isa = isa::lookup_by_name("x86_64")
            .map_err(|e| alloc::format!("ISA lookup: {:?}", e))?
            .finish(flags)
            .map_err(|e| alloc::format!("ISA finish: {:?}", e))?;

        // Build function signature (System V AMD64 ABI)
        let mut sig = Signature::new(CallConv::SystemV);
        for param in &func.params {
            sig.params.push(AbiParam::new(Self::ctype_to_clif(&param.ty)));
        }
        if !matches!(func.return_type, CType::Void) {
            sig.returns.push(AbiParam::new(Self::ctype_to_clif(&func.return_type)));
        }

        let mut clif_func = Function::with_name_signature(UserFuncName::default(), sig);
        let mut builder_ctx = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut clif_func, &mut builder_ctx);

        // Entry block
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);

        // Reset codegen state
        self.var_counter = 0;
        self.var_map.clear();
        self.break_targets.clear();
        self.continue_targets.clear();
        self.labels.clear();

        // Declare parameters as variables
        for (i, param) in func.params.iter().enumerate() {
            if let Some(ref name) = param.name {
                let clif_ty = Self::ctype_to_clif(&param.ty);
                let var = self.new_var();
                builder.declare_var(var, clif_ty);
                let param_val = builder.block_params(entry)[i];
                builder.def_var(var, param_val);
                self.var_map.insert(name.clone(), (var, clif_ty));
            }
        }

        // Compile function body
        let returned = self.compile_block(&func.body, &mut builder)?;

        // If the last block is not terminated, add a default return
        if !returned {
            if matches!(func.return_type, CType::Void) {
                builder.ins().return_(&[]);
            } else {
                let zero = builder.ins().iconst(Self::ctype_to_clif(&func.return_type), 0);
                builder.ins().return_(&[zero]);
            }
        }

        builder.seal_all_blocks();
        builder.finalize();

        // Compile to machine code
        let mut ctx = Context::for_function(clif_func);
        let compiled = ctx
            .compile(&*isa, &mut Default::default())
            .map_err(|e| alloc::format!("codegen: {:?}", e))?;

        let code = compiled.code_buffer().to_vec();
        log::info!("[cc] {} compiled to {} bytes", func.name, code.len());

        Ok(CompiledFunc {
            name: func.name.clone(),
            code,
        })
    }

    fn compile_block(&mut self, block: &Block, builder: &mut FunctionBuilder) -> Result<bool, String> {
        let mut returned = false;
        for stmt in &block.stmts {
            if returned {
                break;
            }
            returned = self.compile_stmt(stmt, builder)?;
        }
        Ok(returned)
    }

    fn compile_stmt(&mut self, stmt: &Stmt, builder: &mut FunctionBuilder) -> Result<bool, String> {
        match stmt {
            Stmt::Return(expr) => {
                if let Some(expr) = expr {
                    let val = self.compile_expr(expr, builder)?;
                    builder.ins().return_(&[val]);
                } else {
                    builder.ins().return_(&[]);
                }
                // Create a new unreachable block for any code after return
                let dead = builder.create_block();
                builder.switch_to_block(dead);
                Ok(true)
            }
            Stmt::Expr(expr) => {
                let _val = self.compile_expr(expr, builder)?;
                Ok(false)
            }
            Stmt::VarDecl(var) => {
                let clif_ty = Self::ctype_to_clif(&var.ty);
                let variable = self.new_var();
                builder.declare_var(variable, clif_ty);
                if let Some(ref init) = var.init {
                    let val = self.compile_expr(init, builder)?;
                    builder.def_var(variable, val);
                } else {
                    let zero = builder.ins().iconst(clif_ty, 0);
                    builder.def_var(variable, zero);
                }
                self.var_map.insert(var.name.clone(), (variable, clif_ty));
                Ok(false)
            }
            Stmt::Block(block) => self.compile_block(block, builder),
            Stmt::If { cond, then_body, else_body } => {
                let cond_val = self.compile_expr(cond, builder)?;
                let then_block = builder.create_block();
                let else_block = builder.create_block();
                let merge_block = builder.create_block();

                builder.ins().brif(cond_val, then_block, &[], else_block, &[]);

                builder.switch_to_block(then_block);
                builder.seal_block(then_block);
                let then_ret = self.compile_stmt(then_body, builder)?;
                if !then_ret {
                    builder.ins().jump(merge_block, &[]);
                }

                builder.switch_to_block(else_block);
                builder.seal_block(else_block);
                if let Some(eb) = else_body {
                    let else_ret = self.compile_stmt(eb, builder)?;
                    if !else_ret {
                        builder.ins().jump(merge_block, &[]);
                    }
                } else {
                    builder.ins().jump(merge_block, &[]);
                }

                builder.switch_to_block(merge_block);
                builder.seal_block(merge_block);
                Ok(false)
            }
            Stmt::While { cond, body } => {
                let header = builder.create_block();
                let body_block = builder.create_block();
                let exit = builder.create_block();

                self.break_targets.push(exit);
                self.continue_targets.push(header);

                builder.ins().jump(header, &[]);

                builder.switch_to_block(header);
                let cond_val = self.compile_expr(cond, builder)?;
                builder.ins().brif(cond_val, body_block, &[], exit, &[]);

                builder.switch_to_block(body_block);
                builder.seal_block(body_block);
                let _ret = self.compile_stmt(body, builder)?;
                if !_ret {
                    builder.ins().jump(header, &[]);
                }

                builder.seal_block(header);
                builder.switch_to_block(exit);
                builder.seal_block(exit);

                self.break_targets.pop();
                self.continue_targets.pop();
                Ok(false)
            }
            Stmt::DoWhile { body, cond } => {
                let body_block = builder.create_block();
                let cond_block = builder.create_block();
                let exit = builder.create_block();

                self.break_targets.push(exit);
                self.continue_targets.push(cond_block);

                builder.ins().jump(body_block, &[]);

                builder.switch_to_block(body_block);
                let _ret = self.compile_stmt(body, builder)?;
                if !_ret {
                    builder.ins().jump(cond_block, &[]);
                }

                builder.switch_to_block(cond_block);
                builder.seal_block(cond_block);
                let cond_val = self.compile_expr(cond, builder)?;
                builder.ins().brif(cond_val, body_block, &[], exit, &[]);

                builder.seal_block(body_block);
                builder.switch_to_block(exit);
                builder.seal_block(exit);

                self.break_targets.pop();
                self.continue_targets.pop();
                Ok(false)
            }
            Stmt::For { init, cond, incr, body } => {
                // Init
                if let Some(init) = init {
                    self.compile_stmt(init, builder)?;
                }

                let header = builder.create_block();
                let body_block = builder.create_block();
                let incr_block = builder.create_block();
                let exit = builder.create_block();

                self.break_targets.push(exit);
                self.continue_targets.push(incr_block);

                builder.ins().jump(header, &[]);

                builder.switch_to_block(header);
                if let Some(cond) = cond {
                    let cond_val = self.compile_expr(cond, builder)?;
                    builder.ins().brif(cond_val, body_block, &[], exit, &[]);
                } else {
                    builder.ins().jump(body_block, &[]);
                }

                builder.switch_to_block(body_block);
                builder.seal_block(body_block);
                let _ret = self.compile_stmt(body, builder)?;
                if !_ret {
                    builder.ins().jump(incr_block, &[]);
                }

                builder.switch_to_block(incr_block);
                builder.seal_block(incr_block);
                if let Some(incr) = incr {
                    let _ = self.compile_expr(incr, builder)?;
                }
                builder.ins().jump(header, &[]);

                builder.seal_block(header);
                builder.switch_to_block(exit);
                builder.seal_block(exit);

                self.break_targets.pop();
                self.continue_targets.pop();
                Ok(false)
            }
            Stmt::Break => {
                if let Some(&target) = self.break_targets.last() {
                    builder.ins().jump(target, &[]);
                    let dead = builder.create_block();
                    builder.switch_to_block(dead);
                }
                Ok(false)
            }
            Stmt::Continue => {
                if let Some(&target) = self.continue_targets.last() {
                    builder.ins().jump(target, &[]);
                    let dead = builder.create_block();
                    builder.switch_to_block(dead);
                }
                Ok(false)
            }
            Stmt::Empty => Ok(false),
            _ => {
                // Switch, case, goto, label — simplified: treat as no-op
                log::warn!("[cc] unimplemented statement kind");
                Ok(false)
            }
        }
    }

    fn compile_expr(&mut self, expr: &Expr, builder: &mut FunctionBuilder) -> Result<Value, String> {
        match expr {
            Expr::IntLit(v) => {
                Ok(builder.ins().iconst(I64, *v))
            }
            Expr::FloatLit(v) => {
                Ok(builder.ins().f64const(*v))
            }
            Expr::CharLit(v) => {
                Ok(builder.ins().iconst(I8, *v as i64))
            }
            Expr::StringLit(bytes) => {
                // Store string literal and return its index (to be resolved later)
                let idx = self.string_literals.len();
                let mut data = bytes.clone();
                data.push(0); // null terminator
                self.string_literals.push(data);
                // Return a placeholder pointer (index)
                Ok(builder.ins().iconst(I64, idx as i64))
            }
            Expr::Ident(name) => {
                // Check enum constants first
                if let Some(&val) = self.sema.enum_values.get(name) {
                    return Ok(builder.ins().iconst(I32, val));
                }
                if let Some(&(var, _ty)) = self.var_map.get(name) {
                    Ok(builder.use_var(var))
                } else {
                    // Unknown variable — return 0
                    log::warn!("[cc] undefined variable: {}", name);
                    Ok(builder.ins().iconst(I64, 0))
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let l = self.compile_expr(lhs, builder)?;
                let r = self.compile_expr(rhs, builder)?;
                let result = match op {
                    BinOp::Add => builder.ins().iadd(l, r),
                    BinOp::Sub => builder.ins().isub(l, r),
                    BinOp::Mul => builder.ins().imul(l, r),
                    BinOp::Div => builder.ins().sdiv(l, r),
                    BinOp::Mod => builder.ins().srem(l, r),
                    BinOp::BitAnd => builder.ins().band(l, r),
                    BinOp::BitOr => builder.ins().bor(l, r),
                    BinOp::BitXor => builder.ins().bxor(l, r),
                    BinOp::Shl => builder.ins().ishl(l, r),
                    BinOp::Shr => builder.ins().sshr(l, r),
                    BinOp::Eq => builder.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, l, r),
                    BinOp::Ne => builder.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::NotEqual, l, r),
                    BinOp::Lt => builder.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::SignedLessThan, l, r),
                    BinOp::Le => builder.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::SignedLessThanOrEqual, l, r),
                    BinOp::Gt => builder.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThan, l, r),
                    BinOp::Ge => builder.ins().icmp(cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThanOrEqual, l, r),
                    BinOp::LogAnd => {
                        // Short-circuit: l != 0 && r != 0
                        let l_bool = builder.ins().icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, l, 0);
                        let r_bool = builder.ins().icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, r, 0);
                        builder.ins().band(l_bool, r_bool)
                    }
                    BinOp::LogOr => {
                        let l_bool = builder.ins().icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, l, 0);
                        let r_bool = builder.ins().icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, r, 0);
                        builder.ins().bor(l_bool, r_bool)
                    }
                };
                Ok(result)
            }
            Expr::Unary { op, operand } => {
                let val = self.compile_expr(operand, builder)?;
                let result = match op {
                    UnaryOp::Neg => builder.ins().ineg(val),
                    UnaryOp::BitNot => builder.ins().bnot(val),
                    UnaryOp::LogNot => {
                        builder.ins().icmp_imm(cranelift_codegen::ir::condcodes::IntCC::Equal, val, 0)
                    }
                };
                Ok(result)
            }
            Expr::Assign { op, lhs, rhs } => {
                let rhs_val = self.compile_expr(rhs, builder)?;
                let name = match **lhs {
                    Expr::Ident(ref n) => n.clone(),
                    _ => return Err(String::from("assignment to non-lvalue")),
                };
                let val = if matches!(op, AssignOp::Assign) {
                    rhs_val
                } else {
                    let lhs_val = self.compile_expr(lhs, builder)?;
                    match op {
                        AssignOp::AddAssign => builder.ins().iadd(lhs_val, rhs_val),
                        AssignOp::SubAssign => builder.ins().isub(lhs_val, rhs_val),
                        AssignOp::MulAssign => builder.ins().imul(lhs_val, rhs_val),
                        AssignOp::DivAssign => builder.ins().sdiv(lhs_val, rhs_val),
                        AssignOp::ModAssign => builder.ins().srem(lhs_val, rhs_val),
                        AssignOp::AndAssign => builder.ins().band(lhs_val, rhs_val),
                        AssignOp::OrAssign => builder.ins().bor(lhs_val, rhs_val),
                        AssignOp::XorAssign => builder.ins().bxor(lhs_val, rhs_val),
                        AssignOp::ShlAssign => builder.ins().ishl(lhs_val, rhs_val),
                        AssignOp::ShrAssign => builder.ins().sshr(lhs_val, rhs_val),
                        _ => unreachable!(),
                    }
                };
                if let Some(&(var, _)) = self.var_map.get(&name) {
                    builder.def_var(var, val);
                }
                Ok(val)
            }
            Expr::Call { func, args } => {
                // For now, compile args and return 0 (external calls need linking)
                let _arg_vals: Vec<Value> = args
                    .iter()
                    .map(|a| self.compile_expr(a, builder))
                    .collect::<Result<_, _>>()?;
                log::debug!("[cc] function call (external linking not yet wired)");
                Ok(builder.ins().iconst(I64, 0))
            }
            Expr::PostIncr(operand) => {
                let val = self.compile_expr(operand, builder)?;
                if let Expr::Ident(ref name) = **operand {
                    let new_val = builder.ins().iadd_imm(val, 1);
                    if let Some(&(var, _)) = self.var_map.get(name) {
                        builder.def_var(var, new_val);
                    }
                }
                Ok(val) // return old value
            }
            Expr::PostDecr(operand) => {
                let val = self.compile_expr(operand, builder)?;
                if let Expr::Ident(ref name) = **operand {
                    let new_val = builder.ins().iadd_imm(val, -1);
                    if let Some(&(var, _)) = self.var_map.get(name) {
                        builder.def_var(var, new_val);
                    }
                }
                Ok(val)
            }
            Expr::PreIncr(operand) => {
                let val = self.compile_expr(operand, builder)?;
                let new_val = builder.ins().iadd_imm(val, 1);
                if let Expr::Ident(ref name) = **operand {
                    if let Some(&(var, _)) = self.var_map.get(name) {
                        builder.def_var(var, new_val);
                    }
                }
                Ok(new_val)
            }
            Expr::PreDecr(operand) => {
                let val = self.compile_expr(operand, builder)?;
                let new_val = builder.ins().iadd_imm(val, -1);
                if let Expr::Ident(ref name) = **operand {
                    if let Some(&(var, _)) = self.var_map.get(name) {
                        builder.def_var(var, new_val);
                    }
                }
                Ok(new_val)
            }
            Expr::Cast { ty, expr } => {
                let val = self.compile_expr(expr, builder)?;
                // Simplified cast — truncate or extend
                let target = Self::ctype_to_clif(ty);
                Ok(val) // TODO: proper casts
            }
            Expr::SizeofType(ty) => {
                let size = self.sema.type_size(ty);
                Ok(builder.ins().iconst(I64, size as i64))
            }
            Expr::SizeofExpr(_) => {
                // Would need type inference; return 8 as default
                Ok(builder.ins().iconst(I64, 8))
            }
            Expr::Ternary { cond, then_expr, else_expr } => {
                let cond_val = self.compile_expr(cond, builder)?;
                let then_val = self.compile_expr(then_expr, builder)?;
                let else_val = self.compile_expr(else_expr, builder)?;
                Ok(builder.ins().select(cond_val, then_val, else_val))
            }
            _ => {
                // Deref, AddrOf, Member, ArrowMember, Index, etc.
                // Return 0 for unimplemented
                log::warn!("[cc] unimplemented expression kind");
                Ok(builder.ins().iconst(I64, 0))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::parse_tokens;
    use crate::sema::SemaContext;

    #[test]
    fn test_compile_simple_function() {
        let tokens = tokenize("int add(int a, int b) { return a + b; }").unwrap();
        let ast = parse_tokens(&tokens).unwrap();
        let mut sema = SemaContext::new();
        sema.analyze(&ast);
        let mut codegen = CodeGen::new(&sema);
        if let ExternalDecl::FuncDef(ref func) = ast.decls[0] {
            let result = codegen.compile_function(func);
            assert!(result.is_ok());
            let compiled = result.unwrap();
            assert!(compiled.code.len() > 0);
        }
    }
}
