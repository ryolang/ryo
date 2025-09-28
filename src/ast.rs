#[derive(Debug, Eq, PartialEq, Clone)]
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

impl Expr {
    pub fn pretty_print(&self) {
        self.print_with_indent("", true);
    }

    fn print_with_indent(&self, prefix: &str, is_last: bool) {
        let connector = if is_last { "└── " } else { "├── " };
        let node_name = match self {
            Expr::Int(n) => format!("Int({})", n),
            Expr::Neg(_) => "Neg".to_string(),
            Expr::Add(_, _) => "Add".to_string(),
            Expr::Sub(_, _) => "Sub".to_string(),
            Expr::Mul(_, _) => "Mul".to_string(),
            Expr::Div(_, _) => "Div".to_string(),
        };

        println!("{}{}{}", prefix, connector, node_name);

        let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

        match self {
            Expr::Neg(expr) => {
                expr.print_with_indent(&new_prefix, true);
            }
            Expr::Add(left, right)
            | Expr::Sub(left, right)
            | Expr::Mul(left, right)
            | Expr::Div(left, right) => {
                left.print_with_indent(&new_prefix, false);
                right.print_with_indent(&new_prefix, true);
            }
            Expr::Int(_) => {} // Leaf node, no children
        }
    }
}
