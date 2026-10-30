use super::{FuncCtx, Scope, Sema, analyze_expr};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{
    Discriminant, MatchBinding, TirData, TirRef, TirTag, TypedMatchArm, TypedMatchBody,
};
use ryo_core::types::{StringId, TypeId, TypeKind, VariantKind};
use ryo_core::uir::{InstData, InstRef, Span};

pub(super) fn register_errors(sema: &mut Sema<'_>) {
    for decl in &sema.uir.error_decls {
        for field in &decl.payload.fields {
            if sema.pool.is_view(field.ty) {
                sema.sink.emit(Diag::error(
                    field.span,
                    DiagCode::ViewFieldType,
                    format!(
                        "error payload fields must be owned values; '{}' is a projection (Rule 6)",
                        sema.pool.display(field.ty),
                    ),
                ));
            }
        }
        sema.error_types.entry(decl.name).or_insert(decl.ty);
    }
    super::builtins::register_utf8_error(sema);
}

pub(super) fn is_error_union(sema: &Sema<'_>, ty: TypeId) -> bool {
    matches!(
        sema.pool.kind(ty),
        TypeKind::ErrorUnion | TypeKind::InferredErrorUnion
    )
}

pub(super) fn is_rejected_error_declaration(sema: &Sema<'_>, name: StringId) -> bool {
    sema.error_types.contains_key(&name)
        && !sema
            .pool
            .str(name)
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_uppercase)
}

