//! Expression analysis and const-int folding — split from `mod.rs`.

use super::{FuncCtx, Scope, Sema, analyze_match, check_call};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::extra::enum_lit_extra;
use ryo_core::tir::{TirData, TirRef, TirTag};
use ryo_core::types::{StringId, StructField, TypeId, TypeKind, VariantKind, ViewKind};
use ryo_core::uir::{InstData, InstRef, InstTag, Span, StructLitView, Uir};

mod methods;
mod operators;

/// Resolve a user-spelled variable read. The `__ryo_` namespace belongs
/// to compiler-generated temporaries (nested-destructuring temps,
/// runtime shims), which astgen emits as `TempVar` — never `Var` — so
/// a `__ryo_` spelling here is USER source, and since every declaration
/// path rejects the prefix there is nothing legitimate it can refer
/// to: reject the read before it can reach any compiler side table.
/// Otherwise walk the block scopes; unknown names are a compile error
/// (spec §3, Variables).
fn resolve_var_read(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    name: StringId,
    span: Span,
) -> TirRef {
    if sema.pool.str(name).starts_with("__ryo_") {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ReservedIdentifier,
            format!(
                "identifiers starting with '__ryo_' are reserved for the compiler runtime: '{}'",
                sema.pool.str(name),
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    match scope.lookup(name) {
        Some(t) => fcx.builder.var(name, t, span),
        None => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UndefinedVariable,
                format!("undefined variable: '{}'", sema.pool.str(name)),
            ));
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
    }
}

/// Resolve a compiler-generated temporary read (see [`InstTag::TempVar`]).
/// Nested destructuring and catch lowering register every temp before
/// emitting its read — a miss is a compiler bug, reported rather than
/// panicked.
fn resolve_temp_read(sema: &mut Sema<'_>, fcx: &mut FuncCtx, name: StringId, span: Span) -> TirRef {
    match fcx.destructure_temps.get(&name).copied() {
        Some(t) => fcx.builder.var(name, t, span),
        None => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UndefinedVariable,
                format!(
                    "internal compiler error: unbound destructuring temporary '{}'",
                    sema.pool.str(name),
                ),
            ));
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
    }
}

/// Expression-position analysis. A `never`-typed result (e.g. a
/// `panic` call) is rejected: `panic` may only appear as a bare
/// statement, never where a value is required (return operand,
/// operator operands, call args, conditions, slice/range bounds).
/// All recursive descent goes through this wrapper, so the rule
/// covers every operand position uniformly. The never-tolerant
/// entry points — a bare ExprStmt and the binding sites, which run
/// their own valueless-RHS check — call [`analyze_expr_allow_never`].
pub(crate) fn analyze_expr(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
) -> TirRef {
    let t = analyze_expr_allow_never(sema, fcx, scope, r);
    if sema.pool.is_never(fcx.builder.ty_of(t)) {
        sema.sink.emit(Diag::error(
            sema.uir.span(r),
            DiagCode::VoidValueInExpression,
            "a 'never' value (e.g. `panic(...)`) can only be used as a statement".to_string(),
        ));
        return fcx
            .builder
            .unreachable(sema.pool.error_type(), sema.uir.span(r));
    }
    t
}

