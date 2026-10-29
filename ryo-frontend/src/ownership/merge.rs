//! Branch-state merging — split from `mod.rs`; see module docs there.

use super::{Owner, OwnerState, Ownership};
use ryo_core::flow::PlaceId;
use ryo_core::tir::TirRef;
use ryo_core::types::StringId;
use std::collections::{HashMap, HashSet};

/// The four non-monotone `Ownership` fields, extracted as a unit for
/// per-arm / per-loop-pass snapshot and restore. Everything else in
/// `Ownership` is monotone (arms accumulate it in place and never
/// restore it) or walk-constant — see the field docs in `mod.rs`.
#[derive(Clone, Default)]
pub(crate) struct BranchState {
    pub states: HashMap<Owner, OwnerState>,
    /// Binding home place → the owner seated in it (a shadowing
    /// declaration mints a new place, so same-named bindings in
    /// different scopes never collide).
    pub current_owner: HashMap<PlaceId, Owner>,
    /// Name → the lexical binding the name currently resolves to (the
    /// declaring `VarDecl` inst, or a param's virtual ref) — the
    /// scope-resolution record, seeded from write facts only.
    pub binding_of_name: HashMap<StringId, TirRef>,
    pub live_projections: HashMap<Owner, Vec<Owner>>,
}

