//! AST node types for all C constructs.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::lexer::Span;

/// A translation unit (one C source file).
#[derive(Debug, Clone)]
pub struct TranslationUnit {
    pub decls: Vec<ExternalDecl>,
}

/// Top-level declaration.
#[derive(Debug, Clone)]
pub enum ExternalDecl {
    FuncDef(FuncDef),
    VarDecl(VarDecl),
    StructDef(StructDef),
    UnionDef(UnionDef),
    EnumDef(EnumDef),
    TypedefDecl(TypedefDecl),
}

/// Function definition.
#[derive(Debug, Clone)]
pub struct FuncDef {
    pub return_type: CType,
    pub name: String,
    pub params: Vec<Param>,
    pub is_variadic: bool,
    pub body: Block,
    pub span: Span,
}

/// Function parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub ty: CType,
    pub name: Option<String>,
}

/// Variable declaration (can be global or local).
#[derive(Debug, Clone)]
pub struct VarDecl {
    pub ty: CType,
    pub name: String,
    pub init: Option<Expr>,
    pub is_static: bool,
    pub is_extern: bool,
    pub span: Span,
}

/// Struct definition.
#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: Option<String>,
    pub fields: Vec<StructField>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub ty: CType,
    pub name: String,
}

/// Union definition.
#[derive(Debug, Clone)]
pub struct UnionDef {
    pub name: Option<String>,
    pub fields: Vec<StructField>,
    pub span: Span,
}

/// Enum definition.
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: Option<String>,
    pub variants: Vec<EnumVariant>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub value: Option<Expr>,
}

/// Typedef declaration.
#[derive(Debug, Clone)]
pub struct TypedefDecl {
    pub original: CType,
    pub new_name: String,
    pub span: Span,
}

/// C type representation.
#[derive(Debug, Clone, PartialEq)]
pub enum CType {
    Void,
    Bool,
    Char,
    Short,
    Int,
    Long,
    LongLong,
    UChar,
    UShort,
    UInt,
    ULong,
    ULongLong,
    Float,
    Double,
    /// Pointer to another type.
    Pointer(Box<CType>),
    /// Array with optional size.
    Array(Box<CType>, Option<usize>),
    /// Named struct.
    Struct(String),
    /// Named union.
    Union(String),
    /// Named enum.
    Enum(String),
    /// Typedef name.
    TypedefName(String),
    /// Function pointer: return type + param types.
    FuncPtr {
        ret: Box<CType>,
        params: Vec<CType>,
        variadic: bool,
    },
    /// Const-qualified type.
    Const(Box<CType>),
    /// Volatile-qualified type.
    Volatile(Box<CType>),
}

impl CType {
    /// Size in bytes on x86_64.
    pub fn size(&self) -> usize {
        match self {
            CType::Void => 0,
            CType::Bool | CType::Char | CType::UChar => 1,
            CType::Short | CType::UShort => 2,
            CType::Int | CType::UInt | CType::Enum(_) | CType::Float => 4,
            CType::Long | CType::ULong | CType::LongLong | CType::ULongLong
            | CType::Double | CType::Pointer(_) | CType::FuncPtr { .. } => 8,
            CType::Array(inner, Some(n)) => inner.size() * n,
            CType::Array(_, None) => 8, // decay to pointer
            CType::Struct(_) | CType::Union(_) => 0, // looked up in sema
            CType::TypedefName(_) => 0, // resolved in sema
            CType::Const(inner) | CType::Volatile(inner) => inner.size(),
        }
    }

    /// Alignment in bytes.
    pub fn align(&self) -> usize {
        match self {
            CType::Void => 1,
            CType::Bool | CType::Char | CType::UChar => 1,
            CType::Short | CType::UShort => 2,
            CType::Int | CType::UInt | CType::Enum(_) | CType::Float => 4,
            CType::Long | CType::ULong | CType::LongLong | CType::ULongLong
            | CType::Double | CType::Pointer(_) | CType::FuncPtr { .. } => 8,
            CType::Array(inner, _) => inner.align(),
            CType::Struct(_) | CType::Union(_) => 8,
            CType::TypedefName(_) => 8,
            CType::Const(inner) | CType::Volatile(inner) => inner.align(),
        }
    }

    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            CType::Bool | CType::Char | CType::UChar | CType::Short | CType::UShort
            | CType::Int | CType::UInt | CType::Long | CType::ULong
            | CType::LongLong | CType::ULongLong | CType::Enum(_)
        )
    }

    pub fn is_float(&self) -> bool {
        matches!(self, CType::Float | CType::Double)
    }

    pub fn is_pointer(&self) -> bool {
        matches!(self, CType::Pointer(_))
    }

    pub fn is_signed(&self) -> bool {
        matches!(
            self,
            CType::Char | CType::Short | CType::Int | CType::Long | CType::LongLong
        )
    }
}

/// Statement node.
#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr),
    Return(Option<Expr>),
    If {
        cond: Expr,
        then_body: Box<Stmt>,
        else_body: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        cond: Expr,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        incr: Option<Expr>,
        body: Box<Stmt>,
    },
    Switch {
        expr: Expr,
        body: Box<Stmt>,
    },
    Case {
        value: Expr,
        body: Box<Stmt>,
    },
    Default(Box<Stmt>),
    Break,
    Continue,
    Goto(String),
    Label(String, Box<Stmt>),
    Block(Block),
    VarDecl(VarDecl),
    Empty,
}

/// A block (compound statement).
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

/// Expression node.
#[derive(Debug, Clone)]
pub enum Expr {
    IntLit(i64),
    FloatLit(f64),
    CharLit(u8),
    StringLit(Vec<u8>),
    Ident(String),

    /// Binary operation
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Unary operation
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    /// Assignment (=, +=, -=, etc.)
    Assign {
        op: AssignOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Function call
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
    },
    /// Array subscript: arr[idx]
    Index {
        array: Box<Expr>,
        index: Box<Expr>,
    },
    /// Member access: expr.field
    Member {
        object: Box<Expr>,
        field: String,
    },
    /// Arrow member access: expr->field
    ArrowMember {
        object: Box<Expr>,
        field: String,
    },
    /// Post-increment/decrement
    PostIncr(Box<Expr>),
    PostDecr(Box<Expr>),
    /// Pre-increment/decrement
    PreIncr(Box<Expr>),
    PreDecr(Box<Expr>),
    /// Cast expression: (type)expr
    Cast {
        ty: CType,
        expr: Box<Expr>,
    },
    /// sizeof(type) or sizeof(expr)
    SizeofType(CType),
    SizeofExpr(Box<Expr>),
    /// Ternary: cond ? then : else
    Ternary {
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    /// Address-of: &expr
    AddrOf(Box<Expr>),
    /// Dereference: *expr
    Deref(Box<Expr>),
    /// Comma expression: expr1, expr2
    Comma(Box<Expr>, Box<Expr>),
    /// Initializer list: { expr, expr, ... }
    InitList(Vec<Expr>),
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod,
    BitAnd, BitOr, BitXor,
    Shl, Shr,
    Eq, Ne, Lt, Le, Gt, Ge,
    LogAnd, LogOr,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOp {
    Neg,      // -
    BitNot,   // ~
    LogNot,   // !
}

/// Assignment operators.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AssignOp {
    Assign,       // =
    AddAssign,    // +=
    SubAssign,    // -=
    MulAssign,    // *=
    DivAssign,    // /=
    ModAssign,    // %=
    AndAssign,    // &=
    OrAssign,     // |=
    XorAssign,    // ^=
    ShlAssign,    // <<=
    ShrAssign,    // >>=
}
