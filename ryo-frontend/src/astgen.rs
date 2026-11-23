//! AST → UIR structural translation.
//!
//! Named for symmetry with Zig's `AstGen.zig` — the responsibility
//! (lowering an AST into the first IR) is the same. Pure
//! structural translation. Type annotations and nominal declarations are
//! copied into UIR-owned unresolved side arenas; Sema resolves them later.
//! No types are attached to instructions or declaration side tables.
//!
//! Identifier names come pre-interned as `StringId` from the parser,
//! so this stage is allocation-light: it copies handles around.
//!
//! ## Output
//!
//! [`generate`] returns the canonical [`Uir`]. The pipeline driver
//! threads it directly into `sema::analyze` and from there into
//! `codegen::compile`; there is no intermediate tree-shaped IR.

mod errors;
use ryo_core::ast;
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::types::{InternPool, StringId};
use ryo_core::uir::{
    InstRef, InstTag, Uir, UirBuilder, UirEnumDecl, UirEnumField, UirEnumVariant, UirErrorDecl,
    UirErrorFamilyDecl, UirErrorMemberLink, UirParam, UirStructDecl, UirStructField,
};

type Span = ryo_core::span::Span;

struct Names {
    main: StringId,
    range: StringId,
}

fn copy_type_expr(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    expr: &ast::TypeExpr,
) -> ryo_core::uir::TypeExprId {
    if let ast::TypeExprKind::Name { name, is_view } = expr.kind {
        return b.type_name(name, is_view, expr.span);
    }

    enum Task {
        Enter(ast::TypeExpr),
        Anon {
            fields: ast::TypeFieldList,
            span: Span,
        },
        Positional {
            elements: ast::TypeExprList,
            span: Span,
        },
        ErrorUnion {
            error_set: ast::TypeExprList,
            inferred: bool,
            span: Span,
        },
    }

    let mut tasks = vec![Task::Enter(*expr)];
    let mut results = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Enter(expr) => match expr.kind {
                ast::TypeExprKind::Name { name, is_view } => {
                    results.push(b.type_name(name, is_view, expr.span));
                }
                ast::TypeExprKind::Anon { fields } => {
                    tasks.push(Task::Anon {
                        fields,
                        span: expr.span,
                    });
                    for (_, field) in ast.type_field_list(fields).iter().rev() {
                        tasks.push(Task::Enter(*field));
                    }
                }
                ast::TypeExprKind::Positional(elements) => {
                    tasks.push(Task::Positional {
                        elements,
                        span: expr.span,
                    });
                    for element in ast.type_expr_list(elements).iter().rev() {
                        tasks.push(Task::Enter(*element));
                    }
                }
                ast::TypeExprKind::ErrorUnion {
                    error_set,
                    success,
                    inferred,
                } => {
                    tasks.push(Task::ErrorUnion {
                        error_set,
                        inferred,
                        span: expr.span,
                    });
                    tasks.push(Task::Enter(ast.type_expr(success)));
                    for member in ast.type_expr_list(error_set).iter().rev() {
                        tasks.push(Task::Enter(*member));
                    }
                }
            },
            Task::Anon { fields, span } => {
                let field_values =
                    results.split_off(results.len() - ast.type_field_list(fields).len());
                let fields = ast
                    .type_field_list(fields)
                    .iter()
                    .zip(field_values)
                    .map(|(&(name, _), value)| (name, value))
                    .collect::<Vec<_>>();
                results.push(b.type_anon(&fields, span));
            }
            Task::Positional { elements, span } => {
                let element_values =
                    results.split_off(results.len() - ast.type_expr_list(elements).len());
                results.push(b.type_positional(&element_values, span));
            }
            Task::ErrorUnion {
                error_set,
                inferred,
                span,
            } => {
                let success = results
                    .pop()
                    .expect("error-union success must be copied first");
                let error_set_len = ast.type_expr_list(error_set).len();
                let error_set = results.split_off(results.len() - error_set_len);
                results.push(b.type_error_union(&error_set, success, inferred, span));
            }
        }
    }
    results.pop().expect("root type expression must be copied")
}

