//! Tree-drawing pretty printer for the surface-syntax AST.
//!
//! Presentation logic lives here so `ast.rs` stays data-only. The
//! printer resolves `StringId` handles through the compilation's
//! `InternPool` and renders into a `String`, so callers decide where
//! the output goes and tests can capture it.
//!
//! Layout convention: every node occupies one line as
//! `{prefix}{connector}{label} (span)`, where `connector` is `├── `
//! or `└── `, and its children are rendered under
//! `{prefix}{"│   " | "    "}` depending on whether the node was the
//! last child of its parent.

use crate::ast::{ExprKind, Expression, IfStmt, Literal, Program, Statement, StmtKind, VarDecl};
use crate::tir::ParamMode;
use crate::types::InternPool;
use std::fmt;
use std::fmt::Write as _;

/// Render the full program as an indented tree.
pub fn render_program(program: &Program, pool: &InternPool) -> String {
    let mut out = String::new();
    write_program(&mut out, program, pool).expect("writing to a String is infallible");
    out
}

fn write_program(out: &mut String, program: &Program, pool: &InternPool) -> fmt::Result {
    writeln!(
        out,
        "Program ({}..{})",
        program.span.start, program.span.end
    )?;
    for (idx, stmt) in program.statements.iter().enumerate() {
        write_stmt_tree(out, stmt, "", idx == program.statements.len() - 1, pool)?;
    }
    Ok(())
}

/// Write a statement as a tree node: inline label on its own line
/// with a branch connector, then its children on continuation lines.
fn write_stmt_tree(
    out: &mut String,
    stmt: &Statement,
    prefix: &str,
    is_last: bool,
    pool: &InternPool,
) -> fmt::Result {
    write!(out, "{}{}", prefix, connector(is_last))?;
    write_stmt_inline(out, stmt)?;
    writeln!(out)?;
    let child_prefix = format!("{}{}", prefix, continuation(is_last));
    write_stmt_children(out, stmt, &child_prefix, pool)
}

fn connector(is_last: bool) -> &'static str {
    if is_last { "└── " } else { "├── " }
}

fn continuation(is_last: bool) -> &'static str {
    if is_last { "    " } else { "│   " }
}

fn write_stmt_inline(out: &mut String, stmt: &Statement) -> fmt::Result {
    let label = match &stmt.kind {
        StmtKind::VarDecl(_) => "VarDecl",
        StmtKind::FunctionDef(_) => "FunctionDef",
        StmtKind::Return(_) => "Return",
        StmtKind::ExprStmt(_) => "ExprStmt",
        StmtKind::IfStmt(_) => "IfStmt",
        StmtKind::AssignOrDecl { .. } => "AssignOrDecl",
        StmtKind::CompoundAssign { .. } => "CompoundAssign",
        StmtKind::WhileLoop { .. } => "WhileLoop",
        StmtKind::ForRange { .. } => "ForRange",
        StmtKind::Break => "Break",
        StmtKind::Continue => "Continue",
        StmtKind::Error => "Error",
    };
    write!(
        out,
        "Statement [{}] ({}..{})",
        label, stmt.span.start, stmt.span.end
    )
}

/// Write a list of block statements (function/if/loop bodies) under a
/// header line such as `body:` or `then:`.
fn write_block(
    out: &mut String,
    header: &str,
    body: &[Statement],
    prefix: &str,
    is_last: bool,
    pool: &InternPool,
) -> fmt::Result {
    writeln!(out, "{}{}{}", prefix, connector(is_last), header)?;
    let body_prefix = format!("{}{}", prefix, continuation(is_last));
    for (i, stmt) in body.iter().enumerate() {
        write_stmt_tree(out, stmt, &body_prefix, i == body.len() - 1, pool)?;
    }
    Ok(())
}

