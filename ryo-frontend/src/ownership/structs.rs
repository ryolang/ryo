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
    Owner, OwnerState, Ownership, check_source_projected, consume_for_assignment,
    consumed_binding_name, needs_tracking, register_pending_dead_store, underlying_owner,
    visit_expr,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::ownership::FunctionSidecar;
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

/// Field-index path from the struct root down to the field a
/// `FieldAccess` chain targets: `p.a.b` → `[a, b]`. `None` when `r`
/// is not a `FieldAccess` chain. The path identifies one field's
/// storage within the root, so a freeze check can tell `p.a = x`
/// (threatens slices of `p.a` only) apart from a sibling-field
/// reassign.
pub(crate) fn field_path_of(tir: &Tir, mut r: TirRef) -> Option<Vec<u32>> {
    let mut path = Vec::new();
    loop {
        match tir.inst(r).data {
            TirData::FieldAccess {
                object,
                field_index,
            } => {
                path.push(field_index);
                r = object;
            }
            TirData::Var(_) => {
                path.reverse();
                return Some(path);
            }
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
    // Sema guarantees the object is a struct (named or anonymous); a
    // poisoned (error-typed) chain already has a sema diagnostic, so
    // don't add noise — the normal consume path no-ops on it.
    if !matches!(pool.kind(obj_ty), TypeKind::Struct | TypeKind::AnonStruct) {
        return false;
    }
    let sview = pool.struct_view(obj_ty);
    let field = sview.fields[field_index as usize];
    let field_name = pool.str(field.name);
    // `display` renders the bare name for named structs and the full
    // shape (`(int, str)`, `{q: int, r: int}`) for anonymous ones —
    // `sview.name` is the empty sentinel for anon shapes.
    let struct_display = pool.display(obj_ty);
    let base = struct_base_name(tir, object);
    let borrow_form = match base {
        Some(name) => format!("f({}.{field_name})", pool.str(name)),
        None => format!("f(x.{field_name})"),
    };
    sink.emit(Diag::error(
        span,
        DiagCode::MoveOutOfField,
        format!(
            "cannot move field `{field_name}` out of `{struct_display}`; \
             borrow it (`{borrow_form}`) or move the whole struct"
        ),
    ));
    true
}

/// Destructuring assignment (M10): `(a, _) = rhs` / `{q, r} = rhs` —
/// the one place fields move out of a struct. The rhs struct owner is
/// consumed at the statement (a whole-struct move: no partial-move
/// state survives it), and each bound field registers as a FRESH owner
/// keyed on the `FieldAccess` token sema emitted immediately before
/// the `Destructure` instruction (`Tir::destructure_bound_owner_refs`)
/// — those refs are never evaluated by codegen; they exist purely so
/// the moved-out fields have independent owner identities for the
/// last-use / dead-store / branch-divergence free passes, which lower
/// through the binding's slot at codegen (the binding-path redirect).
///
/// Wildcard fields are CODEGEN's jurisdiction — destroyed inline at
/// the copy-out, never tracked here — and no whole-struct Free is
/// scheduled for the shell: consuming the rhs owner stamps it `Moved`,
/// which every free pass skips. Copy-typed bindings need no owner at
/// all.
pub(crate) fn analyze_destructure(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    stmt: TirRef,
) {
    let view = tir.destructure_view(stmt);
    let rhs = view.rhs;
    let rhs_ty = tir.inst(rhs).ty;
    visit_expr(tir, pool, own, sink, sidecar, rhs);
    if needs_tracking(rhs_ty, pool) {
        let span = tir.span(stmt);
        let consumed_name = consumed_binding_name(tir, rhs);
        // P2 freeze (final spec §3.2): the consume moves the owner.
        check_source_projected(
            tir,
            pool,
            own,
            sink,
            underlying_owner(own, rhs),
            span,
            "move",
            consumed_name,
        );
        consume_for_assignment(tir, pool, own, sink, rhs, span, consumed_name, stmt);
    }

    let owners = tir.destructure_bound_owner_refs(stmt);
    debug_assert_eq!(
        owners.len(),
        view.fields.iter().filter(|f| f.bind.is_some()).count(),
        "one owner token per bound field"
    );
    let mut owner_idx = 0;
    for field in &view.fields {
        let Some(bind) = field.bind else {
            continue;
        };
        let owner_ref = owners[owner_idx];
        owner_idx += 1;
        own.binding_of_name.insert(bind, stmt);
        if !needs_tracking(field.ty, pool) {
            continue;
        }
        let span = tir.span(stmt);
        own.states.insert(Owner::Inst(owner_ref), OwnerState::Valid);
        Ownership::dense_set(&mut own.origin, owner_ref, None);
        own.current_owner.insert(bind, Owner::Inst(owner_ref));
        // A never-read bound field is a dead store (W0001) and its
        // allocation dies with the statement — the drain anchors the
        // Free right after it, where codegen has just populated the
        // binding's slot.
        register_pending_dead_store(own, owner_ref, bind, span, stmt);
    }
}
