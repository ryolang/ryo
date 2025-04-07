

#[derive(Debug, Eq, PartialEq)]
pub enum Expr {
    // Integer literal.
    Int(isize),
    // Float literal
    //Float(f64),

    // Unary minus.
    Neg(Box<Expr>),

    // Binary operators.
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
}