impl Ownership {
    /// Clone the four non-monotone fields — the single snapshot taken
    /// per branch point / loop.
    pub(crate) fn snapshot_branch(&self) -> BranchState {
        BranchState {
            states: self.states.clone(),
            current_owner: self.current_owner.clone(),
            binding_of_name: self.binding_of_name.clone(),
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
            binding_of_name: std::mem::replace(&mut self.binding_of_name, restore.binding_of_name),
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
    /// - `live_projections`: union per root — a view live on any branch is
    ///   live at the join (P2). `analyze_if_stmt` prunes views whose last
    ///   use is inside the branch afterwards (they are dead on every path
    ///   at the join).
    /// - Final pass: per-binding state is recomputed through whichever
    ///   end-of-branch owner each branch left, so reseats inside a branch
    ///   contribute their state to the merged binding.
    ///
    /// Monotone fields (`origin`, `owner_at_read`, `temp_owners`,
    /// `root_owner`, `owner_hazards`, `reseat_drops`, `return_epilogue`)
    /// are deliberately NOT merged: arms walk the same
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
            // Binding identity follows the same first-wins rule as
            // `current_owner`: post-merge, a name resolves to the
            // pre-branch binding unless every side re-bound it.
            merge_name_map_first_wins(&mut self.binding_of_name, &b.binding_of_name);
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

/// First-write-wins merge of key→owner entries: `dst` keeps its
/// existing entries, entries present only in `src` are copied over.
/// Shared by `Ownership::merge_branches` and `merge_non_monotone` for
/// `current_owner` (home-place keyed), `binding_of_name` (name keyed),
/// and `binding_of_name`-style maps (same join semantics: a
/// branch-local rebinding does not survive the join).
pub(crate) fn merge_current_owner_first_wins(
    dst: &mut HashMap<PlaceId, Owner>,
    src: &HashMap<PlaceId, Owner>,
) {
    merge_name_map_first_wins(dst, src);
}

/// The key-type-generic half of [`merge_current_owner_first_wins`].
pub(crate) fn merge_name_map_first_wins<K: Copy + Eq + std::hash::Hash, V: Copy>(
    dst: &mut HashMap<K, V>,
    src: &HashMap<K, V>,
) {
    for (&name, &value) in src {
        dst.entry(name).or_insert(value);
    }
}

/// One side of a binding-aware merge: the side's binding → owner map
/// paired with its owner-state map.
type MergeSide<'a> = (&'a HashMap<PlaceId, Owner>, &'a HashMap<Owner, OwnerState>);

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
/// to `Valid`. The loop's propagate phase (G3) therefore keeps this
/// override OUT of its monotone iteration and applies it once as a
/// final polish on the converged entry — see the proof comment in
/// `analyze_loop`. `Ownership::merge_branches` merges a fixed branch
/// list, so monotonicity does not arise on that path.
pub(crate) fn merge_binding_states(
    dst_states: &mut HashMap<Owner, OwnerState>,
    pre_owners: &HashMap<PlaceId, Owner>,
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
    pre_owners: &HashMap<PlaceId, Owner>,
    sides: &[MergeSide<'_>],
) -> Vec<(Owner, OwnerState)> {
    let mut writes: Vec<(Owner, OwnerState)> = Vec::with_capacity(pre_owners.len());
    for (&home, &owner_pre) in pre_owners {
        let mut merged: Option<OwnerState> = None;
        for &(current_owner, states) in sides {
            let owner_side = current_owner.get(&home).copied().unwrap_or(owner_pre);
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
/// Moved-ness) plus the MEMBERSHIP of each owner's live-projection set
/// (order- and duplicate-insensitive) — between the entry and post-body
/// snapshots. A `Valid` ↔ `Borrowed` flip or a change in which views
/// freeze a root across the back-edge must force a re-walk, or P2 freeze
/// state inside loop bodies is unsound.
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
    // Freeze-range comparison: which views are live per owner. The
    // merged set is exactly the state the next iteration consumes:
    // membership decides where freeze violations are diagnosed inside
    // the body and which projections later passes create or prune.
    // `{v1}` vs `{v1, v2}` are different states even though both are
    // non-empty — comparing only emptiness can claim a fixed point
    // that was never walked. Compare sets semantically (order- and
    // duplicate-insensitive). The union merge is monotone over
    // membership, so a membership change here is a genuine ascension
    // (see the height argument in loops.rs).
    let mut owners: HashSet<Owner> = a_live.keys().copied().collect();
    owners.extend(b_live.keys().copied());
    for o in owners {
        let a_views: &[Owner] = a_live.get(&o).map_or(&[], Vec::as_slice);
        let b_views: &[Owner] = b_live.get(&o).map_or(&[], Vec::as_slice);
        if !same_live_views(a_views, b_views) {
            return true;
        }
    }
    false
}

/// Set-membership equality of two live-view lists: order- and
/// duplicate-insensitive. Two-directional `contains` over the small
/// per-owner lists — no allocation; O(n·m) with n, m the live-view
/// count (a handful at most). A key present with an empty vec and a
/// missing key are both the empty set, hence equal.
fn same_live_views(a: &[Owner], b: &[Owner]) -> bool {
    a.iter().all(|v| b.contains(v)) && b.iter().all(|v| a.contains(v))
}

/// Loop fixed-point convergence comparison (G3 fix): does the merged
/// loop-entry state now installed in `own` differ from the PREVIOUS
/// entry snapshot `prev`? The convergence test must compare against the
/// state the iteration actually feeds back — the MERGE RESULT — not the
/// raw post-body state: when the body's live exits are Return-only the
/// merge discards the body's changes (path-local to the exit), the
/// merged entry equals the previous entry (a genuine fixed point), and
/// the raw post-body state keeps differing forever (cap abort).
///
/// Covered components — every field `merge_loop_plain` can mutate, so no
/// merge-mutated component can change while this says false:
/// 1. `states` — full `OwnerState` comparison (not just Moved-ness).
/// 2. `live_projections` — per-owner MEMBERSHIP of the view set
///    (order- and duplicate-insensitive): membership decides where
///    freeze violations are diagnosed inside the body and which
///    projections later passes create or prune, so `{v1}` vs `{v1, v2}`
///    must force a re-walk even though both are non-empty — see
///    states_differ_snapshot.
/// 3. `current_owner` — the binding homes (full key/value comparison);
///    a body-local home inserted by the merge requires a re-walk.
/// 4. `binding_of_name` — the scope-resolution record, same rule.
pub(crate) fn branch_states_differ(prev: &BranchState, own: &Ownership) -> bool {
    if states_differ_snapshot(
        &prev.states,
        &own.states,
        &prev.live_projections,
        &own.live_projections,
    ) {
        return true;
    }
    map_differs(&prev.current_owner, &own.current_owner)
        || map_differs(&prev.binding_of_name, &own.binding_of_name)
}

/// Key/value equality of two maps (differ = key-set or value mismatch).
fn map_differs<K: Eq + std::hash::Hash, V: PartialEq>(
    a: &HashMap<K, V>,
    b: &HashMap<K, V>,
) -> bool {
    if a.len() != b.len() {
        return true;
    }
    a.iter().any(|(k, v)| b.get(k).is_none_or(|bv| bv != v))
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
    let writes = merge_loop_plain(own, entry, after, after_flows_off_end);
    // Binding-aware override (NOT monotone — see binding_state_writes):
    // applied only by callers that do not feed the result back into
    // further iteration (the loop's final polish / the final merge).
    for (owner, state) in writes {
        own.states.insert(owner, state);
    }
}

/// The plain loop-entry join — MONOTONE (G3): per-owner any-Moved-wins
/// (Moved is absorbing; first-observed otherwise), home/binding maps
/// first-wins (the entry side is fixed, so these never change after the
/// first merge), projection union. Returns
/// the binding-aware override writes UNAPPLIED so the caller decides
/// (iteration: discard; final polish/merge: apply once, no feedback).
///
/// `after_flows_off_end == false` (the body's live exits are Return-only):
/// the post-body state reaches neither the back-edge nor the post-loop
/// join, so — exactly like its `Moved` entries, which are discarded
/// above — its binding-state side is EXCLUDED from the binding-aware
/// override: a reseat/move on a path to the exit is path-local there
/// (the return epilogue owns destruction) and must not be written into
/// the joined binding state. The override then has only the entry side,
/// which writes each pre-loop owner's own state back — a pure no-op —
/// so `writes` is empty.
pub(crate) fn merge_loop_plain(
    own: &mut Ownership,
    entry: BranchState,
    after: BranchState,
    after_flows_off_end: bool,
) -> Vec<(Owner, OwnerState)> {
    let mut after = after;
    if !after_flows_off_end {
        after
            .states
            .retain(|_, s| !matches!(s, OwnerState::Moved { .. }));
    }
    // Computed while `entry`/`after` are intact; the caller applies.
    // One side per fall-through policy (see the doc above).
    let writes = if after_flows_off_end {
        binding_state_writes(
            &entry.current_owner,
            &[
                (&entry.current_owner, &entry.states),
                (&after.current_owner, &after.states),
            ],
        )
    } else {
        Vec::new()
    };

    // 1. Merge states: any side Moved -> Moved; otherwise entry (first
    //    observed) wins.
    let mut merged_states = entry.states;
    merge_states_any_moved_wins(&mut merged_states, &after.states);

    // 2. Merge current_owner: first-wins.
    let mut merged_current_owner = entry.current_owner;
    merge_current_owner_first_wins(&mut merged_current_owner, &after.current_owner);

    // 2b. Merge binding_of_name: first-wins, same rule.
    let mut merged_binding_of_name = entry.binding_of_name;
    merge_name_map_first_wins(&mut merged_binding_of_name, &after.binding_of_name);

    // 3. Merge live_projections: union per root (P2 freeze ranges,
    //    final spec §3.2) — a view live on either side of the back-edge
    //    stays live. Monotone (membership grows in one direction only).
    let mut merged_live_projections = entry.live_projections;
    union_live_projections(&mut merged_live_projections, &after.live_projections);

    own.states = merged_states;
    own.current_owner = merged_current_owner;
    own.binding_of_name = merged_binding_of_name;
    own.live_projections = merged_live_projections;
    writes
}
