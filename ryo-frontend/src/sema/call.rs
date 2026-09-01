//! Call analysis and reserved-builtin checks — split from `mod.rs`.

use super::{
    FuncCtx, Scope, Sema, emit_builtin_call, emit_bytes_materialize, emit_str_materialize,
    materialize_name,
};
use crate::builtins;
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{ParamMode, TirRef};
use ryo_core::types::{StringId, TypeId};
use ryo_core::uir::{CallView, InstData, InstRef, InstTag, Span};

pub(crate) fn check_call(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    view: &CallView,
    arg_tirs: &[TirRef],
    span: Span,
) -> TirRef {
    let name_id = view.name;

    // M8.4.1.2: `str(view)` materialization — a call-form intercept,
    // NOT a BUILTINS-table builtin. Type names are not reserved names,
    // so a user-defined `fn str` must win over the intercept (least
    // surprise): it fires only when no user declaration carries the name.
    if sema.pool.str(name_id) == "str" && !sema.name_to_decl.contains_key(&name_id) {
        return emit_str_materialize(sema, fcx, view, arg_tirs, span);
    }

    // M8.4.2: `bytes(bview)` materialization — same call-form
    // intercept rule as `str(view)`: a user-defined `fn bytes` wins.
    if sema.pool.str(name_id) == "bytes" && !sema.name_to_decl.contains_key(&name_id) {
        return emit_bytes_materialize(sema, fcx, view, arg_tirs, span);
    }

    // Builtins short-circuit: they're not in `signatures` /
    // `name_to_decl`, so signature resolution and the worklist
    // never see them.
    if let Some(builtin) = builtins::lookup(sema.pool.str(name_id)) {
        return emit_builtin_call(sema, fcx, scope, view, arg_tirs, span, builtin);
    }

    if check_reserved_builtin(
        sema,
        name_id,
        span,
        "is a syntactic construct, not a callable function; use `for i in range(start, end):`",
    ) {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    // Demand the callee. With eagerly-resolved signatures this is
    // a check that the decl exists *and* is not currently
    // `InProgress` (cycle). Today the latter is unreachable —
    // bodies don't depend on bodies — but the call sits here so
    // future inferred-return-type / comptime work picks up cycle
    // detection for free.
    // UPDATE: We must NOT call `require_decl` here for normal function
    // calls because it breaks recursive functions. A recursive function
    // is `InProgress`, but its signature is already known!
    let callee = match sema.name_to_decl.get(&name_id).copied() {
        Some(d) => d,
        None => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UndefinedFunction,
                format!("undefined function: '{}'", sema.pool.str(name_id)),
            ));
            return fcx.builder.unreachable(sema.pool.error_type(), span);
        }
    };
    // Snapshot the callee's per-parameter modes from its UIR
    // body so we can stamp `ParamMode`s without holding a borrow on
    // `sema` through the per-argument diagnostics below.
    let callee_modes: Vec<ParamMode> = sema.uir.func_bodies[callee.index()]
        .params
        .iter()
        .map(|p| p.mode)
        .collect();

    let sig = sema
        .signatures
        .get(&name_id)
        .expect("signature must be present once decl exists");

    // Snapshot the parts of `sig` we need so we can release the
    // borrow on `sema` before emitting per-argument diagnostics.
    let expected: Vec<TypeId> = sig.params.clone();
    let return_type = sig.return_type;
    // One mode per call-site argument, stamped from the callee
    // signature's parameter modes. When the arity mismatches we
    // fall back to all-Borrow over the actual argument count so the
    // encoded payload stays well-formed on the error path.
    let modes: Vec<ParamMode> = if view.args.len() == callee_modes.len() {
        callee_modes.clone()
    } else {
        vec![ParamMode::Borrow; view.args.len()]
    };

    // Per-argument validation may replace an arg's TirRef with a
    // conversion inst (M8.4 §3.4), so the call is built from this
    // copy rather than the incoming slice.
    let mut converted: Vec<TirRef> = arg_tirs.to_vec();

    if view.args.len() != expected.len() {
        // arity must match the signature — no variadic parameters (spec §6.1.2)
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!(
                "call to '{}' has wrong arity: expected {} argument(s), got {}",
                sema.pool.str(name_id),
                expected.len(),
                view.args.len(),
            ),
        ));
    } else {
        for (idx, ((arg_uir, arg_tir), &exp_ty)) in view
            .args
            .iter()
            .zip(arg_tirs.iter())
            .zip(expected.iter())
            .enumerate()
        {
            // W0003 case A (M8.4.1.2, generalized M8.4.2): a
            // `str(view)` / `bytes(bview)` materialize call sitting
            // directly in a borrowed owner parameter's argument
            // position is redundant — the view would pass via the P6'
            // re-borrow below (cap=0, no allocation). Borrow-mode only:
            // `move`/`inout` params cannot be served by the re-borrow.
            // The `ty_of == exp_ty` guard keeps error paths
            // warning-free.
            if modes[idx] == ParamMode::Borrow
                && fcx.builder.ty_of(*arg_tir) == exp_ty
                && let Some(owner_name) = materialize_name(sema, *arg_uir)
            {
                let owner_ty = match owner_name {
                    "str" => sema.pool.str_(),
                    _ => sema.pool.bytes(),
                };
                if exp_ty == owner_ty {
                    sema.sink.emit(Diag::warning(
                        sema.uir.span(*arg_uir),
                        DiagCode::RedundantMaterialize,
                        format!(
                            "redundant `{owner_name}(...)` — views pass to `{owner_name}` parameters via the re-borrow with no allocation (drop the `{owner_name}(...)` call)"
                        ),
                    ));
                }
            }
            let actual = fcx.builder.ty_of(*arg_tir);
            let arg_tir = if sema.pool.is_view(actual)
                && sema.pool.view_owner(actual) == Some(exp_ty)
                && modes[idx] == ParamMode::Borrow
            {
                // P6': view → owner param re-borrows (cap=0, no copy),
                // call-scoped — same shape as the owner → view
                // conversion below. Borrow-mode only: a `move` param
                // would let the view escape the call (E2), and an
                // `inout` param is rejected by the `&` check below.
                let v = fcx
                    .builder
                    .view_as_owner(*arg_tir, exp_ty, sema.uir.span(*arg_uir));
                converted[idx] = v;
                v
            } else if sema.pool.owner_view(actual) == Some(exp_ty) {
                // Implicit `str → strview` view conversion (§3.4): passing an
                // owned `str` to a `strview` parameter drops the `cap` word —
                // a representation coercion, not a copy. Routed through the
                // pool's owner → view table, not a bare kind comparison.
                let v = fcx
                    .builder
                    .to_view(*arg_tir, exp_ty, sema.uir.span(*arg_uir));
                converted[idx] = v;
                v
            } else {
                *arg_tir
            };
            let actual = fcx.builder.ty_of(arg_tir);
            if !sema.pool.compatible(actual, exp_ty) {
                sema.sink.emit(Diag::error(
                    sema.uir.span(*arg_uir),
                    DiagCode::TypeMismatch,
                    format!(
                        "call to '{}': argument {} has type '{}', expected '{}'",
                        sema.pool.str(name_id),
                        idx + 1,
                        sema.pool.display(actual),
                        sema.pool.display(exp_ty),
                    ),
                ));
            }

            // --- M8.3: `&`/`inout` agreement + mutable-lvalue validation ---
            let arg_is_borrow = matches!(sema.uir.inst(*arg_uir).tag, InstTag::Borrow);
            let param_is_inout = modes[idx] == ParamMode::Inout;
            if arg_is_borrow && !param_is_inout {
                sema.sink.emit(
                    Diag::error(
                        sema.uir.span(*arg_uir),
                        DiagCode::BorrowMismatch,
                        format!(
                            "argument {} is passed by `&` but parameter is not `inout`",
                            idx + 1
                        ),
                    )
                    .with_help("remove the `&`, or declare the parameter `inout`"),
                );
            } else if !arg_is_borrow && param_is_inout {
                sema.sink.emit(
                    Diag::error(
                        sema.uir.span(*arg_uir),
                        DiagCode::BorrowMismatch,
                        format!(
                            "`inout` parameter {} requires `&` at the call site",
                            idx + 1
                        ),
                    )
                    .with_help("pass the argument as `&name` to mark the mutation"),
                );
            } else if arg_is_borrow {
                // Parameter is `inout` and arg is `&expr`: validate the
                // borrow target is an assignable lvalue.
                let inner = match sema.uir.inst(*arg_uir).data {
                    InstData::Borrow(inner) => inner,
                    _ => unreachable!("Borrow must carry InstData::Borrow"),
                };
                if let Some(reason) = borrow_target_reason(sema, scope, inner) {
                    sema.sink.emit(Diag::error(
                        sema.uir.span(*arg_uir),
                        DiagCode::BorrowMismatch,
                        format!("cannot borrow this expression as mutable: {}", reason),
                    ));
                }
            }
        }
    }
    fcx.builder
        .call(name_id, &converted, &modes, return_type, span)
}

