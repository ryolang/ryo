use chumsky::span::SimpleSpan;
use std::fmt;

pub type Span = SimpleSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Int,
    Str,
    Void,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Int => write!(f, "int"),
            Type::Str => write!(f, "str"),
            Type::Void => write!(f, "void"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HirProgram {
    pub functions: Vec<HirFunction>,
}

#[derive(Debug, Clone)]
pub struct HirFunction {
    pub name: String,
    pub params: Vec<HirParam>,
    pub return_type: Type,
    pub body: Vec<HirStmt>,
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub enum HirStmt {
    VarDecl {
        name: String,
        mutable: bool,
        ty: Type,
        initializer: HirExpr,
        span: Span,
    },
    Return(Option<HirExpr>, Span),
    Expr(HirExpr, Span),
}

#[derive(Debug, Clone)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirExprKind {
    IntLiteral(isize),
    StrLiteral(String),
    Var(String),
    BinaryOp(Box<HirExpr>, BinaryOp, Box<HirExpr>),
    UnaryOp(UnaryOp, Box<HirExpr>),
    Call(String, Vec<HirExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
}
