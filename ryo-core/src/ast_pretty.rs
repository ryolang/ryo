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

use crate::ast::{
    Ast, ExprId, ExprKind, FunctionDef, IfStmt, Literal, PatternId, PatternKind, StmtId, StmtKind,
    TypeExpr, TypeExprKind, VarDecl, VariantArgs,
};
use crate::tir::ParamMode;
use crate::types::{InternPool, VariantKind};
use std::borrow::Cow;
use std::fmt;
use std::fmt::Write as _;

/// Render a type expression back to source-shaped text: a plain name
/// as-is, an anonymous struct literal as `{q: int, r: int}`, a
/// positional form as `(int, str)` (the AST-level mirror of the
/// pool's structural display, which needs a `TypeId` this layer
/// doesn't have).
fn fmt_type_expr<'p>(texpr: &TypeExpr, ast: &Ast, pool: &'p InternPool) -> Cow<'p, str> {
    match &texpr.kind {
        TypeExprKind::Name { name, is_view } => {
            let name = pool.str(*name);
            if *is_view {
                Cow::Owned(format!("&{name}"))
            } else {
                Cow::Borrowed(name)
            }
        }
        TypeExprKind::Anon { fields } => {
            let fields = ast
                .type_field_list(*fields)
                .iter()
                .map(|(name, ty)| format!("{}: {}", pool.str(*name), fmt_type_expr(ty, ast, pool)))
                .collect::<Vec<_>>()
                .join(", ");
            Cow::Owned(format!("{{{fields}}}"))
        }
        TypeExprKind::Positional(elems) => {
            let list = ast.type_expr_list(*elems);
            let rendered = list
                .iter()
                .map(|ty| fmt_type_expr(ty, ast, pool).into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            // A one-element tuple keeps the trailing comma (`(int,)`);
            // `(int)` would read as a parenthesized type, not a tuple.
            if list.len() == 1 {
                Cow::Owned(format!("({rendered},)"))
            } else {
                Cow::Owned(format!("({rendered})"))
            }
        }
    }
}

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
        StmtKind::StructDef(_) => "StructDef",
        StmtKind::EnumDef(_) => "EnumDef",
        StmtKind::Return(_) => "Return",
        StmtKind::ExprStmt(_) => "ExprStmt",
        StmtKind::IfStmt(_) => "IfStmt",
        StmtKind::AssignOrDecl { .. } => "AssignOrDecl",
        StmtKind::CompoundAssign { .. } => "CompoundAssign",
        StmtKind::FieldAssign { .. } => "FieldAssign",
        StmtKind::CompoundFieldAssign { .. } => "CompoundFieldAssign",
        StmtKind::Destructure { .. } => "Destructure",
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
        StmtKind::StructDef(def) => {
            writeln!(out, "{}StructDef: {}", prefix, pool.str(def.name.name))?;
            let inner = format!("{}  ", prefix);
            for (field_name, field_ty) in ast.struct_field_decls(def.fields) {
                writeln!(
                    out,
                    "{}├── field: {}: {}",
                    inner,
                    pool.str(*field_name),
                    fmt_type_expr(field_ty, ast, pool)
                )?;
            }
            Ok(())
        }
        StmtKind::EnumDef(def) => {
            writeln!(out, "{}EnumDef: {}", prefix, pool.str(def.name.name))?;
            let inner = format!("{}  ", prefix);
            for variant in ast.enum_variants(def.variants) {
                let shape = match variant.payload.kind {
                    VariantKind::Unit => "unit",
                    VariantKind::Tuple => "tuple",
                    VariantKind::Named => "named",
                };
                write!(
                    out,
                    "{}├── variant: {} ({})",
                    inner,
                    pool.str(variant.name.name),
                    shape
                )?;
                let fields = ast.struct_field_decls(variant.payload.fields);
                for (field_name, field_ty) in fields {
                    write!(
                        out,
                        " {}: {}",
                        pool.str(*field_name),
                        fmt_type_expr(field_ty, ast, pool)
                    )?;
                }
                writeln!(out)?;
            }
            Ok(())
        }
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
        StmtKind::FieldAssign { target, value } => {
            writeln!(out, "{}FieldAssign", prefix)?;
            let inner = format!("{}  ", prefix);
            write_expr(out, ast, *target, &inner, false, "target: ", pool)?;
            write_expr(out, ast, *value, &inner, true, "value: ", pool)
        }
        StmtKind::CompoundFieldAssign { target, op, value } => {
            writeln!(out, "{}CompoundFieldAssign: {:?}", prefix, op)?;
            let inner = format!("{}  ", prefix);
            write_expr(out, ast, *target, &inner, false, "target: ", pool)?;
            write_expr(out, ast, *value, &inner, true, "value: ", pool)
        }
        StmtKind::Destructure { target, value } => {
            writeln!(out, "{}Destructure", prefix)?;
            let inner = format!("{}  ", prefix);
            write_pattern(out, ast, *target, &inner, false, "target: ", pool)?;
            write_expr(out, ast, *value, &inner, true, "value: ", pool)
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
            fmt_type_expr(&param.type_annotation, ast, pool),
        )?;
    }
    if let Some(ret_ty) = &func.return_type {
        writeln!(
            out,
            "{}├── returns: {}",
            inner,
            fmt_type_expr(ret_ty, ast, pool)
        )?;
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
            fmt_type_expr(ty, ast, pool),
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
        ExprKind::Index { .. } => Cow::Borrowed("Index"),
        ExprKind::StructLiteral(lit) => {
            let name = match lit.name {
                Some(ident) => pool.str(ident.name),
                None => "anonymous",
            };
            Cow::Owned(format!("StructLiteral({name})"))
        }
        ExprKind::FieldAccess { field, .. } => {
            Cow::Owned(format!("FieldAccess(.{})", pool.str(field.name)))
        }
        ExprKind::VariantConstruct(c) => Cow::Owned(format!(
            "VariantConstruct({}.{})",
            pool.str(c.enum_name.name),
            pool.str(c.variant.name)
        )),
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
        ExprKind::Index { base, index } => {
            write_expr(out, ast, base, &new_prefix, false, "base: ", pool)?;
            write_expr(out, ast, index, &new_prefix, true, "index: ", pool)
        }
        ExprKind::StructLiteral(lit) => {
            let fields = ast.struct_field_inits(lit.fields);
            for (i, (name, value)) in fields.iter().enumerate() {
                let label = format!("{}: ", pool.str(*name));
                write_expr(
                    out,
                    ast,
                    *value,
                    &new_prefix,
                    i == fields.len() - 1,
                    &label,
                    pool,
                )?;
            }
            Ok(())
        }
        ExprKind::FieldAccess { object, .. } => {
            write_expr(out, ast, object, &new_prefix, true, "object: ", pool)
        }
        ExprKind::VariantConstruct(c) => match &c.args {
            Some(VariantArgs {
                positional: Some(list),
                ..
            }) => write_expr_args(out, ast, ast.expr_list(*list), &new_prefix, pool),
            Some(VariantArgs {
                named: Some(inits), ..
            }) => {
                let inits = ast.struct_field_inits(*inits);
                for (i, (name, value)) in inits.iter().enumerate() {
                    let label = format!("{}: ", pool.str(*name));
                    write_expr(
                        out,
                        ast,
                        *value,
                        &new_prefix,
                        i == inits.len() - 1,
                        &label,
                        pool,
                    )?;
                }
                Ok(())
            }
            _ => Ok(()),
        },
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

/// Render a destructuring pattern subtree (M10) in the same
/// tree-drawing style as [`write_expr`].
fn write_pattern(
    out: &mut String,
    ast: &Ast,
    pat: PatternId,
    prefix: &str,
    is_last: bool,
    label: &str,
    pool: &InternPool,
) -> fmt::Result {
    let pattern = ast.pattern(pat);
    let name: Cow<'static, str> = match &pattern.kind {
        PatternKind::Wildcard => Cow::Borrowed("Wildcard"),
        PatternKind::Bind(ident) => Cow::Owned(format!("Bind({})", pool.str(ident.name))),
        PatternKind::Anon { .. } => Cow::Borrowed("Anon"),
        PatternKind::Positional(_) => Cow::Borrowed("Positional"),
    };
    writeln!(
        out,
        "{}{}{}{} ({}..{})",
        prefix,
        connector(is_last),
        label,
        name,
        pattern.span.start,
        pattern.span.end
    )?;
    let new_prefix = format!("{}{}", prefix, continuation(is_last));
    match &pattern.kind {
        PatternKind::Wildcard | PatternKind::Bind(_) => Ok(()),
        PatternKind::Anon { fields } => {
            // Fields are scalar (name, binding) pairs, not nodes:
            // render one line each, no recursion.
            let fields = ast.pattern_field_list(*fields);
            for (i, field) in fields.iter().enumerate() {
                writeln!(
                    out,
                    "{}{}{}: {} ({}..{})",
                    new_prefix,
                    connector(i == fields.len() - 1),
                    pool.str(field.name),
                    pool.str(field.binding.name),
                    field.binding.span.start,
                    field.binding.span.end
                )?;
            }
            Ok(())
        }
        PatternKind::Positional(elems) => {
            let elems = ast.pattern_list(*elems);
            for (i, &elem) in elems.iter().enumerate() {
                write_pattern(out, ast, elem, &new_prefix, i == elems.len() - 1, "", pool)?;
            }
            Ok(())
        }
    }
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
    fn positional_type_renders_tuple_sugar() {
        // `(int,)` keeps the trailing comma — without it the form is
        // ambiguous with a parenthesized type; multi-element and empty
        // forms render plain.
        let mut pool = InternPool::new();
        let mut ast = Ast::new();
        let int_name = ident(&mut pool, "int").name;
        let str_name = ident(&mut pool, "str").name;
        let one = ast.type_expr_positional(&[TypeExpr::new(int_name, span(0, 0))], span(0, 0));
        let two = ast.type_expr_positional(
            &[
                TypeExpr::new(int_name, span(0, 0)),
                TypeExpr::new(str_name, span(0, 0)),
            ],
            span(0, 0),
        );
        let empty = ast.type_expr_positional(&[], span(0, 0));
        let f_one = ast.function_def(ident(&mut pool, "f_one"), &[], Some(one), &[], span(0, 0));
        let f_two = ast.function_def(ident(&mut pool, "f_two"), &[], Some(two), &[], span(0, 0));
        let f_empty = ast.function_def(
            ident(&mut pool, "f_empty"),
            &[],
            Some(empty),
            &[],
            span(0, 0),
        );
        ast.set_top_level(vec![f_one, f_two, f_empty]);

        let out = render_program(&ast, &pool);
        assert!(out.contains("returns: (int,)"), "one-elem: {out}");
        assert!(out.contains("returns: (int, str)"), "two-elem: {out}");
        assert!(out.contains("returns: ()"), "empty: {out}");
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