/// Returns `None` if `inner` is an assignable lvalue (a `mut` local or
/// an `inout` parameter), else a human reason why it is not borrowable
/// as mutable (M8.3).
pub(crate) fn borrow_target_reason(
    sema: &Sema<'_>,
    scope: &Scope,
    inner: InstRef,
) -> Option<String> {
    match sema.uir.inst(inner).tag {
        InstTag::Var => {
            let name = match sema.uir.inst(inner).data {
                InstData::Var(n) => n,
                _ => unreachable!("Var must carry InstData::Var"),
            };
            match scope.lookup_full(name) {
                Some((_, true)) => None, // mutable binding (mut local or inout param)
                Some((_, false)) => {
                    Some(format!("`{}` is not declared `mut`", sema.pool.str(name)))
                }
                None => Some(format!("`{}` is not defined", sema.pool.str(name))),
            }
        }
        _ => Some("only `mut` variables can be borrowed as mutable".to_string()),
    }
}

pub(crate) fn check_reserved_builtin(
    sema: &mut Sema<'_>,
    name_id: StringId,
    span: Span,
    message: &str,
) -> bool {
    let name = sema.pool.str(name_id);
    if crate::builtins::is_reserved_name(name) {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ReservedBuiltinName,
            format!("'{}' {}", name, message),
        ));
        true
    } else {
        false
    }
}