pub(super) fn analyze_error_message(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    args_empty: bool,
    receiver: TirRef,
    span: Span,
) -> TirRef {
    if !args_empty {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            "message() takes no arguments",
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    fcx.builder.push_typed(
        TirTag::ErrorMessage,
        TirData::UnOp(receiver),
        sema.pool.str_(),
        span,
    )
}

pub(super) fn analyze_catch(
    sema: &mut Sema,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    super::match_::analyze_catch(sema, fcx, scope, r, span)
}

pub(super) fn analyze_try(
    sema: &mut Sema,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    let operand_ref = match sema.uir.inst(r).data {
        InstData::UnOp(operand) => operand,
        _ => unreachable!("Try must carry InstData::UnOp"),
    };
    let operand = analyze_expr(sema, fcx, scope, operand_ref);
    let operand_ty = fcx.builder.ty_of(operand);
    if sema.pool.is_error(operand_ty) {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if !is_error_union(sema, operand_ty) {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            format!(
                "try requires an error union operand, found '{}'",
                sema.pool.display(operand_ty)
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if !is_error_union(sema, fcx.return_type) {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            format!(
                "try requires an error union return type on the enclosing function, found '{}'",
                sema.pool.display(fcx.return_type)
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let source = sema.pool.error_union_view(operand_ty);
    let target = sema.pool.error_union_view(fcx.return_type);
    let source_members = sema.pool.error_set_view(source.error_set).members;
    let target_members = sema.pool.error_set_view(target.error_set).members;
    let target_inferred = sema.pool.kind(fcx.return_type) == TypeKind::InferredErrorUnion;
    if target_inferred {
        super::error_inference::note_dependency(sema, fcx, operand);
    }
    let missing: Vec<TypeId> = source_members
        .iter()
        .copied()
        .filter(|member| !target_inferred && !target_members.contains(member))
        .collect();
    if !missing.is_empty() {
        let members = missing
            .iter()
            .map(|member| format!("'{}'", sema.pool.display(*member)))
            .collect::<Vec<_>>()
            .join(", ");
        sema.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            format!(
                "cannot propagate '{}' through '{}': missing error {} {}",
                sema.pool.display(operand_ty),
                sema.pool.display(fcx.return_type),
                if missing.len() == 1 {
                    "member"
                } else {
                    "members"
                },
                members
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let success_name = sema
        .pool
        .intern_str(&format!("__ryo_try_success_{}", r.index()));
    let error_name = sema
        .pool
        .intern_str(&format!("__ryo_try_error_{}", r.index()));
    let success_bindings = if source.success == sema.pool.void() {
        Vec::new()
    } else {
        vec![MatchBinding {
            field_index: 0,
            name: success_name,
            ty: source.success,
        }]
    };
    let error_bindings = [MatchBinding {
        field_index: 0,
        name: error_name,
        ty: source.error_set,
    }];
    let success = if source.success == sema.pool.void() {
        fcx.builder.int_const(0, source.success, span)
    } else {
        fcx.builder.var(success_name, source.success, span)
    };
    let error = fcx.builder.var(error_name, source.error_set, span);
    let returned = coerce_to_error_union(sema, fcx, error, fcx.return_type, span);
    let early_return = fcx
        .builder
        .unary(TirTag::Return, sema.pool.void(), returned, span);
    let error_body = [early_return];
    let ok_arm = TypedMatchArm {
        span,
        discriminant: Discriminant::Variant(0),
        bindings: &success_bindings,
        guards: &[],
        body: TypedMatchBody::Expr(success),
    };
    let err_arm = TypedMatchArm {
        span,
        discriminant: Discriminant::Variant(1),
        bindings: &error_bindings,
        guards: &[],
        body: TypedMatchBody::Stmts(&error_body),
    };
    super::match_::emit_error_union_dispatch(
        fcx,
        sema.pool,
        operand,
        source.success,
        ok_arm,
        err_arm,
        span,
    )
}

pub(super) fn direct_use_diag(
    sema: &Sema<'_>,
    span: Span,
    code: DiagCode,
    ty: TypeId,
    action: &str,
) -> Diag {
    Diag::error(
        span,
        code,
        format!(
            "cannot {action} error union '{}' directly; use `try` or `catch` to unwrap it",
            sema.pool.display(ty)
        ),
    )
}

pub(super) fn direct_error_message_diag(sema: &Sema<'_>, span: Span, ty: TypeId) -> Diag {
    Diag::error(
        span,
        DiagCode::TypeMismatch,
        format!(
            "cannot call a method on error union '{}' directly",
            sema.pool.display(ty)
        ),
    )
    .with_help("use an indented `catch as err:` handler that calls `err.message()` to catch and bind the error")
}

pub(super) fn reject_direct_use(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    ty: TypeId,
    span: Span,
    code: DiagCode,
    action: &str,
) -> Option<TirRef> {
    if !is_error_union(sema, ty) {
        return None;
    }
    sema.sink
        .emit(direct_use_diag(sema, span, code, ty, action));
    Some(fcx.builder.unreachable(sema.pool.error_type(), span))
}

pub(super) fn analyze_error_construct(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    let (name, view) = sema.uir.error_construct_view(r);
    if let Some(bound) = scope.lookup(name) {
        if !view.positional && view.fields().count() == 0 {
            return fcx.builder.var(name, bound, span);
        }
        for (_, value) in view.fields() {
            analyze_expr(sema, fcx, scope, value);
        }
        sema.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            format!(
                "'{}' is a bound value, not an error constructor or callable value",
                sema.pool.str(name)
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    if sema.pool.is_error(view.ty) {
        for (_, value) in view.fields() {
            analyze_expr(sema, fcx, scope, value);
        }
        sema.sink.emit(Diag::error(
            span,
            DiagCode::NamedArgInParens,
            "named arguments are not supported in ordinary function calls",
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let (error_name, kind, fields) = {
        let errors = sema.pool.enum_view(view.ty);
        let variant = errors
            .variants()
            .next()
            .expect("declared errors have one variant");
        (errors.name(), variant.kind, variant.fields.to_vec())
    };
    if kind == VariantKind::Unit {
        if view.positional || view.fields().count() != 0 {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UnitVariantConstructed,
                format!(
                    "'{}' is a unit error — construct it as {}",
                    sema.pool.str(error_name),
                    sema.pool.str(error_name)
                ),
            ));
            return fcx.builder.unreachable(sema.pool.error_type(), span);
        }
        return fcx.builder.enum_lit(view.ty, 0, false, &[], span);
    }

    let mut by_index: Vec<Option<TirRef>> = vec![None; fields.len()];
    let mut field_errored = false;
    for (selector, value_ref) in view.fields() {
        let value = analyze_expr(sema, fcx, scope, value_ref);
        let field = if view.positional {
            fields.get(selector as usize)
        } else {
            let field_name = StringId::from_raw(selector);
            fields.iter().find(|field| field.name == field_name)
        };
        let Some(field) = field else {
            field_errored = true;
            let message = if view.positional {
                format!(
                    "'{}' takes {} positional argument{}; found an extra argument",
                    sema.pool.str(error_name),
                    fields.len(),
                    if fields.len() == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "error '{}' has no field '{}'",
                    sema.pool.str(error_name),
                    sema.pool.str(StringId::from_raw(selector))
                )
            };
            sema.sink
                .emit(Diag::error(span, DiagCode::UnknownVariantField, message));
            continue;
        };
        let index = field.idx as usize;
        if by_index[index].is_some() {
            field_errored = true;
            sema.sink.emit(Diag::error(
                sema.uir.span(value_ref),
                DiagCode::DuplicateVariantField,
                format!(
                    "field '{}' is specified more than once in error '{}'",
                    sema.pool.str(field.name),
                    sema.pool.str(error_name)
                ),
            ));
            continue;
        }
        let actual = fcx.builder.ty_of(value);
        if !sema.pool.compatible(actual, field.ty) {
            sema.sink.emit(Diag::error(
                sema.uir.span(value_ref),
                DiagCode::TypeMismatch,
                format!(
                    "field '{}' of error '{}': expected '{}', found '{}'",
                    sema.pool.str(field.name),
                    sema.pool.str(error_name),
                    sema.pool.display(field.ty),
                    sema.pool.display(actual)
                ),
            ));
        }
        by_index[index] = Some(value);
    }

    let missing: Vec<_> = fields
        .iter()
        .zip(&by_index)
        .filter(|(_, value)| value.is_none())
        .map(|(field, _)| format!("'{}'", sema.pool.str(field.name)))
        .collect();
    if !missing.is_empty() && !field_errored {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::MissingVariantFields,
            format!(
                "missing field(s) {} in error '{}' construction",
                missing.join(", "),
                sema.pool.str(error_name)
            ),
        ));
    }

    let error_ty = sema.pool.error_type();
    let args: Vec<_> = by_index
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            (
                index as u32,
                value.unwrap_or_else(|| fcx.builder.unreachable(error_ty, span)),
            )
        })
        .collect();
    fcx.builder
        .enum_lit(view.ty, 0, view.positional, &args, span)
}

pub(super) fn coerce_to_error_union(
    sema: &mut Sema,
    fcx: &mut FuncCtx,
    operand: TirRef,
    target: TypeId,
    span: Span,
) -> TirRef {
    let actual = fcx.builder.ty_of(operand);
    if actual == target || sema.pool.is_error(actual) || sema.pool.is_never(actual) {
        return operand;
    }
    debug_assert!(is_error_union(sema, target));
    let target_view = sema.pool.error_union_view(target);
    let target_members = sema.pool.error_set_view(target_view.error_set).members;
    let target_inferred = sema.pool.kind(target) == TypeKind::InferredErrorUnion;
    let subset = |members: &[TypeId]| {
        target_inferred || members.iter().all(|member| target_members.contains(member))
    };

    let (source_members, allowed) = match sema.pool.kind(actual) {
        TypeKind::DeclaredError | TypeKind::ErrorSet => {
            let members = sema.pool.error_set_view(actual).members;
            let allowed = subset(&members);
            (Some(members), allowed)
        }
        TypeKind::ErrorUnion | TypeKind::InferredErrorUnion => {
            let source = sema.pool.error_union_view(actual);
            let success_matches = sema.pool.compatible(source.success, target_view.success);
            let members = sema.pool.error_set_view(source.error_set).members;
            let allowed = success_matches && subset(&members);
            (Some(members), allowed)
        }
        _ => (None, sema.pool.compatible(actual, target_view.success)),
    };
    if allowed {
        if target_inferred {
            if let Some(members) = &source_members {
                super::error_inference::record_members(sema, fcx.decl, members);
            }
            super::error_inference::note_dependency(sema, fcx, operand);
        }
        let coercion = fcx.builder.error_union_coerce(operand, target, span);
        if target_inferred {
            super::error_inference::defer_coercion(sema, fcx.decl, coercion);
        }
        return coercion;
    }

    sema.sink.emit(Diag::error(
        span,
        DiagCode::TypeMismatch,
        format!(
            "cannot coerce '{}' to '{}': success types must match and the source error set must be a subset of the destination",
            sema.pool.display(actual),
            sema.pool.display(target)
        ),
    ));
    operand
}