/// Lower an [`ast::Ast`] to unresolved UIR, accumulating only structural
/// diagnostics. Nominal failures remain in the side tables for Sema.
pub fn generate(program: &ast::Ast, pool: &mut InternPool, sink: &mut DiagSink) -> Uir {
    let mut func_defs: Vec<&ast::FunctionDef> = Vec::new();
    let mut top_level: Vec<ast::StmtId> = Vec::new();
    let names = Names {
        main: pool.intern_str("main"),
        range: pool.intern_str("range"),
    };
    let mut b = UirBuilder::new();
    let mut error_family_count = 0usize;

    for (source_index, &stmt) in program.top_level_stmts().iter().enumerate() {
        let source_index = u32::try_from(source_index).expect("top-level source index fits u32");
        let span = program.stmt_span(stmt);
        match &program.stmt(stmt).kind {
            ast::StmtKind::FunctionDef(def) => func_defs.push(def),
            ast::StmtKind::Error => {}
            ast::StmtKind::StructDef(def) => {
                let fields = program
                    .struct_field_decls(def.fields)
                    .iter()
                    .map(|&(name, ref field)| UirStructField {
                        name,
                        type_expr: copy_type_expr(&mut b, program, field),
                        span: field.span,
                    })
                    .collect();
                b.add_struct_decl(UirStructDecl {
                    name: def.name.name,
                    name_span: def.name.span,
                    attrs: def.attrs,
                    fields,
                    span,
                    source_index,
                });
            }
            ast::StmtKind::EnumDef(def) => {
                let variants = program
                    .enum_variants(def.variants)
                    .iter()
                    .map(|variant| UirEnumVariant {
                        name: variant.name.name,
                        name_span: variant.name.span,
                        kind: variant.payload.kind,
                        fields: copy_payload_fields(&mut b, program, variant.payload),
                        span: variant.span,
                    })
                    .collect();
                b.add_enum_decl(UirEnumDecl {
                    name: def.name.name,
                    name_span: def.name.span,
                    attrs: def.attrs,
                    variants,
                    span,
                    source_index,
                });
            }
            ast::StmtKind::ErrorFamilyDef(def) => {
                let family =
                    u32::try_from(error_family_count).expect("error family index fits u32");
                error_family_count = error_family_count
                    .checked_add(1)
                    .expect("error family count overflow");
                let source_members = program.enum_variants(def.members);
                let mut member_names = Vec::with_capacity(source_members.len());
                let mut members = Vec::with_capacity(source_members.len());
                for (member_index, source_member) in source_members.iter().enumerate() {
                    let member = u32::try_from(member_index).expect("error member index fits u32");
                    let qualified =
                        errors::qualified_error_name(pool, def.name.name, source_member.name.name);
                    member_names.push(source_member.name.name);
                    members.push(qualified);
                    let fields = copy_payload_fields(&mut b, program, source_member.payload);
                    b.add_error_decl(UirErrorDecl {
                        name: qualified,
                        payload: UirEnumVariant {
                            name: qualified,
                            name_span: source_member.name.span,
                            kind: source_member.payload.kind,
                            fields,
                            span: source_member.name.span,
                        },
                        family: Some(UirErrorMemberLink { family, member }),
                        span: source_member.span,
                        source_index,
                    });
                }
                b.add_error_family_decl(UirErrorFamilyDecl {
                    name: def.name.name,
                    name_span: def.name.span,
                    member_names,
                    members,
                    visibility: def.visibility,
                    span,
                    source_index,
                });
            }
            ast::StmtKind::ErrorDef(def) => {
                let fields = copy_payload_fields(&mut b, program, def.payload);
                b.add_error_decl(UirErrorDecl {
                    name: def.name.name,
                    payload: UirEnumVariant {
                        name: def.name.name,
                        name_span: def.name.span,
                        kind: def.payload.kind,
                        fields,
                        span: def.name.span,
                    },
                    family: None,
                    span,
                    source_index,
                });
            }
            _ => top_level.push(stmt),
        }
    }

    let has_explicit_main = func_defs.iter().any(|def| def.name.name == names.main);
    if has_explicit_main && !top_level.is_empty() {
        let span = program.stmt_span(top_level[0]);
        sink.emit(Diag::error(
            span,
            DiagCode::TopLevelWithExplicitMain,
            "top-level statements are not allowed when fn main() is defined",
        ));
    }

    for func in &func_defs {
        gen_function_def(&mut b, program, func, &names, pool, sink);
    }
    if !has_explicit_main {
        gen_implicit_main(&mut b, program, &top_level, &names, pool, sink);
    }

    b.finish().with_layout_context(pool)
}

