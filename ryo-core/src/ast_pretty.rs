//! Tree-drawing pretty printer for the surface-syntax AST.
//!
//! Presentation logic lives here so `ast.rs` stays data-only. The
//! printer resolves `StringId` handles through the compilation's
//! `InternPool` and renders into a `String`, so callers decide where
//! the output goes and tests can capture it. It walks [`NodeRef`]s
//! into the flat arena — the AST is not a pointer tree, so the
//! printer carries `&Ast` alongside every ref.
//!
//! Layout convention: every node occupies one line as
//! `{prefix}{connector}{label} (span)`, where `connector` is `├── `
//! or `└── `, and its children are rendered under
//! `{prefix}{"│   " | "    "}` depending on whether the node was the
//! last child of its parent.

use crate::ast::{Ast, Literal, NodeRef, NodeTag};
use crate::tir::ParamMode;
use crate::types::InternPool;
use std::fmt;
use std::fmt::Write as _;

/// Render the full program as an indented tree.
pub fn render_program(ast: &Ast, pool: &InternPool) -> String {
    let mut out = String::new();
    write_program(&mut out, ast, pool).expect("writing to a String is infallible");
    out
}

fn write_program(out: &mut String, ast: &Ast, pool: &InternPool) -> fmt::Result {
    writeln!(out, "Program ({}..{})", ast.span.start, ast.span.end)?;
    let stmts = ast.top_level_stmts();
    for (idx, &stmt) in stmts.iter().enumerate() {
        write_stmt_tree(out, ast, stmt, "", idx == stmts.len() - 1, pool)?;
    }
    Ok(())
}

/// Write a statement as a tree node: inline label on its own line
/// with a branch connector, then its children on continuation lines.
fn write_stmt_tree(
    out: &mut String,
    ast: &Ast,
    stmt: NodeRef,
    prefix: &str,
    is_last: bool,
    pool: &InternPool,
) -> fmt::Result {
    write!(out, "{}{}", prefix, connector(is_last))?;
    write_stmt_inline(out, ast, stmt)?;
    writeln!(out)?;
    let child_prefix = format!("{}{}", prefix, continuation(is_last));
    write_stmt_children(out, ast, stmt, &child_prefix, pool)
}

fn connector(is_last: bool) -> &'static str {
    if is_last { "└── " } else { "├── " }
}

fn continuation(is_last: bool) -> &'static str {
    if is_last { "    " } else { "│   " }
}

fn write_stmt_inline(out: &mut String, ast: &Ast, stmt: NodeRef) -> fmt::Result {
    let label = match ast.tag(stmt) {
        NodeTag::VarDecl => "VarDecl",
        NodeTag::FunctionDef => "FunctionDef",
        NodeTag::Return => "Return",
        NodeTag::ExprStmt => "ExprStmt",
        NodeTag::IfStmt => "IfStmt",
        NodeTag::AssignOrDecl => "AssignOrDecl",
        NodeTag::CompoundAssign => "CompoundAssign",
        NodeTag::WhileLoop => "WhileLoop",
        NodeTag::ForRange => "ForRange",
        NodeTag::Break => "Break",
        NodeTag::Continue => "Continue",
        NodeTag::Error => "Error",
        other => unreachable!("statement node expected, got {other:?}"),
    };
    let span = ast.span(stmt);
    write!(out, "Statement [{}] ({}..{})", label, span.start, span.end)
}

/// Write a list of block statements (function/if/loop bodies) under a
/// header line such as `body:` or `then:`.
fn write_block(
    out: &mut String,
    header: &str,
    body: &[NodeRef],
    prefix: &str,
    is_last: bool,
    ast: &Ast,
    pool: &InternPool,
) -> fmt::Result {
    writeln!(out, "{}{}{}", prefix, connector(is_last), header)?;
    let body_prefix = format!("{}{}", prefix, continuation(is_last));
    for (i, &stmt) in body.iter().enumerate() {
        write_stmt_tree(out, ast, stmt, &body_prefix, i == body.len() - 1, pool)?;
    }
    Ok(())
}

