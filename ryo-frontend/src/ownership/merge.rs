//! Branch-state merging — split from `mod.rs`; see module docs there.

use super::{Owner, OwnerState, Ownership};
use ryo_core::tir::{Span, TirRef};
use ryo_core::types::StringId;
use std::collections::{HashMap, HashSet};

impl Ownership {
    /// Conservatively merge per-branch lattices into `self`. Per-field rules:
    /// - `next_branch_id`: max across self + branches (monotonic; loop merges
    ///   must not roll the allocator backward).
    /// - `states`: any branch `Moved` → `Moved`; otherwise first observed
    ///   wins. So a value consumed on only one branch is still treated as
    ///   moved at the join point and a post-`if` use trips E0020.
    /// - `current_owner`, `origin`: first-write-wins across branches; reseats
    ///   inside a branch survive the join.
    /// - `temp_owners`: union (entries minted inside a branch/loop body
    ///   must survive so the anonymous-temp pass sees them at function
    ///   exit).
    /// - `owner_at_read`: union with first-write-wins (read TirRefs are
    ///   unique per TIR, so collisions don't happen in practice).
    /// - `pending_dead_store`: pre-branch keys intersect (any branch that
    ///   read the binding clears the dead-store warning); branch-local keys
    ///   union.
    /// - `root_owner`: first-write-wins, mirroring `origin` (a view's root
    ///   never changes, so conflicts don't happen in practice).
    /// - `live_projections`: union per root — a view live on any branch is
    ///   live at the join (P2). `analyze_if_stmt` prunes views whose last
    ///   use is inside the branch afterwards (they are dead on every path
    ///   at the join).
    /// - `owner_hazards`: union — the hazard log is monotone; duplicates
    ///   are harmless to its `any()`-shaped queries.
    /// - Final pass: per-binding state is recomputed through whichever
    ///   end-of-branch owner each branch left, so reseats inside a branch
    ///   contribute their state to the merged binding.
    pub(super) fn merge_branches(&mut self, branches: &[&Ownership]) {
        // BranchId monotonicity: never let a merge roll the allocator
        // backward. The loop fixed-point (analyze_loop_body) merges
        // only the four non-monotone fields via merge_non_monotone,
        // so next_branch_id minted inside a loop body survives into
        // post-loop ifs through `self`; this max rule is what keeps
        // if-arm merges from ever rolling it backward and colliding
        // in codegen's branch_blocks map.
        self.next_branch_id = std::cmp::max(
            self.next_branch_id,
            branches.iter().map(|b| b.next_branch_id).max().unwrap_or(0),
        );

        // Snapshot pre-branch (name → owner) bindings before we start
        // touching `self.states`. After the per-TirRef merge below
        // the binding-aware override (merge_binding_states) revisits
        // each pre-branch binding and recomputes its state through
        // whichever owner each branch ended on. Must be taken before
        // the `current_owner` union below, or branch-local bindings
        // would be mistaken for pre-branch ones.
        let pre_branch_owners = self.current_owner.clone();

        // Rule: any branch Moved → Moved; otherwise first observed
        // (across branches) wins. Walk each branch once and merge
        // directly into `self.states` — no intermediate set of keys.
        for b in branches {
            merge_states_any_moved_wins(&mut self.states, &b.states);
        }
        // Union the remaining branch-local fields. `current_owner` /
        // `origin` use first-wins via `or_insert`. `temp_owners` is
        // unioned so entries introduced inside a branch (or loop body)
        // survive the merge — without this a `StrConst`/`StrConcat`/Call
        // inside a `while` body is silently dropped from `temp_owners`
        // when the merged state starts from the pre-loop entry.
        // `owner_at_read`
        // keys are unique per TirRef (instructions aren't shared across
        // blocks), so each key appears in at most one branch and
        // `or_insert` is correct.
        for b in branches {
            merge_current_owner_first_wins(&mut self.current_owner, &b.current_owner);
            for (k, v) in &b.origin {
                self.origin.entry(*k).or_insert(*v);
            }
            self.temp_owners.extend(b.temp_owners.iter().copied());
            for (&read, &owner) in &b.owner_at_read {
                self.owner_at_read.entry(read).or_insert(owner);
            }
            // P3 root mapping: first-wins, mirroring `origin`.
            for (k, v) in &b.root_owner {
                self.root_owner.entry(*k).or_insert(*v);
            }
            // P2 freeze ranges: union per root (the caller prunes
            // views whose last use is inside the branch).
            union_live_projections(&mut self.live_projections, &b.live_projections);
            // W0003 hazard log: union (monotone; duplicates harmless).
            self.owner_hazards.extend(b.owner_hazards.iter().copied());
        }

        // Binding-aware override: recompute each pre-branch binding's
        // state through whichever owner each branch ended on (shared
        // with the loop fixed-point merge — see merge_binding_states).
        let sides: Vec<_> = branches
            .iter()
            .map(|b| (&b.current_owner, &b.states))
            .collect();
        merge_binding_states(&mut self.states, &pre_branch_owners, &sides);

        // Pending dead-store entries: pre-branch keys intersect,
        // branch-local keys union (see merge_pending_dead_store).
        let branch_stores: Vec<_> = branches.iter().map(|b| &b.pending_dead_store).collect();
        merge_pending_dead_store(&mut self.pending_dead_store, &branch_stores);
    }
}

