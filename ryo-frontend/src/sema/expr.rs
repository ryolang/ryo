//! Expression analysis and const-int folding — split from `mod.rs`.

use super::{FuncCtx, Scope, Sema, check_call};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{TirData, TirRef, TirTag};
use ryo_core::types::{TypeId, TypeKind, ViewKind};
use ryo_core::uir::{InstData, InstRef, InstTag, Span, Uir};

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
            match scope.lookup(name) {
                Some(t) => fcx.builder.var(name, t, span),
                None => {
                    // block-scoped name resolution; unknown names are a
                    // compile error (spec §3, Variables)
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UndefinedVariable,
                        format!("undefined variable: '{}'", sema.pool.str(name)),
                    ));
                    fcx.builder.unreachable(sema.pool.error_type(), span)
                }
            }
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
        InstTag::Neg => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Neg must carry InstData::UnOp"),
            };
            let sub = analyze_expr(sema, fcx, scope, operand);
            let sub_ty = fcx.builder.ty_of(sub);
            match sema.pool.kind(sub_ty) {
                TypeKind::Int => {
                    // `-(i64::MIN)` is the only Neg that can overflow,
                    // reachable through constant sub-expressions.
                    if matches!(const_eval_int(sema.uir, r), ConstInt::Overflow) {
                        sema.sink.emit(Diag::error(
                            span,
                            DiagCode::ConstEvalFailure,
                            "integer overflow in constant expression".to_string(),
                        ));
                        fcx.builder.unreachable(sema.pool.error_type(), span)
                    } else {
                        fcx.builder.unary(TirTag::INeg, sema.pool.int(), sub, span)
                    }
                }
                TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
                _ => {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UnsupportedOperator,
                        format!(
                            "unary operator '-' not supported for type '{}'",
                            sema.pool.display(sub_ty),
                        ),
                    ));
                    fcx.builder.unreachable(sema.pool.error_type(), span)
                }
            }
        }
        InstTag::Not => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Not must carry InstData::UnOp"),
            };
            let sub = analyze_expr(sema, fcx, scope, operand);
            let sub_ty = fcx.builder.ty_of(sub);
            match sema.pool.kind(sub_ty) {
                TypeKind::Bool => fcx
                    .builder
                    .unary(TirTag::BoolNot, sema.pool.bool_(), sub, span),
                TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
                _ => {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UnsupportedOperator,
                        format!(
                            "logical operator 'not' requires 'bool' operand, got '{}'",
                            sema.pool.display(sub_ty),
                        ),
                    ));
                    fcx.builder.unreachable(sema.pool.error_type(), span)
                }
            }
        }
        InstTag::Call => {
            let view = sema.uir.call_view(r);
            // Translate args first (in source order) to fix their
            // TIR refs and types, *then* validate against the
            // signature so per-argument diagnostics carry the right
            // span and the call still emits a well-formed TIR Call.
            let mut arg_tirs = Vec::with_capacity(view.args.len());
            for a in &view.args {
                arg_tirs.push(analyze_expr(sema, fcx, scope, *a));
            }
            check_call(sema, fcx, scope, &view, &arg_tirs, span)
        }
        InstTag::MethodCall => {
            let view = sema.uir.method_call_view(r);
            let receiver_tir = analyze_expr(sema, fcx, scope, view.receiver);
            let receiver_ty = fcx.builder.ty_of(receiver_tir);
            let method_name = sema.pool.str(view.name).to_string();

            for &arg in &view.args {
                analyze_expr(sema, fcx, scope, arg);
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

            match method_name.as_str() {
                "len" => {
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
                "is_empty" => {
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
                _ => {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UndefinedFunction,
                        format!(
                            "{} has no method '{}'",
                            sema.pool.display(receiver_ty),
                            method_name
                        ),
                    ));
                    fcx.builder.unreachable(sema.pool.error_type(), span)
                }
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
        InstTag::Borrow => {
            let inner = match inst.data {
                InstData::Borrow(inner) => inner,
                _ => unreachable!("Borrow must carry InstData::Borrow"),
            };
            // The `&` is a marker, not an op: lower to the inner value's
            // TirRef. Codegen decides pass-by-pointer from the callee's
            // `ParamMode::Inout`. (&/inout agreement + lvalue validation
            // are enforced in `check_call`, not here.)
            if !sema.call_arg_refs[r.index()] {
                // A `&` that is not a direct call argument marks
                // no mutation at all — reject it instead of silently
                // discarding it.
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::BorrowMismatch,
                    "`&` is only valid as an argument to an `inout` parameter".to_string(),
                ));
            }
            analyze_expr(sema, fcx, scope, inner)
        }
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
                bin_op_symbol(tag),
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
    let is_modulo = matches!(tag, InstTag::Mod);
    let is_logical = matches!(tag, InstTag::And | InstTag::Or);
    let kind = sema.pool.kind(kind_ty);

    if is_logical {
        match kind {
            TypeKind::Bool => {
                let tir_tag = match tag {
                    InstTag::And => TirTag::BoolAnd,
                    InstTag::Or => TirTag::BoolOr,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            _ => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "logical operator '{}' requires 'bool' operands, got '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
    } else if is_equality {
        match kind {
            TypeKind::Int | TypeKind::Bool => {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::ICmpEq,
                    InstTag::NotEq => TirTag::ICmpNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Float => {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::FCmpEq,
                    InstTag::NotEq => TirTag::FCmpNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            TypeKind::Str => {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::StrCmpEq,
                    InstTag::NotEq => TirTag::StrCmpNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Bytes => {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::BytesCmpEq,
                    InstTag::NotEq => TirTag::BytesCmpNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            // M8.4 §3.3: view equality compares viewed contents. Same
            // `StrCmpEq`/`StrCmpNe` (or M8.4.2 `BytesCmpEq`/`BytesCmpNe`)
            // tags — operands are `{ptr, len}` pairs instead of full fat
            // pointers (codegen Task 13).
            TypeKind::View(ViewKind::Str) => {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::StrCmpEq,
                    InstTag::NotEq => TirTag::StrCmpNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::View(ViewKind::Bytes) => {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::BytesCmpEq,
                    InstTag::NotEq => TirTag::BytesCmpNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Void | TypeKind::Never | TypeKind::Tuple | TypeKind::View(_) => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "equality operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
    } else if is_ordering {
        match kind {
            TypeKind::Int => {
                let tir_tag = match tag {
                    InstTag::Lt => TirTag::ICmpLt,
                    InstTag::LtEq => TirTag::ICmpLe,
                    InstTag::Gt => TirTag::ICmpGt,
                    InstTag::GtEq => TirTag::ICmpGe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Float => {
                let tir_tag = match tag {
                    InstTag::Lt => TirTag::FCmpLt,
                    InstTag::LtEq => TirTag::FCmpLe,
                    InstTag::Gt => TirTag::FCmpGt,
                    InstTag::GtEq => TirTag::FCmpGe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Str => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "ordering operator '{}' not supported for type 'str' (yet)",
                        bin_op_symbol(tag),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
            TypeKind::Bool
            | TypeKind::Void
            | TypeKind::Never
            | TypeKind::Tuple
            | TypeKind::Bytes
            | TypeKind::View(_) => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "ordering operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
        }
    } else if is_modulo {
        match kind {
            TypeKind::Int => fcx
                .builder
                .binary(TirTag::IMod, sema.pool.int(), lhs, rhs, span),
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            _ => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "modulo operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
    } else {
        // Arithmetic: +, -, *, /
        match kind {
            TypeKind::Int => {
                let tir_tag = match tag {
                    InstTag::Add => TirTag::IAdd,
                    InstTag::Sub => TirTag::ISub,
                    InstTag::Mul => TirTag::IMul,
                    InstTag::Div => TirTag::ISDiv,
                    _ => unreachable!(),
                };
                fcx.builder.binary(tir_tag, sema.pool.int(), lhs, rhs, span)
            }
            TypeKind::Float => {
                let tir_tag = match tag {
                    InstTag::Add => TirTag::FAdd,
                    InstTag::Sub => TirTag::FSub,
                    InstTag::Mul => TirTag::FMul,
                    InstTag::Div => TirTag::FDiv,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.float(), lhs, rhs, span)
            }
            TypeKind::Str => {
                if tag != InstTag::Add {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UnsupportedOperator,
                        format!(
                            "arithmetic operator '{}' not supported for type 'str'",
                            bin_op_symbol(tag),
                        ),
                    ));
                    return fcx.builder.unreachable(sema.pool.error_type(), span);
                }
                fcx.builder
                    .binary(TirTag::StrConcat, sema.pool.str_(), lhs, rhs, span)
            }
            TypeKind::Bytes => {
                if tag != InstTag::Add {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UnsupportedOperator,
                        format!(
                            "arithmetic operator '{}' not supported for type 'bytes'",
                            bin_op_symbol(tag),
                        ),
                    ));
                    return fcx.builder.unreachable(sema.pool.error_type(), span);
                }
                fcx.builder
                    .binary(TirTag::BytesConcat, sema.pool.bytes(), lhs, rhs, span)
            }
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            _ => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "arithmetic operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
    }
}

pub(crate) fn bin_op_symbol(tag: InstTag) -> &'static str {
    match tag {
        InstTag::Add => "+",
        InstTag::Sub => "-",
        InstTag::Mul => "*",
        InstTag::Div => "/",
        InstTag::Mod => "%",
        InstTag::Eq => "==",
        InstTag::NotEq => "!=",
        InstTag::Lt => "<",
        InstTag::Gt => ">",
        InstTag::LtEq => "<=",
        InstTag::GtEq => ">=",
        InstTag::And => "and",
        InstTag::Or => "or",
        _ => "?",
    }
}
