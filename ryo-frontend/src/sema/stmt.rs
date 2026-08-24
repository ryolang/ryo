//! Statement analysis — split from `mod.rs`; see module docs there.

use super::{
    ConstInt, FuncCtx, Scope, Sema, analyze_expr, analyze_expr_allow_never, check_reserved_builtin,
    const_eval_int,
};
use ryo_core::ast::CompoundOp;
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{TirRef, TirTag};
use ryo_core::types::{StringId, TypeId};
use ryo_core::uir::{InstData, InstRef, InstTag, Span, VarDeclView};
use std::collections::HashMap;

pub(crate) fn analyze_stmt(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &mut Scope,
    r: InstRef,
) -> TirRef {
    let inst = sema.uir.inst(r);
    let span = sema.uir.span(r);
    match inst.tag {
        InstTag::VarDecl => {
            let view = sema.uir.var_decl_view(r);
            let init_tir = analyze_expr_allow_never(sema, fcx, scope, view.initializer);
            let inferred = fcx.builder.ty_of(init_tir);
            // Reject void/never RHS and recover with the error
            // sentinel so downstream uses don't cascade.
            let inferred =
                if check_bindable_value(sema, view.name, inferred, sema.uir.span(view.initializer))
                {
                    sema.pool.error_type()
                } else {
                    inferred
                };
            let resolved = resolve_var_decl_type(&view, inferred, sema);

            if scope.contains_in_current(view.name) {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::DuplicateDeclaration,
                    format!(
                        "'{}' is already declared in this scope",
                        sema.pool.str(view.name),
                    ),
                ));
            } else if check_reserved_builtin(
                sema,
                view.name,
                span,
                "is a reserved builtin and cannot be redefined",
            ) {
                scope.insert_binding(view.name, sema.pool.error_type(), view.mutable);
            } else {
                scope.insert_binding(view.name, resolved, view.mutable);
            }
            fcx.builder
                .var_decl(view.name, view.mutable, resolved, init_tir, span)
        }
        InstTag::Return => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Return must carry InstData::UnOp"),
            };
            let val_tir = analyze_expr(sema, fcx, scope, operand);
            let actual = fcx.builder.ty_of(val_tir);
            if fcx.return_type == sema.pool.void() {
                if !sema.pool.is_error(actual) {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::TypeMismatch,
                        format!(
                            "cannot return a value from a function with return type 'void' (got '{}')",
                            sema.pool.display(actual),
                        ),
                    ));
                }
            } else if !sema.pool.compatible(actual, fcx.return_type) {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!(
                        "return type mismatch: function expects '{}', got '{}'",
                        sema.pool.display(fcx.return_type),
                        sema.pool.display(actual),
                    ),
                ));
            }
            fcx.builder
                .unary(TirTag::Return, sema.pool.void(), val_tir, span)
        }
        InstTag::ReturnVoid => {
            if fcx.return_type != sema.pool.void() && !sema.pool.is_error(fcx.return_type) {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!(
                        "missing return value: function expects '{}'",
                        sema.pool.display(fcx.return_type),
                    ),
                ));
            }
            fcx.builder.return_void(sema.pool.void(), span)
        }
        InstTag::ExprStmt => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("ExprStmt must carry InstData::UnOp"),
            };
            // The one position where a `never` value is legal: a bare
            // `panic(...)` statement diverges by design.
            let val_tir = analyze_expr_allow_never(sema, fcx, scope, operand);
            fcx.builder
                .unary(TirTag::ExprStmt, sema.pool.void(), val_tir, span)
        }
        InstTag::IfStmt => {
            let view = sema.uir.if_stmt_view(r);

            let cond_tir = analyze_expr(sema, fcx, scope, view.cond);
            check_condition_bool(sema, fcx, cond_tir, view.cond);

            let then_tirs = analyze_block(sema, fcx, scope, &view.then_stmts);

            let mut elif_tirs = Vec::with_capacity(view.elif_branches.len());
            for elif in &view.elif_branches {
                let elif_cond_tir = analyze_expr(sema, fcx, scope, elif.cond);
                check_condition_bool(sema, fcx, elif_cond_tir, elif.cond);
                let elif_body_tirs = analyze_block(sema, fcx, scope, &elif.body);
                elif_tirs.push((elif_cond_tir, elif_body_tirs));
            }

            let else_tirs = view
                .else_stmts
                .as_ref()
                .map(|stmts| analyze_block(sema, fcx, scope, stmts));

            fcx.builder.if_stmt(
                cond_tir,
                &then_tirs,
                &elif_tirs,
                else_tirs.as_deref(),
                sema.pool.void(),
                span,
            )
        }
        InstTag::AssignOrDecl => {
            let view = sema.uir.assign_or_decl_view(r);
            let value_tir = analyze_expr_allow_never(sema, fcx, scope, view.value);
            let value_ty = fcx.builder.ty_of(value_tir);

            match scope.lookup_full(view.name) {
                Some((existing_ty, true)) => {
                    if check_bindable_value(sema, view.name, value_ty, sema.uir.span(view.value)) {
                        return fcx.builder.unreachable(sema.pool.error_type(), span);
                    }
                    if !sema.pool.is_error(value_ty)
                        && !sema.pool.is_error(existing_ty)
                        && !sema.pool.compatible(existing_ty, value_ty)
                    {
                        sema.sink.emit(Diag::error(
                            sema.uir.span(view.value),
                            DiagCode::TypeMismatch,
                            format!(
                                "type mismatch: '{}' is '{}', got '{}'",
                                sema.pool.str(view.name),
                                sema.pool.display(existing_ty),
                                sema.pool.display(value_ty),
                            ),
                        ));
                    }
                    fcx.builder.assign(view.name, existing_ty, value_tir, span)
                }
                Some((_, false)) => {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::ImmutableAssign,
                        format!(
                            "cannot assign to immutable variable '{}'",
                            sema.pool.str(view.name),
                        ),
                    ));
                    fcx.builder.unreachable(sema.pool.error_type(), span)
                }
                None => {
                    let resolved_ty = if check_bindable_value(
                        sema,
                        view.name,
                        value_ty,
                        sema.uir.span(view.value),
                    ) {
                        sema.pool.error_type()
                    } else {
                        value_ty
                    };

                    if check_reserved_builtin(
                        sema,
                        view.name,
                        span,
                        "is a reserved builtin and cannot be redefined",
                    ) {
                        scope.insert_binding(view.name, sema.pool.error_type(), false);
                    } else {
                        scope.insert_binding(view.name, resolved_ty, false);
                    }
                    fcx.builder
                        .var_decl(view.name, false, resolved_ty, value_tir, span)
                }
            }
        }
        InstTag::CompoundAssign => {
            let view = sema.uir.compound_assign_view(r);
            let value_tir = analyze_expr_allow_never(sema, fcx, scope, view.value);
            let value_ty = fcx.builder.ty_of(value_tir);

            let (existing_ty, is_mutable) = match scope.lookup_full(view.name) {
                Some(pair) => pair,
                None => {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UndefinedAssignTarget,
                        format!(
                            "cannot use compound assignment on undeclared variable '{}'",
                            sema.pool.str(view.name),
                        ),
                    ));
                    return fcx.builder.unreachable(sema.pool.error_type(), span);
                }
            };

            if !is_mutable {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ImmutableAssign,
                    format!(
                        "cannot assign to immutable variable '{}'",
                        sema.pool.str(view.name),
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }

            if existing_ty == sema.pool.error_type() {
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }

            let op = view.op;
            let is_int = existing_ty == sema.pool.int();
            let is_float = existing_ty == sema.pool.float();

            if check_bindable_value(sema, view.name, value_ty, sema.uir.span(view.value)) {
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }

            if op == CompoundOp::Mod && is_float {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::FloatModulo,
                    "operator '%=' is not defined for 'float'".to_string(),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }

            if !is_int && !is_float {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "compound assignment is not defined for '{}'",
                        sema.pool.display(existing_ty),
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }

            if !sema.pool.is_error(value_ty)
                && !sema.pool.is_error(existing_ty)
                && !sema.pool.compatible(existing_ty, value_ty)
            {
                sema.sink.emit(Diag::error(
                    sema.uir.span(view.value),
                    DiagCode::TypeMismatch,
                    format!(
                        "type mismatch in compound assignment: '{}' is '{}', got '{}'",
                        sema.pool.str(view.name),
                        sema.pool.display(existing_ty),
                        sema.pool.display(value_ty),
                    ),
                ));
            }

            // Same constant-zero-divisor rule as binary `x / 0`:
            // always panics at runtime, so reject it here.
            if matches!(op, CompoundOp::Div | CompoundOp::Mod)
                && is_int
                && matches!(const_eval_int(sema.uir, view.value), ConstInt::Value(0))
            {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::DivisionByZero,
                    if op == CompoundOp::Div {
                        "division by zero".to_string()
                    } else {
                        "modulo by zero".to_string()
                    },
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }

            fcx.builder
                .compound_assign(view.name, view.op, existing_ty, value_tir, span)
        }
        InstTag::WhileLoop => {
            let view = sema.uir.while_loop_view(r);

            let cond_tir = analyze_expr(sema, fcx, scope, view.cond);
            check_condition_bool(sema, fcx, cond_tir, view.cond);

            fcx.loop_depth += 1;
            let body_tirs = analyze_block(sema, fcx, scope, &view.body);
            fcx.loop_depth -= 1;

            fcx.builder
                .while_loop(cond_tir, &body_tirs, sema.pool.void(), span)
        }
        InstTag::ForRange => {
            let view = sema.uir.for_range_view(r);

            let start_tir = analyze_expr(sema, fcx, scope, view.start);
            let end_tir = analyze_expr(sema, fcx, scope, view.end);

            let start_ty = fcx.builder.ty_of(start_tir);
            let end_ty = fcx.builder.ty_of(end_tir);

            if !sema.pool.is_error(start_ty) && start_ty != sema.pool.int() {
                sema.sink.emit(Diag::error(
                    sema.uir.span(view.start),
                    DiagCode::RangeArgType,
                    format!(
                        "range() start must be 'int', got '{}'",
                        sema.pool.display(start_ty),
                    ),
                ));
            }
            if !sema.pool.is_error(end_ty) && end_ty != sema.pool.int() {
                sema.sink.emit(Diag::error(
                    sema.uir.span(view.end),
                    DiagCode::RangeArgType,
                    format!(
                        "range() end must be 'int', got '{}'",
                        sema.pool.display(end_ty),
                    ),
                ));
            }

            let int_ty = sema.pool.int();
            let void_ty = sema.pool.void();
            let var_name = view.var_name;

            fcx.loop_depth += 1;
            let is_reserved = check_reserved_builtin(
                sema,
                var_name,
                span,
                "is a reserved builtin and cannot be redefined",
            );
            let error_ty = sema.pool.error_type();
            let body_tirs = analyze_block_seeded(sema, fcx, scope, &view.body, |child_scope| {
                if is_reserved {
                    child_scope.insert_binding(var_name, error_ty, false);
                } else {
                    child_scope.insert_binding(var_name, int_ty, false);
                }
            });
            fcx.loop_depth -= 1;

            fcx.builder
                .for_range(var_name, start_tir, end_tir, &body_tirs, void_ty, span)
        }
        InstTag::Break => {
            // break outside a loop is a compile error (spec §3, Control Flow)
            if fcx.loop_depth == 0 {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::BreakOutsideLoop,
                    "'break' can only be used inside a loop".to_string(),
                ));
            }
            fcx.builder.break_stmt(sema.pool.void(), span)
        }
        InstTag::Continue => {
            // continue outside a loop is a compile error (spec §3, Control Flow)
            if fcx.loop_depth == 0 {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ContinueOutsideLoop,
                    "'continue' can only be used inside a loop".to_string(),
                ));
            }
            fcx.builder.continue_stmt(sema.pool.void(), span)
        }
        // UIR trusted-producer contract (see the `uir.rs` module
        // header): astgen is the only producer, so a non-statement tag
        // reaching `analyze_stmt` is a compiler bug, not user input.
        other => unreachable!(
            "analyze_stmt: instruction at %{} is not a statement (tag={:?})",
            r.index(),
            other
        ),
    }
}

