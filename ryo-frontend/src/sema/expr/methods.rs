//! Method-call cold paths of expression analysis, split from
//! `expr/mod.rs` (the 2000-line file cap keeps these out of it): the
//! lowercase-enum/struct-receiver recovery, the generalized
//! "X has no method 'Y'" diagnostic, and the `to_str`/`to_bytes`
//! bridging-method lowering. The dispatch itself lives in
//! `expr/mod.rs` and compares interned ids; these paths materialize
//! name strings and diagnostics only on failure.

use super::{FuncCtx, NeverMode, Scope, Sema, SemaExprContext, analyze_expr_with};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{ParamMode, TirRef};
use ryo_core::types::{StringId, TypeId, TypeKind, ViewKind};
use ryo_core::uir::{InstData, InstRef, InstTag, MethodCallView, Span};

/// M11: `color.red(5)` where `color` is an enum — the parser only
/// claims uppercase-led receivers for variant construction (spec §1
/// PascalCase convention; a lowercase receiver keeps its method-call
/// meaning), so a lowercase-named enum lands in the method-call path
/// with its *type* as the receiver. Name the enum and point at the
/// constructor spelling instead of the misleading "undefined
/// variable". A bound variable of the same name still wins, matching
/// the FieldAccess reinterpretation. Returns the error-typed
/// recovery TIR when the check fired, `None` to continue ordinary
/// method analysis.
pub(super) fn lowercase_enum_method_receiver_recovery(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    receiver: InstRef,
    method: StringId,
    span: Span,
) -> Option<TirRef> {
    if let InstTag::Var = sema.uir.inst(receiver).tag {
        let name = match sema.uir.inst(receiver).data {
            InstData::Var(name) => name,
            _ => unreachable!("Var must carry InstData::Var"),
        };
        if scope.lookup(name).is_none() && sema.enum_types.contains_key(&name) {
            let raw = sema.pool.str(name);
            let mut chars = raw.chars();
            let pascal = match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            };
            sema.sink.emit(
                Diag::error(
                    span,
                    DiagCode::UnknownEnum,
                    format!("'{raw}' is an enum, not a value"),
                )
                .with_help(format!(
                    "types are PascalCase by convention: declare the enum as \
                     '{pascal}' and construct '{pascal}.{}(...)'",
                    sema.pool.str(method),
                )),
            );
            return Some(fcx.builder.unreachable(sema.pool.error_type(), span));
        }
        if scope.lookup(name).is_none()
            && let Some(&sty) = sema.struct_types.get(&name)
        {
            let raw = sema.pool.str(name).to_string();
            let example = {
                let fields = sema.pool.struct_view(sty).fields;
                let names = fields
                    .iter()
                    .map(|f| format!("{}=...", sema.pool.str(f.name)))
                    .collect::<Vec<_>>();
                if names.is_empty() {
                    "{}".to_string()
                } else {
                    format!("{{{}}}", names.join(", "))
                }
            };
            sema.sink.emit(Diag::error(
                span,
                DiagCode::NotAStruct,
                format!("'{raw}' is a struct, not an enum — construct it as {raw}{example}"),
            ));
            return Some(fcx.builder.unreachable(sema.pool.error_type(), span));
        }
    }
    None
}

pub(super) fn lowercase_enum_method_receiver(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    view: &MethodCallView<'_>,
    span: Span,
    context: SemaExprContext,
) -> Option<TirRef> {
    let recovery =
        lowercase_enum_method_receiver_recovery(sema, fcx, scope, view.receiver, view.name, span);
    if recovery.is_some() {
        for arg in &view.args {
            analyze_expr_with(sema, fcx, scope, arg, NeverMode::Reject, context.child());
        }
    }
    recovery
}

/// Unknown receiver method: the generalized "X has no method 'Y'"
/// diagnostic (M8.4 family). The name string is materialized only on
/// this cold path — the dispatch itself compares interned ids.
pub(super) fn unknown_method_error(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    span: Span,
    receiver_ty: TypeId,
    name_id: StringId,
) -> TirRef {
    let name = sema.pool.str(name_id);
    sema.sink.emit(Diag::error(
        span,
        DiagCode::UndefinedFunction,
        format!(
            "{} has no method '{}'",
            sema.pool.display(receiver_ty),
            name
        ),
    ));
    fcx.builder.unreachable(sema.pool.error_type(), span)
}

