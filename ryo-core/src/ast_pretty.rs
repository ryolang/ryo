//! Tree-drawing pretty printer for the surface-syntax AST.
//!
//! Presentation logic lives here so `ast.rs` stays data-only. The
//! printer resolves `StringId` handles through the compilation's
//! `InternPool` and renders into a `String`, so callers decide where
//! the output goes and tests can capture it. It walks the typed
//! arenas — the AST is not a pointer tree, so the printer carries
//! `&Ast` alongside every id.
//!
//! Layout convention: every node occupies one line as
//! `{prefix}{connector}{label} (span)`, where `connector` is `├── `
//! or `└── `, and its children are rendered under
//! `{prefix}{"│   " | "    "}` depending on whether the node was the
//! last child of its parent.

use crate::ast::{Ast, ExprId, ExprKind, FunctionDef, IfStmt, Literal, StmtId, StmtKind, VarDecl};
use crate::tir::ParamMode;
use crate::types::InternPool;
use std::borrow::Cow;
use std::fmt;
use std::fmt::Write as _;

/// Render the full program as an indented tree.
pub fn render_program(ast: &Ast, pool: &InternPool) -> String {
    let mut out = String::new();
    write_program(&mut out, ast, pool).expect("writing to a String is infallible");
    out
}

fn write_program(out: &mut String, ast: &Ast, pool: &InternPool) -> fmt::Result {
    writeln!(out, "Program ({}..{})", ast.span().start, ast.span().end)?;
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
    stmt: StmtId,
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

fn write_stmt_inline(out: &mut String, ast: &Ast, stmt: StmtId) -> fmt::Result {
    let stmt = ast.stmt(stmt);
    let label = match stmt.kind {
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
    body: &[StmtId],
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
    stmt: StmtId,
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    match &ast.stmt(stmt).kind {
        StmtKind::VarDecl(decl) => write_var_decl(out, ast, decl, prefix, pool),
        StmtKind::FunctionDef(func) => write_function_def(out, ast, func, prefix, pool),
        StmtKind::Return(value) => {
            if let Some(e) = value {
                write_expr(out, ast, *e, prefix, true, "", pool)?;
            }
            Ok(())
        }
        StmtKind::ExprStmt(value) => write_expr(out, ast, *value, prefix, true, "", pool),
        StmtKind::IfStmt(if_stmt) => write_if_stmt(out, ast, if_stmt, prefix, pool),
        StmtKind::AssignOrDecl { target, value } => {
            writeln!(out, "{}AssignOrDecl: {}", prefix, pool.str(target.name))?;
            let inner = format!("{}  ", prefix);
            write_expr(out, ast, *value, &inner, true, "", pool)
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
            write_expr(out, ast, *value, &inner, true, "", pool)
        }
        StmtKind::WhileLoop { cond, body } => {
            writeln!(out, "{}WhileLoop", prefix)?;
            let inner = format!("{}  ", prefix);
            write_expr(out, ast, *cond, &inner, false, "cond: ", pool)?;
            write_block(out, "body:", ast.stmt_list(*body), &inner, true, ast, pool)
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
            write_expr(out, ast, *start, &inner, false, "start: ", pool)?;
            write_expr(out, ast, *end, &inner, false, "end: ", pool)?;
            write_block(out, "body:", ast.stmt_list(*body), &inner, true, ast, pool)
        }
        StmtKind::Break => writeln!(out, "{}Break", prefix),
        StmtKind::Continue => writeln!(out, "{}Continue", prefix),
        StmtKind::Error => writeln!(out, "{}Error (unparseable)", prefix),
    }
}

fn write_function_def(
    out: &mut String,
    ast: &Ast,
    func: &FunctionDef,
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
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
    write_block(
        out,
        "body:",
        ast.stmt_list(func.body),
        &inner,
        true,
        ast,
        pool,
    )
}

fn write_if_stmt(
    out: &mut String,
    ast: &Ast,
    if_stmt: &IfStmt,
    prefix: &str,
    pool: &InternPool,
) -> fmt::Result {
    writeln!(out, "{}IfStmt", prefix)?;
    let inner = format!("{}  ", prefix);
    let elifs = ast.elif_list(if_stmt.elif_branches);
    // Children: cond, then, elif*, else?. `then` always follows `cond`,
    // so `cond` is never the last child.
    write_expr(out, ast, if_stmt.cond, &inner, false, "cond: ", pool)?;
    let has_tail = !elifs.is_empty() || if_stmt.else_block.is_some();
    write_block(
        out,
        "then:",
        ast.stmt_list(if_stmt.then_block),
        &inner,
        !has_tail,
        ast,
        pool,
    )?;
    for (i, elif) in elifs.iter().enumerate() {
        let last_elif = i == elifs.len() - 1 && if_stmt.else_block.is_none();
        write_expr(out, ast, elif.cond, &inner, false, "elif cond: ", pool)?;
        write_block(
            out,
            "elif body:",
            ast.stmt_list(elif.block),
            &inner,
            last_elif,
            ast,
            pool,
        )?;
    }
    if let Some(else_block) = if_stmt.else_block {
        write_block(
            out,
            "else:",
            ast.stmt_list(else_block),
            &inner,
            true,
            ast,
            pool,
        )?;
    }
    Ok(())
}

fn write_var_decl(
    out: &mut String,
    ast: &Ast,
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
    write_expr(out, ast, decl.initializer, &init_prefix, true, "", pool)
}

/// Write an expression node: `{prefix}{connector}{label}{name} (span)`
/// followed by its children under the proper continuation prefix.
fn write_expr(
    out: &mut String,
    ast: &Ast,
    expr: ExprId,
    prefix: &str,
    is_last: bool,
    label: &str,
    pool: &InternPool,
) -> fmt::Result {
    let expr = ast.expr(expr);
    // `Cow` so the constant labels borrow instead of allocating.
    let name: Cow<'static, str> = match expr.kind {
        ExprKind::Literal(lit) => match lit {
            Literal::Int(n) => Cow::Owned(format!("Literal(Int({}))", n)),
            Literal::Str(s) => Cow::Owned(format!("Literal(Str({:?}))", pool.str(s))),
            Literal::Bytes(s) => Cow::Owned(format!("Literal(Bytes({:?}))", pool.bytes_payload(s))),
            Literal::Bool(b) => Cow::Owned(format!("Literal(Bool({}))", b)),
            Literal::Float(v) => Cow::Owned(format!("Literal(Float({}))", v)),
        },
        ExprKind::Ident(name) => Cow::Owned(format!("Ident({})", pool.str(name))),
        ExprKind::BinaryOp(_, op, _) => Cow::Owned(format!("BinaryOp({})", op)),
        ExprKind::UnaryOp(op, _) => Cow::Owned(format!("UnaryOp({})", op)),
        ExprKind::Call(name, _) => Cow::Owned(format!("Call({})", pool.str(name))),
        ExprKind::MethodCall { method, .. } => {
            Cow::Owned(format!("MethodCall(.{})", pool.str(method)))
        }
        ExprKind::Borrow(_) => Cow::Borrowed("Borrow"),
        ExprKind::Slice { .. } => Cow::Borrowed("Slice"),
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
    match expr.kind {
        ExprKind::Literal(_) | ExprKind::Ident(_) => Ok(()),
        ExprKind::BinaryOp(lhs, _, rhs) => {
            write_expr(out, ast, lhs, &new_prefix, false, "", pool)?;
            write_expr(out, ast, rhs, &new_prefix, true, "", pool)
        }
        ExprKind::UnaryOp(_, operand) => write_expr(out, ast, operand, &new_prefix, true, "", pool),
        ExprKind::Call(_, args) => {
            write_expr_args(out, ast, ast.expr_list(args), &new_prefix, pool)
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            let args = ast.expr_list(args);
            write_expr(
                out,
                ast,
                receiver,
                &new_prefix,
                args.is_empty(),
                "recv: ",
                pool,
            )?;
            write_expr_args(out, ast, args, &new_prefix, pool)
        }
        ExprKind::Borrow(inner) => write_expr(out, ast, inner, &new_prefix, true, "", pool),
        ExprKind::Slice { base, start, end } => {
            write_expr(out, ast, base, &new_prefix, false, "base: ", pool)?;
            write_optional_bound(out, ast, start, &new_prefix, false, "start: ", pool)?;
            write_optional_bound(out, ast, end, &new_prefix, true, "end: ", pool)
        }
    }
}

fn write_expr_args(
    out: &mut String,
    ast: &Ast,
    args: &[ExprId],
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
    bound: Option<ExprId>,
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

    fn int_expr(ast: &mut Ast, n: i64) -> ExprId {
        ast.literal_int(n, span(0, 0))
    }

    fn return_stmt(ast: &mut Ast, value: i64) -> StmtId {
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
        let elif_block = ast.push_stmt_list(&[elif_s]);
        let else_s = return_stmt(&mut ast, 4);
        let if_stmt = ast.if_stmt(
            cond,
            &[then_s],
            &[(elif_cond, elif_block)],
            Some(&[else_s]),
            span(0, 0),
        );
        let func = ast.function_def(ident(&mut pool, "f"), &[], None, &[if_stmt], span(0, 0));
        ast.set_top_level(vec![func]);

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
        ast.set_top_level(vec![decl]);

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
        ast.set_top_level(vec![stmt]);

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
