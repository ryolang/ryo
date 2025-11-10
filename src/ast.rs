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
    pub fn pretty_print(&self) {
        println!("Program ({}..{})", self.span.start, self.span.end);
        for (idx, stmt) in self.statements.iter().enumerate() {
            let is_last = idx == self.statements.len() - 1;
            let prefix = if is_last { "└── " } else { "├── " };
            print!("{}", prefix);
            stmt.pretty_print_inline();
            println!();
            if !is_last {
                stmt.pretty_print_children("│   ");
            } else {
                stmt.pretty_print_children("    ");
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
        match &self.kind {
            StmtKind::VarDecl(_) => {
                print!("Statement [VarDecl] ({}..{})", self.span.start, self.span.end);
            }
        }
    }

    #[allow(unused_variables)]
    fn pretty_print_children(&self, prefix: &str) {
        match &self.kind {
            StmtKind::VarDecl(decl) => {
                decl.pretty_print(prefix);
            }
        }
    }
}

/// The kind of statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StmtKind {
    /// Variable declaration: [mut] name [: type] = expr
    VarDecl(VarDecl),
}

impl StmtKind {
    /// Extract the VarDecl if this is a VarDecl statement.
    pub fn as_var_decl(&self) -> &VarDecl {
        match self {
            StmtKind::VarDecl(decl) => decl,
        }
    }
}

/// A variable declaration statement.
/// Syntax: `[mut] ident [: type] = expression`
/// Examples:
///   - `x = 42`
///   - `x: int = 42`
///   - `mut x = 42`
///   - `mut counter: int = 0`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarDecl {
    pub mutable: bool,                      // true if 'mut' keyword was present
    pub name: Ident,                        // variable name
    pub type_annotation: Option<TypeExpr>,  // optional explicit type
    pub initializer: Expression,            // initial value expression
}

impl VarDecl {
    fn pretty_print(&self, prefix: &str) {
        println!("{}VarDecl", prefix);
        let new_prefix = format!("{}  ", prefix);

        // Print mutable flag if true
        if self.mutable {
            println!("{}├── mutable: true", new_prefix);
        }

        // Print name
        println!("{}├── name: {} ({}..{})", new_prefix, self.name.name, self.name.span.start, self.name.span.end);

        // Print type annotation if present
        if let Some(ty) = &self.type_annotation {
            println!("{}├── type: {} ({}..{})", new_prefix, ty.name, ty.span.start, ty.span.end);
        }

        // Print initializer
        println!("{}└── initializer:", new_prefix);
        self.initializer.pretty_print(&format!("{}    ", new_prefix));
    }
}

// ============================================================================
// Identifiers and Types
// ============================================================================

/// An identifier (variable or type name) with span information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: SimpleSpan,
}

impl Ident {
    pub fn new(name: String, span: SimpleSpan) -> Self {
        Ident { name, span }
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

/// A type expression.
/// Currently just a name like "int", "float", "str", etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExpr {
    pub name: String,
    pub span: SimpleSpan,
}

impl TypeExpr {
    pub fn new(name: String, span: SimpleSpan) -> Self {
        TypeExpr { name, span }
    }
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

// ============================================================================
// Expressions
// ============================================================================

/// An expression with span information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expression {
    pub kind: ExprKind,
    pub span: SimpleSpan,
}

impl Expression {
    pub fn new(kind: ExprKind, span: SimpleSpan) -> Self {
        Expression { kind, span }
    }

    fn pretty_print(&self, prefix: &str) {
        let connector_name = match &self.kind {
            ExprKind::Literal(lit) => match lit {
                Literal::Int(n) => format!("Literal(Int({}))", n),
                Literal::Str(s) => format!("Literal(Str(\"{}\"))", s),
            },
            ExprKind::BinaryOp(_, op, _) => format!("BinaryOp({})", op),
            ExprKind::UnaryOp(op, _) => format!("UnaryOp({})", op),
            ExprKind::Call(name, _) => format!("Call({})", name),
        };

        println!("{}{} ({}..{})", prefix, connector_name, self.span.start, self.span.end);

        let new_prefix = format!("{}  ", prefix);
        match &self.kind {
            ExprKind::Literal(_) => {} // Leaf node
            ExprKind::BinaryOp(left, _op, right) => {
                left.pretty_print(&format!("{}├── ", new_prefix));
                right.pretty_print(&format!("{}└── ", new_prefix));
            }
            ExprKind::UnaryOp(_op, expr) => {
                expr.pretty_print(&format!("{}└── ", new_prefix));
            }
            ExprKind::Call(_name, args) => {
                for (i, arg) in args.iter().enumerate() {
                    let is_last = i == args.len() - 1;
                    let prefix_char = if is_last { "└── " } else { "├── " };
                    arg.pretty_print(&format!("{}{}", new_prefix, prefix_char));
                }
            }
        }
    }
}

/// The kind of expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprKind {
    /// Literal value: integer, string, etc.
    Literal(Literal),
    /// Binary operation: left op right
    BinaryOp(Box<Expression>, BinaryOperator, Box<Expression>),
    /// Unary operation: op expr
    UnaryOp(UnaryOperator, Box<Expression>),
    /// Function call: function_name(arg1, arg2, ...)
    Call(String, Vec<Expression>),
}

/// Literal values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal {
    /// Integer literal
    Int(isize),
    /// String literal
    Str(String),
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    Add,
    Sub,
    Mul,
    Div,
}

impl fmt::Display for BinaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryOperator::Add => write!(f, "+"),
            BinaryOperator::Sub => write!(f, "-"),
            BinaryOperator::Mul => write!(f, "*"),
            BinaryOperator::Div => write!(f, "/"),
        }
    }
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
    Neg, // Negation: -expr
}

impl fmt::Display for UnaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnaryOperator::Neg => write!(f, "-"),
        }
    }
}
