//! M9 struct ownership helpers — split from `walk.rs`.
//!
//! A struct value is ONE `Owner`: a whole-struct move moves every
//! field at once, so the lattice shape is unchanged from `str`/`bytes`.
//! What structs add is field-level rules:
//!
//! * a `StructLit` consumes each needs-drop field value under the
//!   normal rules (`Person{name=s}` MOVES `s`);
//! * a needs-drop field read in a borrow position (a borrow-mode call
//!   argument) borrows the ROOT owner for the call's duration, keyed
//!   on the root so a same-call move of the struct is E0031;
//! * a needs-drop field read in a consuming position (var-decl init,
//!   assign RHS, return, move-mode argument) is `MoveOutOfField` —
//!   fields move only together with the whole struct;
//! * a `FieldAssign` reads its root (liveness) and, when the field
//!   type needs-drop, schedules the old field value's free via
//!   `field_free_on_reassign`.

use super::{
    Owner, Ownership, check_source_projected, consume_for_assignment, consumed_binding_name,
    needs_tracking, underlying_owner,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::tir::{Span, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeKind};

/// The root owner a `FieldAccess` chain reads from: walk nested
/// `FieldAccess` objects down to the base `Var` read and resolve the
/// binding's current owner (mirrors `projection_root` in views.rs for
/// slice chains). The unwrap-or-`Param` fallback mirrors
/// `inout_owner`: an unregistered name is a Copy-typed parameter,
/// whose per-binding key is `Owner::Param(name)`. A chain rooted in
/// anything else (a call result, a literal) resolves through the
/// usual origin walk.
pub(crate) fn struct_root(own: &Ownership, tir: &Tir, r: TirRef) -> Option<Owner> {
    match tir.inst(r).data {
        TirData::FieldAccess { object, .. } => struct_root(own, tir, object),
        TirData::Var(name) => Some(
            own.current_owner
                .get(&name)
                .copied()
                .unwrap_or(Owner::Param(name)),
        ),
        _ => Some(underlying_owner(own, r)),
    }
}

/// The binding name at the base of a `FieldAccess` chain, for
/// diagnostics (`p` for `p.name.first`). `None` when the chain is
/// rooted in an anonymous value.
pub(crate) fn struct_base_name(tir: &Tir, mut r: TirRef) -> Option<StringId> {
    loop {
        match tir.inst(r).data {
            TirData::FieldAccess { object, .. } => r = object,
            TirData::Var(name) => return Some(name),
            _ => return None,
        }
    }
}

/// Consume each needs-drop field value of a `StructLit` under the
/// normal rules: a bound source moves (`Person{name=s}` invalidates
/// `s`), a fresh temp is stamped `Moved` so the anon-temp free pass
/// leaves it to the whole-struct free. A field value that is itself a
/// needs-drop `FieldAccess` trips the `MoveOutOfField` guard inside
/// `consume_underlying`. Copy-typed field values just evaluate.
/// `lit` is the `StructLit` instruction, recorded as the consume site.
pub(crate) fn consume_struct_lit_fields(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    lit: TirRef,
) {
    let view = tir.struct_lit_view(lit);
    for &(_, value) in &view.fields {
        if !needs_tracking(tir.inst(value).ty, pool) {
            continue;
        }
        let span = tir.span(value);
        let consumed_name = consumed_binding_name(tir, value);
        // P2 freeze (final spec §3.2): the consume moves the owner.
        check_source_projected(
            tir,
            pool,
            own,
            sink,
            underlying_owner(own, value),
            span,
            "move",
            consumed_name,
        );
        consume_for_assignment(tir, pool, own, sink, value, span, consumed_name, lit);
    }
}

/// Consuming-context field read guard (M9): a needs-drop field read in
/// a consuming position (var-decl init, assign RHS, return, move-mode
/// call argument) cannot move — the field's value leaves through the
/// whole struct only. Emits `MoveOutOfField` and returns `true` when
/// `access` is such a read; `false` for Copy-typed fields (no
/// ownership effect) and non-field operands. Copy-typed field reads
/// return `false` so the caller's normal consume path no-ops on them.
pub(crate) fn check_field_move_out(
    tir: &Tir,
    pool: &InternPool,
    sink: &mut DiagSink,
    access: TirRef,
    span: Span,
) -> bool {
    if tir.inst(access).tag != TirTag::FieldAccess || !needs_tracking(tir.inst(access).ty, pool) {
        return false;
    }
    let TirData::FieldAccess {
        object,
        field_index,
    } = tir.inst(access).data
    else {
        unreachable!("FieldAccess must carry TirData::FieldAccess");
    };
    let obj_ty = tir.inst(object).ty;
    // Sema guarantees the object is a struct; a poisoned (error-typed)
    // chain already has a sema diagnostic, so don't add noise — the
    // normal consume path no-ops on it.
    if !matches!(pool.kind(obj_ty), TypeKind::Struct) {
        return false;
    }
    let sview = pool.struct_view(obj_ty);
    let field = sview.fields[field_index as usize];
    let field_name = pool.str(field.name);
    let struct_name = pool.str(sview.name);
    let base = struct_base_name(tir, object);
    let borrow_form = match base {
        Some(name) => format!("f({}.{field_name})", pool.str(name)),
        None => format!("f(x.{field_name})"),
    };
    sink.emit(Diag::error(
        span,
        DiagCode::MoveOutOfField,
        format!(
            "cannot move field `{field_name}` out of `{struct_name}`; \
             borrow it (`{borrow_form}`) or move the whole struct"
        ),
    ));
    true
}
