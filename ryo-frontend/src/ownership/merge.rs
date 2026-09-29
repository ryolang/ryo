//! Branch-state merging — split from `mod.rs`; see module docs there.

use super::{Owner, OwnerState, Ownership};
use ryo_core::tir::{Span, TirRef};
use ryo_core::types::StringId;
use std::collections::{HashMap, HashSet};

/// The four non-monotone `Ownership` fields, extracted as a unit for
/// per-arm / per-loop-pass snapshot and restore. Everything else in
/// `Ownership` is monotone (arms accumulate it in place and never
/// restore it) or walk-constant — see the field docs in `mod.rs`.
#[derive(Clone, Default)]
pub(crate) struct BranchState {
    pub states: HashMap<Owner, OwnerState>,
    pub current_owner: HashMap<StringId, Owner>,
    pub pending_dead_store: HashMap<Owner, (StringId, Span, TirRef)>,
    pub live_projections: HashMap<Owner, Vec<Owner>>,
}

impl Ownership {
    /// Clone the four non-monotone fields — the single snapshot taken
    /// per branch point / loop.
    pub(crate) fn snapshot_branch(&self) -> BranchState {
        BranchState {
            states: self.states.clone(),
            current_owner: self.current_owner.clone(),
            pending_dead_store: self.pending_dead_store.clone(),
            live_projections: self.live_projections.clone(),
        }
    }

    /// Move the four non-monotone fields out (installing `restore`
    /// in their place, without cloning it) so a finished arm / loop
    /// pass can hand its end state to the merge.
    pub(crate) fn take_branch(&mut self, restore: BranchState) -> BranchState {
        BranchState {
            states: std::mem::replace(&mut self.states, restore.states),
            current_owner: std::mem::replace(&mut self.current_owner, restore.current_owner),
            pending_dead_store: std::mem::replace(
                &mut self.pending_dead_store,
                restore.pending_dead_store,
            ),
            live_projections: std::mem::replace(
                &mut self.live_projections,
                restore.live_projections,
            ),
        }
    }
}

