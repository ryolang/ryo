use chumsky::span::SimpleSpan;
use std::fmt;

pub use crate::types::TypeId;

pub type Span = SimpleSpan;

#[derive(Debug, Clone)]
pub struct HirProgram {
    pub functions: Vec<HirFunction>,
}

#[derive(Debug, Clone)]
pub struct HirFunction {
    pub name: String,
    pub params: Vec<HirParam>,
    pub return_type: TypeId,
    pub body: Vec<HirStmt>,
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub name: String,
    pub ty: TypeId,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum HirStmt {
    VarDecl {
        name: String,
        mutable: bool,
        /// `None` after `ast_lower` when there is no type annotation;
        /// sema replaces it with `Some(inferred_from_initializer)`.
        /// When the source has an annotation, `ast_lower` stores it
        /// eagerly as `Some(annotated)` so sema can cross-check.
        ty: Option<TypeId>,
        initializer: HirExpr,
        span: Span,
    },
    Return(Option<HirExpr>, Span),
    Expr(HirExpr, Span),
}

/// HIR expression.
///
/// `ty` is `None` immediately after `ast_lower::lower` and becomes
/// `Some(...)` after `sema::analyze` succeeds. Codegen requires all
/// expressions to carry a type and will assert on entry.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: Option<TypeId>,
    pub span: Span,
}

impl HirExpr {
    /// Returns the resolved type or panics if sema has not run.
    pub fn expect_ty(&self) -> TypeId {
        self.ty.expect("sema must run before codegen / ty access")
    }
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

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryOp::Add => write!(f, "+"),
            BinaryOp::Sub => write!(f, "-"),
            BinaryOp::Mul => write!(f, "*"),
            BinaryOp::Div => write!(f, "/"),
            BinaryOp::Eq => write!(f, "=="),
            BinaryOp::NotEq => write!(f, "!="),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
}

#[cfg(test)]
mod tests {
    use super::*;

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
