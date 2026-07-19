//! Ownership pass — validates move safety on per-`TirRef` lattice.
//!
//! Runs between sema and codegen. Walks each `Tir` forward, tracking
//! ownership state for every Move-typed value. Catches use-after-move,
//! moves out of borrowed parameters, and returns of borrowed values.
//! Emits diagnostics into the shared `DiagSink` — does not mutate TIR
//! and does not insert Free instructions (that lands in M8.1c).
//!
//! ## State lattice
//!
//! Per-TirRef state, not per-binding. A binding name resolves through
//! a shadow `current_owner: HashMap<StringId, TirRef>` to whichever
//! SSA value currently owns the underlying allocation. Anonymous owned
//! temporaries (concat results, formatter outputs) live in the same
//! `states` map with no shadow entry.
//!
//! See `docs/superpowers/specs/2026-05-20-milestone-8.1-heap-str-and-move-semantics-design.md`
//! sub-milestone 8.1b for the full algorithm.
//!
//! ## Mojo reference
//!
//! See `docs/dev/mojo_reference.md`.

use crate::builtins::is_borrowed_scalar_param;
use ryo_core::diag::{Diag, DiagCode, DiagSink};
pub use ryo_core::ownership::{
    BranchId, FreePoint, FunctionSidecar, IfBranchIds, OwnershipSidecar,
};
use ryo_core::tir::{ParamMode, Span, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeId, TypeKind};
use std::collections::{HashMap, HashSet};

// ---------- Classification ----------

/// True for types whose values transfer ownership on `=` and must be
/// tracked through the function body. Today: `str` only. Future heap
/// types (`List[T]`, `Dict[K, V]`) will join this set.
pub(crate) fn is_move_type(ty: TypeId, pool: &InternPool) -> bool {
    matches!(pool.kind(ty), TypeKind::Str)
}

/// Predicate the ownership walk uses to decide whether a `TirRef`
/// needs a lattice slot. Currently identical to `is_move_type`, but
/// kept as its own name so the walk reads correctly when borrows
/// land and the answer becomes "move OR borrowed-of-move".
pub(crate) fn needs_tracking(ty: TypeId, pool: &InternPool) -> bool {
    is_move_type(ty, pool)
}

// ---------- Lattice ----------

/// Per-`TirRef` ownership state. Anything Copy-typed lives in
/// `NotTracked` for its whole lifetime (the walk skips it). Move-
/// typed values start at `Valid` on definition, transition to
/// `Borrowed` while a borrow is live, and to `Moved` once consumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OwnerState {
    NotTracked,
    Valid,
    Borrowed,
    Moved { moved_at: Span },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Owner {
    Param(StringId),
    Inst(TirRef),
}

impl Owner {
    /// Return the underlying `TirRef` for an `Inst` owner, or `None`
    /// for a `Param`. Used wherever a Free target / codegen lookup is
    /// needed (FreePoint.target, free_on_reassign values, inst_values).
    fn inst_tirref(self) -> Option<TirRef> {
        match self {
            Owner::Inst(r) => Some(r),
            Owner::Param(_) => None,
        }
    }

    pub(crate) fn tirref(self, tir: &Tir) -> TirRef {
        match self {
            Owner::Inst(r) => r,
            Owner::Param(name) => {
                let idx = tir
                    .params
                    .iter()
                    .position(|p| p.name == name)
                    .expect("param exists");
                TirRef::param(idx)
            }
        }
    }
}

/// Per-function ownership state. `states` is the lattice itself,
/// keyed by the `TirRef` that produced the value. `current_owner`
/// is a shadow map from binding name to whichever SSA value
/// currently owns the underlying allocation (so reassignment
/// reseats ownership without disturbing the producing SSA value).
/// `origin` records, for each tracked `TirRef`, the upstream value
/// it derives from (or `None` for fresh allocations) — used to walk
/// back to the root owner when diagnosing a use-after-move.
#[derive(Default, Clone)]
pub(crate) struct Ownership {
    pub states: HashMap<Owner, OwnerState>,
    pub current_owner: HashMap<StringId, Owner>,
    pub origin: HashMap<TirRef, Option<Owner>>,
    /// VarDecls of Move-typed values, keyed by the underlying owner
    /// `TirRef`. Cleared when the binding is read (`Var`) or consumed
    /// (move/return). Whatever remains at function end is a dead
    /// store — surfaced as W0001 + a Free anchored after the
    /// declaring/assigning instruction. The third tuple element is
    /// the `VarDecl`/`Assign` instruction's own `TirRef`, used as
    /// the anchor for the dead-store Free.
    pub pending_dead_store: HashMap<Owner, (StringId, Span, TirRef /* decl_inst */)>,

    /// SSA values that allocated heap-owned strings during the
    /// forward walk: `StrConst`, `StrConcat`, and Move-typed `Call`
    /// results. Used by the anonymous-temporary-free pass to identify
    /// candidates for scheduling. A temp_owner that ends up bound to
    /// a `VarDecl`/`Assign` is a "named init" and is skipped by the
    /// anon-temp pass (classified statically via `collect_named_inits`,
    /// I-060) — it is freed via the last-use / dead-store /
    /// `free_on_reassign` / loop-exit pass instead.
    pub temp_owners: HashSet<Owner>,

    /// Per-`Var`-read snapshot of the owner that was live at the
    /// program point of the read. Populated during the forward walk
    /// (`visit_expr`'s `Var` arm) and consulted by `collect_last_uses`
    /// instead of resolving through `current_owner`'s end-of-function
    /// state — which would misroute reads that precede a `mut`
    /// reassignment to the post-rebind owner. For Move-typed reads
    /// this anchors the last-use Free to the correct allocation.
    pub owner_at_read: HashMap<TirRef, Owner>,

    /// Monotonic `BranchId` allocator. Bumped each time
    /// `analyze_if_stmt` enters an arm (then / each elif / else) so
    /// the resulting ids are unique across the function body.
    pub next_branch_id: u32,

    /// Names of `inout` parameters whose type is Move-tracked (i.e.
    /// `str` in v0.1). The value bound to such a param ESCAPES through
    /// the write-back pointer at function exit, so it must not be freed
    /// by the callee (no last-use/dead-store Free, no W0001) and must
    /// not be moved out — but reassigning the param drops the old
    /// pointee. Constant per function (derived from `tir.params`), so
    /// branch merges need no per-field rule for it.
    pub inout_str_params: HashSet<StringId>,
}

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
    /// - Final pass: per-binding state is recomputed through whichever
    ///   end-of-branch owner each branch left, so reseats inside a branch
    ///   contribute their state to the merged binding.
    fn merge_branches(&mut self, branches: &[&Ownership]) {
        // BranchId monotonicity: never let a merge roll the allocator
        // backward. analyze_while_loop and analyze_for_range build
        // their post-loop state via merge_two(&entry, &after_body),
        // which clones `entry` — that would discard any BranchIds
        // minted inside the loop body and let post-loop ifs reuse
        // them, colliding in codegen's branch_blocks map.
        self.next_branch_id = std::cmp::max(
            self.next_branch_id,
            branches.iter().map(|b| b.next_branch_id).max().unwrap_or(0),
        );

        // Snapshot pre-branch (name, owner) pairs before we start
        // touching `self.states`. After the per-TirRef merge below
        // we revisit each pre-branch binding and recompute its state
        // through whichever owner each branch ended on, so a branch
        // that reseats `current_owner[name]` to a fresh TirRef still
        // contributes its end-of-branch state to the merged binding.
        // Without this, post-`if` reads of `name` resolve through the
        // pre-branch owner whose state reflects only what happened to
        // *that* TirRef, missing reseats inside branches.
        let pre_branch_owners: Vec<(StringId, Owner)> =
            self.current_owner.iter().map(|(n, t)| (*n, *t)).collect();

        // Rule: any branch Moved → Moved; otherwise first observed
        // (across branches) wins. Walk each branch once and merge
        // directly into `self.states` — no intermediate set of keys.
        for b in branches {
            for (r, s) in &b.states {
                self.states
                    .entry(*r)
                    .and_modify(|cur| {
                        if !matches!(cur, OwnerState::Moved { .. })
                            && matches!(s, OwnerState::Moved { .. })
                        {
                            *cur = s.clone();
                        }
                    })
                    .or_insert_with(|| s.clone());
            }
        }
        // Union the remaining branch-local fields. `current_owner` /
        // `origin` use first-wins via `or_insert`. `temp_owners` is
        // unioned so entries introduced inside a branch (or loop body)
        // survive the merge — without this a `StrConst`/`StrConcat`/Call
        // inside a `while` body is silently dropped from `temp_owners`
        // when `merge_two` clones the pre-loop entry. `owner_at_read`
        // keys are unique per TirRef (instructions aren't shared across
        // blocks), so each key appears in at most one branch and
        // `or_insert` is correct.
        for b in branches {
            for (name, owner) in &b.current_owner {
                self.current_owner.entry(*name).or_insert(*owner);
            }
            for (k, v) in &b.origin {
                self.origin.entry(*k).or_insert(*v);
            }
            self.temp_owners.extend(b.temp_owners.iter().copied());
            for (&read, &owner) in &b.owner_at_read {
                self.owner_at_read.entry(read).or_insert(owner);
            }
        }

        // Binding-aware override: for each name that existed before
        // the branches walked, look up where each branch left that
        // binding (b.current_owner[name]), read that owner's state in
        // the branch, and merge across branches with the same any-
        // Moved-wins rule. Write the merged state back onto the
        // pre-branch owner in `self.states` so post-merge reads of
        // `name` (which still resolve via `self.current_owner[name]`
        // = owner_pre) see the union of what each branch did to its
        // respective end-of-branch owner.
        for (name, owner_pre) in &pre_branch_owners {
            let mut merged: Option<OwnerState> = None;
            for b in branches {
                let owner_b = b.current_owner.get(name).copied().unwrap_or(*owner_pre);
                let state_b = b
                    .states
                    .get(&owner_b)
                    .cloned()
                    .unwrap_or(OwnerState::NotTracked);
                // any-Moved-wins, otherwise first observed wins. When
                // `merged` is already Moved, keep it — preserves the
                // first-observed Moved span for diagnostics.
                let take = match (&merged, &state_b) {
                    (None, _) => true,
                    (Some(OwnerState::Moved { .. }), _) => false,
                    (_, OwnerState::Moved { .. }) => true,
                    _ => false,
                };
                if take {
                    merged = Some(state_b);
                }
            }
            if let Some(state) = merged {
                self.states.insert(*owner_pre, state);
            }
        }

        // Pending dead-store entries: a key falls into one of two
        // buckets relative to the pre-branch snapshot in `self`.
        //
        // (1) Pre-branch entries (already in `self.pending_dead_store`
        //     before the branches walked): every branch started with
        //     the entry. If any branch cleared it, the value was
        //     used somewhere — drop it. Rule: intersect across
        //     branches.
        //
        // (2) Branch-local entries (introduced inside a branch by a
        //     VarDecl): only the introducing branch has the key. If
        //     that branch ended with the entry still pending, W0001
        //     should still fire after the join. Rule: union across
        //     branches (skipping keys that were pre-branch — those
        //     are governed by rule (1)).
        //
        // Snapshot the pre-branch key set so the union step can
        // distinguish branch-local keys from pre-branch keys that
        // (1) may have just dropped.
        let pre_branch_keys: HashSet<Owner> = self.pending_dead_store.keys().copied().collect();
        self.pending_dead_store.retain(|k, _| {
            branches
                .iter()
                .all(|b| b.pending_dead_store.contains_key(k))
        });
        for b in branches {
            for (k, v) in &b.pending_dead_store {
                if !pre_branch_keys.contains(k) {
                    self.pending_dead_store.insert(*k, *v);
                }
            }
        }
    }
}

/// Validate move safety for every function body. Emits diagnostics
/// into `sink`. Returns an [`OwnershipSidecar`] that codegen consults
/// to decide where to emit `ryo_str_free` calls. The TIR itself is
/// never mutated.
pub fn check(tirs: &[Tir], pool: &InternPool, sink: &mut DiagSink) -> OwnershipSidecar {
    let mut sidecar = OwnershipSidecar::default();
    for tir in tirs {
        let mut func_sidecar = FunctionSidecar::default();
        analyze_function(tir, pool, sink, &mut func_sidecar);
        sidecar.functions.insert(tir.name, func_sidecar);
    }
    sidecar
}

/// Collect the init/value `TirRef` of every `VarDecl`/`Assign` anywhere
/// in `stmts`, recursing into nested control flow. These are the
/// "named" producers whose Free is owned by the last-use / dead-store /
/// `free_on_reassign` / loop-exit pass; the anon-temp pass skips them to
/// avoid a double-free. Stateless replacement (I-060) for the old
/// sticky side-set that formerly lived on `Ownership` and accumulated
/// named-init TirRefs during the forward walk. Unlike a
/// `current_owner.values()` derivation this is merge-immune: a temp
/// reassigned inside a loop body is statically the loop-body `Assign`'s
/// value regardless of any loop-merge state.
fn collect_named_inits(tir: &Tir, stmts: &[TirRef]) -> HashSet<TirRef> {
    let mut set = HashSet::new();
    for &s in stmts {
        collect_named_inits_rec(tir, s, &mut set);
    }
    set
}

/// Recursive core of [`collect_named_inits`]. Dispatches on the
/// statement tag, records each `VarDecl`/`Assign` producer, and
/// recurses into `IfStmt` arms / `WhileLoop` body / `ForRange` body so
/// named initializers buried in nested control flow are still
/// classified as named inits.
fn collect_named_inits_rec(tir: &Tir, r: TirRef, set: &mut HashSet<TirRef>) {
    match tir.inst(r).tag {
        TirTag::VarDecl => {
            set.insert(tir.var_decl_view(r).initializer);
        }
        TirTag::Assign => {
            set.insert(tir.assign_view(r).value);
        }
        TirTag::IfStmt => {
            let v = tir.if_stmt_view(r);
            for &s in &v.then_stmts {
                collect_named_inits_rec(tir, s, set);
            }
            for arm in &v.elif_branches {
                for &s in &arm.body {
                    collect_named_inits_rec(tir, s, set);
                }
            }
            if let Some(else_stmts) = v.else_stmts.as_deref() {
                for &s in else_stmts {
                    collect_named_inits_rec(tir, s, set);
                }
            }
        }
        TirTag::WhileLoop => {
            for &s in tir.while_loop_view(r).body.iter() {
                collect_named_inits_rec(tir, s, set);
            }
        }
        TirTag::ForRange => {
            for &s in tir.for_range_view(r).body.iter() {
                collect_named_inits_rec(tir, s, set);
            }
        }
        _ => {}
    }
}

