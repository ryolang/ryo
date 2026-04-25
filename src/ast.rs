//! Surface-syntax AST.
//!
//! Identifiers, type names, and string literals are stored as
//! `StringId` handles into the compilation's `InternPool`. Pretty
//! printing and Display impls accept a pool reference so they can
//! resolve those handles back to source text.

use crate::types::{InternPool, StringId};
use chumsky::span::SimpleSpan;
use std::fmt;

// ============================================================================
// Program Structure
// ============================================================================

/// A complete Ryo program consisting of multiple statements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub statements: Vec<Statement>,
    pub span: SimpleSpan,
}

impl Program {
    pub fn pretty_print(&self, pool: &InternPool) {
        println!("Program ({}..{})", self.span.start, self.span.end);
        for (idx, stmt) in self.statements.iter().enumerate() {
            let is_last = idx == self.statements.len() - 1;
            let prefix = if is_last { "└── " } else { "├── " };
            print!("{}", prefix);
            stmt.pretty_print_inline();
            println!();
            if !is_last {
                stmt.pretty_print_children("│   ", pool);
            } else {
                stmt.pretty_print_children("    ", pool);
            }
        }
    }
}

// ============================================================================
// Statements
// ============================================================================

/// A single statement in a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub kind: StmtKind,
    pub span: SimpleSpan,
}

impl Statement {
    fn pretty_print_inline(&self) {
        let label = match &self.kind {
            StmtKind::VarDecl(_) => "VarDecl",
            StmtKind::FunctionDef(_) => "FunctionDef",
            StmtKind::Return(_) => "Return",
            StmtKind::ExprStmt(_) => "ExprStmt",
        };
        print!(
            "Statement [{}] ({}..{})",
            label, self.span.start, self.span.end
        );
    }

    fn pretty_print_children(&self, prefix: &str, pool: &InternPool) {
        match &self.kind {
            StmtKind::VarDecl(decl) => decl.pretty_print(prefix, pool),
            StmtKind::FunctionDef(func) => {
                println!("{}FunctionDef: {}", prefix, pool.str(func.name.name));
                let inner = format!("{}  ", prefix);
                for param in &func.params {
                    println!(
                        "{}├── param: {}: {}",
                        inner,
                        pool.str(param.name.name),
                        pool.str(param.type_annotation.name),
                    );
                }
                if let Some(ret_ty) = &func.return_type {
                    println!("{}├── returns: {}", inner, pool.str(ret_ty.name));
                }
                println!("{}└── body:", inner);
                for stmt in &func.body {
                    print!("{}    ", inner);
                    stmt.pretty_print_inline();
                    println!();
                }
            }
            StmtKind::Return(expr) => {
                if let Some(e) = expr {
                    e.pretty_print(prefix, pool);
                }
            }
            StmtKind::ExprStmt(expr) => expr.pretty_print(prefix, pool),
        }
    }
}

/// The kind of statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StmtKind {
    VarDecl(VarDecl),
    FunctionDef(FunctionDef),
    Return(Option<Expression>),
    ExprStmt(Expression),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarDecl {
    pub mutable: bool,
    pub name: Ident,
    pub type_annotation: Option<TypeExpr>,
    pub initializer: Expression,
}

impl VarDecl {
    fn pretty_print(&self, prefix: &str, pool: &InternPool) {
        println!("{}VarDecl", prefix);
        let new_prefix = format!("{}  ", prefix);
        if self.mutable {
            println!("{}├── mutable: true", new_prefix);
        }
        println!(
            "{}├── name: {} ({}..{})",
            new_prefix,
            pool.str(self.name.name),
            self.name.span.start,
            self.name.span.end
        );
        if let Some(ty) = &self.type_annotation {
            println!(
                "{}├── type: {} ({}..{})",
                new_prefix,
                pool.str(ty.name),
                ty.span.start,
                ty.span.end
            );
        }
        println!("{}└── initializer:", new_prefix);
        self.initializer
            .pretty_print(&format!("{}    ", new_prefix), pool);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// Display adapter that resolves `name` through `pool`.
    #[allow(dead_code)]
    pub fn display<'a>(&'a self, pool: &'a InternPool) -> impl fmt::Display + 'a {
        struct D<'a> {
            pool: &'a InternPool,
            id: StringId,
        }
        impl fmt::Display for D<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.pool.str(self.id))
            }
        }
        D {
            pool,
            id: self.name,
        }
    }
}

/// A type expression. Currently just a name like `int`, `bool`, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeExpr {
    pub name: StringId,
    pub span: SimpleSpan,
}

impl TypeExpr {
    pub fn new(name: StringId, span: SimpleSpan) -> Self {
        TypeExpr { name, span }
    }
}

// ============================================================================
// Expressions
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    pub kind: ExprKind,
    pub span: SimpleSpan,
}

impl Expression {
    pub fn new(kind: ExprKind, span: SimpleSpan) -> Self {
        Expression { kind, span }
    }

    fn pretty_print(&self, prefix: &str, pool: &InternPool) {
        let connector_name = match &self.kind {
            ExprKind::Literal(lit) => match lit {
                Literal::Int(n) => format!("Literal(Int({}))", n),
                Literal::Str(s) => format!("Literal(Str(\"{}\"))", pool.str(*s)),
                Literal::Bool(b) => format!("Literal(Bool({}))", b),
            },
            ExprKind::Ident(name) => format!("Ident({})", pool.str(*name)),
            ExprKind::BinaryOp(_, op, _) => format!("BinaryOp({})", op),
            ExprKind::UnaryOp(op, _) => format!("UnaryOp({})", op),
            ExprKind::Call(name, _) => format!("Call({})", pool.str(*name)),
        };

        println!(
            "{}{} ({}..{})",
            prefix, connector_name, self.span.start, self.span.end
        );

        let new_prefix = format!("{}  ", prefix);
        match &self.kind {
            ExprKind::Literal(_) | ExprKind::Ident(_) => {}
            ExprKind::BinaryOp(left, _op, right) => {
                left.pretty_print(&format!("{}├── ", new_prefix), pool);
                right.pretty_print(&format!("{}└── ", new_prefix), pool);
            }
            ExprKind::UnaryOp(_op, expr) => {
                expr.pretty_print(&format!("{}└── ", new_prefix), pool);
            }
            ExprKind::Call(_name, args) => {
                for (i, arg) in args.iter().enumerate() {
                    let is_last = i == args.len() - 1;
                    let prefix_char = if is_last { "└── " } else { "├── " };
                    arg.pretty_print(&format!("{}{}", new_prefix, prefix_char), pool);
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprKind {
    Literal(Literal),
    Ident(StringId),
    BinaryOp(Box<Expression>, BinaryOperator, Box<Expression>),
    UnaryOp(UnaryOperator, Box<Expression>),
    Call(StringId, Vec<Expression>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Literal {
    Int(i64),
    Str(StringId),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    NotEq,
}

impl fmt::Display for BinaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryOperator::Add => write!(f, "+"),
            BinaryOperator::Sub => write!(f, "-"),
            BinaryOperator::Mul => write!(f, "*"),
            BinaryOperator::Div => write!(f, "/"),
            BinaryOperator::Eq => write!(f, "=="),
            BinaryOperator::NotEq => write!(f, "!="),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    Neg,
}

impl fmt::Display for UnaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnaryOperator::Neg => write!(f, "-"),
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
    fn literal_bool_variants_exist() {
        let _t = Literal::Bool(true);
        let _f = Literal::Bool(false);
    }
}