impl Ownership {
    /// Conservatively merge per-branch lattices into `self`. On entry,
    /// `self`'s four non-monotone fields hold the pre-branch state (the
    /// caller's snapshot); each arm's end state merges into them in arm
    /// order. Per-field rules:
    /// - `states`: any branch `Moved` → `Moved`; otherwise first observed
    ///   wins — but only across branches that fall through. A branch whose
    ///   end-state terminates in `Return`/`Break`/`Continue`
    ///   (`falls_through[i] == false`) contributes nothing: its move is
    ///   path-local to an exit the join point never sees (the return
    ///   epilogue / loop-exit passes own destruction on those paths), so
    ///   a conditional `return` of a value no longer poisons post-`if`
    ///   uses with E0020.
    /// - `current_owner`: first-write-wins across branches; reseats
    ///   inside a branch survive the join.
    /// - `pending_dead_store`: pre-branch keys intersect (any branch that
    ///   read the binding clears the dead-store warning); branch-local keys
    ///   union.
    /// - `live_projections`: union per root — a view live on any branch is
    ///   live at the join (P2). `analyze_if_stmt` prunes views whose last
    ///   use is inside the branch afterwards (they are dead on every path
    ///   at the join).
    /// - Final pass: per-binding state is recomputed through whichever
    ///   end-of-branch owner each branch left, so reseats inside a branch
    ///   contribute their state to the merged binding.
    ///
    /// Monotone fields (`origin`, `owner_at_read`, `temp_owners`,
    /// `root_owner`, `owner_hazards`, `reseat_drops`, `return_epilogue`,
    /// `next_branch_id`) are deliberately NOT merged: arms walk the same
    /// `Ownership` in place and never restore those fields, so `self`
    /// already holds every arm's contribution and a union would be a
    /// subset-into-superset no-op.
    pub(super) fn merge_branches(&mut self, branches: Vec<BranchState>, falls_through: &[bool]) {
        debug_assert_eq!(
            branches.len(),
            falls_through.len(),
            "one fall-through flag per branch"
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
        // (across branches) wins. Branches that do not fall through
        // (Return/Break/Continue terminator) are skipped — their Moved
        // state is path-local to an exit the join never sees.
        for (b, &ft) in branches.iter().zip(falls_through) {
            if ft {
                merge_states_any_moved_wins(&mut self.states, &b.states);
            }
        }
        for b in &branches {
            merge_current_owner_first_wins(&mut self.current_owner, &b.current_owner);
            // P2 freeze ranges: union per root (the caller prunes
            // views whose last use is inside the branch).
            union_live_projections(&mut self.live_projections, &b.live_projections);
        }

        // Binding-aware override: recompute each pre-branch binding's
        // state through whichever owner each branch ended on (shared
        // with the loop fixed-point merge — see merge_binding_states).
        // Non-fall-through branches are excluded from the sides for the
        // same reason as the per-branch Moved merge above.
        let sides: Vec<_> = branches
            .iter()
            .zip(falls_through)
            .filter(|&(_, &ft)| ft)
            .map(|(b, _)| (&b.current_owner, &b.states))
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
    for (owner, state) in binding_state_writes(pre_owners, sides) {
        dst_states.insert(owner, state);
    }
}

/// Compute the binding-aware override writes without touching `dst`
/// (see merge_binding_states for semantics). Split out so by-value
/// callers (`merge_non_monotone`) can run it while the side maps are
/// still intact, then apply the writes after moving a side into the
/// merge accumulator.
pub(crate) fn binding_state_writes(
    pre_owners: &HashMap<StringId, Owner>,
    sides: &[MergeSide<'_>],
) -> Vec<(Owner, OwnerState)> {
    let mut writes: Vec<(Owner, OwnerState)> = Vec::with_capacity(pre_owners.len());
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
            writes.push((owner_pre, state));
        }
    }
    writes
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

/// Merge only the non-monotone fields of two states into `own`'s
/// corresponding fields, leaving the monotone fields intact. Takes
/// both states BY VALUE: `entry`'s maps become the merge accumulators,
/// so no snapshot clones happen here. Shares its per-field merge rules
/// with `Ownership::merge_branches`.
///
/// `after_flows_off_end` — false when the loop body's end-state
/// terminates in an exit (a trailing `return`): the post-body state
/// reaches neither the back-edge nor the post-loop join, so its
/// `Moved` entries are path-local to the exit and must not poison the
/// entry state (the return epilogue owns destruction there). When
/// false, `after`'s `Moved` entries are dropped before merging. This
/// is structural (whole-body), never per-owner: a `break`/`continue`
/// end-state still flows to the join / back-edge and keeps
/// contributing.
pub(crate) fn merge_non_monotone(
    own: &mut Ownership,
    entry: BranchState,
    after: BranchState,
    after_flows_off_end: bool,
) {
    let mut after = after;
    if !after_flows_off_end {
        after
            .states
            .retain(|_, s| !matches!(s, OwnerState::Moved { .. }));
    }
    // Binding-aware override computed FIRST, while `entry`/`after` are
    // intact — NOT monotone (see binding_state_writes);
    // analyze_loop_body's propagate-phase cap depends on that. Applied
    // after the Moved-merge, matching the old clone-then-merge order.
    let writes = binding_state_writes(
        &entry.current_owner,
        &[
            (&entry.current_owner, &entry.states),
            (&after.current_owner, &after.states),
        ],
    );

    // 1. Merge states: any side Moved -> Moved; otherwise entry (first
    //    observed) wins.
    let mut merged_states = entry.states;
    merge_states_any_moved_wins(&mut merged_states, &after.states);
    for (owner, state) in writes {
        merged_states.insert(owner, state);
    }

    // 2. Merge current_owner: first-wins.
    let mut merged_current_owner = entry.current_owner;
    merge_current_owner_first_wins(&mut merged_current_owner, &after.current_owner);

    // 3. Merge pending_dead_store: pre-existing keys intersect; local keys union.
    let mut merged_pending_dead_store = entry.pending_dead_store;
    merge_pending_dead_store(&mut merged_pending_dead_store, &[&after.pending_dead_store]);

    // 4. Merge live_projections: union per root (P2 freeze ranges,
    //    final spec §3.2) — a view live on either side of the back-edge
    //    stays live. Monotone, so the 2-pass fixed point still suffices.
    let mut merged_live_projections = entry.live_projections;
    union_live_projections(&mut merged_live_projections, &after.live_projections);

    own.states = merged_states;
    own.current_owner = merged_current_owner;
    own.pending_dead_store = merged_pending_dead_store;
    own.live_projections = merged_live_projections;
}
