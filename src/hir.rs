// HIR is a test-only inspection target after Phase 3 commit 4: only
// astgen / sema unit tests reconstruct it via
// `astgen::uir_to_hir_typed`. The dead-code lint flags fields the
// tests destructure with `..` rather than bind — those reads don't
// register. Allow until commit 5 deletes this file outright.
#![allow(dead_code)]

//! High-level IR.
//!
//! Identifiers, function names, and string-literal payloads are
//! `StringId` handles into the compilation's `InternPool`. Per-stage
//! invariants:
//!
//! - On exit from `astgen::uir_to_hir`, every `HirExpr.ty` is `None`.
//! - On exit from `sema::analyze`, every `HirExpr.ty` is `Some(...)`.
//!   Codegen requires the latter and asserts on entry.

use chumsky::span::SimpleSpan;
use std::fmt;

pub use crate::types::{StringId, TypeId};

pub type Span = SimpleSpan;

#[derive(Debug, Clone)]
pub struct HirProgram {
    pub functions: Vec<HirFunction>,
}

#[derive(Debug, Clone)]
pub struct HirFunction {
    pub name: StringId,
    pub params: Vec<HirParam>,
    pub return_type: TypeId,
    pub body: Vec<HirStmt>,
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub name: StringId,
    pub ty: TypeId,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum HirStmt {
    VarDecl {
        name: StringId,
        mutable: bool,
        /// `None` after astgen when there is no annotation.
        /// Sema replaces it with `Some(inferred_from_initializer)`.
        ty: Option<TypeId>,
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
    IntLiteral(i64),
    /// Decoded string-literal contents, interned as a `StringId`.
    StrLiteral(StringId),
    BoolLiteral(bool),
    Var(StringId),
    BinaryOp(Box<HirExpr>, BinaryOp, Box<HirExpr>),
    UnaryOp(UnaryOp, Box<HirExpr>),
    Call(StringId, Vec<HirExpr>),
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
