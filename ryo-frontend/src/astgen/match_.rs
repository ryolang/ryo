//! Match-expression lowering (M12): `ExprKind::Match` →
//! [`InstTag::Match`]. Split out of `astgen.rs` so the parent file
//! stays under the file-length gate (R2). Patterns lower to
//! [`UirPattern`] builder inputs; arm bodies keep the parser's
//! normalization — a single-line expression stays a
//! [`UirArmBody::Expr`], statement and block arms become
//! [`UirArmBody::Stmts`] over per-statement lowering.
//!
//! All pattern names arrive pre-interned from the parser, so lowering
//! is shape mapping only: pun binds the field name, rename binds the
//! alias, wildcard binds nothing, literal fields carry their
//! [`LitPattern`][ast::LitPattern] into the match pattern wire (R2 —
//! never through `InstTag::Destructure`, which stays M10-only). No
//! type checking here: sema resolves patterns against the scrutinee.

use super::*;
use ryo_core::uir::{
    UirAnonField, UirArgTarget, UirArmBody, UirFieldTarget, UirMatchArm, UirPattern,
};

/// One arm's lowered pattern. Owns the vectors the borrowing
/// [`UirPattern`] builder input points into while `match_expr` copies
/// them into the `extra` arena.
enum OwnedPattern {
    Wildcard,
    Literal(ast::LitPattern),
    Anon {
        positional: bool,
        fields: Vec<UirAnonField>,
    },
    Variant {
        enum_name: Option<StringId>,
        variant: StringId,
        args: Option<OwnedVariantArgs>,
    },
}

/// Owned form of [`UirVariantArgs`]; see [`OwnedPattern`].
struct OwnedVariantArgs {
    by_position: bool,
    args: Vec<(Option<StringId>, UirArgTarget)>,
}

/// One arm's lowered body: the expression ref, or the statement refs
/// in order.
enum OwnedBody {
    Expr(InstRef),
    Stmts(Vec<InstRef>),
}

impl OwnedPattern {
    fn view(&self) -> UirPattern<'_> {
        match self {
            OwnedPattern::Wildcard => UirPattern::Wildcard,
            OwnedPattern::Literal(lit) => UirPattern::Literal(*lit),
            OwnedPattern::Anon { positional, fields } => UirPattern::Anon {
                positional: *positional,
                fields,
            },
            OwnedPattern::Variant {
                enum_name,
                variant,
                args,
            } => UirPattern::Variant {
                enum_name: *enum_name,
                variant: *variant,
                args: args.as_ref().map(|a| ryo_core::uir::UirVariantArgs {
                    by_position: a.by_position,
                    args: &a.args,
                }),
            },
        }
    }
}

impl OwnedBody {
    fn view(&self) -> UirArmBody<'_> {
        match self {
            OwnedBody::Expr(r) => UirArmBody::Expr(*r),
            OwnedBody::Stmts(stmts) => UirArmBody::Stmts(stmts),
        }
    }
}

pub(super) struct MatchState {
    scrutinee: Option<InstRef>,
    arms: ast::MatchArmList,
    next_arm: usize,
    owned: Vec<(OwnedPattern, OwnedBody)>,
    pending_pattern: Option<OwnedPattern>,
    span: Span,
}

pub(super) enum MatchStep {
    Body(MatchState, ast::ExprId),
    Stmts(MatchState, ast::StmtList),
    Complete(InstRef),
}