pub(crate) fn analyze_expr_allow_never(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
) -> TirRef {
    if let Some(t) = fcx.inst_map[r.index()] {
        return t;
    }

    let inst = sema.uir.inst(r);
    let span = sema.uir.span(r);
    let emitted = match inst.tag {
        InstTag::IntLiteral => match inst.data {
            InstData::Int(v) => fcx.builder.int_const(v, sema.pool.int(), span),
            _ => unreachable!("IntLiteral must carry InstData::Int"),
        },
        InstTag::StrLiteral => match inst.data {
            InstData::Str(s) => fcx.builder.str_const(s, sema.pool.str_(), span),
            _ => unreachable!("StrLiteral must carry InstData::Str"),
        },
        InstTag::BoolLiteral => match inst.data {
            InstData::Bool(b) => fcx.builder.bool_const(b, sema.pool.bool_(), span),
            _ => unreachable!("BoolLiteral must carry InstData::Bool"),
        },
        InstTag::FloatLiteral => match inst.data {
            InstData::Float(v) => fcx.builder.float_const(v, sema.pool.float(), span),
            _ => unreachable!("FloatLiteral must carry InstData::Float"),
        },
        InstTag::BytesLiteral => match inst.data {
            InstData::Str(s) => fcx.builder.bytes_const(s, sema.pool.bytes(), span),
            _ => unreachable!("BytesLiteral must carry InstData::Str"),
        },
        InstTag::Var => {
            let name = match inst.data {
                InstData::Var(s) => s,
                _ => unreachable!("Var must carry InstData::Var"),
            };
            resolve_var_read(sema, fcx, scope, name, span)
        }
        InstTag::TempVar => {
            let name = match inst.data {
                InstData::Var(s) => s,
                _ => unreachable!("TempVar must carry InstData::Var"),
            };
            resolve_temp_read(sema, fcx, name, span)
        }
        InstTag::Add
        | InstTag::Sub
        | InstTag::Mul
        | InstTag::Div
        | InstTag::Mod
        | InstTag::Eq
        | InstTag::NotEq
        | InstTag::Lt
        | InstTag::Gt
        | InstTag::LtEq
        | InstTag::GtEq
        | InstTag::And
        | InstTag::Or => {
            let (lhs, rhs) = match inst.data {
                InstData::BinOp { lhs, rhs } => (lhs, rhs),
                _ => unreachable!("binary op must carry InstData::BinOp"),
            };
            let l = analyze_expr(sema, fcx, scope, lhs);
            let r2 = analyze_expr(sema, fcx, scope, rhs);
            let lhs_ty = fcx.builder.ty_of(l);
            let rhs_ty = fcx.builder.ty_of(r2);
            // Constant-evaluate pure integer arithmetic for
            // diagnostics. A constant-zero divisor always panics at
            // runtime (the codegen zero-divisor guard), so reject it
            // at compile time; constant overflow is a compile error
            // per §18 (overflow traps in all build modes). Float
            // `x / 0.0` is IEEE-defined (inf) and unaffected.
            if matches!(
                inst.tag,
                InstTag::Add | InstTag::Sub | InstTag::Mul | InstTag::Div | InstTag::Mod
            ) && lhs_ty == sema.pool.int()
                && rhs_ty == sema.pool.int()
            {
                if matches!(inst.tag, InstTag::Div | InstTag::Mod)
                    && matches!(const_eval_int(sema.uir, rhs), ConstInt::Value(0))
                {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::DivisionByZero,
                        if inst.tag == InstTag::Div {
                            "division by zero".to_string()
                        } else {
                            "modulo by zero".to_string()
                        },
                    ));
                    return fcx.builder.unreachable(sema.pool.error_type(), span);
                }
                if matches!(const_eval_int(sema.uir, r), ConstInt::Overflow) {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::ConstEvalFailure,
                        "integer overflow in constant expression".to_string(),
                    ));
                    return fcx.builder.unreachable(sema.pool.error_type(), span);
                }
            }
            check_binary_op(sema, fcx, inst.tag, lhs_ty, rhs_ty, l, r2, span)
        }
        InstTag::Neg | InstTag::Not => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("unary operator must carry InstData::UnOp"),
            };
            operators::analyze_unary_operator(sema, fcx, scope, inst.tag, r, operand, span)
        }
        InstTag::Call => {
            let view = sema.uir.call_view(r);
            // Translate args first (in source order) to fix their
            // TIR refs and types, *then* validate against the
            // signature so per-argument diagnostics carry the right
            // span and the call still emits a well-formed TIR Call.
            let mut arg_tirs = Vec::with_capacity(view.args.len());
            for a in &view.args {
                arg_tirs.push(analyze_expr(sema, fcx, scope, a));
            }
            check_call(sema, fcx, scope, &view, &arg_tirs, span)
        }
        InstTag::MethodCall => {
            let view = sema.uir.method_call_view(r);
            if let Some(recovery) = methods::lowercase_enum_method_receiver(
                sema,
                fcx,
                scope,
                view.receiver,
                view.name,
                &view.args,
                span,
            ) {
                return recovery;
            }
            let receiver_tir = analyze_expr(sema, fcx, scope, view.receiver);
            let receiver_ty = fcx.builder.ty_of(receiver_tir);
            let ids = sema.names;

            for arg in &view.args {
                analyze_expr(sema, fcx, scope, arg);
            }
            if super::errors::is_error_union(sema, receiver_ty) {
                sema.sink.emit(super::errors::direct_use_diag(
                    sema,
                    span,
                    DiagCode::TypeMismatch,
                    receiver_ty,
                    "call a method on",
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }

            if view.name == ids.message
                && matches!(
                    sema.pool.kind(receiver_ty),
                    TypeKind::DeclaredError | TypeKind::ErrorSet
                )
            {
                return super::errors::analyze_error_message(
                    sema,
                    fcx,
                    view.args.is_empty(),
                    receiver_tir,
                    span,
                );
            }

            // `str`/`strview` (M8.4) and `bytes`/`bytesview` (M8.4.2)
            // have methods.
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

            match view.name {
                n if n == ids.len => {
                    if !view.args.is_empty() {
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
                        TirTag::StrLen,
                        TirData::UnOp(receiver_tir),
                        sema.pool.int(),
                        span,
                    )
                }
                n if n == ids.is_empty => {
                    if !view.args.is_empty() {
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
                        TirTag::StrLen,
                        TirData::UnOp(receiver_tir),
                        sema.pool.int(),
                        span,
                    );
                    let zero = fcx.builder.int_const(0, sema.pool.int(), span);
                    fcx.builder
                        .binary(TirTag::ICmpEq, sema.pool.bool_(), len_tir, zero, span)
                }
                n if n == ids.to_str || n == ids.to_bytes => methods::bridge_method_call(
                    sema,
                    fcx,
                    if n == ids.to_str {
                        "to_str"
                    } else {
                        "to_bytes"
                    },
                    view.args.is_empty(),
                    receiver_tir,
                    receiver_ty,
                    span,
                ),
                n if n == ids.as_bytes => as_bytes_projection(
                    sema,
                    fcx,
                    view.args.is_empty(),
                    receiver_tir,
                    receiver_ty,
                    span,
                ),
                _ => methods::unknown_method_error(sema, fcx, span, receiver_ty, view.name),
            }
        }
        InstTag::Slice => {
            let (base_uir, start_uir, end_uir) = match inst.data {
                InstData::Slice { base, start, end } => (base, start, end),
                _ => unreachable!("Slice must carry InstData::Slice"),
            };
            let base_tir = analyze_expr(sema, fcx, scope, base_uir);
            let base_ty = fcx.builder.ty_of(base_tir);
            let base_kind = sema.pool.kind(base_ty);
            // §3.2 P1: a slice projects an owner (`str`/`bytes`) or
            // re-projects an existing view (P3); anything else is not
            // sliceable.
            if !matches!(
                base_kind,
                TypeKind::Str
                    | TypeKind::View(ViewKind::Str)
                    | TypeKind::Bytes
                    | TypeKind::View(ViewKind::Bytes)
            ) && !sema.pool.is_error(base_ty)
            {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!("cannot slice type '{}'", sema.pool.display(base_ty)),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let start_tir = start_uir.map(|b| check_slice_bound(sema, fcx, scope, b));
            let end_tir = end_uir.map(|b| check_slice_bound(sema, fcx, scope, b));
            let view_ty = match base_kind {
                TypeKind::Str | TypeKind::View(ViewKind::Str) => sema.pool.str_view(),
                TypeKind::Bytes | TypeKind::View(ViewKind::Bytes) => sema.pool.bytes_view(),
                _ => sema.pool.error_type(),
            };
            fcx.builder.push_typed(
                TirTag::Slice,
                TirData::Slice {
                    base: base_tir,
                    start: start_tir,
                    end: end_tir,
                },
                view_ty,
                span,
            )
        }
        InstTag::Index => {
            let (base_uir, index_uir) = match inst.data {
                InstData::BinOp { lhs, rhs } => (lhs, rhs),
                _ => unreachable!("Index must carry InstData::BinOp"),
            };
            let base_tir = analyze_expr(sema, fcx, scope, base_uir);
            let base_ty = fcx.builder.ty_of(base_tir);
            let base_kind = sema.pool.kind(base_ty);
            // M8.4.2 stopgap: scalar indexing exists for bytes/bytesview
            // only and yields `int` (0-255) until M17.1 makes it `u8`.
            // `str` indexing stays forbidden (§4.7).
            match base_kind {
                TypeKind::Bytes | TypeKind::View(ViewKind::Bytes) => {}
                TypeKind::Str | TypeKind::View(ViewKind::Str) => {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::TypeMismatch,
                        "str does not support indexing — slice instead (s[i:i+1])".to_string(),
                    ));
                    return fcx.builder.unreachable(sema.pool.error_type(), span);
                }
                _ => {
                    if !sema.pool.is_error(base_ty) {
                        sema.sink.emit(Diag::error(
                            span,
                            DiagCode::TypeMismatch,
                            format!("cannot index type '{}'", sema.pool.display(base_ty)),
                        ));
                    }
                    return fcx.builder.unreachable(sema.pool.error_type(), span);
                }
            }
            let index_tir = analyze_expr(sema, fcx, scope, index_uir);
            let index_ty = fcx.builder.ty_of(index_tir);
            if sema.pool.kind(index_ty) != TypeKind::Int && !sema.pool.is_error(index_ty) {
                sema.sink.emit(Diag::error(
                    sema.uir.span(index_uir),
                    DiagCode::TypeMismatch,
                    format!("index must be int, got '{}'", sema.pool.display(index_ty)),
                ));
            }
            fcx.builder.binary(
                TirTag::BytesIndex,
                sema.pool.int(),
                base_tir,
                index_tir,
                span,
            )
        }
        InstTag::Borrow => analyze_borrow(sema, fcx, scope, r, inst.data, span),
        InstTag::StructLit => analyze_struct_lit(sema, fcx, scope, r, span),
        InstTag::EnumLit => analyze_variant_construct(sema, fcx, scope, r, span),
        InstTag::ErrorConstruct => {
            super::errors::analyze_error_construct(sema, fcx, scope, r, span)
        }
        InstTag::Match => {
            let emitted = analyze_match(sema, fcx, scope, r, span);
            if sema.uir.is_direct_catch_match(r) {
                fcx.builder.mark_direct_catch_match(emitted);
            }
            emitted
        }
        InstTag::Catch => super::errors::analyze_catch(sema, fcx, scope, r, span),
        InstTag::Try => super::errors::analyze_try(sema, fcx, scope, r, span),
        InstTag::FieldAccess => analyze_field_access(sema, fcx, scope, r, span),
        // UIR trusted-producer contract (see the `uir.rs` module
        // header): astgen is the only producer, so a non-expression tag
        // reaching `analyze_expr` is a compiler bug, not user input.
        other => unreachable!(
            "analyze_expr: instruction at %{} is not an expression (tag={:?})",
            r.index(),
            other
        ),
    };

    fcx.inst_map[r.index()] = Some(emitted);
    emitted
}

