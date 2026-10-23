//! Cleanup scheduling and destruction anchors.

use super::{Owner, OwnerState, Ownership, inout_escape_owner, owner_sort_key};
use ryo_core::flow::{BlockId, Flow, Terminator};
use ryo_core::tir::{Tir, TirRef, TirTag};
use std::collections::{HashMap, HashSet};

/// Document-order rank: the build's emission stamp
/// (`Flow::order_of`) — parents before children in source position,
/// dead continuations emitted in place. Replaces the old TIR pre-order
/// `program_order` scan; the Flow builder stamps every instruction
/// exactly once while lowering, so no second walk is needed.
pub(crate) fn document_rank(flow: &Flow, r: TirRef) -> u32 {
    flow.order_of(r).unwrap_or(0)
}

/// P5 (final spec §3.2): an owner's destruction is deferred to the
/// last use of any projection of it. Returns the later of the owner's
/// own `anchor` and its projections' last uses by program order. A
/// projection that is never read defers nothing — no one can observe
/// the buffer through it.
///
/// A projection's final use lives in the view-liveness table
/// (`view_last_use`), never in `last_use`: a view is consumed by its
/// binding/assign, not Var-read, so the owner-at-read-keyed table has
/// no entry for it. Reading only `last_use` let a base's destruction
/// anchor at a branch exit that fires before a reseated view's final
/// use (view-protection UAF). Type-agnostic — the tables are keyed by
/// view instruction, so strview/bytesview and future slice[T]/list[T]/
/// map[K][V] views defer their bases identically.
pub(crate) fn defer_anchor(
    anchor: TirRef,
    owner: &Owner,
    projections_of: &HashMap<Owner, Vec<TirRef>>,
    last_use: &HashMap<TirRef, TirRef>,
    view_last_use: &[Option<TirRef>],
    order: &[u32],
) -> TirRef {
    let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
    let mut best = anchor;
    if let Some(views) = projections_of.get(owner) {
        for &view in views {
            let uses = last_use
                .get(&view)
                .copied()
                .into_iter()
                .chain(Ownership::dense_get(view_last_use, view));
            for read in uses {
                if rank(read) > rank(best) {
                    best = read;
                }
            }
        }
    }
    best
}

/// Collect the init/value `TirRef` of every `VarDecl`/`Assign`/
/// `Destructure` in the body — the "named" producers whose Free is
/// owned by the last-use / dead-store / `free_on_reassign` / loop-exit
/// pass; the anon-temp pass skips them to avoid a double-free. Driven
/// from Flow IR steps: declarations are always stepped, and block
/// iteration covers arms and nested positions with no statement
/// recursion (the old match-descent machinery is unnecessary). Unlike a
/// `current_owner.values()` derivation this static set is merge-immune:
/// a temp reassigned inside a loop body is statically the loop-body
/// `Assign`'s value regardless of any loop-merge state.
pub(crate) fn collect_named_inits(tir: &Tir, flow: &Flow) -> HashSet<TirRef> {
    let mut set = HashSet::new();
    for b in flow.forward_order() {
        for step in flow.block_steps(b) {
            match tir.inst(step.inst).tag {
                TirTag::VarDecl => {
                    set.insert(tir.var_decl_view(step.inst).initializer);
                }
                TirTag::Assign => {
                    set.insert(tir.assign_view(step.inst).value);
                }
                TirTag::Destructure => {
                    // M10: a destructured rhs is "named" by the
                    // destructure — the anon-temp pass must skip it or
                    // it would schedule a whole-struct Free on top of
                    // the field moves (double-free).
                    set.insert(tir.destructure_view(step.inst).rhs);
                }
                _ => {}
            }
        }
    }
    set
}

