use super::candidates::{Candidate, ResolvedEnumLit, resolve_variant};
use super::{
    FuncCtx, NeverMode, Scope, Sema, SemaExprContext, analyze_expr_with, field_list, methods,
    ordinal,
};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::extra::enum_lit_extra;
use ryo_core::tir::TirRef;
use ryo_core::types::VariantKind;
use ryo_core::uir::{InstRef, Span};

pub(super) fn analyze_variant_construct(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
    context: SemaExprContext,
) -> TirRef {
    let candidate = resolve_variant(sema, scope, r);
    analyze_variant_candidate_with_skips(sema, fcx, scope, r, span, context, candidate, None)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn analyze_variant_candidate_with_skips(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
    context: SemaExprContext,
    candidate: Candidate,
    diagnosed: Option<std::collections::HashSet<usize>>,
) -> TirRef {
    match candidate {
        Candidate::Enum(resolved) => {
            analyze_resolved_enum(sema, fcx, scope, r, span, context, resolved, diagnosed)
        }
        Candidate::Error(error) => super::super::errors::analyze_resolved_error_construct(
            sema, fcx, scope, span, context, error, diagnosed,
        ),
        Candidate::Method => {
            let view = sema.uir.enum_lit_view(r);
            let args: Vec<_> = view.fields().map(|(_, value)| value).collect();
            methods::analyze_uppercase_candidate(
                sema,
                fcx,
                scope,
                view.enum_name,
                view.enum_span,
                view.variant,
                &args,
                span,
                context,
            )
        }
        Candidate::Recovery { analyze_args } => {
            if analyze_args {
                let values: Vec<_> = sema
                    .uir
                    .enum_lit_view(r)
                    .fields()
                    .map(|(_, value)| value)
                    .collect();
                for value in values {
                    analyze_expr_with(sema, fcx, scope, value, NeverMode::Reject, context.child());
                }
            }
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
        Candidate::Call | Candidate::Variable(_) => {
            unreachable!("qualified candidates do not use unqualified fallbacks")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze_resolved_enum(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
    context: SemaExprContext,
    resolved: ResolvedEnumLit,
    diagnosed: Option<std::collections::HashSet<usize>>,
) -> TirRef {
    let view = sema.uir.enum_lit_view(r);
    let ety = resolved.ty;
    let (ename, vname, kind, fields) = {
        let eview = sema.pool.enum_view(ety);
        let variant = eview
            .variants()
            .nth(resolved.variant_index as usize)
            .expect("resolved variant index");
        (
            eview.name(),
            variant.name,
            variant.kind,
            variant.fields.to_vec(),
        )
    };
    if view.form.positional && kind == VariantKind::Named {
        let brace_form = fields
            .iter()
            .map(|f| format!("{}=...", sema.pool.str(f.name)))
            .collect::<Vec<_>>()
            .join(", ");
        sema.sink.emit(Diag::error(
            span,
            DiagCode::PositionalConstructOnNamedVariant,
            format!(
                "variant '{}' of enum '{}' has named fields — construct it as {}.{}{{{}}}",
                sema.pool.str(vname),
                sema.pool.str(ename),
                sema.pool.str(ename),
                sema.pool.str(vname),
                brace_form,
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if kind == VariantKind::Unit && view.form.positional {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::UnitVariantConstructed,
            format!(
                "'{}' is a unit variant — construct it as {}.{}",
                sema.pool.str(vname),
                sema.pool.str(ename),
                sema.pool.str(vname),
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let mut by_index: Vec<Option<TirRef>> = vec![None; fields.len()];
    let mut field_errored = false;
    for (index, (fidx, value_ref)) in resolved.fields.iter().copied().enumerate() {
        let skip = diagnosed
            .as_ref()
            .is_some_and(|fields| fields.contains(&index));
        let fspan = sema.uir.span(value_ref);
        if fidx == enum_lit_extra::ORPHANED_FIELD {
            field_errored = true;
            analyze_expr_with(
                sema,
                fcx,
                scope,
                value_ref,
                NeverMode::Reject,
                context.child(),
            );
            continue;
        }
        let Some(field) = fields.get(fidx as usize) else {
            field_errored = true;
            let message = if kind == VariantKind::Tuple {
                format!(
                    "found a {} positional argument — '{}' takes {}",
                    ordinal(fidx as usize + 1),
                    sema.pool.str(vname),
                    fields.len(),
                )
            } else {
                format!(
                    "variant '{}' of enum '{}' has no field '{}' (fields: {})",
                    sema.pool.str(vname),
                    sema.pool.str(ename),
                    fidx,
                    field_list(sema.pool, &fields),
                )
            };
            if !skip {
                sema.sink
                    .emit(Diag::error(fspan, DiagCode::UnknownVariantField, message));
            }
            analyze_expr_with(
                sema,
                fcx,
                scope,
                value_ref,
                NeverMode::Reject,
                context.child(),
            );
            continue;
        };
        if by_index[fidx as usize].is_some() {
            field_errored = true;
            if !skip {
                sema.sink.emit(Diag::error(
                    fspan,
                    DiagCode::DuplicateVariantField,
                    format!(
                        "field '{}' is specified more than once in variant '{}' of enum '{}'",
                        sema.pool.str(field.name),
                        sema.pool.str(vname),
                        sema.pool.str(ename),
                    ),
                ));
            }
            analyze_expr_with(
                sema,
                fcx,
                scope,
                value_ref,
                NeverMode::Reject,
                context.child(),
            );
            continue;
        }
        let value = analyze_expr_with(
            sema,
            fcx,
            scope,
            value_ref,
            NeverMode::Reject,
            context.child(),
        );
        let vty = fcx.builder.ty_of(value);
        if !skip && !sema.pool.compatible(vty, field.ty) {
            let message = if kind == VariantKind::Tuple {
                format!(
                    "positional argument {} of variant '{}' of enum '{}': expected '{}', found '{}'",
                    fidx + 1,
                    sema.pool.str(vname),
                    sema.pool.str(ename),
                    sema.pool.display(field.ty),
                    sema.pool.display(vty),
                )
            } else {
                format!(
                    "field '{}' of variant '{}' of enum '{}': expected '{}', found '{}'",
                    sema.pool.str(field.name),
                    sema.pool.str(vname),
                    sema.pool.str(ename),
                    sema.pool.display(field.ty),
                    sema.pool.display(vty),
                )
            };
            sema.sink
                .emit(Diag::error(fspan, DiagCode::TypeMismatch, message));
        }
        by_index[fidx as usize] = Some(value);
    }

    let missing: Vec<String> = fields
        .iter()
        .zip(&by_index)
        .filter(|(_, slot)| slot.is_none())
        .map(|(f, _)| format!("'{}'", sema.pool.str(f.name)))
        .collect();
    if !missing.is_empty() && !field_errored {
        let message = if kind == VariantKind::Tuple {
            let total = fields.len();
            let got = total - missing.len();
            format!(
                "'{}' takes {} positional argument{}; got {}",
                sema.pool.str(vname),
                total,
                if total == 1 { "" } else { "s" },
                got,
            )
        } else {
            format!(
                "missing field(s) {} in variant '{}' of enum '{}' construction",
                missing.join(", "),
                sema.pool.str(vname),
                sema.pool.str(ename),
            )
        };
        sema.sink
            .emit(Diag::error(span, DiagCode::MissingVariantFields, message));
    }

    let error_ty = sema.pool.error_type();
    let args: Vec<(u32, TirRef)> = by_index
        .into_iter()
        .enumerate()
        .map(|(i, slot)| {
            let value = slot.unwrap_or_else(|| fcx.builder.unreachable(error_ty, span));
            (i as u32, value)
        })
        .collect();
    fcx.builder.enum_lit(
        ety,
        resolved.variant_index,
        view.form.positional,
        &args,
        span,
    )
}
