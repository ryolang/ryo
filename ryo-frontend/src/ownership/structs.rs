//! M9 struct ownership helpers — split from `walk.rs`; M11 adds
//! enum variant construction, whose payload consumption mirrors the
//! `StructLit` rules below.
//!
//! A struct value is ONE `Owner`: a whole-struct move moves every
//! field at once, so the lattice shape is unchanged from `str`/`bytes`.
//! M11 extends the same whole-value model to enums: an enum value is
//! ONE `Owner`, and an `EnumLit` consumes each needs-drop payload arg
//! like a `StructLit` consumes its fields — no new sidecar kinds. What
//! structs add is field-level rules:
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
    HazardKind, Owner, OwnerState, Ownership, Walk, check_source_projected, consumed_binding_name,
    format_binding, inout_escape_owner, needs_tracking, register_pending_dead_store,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{Span, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeKind};

/// The root owner a `FieldAccess` chain reads from (G2): the chain's
/// step's `read` fact names the ROOT place — Flow resolved the chain
/// at build — so the root owner is `underlying_owner` of the chain
/// inst itself. Kept as a named helper so call sites read correctly;
/// `tir` is unused (payload-free resolution).
pub(crate) fn struct_root(own: &Ownership, _tir: &Tir, r: TirRef) -> Option<Owner> {
    Some(own.underlying_owner(r))
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
    flow: &ryo_core::flow::Flow,
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
            flow,
            pool,
            own,
            sink,
            own.underlying_owner(value),
            span,
            "move",
            consumed_name,
        );
        consume_for_assignment(tir, pool, own, sink, value, span, consumed_name, lit);
    }
}

/// `process_step`'s `EnumLit` arm (M11). Whole-value model: an enum
/// value is ONE `Owner`, exactly like a struct — this mirrors the
/// `StructLit` step. The payload arg steps already ran (block iteration
/// order); only the fresh-owner seeding and the per-payload consume
/// remain. A Copy enum skips the lattice entirely.
pub(crate) fn analyze_enum_lit(
    w: &mut Walk<'_>,
    own: &mut Ownership,
    sink: &mut DiagSink,
    lit: TirRef,
) {
    if needs_tracking(w.tir.inst(lit).ty, w.pool) {
        let owner = own.owner_of_inst(lit);
        own.states.insert(owner, OwnerState::Valid);
        own.temp_owners.insert(owner);
    }
    consume_enum_lit_fields(w.tir, w.flow, w.pool, own, sink, lit);
}

