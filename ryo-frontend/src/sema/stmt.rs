//! Statement analysis — split from `mod.rs`; see module docs there.

use super::{
    ConstInt, FuncCtx, Scope, Sema, analyze_expr, analyze_expr_allow_never, check_reserved_name,
    const_eval_int, with_struct_shape_notes,
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
            } else if check_reserved_name(
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
                sema.sink.emit(with_struct_shape_notes(
                    Diag::error(
                        span,
                        DiagCode::TypeMismatch,
                        format!(
                            "return type mismatch: function expects '{}', got '{}'",
                            sema.pool.display(fcx.return_type),
                            sema.pool.display(actual),
                        ),
                    ),
                    sema.pool,
                    fcx.return_type,
                    actual,
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

                    if check_reserved_name(
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
        InstTag::CompoundAssign => analyze_compound_assign(sema, fcx, scope, r, span),
        InstTag::FieldAssign => analyze_field_assign(sema, fcx, scope, r, span),
        InstTag::CompoundFieldAssign => analyze_compound_field_assign(sema, fcx, scope, r, span),
        InstTag::Destructure => analyze_destructure(sema, fcx, scope, r, span),
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
            let is_reserved = check_reserved_name(
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
    analyze_block_seeded(sema, fcx, scope, stmts, |_| {})
}

/// The final field name of a field-assign target chain, for
/// diagnostics (M9). The parser only produces `FieldAccess`-topped
/// chains.
fn field_assign_target_name(sema: &Sema<'_>, target: InstRef) -> StringId {
    match sema.uir.inst(target).data {
        InstData::FieldAccess { field, .. } => field,
        _ => unreachable!("field-assign target must be a FieldAccess chain"),
    }
}

/// Compound assignment (`+=`, `-=`, etc.) to a mutable variable:
/// same operator-vs-type rules as the binary ops, plus the
/// constant-zero-divisor rejection shared with bare `/` and `%`.
fn analyze_compound_assign(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &mut Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
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

/// Field-path assignment `p.x = v` (M9).
fn analyze_field_assign(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &mut Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    let view = sema.uir.field_assign_view(r);
    let field_name = field_assign_target_name(sema, view.target);
    let Some((target_tir, field_ty)) = check_field_chain(sema, fcx, scope, view.target) else {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    };
    let value_tir = analyze_expr_allow_never(sema, fcx, scope, view.value);
    let value_ty = fcx.builder.ty_of(value_tir);
    if check_bindable_value(sema, field_name, value_ty, sema.uir.span(view.value)) {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if !sema.pool.is_error(value_ty)
        && !sema.pool.is_error(field_ty)
        && !sema.pool.compatible(field_ty, value_ty)
    {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.value),
            DiagCode::TypeMismatch,
            format!(
                "type mismatch: '{}' is '{}', got '{}'",
                sema.pool.str(field_name),
                sema.pool.display(field_ty),
                sema.pool.display(value_ty),
            ),
        ));
    }
    fcx.builder
        .field_assign(target_tir, value_tir, field_ty, span)
}

/// Compound field-path assignment `p.x += v` (M9). Same
/// operator-vs-type rules as bare `CompoundAssign`, with the field
/// type as the LHS.
fn analyze_compound_field_assign(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &mut Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    let view = sema.uir.compound_field_assign_view(r);
    let field_name = field_assign_target_name(sema, view.target);
    let Some((target_tir, field_ty)) = check_field_chain(sema, fcx, scope, view.target) else {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    };
    let op = view.op;
    let value_tir = analyze_expr_allow_never(sema, fcx, scope, view.value);
    let value_ty = fcx.builder.ty_of(value_tir);

    if field_ty == sema.pool.error_type() {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if check_bindable_value(sema, field_name, value_ty, sema.uir.span(view.value)) {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let is_int = field_ty == sema.pool.int();
    let is_float = field_ty == sema.pool.float();

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
                sema.pool.display(field_ty),
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    if !sema.pool.is_error(value_ty) && !sema.pool.compatible(field_ty, value_ty) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.value),
            DiagCode::TypeMismatch,
            format!(
                "type mismatch in compound assignment: '{}' is '{}', got '{}'",
                sema.pool.str(field_name),
                sema.pool.display(field_ty),
                sema.pool.display(value_ty),
            ),
        ));
    }

    // Same constant-zero-divisor rule as binary `x / 0`.
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
        .compound_field_assign(target_tir, op, field_ty, value_tir, span)
}

/// Check the target of a field-path assignment (M9): walk the
/// `FieldAccess` chain down to its root identifier, require that
/// binding to exist and be mutable, then analyze the whole chain as a
/// normal field access (which re-checks every hop against the struct
/// declarations). Returns the analyzed target and the final field
/// type, or `None` after emitting the root diagnostic.
fn check_field_chain(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    target: InstRef,
) -> Option<(TirRef, TypeId)> {
    let mut root = target;
    loop {
        match sema.uir.inst(root).data {
            InstData::FieldAccess { object, .. } => root = object,
            InstData::Var(name) => {
                let root_span = sema.uir.span(root);
                match scope.lookup_full(name) {
                    Some((_, true)) => break,
                    Some((_, false)) => {
                        sema.sink.emit(Diag::error(
                            root_span,
                            DiagCode::ImmutableAssign,
                            format!(
                                "cannot assign to field of immutable binding '{}'",
                                sema.pool.str(name)
                            ),
                        ));
                        return None;
                    }
                    None => {
                        sema.sink.emit(Diag::error(
                            root_span,
                            DiagCode::UndefinedAssignTarget,
                            format!(
                                "cannot assign to field of undeclared variable '{}'",
                                sema.pool.str(name)
                            ),
                        ));
                        return None;
                    }
                }
            }
            _ => unreachable!("field-assign target chain must be rooted at a Var"),
        }
    }
    let target_tir = analyze_expr(sema, fcx, scope, target);
    let field_ty = fcx.builder.ty_of(target_tir);
    Some((target_tir, field_ty))
}

/// Destructuring assignment `pattern = value` (M10): `(a, b) = e`,
/// `{q, r} = e`. Sema resolves the rhs type (anonymous or named
/// struct), validates the pattern against the struct shape — full
/// coverage (bind or `_` every field), no unknown or duplicate
/// fields, no binding collisions — inserts the fresh bindings, and
/// emits one `TirTag::Destructure` whose plan carries canonical field
/// indices and resolved field types. One `FieldAccess` owner token is
/// emitted immediately before it per bound field (the arena-adjacent
/// convention documented on `TirBuilder::destructure`); the ownership
/// pass adopts those as the moved-out fields' fresh owners.
///
/// On any validation error the statement recovers with
/// `TirTag::Unreachable`: no partial `Destructure` reaches codegen,
/// and (mirroring `VarDecl`'s collision path) bindings insert with the
/// error type so later uses don't cascade.
fn analyze_destructure(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &mut Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    let view = sema.uir.destructure_view(r);
    let value_tir = analyze_expr_allow_never(sema, fcx, scope, view.value);
    let value_ty = fcx.builder.ty_of(value_tir);
    let fail = |sema: &mut Sema<'_>, fcx: &mut FuncCtx| -> TirRef {
        fcx.builder.unreachable(sema.pool.error_type(), span)
    };
    // Recovery with the docstring contract below: a failed destructure
    // declares nothing, but its names must still resolve — user binds
    // with the error type so later uses point back at this statement
    // instead of cascading an 'undefined variable' per use, temps in
    // the side table so the nested sub-pattern's own statement fails
    // silently. User names insert only when absent: a failed pattern
    // never actually shadowed a same-named outer binding.
    let fail_registered = |sema: &mut Sema<'_>, fcx: &mut FuncCtx, scope: &mut Scope| -> TirRef {
        for (_, bind) in &view.plan {
            let Some(name) = bind else { continue };
            if sema.pool.str(*name) == "_" {
                continue;
            }
            if fcx_temp_name(sema, *name) {
                fcx.destructure_temps.insert(*name, sema.pool.error_type());
            } else if !scope.contains_in_current(*name) {
                scope.insert_binding(*name, sema.pool.error_type(), false);
            }
        }
        fail(sema, fcx)
    };

    if sema.pool.is_error(value_ty) {
        return fail_registered(sema, fcx, scope);
    }
    // Name the void/never diagnostic after the first USER binding —
    // leading with a compiler temp would leak `__ryo_destructure_N`
    // into a user-facing message.
    let first_bind = view
        .plan
        .iter()
        .find_map(|(_, bind)| bind.filter(|b| !fcx_temp_name(sema, *b)))
        .unwrap_or_else(|| sema.pool.intern_str("_"));
    if check_bindable_value(sema, first_bind, value_ty, sema.uir.span(view.value)) {
        return fail_registered(sema, fcx, scope);
    }
    let kind = sema.pool.kind(value_ty);
    if !matches!(
        kind,
        ryo_core::types::TypeKind::Struct | ryo_core::types::TypeKind::AnonStruct
    ) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.value),
            DiagCode::NotAStruct,
            format!(
                "cannot destructure `{}` — destructuring works on structs and tuples",
                sema.pool.display(value_ty),
            ),
        ));
        return fail_registered(sema, fcx, scope);
    }
    if matches!(kind, ryo_core::types::TypeKind::Struct) && !sema.pool.is_defined_struct(value_ty) {
        // Declared but never defined (cycle / unknown field type) —
        // astgen already diagnosed it. Recover without touching
        // `struct_view`, which panics on undefined structs. (Anon
        // structs are interned whole; they are always defined.)
        return fail_registered(sema, fcx, scope);
    }
    let shape = sema.pool.struct_view(value_ty);

    // ---- Shape validation: build the per-field plan ----
    let mut fields: Vec<(u32, Option<StringId>, TypeId)> = Vec::new();
    let mut had_error = false;
    if view.by_position {
        if matches!(kind, ryo_core::types::TypeKind::Struct) {
            // Named structs have no "0"/"1" fields; the by-name
            // spelling is the only shape.
            let names = shape
                .fields
                .iter()
                .map(|f| sema.pool.str(f.name))
                .collect::<Vec<_>>()
                .join(", ");
            sema.sink.emit(
                Diag::error(
                    span,
                    DiagCode::DestructurePositionalOnNamed,
                    format!(
                        "cannot destructure named struct `{}` positionally — \
                         named structs destructure by field name",
                        sema.pool.display(value_ty),
                    ),
                )
                .with_help(format!("write the brace pattern: `{{{names}}} = ...`")),
            );
            had_error = true;
        } else if view.plan.len() != shape.fields.len() {
            let n = shape.fields.len();
            let m = view.plan.len();
            sema.sink.emit(Diag::error(
                span,
                DiagCode::DestructureArity,
                format!(
                    "expected {n} fields, found {m} binding{}",
                    if m == 1 { "" } else { "s" },
                ),
            ));
            had_error = true;
        } else {
            for (i, (_, bind)) in view.plan.iter().enumerate() {
                fields.push((i as u32, *bind, shape.fields[i].ty));
            }
        }
    } else {
        // Brace pattern. Per-entry resolution:
        // - rename (`{x = quot}` — binding differs from the field
        //   name): field `x` must exist (else DestructureUnknownField)
        //   and binds the rename target (`_` skips the field);
        // - pun (`{q}` — binding equals the field name): field `q`
        //   when the struct has one; otherwise the NEXT UNCOVERED
        //   field in canonical order — the Python-unpack fallback that
        //   makes `{quot, _} = divmod(...)` bind `q` to `quot`;
        // - a field named `_`: the wildcard-rest form — covers every
        //   otherwise-uncovered field as an inline drop.
        let mut covered: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for (selector, bind) in &view.plan {
            let name = StringId::from_raw(*selector);
            let bind = *bind;
            if sema.pool.str(name) == "_" {
                // Explicit bindings after the rest marker hit the
                // covered-field error below.
                for f in &shape.fields {
                    if covered.insert(f.idx) {
                        fields.push((f.idx, None, f.ty));
                    }
                }
                continue;
            }
            let is_pun = bind == Some(name);
            let field = if is_pun {
                match shape.fields.iter().find(|f| f.name == name) {
                    Some(f) => Some(f),
                    // Unmatched pun: bind the next uncovered field.
                    None => shape.fields.iter().find(|f| !covered.contains(&f.idx)),
                }
            } else {
                match shape.fields.iter().find(|f| f.name == name) {
                    Some(f) => Some(f),
                    None => {
                        sema.sink.emit(Diag::error(
                            span,
                            DiagCode::DestructureUnknownField,
                            format!(
                                "struct `{}` has no field `{}`",
                                sema.pool.display(value_ty),
                                sema.pool.str(name),
                            ),
                        ));
                        had_error = true;
                        continue;
                    }
                }
            };
            let Some(field) = field else {
                // More bindings than fields — the positional reading
                // of the arity rule.
                let n = shape.fields.len();
                let m = view.plan.iter().filter(|(_, b)| b.is_some()).count();
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::DestructureArity,
                    format!(
                        "expected {n} fields, found {m} binding{}",
                        if m == 1 { "" } else { "s" },
                    ),
                ));
                had_error = true;
                continue;
            };
            if !covered.insert(field.idx) {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::DuplicateStructField,
                    format!(
                        "field '{}' is specified more than once",
                        sema.pool.str(field.name),
                    ),
                ));
                had_error = true;
                continue;
            }
            // A rename whose target is `_` skips the field without
            // binding it.
            let bind = bind.filter(|b| sema.pool.str(*b) != "_");
            fields.push((field.idx, bind, field.ty));
        }
        // Full coverage: every field is bound, wildcarded, or rest-
        // covered above — anything still uncovered is an error.
        let missing: Vec<StringId> = shape
            .fields
            .iter()
            .filter(|f| !covered.contains(&f.idx))
            .map(|f| f.name)
            .collect();
        if !missing.is_empty() {
            let names = missing
                .iter()
                .map(|n| format!("`{}`", sema.pool.str(*n)))
                .collect::<Vec<_>>()
                .join(", ");
            let (noun, pronoun) = if missing.len() == 1 {
                ("field", "it")
            } else {
                ("fields", "them")
            };
            sema.sink.emit(Diag::error(
                span,
                DiagCode::DestructureUnknownField,
                format!(
                    "destructuring `{}` does not cover {noun} {names} — \
                     bind {pronoun} or write `_`",
                    sema.pool.display(value_ty),
                ),
            ));
            had_error = true;
        }
    }

    // ---- Bindings: collision / reserved checks, then insertion ----
    // Temps (nested-pattern sub-bindings) live in the side table and
    // cannot collide; user bindings insert into the scope.
    let mut owners: Vec<(Option<StringId>, TypeId, u32)> = Vec::new();
    for &(field_index, bind, ty) in &fields {
        let Some(name) = bind else { continue };
        let is_temp = fcx_temp_name(sema, name);
        if !is_temp && scope.contains_in_current(name) {
            sema.sink.emit(
                Diag::error(
                    span,
                    DiagCode::DuplicateDeclaration,
                    format!(
                        "'{}' is already declared in this scope",
                        sema.pool.str(name),
                    ),
                )
                .with_help(format!(
                    "destructuring declares fresh bindings; to update the \
                     existing variable, assign individually instead: \
                     `{} = <value>`",
                    sema.pool.str(name),
                )),
            );
            scope.insert_binding(name, sema.pool.error_type(), false);
            had_error = true;
            continue;
        }
        if !is_temp
            && check_reserved_name(
                sema,
                name,
                span,
                "is a reserved builtin and cannot be redefined",
            )
        {
            scope.insert_binding(name, sema.pool.error_type(), false);
            had_error = true;
            continue;
        }
        if is_temp {
            fcx.destructure_temps.insert(name, ty);
        } else {
            scope.insert_binding(name, ty, false);
        }
        owners.push((bind, ty, field_index));
    }

    if had_error {
        // Shape validation failed (arity, unknown/duplicate field,
        // coverage): the loop above bound only the fields that
        // validated — register every remaining plan name so later
        // uses still resolve (already-inserted names keep their
        // error-typed or validated binding).
        return fail_registered(sema, fcx, scope);
    }

    // ---- Emit: owner tokens (bound fields, plan order) then the
    // Destructure itself ----
    for &(bind, ty, field_index) in &owners {
        debug_assert!(bind.is_some());
        let token = fcx.builder.field_access(value_tir, field_index, ty, span);
        let _ = token;
    }
    let plan: Vec<(u32, Option<StringId>, TypeId)> = fields;
    fcx.builder.destructure(value_tir, &plan, span)
}

/// True for the compiler-temp names astgen mints for nested
/// destructuring patterns (`__ryo_destructure_N`) — those bind in
/// sema's side table, never the user scope.
fn fcx_temp_name(sema: &Sema<'_>, name: StringId) -> bool {
    sema.pool.str(name).starts_with("__ryo_destructure_")
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
            sema.sink.emit(with_struct_shape_notes(
                Diag::error(
                    sema.uir.span(view.initializer),
                    DiagCode::TypeMismatch,
                    format!(
                        "type mismatch: '{}' annotated '{}', initializer is '{}'",
                        sema.pool.str(view.name),
                        sema.pool.display(annotated),
                        sema.pool.display(inferred),
                    ),
                ),
                sema.pool,
                annotated,
                inferred,
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