fn write_stmt_children(
    out: &mut String,
    stmt: &Statement,
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    match &stmt.kind {
        StmtKind::VarDecl(decl) => write_var_decl(out, decl, prefix, pool),
        StmtKind::FunctionDef(func) => {
            writeln!(out, "{}FunctionDef: {}", prefix, pool.str(func.name.name))?;
            let inner = format!("{}  ", prefix);
            for param in &func.params {
                let mode_prefix = match param.mode {
                    ParamMode::Move => "move ",
                    ParamMode::Inout => "inout ",
                    ParamMode::Borrow => "",
                };
                writeln!(
                    out,
                    "{}├── param: {}{}: {}",
                    inner,
                    mode_prefix,
                    pool.str(param.name.name),
                    pool.str(param.type_annotation.name),
                )?;
            }
            if let Some(ret_ty) = &func.return_type {
                writeln!(out, "{}├── returns: {}", inner, pool.str(ret_ty.name))?;
            }
            write_block(out, "body:", &func.body, &inner, true, pool)
        }
        StmtKind::Return(expr) => {
            if let Some(e) = expr {
                write_expr(out, e, prefix, true, "", pool)?;
            }
            Ok(())
        }
        StmtKind::ExprStmt(expr) => write_expr(out, expr, prefix, true, "", pool),
        StmtKind::IfStmt(if_stmt) => write_if_stmt(out, if_stmt, prefix, pool),
        StmtKind::AssignOrDecl { target, value } => {
            writeln!(out, "{}AssignOrDecl: {}", prefix, pool.str(target.name))?;
            let inner = format!("{}  ", prefix);
            write_expr(out, value, &inner, true, "", pool)
        }
        StmtKind::CompoundAssign { target, op, value } => {
            writeln!(
                out,
                "{}CompoundAssign: {} {:?}",
                prefix,
                pool.str(target.name),
                op
            )?;
            let inner = format!("{}  ", prefix);
            write_expr(out, value, &inner, true, "", pool)
        }
        StmtKind::WhileLoop { cond, body } => {
            writeln!(out, "{}WhileLoop", prefix)?;
            let inner = format!("{}  ", prefix);
            write_expr(out, cond, &inner, false, "cond: ", pool)?;
            write_block(out, "body:", body, &inner, true, pool)
        }
        StmtKind::ForRange {
            var,
            iterator,
            start,
            end,
            body,
        } => {
            writeln!(
                out,
                "{}ForRange: {} in {}",
                prefix,
                pool.str(var.name),
                pool.str(iterator.name)
            )?;
            let inner = format!("{}  ", prefix);
            write_expr(out, start, &inner, false, "start: ", pool)?;
            write_expr(out, end, &inner, false, "end: ", pool)?;
            write_block(out, "body:", body, &inner, true, pool)
        }
        StmtKind::Break => writeln!(out, "{}Break", prefix),
        StmtKind::Continue => writeln!(out, "{}Continue", prefix),
        StmtKind::Error => writeln!(out, "{}Error (unparseable)", prefix),
    }
}

fn write_if_stmt(
    out: &mut String,
    if_stmt: &IfStmt,
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    writeln!(out, "{}IfStmt", prefix)?;
    let inner = format!("{}  ", prefix);
    // Children: cond, then, elif*, else?. `then` always follows `cond`,
    // so `cond` is never the last child.
    write_expr(out, &if_stmt.cond, &inner, false, "cond: ", pool)?;
    let has_tail = !if_stmt.elif_branches.is_empty() || if_stmt.else_block.is_some();
    write_block(out, "then:", &if_stmt.then_block, &inner, !has_tail, pool)?;
    for (i, elif) in if_stmt.elif_branches.iter().enumerate() {
        let last_elif = i == if_stmt.elif_branches.len() - 1 && if_stmt.else_block.is_none();
        write_expr(out, &elif.cond, &inner, false, "elif cond: ", pool)?;
        write_block(out, "elif body:", &elif.block, &inner, last_elif, pool)?;
    }
    if let Some(else_block) = &if_stmt.else_block {
        write_block(out, "else:", else_block, &inner, true, pool)?;
    }
    Ok(())
}