/// Any-Moved-wins owner-state merge: a `Moved` entry in `src`
/// overwrites a non-`Moved` entry in `dst`, a key missing from `dst`
/// is inserted, and anything else keeps `dst`'s first-observed state.
/// Shared by `Ownership::merge_branches` (per branch, into
/// `self.states`) and `merge_non_monotone` (entry ⊔ post-body).
pub(crate) fn merge_states_any_moved_wins(
    dst: &mut HashMap<Owner, OwnerState>,
    src: &HashMap<Owner, OwnerState>,
) {
    for (r, s) in src {
        dst.entry(*r)
            .and_modify(|cur| {
                if !matches!(cur, OwnerState::Moved { .. }) && matches!(s, OwnerState::Moved { .. })
                {
                    *cur = s.clone();
                }
            })
            .or_insert_with(|| s.clone());
    }
}

/// First-write-wins merge of binding → owner entries: `dst` keeps
/// its existing entries, entries present only in `src` are copied
/// over. Shared by `Ownership::merge_branches` and
/// `merge_non_monotone`.
pub(crate) fn merge_current_owner_first_wins(
    dst: &mut HashMap<StringId, Owner>,
    src: &HashMap<StringId, Owner>,
) {
    for (&name, &owner) in src {
        dst.entry(name).or_insert(owner);
    }
}

/// One side of a binding-aware merge: the side's binding → owner map
/// paired with its owner-state map.
type MergeSide<'a> = (&'a HashMap<StringId, Owner>, &'a HashMap<Owner, OwnerState>);

