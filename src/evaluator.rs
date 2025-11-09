use crate::ast::{BinaryOperator, Expression, ExprKind, Literal, Program, UnaryOperator};

/// Evaluate a program by evaluating all statements and returning the value
/// of the last initializer expression.
#[allow(dead_code)]
pub fn eval_program(program: &Program) -> isize {
    program
        .statements
        .iter()
        .map(|stmt| {
            // For now, we only support variable declarations
            // In the future, we'll track variable bindings in a scope
            eval_expr(&stmt.kind.as_var_decl().initializer)
        })
        .last()
        .unwrap_or(0)
}

/// Evaluate an expression to an integer value.
#[allow(dead_code)]
pub fn eval_expr(expr: &Expression) -> isize {
    match &expr.kind {
        ExprKind::Literal(Literal::Int(n)) => *n,
        ExprKind::UnaryOp(UnaryOperator::Neg, sub_expr) => -eval_expr(sub_expr),
        ExprKind::BinaryOp(lhs, op, rhs) => {
            let lhs_val = eval_expr(lhs);
            let rhs_val = eval_expr(rhs);

            match op {
                BinaryOperator::Add => lhs_val + rhs_val,
                BinaryOperator::Sub => lhs_val - rhs_val,
                BinaryOperator::Mul => lhs_val * rhs_val,
                BinaryOperator::Div => lhs_val / rhs_val,
            }
        }
    }
}