fn write_var_decl(
    out: &mut String,
    decl: &VarDecl,
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    writeln!(out, "{}VarDecl", prefix)?;
    let new_prefix = format!("{}  ", prefix);
    if decl.mutable {
        writeln!(out, "{}├── mutable: true", new_prefix)?;
    }
    writeln!(
        out,
        "{}├── name: {} ({}..{})",
        new_prefix,
        pool.str(decl.name.name),
        decl.name.span.start,
        decl.name.span.end
    )?;
    if let Some(ty) = &decl.type_annotation {
        writeln!(
            out,
            "{}├── type: {} ({}..{})",
            new_prefix,
            pool.str(ty.name),
            ty.span.start,
            ty.span.end
        )?;
    }
    writeln!(out, "{}└── initializer:", new_prefix)?;
    let init_prefix = format!("{}    ", new_prefix);
    write_expr(out, &decl.initializer, &init_prefix, true, "", pool)
}

/// Write an expression node: `{prefix}{connector}{label}{name} (span)`
/// followed by its children under the proper continuation prefix.
fn write_expr(
    out: &mut String,
    expr: &Expression,
    prefix: &str,
    is_last: bool,
    label: &str,
    pool: &InternPool,
) -> fmt::Result {
    let name = match &expr.kind {
        ExprKind::Literal(lit) => match lit {
            Literal::Int(n) => format!("Literal(Int({}))", n),
            Literal::Str(s) => format!("Literal(Str(\"{}\"))", pool.str(*s).escape_debug()),
            Literal::Bool(b) => format!("Literal(Bool({}))", b),
            Literal::Float(v) => format!("Literal(Float({}))", v),
        },
        ExprKind::Ident(name) => format!("Ident({})", pool.str(*name)),
        ExprKind::BinaryOp(_, op, _) => format!("BinaryOp({})", op),
        ExprKind::UnaryOp(op, _) => format!("UnaryOp({})", op),
        ExprKind::Call(name, _) => format!("Call({})", pool.str(*name)),
        ExprKind::MethodCall { method, .. } => {
            format!("MethodCall(.{})", pool.str(*method))
        }
        ExprKind::Borrow(_) => "Borrow".to_string(),
        ExprKind::Slice { .. } => "Slice".to_string(),
    };

    writeln!(
        out,
        "{}{}{}{} ({}..{})",
        prefix,
        connector(is_last),
        label,
        name,
        expr.span.start,
        expr.span.end
    )?;

    let new_prefix = format!("{}{}", prefix, continuation(is_last));
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Ident(_) => Ok(()),
        ExprKind::BinaryOp(left, _op, right) => {
            write_expr(out, left, &new_prefix, false, "", pool)?;
            write_expr(out, right, &new_prefix, true, "", pool)
        }
        ExprKind::UnaryOp(_op, expr) => write_expr(out, expr, &new_prefix, true, "", pool),
        ExprKind::Call(_name, args) => write_expr_args(out, args, &new_prefix, pool),
        ExprKind::MethodCall { receiver, args, .. } => {
            write_expr(out, receiver, &new_prefix, args.is_empty(), "recv: ", pool)?;
            write_expr_args(out, args, &new_prefix, pool)
        }
        ExprKind::Borrow(inner) => write_expr(out, inner, &new_prefix, true, "", pool),
        ExprKind::Slice { base, start, end } => {
            write_expr(out, base, &new_prefix, false, "base: ", pool)?;
            write_optional_bound(out, start.as_deref(), &new_prefix, false, "start: ", pool)?;
            write_optional_bound(out, end.as_deref(), &new_prefix, true, "end: ", pool)
        }
    }
}

fn write_expr_args(
    out: &mut String,
    args: &[Expression],
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    for (i, arg) in args.iter().enumerate() {
        write_expr(out, arg, prefix, i == args.len() - 1, "", pool)?;
    }
    Ok(())
}