/// Binding-aware override, shared by `Ownership::merge_branches`
/// (sides = the branch lattices) and `merge_non_monotone` (sides =
/// the loop-entry and post-body snapshots). For each name that
/// existed before the branches / loop body walked (`pre_owners`),
/// look up where each side left that binding (`current_owner[name]`,
/// falling back to the pre-branch owner when the side never touched
/// it), read that owner's state on that side, and merge across sides
/// with the same any-Moved-wins rule (first-observed otherwise; an
/// already-`Moved` merge is kept so the first-observed Moved span
/// survives for diagnostics). Write the merged state back onto the
/// pre-branch owner in `dst_states` so post-merge reads of `name`
/// (which still resolve via the pre-branch `current_owner[name]` =
/// owner_pre) see the union of what each side did to its respective
/// end-of-branch owner. Without this, post-merge reads of `name`
/// resolve through the pre-branch owner whose state reflects only
/// what happened to *that* TirRef, missing reseats inside a side.
///
/// NOT monotone: when a loop body reseats a binding (consume-then-
/// rebind), this merges the pre-reseat owner's entry state with the
/// post-reseat owner's post-body state, which can flip `Moved` back
/// to `Valid` on every merge. `analyze_loop_body`'s propagate phase
/// is capped at MAX_PROPAGATE_PASSES walks precisely because of this
/// (see the cap comment there and the maintainer note on
/// `analyze_while_loop`). `Ownership::merge_branches` merges a fixed
/// branch list, so monotonicity does not arise on that path.
pub(crate) fn merge_binding_states(
    dst_states: &mut HashMap<Owner, OwnerState>,
    pre_owners: &HashMap<StringId, Owner>,
    sides: &[MergeSide<'_>],
) {
    for (&name, &owner_pre) in pre_owners {
        let mut merged: Option<OwnerState> = None;
        for &(current_owner, states) in sides {
            let owner_side = current_owner.get(&name).copied().unwrap_or(owner_pre);
            let state_side = states
                .get(&owner_side)
                .cloned()
                .unwrap_or(OwnerState::NotTracked);
            // any-Moved-wins, otherwise first observed wins. When
            // `merged` is already Moved, keep it — preserves the
            // first-observed Moved span for diagnostics.
            let take = match (&merged, &state_side) {
                (None, _) => true,
                (Some(OwnerState::Moved { .. }), _) => false,
                (_, OwnerState::Moved { .. }) => true,
                _ => false,
            };
            if take {
                merged = Some(state_side);
            }
        }
        if let Some(state) = merged {
            dst_states.insert(owner_pre, state);
        }
    }
}

/// Pending dead-store merge. A key falls into one of two buckets
/// relative to the pre-merge snapshot in `dst`:
///
/// (1) Pre-existing entries (already in `dst` before the branches /
///     loop body walked): every side started with the entry. If any
///     side cleared it, the value was used somewhere — drop it.
///     Rule: intersect across sides.
///
/// (2) Local entries (introduced inside a side by a VarDecl): only
///     the introducing side has the key. If that side ended with the
///     entry still pending, W0001 should still fire after the join.
///     Rule: union across sides (skipping pre-existing keys — those
///     are governed by rule (1)).
///
/// Snapshots the pre-merge key set so the union step can distinguish
/// local keys from pre-existing keys that (1) may have just dropped.
/// Shared by `Ownership::merge_branches` (N sides) and
/// `merge_non_monotone` (one side: the post-body snapshot).
pub(crate) fn merge_pending_dead_store(
    dst: &mut HashMap<Owner, (StringId, Span, TirRef)>,
    branches: &[&HashMap<Owner, (StringId, Span, TirRef)>],
) {
    let pre_branch_keys: HashSet<Owner> = dst.keys().copied().collect();
    dst.retain(|k, _| branches.iter().all(|b| b.contains_key(k)));
    for b in branches {
        for (k, v) in *b {
            if !pre_branch_keys.contains(k) {
                dst.insert(*k, *v);
            }
        }
    }
}

/// P2 freeze ranges: union per root — a view live on any side is
/// live at the join (final spec §3.2). Monotone over membership, so
/// the loop fixed point's two walks still suffice. Callers prune
/// views whose last use is inside the branch / loop afterwards.
/// Shared by `Ownership::merge_branches` and `merge_non_monotone`.
pub(crate) fn union_live_projections(
    dst: &mut HashMap<Owner, Vec<Owner>>,
    src: &HashMap<Owner, Vec<Owner>>,
) {
    for (root, views) in src {
        let entry = dst.entry(*root).or_default();
        for v in views {
            if !entry.contains(v) {
                entry.push(*v);
            }
        }
    }
}

/// Loop fixed-point convergence comparison. Compares the full
/// state tuple — every tracked owner's full `OwnerState` (not just its
/// Moved-ness) plus the emptiness of each owner's live-projection set
/// — between the entry and post-body snapshots. A `Valid` ↔ `Borrowed`
/// flip or a change in freeze state across the back-edge must force a
/// re-walk, or P2 freeze state inside loop bodies is unsound.
pub(crate) fn states_differ_snapshot(
    a: &HashMap<Owner, OwnerState>,
    b: &HashMap<Owner, OwnerState>,
    a_live: &HashMap<Owner, Vec<Owner>>,
    b_live: &HashMap<Owner, Vec<Owner>>,
) -> bool {
    // Full OwnerState comparison. Missing entries default to
    // NotTracked, so a key present on only one side diverges unless
    // the other side's state is also NotTracked.
    let mut keys: HashSet<Owner> = a.keys().copied().collect();
    keys.extend(b.keys().copied());
    for k in keys {
        let av = a.get(&k).cloned().unwrap_or(OwnerState::NotTracked);
        let bv = b.get(&k).cloned().unwrap_or(OwnerState::NotTracked);
        if av != bv {
            return true;
        }
    }
    // Freeze-range comparison: a live projection on one side and none
    // on the other changes what consume sites inside the body
    // diagnose. Only EMPTINESS is compared — which particular view is
    // live does not change the freeze decision, and the union merge
    // is monotone over membership.
    let mut owners: HashSet<Owner> = a_live.keys().copied().collect();
    owners.extend(b_live.keys().copied());
    for o in owners {
        let a_live_o = a_live.get(&o).is_some_and(|v| !v.is_empty());
        let b_live_o = b_live.get(&o).is_some_and(|v| !v.is_empty());
        if a_live_o != b_live_o {
            return true;
        }
    }
    false
}

/// Merge only the non-monotone fields of two states (represented by their snapshots)
/// into `own`'s corresponding fields, leaving the monotone fields intact.
/// Shares its per-field merge rules with `Ownership::merge_branches`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_non_monotone(
    own: &mut Ownership,
    snap_states: &HashMap<Owner, OwnerState>,
    after_states: &HashMap<Owner, OwnerState>,
    snap_current_owner: &HashMap<StringId, Owner>,
    after_current_owner: &HashMap<StringId, Owner>,
    snap_pending_dead_store: &HashMap<Owner, (StringId, Span, TirRef)>,
    after_pending_dead_store: &HashMap<Owner, (StringId, Span, TirRef)>,
    snap_live_projections: &HashMap<Owner, Vec<Owner>>,
    after_live_projections: &HashMap<Owner, Vec<Owner>>,
) {
    // 1. Merge states: any side Moved -> Moved; otherwise first observed (snap_states) wins.
    let mut merged_states = snap_states.clone();
    merge_states_any_moved_wins(&mut merged_states, after_states);

    // Binding-aware override — NOT monotone (see merge_binding_states);
    // analyze_loop_body's propagate-phase cap depends on that.
    merge_binding_states(
        &mut merged_states,
        snap_current_owner,
        &[
            (snap_current_owner, snap_states),
            (after_current_owner, after_states),
        ],
    );

    // 2. Merge current_owner: first-wins.
    let mut merged_current_owner = snap_current_owner.clone();
    merge_current_owner_first_wins(&mut merged_current_owner, after_current_owner);

    // 3. Merge pending_dead_store: pre-existing keys intersect; local keys union.
    let mut merged_pending_dead_store = snap_pending_dead_store.clone();
    merge_pending_dead_store(&mut merged_pending_dead_store, &[after_pending_dead_store]);

    // 4. Merge live_projections: union per root (P2 freeze ranges,
    // final spec §3.2) — a view live on either side of the back-edge
    // stays live. Monotone, so the 2-pass fixed point still suffices.
    let mut merged_live_projections = snap_live_projections.clone();
    union_live_projections(&mut merged_live_projections, after_live_projections);

    own.states = merged_states;
    own.current_owner = merged_current_owner;
    own.pending_dead_store = merged_pending_dead_store;
    own.live_projections = merged_live_projections;
}
