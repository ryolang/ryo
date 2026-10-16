//! Ownership transfer and moved-value checks.

use super::{
    HazardKind, OwnerState, Ownership, check_field_move_out, consumed_binding_name, format_binding,
    inout_escape_owner,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::tir::{Span, Tir, TirRef};
use ryo_core::types::{InternPool, StringId};

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
