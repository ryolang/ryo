//! Ownership pass — validates move safety on per-`TirRef` lattice.
//!
//! Runs between sema and codegen. Walks each `Tir` forward, tracking
//! ownership state for every Move-typed value. Catches use-after-move,
//! moves out of borrowed parameters, and returns of borrowed values.
//! M8.4 adds slice-projection tracking (final spec §3.2/§3.3): bound
//! `strview` views register against their root owner (P3), an owner with
//! a live projection is frozen against moves and mutation (P2),
//! projections end at their last use (P4), destruction defers to the
//! last projection use (P5), and views cannot escape (E1/E2). Emits
//! diagnostics into the shared `DiagSink` — does not mutate TIR and
//! does not insert Free instructions (that lands in M8.1c).
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

use crate::builtins::{is_borrowed_scalar_param, view_borrow_params};
use ryo_core::diag::{Diag, DiagCode, DiagSink};

mod diag_fmt;
pub(crate) use diag_fmt::*;
mod frees;
pub(crate) use frees::*;
mod loops;
pub(crate) use loops::*;
mod merge;
pub(crate) use merge::*;
mod views;
pub use ryo_core::ownership::{
    BranchId, ConditionalDeadDrop, FreePoint, FunctionSidecar, IfBranchIds, OwnershipSidecar,
};
use ryo_core::tir::{ParamMode, Span, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeId, TypeKind};
use std::collections::{HashMap, HashSet};
pub(crate) use views::*;
mod walk;
pub(crate) use walk::*;

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

    pub(crate) fn tirref(self, param_index: &HashMap<StringId, usize>) -> TirRef {
        match self {
            Owner::Inst(r) => r,
            Owner::Param(name) => TirRef::param(*param_index.get(&name).expect("param exists")),
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
    /// Param name → index into `tir.params`, built once per function
    /// in `analyze_function`. Resolving a `Param` owner to its virtual
    /// `TirRef`, type, or span happens inside per-owner loops, so a
    /// linear scan of `tir.params` per lookup would be O(P) each time.
    /// Every `Owner::Param` name originates from `tir.params` by
    /// construction, so a missing key is an internal invariant
    /// violation (`expect` at the lookup sites), not a diagnostic.
    pub param_index: HashMap<StringId, usize>,
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
    /// anon-temp pass (classified statically via `collect_named_inits`)
    /// — it is freed via the last-use / dead-store /
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

    /// Conditional reseats observed while walking if/elif/else arms:
    /// bindings that SOME arm reseated while other arms kept
    /// the pre-branch owner. Monotone-accumulating (like
    /// `owner_at_read`) — loop convergence re-walks are deduped at push
    /// time. Consumed by the dead-store drain, which converts the
    /// matching entries into arm-gated `ConditionalDeadDrop`s so the
    /// pre-branch buffer is also freed on the untouched paths.
    pub reseat_drops: Vec<ReseatDrop>,

    /// Owners still `Valid` at each `Return`/`ReturnVoid`, snapshotted
    /// mid-walk while the lattice state is path-correct for that exit
    /// point. The function must destroy those values on that path —
    /// they are dead at the return, but the last-use / temp / drain
    /// passes (which run at function exit) anchor their Frees on OTHER
    /// paths or program points the early return never reaches.
    /// Monotone-accumulating; loop convergence re-walks may record the
    /// same return twice — deduped at scheduling time.
    pub return_epilogue: Vec<(TirRef, Vec<Owner>)>,

    /// P3 (final spec §3.2): each bound view → the root owner it
    /// projects (re-slices resolve transitively to the original
    /// owner). Monotone (insert-only): a view's root never changes,
    /// and branch-scope-dropped views keep their entry so the P5
    /// deferral on the root survives the merge. Merges first-wins,
    /// mirroring `origin`. Sparse keys, like `states`.
    pub root_owner: HashMap<Owner, Owner>,

    /// P2 freeze ranges (final spec §3.2): root owner → its currently
    /// live view bindings. A view is registered when bound (a
    /// `strview`-typed `VarDecl`/`Assign`) and removed when its
    /// projection ends (P4: at its last read, at a rebind that kills
    /// it, at a loop exit for loop-deferred reads, or at a branch
    /// join for branch-local deaths). Consume/mutate sites of the
    /// owner consult this set. `Vec` values are in registration
    /// (walk) order, which keeps the freeze note's span choice
    /// deterministic.
    pub live_projections: HashMap<Owner, Vec<Owner>>,

    /// Walk-constant pre-pass liveness (P4): bound view instruction →
    /// its last reading instruction. Views with no entry are never
    /// read — their projection lives to scope end. Constant per
    /// function (computed before the walk), so branch merges need no
    /// per-field rule for it. `analyze_if_stmt` temporarily refines
    /// entries per arm (see `if_arm_last_reads`) and restores them at
    /// each arm's end.
    pub view_last_use: HashMap<TirRef, TirRef>,

    /// Walk-constant pre-pass liveness (P4 per-arm refinement): if
    /// stmt → per-arm (view instruction → its last read within that
    /// arm's subtree), in walk order [then, elif..., else]. Consulted
    /// by `analyze_if_stmt`; constant per function, so branch merges
    /// need no per-field rule for it.
    pub if_arm_last_reads: HashMap<TirRef, Vec<HashMap<TirRef, TirRef>>>,

    /// Walk-constant pre-pass liveness (P4): view instruction → the
    /// loop at whose exit the projection dies. A view whose last read
    /// sits inside a loop its creation is outside of re-executes on
    /// later iterations, so it stays live through the whole loop.
    pub view_defer_loop: HashMap<TirRef, TirRef>,

    /// Walk-constant pre-pass structure: instruction → the
    /// `WhileLoop`/`ForRange` instructions whose body, condition, or
    /// bounds contain it, outermost first. Computed in one traversal
    /// before the walk so the liveness passes and the
    /// redundant-materialize pass look nesting up instead of
    /// re-walking the body per query. Constant per function, so
    /// branch merges need no per-field rule for it.
    pub loop_nesting: HashMap<TirRef, Vec<TirRef>>,

    /// Views whose projection ends at the current statement's end
    /// (P4). Drained by `analyze_stmt` after every statement, so a
    /// read and a consume within the same statement both see the view
    /// as live (borrow-for-the-whole-statement semantics, matching
    /// Rule 7).
    pub pending_dying: Vec<Owner>,

    /// W0003 case-B support (M8.4.1.2): every move, mutation
    /// (reassign), or `inout` pass of a tracked owner the walk
    /// observed, as `(owner, site)` pairs. Monotone-accumulating like
    /// `reseat_drops` — loop convergence re-walks may push duplicates
    /// (queries are `any()`-shaped, so no dedup is needed). Read by
    /// the post-walk redundant-materialize pass to classify escapes of
    /// the copy and defensive-copy hazards on the view's root owner.
    pub owner_hazards: Vec<(Owner, TirRef)>,
}

/// One conditional-reseat observation, recorded by
/// `analyze_if_stmt` after walking an if's arms. `reseat_owners` is the
/// set of owners the binding was reseated TO across the arms;
/// `untouched_arms` are the arms (by [`BranchId`]) that kept the
/// pre-branch owner — including the synthetic fall-through arm of an
/// else-less if.
#[derive(Clone, Debug)]
pub(crate) struct ReseatDrop {
    pub if_stmt: TirRef,
    pub name: StringId,
    pub pre_owner: Owner,
    pub reseat_owners: HashSet<Owner>,
    pub untouched_arms: Vec<BranchId>,
}

/// Validate move safety for every function body. Emits diagnostics
/// into `sink`. Returns an [`OwnershipSidecar`] that codegen consults
/// to decide where to emit `ryo_str_free` calls. The TIR itself is
/// never mutated. The sidecar is positional with `tirs`: entry `i`
/// belongs to `tirs[i]`.
pub fn check(tirs: &[Tir], pool: &InternPool, sink: &mut DiagSink) -> OwnershipSidecar {
    let mut sidecar = OwnershipSidecar::default();
    for tir in tirs {
        let mut func_sidecar = FunctionSidecar::new(tir.name);
        analyze_function(tir, pool, sink, &mut func_sidecar);
        sidecar.functions.push(func_sidecar);
    }
    sidecar
}

fn analyze_function(
    tir: &Tir,
    pool: &InternPool,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
) {
    let mut own = Ownership {
        // Name → param-index map, built once so the per-owner lookups
        // below (Param owner → TirRef / type / span) are O(1) instead
        // of a linear scan of `tir.params` per call.
        param_index: tir
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name, i))
            .collect(),
        ..Ownership::default()
    };

    // M8.4: view-liveness pre-pass (P4, final spec §3.2). The walk
    // consults these walk-constant tables to know when it passes a
    // projection's last use (and which loop exit defers it). The
    // nesting map the deferral table is derived from is computed
    // first, so the walk's per-arm refinement and the
    // redundant-materialize pass reuse the same table.
    own.loop_nesting = collect_loop_nesting(tir);
    let liveness = collect_view_liveness(tir, pool, &own.loop_nesting);
    own.view_last_use = liveness.last_use;
    own.view_defer_loop = liveness.defer_to_loop;
    own.if_arm_last_reads = liveness.arm_last_reads;

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
    // P5 (final spec §3.2): root owner → every view that ever
    // projected it (sorted for deterministic iteration).
    let order = program_order(tir);
    let mut projections_of: HashMap<Owner, Vec<TirRef>> = HashMap::new();
    for (view, root) in &own.root_owner {
        if let Some(vi) = view.inst_tirref() {
            projections_of.entry(*root).or_default().push(vi);
        }
    }
    for views in projections_of.values_mut() {
        views.sort_by_key(|v| v.raw());
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
    // current `StrLocals`, which is the path-correct buffer.
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
    // Iterate owners in a sorted order so `free_schedule` push
    // order does not depend on HashMap iteration order.
    let mut sorted_states: Vec<(Owner, OwnerState)> =
        own.states.iter().map(|(o, s)| (*o, s.clone())).collect();
    sorted_states.sort_by_key(|(o, _)| owner_sort_key(o));
    for (owner, state) in &sorted_states {
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
                    // P5 (final spec §3.2): defer the destruction to
                    // the last use of any projection of this owner.
                    let after = defer_anchor(after, owner, &projections_of, &last_use, &order);
                    // Conditional last use: a named binding whose LAST
                    // READ is inside a branch is freed at the branch's
                    // exit — the earliest point where the value is dead
                    // on ALL paths. Anchoring after the read itself
                    // fires per-iteration in loops (UAF on later reads)
                    // and never fires on not-taken arms (leak). Skip
                    // the re-anchor when the branch may `return` (the
                    // exit anchor is unreachable on the return path)
                    // and for temps / branch-local bindings (their
                    // values don't exist on every exit path).
                    let anchor = match outermost_branch_of(tir, after) {
                        Some(branch_stmt)
                            if branch_may_not_return(tir, branch_stmt)
                                && owner_binding_name(tir, *r).is_some_and(|name| {
                                    declared_before_stmt(tir, name, branch_stmt)
                                }) =>
                        {
                            branch_stmt
                        }
                        _ => after,
                    };
                    sidecar.free_schedule.push(FreePoint {
                        after: anchor,
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
                let idx = *own.param_index.get(name).expect("param exists");
                // Anchor the Free after the param's last read — the
                // same policy locals get — so later statements that
                // never touch the param don't keep its buffer alive.
                // A never-read param keeps the old anchor (after the
                // last body statement): it must still be freed exactly
                // once.
                let Some(after) = (match last_use.get(&TirRef::param(idx)) {
                    Some(&after) => {
                        // P5 (final spec §3.2): defer the destruction to
                        // the last use of any projection of this param
                        // (a slice of it keeps the buffer alive).
                        let after = defer_anchor(after, owner, &projections_of, &last_use, &order);
                        // Conditional last use: same re-anchor as `Inst`
                        // owners — a last read inside a branch frees at
                        // the branch's exit. Anchoring after the read
                        // itself fires per-iteration in loops (UAF on
                        // later reads) and never fires on not-taken
                        // arms (leak). Skip when the branch may
                        // `return` (the exit anchor is unreachable on
                        // the return path). The declared-before check
                        // locals need is trivially true here: params
                        // precede the body.
                        match outermost_branch_of(tir, after) {
                            Some(branch_stmt) if branch_may_not_return(tir, branch_stmt) => {
                                Some(branch_stmt)
                            }
                            _ => Some(after),
                        }
                    }
                    None => body_stmts.last().copied(),
                }) else {
                    continue;
                };
                sidecar.free_schedule.push(FreePoint {
                    after,
                    target: TirRef::param(idx),
                    span: tir.params[idx].span,
                    branch: None,
                });
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
    // `collect_named_inits`. The old implementation carried a
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
    let mut sorted_temps: Vec<Owner> = own.temp_owners.iter().copied().collect();
    sorted_temps.sort_by_key(owner_sort_key);
    for temp in sorted_temps {
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
            // P5 (final spec §3.2): a sliced temp stays alive until
            // the projection's last use (e.g. `v = (a + b)[0:1]` keeps
            // the concat buffer alive through reads of `v`).
            let anchor = defer_anchor(consumer, &temp, &projections_of, &last_use, &order);
            sidecar.free_schedule.push(FreePoint {
                after: anchor,
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
    let mut sorted_dead: Vec<(Owner, (StringId, Span, TirRef))> = own
        .pending_dead_store
        .iter()
        .map(|(o, v)| (*o, *v))
        .collect();
    sorted_dead.sort_by_key(|(o, _)| owner_sort_key(o));
    for (owner, (name, span, decl_inst)) in &sorted_dead {
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
        // A dead reassign INSIDE A LOOP for a binding declared
        // before that loop: anchor the Free after the outermost loop
        // rather than after the in-loop assign. The in-loop anchor
        // fires only when the body executes — the zero-iteration path
        // leaks the pre-loop buffer. The after-loop anchor emits the
        // binding's CURRENT StrLocals (the init→name map): the final iteration's
        // value on taken paths, the pre-loop value on zero iterations.
        // When the body may `return`, keep the in-loop Free too — the
        // after-loop anchor is unreachable on the return path.
        let (anchor, also_in_body) = match outermost_loop_of(tir, *decl_inst) {
            Some(loop_stmt) if declared_before_stmt(tir, *name, loop_stmt) => {
                let may_return = match tir.inst(loop_stmt).tag {
                    TirTag::WhileLoop => {
                        let view = tir.while_loop_view(loop_stmt);
                        body_may_return(tir, &view.body)
                    }
                    TirTag::ForRange => {
                        let view = tir.for_range_view(loop_stmt);
                        body_may_return(tir, &view.body)
                    }
                    _ => unreachable!("outermost_loop_of returns loops"),
                };
                (loop_stmt, may_return)
            }
            _ => (*decl_inst, false),
        };
        sidecar.free_schedule.push(FreePoint {
            after: anchor,
            target: owner.inst_tirref().expect(
                "pending_dead_store keys are always Owner::Inst (register_pending_dead_store)",
            ),
            span: *span,
            branch: None,
        });
        if also_in_body {
            sidecar.free_schedule.push(FreePoint {
                after: *decl_inst,
                target: owner.inst_tirref().expect(
                    "pending_dead_store keys are always Owner::Inst (register_pending_dead_store)",
                ),
                span: *span,
                branch: None,
            });
        }
    }

    // W0003 case B (M8.4.1.2): redundant bound materializations. Runs
    // after the walk so the escape classification it reuses — final
    // lattice states plus the hazard log — is complete.
    warn_redundant_materialize(tir, pool, &own, sink);

    // Convert honored reseat records into arm-gated
    // `ConditionalDeadDrop`s. A record is honored when a pending entry
    // for one of its reseated owners survived to the drain — i.e. the
    // reassigned value is never read afterwards — so the pre-branch
    // buffer would leak on the paths where the reassign did not happen.
    // (Reads-after clear the pending entry by name, so honored records
    // never collide with the last-use machinery.) Deduped by record:
    // several pending entries can match one record.
    let mut honored: HashSet<usize> = HashSet::new();
    for (owner, (name, _, _)) in &own.pending_dead_store {
        for (idx, drop) in own.reseat_drops.iter().enumerate() {
            if drop.name == *name && drop.reseat_owners.contains(owner) {
                honored.insert(idx);
            }
        }
    }
    // Sorted iteration for deterministic sidecar emission order.
    let mut honored: Vec<usize> = honored.into_iter().collect();
    honored.sort_unstable();
    for idx in honored {
        let drop = &own.reseat_drops[idx];
        sidecar.conditional_dead_drops.push(ConditionalDeadDrop {
            if_stmt: drop.if_stmt,
            target: drop.pre_owner.tirref(&own.param_index),
            arms: drop.untouched_arms.clone(),
        });
    }

    // Loop-exit Frees run LAST so they can inspect the now-complete
    // `free_schedule` and only add jump-anchored Frees for inside-loop
    // owners that no earlier pass already covered.
    schedule_loop_exit_frees_in(tir, &own, sidecar, &body_stmts, None);

    // Return epilogue: destroy locals still live at an early return.
    // Runs LAST so every other Free pass has populated `free_schedule`
    // and we can dedup against it — a value is skipped when another
    // Free already fires on the return's path, or the dead-store drain
    // owns it (its after-decl Free covers every path). Codegen emits
    // due Frees before every `return_`, so anchoring at the return
    // statement itself fires exactly on that exit path.
    let mut epilogue_emitted: HashSet<(TirRef, TirRef)> = HashSet::new();
    for (return_stmt, owners) in &own.return_epilogue {
        let mut on_path: HashSet<TirRef> = HashSet::new();
        let _ = tir.collect_jump_path(&body_stmts, *return_stmt, &mut on_path);
        // A Free anchored after a branch CONTAINING the return never
        // fires on the return's path — the branch statement does not
        // complete before the return exits. Exclude ancestors from the
        // covering set (the path walk counts them as "passed through",
        // which is true for evaluation but false for after-anchoring).
        let ancestors: HashSet<TirRef> = ancestor_branches_of(tir, *return_stmt)
            .into_iter()
            .collect();
        for owner in owners {
            if own.pending_dead_store.contains_key(owner) {
                continue;
            }
            let r = owner.tirref(&own.param_index);
            if !epilogue_emitted.insert((*return_stmt, r)) {
                continue;
            }
            let covered = sidecar.free_schedule.iter().any(|fp| {
                fp.target == r && on_path.contains(&fp.after) && !ancestors.contains(&fp.after)
            });
            if covered {
                continue;
            }
            sidecar.free_schedule.push(FreePoint {
                after: *return_stmt,
                target: r,
                span: tir.span(*return_stmt),
                branch: None,
            });
        }
    }
}

#[cfg(test)]
mod tests;
