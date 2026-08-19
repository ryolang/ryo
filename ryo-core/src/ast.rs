//! Surface-syntax AST.
//!
//! Identifiers, type names, and string literals are stored as
//! `StringId` handles into the compilation's `InternPool`.
//! Tree-drawing lives in `crate::ast_pretty`.

use crate::tir::ParamMode;
use crate::types::StringId;
use chumsky::span::SimpleSpan;
use std::fmt;

// ============================================================================
// Program Structure
// ============================================================================

/// A complete Ryo program consisting of multiple statements.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub statements: Vec<Statement>,
    pub span: SimpleSpan,
}

// ============================================================================
// Statements
// ============================================================================

/// A single statement in a program.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub kind: StmtKind,
    pub span: SimpleSpan,
}

/// The kind of statement.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    VarDecl(VarDecl),
    FunctionDef(FunctionDef),
    Return(Option<Expression>),
    ExprStmt(Expression),
    IfStmt(IfStmt),
    AssignOrDecl {
        target: Ident,
        value: Expression,
    },
    CompoundAssign {
        target: Ident,
        op: CompoundOp,
        value: Expression,
    },
    WhileLoop {
        cond: Expression,
        body: Vec<Statement>,
    },
    ForRange {
        var: Ident,
        iterator: Ident,
        start: Expression,
        end: Expression,
        body: Vec<Statement>,
    },
    Break,
    Continue,
    /// Placeholder for a statement the parser could not parse. The
    /// parser emits the diagnostic itself and recovers at the next
    /// statement boundary, producing this node so later passes keep
    /// a well-formed (partial) AST; astgen lowers it to nothing.
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IfStmt {
    pub cond: Expression,
    pub then_block: Vec<Statement>,
    pub elif_branches: Vec<ElifBranch>,
    pub else_block: Option<Vec<Statement>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElifBranch {
    pub cond: Expression,
    pub block: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VarDecl {
    pub mutable: bool,
    pub name: Ident,
    pub type_annotation: Option<TypeExpr>,
    pub initializer: Expression,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDef {
    pub name: Ident,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: Ident,
    pub type_annotation: TypeExpr,
    pub mode: ParamMode,
    pub span: SimpleSpan,
}

// ============================================================================
// Identifiers and Types
// ============================================================================

/// An identifier (variable / function / type name) with span info.
///
/// Storing a `StringId` rather than `String` keeps the AST `Copy`-ish
/// for the name fields (the rest of the struct is small) and lets
/// later passes compare identifiers without a string compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ident {
    pub name: StringId,
    pub span: SimpleSpan,
}

impl Ident {
    pub fn new(name: StringId, span: SimpleSpan) -> Self {
        Ident { name, span }
    }
}

/// A type expression. Currently just a name like `int`, `bool`, etc.,
/// plus the `is_view` flag for legacy `&name` view syntax (M8.4
/// pre-Q5) — post-M8.4.1 that syntax only feeds the targeted
/// migration error in astgen.
///
/// Field order keeps the struct at 24 bytes: `span` (16 B, align 8)
/// first, then `name` (4 B) and `is_view` (1 B) pack into the tail
/// padding. Declaring `name` before `span` would grow it to 32 B.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeExpr {
    pub span: SimpleSpan,
    pub name: StringId,
    /// Legacy `&name` view-syntax flag (M8.4 pre-Q5): the annotation used
    /// the retired `&` prefix. Post-M8.4.1 this only feeds astgen's
    /// targeted migration error; it never constructs a view type.
    pub is_view: bool,
}

impl TypeExpr {
    pub fn new(name: StringId, span: SimpleSpan) -> Self {
        TypeExpr {
            span,
            name,
            is_view: false,
        }
    }

    pub fn view(name: StringId, span: SimpleSpan) -> Self {
        TypeExpr {
            span,
            name,
            is_view: true,
        }
    }
}

// ============================================================================
// Expressions
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    pub kind: ExprKind,
    pub span: SimpleSpan,
}