fn write_stmt_children(
    out: &mut String,
    ast: &Ast,
    stmt: NodeRef,
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    match ast.tag(stmt) {
        NodeTag::VarDecl => write_var_decl(out, ast, stmt, prefix, pool),
        NodeTag::FunctionDef => {
            let func = ast.function_def_view(stmt);
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
            write_block(out, "body:", &func.body, &inner, true, ast, pool)
        }
        NodeTag::Return => {
            if let Some(e) = ast.return_value(stmt) {
                write_expr(out, ast, e, prefix, true, "", pool)?;
            }
            Ok(())
        }
        NodeTag::ExprStmt => {
            let value = ast.expr_stmt_value(stmt);
            write_expr(out, ast, value, prefix, true, "", pool)
        }
        NodeTag::IfStmt => write_if_stmt(out, ast, stmt, prefix, pool),
        NodeTag::AssignOrDecl => {
            let view = ast.assign_or_decl_view(stmt);
            writeln!(
                out,
                "{}AssignOrDecl: {}",
                prefix,
                pool.str(view.target.name)
            )?;
            let inner = format!("{}  ", prefix);
            write_expr(out, ast, view.value, &inner, true, "", pool)
        }
        NodeTag::CompoundAssign => {
            let view = ast.compound_assign_view(stmt);
            writeln!(
                out,
                "{}CompoundAssign: {} {:?}",
                prefix,
                pool.str(view.target.name),
                view.op
            )?;
            let inner = format!("{}  ", prefix);
            write_expr(out, ast, view.value, &inner, true, "", pool)
        }
        NodeTag::WhileLoop => {
            let view = ast.while_loop_view(stmt);
            writeln!(out, "{}WhileLoop", prefix)?;
            let inner = format!("{}  ", prefix);
            write_expr(out, ast, view.cond, &inner, false, "cond: ", pool)?;
            write_block(out, "body:", &view.body, &inner, true, ast, pool)
        }
        NodeTag::ForRange => {
            let view = ast.for_range_view(stmt);
            writeln!(
                out,
                "{}ForRange: {} in {}",
                prefix,
                pool.str(view.var.name),
                pool.str(view.iterator.name)
            )?;
            let inner = format!("{}  ", prefix);
            write_expr(out, ast, view.start, &inner, false, "start: ", pool)?;
            write_expr(out, ast, view.end, &inner, false, "end: ", pool)?;
            write_block(out, "body:", &view.body, &inner, true, ast, pool)
        }
        NodeTag::Break => writeln!(out, "{}Break", prefix),
        NodeTag::Continue => writeln!(out, "{}Continue", prefix),
        NodeTag::Error => writeln!(out, "{}Error (unparseable)", prefix),
        other => unreachable!("statement node expected, got {other:?}"),
    }
}

fn write_if_stmt(
    out: &mut String,
    ast: &Ast,
    stmt: NodeRef,
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    let if_stmt = ast.if_stmt_view(stmt);
    writeln!(out, "{}IfStmt", prefix)?;
    let inner = format!("{}  ", prefix);
    // Children: cond, then, elif*, else?. `then` always follows `cond`,
    // so `cond` is never the last child.
    write_expr(out, ast, if_stmt.cond, &inner, false, "cond: ", pool)?;
    let has_tail = !if_stmt.elif_branches.is_empty() || if_stmt.else_block.is_some();
    write_block(
        out,
        "then:",
        &if_stmt.then_block,
        &inner,
        !has_tail,
        ast,
        pool,
    )?;
    for (i, elif) in if_stmt.elif_branches.iter().enumerate() {
        let last_elif = i == if_stmt.elif_branches.len() - 1 && if_stmt.else_block.is_none();
        write_expr(out, ast, elif.cond, &inner, false, "elif cond: ", pool)?;
        write_block(out, "elif body:", &elif.block, &inner, last_elif, ast, pool)?;
    }
    if let Some(else_block) = &if_stmt.else_block {
        write_block(out, "else:", else_block, &inner, true, ast, pool)?;
    }
    Ok(())
}

fn write_var_decl(
    out: &mut String,
    ast: &Ast,
    stmt: NodeRef,
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    let decl = ast.var_decl_view(stmt);
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
    write_expr(out, ast, decl.initializer, &init_prefix, true, "", pool)
}

/// Write an expression node: `{prefix}{connector}{label}{name} (span)`
/// followed by its children under the proper continuation prefix.
fn write_expr(
    out: &mut String,
    ast: &Ast,
    expr: NodeRef,
    prefix: &str,
    is_last: bool,
    label: &str,
    pool: &InternPool,
) -> fmt::Result {
    let tag = ast.tag(expr);
    let name = match tag {
        NodeTag::LiteralInt
        | NodeTag::LiteralStr
        | NodeTag::LiteralBool
        | NodeTag::LiteralFloat => match ast.literal_view(expr) {
            Literal::Int(n) => format!("Literal(Int({}))", n),
            Literal::Str(s) => format!("Literal(Str({:?}))", pool.str(s)),
            Literal::Bool(b) => format!("Literal(Bool({}))", b),
            Literal::Float(v) => format!("Literal(Float({}))", v),
        },
        NodeTag::Ident => format!("Ident({})", pool.str(ast.ident_name(expr))),
        NodeTag::BinaryOp => format!("BinaryOp({})", ast.binary_op_view(expr).op),
        NodeTag::UnaryOp => format!("UnaryOp({})", ast.unary_op_view(expr).op),
        NodeTag::Call => format!("Call({})", pool.str(ast.call_view(expr).name)),
        NodeTag::MethodCall => {
            format!(
                "MethodCall(.{})",
                pool.str(ast.method_call_view(expr).method)
            )
        }
        NodeTag::Borrow => "Borrow".to_string(),
        NodeTag::Slice => "Slice".to_string(),
        other => unreachable!("expression node expected, got {other:?}"),
    };

    let span = ast.span(expr);
    writeln!(
        out,
        "{}{}{}{} ({}..{})",
        prefix,
        connector(is_last),
        label,
        name,
        span.start,
        span.end
    )?;

    let new_prefix = format!("{}{}", prefix, continuation(is_last));
    match tag {
        NodeTag::LiteralInt
        | NodeTag::LiteralStr
        | NodeTag::LiteralBool
        | NodeTag::LiteralFloat
        | NodeTag::Ident => Ok(()),
        NodeTag::BinaryOp => {
            let view = ast.binary_op_view(expr);
            write_expr(out, ast, view.lhs, &new_prefix, false, "", pool)?;
            write_expr(out, ast, view.rhs, &new_prefix, true, "", pool)
        }
        NodeTag::UnaryOp => {
            let view = ast.unary_op_view(expr);
            write_expr(out, ast, view.operand, &new_prefix, true, "", pool)
        }
        NodeTag::Call => {
            let view = ast.call_view(expr);
            write_expr_args(out, ast, &view.args, &new_prefix, pool)
        }
        NodeTag::MethodCall => {
            let view = ast.method_call_view(expr);
            write_expr(
                out,
                ast,
                view.receiver,
                &new_prefix,
                view.args.is_empty(),
                "recv: ",
                pool,
            )?;
            write_expr_args(out, ast, &view.args, &new_prefix, pool)
        }
        NodeTag::Borrow => {
            let inner = ast.borrow_inner(expr);
            write_expr(out, ast, inner, &new_prefix, true, "", pool)
        }
        NodeTag::Slice => {
            let view = ast.slice_view(expr);
            write_expr(out, ast, view.base, &new_prefix, false, "base: ", pool)?;
            write_optional_bound(out, ast, view.start, &new_prefix, false, "start: ", pool)?;
            write_optional_bound(out, ast, view.end, &new_prefix, true, "end: ", pool)
        }
        other => unreachable!("expression node expected, got {other:?}"),
    }
}

