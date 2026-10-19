//! Method-call cold paths of expression analysis, split from
//! `expr/mod.rs` (the 2000-line file cap keeps these out of it): the
//! lowercase-enum/struct-receiver recovery, the generalized
//! "X has no method 'Y'" diagnostic, and the `to_str`/`to_bytes`
//! bridging-method lowering. The dispatch itself lives in
//! `expr/mod.rs` and compares interned ids; these paths materialize
//! name strings and diagnostics only on failure.

use super::{FuncCtx, Scope, Sema, analyze_expr};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{ParamMode, TirRef};
use ryo_core::types::{StringId, TypeId, TypeKind, ViewKind};
use ryo_core::uir::{InstData, InstRef, InstTag, Span};

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
pub(super) fn lowercase_enum_method_receiver(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    receiver: InstRef,
    method: StringId,
    args: &ryo_core::uir::ExtraList<'_, InstRef>,
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
            for arg in args {
                analyze_expr(sema, fcx, scope, arg);
            }
            return Some(fcx.builder.unreachable(sema.pool.error_type(), span));
        }
        // `Point.make(1.0)` — a struct name in enum-construct
        // position. Structs construct with braces, not `Name(...)`
        // parens; name the brace spelling with the declared fields.
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
            for arg in args {
                analyze_expr(sema, fcx, scope, arg);
            }
            return Some(fcx.builder.unreachable(sema.pool.error_type(), span));
        }
    }
    None
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
/// `__ryo_bytes_to_str` (a stopgap that panics at runtime on invalid
/// UTF-8; becomes `Utf8Error!str` at M13), `str`/`strview`.to_bytes()
/// lowers to `__ryo_str_to_bytes`. Wrong-family receivers keep the
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
            ("__ryo_bytes_to_str", sema.pool.str_())
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
        .call(callee, &[receiver_tir], &[ParamMode::Borrow], ret_ty, span)
}