impl Expression {
    pub fn new(kind: ExprKind, span: SimpleSpan) -> Self {
        Expression { kind, span }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Literal(Literal),
    Ident(StringId),
    BinaryOp(Box<Expression>, BinaryOperator, Box<Expression>),
    UnaryOp(UnaryOperator, Box<Expression>),
    Call(StringId, Vec<Expression>),
    MethodCall {
        receiver: Box<Expression>,
        method: StringId,
        args: Vec<Expression>,
    },
    /// Call-site mutable borrow marker: `&expr` (M8.3). The inner
    /// expression must resolve to an assignable lvalue (checked in sema).
    Borrow(Box<Expression>),
    /// Slice projection `base[start:end]` (M8.4). Either bound may be
    /// omitted (`s[start:]`, `s[:end]`, `s[:]`). Yields `strview` in sema.
    Slice {
        base: Box<Expression>,
        start: Option<Box<Expression>>,
        end: Option<Box<Expression>>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Literal {
    Int(i64),
    Str(StringId),
    Bool(bool),
    Float(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
}

impl fmt::Display for BinaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryOperator::Add => write!(f, "+"),
            BinaryOperator::Sub => write!(f, "-"),
            BinaryOperator::Mul => write!(f, "*"),
            BinaryOperator::Div => write!(f, "/"),
            BinaryOperator::Mod => write!(f, "%"),
            BinaryOperator::Eq => write!(f, "=="),
            BinaryOperator::NotEq => write!(f, "!="),
            BinaryOperator::Lt => write!(f, "<"),
            BinaryOperator::Gt => write!(f, ">"),
            BinaryOperator::LtEq => write!(f, "<="),
            BinaryOperator::GtEq => write!(f, ">="),
            BinaryOperator::And => write!(f, "and"),
            BinaryOperator::Or => write!(f, "or"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    Neg,
    Not,
}

impl fmt::Display for UnaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnaryOperator::Neg => write!(f, "-"),
            UnaryOperator::Not => write!(f, "not"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CompoundOp {
    Add = 0,
    Sub = 1,
    Mul = 2,
    Div = 3,
    Mod = 4,
}

impl CompoundOp {
    pub fn from_raw(v: u32) -> Self {
        match v {
            0 => Self::Add,
            1 => Self::Sub,
            2 => Self::Mul,
            3 => Self::Div,
            4 => Self::Mod,
            _ => unreachable!("invalid CompoundOp discriminant: {v}"),
        }
    }
}

impl fmt::Display for CompoundOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Add => write!(f, "+="),
            Self::Sub => write!(f, "-="),
            Self::Mul => write!(f, "*="),
            Self::Div => write!(f, "/="),
            Self::Mod => write!(f, "%="),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_operator_display_for_equality() {
        assert_eq!(format!("{}", BinaryOperator::Eq), "==");
        assert_eq!(format!("{}", BinaryOperator::NotEq), "!=");
    }

    #[test]
    fn binary_operator_display_for_ordering_and_modulo() {
        assert_eq!(format!("{}", BinaryOperator::Lt), "<");
        assert_eq!(format!("{}", BinaryOperator::Gt), ">");
        assert_eq!(format!("{}", BinaryOperator::LtEq), "<=");
        assert_eq!(format!("{}", BinaryOperator::GtEq), ">=");
        assert_eq!(format!("{}", BinaryOperator::Mod), "%");
    }

    #[test]
    fn literal_float_variant_round_trips_payload() {
        match Literal::Float(1.5) {
            Literal::Float(v) => assert_eq!(v, 1.5),
            other => panic!("expected Literal::Float, got {:?}", other),
        }
    }

    #[test]
    fn literal_bool_variants_exist() {
        let _t = Literal::Bool(true);
        let _f = Literal::Bool(false);
    }

    #[test]
    fn binary_operator_display_for_logical() {
        assert_eq!(format!("{}", BinaryOperator::And), "and");
        assert_eq!(format!("{}", BinaryOperator::Or), "or");
    }

    #[test]
    fn unary_operator_display_for_not() {
        assert_eq!(format!("{}", UnaryOperator::Not), "not");
    }

    #[test]
    fn type_expr_stays_small() {
        // `span` (16 B) + `name` (4 B) + `is_view` (1 B) must pack
        // into 24 B — see the field-order note on `TypeExpr`.
        assert_eq!(std::mem::size_of::<TypeExpr>(), 24);
    }
}