/// Every `Return`/`ReturnVoid` in the walk's block order (document
/// order). The promotion-free return epilogue anchors a free at each
/// one, so an early-exit path cannot bypass a promotion buffer's single
/// normal anchor. Driven from Flow IR terminators: a return is a
/// terminator payload, and the walk order covers returns inside match
/// arms and nested positions with no statement recursion.
pub(crate) fn collect_return_stmts(flow: &Flow, block_order: &[BlockId], out: &mut Vec<TirRef>) {
    for &b in block_order {
        match flow.term(b) {
            Terminator::Return { .. } | Terminator::ReturnVoid => {
                if let Some(p) = flow.term_payload(b) {
                    out.push(p);
                }
            }
            _ => {}
        }
    }
}

/// Snapshot the owners still `Valid` at a return — values the function
/// must destroy on that exit path (see `Ownership::return_epilogue`).
/// The returned value itself is already `Moved` by `analyze_return`,
/// so it is naturally excluded; inout-escape owners are excluded too —
/// they leave through the write-back pointer, not through destruction.
/// Owners whose producer is ranked AFTER the return's own expression
/// subtree are also dropped: a value produced lexically later than the
/// return cannot exist when it executes. Such owners only appear `Valid`
/// here because loop convergence re-walks seed body-local owners from
/// the backedge state (`merge_non_monotone`) — recording them would
/// schedule a Free the return's path never needs (and codegen cannot
/// lower, since the producer is never materialized on that path).
pub(crate) fn record_return_epilogue(
    tir: &Tir,
    flow: &ryo_core::flow::Flow,
    own: &mut Ownership,
    return_stmt: TirRef,
) {
    // Rank table, built once per function and cached on `Ownership` —
    // loop convergence re-walks Returns, and rebuilding the per-
    // function Vec for each one is pure waste. The rank is the
    // Flow builder's emission stamp (see `document_rank`).
    if own.order.is_empty() {
        own.order = (0..tir.instructions.len() as u32)
            .map(|i| {
                if i == 0 {
                    0
                } else {
                    document_rank(flow, TirRef::from_raw(i))
                }
            })
            .collect();
    }
    let order = &own.order;
    let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
    // The return's operand subtree is contiguous in the build's
    // emission order, and the operand's own step is stamped LAST in
    // it — so the subtree's max rank is the value step's stamp. No
    // TIR operand recursion.
    let ret_rank = match tir.inst(return_stmt).data {
        ryo_core::tir::TirData::UnOp(value) => {
            flow.step_of(value).map(|s| rank(s.inst)).unwrap_or(0)
        }
        _ => rank(return_stmt),
    };

    let mut live: Vec<Owner> = own
        .states
        .iter()
        .filter(|(o, s)| {
            if !matches!(s, OwnerState::Valid) || inout_escape_owner(own, **o) {
                return false;
            }
            match o.inst_tirref(flow) {
                // Params predate every statement.
                None => true,
                Some(r) => rank(r) <= ret_rank,
            }
        })
        .map(|(o, _)| *o)
        .collect();
    // Sorted for deterministic sidecar emission order.
    live.sort_by_key(owner_sort_key);
    if !live.is_empty() {
        own.return_epilogue.push((return_stmt, live));
    }
}

/// Build a `child_TirRef → parent_TirRef` map from the Flow builder's
/// single-parent stamps: `Flow::parent_of` records the consuming
/// instruction of every operand while lowering — no TIR operand walk.
/// Used by the anonymous-temporary-free pass to anchor each temp's
/// Free after its single consumer. (Body-statement edges contribute
/// nothing: `parent_of` stamps `Operand` kind only.)
pub(crate) fn find_consumers(
    flow: &Flow,
    arena_len: usize,
    consumer_of: &mut HashMap<TirRef, TirRef>,
) {
    // The Flow builder stamps the single consuming parent of every
    // operand while lowering (`Flow::parent_of`) — the tree-shaped
    // TIR gives each inst at most one parent, so this is the complete
    // child → consumer map with no TIR operand recursion.
    for i in 1..arena_len {
        let r = TirRef::from_raw(u32::try_from(i).expect("TIR arena index fits u32"));
        if let Some(parent) = flow.parent_of(r) {
            consumer_of.entry(r).or_insert(parent);
        }
    }
}