/// M8.4.2 bridging methods: `bytes`/`bytesview`.to_str() lowers to
/// `__ryo_bytes_to_str` and returns `Utf8Error!str`;
/// `str`/`strview`.to_bytes() lowers to `__ryo_str_to_bytes`.
/// Wrong-family receivers keep the
/// generalized "X has no method 'Y'" diagnostic. Only called for the
/// `to_str` / `to_bytes` names.
pub(super) fn bridge_method_call(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    method_name: &str,
    args_empty: bool,
    receiver_tir: TirRef,
    receiver_ty: TypeId,
    span: Span,
) -> TirRef {
    debug_assert!(matches!(method_name, "to_str" | "to_bytes"));
    if !args_empty {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!("{method_name}() takes no arguments"),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    let (callee_name, ret_ty) = match method_name {
        "to_str" => {
            if !matches!(
                sema.pool.kind(receiver_ty),
                TypeKind::Bytes | TypeKind::View(ViewKind::Bytes)
            ) {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UndefinedFunction,
                    format!(
                        "{} has no method '{}'",
                        sema.pool.display(receiver_ty),
                        method_name
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            (
                "__ryo_bytes_to_str",
                super::super::builtins::utf8_error_union(sema),
            )
        }
        _ => {
            if !matches!(
                sema.pool.kind(receiver_ty),
                TypeKind::Str | TypeKind::View(ViewKind::Str)
            ) {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UndefinedFunction,
                    format!(
                        "{} has no method '{}'",
                        sema.pool.display(receiver_ty),
                        method_name
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            ("__ryo_str_to_bytes", sema.pool.bytes())
        }
    };
    let callee = sema.pool.intern_str(callee_name);
    fcx.builder
        .builtin_call(callee, &[receiver_tir], &[ParamMode::Borrow], ret_ty, span)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn analyze_uppercase_candidate(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    receiver_name: StringId,
    receiver_span: Span,
    method: StringId,
    args: &[InstRef],
    span: Span,
    context: SemaExprContext,
    precomputed_receiver: Option<TirRef>,
) -> TirRef {
    if precomputed_receiver.is_none()
        && let Some(recovery) =
            type_method_receiver_recovery(sema, fcx, scope, receiver_name, method, span)
    {
        for &arg in args {
            analyze_expr_with(sema, fcx, scope, arg, NeverMode::Reject, context.child());
        }
        return recovery;
    }
    let receiver = precomputed_receiver
        .unwrap_or_else(|| super::resolve_var_read(sema, fcx, scope, receiver_name, receiver_span));
    analyze_method_values(sema, fcx, scope, receiver, method, args, span, context)
}

pub(super) fn analyze_method_call(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    view: &MethodCallView<'_>,
    span: Span,
    context: SemaExprContext,
) -> TirRef {
    if let Some(recovery) = lowercase_enum_method_receiver(sema, fcx, scope, view, span, context) {
        return recovery;
    }
    let receiver = analyze_expr_with(
        sema,
        fcx,
        scope,
        view.receiver,
        NeverMode::Reject,
        context.child(),
    );
    let args: Vec<_> = view.args.iter().collect();
    analyze_method_values(sema, fcx, scope, receiver, view.name, &args, span, context)
}

#[allow(clippy::too_many_arguments)]
fn analyze_method_values(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    receiver_tir: TirRef,
    method: StringId,
    args: &[InstRef],
    span: Span,
    context: SemaExprContext,
) -> TirRef {
    let receiver_ty = fcx.builder.ty_of(receiver_tir);
    let ids = sema.names;
    for &arg in args {
        analyze_expr_with(sema, fcx, scope, arg, NeverMode::Reject, context.child());
    }
    if super::super::errors::is_error_union(sema, receiver_ty) {
        let diag = if method == ids.message {
            super::super::errors::direct_error_message_diag(sema, span, receiver_ty)
        } else {
            super::super::errors::direct_use_diag(
                sema,
                span,
                DiagCode::TypeMismatch,
                receiver_ty,
                "call a method on",
            )
        };
        sema.sink.emit(diag);
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    if method == ids.message
        && matches!(
            sema.pool.kind(receiver_ty),
            TypeKind::DeclaredError | TypeKind::ErrorSet
        )
    {
        return super::super::errors::analyze_error_message(
            sema,
            fcx,
            args.is_empty(),
            receiver_tir,
            span,
        );
    }

    if !matches!(
        sema.pool.kind(receiver_ty),
        TypeKind::Str | TypeKind::Bytes | TypeKind::View(_)
    ) {
        if !sema.pool.is_error(receiver_ty) {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::TypeMismatch,
                format!("type '{}' has no methods", sema.pool.display(receiver_ty)),
            ));
        }
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    match method {
        n if n == ids.len => {
            if !args.is_empty() {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ArityMismatch,
                    format!(
                        "{}.len() takes no arguments",
                        sema.pool.display(receiver_ty)
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            fcx.builder.push_typed(
                ryo_core::tir::TirTag::StrLen,
                ryo_core::tir::TirData::UnOp(receiver_tir),
                sema.pool.int(),
                span,
            )
        }
        n if n == ids.is_empty => {
            if !args.is_empty() {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ArityMismatch,
                    format!(
                        "{}.is_empty() takes no arguments",
                        sema.pool.display(receiver_ty)
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let len_tir = fcx.builder.push_typed(
                ryo_core::tir::TirTag::StrLen,
                ryo_core::tir::TirData::UnOp(receiver_tir),
                sema.pool.int(),
                span,
            );
            let zero = fcx.builder.int_const(0, sema.pool.int(), span);
            fcx.builder.binary(
                ryo_core::tir::TirTag::ICmpEq,
                sema.pool.bool_(),
                len_tir,
                zero,
                span,
            )
        }
        n if n == ids.to_str || n == ids.to_bytes => bridge_method_call(
            sema,
            fcx,
            if n == ids.to_str {
                "to_str"
            } else {
                "to_bytes"
            },
            args.is_empty(),
            receiver_tir,
            receiver_ty,
            span,
        ),
        n if n == ids.as_bytes => {
            super::as_bytes_projection(sema, fcx, args.is_empty(), receiver_tir, receiver_ty, span)
        }
        _ => unknown_method_error(sema, fcx, span, receiver_ty, method),
    }
}

pub(super) fn type_method_receiver_recovery(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    name: StringId,
    method: StringId,
    span: Span,
) -> Option<TirRef> {
    if scope.lookup(name).is_some() {
        return None;
    }
    if sema.enum_types.contains_key(&name) {
        let raw = sema.pool.str(name);
        let mut chars = raw.chars();
        let pascal = match chars.next() {
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        };
        sema.sink.emit(
            Diag::error(
                span,
                DiagCode::UnknownEnum,
                format!("'{raw}' is an enum, not a value"),
            )
            .with_help(format!(
                "types are PascalCase by convention: declare the enum as \
                 '{pascal}' and construct '{pascal}.{}(...)'",
                sema.pool.str(method),
            )),
        );
        return Some(fcx.builder.unreachable(sema.pool.error_type(), span));
    }
    let &sty = sema.struct_types.get(&name)?;
    let raw = sema.pool.str(name).to_string();
    let example = {
        let fields = sema.pool.struct_view(sty).fields;
        let names = fields
            .iter()
            .map(|f| format!("{}=...", sema.pool.str(f.name)))
            .collect::<Vec<_>>();
        if names.is_empty() {
            "{}".to_string()
        } else {
            format!("{{{}}}", names.join(", "))
        }
    };
    sema.sink.emit(Diag::error(
        span,
        DiagCode::NotAStruct,
        format!("'{raw}' is a struct, not an enum — construct it as {raw}{example}"),
    ));
    Some(fcx.builder.unreachable(sema.pool.error_type(), span))
}