/// Consume each needs-drop payload arg of an `EnumLit` (M11) under the
/// normal rules, mirroring [`consume_struct_lit_fields`]: a bound
/// source moves (`Opt.Some(s)` invalidates `s`), a fresh temp is
/// stamped `Moved` so the anon-temp free pass leaves it to the
/// whole-enum free. A needs-drop payload arg that is itself a
/// needs-drop `FieldAccess` trips the `MoveOutOfField` guard inside
/// `consume_underlying`, exactly like a struct field value. Copy-typed
/// payload args just evaluate. `lit` is the `EnumLit` instruction,
/// recorded as the consume site.
pub(crate) fn consume_enum_lit_fields(
    tir: &Tir,
    flow: &ryo_core::flow::Flow,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    lit: TirRef,
) {
    for (_, value) in tir.enum_lit_view(lit).fields() {
        if !needs_tracking(tir.inst(value).ty, pool) {
            continue;
        }
        let span = tir.span(value);
        let consumed_name = consumed_binding_name(tir, value);
        // P2 freeze (final spec §3.2): the consume moves the owner.
        check_source_projected(
            tir,
            flow,
            pool,
            own,
            sink,
            own.underlying_owner(value),
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
    // Sema guarantees the object is a struct (named or anonymous) or an
    // enum (M12 payload access); a poisoned (error-typed) chain already
    // has a sema diagnostic, so don't add noise — the normal consume
    // path no-ops on it. M12's by-move match bindings make the enum arm
    // reachable: the owner tokens sema emits are `FieldAccess`es on the
    // enum-typed scrutinee, and any consume path that meets one must be
    // rejected exactly like a struct field read (payload fields move
    // only together with the whole enum).
    let obj_kind = pool.kind(obj_ty);
    if matches!(obj_kind, TypeKind::Enum) {
        // The wire's field_index is variant-relative and the access
        // site carries no variant — the payload shape cannot be named
        // the way the struct arm names `field_name`.
        let enum_display = pool.display(obj_ty);
        sink.emit(Diag::error(
            span,
            DiagCode::MoveOutOfField,
            format!(
                "cannot move a payload field out of `{enum_display}`; \
                 match on the value and bind the field, or move the whole value"
            ),
        ));
        return true;
    }
    if !matches!(obj_kind, TypeKind::Struct | TypeKind::AnonStruct) {
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
/// the one place fields move out of a struct. The rhs operand steps
/// already ran; the rhs struct owner is consumed here (a whole-struct
/// move: no partial-move state survives it), and each bound field
/// registers as a FRESH owner keyed on the `FieldAccess` token sema
/// emitted immediately before the `Destructure` instruction
/// (`Tir::destructure_bound_owner_refs`) — those refs are never
/// evaluated by codegen; they exist purely so the moved-out fields have
/// independent owner identities for the last-use / dead-store /
/// branch-divergence free passes, which lower through the binding's
/// slot at codegen (the binding-path redirect).
///
/// Wildcard fields are CODEGEN's jurisdiction — destroyed inline at
/// the copy-out, never tracked here — and no whole-struct Free is
/// scheduled for the shell: consuming the rhs owner stamps it `Moved`,
/// which every free pass skips. Copy-typed bindings need no owner.
pub(crate) fn analyze_destructure(
    w: &mut Walk<'_>,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    stmt: TirRef,
) {
    let flow = w.flow;
    let view = w.tir.destructure_view(stmt);
    let rhs = view.rhs;
    let rhs_ty = w.tir.inst(rhs).ty;
    if needs_tracking(rhs_ty, w.pool) {
        let span = w.tir.span(stmt);
        let consumed_name = consumed_binding_name(w.tir, rhs);
        // P2 freeze (final spec §3.2): the consume moves the owner.
        check_source_projected(
            w.tir,
            flow,
            w.pool,
            own,
            sink,
            own.underlying_owner(rhs),
            span,
            "move",
            consumed_name,
        );
        consume_for_assignment(w.tir, w.pool, own, sink, rhs, span, consumed_name, stmt);
    }

    let owners = w.tir.destructure_bound_owner_refs(stmt);
    let owner_places = w.flow.token_places(stmt);
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
        let _owner_ref = owners[owner_idx];
        let owner_place = owner_places[owner_idx];
        owner_idx += 1;
        own.binding_of_name.insert(bind, stmt);
        if !needs_tracking(field.ty, w.pool) {
            continue;
        }
        let span = w.tir.span(stmt);
        // The token's place is both the binding identity (Flow binds
        // the name directly to it) and the owner of the moved-out
        // field.
        let owner = Owner(owner_place);
        own.states.insert(owner, OwnerState::Valid);
        // A never-read bound field is a dead store (W0001) and its
        // allocation dies with the statement — the drain anchors the
        // Free right after it, where codegen has just populated the
        // binding's slot.
        register_pending_dead_store(own, owner, bind, span, stmt);
    }
    let _ = sidecar;
}

// ---------- Consume transitions (shared by every consume site) ----------

/// Use-site use-after-move authority (spec §5.3 Rule 1): resolve the
/// operand's underlying owner and emit E0020 if it is `Moved`.
pub(crate) fn check_use_moved(
    tir: &Tir,
    pool: &InternPool,
    own: &Ownership,
    sink: &mut DiagSink,
    operand: TirRef,
    span: Span,
) {
    let owner = own.underlying_owner(operand);
    if let Some(OwnerState::Moved { moved_at }) = own.states.get(&owner).cloned() {
        let name = consumed_binding_name(tir, operand);
        sink.emit(
            Diag::error(span, DiagCode::UseAfterMove,
                format!("use of moved value {}", format_binding(name, pool)))
                .with_note(Some(moved_at), "value moved here")
                .with_help("consider using the value before the move, or pass by default (borrow) instead of `move`"),
        );
    }
}

/// Which diagnostic the shared `consume_underlying` helper emits on the
/// `Borrowed` arm.
pub(crate) enum BorrowedAction {
    /// E0021 — assignment-site consume.
    MoveOutOfParam,
    /// E0022 — `analyze_return` site.
    ReturnBorrowed,
}

/// Apply the consumption transition for a Move-typed initializer.
/// Caller must have already populated origin/state for `init` via the
/// operand steps. `site` is the consuming instruction, recorded in the
/// W0003 hazard log when the consume lands.
#[allow(clippy::too_many_arguments)]
pub(crate) fn consume_for_assignment(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    init: TirRef,
    span: Span,
    name: Option<StringId>,
    site: TirRef,
) {
    consume_underlying(
        tir,
        pool,
        own,
        sink,
        init,
        span,
        name,
        BorrowedAction::MoveOutOfParam,
        site,
    );
}

/// Shared transition for every consume site: a needs-drop `FieldAccess`
/// operand is rejected outright (`MoveOutOfField`, M9); `Valid` → stamp
/// `Moved`, clear the pending dead-store entry, log the W0003 move
/// hazard; `Borrowed` → E0021/E0022; `Moved` → E0020; `NotTracked` →
/// no-op.
#[allow(clippy::too_many_arguments)]
pub(crate) fn consume_underlying(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    operand: TirRef,
    span: Span,
    name: Option<StringId>,
    on_borrowed: BorrowedAction,
    site: TirRef,
) {
    // M9: consuming a needs-drop field read moves only the field out of
    // its struct — rejected; the struct's fields move as one.
    if check_field_move_out(tir, pool, sink, operand, span) {
        return;
    }
    let underlying = own.underlying_owner(operand);
    let mut state = own
        .states
        .get(&underlying)
        .cloned()
        .unwrap_or(OwnerState::NotTracked);
    // A Valid value currently bound to an `inout str` param escapes
    // through the write-back pointer — treat it as borrowed at consume
    // sites.
    if matches!(state, OwnerState::Valid) && inout_escape_owner(own, underlying) {
        state = OwnerState::Borrowed;
    }
    match state {
        OwnerState::Valid => {
            own.pending_dead_store.remove(&underlying);
            own.states
                .insert(underlying, OwnerState::Moved { moved_at: span });
            own.owner_hazards.push((underlying, site, HazardKind::Move));
        }
        OwnerState::Borrowed => {
            let (code, msg, help) = match on_borrowed {
                BorrowedAction::MoveOutOfParam => (
                    DiagCode::MoveOutOfBorrowedParam,
                    format!(
                        "cannot move out of borrowed parameter {}",
                        format_binding(name, pool),
                    ),
                    "add `move` to the parameter declaration if you need ownership",
                ),
                BorrowedAction::ReturnBorrowed => (
                    DiagCode::ReturnBorrowedValue,
                    format!(
                        "cannot return borrowed value {} (Rule 5)",
                        format_binding(name, pool),
                    ),
                    "return a locally-allocated value, or accept the parameter as `move`",
                ),
            };
            sink.emit(Diag::error(span, code, msg).with_help(help));
        }
        OwnerState::Moved { .. } => {
            check_use_moved(tir, pool, own, sink, operand, span);
        }
        OwnerState::NotTracked => {}
    }
}