pub(crate) fn analyze_block(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    stmts: &[InstRef],
) -> Vec<TirRef> {
    let mut child_scope = Scope {
        parent: Some(scope),
        bindings: HashMap::new(),
    };
    let mut tirs = Vec::with_capacity(stmts.len());
    for stmt_ref in stmts {
        tirs.push(analyze_stmt(sema, fcx, &mut child_scope, *stmt_ref));
    }
    tirs
}

/// Variant of [`analyze_block`] that accepts a closure to seed the
/// child scope before analyzing body statements. Used by for-range
/// to inject the loop variable into scope.
pub(crate) fn analyze_block_seeded(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    stmts: &[InstRef],
    seed: impl FnOnce(&mut Scope),
) -> Vec<TirRef> {
    let mut child_scope = Scope {
        parent: Some(scope),
        bindings: HashMap::new(),
    };
    seed(&mut child_scope);
    let mut tirs = Vec::with_capacity(stmts.len());
    for stmt_ref in stmts {
        tirs.push(analyze_stmt(sema, fcx, &mut child_scope, *stmt_ref));
    }
    tirs
}

pub(crate) fn check_condition_bool(
    sema: &mut Sema<'_>,
    fcx: &FuncCtx,
    cond_tir: TirRef,
    cond_uir: InstRef,
) {
    // conditions must be bool (spec §3, Control Flow)
    let cond_ty = fcx.builder.ty_of(cond_tir);
    if !sema.pool.is_error(cond_ty) && cond_ty != sema.pool.bool_() {
        sema.sink.emit(Diag::error(
            sema.uir.span(cond_uir),
            DiagCode::ConditionNotBool,
            format!(
                "condition must be 'bool', got '{}'",
                sema.pool.display(cond_ty),
            ),
        ));
    }
}