/// The `&` marker (M8.3): lowers to the inner value's TirRef.
/// Codegen decides pass-by-pointer from the callee's
/// `ParamMode::Inout`. (&/inout agreement + lvalue validation are
/// enforced in `check_call`, not here.)
fn analyze_borrow(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    data: InstData,
    span: Span,
) -> TirRef {
    let inner = match data {
        InstData::Borrow(inner) => inner,
        _ => unreachable!("Borrow must carry InstData::Borrow"),
    };
    if !sema.call_arg_refs[r.index()] {
        // A `&` that is not a direct call argument marks no mutation
        // at all — reject it instead of silently discarding it.
        sema.sink.emit(Diag::error(
            span,
            DiagCode::BorrowMismatch,
            "`&` is only valid as an argument to an `inout` parameter".to_string(),
        ));
    }
    analyze_expr(sema, fcx, scope, inner)
}

/// Struct literal `Name{field = value, ...}` (M9) or the anonymous
/// `{field = value, ...}` (M10). The named path validates the
/// literal against the declaration registered in `sema.struct_types`
/// (unknown / duplicated / missing / mistyped fields each get their
/// own diagnostic; analysis continues past all of them) and emits a
/// canonical-order TIR `StructLit`. Slots with no valid initializer
/// recover with an error-typed `Unreachable`. The anonymous path
/// infers the structural type from the field values — see
/// [`analyze_anon_struct_lit`].
fn analyze_struct_lit(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    let view = sema.uir.struct_lit_view(r);
    let Some(name) = view.name else {
        return analyze_anon_struct_lit(sema, fcx, scope, &view, span);
    };
    let Some(&sty) = sema.struct_types.get(&name) else {
        // Never declared — this one is on the user.
        sema.sink.emit(Diag::error(
            span,
            DiagCode::UnknownType,
            format!("unknown struct: '{}'", sema.pool.str(name)),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    };
    if sema.pool.is_error(sty) {
        // Declared but its definition failed (E0005 infinite size,
        // unknown field type — already diagnosed). Recover quietly,
        // matching field-access on a failed struct.
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    let sview = sema.pool.struct_view(sty);
    let mut by_index: Vec<Option<TirRef>> = vec![None; sview.fields.len()];
    // Once any field errored (unknown/duplicate), the literal's
    // shape is untrustworthy — a derived "missing field(s)" error on
    // top is noise. Mistyped-but-present fields do not set this: a
    // type mismatch is not a shape error.
    let mut field_errored = false;
    for (fname, value_ref) in view.fields() {
        let fspan = sema.uir.span(value_ref);
        let Some(field) = sema.pool.struct_field(sty, fname) else {
            field_errored = true;
            sema.sink.emit(Diag::error(
                fspan,
                DiagCode::UnknownField,
                format!(
                    "'{}' has no field '{}' (fields: {})",
                    sema.pool.str(sview.name),
                    sema.pool.str(fname),
                    field_list(sema.pool, &sview.fields),
                ),
            ));
            // Still analyze the initializer so diagnostics inside it
            // (unknown names, type errors) are reported.
            analyze_expr(sema, fcx, scope, value_ref);
            continue;
        };
        if by_index[field.idx as usize].is_some() {
            field_errored = true;
            sema.sink.emit(Diag::error(
                fspan,
                DiagCode::DuplicateStructField,
                format!(
                    "field '{}' is specified more than once",
                    sema.pool.str(fname)
                ),
            ));
            // Same recovery: analyze the duplicate's initializer too.
            analyze_expr(sema, fcx, scope, value_ref);
            continue;
        }
        let value = analyze_expr(sema, fcx, scope, value_ref);
        let vty = fcx.builder.ty_of(value);
        if !sema.pool.compatible(vty, field.ty) {
            sema.sink.emit(Diag::error(
                fspan,
                DiagCode::TypeMismatch,
                format!(
                    "field '{}': expected '{}', found '{}'",
                    sema.pool.str(fname),
                    sema.pool.display(field.ty),
                    sema.pool.display(vty),
                ),
            ));
        }
        by_index[field.idx as usize] = Some(value);
    }
    let missing: Vec<String> = sview
        .fields
        .iter()
        .zip(&by_index)
        .filter(|(_, slot)| slot.is_none())
        .map(|(f, _)| format!("'{}'", sema.pool.str(f.name)))
        .collect();
    if !missing.is_empty() && !field_errored {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::MissingStructFields,
            format!(
                "missing field(s) {} in '{}' construction",
                missing.join(", "),
                sema.pool.str(sview.name),
            ),
        ));
    }
    // Canonical declaration order; slots that never got a valid
    // initializer recover with an error-typed Unreachable.
    let error_ty = sema.pool.error_type();
    let fields: Vec<(u32, TirRef)> = by_index
        .into_iter()
        .enumerate()
        .map(|(i, slot)| {
            let v = slot.unwrap_or_else(|| fcx.builder.unreachable(error_ty, span));
            (i as u32, v)
        })
        .collect();
    fcx.builder.struct_lit(sty, &fields, span)
}

/// Variant construction `EnumName.Variant(args...)` (M11). The UIR
/// `EnumLit` carries the enum type, the declaration-order variant
/// index, the source form (astgen-recorded `positional` flag — without
/// it the flat pairs are indistinguishable from in-order brace
/// construction), and `(field_idx, value)` pairs — positional args
/// keyed by position, named args already canonicalized to
/// declaration-order indices by astgen (in source order; duplicates
/// included). Payload typing mirrors [`analyze_struct_lit`]: each arg
/// checks against its declared field, the emitted TIR `EnumLit` carries
/// canonical declaration-order pairs, and slots with no valid
/// initializer recover with an error-typed `Unreachable`.
///
/// Layering with astgen's frontline checks: an undeclared enum name,
/// an unknown variant name, and a typo'd *named* field are all
/// diagnosed during lowering (and the bad args dropped), so this arm
/// recovers quietly on the error type and never re-reports them. What
/// remains for sema: the Brace Law gate on named payloads (parens on a
/// named variant is rejected with the brace spelling), out-of-range
/// positional indices, duplicate fields (duplicate named args lower to
/// repeated indices with no astgen diagnostic), missing fields, and
/// per-field type mismatches.
fn analyze_variant_construct(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    let view = sema.uir.enum_lit_view(r);
    let ety = view.ty;
    if sema.pool.is_error(ety) {
        // astgen diagnosed: UnknownType for a never-declared enum, the
        // definition's own diagnostic for a failed one. Quiet recovery,
        // matching struct literals of failed structs.
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    // Decode the variant directory up front: EnumVariantView borrows the
    // pool, and the arg loop below needs `&mut Sema` for analysis and
    // diagnostics. Construction sites are cold, so the small owned copy
    // is fine.
    let (ename, variant_index, vname, kind, fields) = {
        let eview = sema.pool.enum_view(ety);
        let mut variants = eview.variants();
        let Some(variant) = variants.nth(view.variant_index as usize) else {
            // astgen only writes indices from its own directory — this
            // is producer corruption, not user input. Recover quietly.
            return fcx.builder.unreachable(sema.pool.error_type(), span);
        };
        (
            eview.name(),
            view.variant_index,
            variant.name,
            variant.kind,
            variant.fields.to_vec(),
        )
    };
    if view.positional && kind == VariantKind::Named {
        // Brace Law (D11): named payloads construct with braces only —
        // one way per shape, symmetric with structs. The pairs alone
        // can't distinguish this from in-order brace construction
        // (`Rectangle{width=1.0, height=2.0}` lowers identically), so
        // astgen records the source form on the wire. Quiet recovery:
        // the args are not type-checked, matching the other
        // construction-error paths.
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
    if kind == VariantKind::Unit && view.positional {
        // `Color.Red()` — a unit variant takes no payload; the paren
        // form is the named/tuple construct shape misapplied. Quiet
        // recovery, matching the Brace Law arm above (E0125).
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
    // Once any field errored (unknown/duplicate), the construction's
    // shape is untrustworthy — a derived "missing field(s)" error on
    // top is noise (the struct-literal precedent).
    let mut field_errored = false;
    for (fidx, value_ref) in view.fields() {
        let fspan = sema.uir.span(value_ref);
        if fidx == enum_lit_extra::ORPHANED_FIELD {
            // A braced field astgen didn't recognize (E0038 already
            // emitted). The arg is lowered only so its own
            // diagnostics surface; it binds nothing. The construct's
            // shape is untrustworthy — suppress the derived
            // missing-fields error, matching the unknown/duplicate
            // field recoveries below.
            field_errored = true;
            analyze_expr(sema, fcx, scope, value_ref);
            continue;
        }
        let Some(field) = fields.get(fidx as usize) else {
            field_errored = true;
            // Extra positional argument on a tuple variant: name the
            // position, not the synthesized "0"/"1" field index the
            // developer never wrote (E0122, positional wording).
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
            sema.sink
                .emit(Diag::error(fspan, DiagCode::UnknownVariantField, message));
            // Still analyze the arg so diagnostics inside it surface.
            analyze_expr(sema, fcx, scope, value_ref);
            continue;
        };
        if by_index[fidx as usize].is_some() {
            field_errored = true;
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
            analyze_expr(sema, fcx, scope, value_ref);
            continue;
        }
        let value = analyze_expr(sema, fcx, scope, value_ref);
        let vty = fcx.builder.ty_of(value);
        if !sema.pool.compatible(vty, field.ty) {
            // Tuple payloads carry synthesized "0"/"1" field names the
            // user never wrote — name the argument position instead
            // (1-based, matching how developers count call arguments).
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
        // Tuple payloads carry synthesized "0"/"1" field names the
        // user never wrote — word by position count instead (E0120,
        // positional wording).
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
    // Canonical declaration order; slots that never got a valid
    // initializer recover with an error-typed Unreachable.
    let error_ty = sema.pool.error_type();
    let args: Vec<(u32, TirRef)> = by_index
        .into_iter()
        .enumerate()
        .map(|(i, slot)| {
            let v = slot.unwrap_or_else(|| fcx.builder.unreachable(error_ty, span));
            (i as u32, v)
        })
        .collect();
    fcx.builder
        .enum_lit(ety, variant_index, view.positional, &args, span)
}

/// Anonymous struct literal `{field = value, ...}` (M10). There is no
/// declaration to validate against — the type is structural, inferred
/// from the initializers: each field's type is its value's type, and
/// the ordered (name, type) pairs intern via [`InternPool::anon_struct`].
/// The TIR emission then matches the named path: canonical (here:
/// written) order, duplicates diagnosed with `DuplicateStructField`,
/// and a view-typed initializer rejected with `ViewFieldType` (Rule 6
/// — views cannot live in struct fields). A field whose initializer
/// failed to type-check poisons the whole literal: an error-typed
/// field has no layout, so no anon type can be interned for it.
fn analyze_anon_struct_lit(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    view: &StructLitView<'_>,
    span: Span,
) -> TirRef {
    let error_ty = sema.pool.error_type();
    let mut fields: Vec<(StringId, TypeId, TirRef)> = Vec::with_capacity(view.fields().len());
    let mut poisoned = false;
    for (fname, value_ref) in view.fields() {
        let fspan = sema.uir.span(value_ref);
        if fields.iter().any(|(n, _, _)| *n == fname) {
            sema.sink.emit(Diag::error(
                fspan,
                DiagCode::DuplicateStructField,
                format!(
                    "field '{}' is specified more than once",
                    sema.pool.str(fname)
                ),
            ));
            // Same recovery as the named path: analyze the duplicate's
            // initializer so its own errors still surface.
            analyze_expr(sema, fcx, scope, value_ref);
            continue;
        }
        let value = analyze_expr(sema, fcx, scope, value_ref);
        let vty = fcx.builder.ty_of(value);
        if sema.pool.is_error(vty) {
            poisoned = true;
        }
        if sema.pool.is_view(vty) {
            sema.sink.emit(Diag::error(
                fspan,
                DiagCode::ViewFieldType,
                format!(
                    "struct fields must be owned values; '{}' is a projection (Rule 6)",
                    sema.pool.display(vty)
                ),
            ));
        }
        fields.push((fname, vty, value));
    }
    if poisoned {
        return fcx.builder.unreachable(error_ty, span);
    }
    let pairs: Vec<(StringId, TypeId)> = fields.iter().map(|&(n, t, _)| (n, t)).collect();
    let sty = sema.pool.anon_struct(&pairs);
    // The anon type's canonical order is the written order.
    let values: Vec<(u32, TirRef)> = fields
        .into_iter()
        .enumerate()
        .map(|(i, (_, _, v))| (i as u32, v))
        .collect();
    fcx.builder.struct_lit(sty, &values, span)
}

/// Field access `object.field` (M9; anonymous structs read fields
/// identically, M10). Resolves the field against the object's struct
/// type and emits a TIR `FieldAccess` carrying the canonical
/// field index (declaration order for named structs, written order
/// for anonymous ones) and the field type.
fn analyze_field_access(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    let (object, field) = match sema.uir.inst(r).data {
        InstData::FieldAccess { object, field } => (object, field),
        _ => unreachable!("FieldAccess must carry InstData::FieldAccess"),
    };
    // M11: `EnumName.Variant` — the parser keeps the bare spelling as a
    // FieldAccess, and sema reinterprets it when the object is an
    // unbound identifier that names an enum type (a bound variable of
    // the same name wins, matching ordinary shadowing). An identifier
    // that names a struct keeps its field-access meaning; one that
    // names nothing falls through to the usual undefined-variable path.
    if let InstTag::Var = sema.uir.inst(object).tag {
        let name = match sema.uir.inst(object).data {
            InstData::Var(name) => name,
            _ => unreachable!("Var must carry InstData::Var"),
        };
        if scope.lookup(name).is_none() {
            if let Some(&ety) = sema.enum_types.get(&name) {
                return analyze_unit_variant_access(sema, fcx, ety, field, span);
            }
            if sema.struct_types.contains_key(&name) {
                // `Point.Red` where Point is a struct: the enum-specific
                // wording beats the misleading "undefined variable".
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnknownEnum,
                    format!(
                        "unknown enum: '{}' is a struct, not an enum",
                        sema.pool.str(name),
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
        }
    }
    let obj = analyze_expr(sema, fcx, scope, object);
    let oty = fcx.builder.ty_of(obj);
    if sema.pool.is_error(oty) {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if super::errors::is_error_union(sema, oty) {
        sema.sink.emit(super::errors::direct_use_diag(
            sema,
            span,
            DiagCode::NotAStruct,
            oty,
            "access a field on",
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    let is_struct = matches!(sema.pool.kind(oty), TypeKind::Struct | TypeKind::AnonStruct);
    if !is_struct {
        // A numeric key on a non-struct is never legitimate — and it
        // is the signature of the one-element-tuple pitfall: `z.0`
        // where `z` was declared `("zero")` (a grouping, not a tuple).
        // Point back at the comma rather than leaving the reader to
        // connect it.
        let key = sema.pool.str(field);
        let is_positional_key = !key.is_empty() && key.chars().all(|c| c.is_ascii_digit());
        let diag = Diag::error(
            span,
            DiagCode::NotAStruct,
            format!("type '{}' has no fields", sema.pool.display(oty)),
        );
        let diag = if is_positional_key {
            diag.with_note(
                None,
                "if you meant a one-element tuple, the declaration needs a trailing comma: (x,)",
            )
        } else {
            diag
        };
        sema.sink.emit(diag);
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if matches!(sema.pool.kind(oty), TypeKind::Struct) && !sema.pool.is_defined_struct(oty) {
        // Declared but never defined (cycle / unknown field type) —
        // astgen already diagnosed it. Recover without touching
        // `struct_view`, which panics on undefined structs. (Anon
        // structs are interned whole; they are always defined.)
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    match sema.pool.struct_field(oty, field) {
        Some(f) => fcx.builder.field_access(obj, f.idx, f.ty, span),
        None => {
            let sview = sema.pool.struct_view(oty);
            // Name the owner with its kind: the paren/brace display is
            // concise but assumes the reader already knows the type
            // notation — "tuple" / "anonymous struct" is the search
            // term that gets them to the reference. An anon struct's
            // interned name is the "" sentinel, so its display stands
            // in for the name.
            let owner = match sema.pool.kind(oty) {
                TypeKind::AnonStruct if sema.pool.is_tuple_sugar(oty) => {
                    format!("tuple '{}'", sema.pool.display(oty))
                }
                TypeKind::AnonStruct => {
                    format!("anonymous struct '{}'", sema.pool.display(oty))
                }
                _ => format!("'{}'", sema.pool.str(sview.name)),
            };
            // With exactly one candidate there is no guessing: point
            // straight at it. A numeric key against a NAMED struct is
            // the graduation stumble — tuple users reach for
            // positional access on a type that only has names.
            let key_is_positional = {
                let key = sema.pool.str(field);
                !key.is_empty() && key.chars().all(|c| c.is_ascii_digit())
            };
            let mut diag = Diag::error(
                span,
                DiagCode::UnknownField,
                format!(
                    "{} has no field '{}' (fields: {})",
                    owner,
                    sema.pool.str(field),
                    field_list(sema.pool, &sview.fields),
                ),
            );
            if sview.fields.len() == 1 {
                diag = diag.with_note(
                    None,
                    format!(
                        "the only valid field is '{}'",
                        sema.pool.str(sview.fields[0].name),
                    ),
                );
            }
            if matches!(sema.pool.kind(oty), TypeKind::Struct) && key_is_positional {
                diag = diag.with_note(
                    None,
                    "named structs are accessed by field name — positional \
                     access is tuple sugar for anonymous shapes",
                );
            }
            sema.sink.emit(diag);
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
    }
}

/// Bare `EnumName.Variant` access (M11): the unit-variant value path.
/// The enum type is already resolved by the caller (which guarantees
/// the object ident named an enum, not a variable); a unit member
/// lowers to a zero-arg `EnumLit`. A payload-carrying variant is a
/// constructor pattern, not a value — TypeMismatch names both, with a
/// help note pointing at the construction spelling. An unknown member
/// is `UnknownVariant` (astgen's construction lowering already covers
/// the parenthesized spelling, so no double-report is possible here).
fn analyze_unit_variant_access(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    ety: TypeId,
    field: StringId,
    span: Span,
) -> TirRef {
    if sema.pool.is_error(ety) {
        // Declared but its definition failed — astgen already
        // diagnosed it. Quiet recovery, matching failed struct types.
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    let (ename, variant_index, vname, kind) = {
        let eview = sema.pool.enum_view(ety);
        let variants: Vec<(StringId, VariantKind)> =
            eview.variants().map(|v| (v.name, v.kind)).collect();
        let Some(index) = variants.iter().position(|&(name, _)| name == field) else {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UnknownVariant,
                format!(
                    "enum '{}' has no variant '{}'",
                    sema.pool.str(eview.name()),
                    sema.pool.str(field),
                ),
            ));
            return fcx.builder.unreachable(sema.pool.error_type(), span);
        };
        let (vname, kind) = variants[index];
        (eview.name(), index as u32, vname, kind)
    };
    if kind != VariantKind::Unit {
        sema.sink.emit(
            Diag::error(
                span,
                DiagCode::TypeMismatch,
                format!(
                    "variant '{}' of enum '{}' has a payload and cannot be used as a bare value",
                    sema.pool.str(vname),
                    sema.pool.str(ename),
                ),
            )
            .with_help(format!(
                "construct it: {}.{}(...)",
                sema.pool.str(ename),
                sema.pool.str(vname),
            )),
        );
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    fcx.builder.enum_lit(ety, variant_index, false, &[], span)
}

/// Comma-separated quoted field names of a struct or enum payload, for
/// the "has no field" diagnostics. Shared with the M12 match-pattern
/// payload checks (`sema/match_.rs`).
/// English ordinal for a 1-based argument position: "second",
/// "third", "fourth", then "Nth". Used when an extra positional
/// argument names its own position ("found a second positional
/// argument — 'Some' takes 1").
pub(crate) fn ordinal(n: usize) -> String {
    match n {
        2 => "second".to_string(),
        3 => "third".to_string(),
        4 => "fourth".to_string(),
        _ => format!("{n}th"),
    }
}

/// Comma-separated quoted field names of a struct or enum payload, for
/// the "has no field" diagnostics. Shared with the M12 match-pattern
/// payload checks (`sema/match_.rs`).
pub(crate) fn field_list(pool: &ryo_core::types::InternPool, fields: &[StructField]) -> String {
    fields
        .iter()
        .map(|f| format!("'{}'", pool.str(f.name)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `str`/`strview`.as_bytes(): a zero-copy projection of the string's
/// UTF-8 bytes as a `bytesview` — the mirror of `to_bytes()`, which
/// allocates and copies. Lowers to a `ToView` conversion typed
/// `bytesview` over the receiver: no runtime callee is involved, so no
/// `builtins.rs` ABI entry — codegen's view lowering re-packages the
/// receiver's `(ptr, len)` (promote-on-view for inline strings, exactly
/// like a full-range slice). The result being view-typed routes it
/// through the generic projection machinery (P2 freeze, P4 last-use
/// lifting, E1/E2 escape diagnostics) with no ownership-pass changes.
/// Wrong-family receivers keep the generalized "X has no method 'Y'"
/// diagnostic.
fn as_bytes_projection(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    args_empty: bool,
    receiver_tir: TirRef,
    receiver_ty: TypeId,
    span: Span,
) -> TirRef {
    if !args_empty {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            "as_bytes() takes no arguments".to_string(),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if !matches!(
        sema.pool.kind(receiver_ty),
        TypeKind::Str | TypeKind::View(ViewKind::Str)
    ) {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::UndefinedFunction,
            format!(
                "{} has no method 'as_bytes'",
                sema.pool.display(receiver_ty)
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    fcx.builder
        .to_view(receiver_tir, sema.pool.bytes_view(), span)
}

/// Type-check one slice bound (`start` / `end`): §3.1 requires
/// non-negative `int` indices. The bound's TIR ref is returned either
/// way so the enclosing `Slice` inst stays well-formed on the error
/// path.
pub(crate) fn check_slice_bound(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    b: InstRef,
) -> TirRef {
    let t = analyze_expr(sema, fcx, scope, b);
    let ty = fcx.builder.ty_of(t);
    if sema.pool.kind(ty) != TypeKind::Int && !sema.pool.is_error(ty) {
        sema.sink.emit(Diag::error(
            sema.uir.span(b),
            DiagCode::TypeMismatch,
            format!("slice bound must be int, got '{}'", sema.pool.display(ty)),
        ));
    }
    t
}

/// Result of compile-time evaluating a pure integer constant
/// expression: int literals, unary minus, and `+ - * / %` over
/// constants.
pub(crate) enum ConstInt {
    /// Not a constant expression, or contains an inner division /
    /// modulo by zero (that inner node reports E0037 itself — don't
    /// double-report here).
    NotConst,
    Value(i64),
    /// Evaluation overflowed `int` (i64). Spec §18 traps overflow in
    /// all build modes, so a constant expression that would trap at
    /// runtime is a compile error instead.
    Overflow,
}

/// Evaluate a UIR expression as a compile-time integer constant.
/// Purely diagnostic: the TIR is left unfolded — Cranelift already
/// constant-folds at `opt_level = "speed"` for codegen, so sema
/// evaluates only to reject constant-zero divisors (E0037) and
/// overflowing constant arithmetic (E0200) early.
pub(crate) fn const_eval_int(uir: &Uir, r: InstRef) -> ConstInt {
    let inst = uir.inst(r);
    match inst.data {
        InstData::Int(v) => ConstInt::Value(v),
        InstData::UnOp(operand) if inst.tag == InstTag::Neg => match const_eval_int(uir, operand) {
            ConstInt::Value(v) => v.checked_neg().map_or(ConstInt::Overflow, ConstInt::Value),
            other => other,
        },
        InstData::BinOp { lhs, rhs }
            if matches!(
                inst.tag,
                InstTag::Add | InstTag::Sub | InstTag::Mul | InstTag::Div | InstTag::Mod
            ) =>
        {
            let l = const_eval_int(uir, lhs);
            let rv = const_eval_int(uir, rhs);
            // Overflow propagates past non-constant sub-expressions;
            // anything else non-constant poisons the whole tree.
            if matches!(l, ConstInt::Overflow) || matches!(rv, ConstInt::Overflow) {
                return ConstInt::Overflow;
            }
            let (ConstInt::Value(l), ConstInt::Value(rv)) = (l, rv) else {
                return ConstInt::NotConst;
            };
            let result = match inst.tag {
                InstTag::Add => l.checked_add(rv),
                InstTag::Sub => l.checked_sub(rv),
                InstTag::Mul => l.checked_mul(rv),
                // A constant zero divisor gets E0037 from the inner
                // division's own analysis — treat as non-constant here.
                InstTag::Div if rv != 0 => l.checked_div(rv),
                InstTag::Mod if rv != 0 => l.checked_rem(rv),
                InstTag::Div | InstTag::Mod => return ConstInt::NotConst,
                _ => unreachable!("tag set fixed by the match guard"),
            };
            result.map_or(ConstInt::Overflow, ConstInt::Value)
        }
        _ => ConstInt::NotConst,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn check_binary_op(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    tag: InstTag,
    lhs_ty: TypeId,
    rhs_ty: TypeId,
    lhs: TirRef,
    rhs: TirRef,
    span: Span,
) -> TirRef {
    let union_ty = if super::errors::is_error_union(sema, lhs_ty) {
        Some(lhs_ty)
    } else if super::errors::is_error_union(sema, rhs_ty) {
        Some(rhs_ty)
    } else {
        None
    };
    if let Some(ty) = union_ty {
        sema.sink.emit(super::errors::direct_use_diag(
            sema,
            span,
            DiagCode::UnsupportedOperator,
            ty,
            "use an operator on",
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    // M8.4 §3.3/§3.4, generalized M8.4.2: mixed owner/view equality —
    // wrap the owned side in an explicit `ToView` conversion so the
    // comparison runs view-vs-view. This must happen before the
    // generic `compatible` check below, which rightly rejects owner ≠
    // view for every other operator. Driven by the pool's `owner_view`
    // table (`str`/`strview`, `bytes`/`bytesview`).
    let (lhs, rhs, lhs_ty, rhs_ty) = if matches!(tag, InstTag::Eq | InstTag::NotEq) {
        if sema.pool.owner_view(lhs_ty) == Some(rhs_ty) {
            let v = fcx.builder.to_view(lhs, rhs_ty, span);
            (v, rhs, rhs_ty, rhs_ty)
        } else if sema.pool.owner_view(rhs_ty) == Some(lhs_ty) {
            let v = fcx.builder.to_view(rhs, lhs_ty, span);
            (lhs, v, lhs_ty, lhs_ty)
        } else {
            (lhs, rhs, lhs_ty, rhs_ty)
        }
    } else {
        (lhs, rhs, lhs_ty, rhs_ty)
    };
    if !sema.pool.compatible(lhs_ty, rhs_ty) {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            format!(
                "type mismatch in '{}': left is '{}', right is '{}'",
                operators::bin_op_symbol(tag),
                sema.pool.display(lhs_ty),
                sema.pool.display(rhs_ty),
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    let kind_ty = if sema.pool.is_error(lhs_ty) {
        rhs_ty
    } else {
        lhs_ty
    };
    let is_equality = matches!(tag, InstTag::Eq | InstTag::NotEq);
    let is_ordering = matches!(
        tag,
        InstTag::Lt | InstTag::Gt | InstTag::LtEq | InstTag::GtEq
    );
    if matches!(tag, InstTag::And | InstTag::Or) {
        operators::emit_logical_operator(sema, fcx, tag, kind_ty, lhs, rhs, span)
    } else if is_equality {
        operators::emit_equality_operator(sema, fcx, tag, kind_ty, lhs, rhs, span)
    } else if is_ordering {
        operators::emit_ordering_operator(sema, fcx, tag, kind_ty, lhs, rhs, span)
    } else if tag == InstTag::Mod {
        operators::emit_modulo_operator(sema, fcx, tag, kind_ty, lhs, rhs, span)
    } else {
        operators::emit_arithmetic_operator(sema, fcx, tag, kind_ty, lhs, rhs, span)
    }
}
