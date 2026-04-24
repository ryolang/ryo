use chumsky::span::SimpleSpan;
use std::fmt;

pub type Span = SimpleSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Int,
    Str,
    Void,
    Bool,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Int => write!(f, "int"),
            Type::Str => write!(f, "str"),
            Type::Void => write!(f, "void"),
            Type::Bool => write!(f, "bool"),
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
#[allow(dead_code)]
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
#[allow(dead_code)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirExprKind {
    IntLiteral(isize),
    StrLiteral(String),
    BoolLiteral(bool),
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
    Eq,
    NotEq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_bool_displays_as_bool() {
        assert_eq!(format!("{}", Type::Bool), "bool");
    }

    #[test]
    fn bool_literal_kind_exists() {
        let _e = HirExprKind::BoolLiteral(true);
    }

    #[test]
    fn equality_binary_ops_exist() {
        let _e = BinaryOp::Eq;
        let _n = BinaryOp::NotEq;
    }
}