pub(super) fn prepare_match(arms: ast::MatchArmList, span: Span) -> MatchState {
    MatchState {
        scrutinee: None,
        arms,
        next_arm: 0,
        owned: Vec::new(),
        pending_pattern: None,
        span,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resume_match_scrutinee(
    mut state: MatchState,
    scrutinee: InstRef,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> MatchStep {
    state.scrutinee = Some(scrutinee);
    resume_match_arm(state, b, ast, types, defined_enums, pool, sink)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resume_match_body(
    mut state: MatchState,
    body: InstRef,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> MatchStep {
    let pattern = state.pending_pattern.take().expect("match arm prepared");
    state.owned.push((pattern, OwnedBody::Expr(body)));
    resume_match_arm(state, b, ast, types, defined_enums, pool, sink)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resume_match_stmts(
    mut state: MatchState,
    body: Vec<InstRef>,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> MatchStep {
    let pattern = state.pending_pattern.take().expect("match arm prepared");
    state.owned.push((pattern, OwnedBody::Stmts(body)));
    resume_match_arm(state, b, ast, types, defined_enums, pool, sink)
}

#[allow(clippy::too_many_arguments)]
fn resume_match_arm(
    mut state: MatchState,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    types: &TypeResolver,
    _defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> MatchStep {
    let source_arms = ast.match_arms(state.arms);
    if state.next_arm < source_arms.len() {
        let arm = source_arms[state.next_arm];
        state.next_arm += 1;
        let pattern = lower_match_pattern(ast, arm.pattern, types, pool, sink);
        match arm.body {
            ast::MatchArmBody::Expr(expr) => {
                state.pending_pattern = Some(pattern);
                return MatchStep::Body(state, expr);
            }
            ast::MatchArmBody::Stmts(list) => {
                state.pending_pattern = Some(pattern);
                return MatchStep::Stmts(state, list);
            }
        }
    }
    MatchStep::Complete(finish_match(state, b, ast))
}

fn finish_match(state: MatchState, b: &mut UirBuilder, ast: &ast::Ast) -> InstRef {
    let source_arms = ast.match_arms(state.arms);
    let uir_arms: Vec<UirMatchArm<'_>> = state
        .owned
        .iter()
        .zip(source_arms)
        .map(|((pattern, body), arm)| UirMatchArm {
            span: ast.pattern_span(arm.pattern),
            pattern: pattern.view(),
            body: body.view(),
        })
        .collect();
    b.match_expr(
        state.scrutinee.expect("match scrutinee lowered"),
        &uir_arms,
        state.span,
    )
}

/// Lower `match scrutinee:` (M12) to [`InstTag::Match`]. The scrutinee
/// lowers first; each arm's body lowers next (its insts precede the
/// Match inst in the arena), then the arms are assembled as
/// borrowing builder inputs and copied into the wire by
/// [`UirBuilder::match_expr`].
#[allow(clippy::too_many_arguments)]
pub(super) fn gen_match(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    scrutinee: ast::ExprId,
    arms: ast::MatchArmList,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
) -> InstRef {
    gen_match_d(
        b,
        ast,
        scrutinee,
        arms,
        types,
        defined_enums,
        pool,
        sink,
        span,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn gen_match_d(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    scrutinee: ast::ExprId,
    arms: ast::MatchArmList,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
    depth: u32,
) -> InstRef {
    let scrutinee_ref = super::expr::gen_expr_d(
        b,
        ast,
        scrutinee,
        types,
        defined_enums,
        pool,
        sink,
        depth + 1,
    );
    let step = resume_match_scrutinee(
        prepare_match(arms, span),
        scrutinee_ref,
        b,
        ast,
        types,
        defined_enums,
        pool,
        sink,
    );
    gen_match_step(step, b, ast, types, defined_enums, pool, sink, depth)
}

#[allow(clippy::too_many_arguments)]
fn gen_match_step(
    mut step: MatchStep,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    depth: u32,
) -> InstRef {
    loop {
        match step {
            MatchStep::Complete(result) => return result,
            MatchStep::Body(state, expr) => {
                let body = super::expr::gen_expr_d(
                    b,
                    ast,
                    expr,
                    types,
                    defined_enums,
                    pool,
                    sink,
                    depth + 1,
                );
                step = resume_match_body(state, body, b, ast, types, defined_enums, pool, sink);
            }
            MatchStep::Stmts(state, list) => {
                let body =
                    lower_stmt_body(b, ast, list, types, defined_enums, pool, sink, depth + 1);
                step = resume_match_stmts(state, body, b, ast, types, defined_enums, pool, sink);
            }
        }
    }
}

pub(super) struct CatchState {
    catch: ast::CatchExpr,
    span: Span,
    operand: Option<InstRef>,
    success: Option<StringId>,
    error: Option<StringId>,
    ok: Option<StringId>,
    err: Option<StringId>,
    success_ref: Option<InstRef>,
}

pub(super) enum CatchStep {
    Body(CatchState, ast::ExprId),
    Stmts(CatchState, ast::StmtList),
    Match(CatchState, MatchStep),
}

pub(super) fn prepare_catch(catch: ast::CatchExpr, span: Span) -> CatchState {
    CatchState {
        catch,
        span,
        operand: None,
        success: None,
        error: None,
        ok: None,
        err: None,
        success_ref: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resume_catch_operand(
    mut state: CatchState,
    operand: InstRef,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> CatchStep {
    state.operand = Some(operand);
    let binding = state.catch.binding.map(|ident| ident.name);
    let success = pool.intern_str(&format!("__ryo_catch_success_{}", state.span.start));
    let error = binding
        .unwrap_or_else(|| pool.intern_str(&format!("__ryo_catch_error_{}", state.span.start)));
    state.success = Some(success);
    state.error = Some(error);
    state.ok = Some(pool.intern_str("Ok"));
    state.err = Some(pool.intern_str("Err"));
    state.success_ref = Some(b.temp_var_ref(success, state.span));
    match state.catch.body {
        ast::CatchBody::Handler(ast::MatchArmBody::Expr(expr)) => CatchStep::Body(state, expr),
        ast::CatchBody::Handler(ast::MatchArmBody::Stmts(list)) => CatchStep::Stmts(state, list),
        ast::CatchBody::Arms(arms) => {
            let error_span = state
                .catch
                .binding
                .map(|ident| ident.span)
                .unwrap_or(state.span);
            let error_ref = b.temp_var_ref(error, error_span);
            let step = resume_match_scrutinee(
                prepare_match(arms, state.span),
                error_ref,
                b,
                ast,
                types,
                defined_enums,
                pool,
                sink,
            );
            CatchStep::Match(state, step)
        }
    }
}

pub(super) fn resume_catch_body(state: CatchState, body: InstRef, b: &mut UirBuilder) -> InstRef {
    finish_catch(state, OwnedBody::Expr(body), b)
}

pub(super) fn resume_catch_stmts(
    state: CatchState,
    body: Vec<InstRef>,
    b: &mut UirBuilder,
) -> InstRef {
    finish_catch(state, OwnedBody::Stmts(body), b)
}

pub(super) fn resume_catch_match(
    state: CatchState,
    direct: InstRef,
    b: &mut UirBuilder,
) -> InstRef {
    if state.catch.binding.is_some() {
        b.mark_direct_catch_match(direct);
    }
    finish_catch(state, OwnedBody::Expr(direct), b)
}

fn finish_catch(state: CatchState, error_body: OwnedBody, b: &mut UirBuilder) -> InstRef {
    let binding = state.catch.binding.map(|ident| ident.name);
    let error_span = state
        .catch
        .binding
        .map(|ident| ident.span)
        .unwrap_or(state.span);
    let owned = [
        (
            OwnedPattern::Variant {
                enum_name: None,
                variant: state.ok.expect("catch names prepared"),
                args: Some(OwnedVariantArgs {
                    by_position: true,
                    args: vec![(
                        None,
                        UirArgTarget::Bind(state.success.expect("catch names prepared")),
                    )],
                }),
            },
            OwnedBody::Expr(state.success_ref.expect("catch success lowered")),
            state.span,
        ),
        (
            OwnedPattern::Variant {
                enum_name: None,
                variant: state.err.expect("catch names prepared"),
                args: Some(OwnedVariantArgs {
                    by_position: true,
                    args: vec![(
                        None,
                        UirArgTarget::Bind(state.error.expect("catch names prepared")),
                    )],
                }),
            },
            error_body,
            error_span,
        ),
    ];
    let arms: Vec<UirMatchArm<'_>> = owned
        .iter()
        .map(|(pattern, body, arm_span)| UirMatchArm {
            span: *arm_span,
            pattern: pattern.view(),
            body: body.view(),
        })
        .collect();
    b.catch(
        state.operand.expect("catch operand lowered"),
        binding,
        &arms,
        state.span,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn gen_catch(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    catch: ast::CatchExpr,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
) -> InstRef {
    gen_catch_d(b, ast, catch, types, defined_enums, pool, sink, span, 0)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn gen_catch_d(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    catch: ast::CatchExpr,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
    depth: u32,
) -> InstRef {
    let operand = super::expr::gen_expr_d(
        b,
        ast,
        catch.operand,
        types,
        defined_enums,
        pool,
        sink,
        depth + 1,
    );
    let step = resume_catch_operand(
        prepare_catch(catch, span),
        operand,
        b,
        ast,
        types,
        defined_enums,
        pool,
        sink,
    );
    match step {
        CatchStep::Body(state, expr) => {
            let body =
                super::expr::gen_expr_d(b, ast, expr, types, defined_enums, pool, sink, depth + 1);
            resume_catch_body(state, body, b)
        }
        CatchStep::Stmts(state, list) => {
            let body = lower_stmt_body(b, ast, list, types, defined_enums, pool, sink, depth + 1);
            resume_catch_stmts(state, body, b)
        }
        CatchStep::Match(state, step) => {
            let direct = gen_match_step(step, b, ast, types, defined_enums, pool, sink, depth);
            resume_catch_match(state, direct, b)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_stmt_body(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    list: ast::StmtList,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    depth: u32,
) -> Vec<InstRef> {
    lower_block_d(b, ast, list, types, defined_enums, pool, sink, depth)
}

/// Lower one arm pattern to its [`UirPattern`] builder-input shape.
/// Field selectors reuse the M10 destructure-plan convention (brace
/// fields select by name id, tuple elements by index); literal fields
/// ride in the pattern encoding directly (R2). Nested shapes inside
/// tuple elements are M12.5-deferred: diagnosed and recovered as a
/// wildcard so the arm still lowers.
fn lower_match_pattern(
    ast: &ast::Ast,
    pattern: ast::PatternId,
    types: &TypeResolver,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> OwnedPattern {
    let pat = ast.pattern(pattern);
    match &pat.kind {
        ast::PatternKind::Wildcard => OwnedPattern::Wildcard,
        ast::PatternKind::Literal(lit) => OwnedPattern::Literal(*lit),
        ast::PatternKind::Bind(_) => {
            // Whole-arm binds are rejected at parse time (the
            // did-you-mean diagnostic is already in the sink); recover
            // as the wildcard so the arm still lowers.
            OwnedPattern::Wildcard
        }
        ast::PatternKind::Anon { fields } => {
            let fields = ast
                .pattern_field_list(*fields)
                .iter()
                .map(|field| {
                    // Pun and rename are one bind case: the pun's
                    // target IS the field name (the M10 rule).
                    let target = match field.kind {
                        ast::PatternFieldKind::Pun => UirFieldTarget::Bind(field.name),
                        ast::PatternFieldKind::Rename(bind) => UirFieldTarget::Bind(bind),
                        ast::PatternFieldKind::Wildcard => UirFieldTarget::Wildcard,
                        ast::PatternFieldKind::Literal(lit) => UirFieldTarget::Literal(lit),
                    };
                    UirAnonField {
                        selector: field.name.raw(),
                        target,
                    }
                })
                .collect();
            OwnedPattern::Anon {
                positional: false,
                fields,
            }
        }
        ast::PatternKind::Positional(list) => {
            let fields = ast
                .pattern_list(*list)
                .iter()
                .enumerate()
                .map(|(i, elem)| {
                    let elem_pat = ast.pattern(*elem);
                    let target = match elem_pat.kind {
                        ast::PatternKind::Bind(ident) => UirFieldTarget::Bind(ident.name),
                        ast::PatternKind::Wildcard => UirFieldTarget::Wildcard,
                        ast::PatternKind::Literal(lit) => UirFieldTarget::Literal(lit),
                        ast::PatternKind::Anon { .. }
                        | ast::PatternKind::Positional(_)
                        | ast::PatternKind::Variant { .. } => {
                            sink.emit(Diag::error(
                                elem_pat.span,
                                DiagCode::ParseError,
                                "nested patterns in match arms are not yet supported",
                            ));
                            UirFieldTarget::Wildcard
                        }
                    };
                    UirAnonField {
                        selector: i as u32,
                        target,
                    }
                })
                .collect();
            OwnedPattern::Anon {
                positional: true,
                fields,
            }
        }
        ast::PatternKind::Variant {
            enum_name,
            variant,
            args,
        } => {
            let (enum_name, variant) = if let Some(root) = enum_name
                && let Some(members) = types.error_family_members.get(root)
            {
                if !members.contains(variant) {
                    let available = members
                        .iter()
                        .map(|member| format!("'{}'", pool.str(*member)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    sink.emit(Diag::error(
                        pat.span,
                        DiagCode::UnknownMatchVariant,
                        format!(
                            "error family '{}' has no member '{}' (members: {available})",
                            pool.str(*root),
                            pool.str(*variant),
                        ),
                    ));
                    return OwnedPattern::Wildcard;
                }
                (None, errors::qualified_error_name(pool, *root, *variant))
            } else {
                (*enum_name, *variant)
            };
            let args = args.map(|a| {
                let args = ast
                    .variant_arg_list(a.list)
                    .iter()
                    .map(|arg| {
                        let target = match arg.target {
                            ast::PatternVariantArgTarget::Wildcard => UirArgTarget::Wildcard,
                            ast::PatternVariantArgTarget::Bind(name) => UirArgTarget::Bind(name),
                        };
                        (arg.name, target)
                    })
                    .collect();
                OwnedVariantArgs {
                    by_position: a.by_position,
                    args,
                }
            });
            OwnedPattern::Variant {
                enum_name,
                variant,
                args,
            }
        }
    }
}