fn write_optional_bound(
    out: &mut String,
    bound: Option<&Expression>,
    prefix: &str,
    is_last: bool,
    label: &str,
    pool: &InternPool,
) -> fmt::Result {
    match bound {
        Some(expr) => write_expr(out, expr, prefix, is_last, label, pool),
        None => writeln!(out, "{}{}{}<none>", prefix, connector(is_last), label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinaryOperator, ElifBranch, FunctionDef, Ident};
    use chumsky::span::{SimpleSpan, Span};

    fn span(start: usize, end: usize) -> SimpleSpan {
        SimpleSpan::new((), start..end)
    }

    fn ident(pool: &mut InternPool, name: &str) -> Ident {
        Ident::new(pool.intern_str(name), span(0, 0))
    }

    fn int_expr(n: i64) -> Expression {
        Expression::new(ExprKind::Literal(Literal::Int(n)), span(0, 0))
    }

    fn return_stmt(value: i64) -> Statement {
        Statement {
            kind: StmtKind::Return(Some(int_expr(value))),
            span: span(0, 0),
        }
    }

    #[test]
    fn renders_if_elif_else_children() {
        let mut pool = InternPool::new();
        let cond = Expression::new(
            ExprKind::BinaryOp(
                Box::new(Expression::new(
                    ExprKind::Ident(ident(&mut pool, "n").name),
                    span(0, 0),
                )),
                BinaryOperator::Lt,
                Box::new(int_expr(0)),
            ),
            span(0, 0),
        );
        let if_stmt = Statement {
            kind: StmtKind::IfStmt(IfStmt {
                cond,
                then_block: vec![return_stmt(1)],
                elif_branches: vec![ElifBranch {
                    cond: int_expr(2),
                    block: vec![return_stmt(3)],
                }],
                else_block: Some(vec![return_stmt(4)]),
            }),
            span: span(0, 0),
        };
        let func = Statement {
            kind: StmtKind::FunctionDef(FunctionDef {
                name: ident(&mut pool, "f"),
                params: vec![],
                return_type: None,
                body: vec![if_stmt],
            }),
            span: span(0, 0),
        };
        let program = Program {
            statements: vec![func],
            span: span(0, 0),
        };

        let out = render_program(&program, &pool);
        assert!(out.contains("FunctionDef: f"), "missing function: {out}");
        assert!(out.contains("IfStmt"), "missing IfStmt: {out}");
        assert!(out.contains("cond: BinaryOp(<)"), "missing cond: {out}");
        assert!(out.contains("then:"), "missing then block: {out}");
        assert!(
            out.contains("elif cond: Literal(Int(2))"),
            "missing elif: {out}"
        );
        assert!(out.contains("else:"), "missing else block: {out}");
        assert!(
            out.contains("Statement [Return]"),
            "missing body stmts: {out}"
        );
        // Three return statements: then, elif, and else bodies.
        assert_eq!(out.matches("Statement [Return]").count(), 3, "{out}");
    }

    #[test]
    fn tree_prefixes_track_last_child() {
        let mut pool = InternPool::new();
        let decl = Statement {
            kind: StmtKind::VarDecl(VarDecl {
                mutable: false,
                name: ident(&mut pool, "x"),
                type_annotation: None,
                initializer: Expression::new(
                    ExprKind::BinaryOp(
                        Box::new(int_expr(1)),
                        BinaryOperator::Add,
                        Box::new(int_expr(2)),
                    ),
                    span(0, 0),
                ),
            }),
            span: span(0, 0),
        };
        let program = Program {
            statements: vec![decl],
            span: span(0, 0),
        };

        let out = render_program(&program, &pool);
        let expected = "\
Program (0..0)
└── Statement [VarDecl] (0..0)
    VarDecl
      ├── name: x (0..0)
      └── initializer:
          └── BinaryOp(+) (0..0)
              ├── Literal(Int(1)) (0..0)
              └── Literal(Int(2)) (0..0)
";
        assert_eq!(out, expected);
    }

    #[test]
    fn str_literal_escapes_special_chars() {
        let mut pool = InternPool::new();
        let s = pool.intern_str("say \"hi\"\\n\n\t");
        let stmt = Statement {
            kind: StmtKind::ExprStmt(Expression::new(
                ExprKind::Literal(Literal::Str(s)),
                span(0, 0),
            )),
            span: span(0, 0),
        };
        let program = Program {
            statements: vec![stmt],
            span: span(0, 0),
        };

        let out = render_program(&program, &pool);
        assert!(
            out.contains(r#"Literal(Str("say \"hi\"\\n\n\t"))"#),
            "special chars not escaped: {out}"
        );
        // One node per line: Program, Statement, Literal — the raw
        // newline in the string must not split the literal's line.
        assert_eq!(out.lines().count(), 3, "{out}");
    }
}