fn write_expr_args(
    out: &mut String,
    ast: &Ast,
    args: &[NodeRef],
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    for (i, &arg) in args.iter().enumerate() {
        write_expr(out, ast, arg, prefix, i == args.len() - 1, "", pool)?;
    }
    Ok(())
}

fn write_optional_bound(
    out: &mut String,
    ast: &Ast,
    bound: Option<NodeRef>,
    prefix: &str,
    is_last: bool,
    label: &str,
    pool: &InternPool,
) -> fmt::Result {
    match bound {
        Some(expr) => write_expr(out, ast, expr, prefix, is_last, label, pool),
        None => writeln!(out, "{}{}{}<none>", prefix, connector(is_last), label),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinaryOperator, Ident};
    use chumsky::span::{SimpleSpan, Span};

    fn span(start: usize, end: usize) -> SimpleSpan {
        SimpleSpan::new((), start..end)
    }

    fn ident(pool: &mut InternPool, name: &str) -> Ident {
        Ident::new(pool.intern_str(name), span(0, 0))
    }

    fn int_expr(ast: &mut Ast, n: i64) -> NodeRef {
        ast.literal_int(n, span(0, 0))
    }

    fn return_stmt(ast: &mut Ast, value: i64) -> NodeRef {
        let v = int_expr(ast, value);
        ast.return_stmt(Some(v), span(0, 0))
    }

    #[test]
    fn renders_if_elif_else_children() {
        let mut pool = InternPool::new();
        let mut ast = Ast::new();
        let n = ident(&mut pool, "n");
        let n_ref = ast.ident(n.name, span(0, 0));
        let zero = int_expr(&mut ast, 0);
        let cond = ast.binary(BinaryOperator::Lt, n_ref, zero, span(0, 0));
        let then_s = return_stmt(&mut ast, 1);
        let elif_cond = int_expr(&mut ast, 2);
        let elif_s = return_stmt(&mut ast, 3);
        let else_s = return_stmt(&mut ast, 4);
        let if_stmt = ast.if_stmt(
            cond,
            &[then_s],
            &[(elif_cond, vec![elif_s])],
            Some(&[else_s]),
            span(0, 0),
        );
        let func = ast.function_def(ident(&mut pool, "f"), &[], None, &[if_stmt], span(0, 0));
        ast.set_top_level(&[func]);

        let out = render_program(&ast, &pool);
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
        let mut ast = Ast::new();
        let one = int_expr(&mut ast, 1);
        let two = int_expr(&mut ast, 2);
        let init = ast.binary(BinaryOperator::Add, one, two, span(0, 0));
        let decl = ast.var_decl(false, ident(&mut pool, "x"), None, init, span(0, 0));
        ast.set_top_level(&[decl]);

        let out = render_program(&ast, &pool);
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
        let mut ast = Ast::new();
        let s = pool.intern_str("say \"hi\"\\n\n\t");
        let lit = ast.literal_str(s, span(0, 0));
        let stmt = ast.expr_stmt(lit, span(0, 0));
        ast.set_top_level(&[stmt]);

        let out = render_program(&ast, &pool);
        assert!(
            out.contains(r#"Literal(Str("say \"hi\"\\n\n\t"))"#),
            "special chars not escaped: {out}"
        );
        // One node per line: Program, Statement, Literal — the raw
        // newline in the string must not split the literal's line.
        assert_eq!(out.lines().count(), 3, "{out}");
    }
}
