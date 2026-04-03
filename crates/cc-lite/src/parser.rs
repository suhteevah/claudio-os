//! Recursive descent C parser producing AST.
//!
//! Handles full C operator precedence (15 levels), declarations, statements,
//! structs, unions, enums, typedefs, arrays, pointers, function definitions.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ast::*;
use crate::lexer::{Token, TokenKind, Span};

/// Parser state.
pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    /// Typedef names known so far (needed to disambiguate types vs identifiers).
    typedef_names: Vec<String>,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self {
            tokens,
            pos: 0,
            typedef_names: Vec::new(),
        }
    }

    fn peek(&self) -> &TokenKind {
        self.tokens.get(self.pos).map(|t| &t.kind).unwrap_or(&TokenKind::Eof)
    }

    fn span(&self) -> Span {
        self.tokens.get(self.pos).map(|t| t.span).unwrap_or_default()
    }

    fn advance(&mut self) -> &TokenKind {
        let tok = &self.tokens[self.pos].kind;
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<(), String> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(alloc::format!(
                "{}:{}: expected {:?}, got {:?}",
                self.span().line,
                self.span().col,
                expected,
                self.peek()
            ))
        }
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek() == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        if let TokenKind::Ident(ref name) = *self.peek() {
            let name = name.clone();
            self.advance();
            Ok(name)
        } else {
            Err(alloc::format!(
                "{}:{}: expected identifier, got {:?}",
                self.span().line,
                self.span().col,
                self.peek()
            ))
        }
    }

    fn is_type_start(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Void | TokenKind::Char | TokenKind::Short | TokenKind::Int
            | TokenKind::Long | TokenKind::Float | TokenKind::Double
            | TokenKind::Signed | TokenKind::Unsigned | TokenKind::Struct
            | TokenKind::Union | TokenKind::Enum | TokenKind::Const
            | TokenKind::Volatile | TokenKind::Static | TokenKind::Extern
            | TokenKind::Inline | TokenKind::Bool | TokenKind::Typedef
        ) || self.is_typedef_name()
    }

    fn is_typedef_name(&self) -> bool {
        if let TokenKind::Ident(ref name) = *self.peek() {
            self.typedef_names.iter().any(|n| n == name)
        } else {
            false
        }
    }

    /// Parse a complete translation unit.
    pub fn parse_translation_unit(&mut self) -> Result<TranslationUnit, String> {
        let mut decls = Vec::new();

        // Skip preprocessor tokens (handled at a higher level)
        while !matches!(self.peek(), TokenKind::Eof) {
            self.skip_preprocessor();
            if matches!(self.peek(), TokenKind::Eof) {
                break;
            }
            let decl = self.parse_external_decl()?;
            decls.push(decl);
        }

        Ok(TranslationUnit { decls })
    }

    fn skip_preprocessor(&mut self) {
        while matches!(
            self.peek(),
            TokenKind::PpInclude(_) | TokenKind::PpDefine(_, _)
            | TokenKind::PpIfdef(_) | TokenKind::PpIfndef(_)
            | TokenKind::PpEndif | TokenKind::PpIf | TokenKind::PpElif
            | TokenKind::PpElse | TokenKind::PpPragma(_) | TokenKind::PpError(_)
        ) {
            self.advance();
        }
    }

    fn parse_external_decl(&mut self) -> Result<ExternalDecl, String> {
        let span = self.span();

        // Typedef
        if matches!(self.peek(), TokenKind::Typedef) {
            return self.parse_typedef();
        }

        // Struct definition (standalone)
        if matches!(self.peek(), TokenKind::Struct) {
            // Look ahead to see if this is `struct Name { ... };` (definition only)
            let saved = self.pos;
            self.advance(); // eat struct
            let name = if let TokenKind::Ident(_) = self.peek() {
                let n = self.expect_ident()?;
                Some(n)
            } else {
                None
            };
            if matches!(self.peek(), TokenKind::LBrace) {
                let fields = self.parse_struct_fields()?;
                if matches!(self.peek(), TokenKind::Semicolon) {
                    self.advance();
                    return Ok(ExternalDecl::StructDef(StructDef { name, fields, span }));
                }
                // Else it's a variable/function declaration with struct type
            }
            self.pos = saved; // backtrack
        }

        // Enum definition
        if matches!(self.peek(), TokenKind::Enum) {
            let saved = self.pos;
            self.advance();
            let name = if let TokenKind::Ident(_) = self.peek() {
                Some(self.expect_ident()?)
            } else {
                None
            };
            if matches!(self.peek(), TokenKind::LBrace) {
                let variants = self.parse_enum_variants()?;
                if matches!(self.peek(), TokenKind::Semicolon) {
                    self.advance();
                    return Ok(ExternalDecl::EnumDef(EnumDef { name, variants, span }));
                }
            }
            self.pos = saved;
        }

        // Parse type specifiers and declarator
        let (base_ty, is_static, is_extern) = self.parse_declaration_specifiers()?;
        let (ty, name) = self.parse_declarator(base_ty.clone())?;

        // Check for function definition: type name(...) { ... }
        if matches!(self.peek(), TokenKind::LParen) {
            self.advance();
            let (params, variadic) = self.parse_param_list()?;
            self.expect(&TokenKind::RParen)?;

            if matches!(self.peek(), TokenKind::LBrace) {
                // Function definition
                let body = self.parse_block()?;
                return Ok(ExternalDecl::FuncDef(FuncDef {
                    return_type: ty,
                    name,
                    params,
                    is_variadic: variadic,
                    body,
                    span,
                }));
            } else {
                // Function declaration (prototype)
                self.expect(&TokenKind::Semicolon)?;
                return Ok(ExternalDecl::VarDecl(VarDecl {
                    ty: CType::FuncPtr {
                        ret: Box::new(ty),
                        params: params.iter().map(|p| p.ty.clone()).collect(),
                        variadic,
                    },
                    name,
                    init: None,
                    is_static,
                    is_extern,
                    span,
                }));
            }
        }

        // Variable declaration with optional initializer
        let init = if self.eat(&TokenKind::Assign) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::Semicolon)?;
        Ok(ExternalDecl::VarDecl(VarDecl {
            ty,
            name,
            init,
            is_static,
            is_extern,
            span,
        }))
    }

    fn parse_typedef(&mut self) -> Result<ExternalDecl, String> {
        let span = self.span();
        self.advance(); // eat 'typedef'
        let (base_ty, _, _) = self.parse_declaration_specifiers()?;
        let (ty, name) = self.parse_declarator(base_ty)?;
        self.typedef_names.push(name.clone());
        self.expect(&TokenKind::Semicolon)?;
        Ok(ExternalDecl::TypedefDecl(TypedefDecl {
            original: ty,
            new_name: name,
            span,
        }))
    }

    fn parse_declaration_specifiers(&mut self) -> Result<(CType, bool, bool), String> {
        let mut is_static = false;
        let mut is_extern = false;
        let mut is_const = false;
        let mut is_volatile = false;
        let mut _is_signed = true;
        let mut explicit_sign = false;
        let mut is_unsigned = false;
        let mut base = None;
        let mut long_count = 0u8;

        loop {
            match self.peek() {
                TokenKind::Static => { is_static = true; self.advance(); }
                TokenKind::Extern => { is_extern = true; self.advance(); }
                TokenKind::Const => { is_const = true; self.advance(); }
                TokenKind::Volatile => { is_volatile = true; self.advance(); }
                TokenKind::Inline => { self.advance(); } // ignored
                TokenKind::Signed => { _is_signed = true; explicit_sign = true; self.advance(); }
                TokenKind::Unsigned => { is_unsigned = true; _is_signed = false; explicit_sign = true; self.advance(); }
                TokenKind::Void => { base = Some(CType::Void); self.advance(); break; }
                TokenKind::Bool => { base = Some(CType::Bool); self.advance(); break; }
                TokenKind::Char => { base = Some(if is_unsigned { CType::UChar } else { CType::Char }); self.advance(); break; }
                TokenKind::Short => { base = Some(if is_unsigned { CType::UShort } else { CType::Short }); self.advance(); break; }
                TokenKind::Int => { base = Some(if is_unsigned { CType::UInt } else { CType::Int }); self.advance(); break; }
                TokenKind::Float => { base = Some(CType::Float); self.advance(); break; }
                TokenKind::Double => { base = Some(CType::Double); self.advance(); break; }
                TokenKind::Long => {
                    long_count += 1;
                    self.advance();
                    // Check for 'long long', 'long int', 'long double'
                    if matches!(self.peek(), TokenKind::Long) {
                        long_count += 1;
                        self.advance();
                    }
                    if matches!(self.peek(), TokenKind::Double) {
                        base = Some(CType::Double); // long double = double for us
                        self.advance();
                    } else if matches!(self.peek(), TokenKind::Int) {
                        self.advance(); // consume 'int'
                    }
                    if base.is_none() {
                        if long_count >= 2 {
                            base = Some(if is_unsigned { CType::ULongLong } else { CType::LongLong });
                        } else {
                            base = Some(if is_unsigned { CType::ULong } else { CType::Long });
                        }
                    }
                    break;
                }
                TokenKind::Struct => {
                    self.advance();
                    let name = self.expect_ident()?;
                    if matches!(self.peek(), TokenKind::LBrace) {
                        // Struct definition with body — just consume and use name
                        let _fields = self.parse_struct_fields()?;
                    }
                    base = Some(CType::Struct(name));
                    break;
                }
                TokenKind::Union => {
                    self.advance();
                    let name = self.expect_ident()?;
                    if matches!(self.peek(), TokenKind::LBrace) {
                        let _fields = self.parse_struct_fields()?;
                    }
                    base = Some(CType::Union(name));
                    break;
                }
                TokenKind::Enum => {
                    self.advance();
                    let name = self.expect_ident()?;
                    if matches!(self.peek(), TokenKind::LBrace) {
                        let _variants = self.parse_enum_variants()?;
                    }
                    base = Some(CType::Enum(name));
                    break;
                }
                _ if self.is_typedef_name() => {
                    if let TokenKind::Ident(ref name) = *self.peek() {
                        base = Some(CType::TypedefName(name.clone()));
                        self.advance();
                        break;
                    }
                    break;
                }
                _ => break,
            }
        }

        // Default: if only sign specifier, it's int
        let mut ty = base.unwrap_or(if explicit_sign {
            if is_unsigned { CType::UInt } else { CType::Int }
        } else {
            CType::Int
        });

        if is_const {
            ty = CType::Const(Box::new(ty));
        }
        if is_volatile {
            ty = CType::Volatile(Box::new(ty));
        }

        Ok((ty, is_static, is_extern))
    }

    fn parse_declarator(&mut self, base: CType) -> Result<(CType, String), String> {
        let mut ty = base;
        // Pointer(s)
        while self.eat(&TokenKind::Star) {
            ty = CType::Pointer(Box::new(ty));
            if self.eat(&TokenKind::Const) {
                ty = CType::Const(Box::new(ty));
            }
        }
        let name = self.expect_ident()?;
        // Array dimensions
        while self.eat(&TokenKind::LBracket) {
            if let TokenKind::IntLit(n) = *self.peek() {
                self.advance();
                ty = CType::Array(Box::new(ty), Some(n as usize));
            } else {
                ty = CType::Array(Box::new(ty), None);
            }
            self.expect(&TokenKind::RBracket)?;
        }
        Ok((ty, name))
    }

    fn parse_param_list(&mut self) -> Result<(Vec<Param>, bool), String> {
        let mut params = Vec::new();
        let mut variadic = false;

        if matches!(self.peek(), TokenKind::RParen) {
            return Ok((params, false));
        }

        // void params
        if matches!(self.peek(), TokenKind::Void) {
            let saved = self.pos;
            self.advance();
            if matches!(self.peek(), TokenKind::RParen) {
                return Ok((params, false));
            }
            self.pos = saved;
        }

        loop {
            if self.eat(&TokenKind::Ellipsis) {
                variadic = true;
                break;
            }
            let (base_ty, _, _) = self.parse_declaration_specifiers()?;
            let name = if let TokenKind::Ident(_) = self.peek() {
                // Has a name
                let mut ty = base_ty;
                while self.eat(&TokenKind::Star) {
                    ty = CType::Pointer(Box::new(ty));
                }
                let n = self.expect_ident()?;
                // Array params decay to pointers
                while self.eat(&TokenKind::LBracket) {
                    if let TokenKind::IntLit(_) = self.peek() {
                        self.advance();
                    }
                    self.expect(&TokenKind::RBracket)?;
                    ty = CType::Pointer(Box::new(ty));
                }
                params.push(Param { ty, name: Some(n) });
            } else if matches!(self.peek(), TokenKind::Star) {
                let mut ty = base_ty;
                while self.eat(&TokenKind::Star) {
                    ty = CType::Pointer(Box::new(ty));
                }
                if let TokenKind::Ident(_) = self.peek() {
                    let n = self.expect_ident()?;
                    params.push(Param { ty, name: Some(n) });
                } else {
                    params.push(Param { ty, name: None });
                }
            } else {
                params.push(Param { ty: base_ty, name: None });
            };

            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }

        Ok((params, variadic))
    }

    fn parse_struct_fields(&mut self) -> Result<Vec<StructField>, String> {
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let (base_ty, _, _) = self.parse_declaration_specifiers()?;
            let (ty, name) = self.parse_declarator(base_ty)?;
            fields.push(StructField { ty, name });
            self.expect(&TokenKind::Semicolon)?;
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(fields)
    }

    fn parse_enum_variants(&mut self) -> Result<Vec<EnumVariant>, String> {
        self.expect(&TokenKind::LBrace)?;
        let mut variants = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let name = self.expect_ident()?;
            let value = if self.eat(&TokenKind::Assign) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            variants.push(EnumVariant { name, value });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(variants)
    }

    fn parse_block(&mut self) -> Result<Block, String> {
        self.expect(&TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Block { stmts })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        match self.peek() {
            TokenKind::LBrace => {
                let block = self.parse_block()?;
                Ok(Stmt::Block(block))
            }
            TokenKind::Return => {
                self.advance();
                let expr = if !matches!(self.peek(), TokenKind::Semicolon) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                self.expect(&TokenKind::Semicolon)?;
                Ok(Stmt::Return(expr))
            }
            TokenKind::If => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let cond = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                let then_body = Box::new(self.parse_stmt()?);
                let else_body = if self.eat(&TokenKind::Else) {
                    Some(Box::new(self.parse_stmt()?))
                } else {
                    None
                };
                Ok(Stmt::If { cond, then_body, else_body })
            }
            TokenKind::While => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let cond = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                let body = Box::new(self.parse_stmt()?);
                Ok(Stmt::While { cond, body })
            }
            TokenKind::Do => {
                self.advance();
                let body = Box::new(self.parse_stmt()?);
                self.expect(&TokenKind::While)?;
                self.expect(&TokenKind::LParen)?;
                let cond = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                self.expect(&TokenKind::Semicolon)?;
                Ok(Stmt::DoWhile { body, cond })
            }
            TokenKind::For => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let init = if matches!(self.peek(), TokenKind::Semicolon) {
                    self.advance();
                    None
                } else if self.is_type_start() {
                    let s = self.parse_local_var_decl()?;
                    // semicolon already consumed in local var decl
                    Some(Box::new(s))
                } else {
                    let expr = self.parse_expr()?;
                    self.expect(&TokenKind::Semicolon)?;
                    Some(Box::new(Stmt::Expr(expr)))
                };
                let cond = if matches!(self.peek(), TokenKind::Semicolon) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect(&TokenKind::Semicolon)?;
                let incr = if matches!(self.peek(), TokenKind::RParen) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect(&TokenKind::RParen)?;
                let body = Box::new(self.parse_stmt()?);
                Ok(Stmt::For { init, cond, incr, body })
            }
            TokenKind::Switch => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                let body = Box::new(self.parse_stmt()?);
                Ok(Stmt::Switch { expr, body })
            }
            TokenKind::Case => {
                self.advance();
                let value = self.parse_expr()?;
                self.expect(&TokenKind::Colon)?;
                let body = Box::new(self.parse_stmt()?);
                Ok(Stmt::Case { value, body })
            }
            TokenKind::Default => {
                self.advance();
                self.expect(&TokenKind::Colon)?;
                let body = Box::new(self.parse_stmt()?);
                Ok(Stmt::Default(body))
            }
            TokenKind::Break => {
                self.advance();
                self.expect(&TokenKind::Semicolon)?;
                Ok(Stmt::Break)
            }
            TokenKind::Continue => {
                self.advance();
                self.expect(&TokenKind::Semicolon)?;
                Ok(Stmt::Continue)
            }
            TokenKind::Goto => {
                self.advance();
                let label = self.expect_ident()?;
                self.expect(&TokenKind::Semicolon)?;
                Ok(Stmt::Goto(label))
            }
            TokenKind::Semicolon => {
                self.advance();
                Ok(Stmt::Empty)
            }
            _ if self.is_type_start() => {
                self.parse_local_var_decl()
            }
            _ => {
                // Check for label: ident ':'
                if let TokenKind::Ident(_) = self.peek() {
                    let saved = self.pos;
                    let name = self.expect_ident().unwrap();
                    if self.eat(&TokenKind::Colon) {
                        let stmt = self.parse_stmt()?;
                        return Ok(Stmt::Label(name, Box::new(stmt)));
                    }
                    self.pos = saved;
                }
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::Semicolon)?;
                Ok(Stmt::Expr(expr))
            }
        }
    }

    fn parse_local_var_decl(&mut self) -> Result<Stmt, String> {
        let span = self.span();
        let (base_ty, is_static, is_extern) = self.parse_declaration_specifiers()?;
        let (ty, name) = self.parse_declarator(base_ty)?;
        let init = if self.eat(&TokenKind::Assign) {
            if matches!(self.peek(), TokenKind::LBrace) {
                // Initializer list
                Some(self.parse_init_list()?)
            } else {
                Some(self.parse_expr()?)
            }
        } else {
            None
        };
        self.expect(&TokenKind::Semicolon)?;
        Ok(Stmt::VarDecl(VarDecl { ty, name, init, is_static, is_extern, span }))
    }

    fn parse_init_list(&mut self) -> Result<Expr, String> {
        self.expect(&TokenKind::LBrace)?;
        let mut exprs = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            if matches!(self.peek(), TokenKind::LBrace) {
                exprs.push(self.parse_init_list()?);
            } else {
                exprs.push(self.parse_expr()?);
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr::InitList(exprs))
    }

    // === Expression parsing: precedence climbing ===

    pub fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, String> {
        let lhs = self.parse_ternary()?;

        let op = match self.peek() {
            TokenKind::Assign => AssignOp::Assign,
            TokenKind::PlusAssign => AssignOp::AddAssign,
            TokenKind::MinusAssign => AssignOp::SubAssign,
            TokenKind::StarAssign => AssignOp::MulAssign,
            TokenKind::SlashAssign => AssignOp::DivAssign,
            TokenKind::PercentAssign => AssignOp::ModAssign,
            TokenKind::AmpAssign => AssignOp::AndAssign,
            TokenKind::PipeAssign => AssignOp::OrAssign,
            TokenKind::CaretAssign => AssignOp::XorAssign,
            TokenKind::ShlAssign => AssignOp::ShlAssign,
            TokenKind::ShrAssign => AssignOp::ShrAssign,
            _ => return Ok(lhs),
        };
        self.advance();
        let rhs = self.parse_assignment()?; // right-associative
        Ok(Expr::Assign {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        })
    }

    fn parse_ternary(&mut self) -> Result<Expr, String> {
        let cond = self.parse_log_or()?;
        if self.eat(&TokenKind::Question) {
            let then_expr = self.parse_expr()?;
            self.expect(&TokenKind::Colon)?;
            let else_expr = self.parse_ternary()?;
            Ok(Expr::Ternary {
                cond: Box::new(cond),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            })
        } else {
            Ok(cond)
        }
    }

    fn parse_log_or(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_log_and()?;
        while self.eat(&TokenKind::PipePipe) {
            let rhs = self.parse_log_and()?;
            lhs = Expr::Binary { op: BinOp::LogOr, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_log_and(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_bit_or()?;
        while self.eat(&TokenKind::AmpAmp) {
            let rhs = self.parse_bit_or()?;
            lhs = Expr::Binary { op: BinOp::LogAnd, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_bit_or(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_bit_xor()?;
        while self.eat(&TokenKind::Pipe) {
            let rhs = self.parse_bit_xor()?;
            lhs = Expr::Binary { op: BinOp::BitOr, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_bit_xor(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_bit_and()?;
        while self.eat(&TokenKind::Caret) {
            let rhs = self.parse_bit_and()?;
            lhs = Expr::Binary { op: BinOp::BitXor, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_bit_and(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_equality()?;
        while self.eat(&TokenKind::Amp) {
            let rhs = self.parse_equality()?;
            lhs = Expr::Binary { op: BinOp::BitAnd, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_equality(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_relational()?;
        loop {
            let op = match self.peek() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::Ne => BinOp::Ne,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_relational()?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_relational(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_shift()?;
        loop {
            let op = match self.peek() {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Le => BinOp::Le,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_shift()?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_shift(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                TokenKind::Shl => BinOp::Shl,
                TokenKind::Shr => BinOp::Shr,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_additive()?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_multiplicative()?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_cast()?;
        loop {
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_cast()?;
            lhs = Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) };
        }
        Ok(lhs)
    }

    fn parse_cast(&mut self) -> Result<Expr, String> {
        // Check for (type)expr
        if matches!(self.peek(), TokenKind::LParen) {
            let saved = self.pos;
            self.advance();
            if self.is_type_start() {
                let (ty, _, _) = self.parse_declaration_specifiers()?;
                // Handle pointer in cast
                let mut cast_ty = ty;
                while self.eat(&TokenKind::Star) {
                    cast_ty = CType::Pointer(Box::new(cast_ty));
                }
                if self.eat(&TokenKind::RParen) {
                    let expr = self.parse_cast()?;
                    return Ok(Expr::Cast {
                        ty: cast_ty,
                        expr: Box::new(expr),
                    });
                }
            }
            self.pos = saved;
        }
        self.parse_unary()
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            TokenKind::PlusPlus => {
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::PreIncr(Box::new(operand)))
            }
            TokenKind::MinusMinus => {
                self.advance();
                let operand = self.parse_unary()?;
                Ok(Expr::PreDecr(Box::new(operand)))
            }
            TokenKind::Amp => {
                self.advance();
                let operand = self.parse_cast()?;
                Ok(Expr::AddrOf(Box::new(operand)))
            }
            TokenKind::Star => {
                self.advance();
                let operand = self.parse_cast()?;
                Ok(Expr::Deref(Box::new(operand)))
            }
            TokenKind::Minus => {
                self.advance();
                let operand = self.parse_cast()?;
                Ok(Expr::Unary { op: UnaryOp::Neg, operand: Box::new(operand) })
            }
            TokenKind::Tilde => {
                self.advance();
                let operand = self.parse_cast()?;
                Ok(Expr::Unary { op: UnaryOp::BitNot, operand: Box::new(operand) })
            }
            TokenKind::Bang => {
                self.advance();
                let operand = self.parse_cast()?;
                Ok(Expr::Unary { op: UnaryOp::LogNot, operand: Box::new(operand) })
            }
            TokenKind::Sizeof => {
                self.advance();
                if matches!(self.peek(), TokenKind::LParen) {
                    let saved = self.pos;
                    self.advance();
                    if self.is_type_start() {
                        let (ty, _, _) = self.parse_declaration_specifiers()?;
                        let mut st = ty;
                        while self.eat(&TokenKind::Star) {
                            st = CType::Pointer(Box::new(st));
                        }
                        self.expect(&TokenKind::RParen)?;
                        return Ok(Expr::SizeofType(st));
                    }
                    self.pos = saved;
                }
                let operand = self.parse_unary()?;
                Ok(Expr::SizeofExpr(Box::new(operand)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_primary()?;

        loop {
            match self.peek() {
                TokenKind::LParen => {
                    // Function call
                    self.advance();
                    let mut args = Vec::new();
                    while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                        args.push(self.parse_expr()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen)?;
                    expr = Expr::Call { func: Box::new(expr), args };
                }
                TokenKind::LBracket => {
                    // Array subscript
                    self.advance();
                    let index = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket)?;
                    expr = Expr::Index { array: Box::new(expr), index: Box::new(index) };
                }
                TokenKind::Dot => {
                    self.advance();
                    let field = self.expect_ident()?;
                    expr = Expr::Member { object: Box::new(expr), field };
                }
                TokenKind::Arrow => {
                    self.advance();
                    let field = self.expect_ident()?;
                    expr = Expr::ArrowMember { object: Box::new(expr), field };
                }
                TokenKind::PlusPlus => {
                    self.advance();
                    expr = Expr::PostIncr(Box::new(expr));
                }
                TokenKind::MinusMinus => {
                    self.advance();
                    expr = Expr::PostDecr(Box::new(expr));
                }
                _ => break,
            }
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek().clone() {
            TokenKind::IntLit(v) => {
                let v = v;
                self.advance();
                Ok(Expr::IntLit(v))
            }
            TokenKind::FloatLit(v) => {
                let v = v;
                self.advance();
                Ok(Expr::FloatLit(v))
            }
            TokenKind::CharLit(v) => {
                let v = v;
                self.advance();
                Ok(Expr::CharLit(v))
            }
            TokenKind::StringLit(ref v) => {
                let v = v.clone();
                self.advance();
                Ok(Expr::StringLit(v))
            }
            TokenKind::Ident(ref name) => {
                let name = name.clone();
                self.advance();
                Ok(Expr::Ident(name))
            }
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect(&TokenKind::RParen)?;
                Ok(expr)
            }
            _ => Err(alloc::format!(
                "{}:{}: unexpected token in expression: {:?}",
                self.span().line,
                self.span().col,
                self.peek()
            )),
        }
    }
}

/// Parse C source tokens into an AST.
pub fn parse_tokens(tokens: &[Token]) -> Result<TranslationUnit, String> {
    let mut parser = Parser::new(tokens);
    parser.parse_translation_unit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    #[test]
    fn test_parse_simple_function() {
        let tokens = tokenize("int main() { return 0; }").unwrap();
        let ast = parse_tokens(&tokens).unwrap();
        assert_eq!(ast.decls.len(), 1);
        assert!(matches!(ast.decls[0], ExternalDecl::FuncDef(_)));
    }

    #[test]
    fn test_parse_variable() {
        let tokens = tokenize("int x = 42;").unwrap();
        let ast = parse_tokens(&tokens).unwrap();
        assert_eq!(ast.decls.len(), 1);
        assert!(matches!(ast.decls[0], ExternalDecl::VarDecl(_)));
    }

    #[test]
    fn test_parse_if_else() {
        let tokens = tokenize("int f() { if (x > 0) return 1; else return 0; }").unwrap();
        let ast = parse_tokens(&tokens).unwrap();
        assert_eq!(ast.decls.len(), 1);
    }

    #[test]
    fn test_parse_for_loop() {
        let tokens = tokenize("void f() { for (int i = 0; i < 10; i++) x++; }").unwrap();
        let ast = parse_tokens(&tokens).unwrap();
        assert_eq!(ast.decls.len(), 1);
    }

    #[test]
    fn test_parse_struct() {
        let tokens = tokenize("struct Point { int x; int y; };").unwrap();
        let ast = parse_tokens(&tokens).unwrap();
        assert!(matches!(ast.decls[0], ExternalDecl::StructDef(_)));
    }

    #[test]
    fn test_parse_pointer() {
        let tokens = tokenize("int *p = &x;").unwrap();
        let ast = parse_tokens(&tokens).unwrap();
        assert_eq!(ast.decls.len(), 1);
    }
}