fn analyze_function(
    tir: &Tir,
    pool: &InternPool,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
) {
    let mut own = Ownership::default();

    // Initialise per-parameter state. Move-typed params start at
    // `Valid` (the callee owns them); borrowed and inout params start
    // at `Borrowed` (the callee does not own the buffer — inout adds
    // mutability, not ownership). Copy-typed params skip the lattice
    // entirely.
    for param in &tir.params {
        if !needs_tracking(param.ty, pool) {
            continue;
        }
        let owner = Owner::Param(param.name);
        let state = match param.mode {
            ParamMode::Move => OwnerState::Valid,
            ParamMode::Borrow | ParamMode::Inout => OwnerState::Borrowed,
        };
        own.states.insert(owner, state);
        own.current_owner.insert(param.name, owner);
        if param.mode == ParamMode::Inout {
            own.inout_str_params.insert(param.name);
        }
    }

    for stmt in tir.body_stmts() {
        analyze_stmt(tir, pool, &mut own, sink, sidecar, stmt);
    }

    // Forward last-use scan: for every owner still `Valid` at function exit
    // (i.e., not moved out via return / move-typed call argument /
    // reassign), find its last reading instruction and schedule a Free
    // anchored after it. The forward walk uses overwriting `insert` so the
    // *latest* forward-order read across the whole function wins — last
    // source-order read in the outer-statement-loop and inner-operand-walk
    // composition. Reads of a binding in the body always alias *some*
    // owner that the forward walk classified; the per-read
    // `owner_at_read` snapshot resolves each read to the owner that was
    // live at that point, regardless of any later rebinds. For any owner
    // whose state is `Moved` at function exit (e.g. the pre-reassign
    // owner of a rebound binding), the final-state filter below skips it.
    let body_stmts = tir.body_stmts();
    let mut last_use: HashMap<TirRef, TirRef> = HashMap::new();
    for &stmt in &body_stmts {
        collect_last_uses(tir, pool, &own, stmt, &mut last_use);
    }
    // Owners already covered by `free_on_reassign` must not be
    // scheduled again via the last-use pass — that would double-free
    // the same allocation. (Pre-rebind owners are now reachable from
    // last_use after the `owner_at_read` snapshot fix; without this
    // guard a pre-rebind owner would receive both a reassign-Free and
    // a last-use-Free.)
    //
    // EXCEPTION: a reassign target that is STILL its binding's current
    // owner at function exit needs the last-use Free after all. That
    // happens when the reseat was branch-divergent: the merge keeps the
    // pre-branch owner (a reseat inside one arm does not survive the
    // join), so on the not-taken path the binding still owns the
    // pre-reassign allocation. Codegen emits the Free from the binding's
    // current `StrLocals` (I-112), which is the path-correct buffer.
    let reassign_targets: HashSet<Owner> = sidecar
        .free_on_reassign
        .values()
        .map(|t| Owner::Inst(*t))
        .collect();
    let live_binding_owners: HashSet<Owner> = own.current_owner.values().copied().collect();
    // Owners that escape through an `inout str` param's write-back
    // pointer at function exit: whatever value is CURRENTLY bound to each
    // inout param name leaves the function alive, so neither the last-use
    // pass nor the dead-store drain may free it (or warn about it).
    let inout_escape_owners: HashSet<Owner> = own
        .inout_str_params
        .iter()
        .filter_map(|n| own.current_owner.get(n).copied())
        .collect();
    for (owner, state) in &own.states {
        if !matches!(state, OwnerState::Valid) {
            continue;
        }
        match owner {
            Owner::Inst(r) => {
                let stale_reassign_target =
                    reassign_targets.contains(owner) && !live_binding_owners.contains(owner);
                if stale_reassign_target || inout_escape_owners.contains(owner) {
                    continue;
                }
                if let Some(&after) = last_use.get(r) {
                    sidecar.free_schedule.push(FreePoint {
                        after,
                        target: *r,
                        span: tir.span(*r),
                        branch: None,
                    });
                }
            }
            Owner::Param(name) => {
                // An `inout str` param's value escapes through the
                // write-back pointer — never freed by the callee, even
                // if a branch merge left its owner stamped Valid.
                if own.inout_str_params.contains(name) {
                    continue;
                }
                if let Some(&after) = body_stmts.last() {
                    let idx = tir
                        .params
                        .iter()
                        .position(|p| p.name == *name)
                        .expect("param exists");
                    sidecar.free_schedule.push(FreePoint {
                        after,
                        target: owner.tirref(tir),
                        span: tir.params[idx].span,
                        branch: None,
                    });
                }
            }
        }
    }

    // Anonymous-temporary frees: temp_owners still Valid at function
    // exit need their own Free anchored after their single consumer —
    // UNLESS the temp was ever a named binding's initializer/value, in
    // which case its Free is owned by the last-use / dead-store /
    // free_on_reassign / loop-exit pass and must be skipped here to
    // avoid a double-free.
    //
    // This "was a named init" predicate is a TIR-shape fact, not a
    // lattice-state fact, so it is derived statically via
    // `collect_named_inits` (I-060). The old implementation carried a
    // walk; the static set is merge-immune where a
    // `current_owner.values()` derivation would not be (it drops a
    // loop-rebound temp at the loop merge but the temp is still freed
    // by the loop-exit pass, so the dynamic classifier would schedule
    // a spurious second Free). The static set is provably equivalent
    // to the old sticky set across all cases.
    let named_inits: HashSet<TirRef> = collect_named_inits(tir, &body_stmts);
    let mut consumer_of: HashMap<TirRef, TirRef> = HashMap::new();
    for &stmt in &body_stmts {
        find_consumers(tir, stmt, &mut consumer_of);
    }
    for &temp in &own.temp_owners {
        // Temps are always `Inst` owners, never `Param`.
        let Some(t) = temp.inst_tirref() else {
            continue;
        };
        if named_inits.contains(&t) {
            // Freed by the last-use / dead-store / free_on_reassign /
            // loop-exit pass — skip to avoid a double-free.
            continue;
        }
        if !matches!(own.states.get(&temp), Some(OwnerState::Valid)) {
            // Already moved (flowed into a `move` arg, return, etc.).
            continue;
        }
        if let Some(&consumer) = consumer_of.get(&t) {
            sidecar.free_schedule.push(FreePoint {
                after: consumer,
                target: t,
                span: tir.span(t),
                branch: None,
            });
        }
        // No consumer = unreachable from any body statement; can't
        // happen in well-formed TIR. Don't emit (no consumer means
        // codegen's inst_values won't have ptr/cap either).
    }

    // Dead-store survivors: emit W0001 and schedule a Free anchored
    // after the declaring instruction. Skip owners already covered by
    // `free_on_reassign` (Task 6) to avoid double-freeing the same
    // allocation. Today no `free_on_reassign` entries exist; this
    // guard activates with Task 6. (`reassign_targets` was computed
    // above for the last-use pass.)
    for (owner, (name, span, decl_inst)) in &own.pending_dead_store {
        // Bound to an `inout str` param: the value escapes through the
        // write-back — it IS used, just not by any TIR instruction the
        // pass can see. Checked by NAME (not by current owner): a rebind
        // inside a branch is discarded by the merge, leaving the entry
        // keyed by a branch-local owner the exit-time escape set can't
        // see. No W0001, no Free.
        if own.inout_str_params.contains(name) {
            continue;
        }
        if inout_escape_owners.contains(owner) {
            continue;
        }
        sink.emit(Diag::warning(
            *span,
            DiagCode::DeadStore,
            format!("value `{}` is declared but never used", pool.str(*name)),
        ));
        if reassign_targets.contains(owner) {
            // Task 6's reassignment-Free already covers this owner;
            // emitting another dead-store Free would double-free.
            continue;
        }
        sidecar.free_schedule.push(FreePoint {
            after: *decl_inst,
            target: owner.inst_tirref().unwrap(),
            span: *span,
            branch: None,
        });
    }

    // Loop-exit Frees run LAST so they can inspect the now-complete
    // `free_schedule` and only add jump-anchored Frees for inside-loop
    // owners that no earlier pass already covered. See I-058.
    schedule_loop_exit_frees_in(tir, &own, sidecar, &body_stmts, None);
}

fn analyze_stmt(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    stmt: TirRef,
) {
    let inst = *tir.inst(stmt);
    match inst.tag {
        TirTag::VarDecl => analyze_var_decl(tir, pool, own, sink, sidecar, stmt),
        TirTag::Assign => analyze_assign(tir, pool, own, sink, sidecar, stmt),
        TirTag::Return => analyze_return(tir, pool, own, sink, sidecar, stmt),
        TirTag::ReturnVoid => {}
        TirTag::IfStmt => analyze_if_stmt(tir, pool, own, sink, sidecar, stmt),
        TirTag::WhileLoop => analyze_while_loop(tir, pool, own, sink, sidecar, stmt),
        TirTag::ForRange => analyze_for_range(tir, pool, own, sink, sidecar, stmt),
        TirTag::Break | TirTag::Continue => {
            // 8.1c attaches Free metadata here; 8.1b is a no-op.
        }
        TirTag::CompoundAssign => {
            // Sema rejects compound-assign on Move-typed values today
            // (str doesn't support `+=`/`-=`/etc.). Enforce the invariant
            // here so a future sema relaxation that reaches the ownership
            // pass without ownership-aware handling trips a debug build
            // instead of silently falling through to no analysis.
            // See ISSUES.md I-046.
            let view = tir.compound_assign_view(stmt);
            debug_assert!(
                !needs_tracking(tir.inst(view.value).ty, pool),
                "compound-assign on Move-typed value reached ownership pass; \
                 sema should have rejected — see ISSUES.md I-046",
            );
        }
        TirTag::ExprStmt => {
            if let TirData::UnOp(o) = inst.data {
                visit_expr(tir, pool, own, sink, sidecar, o);
            }
        }
        _ => {
            visit_expr(tir, pool, own, sink, sidecar, stmt);
        }
    }
}

/// Move-typed `VarDecl` is a consumer: the new binding takes
/// ownership of the initializer's underlying value. If the
/// initializer aliases a borrowed parameter, this is the E0021
/// "move out of borrowed parameter" site; if the underlying owner is
/// already `Moved`, this is the E0020 "use after move" site.
fn analyze_var_decl(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.var_decl_view(r);
    let init = view.initializer;
    let init_ty = tir.inst(init).ty;
    visit_expr(tir, pool, own, sink, sidecar, init);
    if needs_tracking(init_ty, pool) {
        let span = tir.span(r);
        let consumed_name = consumed_binding_name(tir, init);
        consume_for_assignment(tir, pool, own, sink, init, span, consumed_name);
        rebind_to_init(own, view.name, init);
        // Register the new binding as pending dead-store. The walk
        // clears this entry on any later read or consumption; a
        // surviving entry at function end fires W0001. Keyed by
        // `init`, the same TirRef `rebind_to_init` stamped Valid.
        register_pending_dead_store(own, init, view.name, span, r);
    } else {
        own.current_owner.insert(view.name, Owner::Inst(init));
    }
}

/// Reassignment to a Move-typed binding. Same consumption rules as
/// `VarDecl`; the existing binding name is reseated to whichever
/// SSA value owns the new underlying allocation.
fn analyze_assign(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.assign_view(r);
    let value_ty = tir.inst(view.value).ty;
    visit_expr(tir, pool, own, sink, sidecar, view.value);
    if needs_tracking(value_ty, pool) {
        // Capture the old owner before consume_for_assignment / rebind
        // overwrite current_owner[name]. Only emit the Free entry if the
        // old owner is Valid — Borrowed/Moved/missing means there is no
        // live allocation to release. EXCEPTION: an `inout str` param's
        // current owner is Borrowed, yet the callee must drop the old
        // pointee when reassigning the param (the write-back ABI hands
        // the caller whatever triple the param holds at exit — like
        // `*x = new` dropping the old value in Rust). Codegen (Task 8)
        // consults this map when lowering Assign.
        if let Some(&old_owner) = own.current_owner.get(&view.name) {
            let old_droppable = match own.states.get(&old_owner) {
                Some(OwnerState::Valid) => true,
                Some(OwnerState::Borrowed) => own.inout_str_params.contains(&view.name),
                _ => false,
            };
            if old_droppable {
                // `tirref` (not `inst_tirref`): an inout param's old owner
                // is a `Param`, resolved here to its virtual ref — codegen
                // caches that ref's repr at the prologue.
                sidecar.free_on_reassign.insert(r, old_owner.tirref(tir));
                // Reassignment runs the old value's destructor (the
                // free_on_reassign Free above) — that's an observable use,
                // so the prior VarDecl/Assign isn't a dead store. Drop the
                // pending_dead_store entry so W0001 doesn't fire and the
                // drain block doesn't try to schedule a redundant Free.
                own.pending_dead_store.remove(&old_owner);
            }
        }
        let span = tir.span(r);
        let consumed_name = consumed_binding_name(tir, view.value);
        consume_for_assignment(tir, pool, own, sink, view.value, span, consumed_name);
        rebind_to_init(own, view.name, view.value);
        register_pending_dead_store(own, view.value, view.name, span, r);
    }
}

/// Move-typed `Return` is a consumer: the returned value flows out of
/// the function and the caller takes ownership. Borrowed parameters
/// cannot be returned (E0022). If the underlying owner is already
/// `Moved`, this is the E0020 "use after move" site.
fn analyze_return(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    let operand = match inst.data {
        TirData::UnOp(o) => o,
        _ => unreachable!("Return must carry TirData::UnOp"),
    };
    let ty = tir.inst(operand).ty;
    visit_expr(tir, pool, own, sink, sidecar, operand);
    if !needs_tracking(ty, pool) {
        return;
    }
    let span = tir.span(r);
    let consumed_name = consumed_binding_name(tir, operand);
    consume_underlying(
        tir,
        pool,
        own,
        sink,
        operand,
        span,
        consumed_name,
        BorrowedAction::ReturnBorrowed,
    );
}

/// Reseat `name` to point at `init` as its current owner. After a
/// consume, the source binding's underlying value is `Moved`; the
/// new binding takes a fresh, independent slot in the lattice so
/// subsequent reads of `name` resolve to a `Valid` owner instead of
/// tripping over the just-moved underlying. We do this by severing
/// `init`'s `origin` link (if any) and stamping it `Valid`.
fn rebind_to_init(own: &mut Ownership, name: StringId, init: TirRef) {
    own.origin.insert(init, None);
    own.states.insert(Owner::Inst(init), OwnerState::Valid);
    own.current_owner.insert(name, Owner::Inst(init));
}

/// Register a Move-typed binding into `pending_dead_store`. The owner
/// key (`init`/`value` TirRef) is whatever currently owns the
/// allocation; `decl_inst` is the `VarDecl`/`Assign` instruction's own
/// TirRef and serves as the Free anchor if the binding turns out to be
/// a dead store. Single source of truth for `analyze_var_decl` and
/// `analyze_assign` — closes ISSUES.md I-055.
fn register_pending_dead_store(
    own: &mut Ownership,
    owner: TirRef,
    name: StringId,
    span: Span,
    decl_inst: TirRef,
) {
    own.pending_dead_store
        .insert(Owner::Inst(owner), (name, span, decl_inst));
}

/// Walk back from `init` to whichever SSA value currently owns the
/// underlying allocation. `visit_expr` is responsible for populating
/// `origin` for `Var` reads; for fresh producers (`StrConst`,
/// `StrConcat`, `Call`) `init` is itself the owner.
fn underlying_owner(own: &Ownership, init: TirRef) -> Owner {
    match own.origin.get(&init).copied() {
        Some(Some(owner)) => owner,
        _ => Owner::Inst(init),
    }
}

/// Aliasing identity of an `inout` (or Copy borrow) call argument for the
/// Rule 7 overlap check (M8.3). Tracked (Move-typed) args resolve to the
/// usual underlying owner. Copy scalars never enter the lattice, so their
/// stable identity is the binding's `current_owner` slot (seeded at the
/// VarDecl) — two `&c` reads of one `mut c` must collide even though each
/// read is its own SSA value. An unregistered name (a Copy-typed
/// parameter, which the param loop skips) falls back to
/// `Owner::Param(name)` — the correct per-binding key for exactly that
/// case.
fn inout_owner(own: &Ownership, tir: &Tir, arg: TirRef) -> Owner {
    match tir.inst(arg).data {
        TirData::Var(name) => own
            .current_owner
            .get(&name)
            .copied()
            .unwrap_or(Owner::Param(name)),
        _ => underlying_owner(own, arg),
    }
}

/// True if `owner` is the value currently bound to an `inout str`
/// parameter name. That value ESCAPES through the write-back pointer at
/// function exit, so it can be reassigned (dropping the old pointee) but
/// never moved out of the function — not even after a reassign replaced
/// the original (Borrowed) param owner with a fresh Valid one.
fn inout_escape_owner(own: &Ownership, owner: Owner) -> bool {
    own.inout_str_params
        .iter()
        .any(|n| own.current_owner.get(n) == Some(&owner))
}

/// Use-site use-after-move authority (I-050). Resolve the operand's
/// underlying owner and emit E0020 if it is `Moved`. Called from every
/// use site: consume sites, borrow-arg paths, and operand-read sites.
fn check_use_moved(
    tir: &Tir,
    pool: &InternPool,
    own: &Ownership,
    sink: &mut DiagSink,
    operand: TirRef,
    span: Span,
) {
    let owner = underlying_owner(own, operand);
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

/// Which diagnostic the shared `consume_underlying` helper should
/// emit on the `Borrowed` arm. `consume_for_assignment` and
/// `analyze_return` run identical state transitions otherwise — the
/// only thing that diverges is the error code / wording / help when
/// the underlying owner is borrowed.
enum BorrowedAction {
    /// E0021 — `consume_for_assignment` site (VarDecl / Assign /
    /// move-typed Call argument).
    MoveOutOfParam,
    /// E0022 — `analyze_return` site.
    ReturnBorrowed,
}

/// Apply the consumption transition for a Move-typed initializer.
/// Caller must have already populated origin/state for `init` via
/// `visit_expr`.
fn consume_for_assignment(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    init: TirRef,
    span: Span,
    name: Option<StringId>,
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
    );
}

