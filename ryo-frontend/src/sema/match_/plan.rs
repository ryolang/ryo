use super::super::{FuncCtx, Sema, errors};
use super::{
    Exhaustiveness, MatchAnalysis, MatchMode, check_exhaustiveness, is_matchable, resolve_arm,
};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::TirRef;
use ryo_core::types::TypeKind;
use ryo_core::uir::{InstRef, Span, UirArmBodyView};

pub(crate) struct MatchPlan {
    pub(crate) analysis: MatchAnalysis,
    pub(crate) mode: MatchMode,
    pub(crate) span: Span,
}

pub(crate) fn prepare_match(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    r: InstRef,
    catch: bool,
    scrutinee: TirRef,
    scrutinee_borrowed: bool,
) -> MatchPlan {
    let uir = sema.uir;
    let span = uir.span(r);
    let (scrutinee_ref, uir_arms, catch_binding) = if catch {
        let view = uir.catch_view(r);
        (view.scrutinee, view.arms, view.binding)
    } else {
        let view = uir.match_view(r);
        (view.scrutinee, view.arms, None)
    };
    let scrutinee_span = uir.span(scrutinee_ref);
    let mut scrutinee_ty = fcx.builder.ty_of(scrutinee);
    let mut had_error = false;

    if catch && !errors::is_error_union(sema, scrutinee_ty) {
        if !sema.pool.is_error(scrutinee_ty) {
            sema.sink.emit(Diag::error(
                scrutinee_span,
                DiagCode::TypeMismatch,
                format!(
                    "catch requires an error union operand, found '{}'",
                    sema.pool.display(scrutinee_ty)
                ),
            ));
        }
        had_error = true;
        scrutinee_ty = sema.pool.error_type();
    } else if !catch && !is_matchable(sema, scrutinee_ty) {
        if errors::is_error_union(sema, scrutinee_ty) {
            sema.sink.emit(errors::direct_use_diag(
                sema,
                scrutinee_span,
                DiagCode::InvalidMatchScrutinee,
                scrutinee_ty,
                "match on",
            ));
        } else if !sema.pool.is_error(scrutinee_ty) {
            let mut diag = Diag::error(
                scrutinee_span,
                DiagCode::InvalidMatchScrutinee,
                format!(
                    "cannot match on a value of type '{}' — match works on \
                     enums, declared errors, error sets, int, bool, str, and anonymous structs",
                    sema.pool.display(scrutinee_ty),
                ),
            );
            if sema.pool.kind(scrutinee_ty) == TypeKind::Float {
                diag = diag.with_help("float literal patterns are not yet supported");
            }
            sema.sink.emit(diag);
        }
        had_error = true;
        scrutinee_ty = sema.pool.error_type();
    }

    let mut arms = Vec::with_capacity(uir_arms.len());
    for arm in &uir_arms {
        let arm_span = arm.span;
        let body_span = match arm.body {
            UirArmBodyView::Expr(body) => uir.span(body),
            UirArmBodyView::Stmts(stmts) => stmts
                .first()
                .map(|stmt| uir.span(InstRef::from_raw(*stmt)))
                .unwrap_or(arm_span),
        };
        let analysis = resolve_arm(sema, scrutinee_ty, arm, arm_span, body_span, catch);
        had_error |= analysis.had_error;
        arms.push(analysis);
    }

    let exhaustiveness = if sema.pool.is_error(scrutinee_ty) {
        Exhaustiveness::default()
    } else {
        check_exhaustiveness(sema, scrutinee_ty, &arms, span, scrutinee_span)
    };

    MatchPlan {
        analysis: MatchAnalysis {
            had_error,
            scrutinee,
            scrutinee_borrowed,
            scrutinee_ty,
            arms,
            exhaustive: exhaustiveness.exhaustive,
            has_wildcard: exhaustiveness.has_wildcard,
        },
        span,
        mode: if catch {
            MatchMode::Catch(catch_binding)
        } else {
            MatchMode::Source
        },
    }
}
