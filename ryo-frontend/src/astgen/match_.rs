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
    let scrutinee_ref = gen_expr(b, ast, scrutinee, types, defined_enums, pool, sink);
    gen_match_from_ref(
        b,
        ast,
        scrutinee_ref,
        arms,
        types,
        defined_enums,
        pool,
        sink,
        span,
    )
}

#[allow(clippy::too_many_arguments)]
fn gen_match_from_ref(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    scrutinee_ref: InstRef,
    arms: ast::MatchArmList,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
) -> InstRef {
    let source_arms = ast.match_arms(arms);
    let owned: Vec<(OwnedPattern, OwnedBody)> = source_arms
        .iter()
        .map(|arm| lower_arm(b, ast, arm, types, defined_enums, pool, sink))
        .collect();
    let uir_arms: Vec<UirMatchArm<'_>> = owned
        .iter()
        .zip(source_arms)
        .map(|((pattern, body), arm)| UirMatchArm {
            span: ast.pattern_span(arm.pattern),
            pattern: pattern.view(),
            body: body.view(),
        })
        .collect();
    b.match_expr(scrutinee_ref, &uir_arms, span)
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
    let operand = gen_expr(b, ast, catch.operand, types, defined_enums, pool, sink);
    let binding = catch.binding.map(|ident| ident.name);
    let success = pool.intern_str(&format!("__ryo_catch_success_{}", span.start));
    let error =
        binding.unwrap_or_else(|| pool.intern_str(&format!("__ryo_catch_error_{}", span.start)));
    let ok = pool.intern_str("Ok");
    let err = pool.intern_str("Err");
    let success_ref = b.temp_var_ref(success, span);
    let error_span = catch.binding.map(|ident| ident.span).unwrap_or(span);
    let error_body = match catch.body {
        ast::CatchBody::Handler(body) => lower_body(b, ast, body, types, defined_enums, pool, sink),
        ast::CatchBody::Arms(arms) => {
            let error_ref = b.temp_var_ref(error, error_span);
            let direct = gen_match_from_ref(
                b,
                ast,
                error_ref,
                arms,
                types,
                defined_enums,
                pool,
                sink,
                span,
            );
            if binding.is_some() {
                b.mark_direct_catch_match(direct);
            }
            OwnedBody::Expr(direct)
        }
    };
    let owned = [
        (
            OwnedPattern::Variant {
                enum_name: None,
                variant: ok,
                args: Some(OwnedVariantArgs {
                    by_position: true,
                    args: vec![(None, UirArgTarget::Bind(success))],
                }),
            },
            OwnedBody::Expr(success_ref),
            span,
        ),
        (
            OwnedPattern::Variant {
                enum_name: None,
                variant: err,
                args: Some(OwnedVariantArgs {
                    by_position: true,
                    args: vec![(None, UirArgTarget::Bind(error))],
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
    b.catch(operand, binding, &arms, span)
}

#[allow(clippy::too_many_arguments)]
fn lower_arm(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    arm: &ast::MatchArm,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> (OwnedPattern, OwnedBody) {
    let pattern = lower_match_pattern(ast, arm.pattern, sink);
    let body = lower_body(b, ast, arm.body, types, defined_enums, pool, sink);
    (pattern, body)
}

#[allow(clippy::too_many_arguments)]
fn lower_body(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    body: ast::MatchArmBody,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> OwnedBody {
    match body {
        ast::MatchArmBody::Expr(expr) => {
            OwnedBody::Expr(gen_expr(b, ast, expr, types, defined_enums, pool, sink))
        }
        ast::MatchArmBody::Stmts(list) => {
            let mut refs = Vec::new();
            for &stmt in ast.stmt_list(list) {
                gen_stmt(b, ast, stmt, types, defined_enums, pool, sink, &mut refs);
            }
            OwnedBody::Stmts(refs)
        }
    }
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
                enum_name: *enum_name,
                variant: *variant,
                args,
            }
        }
    }
}
