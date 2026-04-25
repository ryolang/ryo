use chumsky::span::SimpleSpan;

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
        ty: TypeId,
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