/// Shared transition for every consume site (assignment, return,
/// move-typed Call argument). Walks back to the underlying owner,
/// reads its state, and either:
///
/// * `Valid` → stamp `Moved { moved_at: span }` and clear any pending
///   dead-store entry,
/// * `Borrowed` → emit E0021 or E0022 per `on_borrowed`,
/// * `Moved` → now a use-after-move check authority (I-050).
/// * `NotTracked` → no-op.
#[allow(clippy::too_many_arguments)]
fn consume_underlying(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    operand: TirRef,
    span: Span,
    name: Option<StringId>,
    on_borrowed: BorrowedAction,
) {
    let underlying = underlying_owner(own, operand);
    let mut state = own
        .states
        .get(&underlying)
        .cloned()
        .unwrap_or(OwnerState::NotTracked);
    // A Valid value currently bound to an `inout str` param still escapes
    // through the write-back pointer — moving it out would leave the slot
    // holding a stale triple. Treat it as borrowed at consume sites.
    if matches!(state, OwnerState::Valid) && inout_escape_owner(own, underlying) {
        state = OwnerState::Borrowed;
    }
    match state {
        OwnerState::Valid => {
            own.pending_dead_store.remove(&underlying);
            own.states
                .insert(underlying, OwnerState::Moved { moved_at: span });
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

/// Render a binding name for inclusion in a diagnostic message.
/// Returns `` `name` `` for known bindings and `value` for anonymous
/// temporaries (concat results, fresh allocations, etc.).
fn format_binding(name: Option<StringId>, pool: &InternPool) -> String {
    match name {
        Some(n) => format!("`{}`", pool.str(n)),
        None => "value".to_string(),
    }
}

/// If `r` is a direct `Var` read, return the binding name it aliases.
/// Used at consume sites to thread the source binding name into
/// E0020/E0021/E0022 messages. Returns `None` for fresh producers
/// (StrConst, StrConcat, Call), where there's no source binding.
fn consumed_binding_name(tir: &Tir, r: TirRef) -> Option<StringId> {
    match tir.inst(r).data {
        TirData::Var(n) => Some(n),
        _ => None,
    }
}

fn owner_name_for_diag(owner: Owner, tir: &Tir, pool: &InternPool) -> String {
    match owner {
        Owner::Param(name) => format!("`{}`", pool.str(name)),
        Owner::Inst(r) => format_binding(consumed_binding_name(tir, r), pool),
    }
}

/// CFG join for `if` / `elif` / `else`. The naïve forward walk
/// would let a move inside a then-branch persist past the merge
/// regardless of whether else also moved — wrong for the spec's
/// guarantee that conditionally-moved values are not safe to use
/// after the join. Snapshot the lattice before each branch, walk
/// each branch from the snapshot independently, then merge. If any
/// branch left a value `Moved`, the post-`if` state is `Moved`;
/// when no `else` is present, the implicit fall-through branch is
/// the pre-`if` snapshot itself, so an unconsumed pre-`if` value
/// stays usable after the join.
fn analyze_if_stmt(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.if_stmt_view(r);
    visit_expr(tir, pool, own, sink, sidecar, view.cond);

    // Allocate fresh BranchIds for this if's arms. Codegen consults
    // `sidecar.if_branches` when lowering the if so each arm pushes
    // the right BranchId onto its `branch_stack`.
    let then_branch = BranchId(own.next_branch_id);
    own.next_branch_id += 1;
    let mut elif_branches: Vec<BranchId> = Vec::with_capacity(view.elif_branches.len());
    for _ in &view.elif_branches {
        elif_branches.push(BranchId(own.next_branch_id));
        own.next_branch_id += 1;
    }
    let else_branch = if view.else_stmts.is_some() {
        let id = BranchId(own.next_branch_id);
        own.next_branch_id += 1;
        Some(id)
    } else {
        None
    };
    sidecar.if_branches.insert(
        r,
        IfBranchIds {
            then_branch,
            elif_branches: elif_branches.clone(),
            else_branch,
        },
    );

    let snap_states = own.states.clone();
    let snap_current_owner = own.current_owner.clone();
    let snap_pending_dead_store = own.pending_dead_store.clone();

    for stmt in &view.then_stmts {
        analyze_stmt(tir, pool, own, sink, sidecar, *stmt);
    }
    let then_state = own.clone();

    let mut branch_results = vec![then_state];
    for elif in &view.elif_branches {
        own.states = snap_states.clone();
        own.current_owner = snap_current_owner.clone();
        own.pending_dead_store = snap_pending_dead_store.clone();

        visit_expr(tir, pool, own, sink, sidecar, elif.cond);
        for stmt in &elif.body {
            analyze_stmt(tir, pool, own, sink, sidecar, *stmt);
        }
        branch_results.push(own.clone());
    }

    if let Some(else_stmts) = &view.else_stmts {
        own.states = snap_states.clone();
        own.current_owner = snap_current_owner.clone();
        own.pending_dead_store = snap_pending_dead_store.clone();

        for stmt in else_stmts {
            analyze_stmt(tir, pool, own, sink, sidecar, *stmt);
        }
        branch_results.push(own.clone());
    } else {
        let mut else_snap = own.clone();
        else_snap.states = snap_states.clone();
        else_snap.current_owner = snap_current_owner.clone();
        else_snap.pending_dead_store = snap_pending_dead_store.clone();
        branch_results.push(else_snap);
    }

    // Schedule branch-gated Frees for owners that diverge across
    // arms (Valid in some, Moved in others). For each Valid arm,
    // anchor a Free after that arm's last body statement and gate
    // it on the arm's BranchId. The post-merge state below stamps
    // such owners as `Moved` (any-Moved-wins), so the function-exit
    // last-use pass will skip them — without these conditional
    // Frees the Valid-arm allocation would leak.
    //
    // The `else_stmts.is_none()` path pushes the pre-if snapshot
    // into `branch_results` for the implicit fall-through. We
    // deliberately don't schedule conditional Frees against that
    // pseudo-arm: the post-if last-use pass already covers any
    // owner whose state remains `Valid` at function exit.
    struct ArmInfo<'a> {
        branch_id: BranchId,
        last_stmt: Option<TirRef>,
        state: &'a Ownership,
    }

    let mut arms: Vec<ArmInfo> = Vec::with_capacity(branch_results.len());
    arms.push(ArmInfo {
        branch_id: then_branch,
        last_stmt: view.then_stmts.last().copied(),
        state: &branch_results[0],
    });
    for (i, elif) in view.elif_branches.iter().enumerate() {
        arms.push(ArmInfo {
            branch_id: elif_branches[i],
            last_stmt: elif.body.last().copied(),
            state: &branch_results[1 + i],
        });
    }
    if let Some(else_stmts) = &view.else_stmts {
        arms.push(ArmInfo {
            branch_id: else_branch.expect("else_branch must be Some when else_stmts is Some"),
            last_stmt: else_stmts.last().copied(),
            state: branch_results
                .last()
                .expect("else snapshot pushed by analyze_if_stmt"),
        });
    }

    let all_keys: HashSet<Owner> = arms
        .iter()
        .flat_map(|a| a.state.states.keys().copied())
        .collect();
    for owner in all_keys {
        let is_tracked = match owner {
            Owner::Inst(_) => true,
            Owner::Param(name) => {
                let idx = tir
                    .params
                    .iter()
                    .position(|p| p.name == name)
                    .expect("param exists");
                needs_tracking(tir.params[idx].ty, pool)
            }
        };
        if !is_tracked {
            continue;
        }
        let any_moved = arms
            .iter()
            .any(|a| matches!(a.state.states.get(&owner), Some(OwnerState::Moved { .. })));
        if !any_moved {
            continue;
        }
        for arm in &arms {
            if matches!(arm.state.states.get(&owner), Some(OwnerState::Valid))
                && let Some(after) = arm.last_stmt
            {
                let span = match owner {
                    Owner::Inst(r) => tir.span(r),
                    Owner::Param(name) => {
                        let idx = tir
                            .params
                            .iter()
                            .position(|p| p.name == name)
                            .expect("param exists");
                        tir.params[idx].span
                    }
                };
                sidecar.free_schedule.push(FreePoint {
                    after,
                    target: owner.tirref(tir),
                    span,
                    branch: Some(arm.branch_id),
                });
            }
            // Empty arm or sema-rejected case: skip. The M8.1
            // grammar forbids empty arms.
        }
    }

    // Final restore: restore only the non-monotone fields.
    own.states = snap_states;
    own.current_owner = snap_current_owner;
    own.pending_dead_store = snap_pending_dead_store;
    let refs: Vec<&Ownership> = branch_results.iter().collect();
    own.merge_branches(&refs);
}

/// Shared loop-body fixed-point (I-051). Caller has already visited the
/// prelude (cond / start+end). Walks the body once into a scratch sink,
/// compares entry vs post-body Moved-ness, and either replays (converged)
/// or re-walks from the merged state against the real sink.
fn analyze_loop_body(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    body: &[TirRef],
) {
    // I-061: snapshot ONLY the non-monotone fields (see Step 2).
    let snap_states = own.states.clone();
    let snap_current_owner = own.current_owner.clone();
    let snap_pending_dead_store = own.pending_dead_store.clone();

    let mut scratch = DiagSink::new();
    for stmt in body {
        analyze_stmt(tir, pool, own, &mut scratch, sidecar, *stmt);
    }
    let after = (
        own.states.clone(),
        own.current_owner.clone(),
        own.pending_dead_store.clone(),
    );

    if states_differ_snapshot(&snap_states, &after.0) {
        // merge the non-monotone fields and re-walk against the real sink
        merge_non_monotone(
            own,
            &snap_states,
            &after.0,
            &snap_current_owner,
            &after.1,
            &snap_pending_dead_store,
            &after.2,
        );
        for stmt in body {
            analyze_stmt(tir, pool, own, sink, sidecar, *stmt);
        }
        // final merge with entry
        let own_states_clone = own.states.clone();
        let own_current_owner_clone = own.current_owner.clone();
        let own_pending_dead_store_clone = own.pending_dead_store.clone();
        merge_non_monotone(
            own,
            &snap_states,
            &own_states_clone,
            &snap_current_owner,
            &own_current_owner_clone,
            &snap_pending_dead_store,
            &own_pending_dead_store_clone,
        );
    } else {
        for d in scratch.into_diags() {
            sink.emit(d);
        }
        // restore union-only fields are already live; reset non-monotone to entry-merged
        merge_non_monotone(
            own,
            &snap_states,
            &after.0,
            &snap_current_owner,
            &after.1,
            &snap_pending_dead_store,
            &after.2,
        );
    }
}

/// 2-pass approximation of a fixed-point ownership analysis for
/// `while`. Walks the body once from the entry state into a scratch
/// sink; if no tracked TirRef changed Moved-ness across the back-edge,
/// the body is loop-invariant for ownership purposes and the scratch
/// diagnostics are flushed. Otherwise we re-walk from the merged
/// (entry ⊔ post-body) state and emit diagnostics on the second pass
/// — a binding moved inside the body without rebinding before the
/// back-edge surfaces as E0020 on iteration two.
///
/// Why two passes suffice for the M8.1 pattern set (instead of a
/// general fixed-point loop): the merge is monotonic over the
/// Moved-ness sub-lattice (`Valid → Moved` is the only transition,
/// and merge takes "any branch Moved → Moved"), and there are no
/// iteration-count-dependent conditionals — so a TirRef that flips
/// from `Valid` to `Moved` in pass one stays `Moved` after the
/// (entry ⊔ post-body) merge and a third walk would observe nothing
/// new. Original phrasing for traceability:
/// "Converges in at most 2 iterations for the M8.1 pattern set".
///
/// **Maintainer note.** If a future change introduces non-monotonic
/// merges (e.g., `Moved → Valid` re-entry as a real lattice transition
/// rather than via rebinding) or iteration-count-dependent control
/// flow inside the body, revert this to a true fixed-point loop
/// (iterate until `states_differ_snapshot` returns false).
fn analyze_while_loop(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.while_loop_view(r);
    visit_expr(tir, pool, own, sink, sidecar, view.cond);
    analyze_loop_body(tir, pool, own, sink, sidecar, &view.body);
}

/// `for i in range(start, end)` loop var is `int` (Copy), so the
/// induction variable never enters the lattice. The body runs the
/// same fixed-point as `while`.
fn analyze_for_range(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.for_range_view(r);
    // Start/end are visited unconditionally — they're plain `int`
    // exprs, so they don't move anything, but they may contain nested
    // reads we want to record.
    visit_expr(tir, pool, own, sink, sidecar, view.start);
    visit_expr(tir, pool, own, sink, sidecar, view.end);
    analyze_loop_body(tir, pool, own, sink, sidecar, &view.body);
}

/// Compare two lattices on the Moved-ness of every tracked `TirRef`.
/// Per the M8.1b plan, the only transition that forces a re-walk is a
/// `Valid`/`Borrowed` → `Moved` flip across the back-edge — non-Moved
/// state changes (e.g. fresh definitions added inside the body) are
/// loop-invariant for the use-after-move check.
fn states_differ_snapshot(a: &HashMap<Owner, OwnerState>, b: &HashMap<Owner, OwnerState>) -> bool {
    // Diverge iff some Owner is Moved in one and not the other.
    // Walk each side once and check the other's matching entry —
    // missing entries default to NotTracked, so a Moved with no
    // counterpart counts as a divergence on its own.
    for (k, av) in a {
        let a_moved = matches!(av, OwnerState::Moved { .. });
        let b_moved = matches!(b.get(k), Some(OwnerState::Moved { .. }));
        if a_moved != b_moved {
            return true;
        }
    }
    for (k, bv) in b {
        if !matches!(bv, OwnerState::Moved { .. }) {
            continue;
        }
        // Only need to catch keys exclusive to b that are Moved —
        // keys present in a were handled in the first loop.
        if !a.contains_key(k) {
            return true;
        }
    }
    false
}

/// Merge only the non-monotone fields of two states (represented by their snapshots)
/// into `own`'s corresponding fields, leaving the monotone fields intact.
fn merge_non_monotone(
    own: &mut Ownership,
    snap_states: &HashMap<Owner, OwnerState>,
    after_states: &HashMap<Owner, OwnerState>,
    snap_current_owner: &HashMap<StringId, Owner>,
    after_current_owner: &HashMap<StringId, Owner>,
    snap_pending_dead_store: &HashMap<Owner, (StringId, Span, TirRef)>,
    after_pending_dead_store: &HashMap<Owner, (StringId, Span, TirRef)>,
) {
    // 1. Merge states: any branch Moved -> Moved; otherwise first observed (snap_states) wins.
    let mut merged_states = snap_states.clone();
    for (r, s) in after_states {
        merged_states
            .entry(*r)
            .and_modify(|cur| {
                if !matches!(cur, OwnerState::Moved { .. }) && matches!(s, OwnerState::Moved { .. })
                {
                    *cur = s.clone();
                }
            })
            .or_insert_with(|| s.clone());
    }

    // Binding-aware override
    for (&name, &owner_pre) in snap_current_owner {
        let owner_snap = owner_pre;
        let owner_after = after_current_owner.get(&name).copied().unwrap_or(owner_pre);

        let state_snap = snap_states
            .get(&owner_snap)
            .cloned()
            .unwrap_or(OwnerState::NotTracked);
        let state_after = after_states
            .get(&owner_after)
            .cloned()
            .unwrap_or(OwnerState::NotTracked);

        let mut merged: Option<OwnerState> = None;
        for state_b in &[state_snap, state_after] {
            let take = match (&merged, state_b) {
                (None, _) => true,
                (Some(OwnerState::Moved { .. }), _) => false,
                (_, OwnerState::Moved { .. }) => true,
                _ => false,
            };
            if take {
                merged = Some(state_b.clone());
            }
        }
        if let Some(state) = merged {
            merged_states.insert(owner_pre, state);
        }
    }

    // 2. Merge current_owner: first-wins via or_insert.
    let mut merged_current_owner = snap_current_owner.clone();
    for (&name, &owner) in after_current_owner {
        merged_current_owner.entry(name).or_insert(owner);
    }

    // 3. Merge pending_dead_store: pre-branch keys intersect; branch-local keys union.
    let mut merged_pending_dead_store = snap_pending_dead_store.clone();
    merged_pending_dead_store.retain(|k, _| after_pending_dead_store.contains_key(k));
    for (k, v) in after_pending_dead_store {
        if !snap_pending_dead_store.contains_key(k) {
            merged_pending_dead_store.insert(*k, *v);
        }
    }

    own.states = merged_states;
    own.current_owner = merged_current_owner;
    own.pending_dead_store = merged_pending_dead_store;
}

/// For every Move-typed owner that has at least one `Var` read,
/// record the *last* read in forward source order. The map is
/// populated by overwriting (not `or_insert`), so the latest read
/// wins — semantically equivalent to the previous reverse-walk +
/// `or_insert` approach for a tree-shaped IR. Recurses through
/// `walk_operands` so reads buried inside calls, loops, and if-arms
/// are still seen.
fn collect_last_uses(
    tir: &Tir,
    pool: &InternPool,
    own: &Ownership,
    r: TirRef,
    last_use: &mut HashMap<TirRef, TirRef>,
) {
    let inst = *tir.inst(r);
    // Record this instruction's own `Var` read, if any. Resolve via
    // the per-read `owner_at_read` snapshot taken during the forward
    // walk — `current_owner`'s end-of-function state would misroute
    // reads that precede a `mut` reassignment to the post-rebind
    // owner (wrong target, double-free hazard once heap-allocated
    // strings reach this pattern). The snapshot anchors each read to
    // the owner that was live *at that read*, regardless of any
    // subsequent rebinds.
    if let TirTag::Var = inst.tag
        && let TirData::Var(_) = inst.data
        && needs_tracking(inst.ty, pool)
        && let Some(&owner) = own.owner_at_read.get(&r)
    {
        // Overwriting insert: latest forward-order read wins =
        // last source-order read.
        if let Some(r_owner) = owner.inst_tirref() {
            last_use.insert(r_owner, r);
        }
    }
    walk_operands(tir, r, &mut |_parent, operand, _kind| {
        collect_last_uses(tir, pool, own, operand, last_use);
    });
}

/// Build a `child_TirRef → parent_TirRef` map. Each temporary owner
/// has at most one direct parent in the TIR (TIR is tree-shaped per
/// function), so `or_insert` correctly preserves the first parent
/// observed. Used by the anonymous-temporary-free pass to anchor
/// each temp's Free after its single consumer.
///
/// Only `Operand`-kinded edges contribute to `consumer_of`: a body
/// statement nested inside an `if`/`while`/`for` is not a consumer
/// of the surrounding control-flow instruction, so its Free must
/// not be anchored on the loop/branch header. Recursion still
/// descends through `BodyStmt` edges to reach operands buried
/// inside those nested statements.
fn find_consumers(tir: &Tir, r: TirRef, consumer_of: &mut HashMap<TirRef, TirRef>) {
    walk_operands(tir, r, &mut |parent, operand, kind| {
        if matches!(kind, ChildKind::Operand) {
            consumer_of.entry(operand).or_insert(parent);
        }
        find_consumers(tir, operand, consumer_of);
    });
}

/// Distinguishes the two kinds of (parent, child) edges announced
/// by [`walk_operands`]:
///
/// * [`ChildKind::Operand`] — a direct data dependency (e.g. a
///   binary-op LHS, a call arg, a `VarDecl`'s initializer, an
///   `if`/`while`/`for` condition or range bound). Consumer of
///   the parent.
/// * [`ChildKind::BodyStmt`] — a statement nested inside an
///   `if`/`while`/`for` body. Reachable from the parent for
///   traversal, but not a consumer of the parent.
#[derive(Clone, Copy)]
enum ChildKind {
    Operand,
    BodyStmt,
}

/// Visit every direct operand and body-statement edge of TIR
/// instruction `r`, invoking `f(parent, child, kind)` for each
/// `(parent, child)` edge in forward source order. **Shallow** —
/// does not recurse on its own; callers' closures drive recursion
/// (see [`collect_last_uses`] / [`find_consumers`]). Avoids the
/// O(2^N) re-walking that an internally-recursive walker would
/// produce when callers also recurse via their closures.
///
/// Single source of truth for TIR-shape coverage across the
/// ownership pass's post-walk analyses (last-use, consumer-of, …).
/// Adding a new TIR shape requires updating exactly this function.
fn walk_operands(tir: &Tir, r: TirRef, f: &mut impl FnMut(TirRef, TirRef, ChildKind)) {
    let inst = *tir.inst(r);
    match inst.data {
        TirData::UnOp(o) => {
            f(r, o, ChildKind::Operand);
        }
        TirData::BinOp { lhs, rhs } => {
            f(r, lhs, ChildKind::Operand);
            f(r, rhs, ChildKind::Operand);
        }
        TirData::Extra(_) => match inst.tag {
            TirTag::Call => {
                let view = tir.call_view(r);
                for &arg in &view.args {
                    f(r, arg, ChildKind::Operand);
                }
            }
            TirTag::VarDecl => {
                let v = tir.var_decl_view(r);
                f(r, v.initializer, ChildKind::Operand);
            }
            TirTag::Assign => {
                let v = tir.assign_view(r);
                f(r, v.value, ChildKind::Operand);
            }
            TirTag::CompoundAssign => {
                let v = tir.compound_assign_view(r);
                f(r, v.value, ChildKind::Operand);
            }
            TirTag::IfStmt => {
                let v = tir.if_stmt_view(r);
                f(r, v.cond, ChildKind::Operand);
                for &s in &v.then_stmts {
                    f(r, s, ChildKind::BodyStmt);
                }
                for elif in &v.elif_branches {
                    f(r, elif.cond, ChildKind::Operand);
                    for &s in &elif.body {
                        f(r, s, ChildKind::BodyStmt);
                    }
                }
                if let Some(else_stmts) = &v.else_stmts {
                    for &s in else_stmts {
                        f(r, s, ChildKind::BodyStmt);
                    }
                }
            }
            TirTag::WhileLoop => {
                let v = tir.while_loop_view(r);
                f(r, v.cond, ChildKind::Operand);
                for &s in &v.body {
                    f(r, s, ChildKind::BodyStmt);
                }
            }
            TirTag::ForRange => {
                let v = tir.for_range_view(r);
                f(r, v.start, ChildKind::Operand);
                f(r, v.end, ChildKind::Operand);
                for &s in &v.body {
                    f(r, s, ChildKind::BodyStmt);
                }
            }
            _ => {}
        },
        TirData::None
        | TirData::Int(_)
        | TirData::Float(_)
        | TirData::Str(_)
        | TirData::Bool(_)
        | TirData::Var(_) => {}
    }
}

/// Schedule unconditional Frees on `break`/`continue` paths. Runs after
/// the last-use, anonymous-temp, and dead-store passes have populated
/// `sidecar.free_schedule`, so per-jump scheduling can read existing
/// entries to decide whether an inside-loop owner is already covered
/// (see `schedule_break_continue_frees`). `enclosing_loop` is the
/// nearest enclosing `WhileLoop`/`ForRange` instruction reference, or
/// `None` at top-level (where `Break`/`Continue` would already have
/// been rejected by sema).
fn schedule_loop_exit_frees_in(
    tir: &Tir,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    stmts: &[TirRef],
    enclosing_loop: Option<TirRef>,
) {
    for &r in stmts {
        let inst = *tir.inst(r);
        match inst.tag {
            TirTag::Break | TirTag::Continue => {
                if let Some(loop_ref) = enclosing_loop {
                    schedule_break_continue_frees(tir, own, sidecar, r, loop_ref);
                }
                // Else: outside any loop — sema rejects this with a
                // dedicated diagnostic, so well-formed TIR never
                // reaches here.
            }
            TirTag::WhileLoop => {
                let view = tir.while_loop_view(r);
                schedule_loop_exit_frees_in(tir, own, sidecar, &view.body, Some(r));
            }
            TirTag::ForRange => {
                let view = tir.for_range_view(r);
                schedule_loop_exit_frees_in(tir, own, sidecar, &view.body, Some(r));
            }
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                schedule_loop_exit_frees_in(tir, own, sidecar, &view.then_stmts, enclosing_loop);
                for elif in &view.elif_branches {
                    schedule_loop_exit_frees_in(tir, own, sidecar, &elif.body, enclosing_loop);
                }
                if let Some(else_stmts) = &view.else_stmts {
                    schedule_loop_exit_frees_in(tir, own, sidecar, else_stmts, enclosing_loop);
                }
            }
            _ => {}
        }
    }
}

/// Slice of body statements for a `WhileLoop`/`ForRange` instruction,
/// materialized into an owned `Vec<TirRef>` so the caller can re-borrow
/// `tir` for recursive walks. Returns `None` for non-loop refs.
fn loop_body(tir: &Tir, loop_inst: TirRef) -> Option<Vec<TirRef>> {
    match tir.inst(loop_inst).tag {
        TirTag::WhileLoop => Some(tir.while_loop_view(loop_inst).body.to_vec()),
        TirTag::ForRange => Some(tir.for_range_view(loop_inst).body.to_vec()),
        _ => None,
    }
}

/// Collect every `TirRef` reachable from `loop_inst`'s body —
/// transitive operands AND nested body statements — into `set`.
/// Used to classify owners as inside-loop vs pre-loop without
/// relying on the broken `raw()` proxy (producer refs are pushed
/// before their parent body stmt, so a producer's `raw()` can be
/// below the parent body-stmt's `raw()` even though it's
/// semantically inside).
///
/// `walk_operands` is shallow; this helper drives the recursion.
fn collect_loop_body_refs(tir: &Tir, loop_inst: TirRef, set: &mut HashSet<TirRef>) {
    let Some(body) = loop_body(tir, loop_inst) else {
        return;
    };
    for stmt in body {
        collect_refs_recursive(tir, stmt, set);
    }
}

fn collect_refs_recursive(tir: &Tir, r: TirRef, set: &mut HashSet<TirRef>) {
    if !set.insert(r) {
        return;
    }
    walk_operands(tir, r, &mut |_parent, child, _kind| {
        collect_refs_recursive(tir, child, set);
    });
}

/// Collect the set of TirRefs evaluated on the path that reaches
/// `target` (a `break`/`continue` instruction) within `body`. Returns
/// `true` if `target` was located.
///
/// Walk-down rule: a body-stmt that does NOT contain `target` runs
/// to completion before the jump's stmt is reached, so all of its
/// operand-reachable refs are on-path. The body-stmt that DOES
/// contain `target` recurses: into the right `IfStmt` arm, or into
/// a nested loop's body. For an `IfStmt` we collect the cond
/// unconditionally (it always runs); we then locate the single arm
/// (then / a specific elif / else) that contains `target` and only
/// recurse into that arm — sibling arms are off-path. For elif
/// arms we also collect each preceding elif's cond (those conds
/// executed; their bodies were skipped).
///
/// Used by `schedule_break_continue_frees` to decide whether an
/// existing `FreePoint` actually fires on the jump's path. A Free
/// anchored in a sibling arm has its `after` ref outside this set,
/// so it's correctly classified as not-covering.
fn collect_jump_path(
    tir: &Tir,
    body: &[TirRef],
    target: TirRef,
    set: &mut HashSet<TirRef>,
) -> bool {
    for &stmt in body {
        if stmt == target {
            set.insert(target);
            return true;
        }
        let mut sub: HashSet<TirRef> = HashSet::new();
        collect_refs_recursive(tir, stmt, &mut sub);
        if sub.contains(&target) {
            // `stmt` contains the jump. Descend along the right arm.
            match tir.inst(stmt).tag {
                TirTag::IfStmt => {
                    let view = tir.if_stmt_view(stmt);
                    set.insert(stmt);
                    collect_refs_recursive(tir, view.cond, set);
                    // Locate the arm containing `target`; only that arm's
                    // statements are on-path. Earlier elif conds executed
                    // (their bodies skipped) so collect just the conds up
                    // to the chosen arm.
                    let mut then_sub: HashSet<TirRef> = HashSet::new();
                    for &s in &view.then_stmts {
                        collect_refs_recursive(tir, s, &mut then_sub);
                    }
                    if then_sub.contains(&target) {
                        return collect_jump_path(tir, &view.then_stmts, target, set);
                    }
                    for elif in &view.elif_branches {
                        collect_refs_recursive(tir, elif.cond, set);
                        let mut elif_sub: HashSet<TirRef> = HashSet::new();
                        for &s in &elif.body {
                            collect_refs_recursive(tir, s, &mut elif_sub);
                        }
                        if elif_sub.contains(&target) {
                            return collect_jump_path(tir, &elif.body, target, set);
                        }
                    }
                    if let Some(else_stmts) = &view.else_stmts {
                        return collect_jump_path(tir, else_stmts, target, set);
                    }
                    return true;
                }
                TirTag::WhileLoop | TirTag::ForRange => {
                    // Inner loop containing the jump. Should not
                    // happen when called from the innermost-enclosing-
                    // loop's break-pass — the inner loop runs its own
                    // schedule_break_continue_frees pass for that jump.
                    return true;
                }
                _ => {
                    // Other shapes (ExprStmt, etc.). The stmt's
                    // operands are evaluated as part of reaching
                    // `target`.
                    collect_refs_recursive(tir, stmt, set);
                    return true;
                }
            }
        }
        // `stmt` does not contain target — fully evaluated before target.
        collect_refs_recursive(tir, stmt, set);
    }
    false
}

/// Schedule one Free per `Valid` owner not already covered by a Free
/// that fires on the jump's path.
///
/// Inside-loop owners (in `inside_loop`) — schedule iff no scheduled
/// Free is anchored on the path that reaches this jump. The next
/// iteration allocates a fresh buffer, so jump-side Frees are safe.
///
/// Pre-loop owners — schedule defensively iff NO Free is scheduled
/// anywhere. We can't free a pre-loop owner on the jump path: a
/// `continue` would resurrect the binding for the next iteration,
/// which would then read freed memory. The defensive emit covers
/// future producers that bypass last-use AND dead-store passes.
///
/// Catches the I-058 leak in two shapes:
///   - linear: `for: s = alloc(); break; print(s)` — natural Free is
///     anchored AFTER the break in source order; not on jump path.
///   - cross-branch: `for: s = alloc(); if cond: print(s) else: break`
///     — natural Free is anchored INSIDE the then-arm; the else-arm's
///     break is not on the same path, so the Free doesn't cover it.
///
/// `inside_loop` and `on_path` are computed by transitive reachability
/// from the loop's body — `raw()` comparisons are unsound (producer
/// refs sit numerically below their parent body stmt, and sibling
/// if-arms compare lexically but are not on the same control-flow
/// path).
fn schedule_break_continue_frees(
    tir: &Tir,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    jump_inst: TirRef,
    loop_inst: TirRef,
) {
    // Classify owners by transitive reachability from the loop body.
    // raw() comparisons are unsound here — producer refs sit
    // numerically below their parent body stmt.
    let mut inside_loop: HashSet<TirRef> = HashSet::new();
    collect_loop_body_refs(tir, loop_inst, &mut inside_loop);

    // Compute the set of TirRefs evaluated on the path that takes
    // the jump. A Free covers this jump iff its `after` anchor is
    // in this set — purely lexical raw() ordering misclassifies
    // anchors in sibling if-arms.
    let mut on_path: HashSet<TirRef> = HashSet::new();
    let Some(body) = loop_body(tir, loop_inst) else {
        return;
    };
    let _ = collect_jump_path(tir, &body, jump_inst, &mut on_path);

    // Index sidecar.free_schedule by target.
    //   has_any           — Free scheduled anywhere
    //   covers_this_jump  — Free anchored on a TirRef in `on_path`
    //                       (i.e. fires on the path that takes the jump)
    let mut has_any: HashSet<TirRef> = HashSet::new();
    let mut covers_this_jump: HashSet<TirRef> = HashSet::new();
    let mut free_inside_loop: HashSet<TirRef> = HashSet::new();
    for fp in &sidecar.free_schedule {
        has_any.insert(fp.target);
        if on_path.contains(&fp.after) {
            covers_this_jump.insert(fp.target);
        }
        if inside_loop.contains(&fp.after) {
            free_inside_loop.insert(fp.target);
        }
    }

    let is_break = matches!(tir.inst(jump_inst).tag, TirTag::Break);

    for (owner, state) in &own.states {
        let r = match owner.inst_tirref() {
            Some(r) => r,
            None => continue,
        };
        if !matches!(state, OwnerState::Valid) {
            continue;
        }

        if inside_loop.contains(&r) {
            // Inside-loop owner: each iteration allocates fresh, so
            // a jump-anchored Free is safe. Schedule iff no Free
            // already fires on this jump's path.
            if covers_this_jump.contains(&r) {
                continue;
            }
            sidecar.free_schedule.push(FreePoint {
                after: jump_inst,
                target: r,
                span: tir.span(jump_inst),
                branch: None,
            });
            continue;
        }

        // Pre-loop owner: cannot free on continue path — `continue`
        // would resurrect the binding for the next iteration's read.
        // But on break path, if its last-use is inside the loop (which
        // we bypassed), we must free it as we exit the loop.
        if is_break && free_inside_loop.contains(&r) {
            if covers_this_jump.contains(&r) {
                continue;
            }
            sidecar.free_schedule.push(FreePoint {
                after: jump_inst,
                target: r,
                span: tir.span(jump_inst),
                branch: None,
            });
            continue;
        }

        // Defensive emit only when no Free is scheduled anywhere — AND only on
        // break. On `continue` the next iteration would re-read the freed buffer
        // (UAF); the principled fix is path-relative liveness, but until then we
        // accept a potential leak over a UAF. See I-067.
        if is_break && has_any.contains(&r) {
            continue;
        }
        if is_break {
            sidecar.free_schedule.push(FreePoint {
                after: jump_inst,
                target: r,
                span: tir.span(jump_inst),
                branch: None,
            });
        }
    }
}

fn visit_expr(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    match inst.tag {
        // ---- Allocating instructions ----
        // `StrConst` materializes a fresh heap string at runtime;
        // `StrConcat` produces a brand-new allocation from its two
        // operands. Both enter the lattice as `Valid` with no
        // upstream origin.
        TirTag::StrConst => {
            if needs_tracking(inst.ty, pool) {
                own.states.insert(Owner::Inst(r), OwnerState::Valid);
                own.origin.insert(r, None);
                own.temp_owners.insert(Owner::Inst(r));
            }
        }
        TirTag::StrConcat => {
            if needs_tracking(inst.ty, pool) {
                own.states.insert(Owner::Inst(r), OwnerState::Valid);
                own.origin.insert(r, None);
                own.temp_owners.insert(Owner::Inst(r));
            }
            if let TirData::BinOp { lhs, rhs } = inst.data {
                visit_expr(tir, pool, own, sink, sidecar, lhs);
                visit_expr(tir, pool, own, sink, sidecar, rhs);
                for op in [lhs, rhs] {
                    if needs_tracking(tir.inst(op).ty, pool) {
                        check_use_moved(tir, pool, own, sink, op, tir.span(op));
                    }
                }
            }
        }
        TirTag::Call => {
            // A str-returning call (e.g. `int_to_str`) is a producer.
            if needs_tracking(inst.ty, pool) {
                own.states.insert(Owner::Inst(r), OwnerState::Valid);
                own.origin.insert(r, None);
                own.temp_owners.insert(Owner::Inst(r));
            }
            let view = tir.call_view(r);

            // Phase 1 — materialise every arg's owner/state.
            for arg in &view.args {
                visit_expr(tir, pool, own, sink, sidecar, *arg);
            }

            // Phase 2 — use-safety check + borrow/move/inout partition.
            let mut borrowed: HashSet<Owner> = HashSet::new();
            let mut moved: HashSet<Owner> = HashSet::new();
            // inout occurrences: (owner, arg span) — a Vec, not a Set, so
            // a double-inout of one owner is detectable by count (Rule 7).
            let mut inout_uses: Vec<(Owner, Span)> = Vec::new();
            for (i, arg) in view.args.iter().enumerate() {
                if is_borrowed_scalar_param(view.name, pool, i)
                    && matches!(tir.inst(*arg).tag, TirTag::StrConst)
                {
                    // Undo the temp_owners insertion seeded by visit_expr's
                    // StrConst arm. `states`, `origin`, and `owner_at_read`
                    // entries are harmless to leave populated — only
                    // `temp_owners` is consulted by the anonymous-temp Free
                    // pass. See I-057.
                    own.temp_owners.remove(&Owner::Inst(*arg));
                }
                let mode = view.modes.get(i).copied().unwrap_or(ParamMode::Borrow);
                let arg_ty = tir.inst(*arg).ty;
                if mode == ParamMode::Inout {
                    // Rule 7 (M8.3): resolve an aliasing identity even for
                    // Copy scalars, which never enter the lattice.
                    let owner = inout_owner(own, tir, *arg);
                    check_use_moved(tir, pool, own, sink, *arg, tir.span(*arg));
                    inout_uses.push((owner, tir.span(*arg)));
                    continue;
                }
                if !needs_tracking(arg_ty, pool) {
                    // A Copy borrow is a no-op for liveness, but it still
                    // aliases an `inout` of the same binding in this call —
                    // record Var reads by name for the Rule 7 overlap check.
                    // (A Copy `move` arg is rejected by sema's RedundantMove,
                    // so only the Borrow arm is reachable from real code.)
                    if mode == ParamMode::Borrow && matches!(tir.inst(*arg).tag, TirTag::Var) {
                        borrowed.insert(inout_owner(own, tir, *arg));
                    }
                    continue;
                }
                let owner = underlying_owner(own, *arg);
                if mode == ParamMode::Borrow {
                    check_use_moved(tir, pool, own, sink, *arg, tir.span(*arg));
                    borrowed.insert(owner);
                } else {
                    moved.insert(owner);
                }
            }
            // Rule 7 (M8.3): at most one mutable borrow per owner in a call,
            // and no immutable borrow / move alongside it. Each DISTINCT
            // inout owner is handled exactly once (the `seen_owners` guard)
            // so cases 2/3 don't re-fire per occurrence.
            let mut seen_owners: HashSet<Owner> = HashSet::new();
            for (owner, _span) in &inout_uses {
                if !seen_owners.insert(*owner) {
                    continue;
                }
                let name = owner_name_for_diag(*owner, tir, pool);

                // (1) The same owner is mutably borrowed more than once.
                // Collect every occurrence so the two notes point at
                // DISTINCT arg spans — the loop item's own span is
                // occurrence 0, so reusing it for the "second" note would
                // duplicate the "first" note's span.
                let occurrences: Vec<Span> = inout_uses
                    .iter()
                    .filter(|(o, _)| o == owner)
                    .map(|(_, s)| *s)
                    .collect();
                if occurrences.len() > 1 {
                    let mut diag = Diag::error(
                        tir.span(r),
                        DiagCode::MutableAliasingViolation,
                        format!(
                            "cannot borrow {} as mutable more than once in the same call",
                            name
                        ),
                    );
                    diag = diag.with_note(Some(occurrences[0]), "first mutable borrow here");
                    diag = diag.with_note(Some(occurrences[1]), "second mutable borrow here");
                    diag = diag.with_help("a value can have one mutable borrow OR many immutable borrows in a call, never both (Rule 7)");
                    sink.emit(diag);
                }
                // (2) inout ∩ borrowed.
                if borrowed.contains(owner) {
                    sink.emit(
                        Diag::error(
                            tir.span(r),
                            DiagCode::MutableAliasingViolation,
                            format!(
                                "cannot borrow {} as immutable while it is mutably borrowed in the same call",
                                name
                            ),
                        )
                        .with_help("a value can have one mutable borrow OR many immutable borrows in a call, never both (Rule 7)"),
                    );
                }
                // (3) inout ∩ moved.
                if moved.contains(owner) {
                    sink.emit(
                        Diag::error(
                            tir.span(r),
                            DiagCode::MutableAliasingViolation,
                            format!(
                                "cannot move {} while it is mutably borrowed in the same call",
                                name
                            ),
                        )
                        .with_help("finish the mutable borrow before moving the value"),
                    );
                }
            }
            // Overlap — same owner borrowed AND moved in one call.
            for owner in borrowed.intersection(&moved) {
                let name = owner_name_for_diag(*owner, tir, pool);

                // Find spans of the conflicting arguments for this owner
                let mut borrow_span = None;
                let mut move_span = None;
                for (i, arg) in view.args.iter().enumerate() {
                    if needs_tracking(tir.inst(*arg).ty, pool)
                        && underlying_owner(own, *arg) == *owner
                    {
                        let mode = view.modes.get(i).copied().unwrap_or(ParamMode::Borrow);
                        match mode {
                            ParamMode::Borrow => borrow_span = Some(tir.span(*arg)),
                            ParamMode::Move => move_span = Some(tir.span(*arg)),
                            // inout overlaps are reported as E0032 above —
                            // never as the "moved here" half of E0031.
                            ParamMode::Inout => {}
                        }
                    }
                }

                let mut diag = Diag::error(
                    tir.span(r),
                    DiagCode::MoveWhileBorrowedInCall,
                    format!("cannot move {} while it is borrowed in the same call", name),
                );
                if let Some(b_span) = borrow_span {
                    diag = diag.with_note(Some(b_span), "borrowed here");
                }
                if let Some(m_span) = move_span {
                    diag = diag.with_note(Some(m_span), "moved here");
                }
                diag = diag.with_help("borrows are live for the whole call; pass by `move` on a separate statement, or borrow in both positions");
                sink.emit(diag);
            }

            // Phase 3 — commit the moves.
            for (i, arg) in view.args.iter().enumerate() {
                let mode = view.modes.get(i).copied().unwrap_or(ParamMode::Borrow);
                if mode == ParamMode::Move && needs_tracking(tir.inst(*arg).ty, pool) {
                    let consumed_name = consumed_binding_name(tir, *arg);
                    consume_for_assignment(tir, pool, own, sink, *arg, tir.span(r), consumed_name);
                }
            }

            // `inout` args need no ownership transition: the callee only
            // borrows the slot, and the binding keeps its pre-call owner.
            // The stale-triple hazard (callee realloc'd/replaced the
            // buffer) is handled in CODEGEN, where named-binding Frees
            // emit the binding's CURRENT `StrLocals` instead of the
            // producing inst's cached repr (I-112) — the same pattern
            // `free_on_reassign` already used.
        }
        // ---- Aliasing read ----
        // `Var` is a non-consuming read. Record which SSA value it
        // currently aliases so a later use-after-move diagnostic can
        // walk back to the root owner. Reads of `Borrowed` owners are
        // fine (Rule 2 — borrowed parameters can be freely read).
        TirTag::Var => {
            let name = match inst.data {
                TirData::Var(n) => n,
                _ => unreachable!("Var must carry TirData::Var"),
            };
            if let Some(&owner) = own.current_owner.get(&name)
                && needs_tracking(inst.ty, pool)
            {
                // Any read counts as "used" for dead-store purposes,
                // even if it ultimately fires E0020 — once the
                // programmer's code looked at the value, they
                // didn't ignore it. Clear by NAME, not by current-owner
                // key: a reseat inside a branch (Assign) is discarded
                // by the branch merge, so the pending entry can survive
                // under a branch-local owner key while the binding
                // itself is provably read afterwards.
                own.pending_dead_store.retain(|_, (n, _, _)| *n != name);
                own.origin.insert(r, Some(owner));
                // Snapshot owner-at-read so the post-walk
                // `collect_last_uses` anchors the last-use Free to the
                // owner that was live *at this read*, not whatever
                // `current_owner[name]` happens to be at function exit
                // (which would route pre-rebind reads to the post-
                // rebind owner — wrong target, double-free).
                own.owner_at_read.insert(r, owner);
            }
        }
        // ---- Everything else: recurse on operands so nested
        // ---- producers/aliases are still observed.
        _ => {
            recurse_operands(tir, pool, own, sink, sidecar, r);
        }
    }
}

fn recurse_operands(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    match inst.data {
        TirData::UnOp(o) => {
            visit_expr(tir, pool, own, sink, sidecar, o);
            if needs_tracking(tir.inst(o).ty, pool) {
                check_use_moved(tir, pool, own, sink, o, tir.span(o));
            }
        }
        TirData::BinOp { lhs, rhs } => {
            visit_expr(tir, pool, own, sink, sidecar, lhs);
            if needs_tracking(tir.inst(lhs).ty, pool) {
                check_use_moved(tir, pool, own, sink, lhs, tir.span(lhs));
            }
            visit_expr(tir, pool, own, sink, sidecar, rhs);
            if needs_tracking(tir.inst(rhs).ty, pool) {
                check_use_moved(tir, pool, own, sink, rhs, tir.span(rhs));
            }
        }
        // `Extra`-shaped instructions (VarDecl, Assign, Call,
        // IfStmt, WhileLoop, ForRange, CompoundAssign) have
        // bespoke decoders. Consumption logic lands in subsequent
        // tasks; until then their operands are deliberately not
        // descended into here so we avoid double-visits when those
        // tasks introduce per-tag handling.
        TirData::Extra(_) => {}
        TirData::None
        | TirData::Int(_)
        | TirData::Float(_)
        | TirData::Str(_)
        | TirData::Bool(_)
        | TirData::Var(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an all-`Borrow` modes slice matching the length of an
    /// argument list. The ownership pass reads `view.modes` directly,
    /// so test/builtin call sites pass all-`Borrow` to avoid
    /// accidentally moving arguments.
    fn all_borrow(args: &[TirRef]) -> Vec<ryo_core::tir::ParamMode> {
        vec![ryo_core::tir::ParamMode::Borrow; args.len()]
    }

    #[test]
    fn copy_types_classified() {
        let pool = InternPool::new();
        assert!(pool.is_copy(pool.int()));
        assert!(pool.is_copy(pool.float()));
        assert!(pool.is_copy(pool.bool_()));
        assert!(!pool.is_copy(pool.str_()));
    }

    #[test]
    fn move_types_classified() {
        let pool = InternPool::new();
        assert!(is_move_type(pool.str_(), &pool));
        assert!(!is_move_type(pool.int(), &pool));
        assert!(!is_move_type(pool.bool_(), &pool));
    }

    #[test]
    fn needs_tracking_matches_move() {
        let pool = InternPool::new();
        assert!(needs_tracking(pool.str_(), &pool));
        assert!(!needs_tracking(pool.int(), &pool));
    }

    #[test]
    fn dead_store_schedules_free_after_decl() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s_name = pool.intern_str("s");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void: s: str = "hello"   # never read
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s_name, false, str_ty, lit, span);
        let tir = tb.finish(&[decl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        // W0001 fires.
        let diags = sink.into_diags();
        assert!(
            diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
            "expected DeadStore warning"
        );

        // Free anchored after the VarDecl, target = the literal's TirRef.
        assert!(
            sidecar
                .free_schedule
                .iter()
                .any(|fp| fp.after == decl && fp.target == lit && fp.branch.is_none()),
            "expected dead-store Free anchored at decl with target=lit; got: {:?}",
            sidecar.free_schedule
        );

        // Exactly one Free for `lit` — guards against Task 3/4 ever
        // double-counting (anonymous-temp pass + dead-store pass both
        // emitting for the same owner).
        assert_eq!(
            sidecar
                .free_schedule
                .iter()
                .filter(|fp| fp.target == lit)
                .count(),
            1,
            "expected exactly one Free for lit"
        );
    }

    #[test]
    fn ryo_panic_str_arg_excluded_via_abi_registry() {
        // Regression test for I-057/I-059. The StrConst arg of `__ryo_panic`
        // uses the borrowed-scalar ABI in codegen — codegen passes the
        // raw .rodata pointer with cap=0 and never owns the buffer. The
        // ownership pass excludes it from `temp_owners` by consulting the
        // `builtins` ABI registry (`is_borrowed_scalar_param`) rather than
        // a `pool.str(name) == "__ryo_panic"` name-match, so the
        // anonymous-temp Free pass does not schedule a Free for it.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let panic_name = pool.intern_str("__ryo_panic");
        let msg = pool.intern_str("boom");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void: __ryo_panic("boom", 4)
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let str_arg = tb.str_const(msg, str_ty, span);
        let len_arg = tb.int_const(4, int_ty, span);
        let call = tb.call(
            panic_name,
            &[str_arg, len_arg],
            &all_borrow(&[str_arg, len_arg]),
            void,
            span,
        );
        let tir = tb.finish(&[call]);

        let mut sink = DiagSink::new();
        let mut sidecar_map = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar_map
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        // No scheduled Free should target __ryo_panic's StrConst arg —
        // codegen's borrowed-scalar ABI never frees it.
        assert!(
            sidecar.free_schedule.iter().all(|fp| fp.target != str_arg),
            "expected no scheduled Free for __ryo_panic's StrConst arg, got: {:?}",
            sidecar.free_schedule
        );

        // I-059: the exclusion is driven by the ABI registry. `__ryo_panic`
        // passes param 0 (the message) via the borrowed-scalar ABI, but not
        // param 1 (the length) or any out-of-range index.
        assert!(
            is_borrowed_scalar_param(panic_name, &pool, 0),
            "__ryo_panic param 0 must be flagged borrowed-scalar by the registry"
        );
        assert!(
            !is_borrowed_scalar_param(panic_name, &pool, 1),
            "__ryo_panic param 1 must not be flagged borrowed-scalar"
        );
    }

    #[test]
    fn inside_loop_temp_is_freed() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let print_name = pool.intern_str("print");
        let inside = pool.intern_str("inside");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     while true:
        //         print("inside")     # StrConst is the temp under test
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let lit = tb.str_const(inside, str_ty, span);
        let print_call = tb.call(print_name, &[lit], &all_borrow(&[lit]), void, span);
        let wl = tb.while_loop(cond, &[print_call], void, span);
        let tir = tb.finish(&[wl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        assert!(
            sidecar.free_schedule.iter().any(|fp| fp.target == lit),
            "expected inside-loop StrConst to be scheduled for Free; got: {:?}",
            sidecar.free_schedule
        );
        assert_eq!(
            sidecar
                .free_schedule
                .iter()
                .filter(|fp| fp.target == lit)
                .count(),
            1,
            "expected exactly one Free for the inside-loop StrConst; got: {:?}",
            sidecar.free_schedule
        );
    }

    #[test]
    fn pre_loop_owner_break_before_last_use_schedules_free_on_break() {
        // A pre-loop owner whose last-use is inside a loop must be
        // freed on a `break` path that bypasses that last-use, as we
        // are exiting the loop and will never reach the last-use again.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s_name = pool.intern_str("s");
        let int_to_str = pool.intern_str("int_to_str");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     s: str = int_to_str(0)
        //     while true:
        //         if true:
        //             break
        //         print(s)
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let zero = tb.int_const(0, int_ty, span);
        let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
        let decl = tb.var_decl(s_name, false, str_ty, alloc, span);

        let cond_w = tb.bool_const(true, bool_ty, span);
        let cond_i = tb.bool_const(true, bool_ty, span);
        let brk = tb.break_stmt(void, span);
        let if_inside = tb.if_stmt(cond_i, &[brk], &[], None, void, span);
        let s_var = tb.var(s_name, str_ty, span);
        let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
        let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
        let wl = tb.while_loop(cond_w, &[if_inside, print_stmt], void, span);
        let tir = tb.finish(&[decl, wl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        assert!(
            sidecar
                .free_schedule
                .iter()
                .any(|fp| fp.after == brk && fp.target == alloc),
            "expected pre-loop owner whose last-use is in the loop to be freed on break; got: {:?}",
            sidecar.free_schedule
        );
    }

    #[test]
    fn pre_loop_owner_continue_before_last_use_does_not_free_on_continue() {
        // A pre-loop owner whose last-use is inside a loop must NOT be
        // freed on a `continue` path, as we will loop back and might
        // read it in the next iteration (causing use-after-free).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s_name = pool.intern_str("s");
        let int_to_str = pool.intern_str("int_to_str");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     s: str = int_to_str(0)
        //     while true:
        //         if true:
        //             continue
        //         print(s)
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let zero = tb.int_const(0, int_ty, span);
        let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
        let decl = tb.var_decl(s_name, false, str_ty, alloc, span);

        let cond_w = tb.bool_const(true, bool_ty, span);
        let cond_i = tb.bool_const(true, bool_ty, span);
        let cont = tb.continue_stmt(void, span);
        let if_inside = tb.if_stmt(cond_i, &[cont], &[], None, void, span);
        let s_var = tb.var(s_name, str_ty, span);
        let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
        let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
        let wl = tb.while_loop(cond_w, &[if_inside, print_stmt], void, span);
        let tir = tb.finish(&[decl, wl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        // The only Free for `alloc` must be anchored on `print_call`,
        // and we must NOT have any Free anchored on `cont`.
        let frees_for_alloc: Vec<_> = sidecar
            .free_schedule
            .iter()
            .filter(|fp| fp.target == alloc)
            .collect();
        assert_eq!(
            frees_for_alloc.len(),
            1,
            "expected exactly one Free for `alloc`; got: {:?}",
            sidecar.free_schedule
        );
        assert_ne!(
            frees_for_alloc[0].after, cont,
            "Free for pre-loop owner must not be anchored on continue; got: {:?}",
            frees_for_alloc[0]
        );
    }

    #[test]
    fn continue_jump_does_not_free_pre_loop_owner_uaf_guard() {
        // Pre-loop owner read only inside the loop, with a `continue` before
        // its last use. The defensive emit must NOT fire on continue (would
        // free the buffer the next iteration reads -> UAF). break still frees.
        // Construct: s = "x"; while c: if d: continue; print(s)
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let bool_ty = pool.bool_();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let c = pool.intern_str("c");
        let d = pool.intern_str("d");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("x"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let cond = tb.var(c, bool_ty, span);
        let cont = tb.continue_stmt(void, span);
        let sv = tb.var(s, str_ty, span);
        let pcall = tb.call(
            print,
            &[sv],
            &[ryo_core::tir::ParamMode::Borrow],
            void,
            span,
        );
        let ifcond = tb.var(d, bool_ty, span);
        let ifstmt = tb.if_stmt(ifcond, &[cont], &[], Some(&[pcall]), void, span);
        let lp = tb.while_loop(cond, &[ifstmt], void, span);
        let tir = tb.finish(&[decl, lp]);
        let mut sink = DiagSink::new();
        let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = sc.functions.remove(&main).unwrap();
        // No Free for `lit` anchored on the continue jump:
        assert!(
            !sc.free_schedule
                .iter()
                .any(|fp| fp.target == lit && fp.after == cont),
            "continue must not free the pre-loop owner (UAF); got {sc:?}"
        );
    }

    #[test]
    fn continue_jump_does_not_free_pre_loop_owner_uaf_guard_reassigned() {
        // Construct: s = "x"; while c: if d: continue; s = "y"
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let bool_ty = pool.bool_();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let c = pool.intern_str("c");
        let d = pool.intern_str("d");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit1 = tb.str_const(pool.intern_str("x"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit1, span);
        let cond = tb.var(c, bool_ty, span);
        let cont = tb.continue_stmt(void, span);
        let ifcond = tb.var(d, bool_ty, span);
        let ifstmt = tb.if_stmt(ifcond, &[cont], &[], None, void, span);
        let lit2 = tb.str_const(pool.intern_str("y"), str_ty, span);
        let assign = tb.assign(s, str_ty, lit2, span);
        let lp = tb.while_loop(cond, &[ifstmt, assign], void, span);
        let tir = tb.finish(&[decl, lp]);
        let mut sink = DiagSink::new();
        let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = sc.functions.remove(&main).unwrap();
        // Under the buggy compiler, `lit1` is defensively freed on `cont`:
        let freed_on_cont = sc
            .free_schedule
            .iter()
            .any(|fp| fp.target == lit1 && fp.after == cont);
        assert!(
            !freed_on_cont,
            "continue must not free the pre-loop owner (UAF); got {sc:?}"
        );
    }

    #[test]
    fn break_before_last_use_schedules_jump_free() {
        // Regression for I-058. A `break` taken before the `print(s)`
        // last-use must trigger a Free anchored on the break instr —
        // otherwise the inside-loop allocation leaks on the break path.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s_name = pool.intern_str("s");
        let int_to_str = pool.intern_str("int_to_str");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     while true:
        //         s: str = int_to_str(0)
        //         if true:
        //             break
        //         print(s)
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let cond_w = tb.bool_const(true, bool_ty, span);
        let zero = tb.int_const(0, int_ty, span);
        let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
        let decl = tb.var_decl(s_name, false, str_ty, alloc, span);
        let cond_i = tb.bool_const(true, bool_ty, span);
        let brk = tb.break_stmt(void, span);
        let if_inside = tb.if_stmt(cond_i, &[brk], &[], None, void, span);
        let s_var = tb.var(s_name, str_ty, span);
        let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
        let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
        let wl = tb.while_loop(cond_w, &[decl, if_inside, print_stmt], void, span);
        let tir = tb.finish(&[wl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        assert!(
            sidecar
                .free_schedule
                .iter()
                .any(|fp| fp.after == brk && fp.target == alloc),
            "expected a Free for the inside-loop owner anchored on break; got: {:?}",
            sidecar.free_schedule
        );
    }

    #[test]
    fn break_after_last_use_does_not_double_schedule() {
        // The `break_inside_loop_owner` shape: print(s) is before
        // break, so the natural last-use Free fires before the jump
        // on any path that reaches it. The break/continue scheduler
        // must NOT add a redundant Free anchored on break, or codegen
        // would double-free.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s_name = pool.intern_str("s");
        let int_to_str = pool.intern_str("int_to_str");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     while true:
        //         s: str = int_to_str(0)
        //         print(s)
        //         if true:
        //             break
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let cond_w = tb.bool_const(true, bool_ty, span);
        let zero = tb.int_const(0, int_ty, span);
        let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
        let decl = tb.var_decl(s_name, false, str_ty, alloc, span);
        let s_var = tb.var(s_name, str_ty, span);
        let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
        let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
        let cond_i = tb.bool_const(true, bool_ty, span);
        let brk = tb.break_stmt(void, span);
        let if_inside = tb.if_stmt(cond_i, &[brk], &[], None, void, span);
        let wl = tb.while_loop(cond_w, &[decl, print_stmt, if_inside], void, span);
        let tir = tb.finish(&[wl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        // Exactly one Free for `alloc`, anchored on `print_call` (the
        // last-use), not on `brk`.
        let frees_for_alloc: Vec<_> = sidecar
            .free_schedule
            .iter()
            .filter(|fp| fp.target == alloc)
            .collect();
        assert_eq!(
            frees_for_alloc.len(),
            1,
            "expected exactly one Free for `alloc`; got: {:?}",
            sidecar.free_schedule
        );
        assert_ne!(
            frees_for_alloc[0].after, brk,
            "Free for `alloc` must not be anchored on break; got: {:?}",
            frees_for_alloc[0]
        );
    }

    #[test]
    fn break_in_else_arm_sibling_print_schedules_jump_free() {
        // Cross-branch I-058 regression. The natural last-use Free for
        // `alloc` anchors on `print(s)` inside the THEN arm; the break
        // sits in the ELSE arm. Lexical raw() ordering would put the
        // print's anchor before the break, but on the break path the
        // print never ran — so the buffer leaks unless we schedule a
        // jump-anchored Free here.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s_name = pool.intern_str("s");
        let int_to_str = pool.intern_str("int_to_str");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     while true:
        //         s: str = int_to_str(0)
        //         if true:
        //             print(s)        # natural last-use, then-arm
        //         else:
        //             break           # cross-branch leak site
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let cond_w = tb.bool_const(true, bool_ty, span);
        let zero = tb.int_const(0, int_ty, span);
        let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
        let decl = tb.var_decl(s_name, false, str_ty, alloc, span);
        let cond_i = tb.bool_const(true, bool_ty, span);
        let s_var = tb.var(s_name, str_ty, span);
        let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
        let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
        let brk = tb.break_stmt(void, span);
        let if_inside = tb.if_stmt(cond_i, &[print_stmt], &[], Some(&[brk]), void, span);
        let wl = tb.while_loop(cond_w, &[decl, if_inside], void, span);
        let tir = tb.finish(&[wl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        // Two Frees expected: one anchored on s_var (the Var read in
        // the then-arm — collect_last_uses anchors on Var reads, not
        // their wrapping Call), one anchored on brk (cross-branch
        // leak fix).
        let frees_for_alloc: Vec<_> = sidecar
            .free_schedule
            .iter()
            .filter(|fp| fp.target == alloc)
            .collect();
        assert!(
            frees_for_alloc.iter().any(|fp| fp.after == s_var),
            "expected then-arm last-use Free anchored on s_var (the Var read); got: {:?}",
            sidecar.free_schedule
        );
        assert!(
            frees_for_alloc.iter().any(|fp| fp.after == brk),
            "expected cross-branch jump-anchored Free on break; got: {:?}",
            sidecar.free_schedule
        );
    }

    #[test]
    fn continue_before_last_use_schedules_jump_free() {
        // Symmetric I-058 regression for `continue` instead of `break`.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s_name = pool.intern_str("s");
        let int_to_str = pool.intern_str("int_to_str");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     while true:
        //         s: str = int_to_str(0)
        //         if true:
        //             continue
        //         print(s)
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let cond_w = tb.bool_const(true, bool_ty, span);
        let zero = tb.int_const(0, int_ty, span);
        let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
        let decl = tb.var_decl(s_name, false, str_ty, alloc, span);
        let cond_i = tb.bool_const(true, bool_ty, span);
        let cont = tb.continue_stmt(void, span);
        let if_inside = tb.if_stmt(cond_i, &[cont], &[], None, void, span);
        let s_var = tb.var(s_name, str_ty, span);
        let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
        let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
        let wl = tb.while_loop(cond_w, &[decl, if_inside, print_stmt], void, span);
        let tir = tb.finish(&[wl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        assert!(
            sidecar
                .free_schedule
                .iter()
                .any(|fp| fp.after == cont && fp.target == alloc),
            "expected a Free for the inside-loop owner anchored on continue; got: {:?}",
            sidecar.free_schedule
        );
    }

    #[test]
    fn pre_loop_owner_read_only_in_loop_is_freed() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s_name = pool.intern_str("s");
        let print_name = pool.intern_str("print");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     s: str = "hello"
        //     while false:
        //         print(s)            # only read of `s`, inside the loop
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s_name, false, str_ty, lit, span);
        let cond = tb.bool_const(false, bool_ty, span);
        let s_var = tb.var(s_name, str_ty, span);
        let print_call = tb.call(print_name, &[s_var], &all_borrow(&[s_var]), void, span);
        let wl = tb.while_loop(cond, &[print_call], void, span);
        let tir = tb.finish(&[decl, wl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        let diags = sink.into_diags();
        assert!(
            !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
            "did not expect DeadStore for `s` (it is read inside the loop): {:?}",
            diags
        );

        let count = sidecar
            .free_schedule
            .iter()
            .filter(|fp| fp.target == lit)
            .count();
        assert_eq!(
            count, 1,
            "expected exactly one Free for pre-loop owner read only inside loop; got {} schedule={:?}",
            count, sidecar.free_schedule
        );
    }

    #[test]
    fn last_use_scheduled_for_unmoved_local() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let hello = pool.intern_str("hello");
        let s_name = pool.intern_str("s");
        let print_name = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     s: str = "hello"
        //     print(s)
        let mut b = TirBuilder::new(main, vec![], void, span);
        let lit = b.str_const(hello, str_ty, span);
        let decl = b.var_decl(s_name, false, str_ty, lit, span);
        let var_read = b.var(s_name, str_ty, span);
        let call = b.call(
            print_name,
            &[var_read],
            &all_borrow(&[var_read]),
            void,
            span,
        );
        let stmt = b.unary(TirTag::ExprStmt, void, call, span);
        let tir = b.finish(&[decl, stmt]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");
        assert!(sink.is_empty(), "expected no diagnostics");
        assert_eq!(sidecar.free_schedule.len(), 1);
        assert_eq!(sidecar.free_schedule[0].target, lit);
        assert_eq!(sidecar.free_schedule[0].after, var_read);
        assert!(sidecar.free_schedule[0].branch.is_none());
    }

    #[test]
    fn reassignment_records_free_on_old_owner() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let hello = pool.intern_str("hello");
        let world = pool.intern_str("world");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        // fn main():
        //     mut s: str = "hello"
        //     s = "world"
        //     print(s)
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let l1 = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, /* mutable = */ true, str_ty, l1, span);
        let l2 = tb.str_const(world, str_ty, span);
        let assign = tb.assign(s, str_ty, l2, span);
        let var_read = tb.var(s, str_ty, span);
        let call = tb.call(print, &[var_read], &all_borrow(&[var_read]), void, span);
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, assign, stmt]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");
        assert!(sink.is_empty(), "expected no diagnostics");

        // Reassign frees l1 (old owner) keyed on the Assign inst.
        assert_eq!(
            sidecar.free_on_reassign.get(&assign),
            Some(&l1),
            "expected free_on_reassign[assign] = l1"
        );

        // Last-use frees l2 (new owner reaches function exit via print(s)).
        assert!(
            sidecar
                .free_schedule
                .iter()
                .any(|fp| fp.target == l2 && fp.after == var_read && fp.branch.is_none()),
            "expected last-use Free for l2; got: {:?}",
            sidecar.free_schedule
        );

        // No dead-store Free for l1 — it's covered by free_on_reassign.
        assert!(
            !sidecar.free_schedule.iter().any(|fp| fp.target == l1),
            "l1 must not be in free_schedule (it's in free_on_reassign): {:?}",
            sidecar.free_schedule
        );
    }

    #[test]
    fn concat_intermediate_freed_after_consumer() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        use std::collections::HashSet;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let print = pool.intern_str("print");
        let a = pool.intern_str("a");
        let b = pool.intern_str("b");
        let span = SimpleSpan::new((), 0..0);

        // print("a" + "b")
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let la = tb.str_const(a, str_ty, span);
        let lb = tb.str_const(b, str_ty, span);
        let cat = tb.binary(TirTag::StrConcat, str_ty, la, lb, span);
        let call = tb.call(print, &[cat], &all_borrow(&[cat]), void, span);
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[stmt]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");
        assert!(sink.is_empty());

        // Three Frees: la, lb, cat. Anchored after consumers (la/lb on
        // cat, cat on call). Order-independent.
        let targets: HashSet<TirRef> = sidecar.free_schedule.iter().map(|fp| fp.target).collect();
        assert!(targets.contains(&la), "expected la in free_schedule");
        assert!(targets.contains(&lb), "expected lb in free_schedule");
        assert!(targets.contains(&cat), "expected cat in free_schedule");
        assert_eq!(sidecar.free_schedule.len(), 3);
    }

    #[test]
    fn last_use_uses_pre_rebind_owner_not_post() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let n = pool.intern_str("n");
        let alice = pool.intern_str("Alice");
        let bob = pool.intern_str("Bob");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        // fn main():
        //     mut n: str = "Alice"
        //     print(n)        # last-use of "Alice"
        //     n = "Bob"
        //     print(n)        # last-use of "Bob"
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let alice_lit = tb.str_const(alice, str_ty, span);
        let decl = tb.var_decl(n, true, str_ty, alice_lit, span);
        let read1 = tb.var(n, str_ty, span);
        let call1 = tb.call(print, &[read1], &all_borrow(&[read1]), void, span);
        let stmt1 = tb.unary(TirTag::ExprStmt, void, call1, span);
        let bob_lit = tb.str_const(bob, str_ty, span);
        let assign = tb.assign(n, str_ty, bob_lit, span);
        let read2 = tb.var(n, str_ty, span);
        let call2 = tb.call(print, &[read2], &all_borrow(&[read2]), void, span);
        let stmt2 = tb.unary(TirTag::ExprStmt, void, call2, span);
        let tir = tb.finish(&[decl, stmt1, assign, stmt2]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");
        assert!(sink.is_empty(), "expected no diagnostics");

        // The Free for "Alice" must come from free_on_reassign[assign],
        // NOT from last-use scheduling. Last-use should target "Bob"
        // (anchored after read2), not "Alice".
        assert_eq!(
            sidecar.free_on_reassign.get(&assign),
            Some(&alice_lit),
            "expected free_on_reassign[assign] = alice_lit"
        );
        // free_schedule must not contain a FreePoint with target=alice_lit
        // anchored at read1 (the bug's signature was wrong-target via post-rebind current_owner).
        assert!(
            !sidecar
                .free_schedule
                .iter()
                .any(|fp| fp.after == read1 && fp.target == alice_lit),
            "expected no last-use Free for Alice anchored at read1 (Alice freed via free_on_reassign): {:?}",
            sidecar.free_schedule
        );
        // Last-use Free for Bob must exist anchored at read2.
        assert!(
            sidecar
                .free_schedule
                .iter()
                .any(|fp| fp.after == read2 && fp.target == bob_lit && fp.branch.is_none()),
            "expected last-use Free for Bob anchored at read2; got: {:?}",
            sidecar.free_schedule
        );
    }

    #[test]
    fn str_const_walk_no_panic() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void: <str_const "hello"> as expr_stmt
        let mut b = TirBuilder::new(main, vec![], void, span);
        let s = b.str_const(hello, str_ty, span);
        let stmt = b.unary(TirTag::ExprStmt, void, s, span);
        let tir = b.finish(&[stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(sink.is_empty());
    }

    #[test]
    fn branch_ids_unique_across_post_loop_if() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let print_name = pool.intern_str("print");
        let lit_a = pool.intern_str("a");
        let lit_b = pool.intern_str("b");
        let span = SimpleSpan::new((), 0..0);

        // fn main() -> void:
        //     while false:
        //         if true:
        //             print("a")
        //     if true:
        //         print("b")
        //
        // The post-loop `if` must not reuse BranchIds that the
        // inside-loop `if` already minted. Today this test may pass
        // vacuously because M8.1's print-of-StrConst doesn't produce
        // branch-gated Frees, so `free_schedule` may have no
        // `Some(BranchId)` entries to inspect. The strong regression
        // for Bug 4 lives in
        // `branch_ids_do_not_collide_after_loop` in
        // `tests/integration_tests.rs`.
        let mut tb = TirBuilder::new(main, vec![], void, span);

        let cond_w = tb.bool_const(false, bool_ty, span);
        let cond_i1 = tb.bool_const(true, bool_ty, span);
        let s_a = tb.str_const(lit_a, str_ty, span);
        let print_a = tb.call(print_name, &[s_a], &all_borrow(&[s_a]), void, span);
        let if_inside = tb.if_stmt(cond_i1, &[print_a], &[], None, void, span);
        let wl = tb.while_loop(cond_w, &[if_inside], void, span);

        let cond_i2 = tb.bool_const(true, bool_ty, span);
        let s_b = tb.str_const(lit_b, str_ty, span);
        let print_b = tb.call(print_name, &[s_b], &all_borrow(&[s_b]), void, span);
        let if_post = tb.if_stmt(cond_i2, &[print_b], &[], None, void, span);

        let tir = tb.finish(&[wl, if_post]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        let max = sidecar
            .free_schedule
            .iter()
            .filter_map(|fp| fp.branch.map(|b| b.0))
            .max();
        if let Some(m) = max {
            assert!(
                m >= 2,
                "post-loop branch reused an inside-loop BranchId; max id = {m}, schedule = {:?}",
                sidecar.free_schedule
            );
        }
        // If no branch-gated frees were scheduled, the test passes
        // vacuously — the integration test
        // `branch_ids_do_not_collide_after_loop` is the stronger
        // guarantee.
    }

    #[test]
    fn merge_branches_takes_max_next_branch_id() {
        // Direct regression for Bug 4 in M8.1c. merge_two clones the
        // pre-loop entry; merge_branches must monotonically advance
        // next_branch_id so that BranchIds minted inside a loop body
        // are not reused by post-loop ifs.
        let mut entry = Ownership {
            next_branch_id: 0,
            ..Ownership::default()
        };

        // inside-loop minted ids 0..=4
        let after_body = Ownership {
            next_branch_id: 5,
            ..Ownership::default()
        };

        entry.merge_branches(&[&entry.clone(), &after_body]);

        assert_eq!(
            entry.next_branch_id, 5,
            "merge_branches must take the max of branch next_branch_id values"
        );
    }

    #[test]
    fn merge_branches_keeps_self_when_self_is_higher() {
        // Symmetry check: max() also wins when self already has the
        // higher value (a branch that didn't allocate any BranchIds
        // shouldn't roll the allocator backward).
        let mut entry = Ownership {
            next_branch_id: 7,
            ..Ownership::default()
        };

        let other = Ownership::default(); // next_branch_id = 0

        entry.merge_branches(&[&other, &other]);

        assert_eq!(
            entry.next_branch_id, 7,
            "merge_branches must not roll next_branch_id backward"
        );
    }

    #[test]
    fn borrowed_param_resolves_under_owner_param_as_borrowed() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let read = pool.intern_str("read"); // fn read(s: str) -> void  (borrowed param)
        let s_name = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            read,
            vec![TirParam {
                name: s_name,
                ty: str_ty,
                mode: ParamMode::Borrow,
                span,
            }],
            void,
            span,
        );
        let v = tb.var(s_name, str_ty, span);
        let tir = tb.finish(&[v]);
        let mut sink = DiagSink::new();
        // check() initialises the param lattice under Owner::Param(s_name) as
        // Borrowed. Assert that reading the borrowed param does NOT trip E0020
        // — the Owner::Param + Borrowed init is load-bearing for the enum migration.
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.into_diags()
                .iter()
                .all(|d| !matches!(d.code, DiagCode::UseAfterMove)),
            "borrowed param read must not trip UAM; the Owner::Param+Borrowed init is load-bearing"
        );
    }

    #[test]
    fn unconsumed_move_param_schedules_free_at_function_end() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let consume = pool.intern_str("consume"); // fn consume(move s: str) -> void
        let s_name = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            consume,
            vec![TirParam {
                name: s_name,
                ty: str_ty,
                mode: ParamMode::Move,
                span,
            }],
            void,
            span,
        );
        // Just return, do not consume `s`.
        let ret = tb.return_void(void, span);
        let tir = tb.finish(&[ret]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = sidecar.functions.remove(&consume).expect("sidecar");

        let virtual_ref = TirRef::param(0);
        let fp = sc
            .free_schedule
            .iter()
            .find(|fp| fp.target == virtual_ref)
            .expect("free scheduled");
        assert_eq!(fp.after, ret);
    }

    #[test]
    fn conditional_move_param_schedules_branch_gated_free() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let consume_cond = pool.intern_str("consume_cond");
        let s_name = pool.intern_str("s");
        let cond_name = pool.intern_str("cond");
        let take = pool.intern_str("take");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(
            consume_cond,
            vec![
                TirParam {
                    name: s_name,
                    ty: str_ty,
                    mode: ParamMode::Move,
                    span,
                },
                TirParam {
                    name: cond_name,
                    ty: bool_ty,
                    mode: ParamMode::Borrow,
                    span,
                },
            ],
            void,
            span,
        );

        let cond_val = tb.var(cond_name, bool_ty, span);
        let s_val_then = tb.var(s_name, str_ty, span);
        let call_then = tb.call(take, &[s_val_then], &[ParamMode::Move], void, span);

        let s_val_else = tb.var(s_name, str_ty, span);
        let call_else = tb.call(take, &[s_val_else], &[ParamMode::Borrow], void, span);

        let if_stmt = tb.if_stmt(cond_val, &[call_then], &[], Some(&[call_else]), void, span);

        let tir = tb.finish(&[if_stmt]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = sidecar.functions.remove(&consume_cond).expect("sidecar");

        let virtual_ref = TirRef::param(0);
        // We expect a branch-gated free for the parameter in the else branch!
        let fp = sc
            .free_schedule
            .iter()
            .find(|fp| fp.target == virtual_ref && fp.branch.is_some())
            .expect("branch-gated free scheduled");
        assert_eq!(fp.after, call_else);
    }

    #[test]
    fn rebind_then_reassign_does_not_double_free() {
        // s = "a"   (temp_a bound to s)
        // s = "b"   (reassign: free_on_reassign covers temp_a; s -> temp_b)
        // At exit, temp_a is Valid (resurrected by rebind) and NOT in
        // current_owner.values() (s moved to temp_b). Without `named_inits`
        // containing temp_a, the anon-temp pass would schedule a second
        // Free for it -> double-free. It must be classified.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s_name = pool.intern_str("s");
        let a = pool.intern_str("a");
        let b = pool.intern_str("b");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_a = tb.str_const(a, str_ty, span);
        let decl = tb.var_decl(s_name, true, str_ty, lit_a, span);
        let lit_b = tb.str_const(b, str_ty, span);
        let assign = tb.assign(s_name, str_ty, lit_b, span);
        let tir = tb.finish(&[decl, assign]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = sidecar.functions.remove(&main).expect("sidecar");

        // temp_a (lit_a) is released via `free_on_reassign` (codegen lowers
        // its destructor at the Assign), so it must NOT also appear in
        // `free_schedule` — that would be a double-free. The anon-temp pass
        // skips it because it is a named init (the VarDecl initializer);
        // if that filter were missing the anon-temp pass would schedule a
        // second Free here. See I-060 and the sibling invariant in
        // `reassignment_records_free_on_old_owner`.
        let a_frees = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == lit_a)
            .count();
        assert_eq!(
            a_frees, 0,
            "temp_a must not be in free_schedule (it's in free_on_reassign); got {sc:?}"
        );
        // Positive half: temp_a must actually be recorded in
        // free_on_reassign (mirrors `reassignment_records_free_on_old_owner`).
        // Without this the test would pass on a leak (temp_a never freed).
        assert_eq!(
            sc.free_on_reassign.get(&assign),
            Some(&lit_a),
            "temp_a must be freed once via free_on_reassign (not leaked); got {sc:?}",
        );
        // lit_b is never read, so it is a dead store and is freed exactly
        // once via the dead-store pass (anchored at the Assign).
        let b_frees = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == lit_b)
            .count();
        assert_eq!(
            b_frees, 1,
            "temp_b freed exactly once via dead-store; got {sc:?}"
        );
        let _ = assign;
    }

    #[test]
    fn rebind_then_read_no_double_free() {
        // s = "a"; print(s)  -> temp_a backed by s, last-use frees it once.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let a = pool.intern_str("a");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(a, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let v = tb.var(s, str_ty, span);
        let call = tb.call(print, &[v], &all_borrow(&[v]), void, span);
        let tir = tb.finish(&[decl, call]);
        let mut sink = DiagSink::new();
        let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = sc.functions.remove(&main).unwrap();
        let lit_frees = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == lit)
            .count();
        assert_eq!(
            lit_frees, 1,
            "backed temp freed exactly once via last-use; got {sc:?}"
        );
    }

    #[test]
    fn rebind_in_loop_converges_no_spurious_free() {
        // s = "a"; while c: s = "a"   (rebind each iteration)
        //
        // The loop-body temp `body_lit` is a named init (the Assign's
        // value), so the anon-temp pass must SKIP it (I-060 static
        // classifier) and let the dead-store pass own its single Free.
        // The rejected dynamic `current_owner.values()` classifier would
        // NOT skip it: at the loop merge the entry-state owner (lit) wins
        // via first-write-wins, so body_lit drops out of
        // current_owner.values() and the anon-temp pass schedules a
        // spurious second Free -> double-free. Assert body_lit is
        // scheduled exactly once (static) rather than twice (dynamic).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let bool_ty = pool.bool_();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let a = pool.intern_str("a");
        let c = pool.intern_str("c");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(a, str_ty, span);
        let decl = tb.var_decl(s, true, str_ty, lit, span);
        let cond = tb.var(c, bool_ty, span);
        let body_lit = tb.str_const(a, str_ty, span);
        let body_assign = tb.assign(s, str_ty, body_lit, span);
        let lp = tb.while_loop(cond, &[body_assign], void, span);
        let tir = tb.finish(&[decl, lp]);
        let mut sink = DiagSink::new();
        let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = sc.functions.remove(&main).unwrap();
        // body_lit must be scheduled exactly once (dead-store pass owns
        // it; anon-temp skips it as a named init). The dynamic
        // current_owner.values() classifier would schedule it twice
        // (anon-temp + dead-store) -> the double-free this test guards.
        let body_lit_frees = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == body_lit)
            .count();
        assert_eq!(
            body_lit_frees, 1,
            "loop-rebound temp must be freed exactly once (static classifier skips it in anon-temp); got {sc:?}"
        );
        // lit (the decl init) is freed via free_on_reassign at the
        // loop-body Assign, so it must NOT also appear in free_schedule.
        let lit_frees = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == lit)
            .count();
        assert_eq!(
            lit_frees, 0,
            "decl-init temp must not be in free_schedule (it's in free_on_reassign); got {sc:?}"
        );
        let _ = (body_assign, lp);
    }

    #[test]
    fn double_consume_reports_uam_once_via_consume_authority() {
        // x = "v"; take(x); take(x)  -- the second take consumes an
        // already-moved binding. Pre-I-050 the Var arm emitted E0020;
        // post-I-050 the consume site (consume_underlying) is the authority
        // and must emit exactly once.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let x = pool.intern_str("x");
        let take = pool.intern_str("take");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
        let decl = tb.var_decl(x, false, str_ty, lit, span);
        let xv1 = tb.var(x, str_ty, span);
        let take1 = tb.call(take, &[xv1], &[ParamMode::Move], void, span);
        let xv2 = tb.var(x, str_ty, span);
        let take2 = tb.call(take, &[xv2], &[ParamMode::Move], void, span);
        let tir = tb.finish(&[decl, take1, take2]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let uam = sink
            .into_diags()
            .iter()
            .filter(|d| matches!(d.code, DiagCode::UseAfterMove))
            .count();
        assert_eq!(uam, 1, "double-consume reports UAM exactly once; got {uam}");
    }

    #[test]
    fn borrow_of_moved_value_still_reports_uam() {
        // print(moved_x) — borrow of a moved value, no consume. After the
        // Var arm is demoted, the borrow-arg check_use_moved must still fire.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let x = pool.intern_str("x");
        let take = pool.intern_str("take");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
        let decl = tb.var_decl(x, false, str_ty, lit, span);
        let xv = tb.var(x, str_ty, span);
        let take_call = tb.call(take, &[xv], &[ParamMode::Move], void, span); // moves x
        let xv2 = tb.var(x, str_ty, span);
        let print_call = tb.call(print, &[xv2], &[ParamMode::Borrow], void, span); // borrow of moved
        let tir = tb.finish(&[decl, take_call, print_call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.code, DiagCode::UseAfterMove)),
            "borrow of moved value must still report E0020 via borrow-arg path; got {diags:?}"
        );
    }

    #[test]
    fn three_arm_if_with_conditional_move_and_loop_rebind() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        use std::collections::HashSet;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let x = pool.intern_str("x");
        let s = pool.intern_str("s");
        let take = pool.intern_str("take");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_v = tb.str_const(pool.intern_str("v"), str_ty, span);
        let decl_x = tb.var_decl(x, false, str_ty, lit_v, span);

        let lit_mut = tb.str_const(pool.intern_str("initial"), str_ty, span);
        let decl_mut = tb.var_decl(s, true, str_ty, lit_mut, span);

        let cond_then = tb.bool_const(true, bool_ty, span);
        let read_then = tb.var(x, str_ty, span);
        let call_then = tb.call(print, &[read_then], &[ParamMode::Borrow], void, span);

        let cond_elif = tb.bool_const(false, bool_ty, span);
        let read_elif = tb.var(x, str_ty, span);
        let call_elif = tb.call(take, &[read_elif], &[ParamMode::Move], void, span);

        let read_else = tb.var(x, str_ty, span);
        let call_else = tb.call(print, &[read_else], &[ParamMode::Borrow], void, span);

        let if_stmt = tb.if_stmt(
            cond_then,
            &[call_then],
            &[(cond_elif, vec![call_elif])],
            Some(&[call_else]),
            void,
            span,
        );

        let cond_w = tb.bool_const(false, bool_ty, span);
        let body_lit = tb.str_const(pool.intern_str("new_val"), str_ty, span);
        let body_assign = tb.assign(s, str_ty, body_lit, span);
        let wl = tb.while_loop(cond_w, &[body_assign], void, span);

        let read_post = tb.var(x, str_ty, span);
        let call_post = tb.call(print, &[read_post], &[ParamMode::Borrow], void, span);

        let tir = tb.finish(&[decl_x, decl_mut, if_stmt, wl, call_post]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = sidecar
            .functions
            .remove(&main)
            .expect("per-function sidecar entry");

        // (a) Assert no next_branch_id collision in sidecar.if_branches
        let mut ids = HashSet::new();
        for ib in sidecar.if_branches.values() {
            assert!(
                ids.insert(ib.then_branch.0),
                "duplicate branch id {}",
                ib.then_branch.0
            );
            for b in &ib.elif_branches {
                assert!(ids.insert(b.0), "duplicate branch id {}", b.0);
            }
            if let Some(b) = ib.else_branch {
                assert!(ids.insert(b.0), "duplicate branch id {}", b.0);
            }
        }

        // (b) Assert a post-if read of the conditionally-moved binding emits exactly one DiagCode::UseAfterMove
        let uam = sink
            .into_diags()
            .iter()
            .filter(|d| matches!(d.code, DiagCode::UseAfterMove))
            .count();
        assert_eq!(
            uam, 1,
            "post-if read of conditionally-moved binding must report UAM exactly once; got {uam}"
        );
    }

    #[test]
    fn move_and_borrow_of_same_owner_in_one_call_e0023_both_orderings() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        use ryo_core::types::TypeId;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let x = pool.intern_str("x");
        let f = pool.intern_str("f");
        let span = SimpleSpan::new((), 0..0);

        fn build(
            tir: &mut TirBuilder,
            x: StringId,
            str_ty: TypeId,
            void: TypeId,
            f: StringId,
            first_move: bool,
            span: Span,
        ) -> (TirRef, TirRef, TirRef) {
            let xv1 = tir.var(x, str_ty, span);
            let xv2 = tir.var(x, str_ty, span);
            let modes = if first_move {
                vec![ParamMode::Move, ParamMode::Borrow]
            } else {
                vec![ParamMode::Borrow, ParamMode::Move]
            };
            let call = tir.call(f, &[xv1, xv2], &modes, void, span);
            (xv1, xv2, call)
        }

        // Ordering 1: Move first, then Borrow
        let mut tb1 = TirBuilder::new(main, vec![], void, span);
        let lit1 = tb1.str_const(pool.intern_str("v"), str_ty, span);
        let decl1 = tb1.var_decl(x, false, str_ty, lit1, span);
        let (_xv1, _xv2, call1) = build(&mut tb1, x, str_ty, void, f, true, span);
        let tir1 = tb1.finish(&[decl1, call1]);

        let mut sink1 = DiagSink::new();
        let _sc1 = check(std::slice::from_ref(&tir1), &pool, &mut sink1);
        let diags1 = sink1.into_diags();
        let e0023_count1 = diags1
            .iter()
            .filter(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall))
            .count();
        assert_eq!(
            e0023_count1, 1,
            "Expected exactly one MoveWhileBorrowedInCall (E0031) in first-move ordering; got {diags1:?}"
        );

        // Ordering 2: Borrow first, then Move
        let mut tb2 = TirBuilder::new(main, vec![], void, span);
        let lit2 = tb2.str_const(pool.intern_str("v"), str_ty, span);
        let decl2 = tb2.var_decl(x, false, str_ty, lit2, span);
        let (_xv3, _xv4, call2) = build(&mut tb2, x, str_ty, void, f, false, span);
        let tir2 = tb2.finish(&[decl2, call2]);

        let mut sink2 = DiagSink::new();
        let _sc2 = check(std::slice::from_ref(&tir2), &pool, &mut sink2);
        let diags2 = sink2.into_diags();
        let e0023_count2 = diags2
            .iter()
            .filter(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall))
            .count();
        assert_eq!(
            e0023_count2, 1,
            "Expected exactly one MoveWhileBorrowedInCall (E0031) in borrow-first ordering; got {diags2:?}"
        );
    }

    #[test]
    fn two_borrows_of_one_owner_ok() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let x = pool.intern_str("x");
        let f = pool.intern_str("f");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
        let decl = tb.var_decl(x, false, str_ty, lit, span);
        let xv1 = tb.var(x, str_ty, span);
        let xv2 = tb.var(x, str_ty, span);
        let modes = vec![ParamMode::Borrow, ParamMode::Borrow];
        let call = tb.call(f, &[xv1, xv2], &modes, void, span);
        let tir = tb.finish(&[decl, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
            "two borrows of one owner is fine (Rule 7 many readers); got {diags:?}"
        );
    }

    #[test]
    fn borrow_and_move_of_distinct_owners_ok() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let a = pool.intern_str("a");
        let b = pool.intern_str("b");
        let f = pool.intern_str("f");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let la = tb.str_const(pool.intern_str("va"), str_ty, span);
        let lb = tb.str_const(pool.intern_str("vb"), str_ty, span);
        let da = tb.var_decl(a, false, str_ty, la, span);
        let db = tb.var_decl(b, false, str_ty, lb, span);
        let av = tb.var(a, str_ty, span);
        let bv = tb.var(b, str_ty, span);
        let modes = vec![ParamMode::Borrow, ParamMode::Move];
        let call = tb.call(f, &[av, bv], &modes, void, span);
        let tir = tb.finish(&[da, db, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            !sink
                .into_diags()
                .iter()
                .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
            "borrow + move of DISTINCT owners is fine"
        );
    }

    #[test]
    fn single_move_arg_no_e0023() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let x = pool.intern_str("x");
        let f = pool.intern_str("f");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
        let decl = tb.var_decl(x, false, str_ty, lit, span);
        let xv = tb.var(x, str_ty, span);
        let call = tb.call(f, &[xv], &[ParamMode::Move], void, span);
        let tir = tb.finish(&[decl, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            !sink
                .into_diags()
                .iter()
                .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
            "a single move arg must not trip E0031"
        );
    }

    #[test]
    fn copy_args_untracked_no_false_positive() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let n = pool.intern_str("n");
        let x = pool.intern_str("x");
        let f = pool.intern_str("f");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let ic = tb.int_const(1, int_ty, span);
        let dn = tb.var_decl(n, false, int_ty, ic, span);
        let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
        let dx = tb.var_decl(x, false, str_ty, lit, span);
        let nv = tb.var(n, int_ty, span);
        let xv = tb.var(x, str_ty, span);
        let modes = vec![ParamMode::Borrow, ParamMode::Move];
        let call = tb.call(f, &[nv, xv], &modes, void, span);
        let tir = tb.finish(&[dn, dx, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            !sink
                .into_diags()
                .iter()
                .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
            "untracked int arg must not trigger a false E0031"
        );
    }

    #[test]
    fn borrow_and_move_in_sequential_statements_ok() {
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let x = pool.intern_str("x");
        let f = pool.intern_str("f");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
        let decl = tb.var_decl(x, false, str_ty, lit, span);
        // first statement: f(borrow x)
        let xv1 = tb.var(x, str_ty, span);
        let call1 = tb.call(f, &[xv1], &[ParamMode::Borrow], void, span);
        // second statement: f(move x)
        let xv2 = tb.var(x, str_ty, span);
        let call2 = tb.call(f, &[xv2], &[ParamMode::Move], void, span);
        let tir = tb.finish(&[decl, call1, call2]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            !sink
                .into_diags()
                .iter()
                .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
            "borrow and move in sequential statements must not trigger E0031"
        );
    }

    #[test]
    fn inout_same_owner_twice_rejected() {
        // swap(&c, &c) — two mutable borrows of one int owner in the same
        // call (Rule 7 case 1). The int args never enter the lattice, so
        // this exercises the name-based inout owner resolution.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let c = pool.intern_str("c");
        let swap = pool.intern_str("swap");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let ic = tb.int_const(1, int_ty, span);
        let decl = tb.var_decl(c, false, int_ty, ic, span);
        let cv1 = tb.var(c, int_ty, span);
        let cv2 = tb.var(c, int_ty, span);
        let call = tb.call(
            swap,
            &[cv1, cv2],
            &[ParamMode::Inout, ParamMode::Inout],
            void,
            span,
        );
        let tir = tb.finish(&[decl, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        let e0032_count = diags
            .iter()
            .filter(|d| matches!(d.code, DiagCode::MutableAliasingViolation))
            .count();
        assert_eq!(
            e0032_count, 1,
            "Expected exactly one MutableAliasingViolation (E0032) for swap(&c, &c); got {diags:?}"
        );
    }

    #[test]
    fn inout_and_borrow_same_owner_rejected() {
        // f(&c, c) — mutable borrow plus immutable borrow of one int owner
        // in the same call (Rule 7 case 2).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let c = pool.intern_str("c");
        let f = pool.intern_str("f");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let ic = tb.int_const(1, int_ty, span);
        let decl = tb.var_decl(c, false, int_ty, ic, span);
        let cv1 = tb.var(c, int_ty, span);
        let cv2 = tb.var(c, int_ty, span);
        let call = tb.call(
            f,
            &[cv1, cv2],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let tir = tb.finish(&[decl, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        let e0032_count = diags
            .iter()
            .filter(|d| matches!(d.code, DiagCode::MutableAliasingViolation))
            .count();
        assert_eq!(
            e0032_count, 1,
            "Expected exactly one MutableAliasingViolation (E0032) for f(&c, c); got {diags:?}"
        );
    }

    #[test]
    fn inout_and_move_same_owner_rejected() {
        // f(&x, move x) — mutable borrow plus move of one str owner in the
        // same call (Rule 7 case 3). The tracked str args exercise the
        // lattice-backed path of the overlap check.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let x = pool.intern_str("x");
        let f = pool.intern_str("f");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
        let decl = tb.var_decl(x, false, str_ty, lit, span);
        let xv1 = tb.var(x, str_ty, span);
        let xv2 = tb.var(x, str_ty, span);
        let call = tb.call(
            f,
            &[xv1, xv2],
            &[ParamMode::Inout, ParamMode::Move],
            void,
            span,
        );
        let tir = tb.finish(&[decl, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        let e0032_count = diags
            .iter()
            .filter(|d| matches!(d.code, DiagCode::MutableAliasingViolation))
            .count();
        assert_eq!(
            e0032_count, 1,
            "Expected exactly one MutableAliasingViolation (E0032) for f(&x, move x); got {diags:?}"
        );
    }

    #[test]
    fn inout_distinct_owners_ok() {
        // swap(&a, &b) — mutable borrows of DISTINCT int owners: no E0032.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let a = pool.intern_str("a");
        let b = pool.intern_str("b");
        let swap = pool.intern_str("swap");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let ia = tb.int_const(1, int_ty, span);
        let ib = tb.int_const(2, int_ty, span);
        let da = tb.var_decl(a, false, int_ty, ia, span);
        let db = tb.var_decl(b, false, int_ty, ib, span);
        let av = tb.var(a, int_ty, span);
        let bv = tb.var(b, int_ty, span);
        let call = tb.call(
            swap,
            &[av, bv],
            &[ParamMode::Inout, ParamMode::Inout],
            void,
            span,
        );
        let tir = tb.finish(&[da, db, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.code, DiagCode::MutableAliasingViolation)),
            "mutable borrows of DISTINCT owners are fine (Rule 7); got {diags:?}"
        );
    }

    #[test]
    fn two_immutable_borrows_ok() {
        // f(c, c) — two immutable borrows of one int owner: no E0032
        // (Rule 7 many readers). Guards the untracked-Borrow recording
        // against false positives.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let c = pool.intern_str("c");
        let f = pool.intern_str("f");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let ic = tb.int_const(1, int_ty, span);
        let decl = tb.var_decl(c, false, int_ty, ic, span);
        let cv1 = tb.var(c, int_ty, span);
        let cv2 = tb.var(c, int_ty, span);
        let call = tb.call(
            f,
            &[cv1, cv2],
            &[ParamMode::Borrow, ParamMode::Borrow],
            void,
            span,
        );
        let tir = tb.finish(&[decl, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.code, DiagCode::MutableAliasingViolation)),
            "two immutable borrows of one owner are fine (Rule 7 many readers); got {diags:?}"
        );
    }

    #[test]
    fn inout_str_param_reassign_escapes_no_dead_store_no_free() {
        // I-112 callee side: `fn set(inout s: str): s = "new"`. The
        // replacement value escapes through the write-back pointer, so the
        // pass must NOT emit W0001 or free the new value; the OLD pointee
        // (the incoming buffer) must be dropped at the reassign.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let set = pool.intern_str("set");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            set,
            vec![TirParam {
                name: s,
                ty: str_ty,
                mode: ParamMode::Inout,
                span,
            }],
            void,
            span,
        );
        let lit = tb.str_const(pool.intern_str("new"), str_ty, span);
        let asg = tb.assign(s, str_ty, lit, span);
        let tir = tb.finish(&[asg]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
            "inout param reassignment must not warn dead-store — the value escapes via write-back; got {diags:?}"
        );
        let sc = sidecar.functions.remove(&set).expect("sidecar");
        assert_eq!(
            sc.free_on_reassign.get(&asg),
            Some(&TirRef::param(0)),
            "reassigning an inout str param must drop the incoming buffer at the reassign; free_on_reassign = {:?}",
            sc.free_on_reassign
        );
        assert!(
            !sc.free_schedule.iter().any(|fp| fp.target == lit),
            "the reassigned value escapes to the caller — no Free may target it; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn inout_str_param_reassign_inside_if_escapes() {
        // I-112: `fn g(inout s: str): if c: s = "b"`. The rebind is
        // branch-divergent — the merge keeps `Param(s)` as the binding's
        // owner and can stamp it Valid — but the bound value still
        // escapes via the write-back: no W0001, no Free for the rebound
        // value or the param, while the taken arm still drops the
        // incoming buffer.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let g = pool.intern_str("g");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            g,
            vec![TirParam {
                name: s,
                ty: str_ty,
                mode: ParamMode::Inout,
                span,
            }],
            void,
            span,
        );
        let cond = tb.bool_const(true, bool_ty, span);
        let lit = tb.str_const(pool.intern_str("b"), str_ty, span);
        let asg = tb.assign(s, str_ty, lit, span);
        let if_s = tb.if_stmt(cond, &[asg], &[], None, void, span);
        let tir = tb.finish(&[if_s]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
            "inout param reassignment escapes — no dead-store warning, even branch-divergent; got {diags:?}"
        );
        let sc = sidecar.functions.remove(&g).expect("sidecar");
        assert!(
            sc.free_on_reassign.contains_key(&asg),
            "the taken arm must drop the incoming buffer; free_on_reassign = {:?}",
            sc.free_on_reassign
        );
        assert!(
            !sc.free_schedule.iter().any(|fp| fp.target == lit),
            "the rebound value escapes to the caller — no Free may target it; schedule = {:?}",
            sc.free_schedule
        );
        assert!(
            !sc.free_schedule
                .iter()
                .any(|fp| fp.target == TirRef::param(0)),
            "the inout param's value escapes — no callee Free for the param owner; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn inout_str_param_move_out_after_reassign_rejected() {
        // I-112: the value bound to an inout str param escapes via the
        // write-back, so moving it out (even after a reassign made it a
        // fresh, Valid owner) must still be an error.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let f = pool.intern_str("f");
        let g = pool.intern_str("g");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            f,
            vec![TirParam {
                name: s,
                ty: str_ty,
                mode: ParamMode::Inout,
                span,
            }],
            void,
            span,
        );
        let lit = tb.str_const(pool.intern_str("new"), str_ty, span);
        let asg = tb.assign(s, str_ty, lit, span);
        let sv = tb.var(s, str_ty, span);
        let call = tb.call(g, &[sv], &[ParamMode::Move], void, span);
        let tir = tb.finish(&[asg, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.code, DiagCode::MoveOutOfBorrowedParam)),
            "moving out of an inout param after reassign must be E0021; got {diags:?}"
        );
    }

    #[test]
    fn inout_str_param_return_after_reassign_rejected() {
        // I-112: returning the value currently bound to an inout str param
        // double-owns it (it also escapes via the write-back) — E0022.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirData, TirParam, TirTag};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let f = pool.intern_str("f");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            f,
            vec![TirParam {
                name: s,
                ty: str_ty,
                mode: ParamMode::Inout,
                span,
            }],
            str_ty,
            span,
        );
        let lit = tb.str_const(pool.intern_str("new"), str_ty, span);
        let asg = tb.assign(s, str_ty, lit, span);
        let sv = tb.var(s, str_ty, span);
        let ret = tb.push_typed(TirTag::Return, TirData::UnOp(sv), str_ty, span);
        let _ = void;
        let tir = tb.finish(&[asg, ret]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.code, DiagCode::ReturnBorrowedValue)),
            "returning an inout param's bound value must be E0022; got {diags:?}"
        );
    }

    #[test]
    fn inout_call_keeps_owner_and_frees_current_buffer() {
        // I-112 caller side: `mut s = "hi"; set(&s); print(s)`. An inout
        // call is a pure borrow of the slot: the binding KEEPS its
        // pre-call owner (no reseat, no Moved, no dead-store churn), and
        // the freshness of the freed buffer is codegen's job — it emits
        // the Free from the binding's current `StrLocals` (which hold the
        // write-back triple after the reload), never the stale pre-call
        // repr. Assert the lattice invariants: owner unchanged, exactly
        // one Free for it, no diagnostics.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let set = pool.intern_str("set");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("hi"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let sv1 = tb.var(s, str_ty, span);
        let call = tb.call(set, &[sv1], &[ParamMode::Inout], void, span);
        let sv2 = tb.var(s, str_ty, span);
        let pr = tb.call(print, &[sv2], &[ParamMode::Borrow], void, span);
        let tir = tb.finish(&[decl, call, pr]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            diags.is_empty(),
            "inout call on a str local must produce no diagnostics; got {diags:?}"
        );
        let sc = sidecar.functions.remove(&main).expect("sidecar");
        let frees: Vec<_> = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == lit)
            .collect();
        assert_eq!(
            frees.len(),
            1,
            "exactly one Free for the binding's owner; codegen emits it from the current StrLocals; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn inout_call_inside_if_no_false_dead_store() {
        // I-112 follow-up: `mut s = "a"; if c: set(&s); print(s)` — the
        // reseat happens inside the if-arm; the post-if read must clear
        // the dead-store entry across the branch merge.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let set = pool.intern_str("set");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sv1 = tb.var(s, str_ty, span);
        let call = tb.call(set, &[sv1], &[ParamMode::Inout], void, span);
        let if_s = tb.if_stmt(cond, &[call], &[], None, void, span);
        let sv2 = tb.var(s, str_ty, span);
        let pr = tb.call(print, &[sv2], &[ParamMode::Borrow], void, span);
        let tir = tb.finish(&[decl, if_s, pr]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
            "s is read after the if — no dead-store warning may survive the merge; got {diags:?}"
        );
    }

    #[test]
    fn inout_call_inside_loop_no_false_dead_store() {
        // I-112 follow-up: `mut s = ""; while i < 3: str_push(&s, "x") ...
        // print(s)` — same merge concern through the loop fixed point.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let set = pool.intern_str("set");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str(""), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sv1 = tb.var(s, str_ty, span);
        let call = tb.call(set, &[sv1], &[ParamMode::Inout], void, span);
        let wl = tb.while_loop(cond, &[call], void, span);
        let sv2 = tb.var(s, str_ty, span);
        let pr = tb.call(print, &[sv2], &[ParamMode::Borrow], void, span);
        let tir = tb.finish(&[decl, wl, pr]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
            "s is read after the loop — no dead-store warning may survive the loop merge; got {diags:?}"
        );
        let sc = sidecar.functions.remove(&main).expect("sidecar");
        assert_eq!(
            sc.free_schedule
                .iter()
                .filter(|fp| fp.target == lit)
                .count(),
            1,
            "exactly one Free for the binding's owner after the loop; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn reassign_inside_if_still_frees_binding_at_last_use() {
        // Pre-existing M8.1 bug exposed by I-112: `mut s = "a"; if c:
        // s = "b"; print(s)`. The merge keeps the pre-branch owner, so the
        // reassign-target guard must not skip its last-use Free — on the
        // not-taken path the binding still owns `lit_a`. Codegen emits the
        // Free from the binding's current StrLocals (path-correct buffer).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit_a, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
        let asg = tb.assign(s, str_ty, lit_b, span);
        let if_s = tb.if_stmt(cond, &[asg], &[], None, void, span);
        let sv = tb.var(s, str_ty, span);
        let pr = tb.call(print, &[sv], &[ParamMode::Borrow], void, span);
        let tir = tb.finish(&[decl, if_s, pr]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
            "s is read after the if — no dead-store warning; got {diags:?}"
        );
        let sc = sidecar.functions.remove(&main).expect("sidecar");
        assert!(
            sc.free_on_reassign.contains_key(&asg),
            "the taken arm must drop the pre-reassign buffer; free_on_reassign = {:?}",
            sc.free_on_reassign
        );
        assert_eq!(
            sc.free_schedule
                .iter()
                .filter(|fp| fp.target == lit_a)
                .count(),
            1,
            "the binding's owner must still get its last-use Free (covers the not-taken path); schedule = {:?}",
            sc.free_schedule
        );
        assert!(
            !sc.free_schedule.iter().any(|fp| fp.target == lit_b),
            "lit_b's buffer is freed through the binding's current StrLocals (same buffer as lit_a's Free on the taken path) — a second Free would double-free; schedule = {:?}",
            sc.free_schedule
        );
    }
}