fn copy_payload_fields(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    payload: ast::EnumPayload,
) -> Vec<UirEnumField> {
    ast.struct_field_decls(payload.fields)
        .iter()
        .map(|&(name, ref field)| UirEnumField {
            name,
            type_expr: copy_type_expr(b, ast, field),
            span: field.span,
        })
        .collect()
}

fn lower_block(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmts: ast::StmtList,
    names: &Names,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> Vec<InstRef> {
    lower_block_d(b, ast, stmts, names, pool, sink, 0)
}

#[allow(clippy::too_many_arguments)]
fn lower_block_d(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmts: ast::StmtList,
    names: &Names,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    depth: u32,
) -> Vec<InstRef> {
    if depth == expr::expr_depth_limit() {
        return expr::gen_block_machine(b, ast, stmts, names, pool, sink, depth);
    }
    let mut out = Vec::new();
    for &s in ast.stmt_list(stmts) {
        gen_stmt_d(b, ast, s, names, pool, sink, &mut out, depth);
    }
    out
}

fn gen_implicit_main(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmts: &[ast::StmtId],
    names: &Names,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    // Synthesize `fn main():` (void return). Codegen falls through
    // to an implicit `ret_void` if no explicit return is emitted —
    // we deliberately do *not* push a synthetic Return here so the
    // body shape matches an explicit `fn main():` written by the
    // user.
    let mut body_stmts: Vec<InstRef> = Vec::new();
    for &stmt in stmts {
        gen_stmt(b, ast, stmt, names, pool, sink, &mut body_stmts);
    }

    b.add_function(names.main, vec![], None, &body_stmts, ast.span());
}

fn gen_function_def(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    func: &ast::FunctionDef,
    names: &Names,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    let params: Vec<UirParam> = func
        .params
        .iter()
        .map(|p| UirParam {
            name: p.name.name,
            type_expr: copy_type_expr(b, ast, &p.type_annotation),
            mode: p.mode,
            span: p.span,
        })
        .collect();

    let return_type = func
        .return_type
        .as_ref()
        .map(|ty| copy_type_expr(b, ast, ty));

    // `fn main()` must be `fn main():` — no args, no return type.
    // Non-zero exit codes go through the future `exit(code)`
    // builtin (M24); main is always void in v0.1 and the C-ABI
    // shim emitted by codegen returns 0 to the OS. `names.main` is
    // interned once by `generate`; the probe is a `StringId` compare.
    if func.name.name == names.main {
        if !func.params.is_empty() {
            sink.emit(Diag::error(
                func.name.span,
                DiagCode::MainSignature,
                "fn main() must take no arguments",
            ));
        }
        if let Some(ret) = &func.return_type {
            sink.emit(Diag::error(
                ret.span,
                DiagCode::MainSignature,
                "fn main() must have no return type (use exit(code) for non-zero exit codes)",
            ));
        }
    }

    let body_stmts = lower_block(b, ast, func.body, names, pool, sink);

    b.add_function(
        func.name.name,
        params,
        return_type,
        &body_stmts,
        func.name.span,
    );
}

/// Lower a destructuring pattern (M10) to `InstTag::Destructure`
/// plan(s). Each pattern level becomes one instruction whose plan
/// entries are `(selector, bind)` pairs — positional patterns select
/// by canonical field index, brace patterns by field name, and a
/// `None` bind is a wildcard (the field is destroyed, not bound).
///
/// A nested pattern element (possible only inside a positional
/// pattern) cannot be expressed in one flat plan: astgen binds the
/// field to a synthesized `__ryo_destructure_N` temp and recurses,
/// emitting the sub-pattern's own `Destructure` over a `TempVar` read
/// of that temp as the following statement. `TempVar` is its own UIR
/// tag, so the read resolves only through sema's destructuring-temp
/// side table — never the user scope — and a user-spelled `Var`
/// reference cannot observe or hijack the temp.
#[allow(clippy::too_many_arguments)]
fn lower_destructure_pattern(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    pool: &mut InternPool,
    pattern: ast::PatternId,
    value: InstRef,
    span: Span,
    sink: &mut DiagSink,
    out: &mut Vec<InstRef>,
) {
    let pat = ast.pattern(pattern);
    match &pat.kind {
        ast::PatternKind::Positional(list) => {
            let mut plan: Vec<(u32, Option<StringId>)> = Vec::new();
            // Nested elements recurse as their own statements, which
            // must FOLLOW the enclosing Destructure (it binds the temp
            // their `Var` rhs resolves through) — collect and lower
            // them after this level's instruction is emitted.
            let mut nested: Vec<(ast::PatternId, StringId, Span)> = Vec::new();
            for (i, elem) in ast.pattern_list(*list).iter().enumerate() {
                let elem_pat = ast.pattern(*elem);
                match &elem_pat.kind {
                    ast::PatternKind::Bind(ident) => {
                        plan.push((i as u32, Some(ident.name)));
                    }
                    ast::PatternKind::Wildcard => plan.push((i as u32, None)),
                    ast::PatternKind::Anon { .. } | ast::PatternKind::Positional(_) => {
                        let n = b.next_destructure_temp();
                        let temp = pool.intern_str(&format!("__ryo_destructure_{n}"));
                        plan.push((i as u32, Some(temp)));
                        nested.push((*elem, temp, elem_pat.span));
                    }
                    // Literal and variant patterns parse only in match
                    // arms; the destructure grammar cannot produce them
                    // (see `grammar/match_.rs`).
                    ast::PatternKind::Literal(_) | ast::PatternKind::Variant { .. } => {
                        unreachable!("destructure patterns bind or skip only (match arms only)")
                    }
                }
            }
            out.push(b.destructure(value, true, &plan, span));
            for (elem, temp, elem_span) in nested {
                let elem_value = b.temp_var_ref(temp, elem_span);
                lower_destructure_pattern(b, ast, pool, elem, elem_value, elem_span, sink, out);
            }
        }
        ast::PatternKind::Anon { fields } => {
            let mut plan: Vec<(u32, Option<StringId>)> = Vec::new();
            for field in ast.pattern_field_list(*fields) {
                let bind = match field.kind {
                    // A pun binds a local named after the field.
                    ast::PatternFieldKind::Pun => Some(field.name),
                    ast::PatternFieldKind::Rename(bind) => Some(bind),
                    ast::PatternFieldKind::Wildcard => None,
                    // The destructure grammar cannot produce literal
                    // fields — they parse only in match arms. Defensive
                    // rejection rather than a silent skip.
                    ast::PatternFieldKind::Literal(_) => {
                        sink.emit(Diag::error(
                            pat.span,
                            DiagCode::ParseError,
                            "literal patterns are valid only in match arms",
                        ));
                        None
                    }
                };
                plan.push((field.name.raw(), bind));
            }
            out.push(b.destructure(value, false, &plan, span));
        }
        ast::PatternKind::Wildcard | ast::PatternKind::Bind(_) => {
            unreachable!("parser guarantees a destructure target is a shaped pattern")
        }
        ast::PatternKind::Literal(_) | ast::PatternKind::Variant { .. } => {
            unreachable!("destructure patterns bind or skip only (match arms only)")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn gen_stmt(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmt: ast::StmtId,
    names: &Names,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    out: &mut Vec<InstRef>,
) {
    gen_stmt_d(b, ast, stmt, names, pool, sink, out, 0);
}

#[allow(clippy::too_many_arguments)]
fn gen_stmt_d(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmt: ast::StmtId,
    names: &Names,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    out: &mut Vec<InstRef>,
    depth: u32,
) {
    if depth == expr::expr_depth_limit() {
        out.extend(expr::gen_stmt_machine(
            b, ast, stmt, names, pool, sink, depth,
        ));
        return;
    }
    let span = ast.stmt_span(stmt);
    match &ast.stmt(stmt).kind {
        ast::StmtKind::VarDecl(decl) => {
            let initializer =
                expr::gen_expr_d(b, ast, decl.initializer, names, pool, sink, depth + 1);
            let type_expr = decl
                .type_annotation
                .as_ref()
                .map(|annotation| copy_type_expr(b, ast, annotation));
            let r = b.var_decl(decl.name.name, decl.mutable, type_expr, initializer, span);
            out.push(r);
        }
        ast::StmtKind::Return(value) => match value {
            Some(expr) => {
                let value = expr::gen_expr_d(b, ast, *expr, names, pool, sink, depth + 1);
                out.push(b.unary(InstTag::Return, value, span));
            }
            None => {
                out.push(b.return_void(span));
            }
        },
        ast::StmtKind::ExprStmt(value) => {
            let value = expr::gen_expr_d(b, ast, *value, names, pool, sink, depth + 1);
            out.push(b.unary(InstTag::ExprStmt, value, span));
        }
        ast::StmtKind::FunctionDef(_) => {
            sink.emit(Diag::error(
                span,
                DiagCode::NestedFunctionDef,
                "nested function definitions are not supported",
            ));
        }
        ast::StmtKind::AssignOrDecl { target, value } => {
            let value_ref = expr::gen_expr_d(b, ast, *value, names, pool, sink, depth + 1);
            let r = b.assign_or_decl(target.name, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::CompoundAssign { target, op, value } => {
            let value_ref = expr::gen_expr_d(b, ast, *value, names, pool, sink, depth + 1);
            let r = b.compound_assign(target.name, *op, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::FieldAssign { target, value } => {
            let target_ref = expr::gen_expr_d(b, ast, *target, names, pool, sink, depth + 1);
            let value_ref = expr::gen_expr_d(b, ast, *value, names, pool, sink, depth + 1);
            let r = b.field_assign(target_ref, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::CompoundFieldAssign { target, op, value } => {
            let target_ref = expr::gen_expr_d(b, ast, *target, names, pool, sink, depth + 1);
            let value_ref = expr::gen_expr_d(b, ast, *value, names, pool, sink, depth + 1);
            let r = b.compound_field_assign(target_ref, *op, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::Destructure { target, value } => {
            let value_ref = expr::gen_expr_d(b, ast, *value, names, pool, sink, depth + 1);
            lower_destructure_pattern(b, ast, pool, *target, value_ref, span, sink, out);
        }
        ast::StmtKind::IfStmt(if_stmt) => {
            let cond = expr::gen_expr_d(b, ast, if_stmt.cond, names, pool, sink, depth + 1);
            let then_stmts =
                lower_block_d(b, ast, if_stmt.then_block, names, pool, sink, depth + 1);

            let elif_branches: Vec<_> = ast
                .elif_list(if_stmt.elif_branches)
                .iter()
                .map(|elif| {
                    let elif_cond =
                        expr::gen_expr_d(b, ast, elif.cond, names, pool, sink, depth + 1);
                    let elif_body = lower_block_d(b, ast, elif.block, names, pool, sink, depth + 1);
                    (elif_cond, elif_body)
                })
                .collect();

            let else_stmts = if_stmt
                .else_block
                .map(|stmts| lower_block_d(b, ast, stmts, names, pool, sink, depth + 1));

            let r = b.if_stmt(
                cond,
                &then_stmts,
                &elif_branches,
                else_stmts.as_deref(),
                span,
            );
            out.push(r);
        }
        ast::StmtKind::WhileLoop { cond, body } => {
            let cond_ref = expr::gen_expr_d(b, ast, *cond, names, pool, sink, depth + 1);
            let body_refs = lower_block_d(b, ast, *body, names, pool, sink, depth + 1);
            let r = b.while_loop(cond_ref, &body_refs, span);
            out.push(r);
        }
        ast::StmtKind::ForRange {
            var,
            iterator,
            start,
            end,
            body,
        } => {
            // `names.range` is interned once by `generate`; the
            // per-loop probe is a `StringId` compare (the `pool.str`
            // survives only in the diagnostic).
            if iterator.name != names.range {
                sink.emit(Diag::error(
                    iterator.span,
                    DiagCode::ParseError,
                    format!(
                        "only `range(...)` is supported in `for` loops in v0.1, got `{}`",
                        pool.str(iterator.name),
                    ),
                ));
            }
            let start_ref = expr::gen_expr_d(b, ast, *start, names, pool, sink, depth + 1);
            let end_ref = expr::gen_expr_d(b, ast, *end, names, pool, sink, depth + 1);
            let body_refs = lower_block_d(b, ast, *body, names, pool, sink, depth + 1);
            let r = b.for_range(var.name, start_ref, end_ref, &body_refs, span);
            out.push(r);
        }
        ast::StmtKind::Break => {
            let r = b.break_stmt(span);
            out.push(r);
        }
        ast::StmtKind::Continue => {
            let r = b.continue_stmt(span);
            out.push(r);
        }
        // Unparseable statement recovered by the parser. The parse
        // diagnostic was already emitted; lower it to a UIR
        // `Unreachable` sentinel so sema's TIR `Unreachable`
        // suppression keeps it from cascading (e.g. a spurious
        // E0036 missing-return when the broken statement was the
        // body's only `return`).
        ast::StmtKind::Error => {
            let r = b.unreachable(span);
            out.push(r);
        }
        // Type declarations are filtered out before lowering.
        ast::StmtKind::StructDef(_) => {}
        ast::StmtKind::EnumDef(_) => {}
        ast::StmtKind::ErrorDef(_) => {}
        ast::StmtKind::ErrorFamilyDef(def) => {
            sink.emit(Diag::error(
                def.name.span,
                DiagCode::ParseError,
                "nested error family declarations are not supported",
            ));
        }
    }
}

mod expr;
mod match_;
mod variants;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod enum_tests;

#[cfg(test)]
mod walker_tests;

#[cfg(test)]
mod match_tests;

#[cfg(test)]
mod type_literal_tests {
    use super::tests::{body_named, parse_and_lower};
    use super::*;

    fn source_span(src: &str, text: &str) -> Span {
        let start = src.find(text).expect("source fragment must exist");
        Span::new(start, start + text.len())
    }

    #[test]
    fn astgen_preserves_parameter_return_and_variable_type_expressions() {
        use ryo_core::uir::UirTypeExprKind;

        let src = "fn inspect(input: Later, shape: {item: (int, str)}) -> Failure!{result: (bool, bytes)}:\n\tlocal: {nested: (Later, int)} = input\n\
                   fn infer() -> !{value: (int, str)}:\n\treturn {value=(1, \"x\")}\n\
                   struct Later:\n\tvalue: int\n\
                   error Failure\n";
        let (uir, pool) = parse_and_lower(src).expect("annotations stay unresolved in AstGen");
        let inspect = body_named(&uir, &pool, "inspect");

        let input = uir.type_expr(inspect.params[0].type_expr);
        let UirTypeExprKind::Name { name, is_view } = input.kind else {
            panic!("parameter must retain its source name");
        };
        assert_eq!(pool.str(name), "Later");
        assert!(!is_view);
        assert_eq!(input.span, source_span(src, "Later"));

        let shape = uir.type_expr(inspect.params[1].type_expr);
        assert_eq!(shape.span, source_span(src, "{item: (int, str)}"));
        let UirTypeExprKind::Anon { fields } = shape.kind else {
            panic!("parameter must retain its anonymous shape");
        };
        let fields = uir.type_field_list(fields);
        assert_eq!(fields.len(), 1);
        assert_eq!(pool.str(fields[0].0), "item");
        let item = uir.type_expr(fields[0].1);
        assert_eq!(item.span, source_span(src, "(int, str)"));
        let UirTypeExprKind::Positional(elements) = item.kind else {
            panic!("anonymous field must retain positional elements");
        };
        let elements = uir.type_expr_list(elements);
        assert_eq!(elements.len(), 2);
        for (&element, expected) in elements.iter().zip(["int", "str"]) {
            let element = uir.type_expr(element);
            assert_eq!(element.span, source_span(src, expected));
            let UirTypeExprKind::Name { name, is_view } = element.kind else {
                panic!("positional element must retain its source name");
            };
            assert_eq!(pool.str(name), expected);
            assert!(!is_view);
        }

        let return_expr = uir.type_expr(inspect.return_type.expect("return annotation"));
        assert_eq!(
            return_expr.span,
            source_span(src, "Failure!{result: (bool, bytes)}")
        );
        let UirTypeExprKind::ErrorUnion {
            error_set,
            success,
            inferred,
        } = return_expr.kind
        else {
            panic!("return must retain its error union");
        };
        assert!(!inferred);
        let error_set = uir.type_expr_list(error_set);
        assert_eq!(error_set.len(), 1);
        let UirTypeExprKind::Name { name, .. } = uir.type_expr(error_set[0]).kind else {
            panic!("error member must retain its source name");
        };
        assert_eq!(pool.str(name), "Failure");
        let UirTypeExprKind::Anon { fields } = uir.type_expr(success).kind else {
            panic!("union success must retain its anonymous shape");
        };
        let result_fields = uir.type_field_list(fields);
        assert_eq!(pool.str(result_fields[0].0), "result");
        let UirTypeExprKind::Positional(result) = uir.type_expr(result_fields[0].1).kind else {
            panic!("union success field must retain positional elements");
        };
        assert_eq!(uir.type_expr_list(result).len(), 2);

        let variable = uir.var_decl_view(uir.body_stmts(inspect).at(0));
        let local = uir.type_expr(variable.type_expr.expect("variable annotation"));
        assert_eq!(local.span, source_span(src, "{nested: (Later, int)}"));
        let UirTypeExprKind::Anon { fields } = local.kind else {
            panic!("variable must retain its anonymous shape");
        };
        let nested_fields = uir.type_field_list(fields);
        assert_eq!(pool.str(nested_fields[0].0), "nested");
        let UirTypeExprKind::Positional(nested) = uir.type_expr(nested_fields[0].1).kind else {
            panic!("variable field must retain positional elements");
        };
        let nested = uir.type_expr_list(nested);
        assert_eq!(nested.len(), 2);
        let UirTypeExprKind::Name { name, .. } = uir.type_expr(nested[0]).kind else {
            panic!("variable child must retain its forward nominal name");
        };
        assert_eq!(pool.str(name), "Later");

        let infer = body_named(&uir, &pool, "infer");
        let inferred_expr = uir.type_expr(infer.return_type.expect("inferred return annotation"));
        assert_eq!(inferred_expr.span, source_span(src, "!{value: (int, str)}"));
        let UirTypeExprKind::ErrorUnion {
            error_set,
            success,
            inferred,
        } = inferred_expr.kind
        else {
            panic!("inferred return must retain its error union");
        };
        assert!(inferred);
        assert!(uir.type_expr_list(error_set).is_empty());
        let UirTypeExprKind::Anon { fields } = uir.type_expr(success).kind else {
            panic!("inferred success must retain its anonymous shape");
        };
        let fields = uir.type_field_list(fields);
        assert_eq!(pool.str(fields[0].0), "value");
        let UirTypeExprKind::Positional(elements) = uir.type_expr(fields[0].1).kind else {
            panic!("inferred success field must retain positional elements");
        };
        assert_eq!(uir.type_expr_list(elements).len(), 2);
    }

    #[test]
    fn uir_dump_shows_unresolved_type_expressions() {
        let (uir, pool) = parse_and_lower(
            "fn convert(value: Later) -> Failure!{pair: (int, str)}:\n\tlocal: (Later, int) = value\n",
        )
        .expect("AstGen must not resolve annotation names");
        assert_eq!(
            uir.dump(&pool).to_string(),
            "fn convert(value: Later) -> Failure!{pair: (int, str)}\n  body: %2\nfn main() -> void\n  body:\n\ninstructions:\n  %1 = var value\n  %2 = var_decl local: (Later, int) = %1\n"
        );
    }
}