pub(crate) fn resolve_var_decl_type(
    view: &VarDeclView,
    inferred: TypeId,
    sema: &mut Sema<'_>,
) -> TypeId {
    match view.ty {
        Some(annotated) if !sema.pool.compatible(annotated, inferred) => {
            // Anchor the squiggle on the offending value (the
            // initializer) rather than on the whole `[mut] name [:
            // type] = expr` decl span — the type came from the
            // annotation but the *mismatch* is the initializer's
            // fault.
            sema.sink.emit(Diag::error(
                sema.uir.span(view.initializer),
                DiagCode::TypeMismatch,
                format!(
                    "type mismatch: '{}' annotated '{}', initializer is '{}'",
                    sema.pool.str(view.name),
                    sema.pool.display(annotated),
                    sema.pool.display(inferred),
                ),
            ));
            annotated
        }
        Some(annotated) => annotated,
        None => inferred,
    }
}

/// Reject a valueless (`void` or `never`) right-hand side in a
/// binding position: a `void` call produced no value; a `never`
/// expression (e.g. `panic`) diverges before producing one — neither
/// can be bound or assigned. Emits the diagnostic and returns true so
/// the caller can recover; false when `ty` is bindable.
pub(crate) fn check_bindable_value(
    sema: &mut Sema<'_>,
    name: StringId,
    ty: TypeId,
    span: Span,
) -> bool {
    let kind = if ty == sema.pool.void() {
        "void"
    } else if sema.pool.is_never(ty) {
        "never"
    } else {
        return false;
    };
    sema.sink.emit(Diag::error(
        span,
        DiagCode::VoidValueInExpression,
        format!(
            "cannot bind '{}' to a '{}' value: the right-hand side has no value",
            sema.pool.str(name),
            kind,
        ),
    ));
    true
}
