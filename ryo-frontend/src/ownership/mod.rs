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
mod tests {
    use super::*;

    /// Build an all-`Borrow` modes slice matching the length of an
    /// argument list. The ownership pass reads `view.modes` directly,
    /// so test/builtin call sites pass all-`Borrow` to avoid
    /// accidentally moving arguments.
    fn all_borrow(args: &[TirRef]) -> Vec<ryo_core::tir::ParamMode> {
        vec![ryo_core::tir::ParamMode::Borrow; args.len()]
    }

    /// Positional replacement for the old name-keyed sidecar lookup:
    /// find `name`'s index in `tirs` and take that entry.
    /// Take the sidecar entry at `index`, positional with the `tirs`
    /// slice handed to `check` — the same contract codegen relies on.
    /// (Every test below checks a single function, so `index` is 0.)
    fn take_function_sidecar(sidecar: &mut OwnershipSidecar, index: usize) -> FunctionSidecar {
        let name = sidecar.functions[index].name;
        std::mem::replace(&mut sidecar.functions[index], FunctionSidecar::new(name))
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
        // Regression test for the borrowed-scalar exclusion. The
        // StrConst arg of `__ryo_panic` uses the borrowed-scalar ABI in
        // codegen — codegen passes the raw .rodata pointer with cap=0
        // and never owns the buffer. The
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
        let sidecar = take_function_sidecar(&mut sidecar_map, 0);

        // No scheduled Free should target __ryo_panic's StrConst arg —
        // codegen's borrowed-scalar ABI never frees it.
        assert!(
            sidecar.free_schedule.iter().all(|fp| fp.target != str_arg),
            "expected no scheduled Free for __ryo_panic's StrConst arg, got: {:?}",
            sidecar.free_schedule
        );

        // The exclusion is driven by the ABI registry: `__ryo_panic`
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
    fn pre_loop_owner_last_use_in_loop_freed_at_loop_exit() {
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

        let frees: Vec<_> = sidecar
            .free_schedule
            .iter()
            .filter(|fp| fp.target == alloc)
            .collect();
        assert_eq!(
            frees.len(),
            1,
            "exactly one Free for the pre-loop owner; got: {:?}",
            sidecar.free_schedule
        );
        assert_eq!(
            frees[0].after, wl,
            "a pre-loop owner whose last use is in the loop is freed at the loop exit (covers break, normal-exit, and zero-iteration paths); got: {:?}",
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
        let sc = take_function_sidecar(&mut sc, 0);
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
        let sc = take_function_sidecar(&mut sc, 0);
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
        // Regression for the break-path leak. A `break` taken before the
        // `print(s)` last-use must trigger a Free anchored on the break
        // instr — otherwise the inside-loop allocation leaks on the break
        // path.
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
        // Cross-branch regression for the break-path leak. The natural
        // last-use Free for `alloc` anchors on `print(s)` inside the
        // THEN arm; the break
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
        // Symmetric regression for `continue` instead of `break`.
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
        let sidecar = take_function_sidecar(&mut sidecar, 0);
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
        // Direct regression for Bug 4 in M8.1c. The loop merge starts
        // from the pre-loop entry; merge_branches must monotonically
        // advance next_branch_id so that BranchIds minted inside a
        // loop body are not reused by post-loop ifs.
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
        let sc = take_function_sidecar(&mut sidecar, 0);

        let virtual_ref = TirRef::param(0);
        let fp = sc
            .free_schedule
            .iter()
            .find(|fp| fp.target == virtual_ref)
            .expect("free scheduled");
        assert_eq!(fp.after, ret);
    }

    #[test]
    fn read_move_param_frees_after_last_read_not_last_stmt() {
        // fn f(move s: str): print(s); print(42) — the param's Free
        // anchors after its last read (the `Var` inside print(s)),
        // not after the later statement that never touches it.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let void = pool.void();
        let f = pool.intern_str("f");
        let s_name = pool.intern_str("s");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            f,
            vec![TirParam {
                name: s_name,
                ty: str_ty,
                mode: ParamMode::Move,
                span,
            }],
            void,
            span,
        );
        let s_read = tb.var(s_name, str_ty, span);
        let call1 = tb.call(print, &[s_read], &all_borrow(&[s_read]), void, span);
        let n = tb.int_const(42, int_ty, span);
        let call2 = tb.call(print, &[n], &all_borrow(&[n]), void, span);
        let tir = tb.finish(&[call1, call2]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);

        let frees: Vec<_> = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == TirRef::param(0))
            .collect();
        assert_eq!(
            frees.len(),
            1,
            "exactly one Free for the owned param; schedule = {:?}",
            sc.free_schedule
        );
        assert_eq!(
            frees[0].after, s_read,
            "the Free must anchor after the param's last read; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn param_last_read_inside_loop_frees_after_loop() {
        // fn f(move s: str, cond: bool): while cond: print(s) — the
        // last read sits inside the loop; anchoring after it would fire
        // the Free per iteration (UAF on the next iteration's read), so
        // the Free moves to after the loop statement.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let f = pool.intern_str("f");
        let s_name = pool.intern_str("s");
        let cond_name = pool.intern_str("cond");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            f,
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
        let cond = tb.var(cond_name, bool_ty, span);
        let s_read = tb.var(s_name, str_ty, span);
        let call = tb.call(print, &[s_read], &all_borrow(&[s_read]), void, span);
        let while_stmt = tb.while_loop(cond, &[call], void, span);
        let tir = tb.finish(&[while_stmt]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);

        let frees: Vec<_> = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == TirRef::param(0))
            .collect();
        assert_eq!(
            frees.len(),
            1,
            "exactly one Free for the owned param; schedule = {:?}",
            sc.free_schedule
        );
        assert_eq!(
            frees[0].after, while_stmt,
            "a param last read inside a loop must be freed after the loop, not inside it; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn param_slice_read_defers_free_to_projection_last_use() {
        // fn f(move s: str): v = s[0:2]; print(v); print(42) — the
        // slice projects the param, so its Free defers to the
        // projection's last use (the read inside print(v)), past the
        // param's own last direct read.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let f = pool.intern_str("f");
        let s_name = pool.intern_str("s");
        let v_name = pool.intern_str("v");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            f,
            vec![TirParam {
                name: s_name,
                ty: str_ty,
                mode: ParamMode::Move,
                span,
            }],
            void,
            span,
        );
        let base = tb.var(s_name, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v_name, false, view_ty, sl, span);
        let vread = tb.var(v_name, view_ty, span);
        let call1 = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let n = tb.int_const(42, int_ty, span);
        let call2 = tb.call(print, &[n], &all_borrow(&[n]), void, span);
        let tir = tb.finish(&[vdecl, call1, call2]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);

        let frees: Vec<_> = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == TirRef::param(0))
            .collect();
        assert_eq!(
            frees.len(),
            1,
            "exactly one Free for the owned param; schedule = {:?}",
            sc.free_schedule
        );
        assert_eq!(
            frees[0].after, vread,
            "a slice of the param defers its Free to the projection's last use; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn never_read_move_param_still_freed_once_after_last_stmt() {
        // fn f(move s: str): print(42) — a never-read owned param must
        // still be freed exactly once, anchored after the last body
        // statement (there is no last read to anchor on).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let void = pool.void();
        let f = pool.intern_str("f");
        let s_name = pool.intern_str("s");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            f,
            vec![TirParam {
                name: s_name,
                ty: str_ty,
                mode: ParamMode::Move,
                span,
            }],
            void,
            span,
        );
        let n = tb.int_const(42, int_ty, span);
        let call = tb.call(print, &[n], &all_borrow(&[n]), void, span);
        let tir = tb.finish(&[call]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);

        let frees: Vec<_> = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.target == TirRef::param(0))
            .collect();
        assert_eq!(
            frees.len(),
            1,
            "exactly one Free for the owned param; schedule = {:?}",
            sc.free_schedule
        );
        assert_eq!(
            frees[0].after, call,
            "a never-read param keeps the last-body-statement anchor; schedule = {:?}",
            sc.free_schedule
        );
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
        let sc = take_function_sidecar(&mut sidecar, 0);

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
        let sc = take_function_sidecar(&mut sidecar, 0);

        // temp_a (lit_a) is released via `free_on_reassign` (codegen lowers
        // its destructor at the Assign), so it must NOT also appear in
        // `free_schedule` — that would be a double-free. The anon-temp pass
        // skips it because it is a named init (the VarDecl initializer);
        // if that filter were missing the anon-temp pass would schedule a
        // second Free here. See the sibling invariant in
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
        let sc = take_function_sidecar(&mut sc, 0);
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
        // value), so the anon-temp pass must SKIP it (static classifier)
        // and let the dead-store pass own its single Free.
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
        let sc = take_function_sidecar(&mut sc, 0);
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
        // already-moved binding. The Var arm used to emit E0020 for it;
        // now the consume site (consume_underlying) is the authority
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
        let sidecar = take_function_sidecar(&mut sidecar, 0);

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
    fn move_and_viewasstr_borrow_of_one_owner_reports_borrow_note() {
        // `two(move s, s[0:1])` against a `(move, str)` signature — the
        // P6'-converted ViewAsStr arg borrows the view's ROOT owner, so
        // E0031 must carry the "borrowed here" note (look through the
        // conversion, like the Rule-7 partition does).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let two = pool.intern_str("two");
        let span = SimpleSpan::new((), 0..0);
        // Distinct spans for the move arg and the reborrow chain, so
        // the note can be pinned to the reborrow specifically.
        let move_span = SimpleSpan::new((), 10..11);
        let reborrow_span = SimpleSpan::new((), 20..26);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let sv1 = tb.var(s, str_ty, move_span);
        let sv2 = tb.var(s, str_ty, reborrow_span);
        let zero = tb.int_const(0, int_ty, reborrow_span);
        let one = tb.int_const(1, int_ty, reborrow_span);
        let sl = tb.slice(sv2, Some(zero), Some(one), view_ty, reborrow_span);
        let reborrow = tb.view_as_str(sl, str_ty, reborrow_span);
        let modes = vec![ParamMode::Move, ParamMode::Borrow];
        let call = tb.call(two, &[sv1, reborrow], &modes, void, span);
        let tir = tb.finish(&[decl, call]);
        let mut sink = DiagSink::new();
        let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        let e0031: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall))
            .collect();
        assert_eq!(e0031.len(), 1, "expected exactly one E0031; got {diags:?}");
        let note = e0031[0]
            .notes
            .iter()
            .find(|n| n.message == "borrowed here")
            .unwrap_or_else(|| {
                panic!(
                    "E0031 must carry the 'borrowed here' note through ViewAsStr; got {:?}",
                    e0031[0].notes
                )
            });
        assert_eq!(
            note.span,
            Some(tir.span(reborrow)),
            "the note must attach to the reborrow, not the move arg ({move_span:?})"
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
    fn e0032_names_local_binding_not_value() {
        // `swap(&c, &c)` must name `c` in the message — the spec's
        // rendered example shows the backticked binding name, not the
        // generic "value" that `owner_name_for_diag` falls back to for
        // locals (it inspects the initializer, not the read).
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
        let msg = &diags
            .iter()
            .find(|d| matches!(d.code, DiagCode::MutableAliasingViolation))
            .expect("E0032 must fire")
            .message;
        assert!(
            msg.contains("`c`"),
            "E0032 must name the binding `c`; got: {msg}"
        );
        assert!(
            !msg.contains("value"),
            "E0032 must not fall back to 'value'; got: {msg}"
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
        // Callee side: `fn set(inout s: str): s = "new"`. The
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
        let sc = take_function_sidecar(&mut sidecar, 0);
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
        // `fn g(inout s: str): if c: s = "b"`. The rebind is
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
        let sc = take_function_sidecar(&mut sidecar, 0);
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
        // The value bound to an inout str param escapes via the
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
        // Returning the value currently bound to an inout str param
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
        // Caller side: `mut s = "hi"; set(&s); print(s)`. An inout
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
        let sc = take_function_sidecar(&mut sidecar, 0);
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
        // `mut s = "a"; if c: set(&s); print(s)` — the
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
        // `mut s = ""; while i < 3: str_push(&s, "x") ...
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
        let sc = take_function_sidecar(&mut sidecar, 0);
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
        // Pre-existing M8.1 bug: `mut s = "a"; if c:
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
        let sc = take_function_sidecar(&mut sidecar, 0);
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

    #[test]
    fn conditional_dead_reassign_schedules_fallthrough_drop() {
        // `mut s = "a"; if c: s = "b"` with s never read after.
        // The taken arm drops "a" (free_on_reassign) and the drain frees
        // "b"; the NOT-taken path must also free "a" — via an arm-gated
        // ConditionalDeadDrop for the pre-branch owner.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit_a, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
        let asg = tb.assign(s, str_ty, lit_b, span);
        let if_s = tb.if_stmt(cond, &[asg], &[], None, void, span);
        let tir = tb.finish(&[decl, if_s]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        let drops: Vec<_> = sc
            .conditional_dead_drops
            .iter()
            .filter(|d| d.target == lit_a)
            .collect();
        assert_eq!(
            drops.len(),
            1,
            "expected one ConditionalDeadDrop for the pre-branch owner; got {:?}",
            sc.conditional_dead_drops
        );
        assert_eq!(drops[0].if_stmt, if_s, "the drop must key on the if");
        assert!(
            !drops[0].arms.is_empty(),
            "the drop must name at least one untouched arm (the fall-through)"
        );
    }

    #[test]
    fn conditional_reassign_all_arms_reseated_no_drop() {
        // `if c: s = "b" else: s = "d"` — every arm reseats, so the
        // pre-branch buffer is dropped by free_on_reassign on every
        // path; no ConditionalDeadDrop may be scheduled.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit_a, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
        let asg_then = tb.assign(s, str_ty, lit_b, span);
        let lit_d = tb.str_const(pool.intern_str("d"), str_ty, span);
        let asg_else = tb.assign(s, str_ty, lit_d, span);
        let if_s = tb.if_stmt(cond, &[asg_then], &[], Some(&[asg_else]), void, span);
        let tir = tb.finish(&[decl, if_s]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sc.conditional_dead_drops.is_empty(),
            "all arms reseated — no untouched path, no ConditionalDeadDrop; got {:?}",
            sc.conditional_dead_drops
        );
    }

    #[test]
    fn loop_dead_reassign_anchors_after_loop() {
        // `mut s = "a"; while c: s = "b"` with s never read
        // after. The dead-store Free must anchor AFTER THE LOOP (not
        // after the in-loop assign): the in-loop anchor never fires on
        // the zero-iteration path, leaking the pre-loop buffer. The
        // after-loop anchor emits the binding's current StrLocals —
        // final value on taken paths, pre-loop value on zero iterations.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit_a, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
        let asg = tb.assign(s, str_ty, lit_b, span);
        let wl = tb.while_loop(cond, &[asg], void, span);
        let tir = tb.finish(&[decl, wl]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.after == wl && fp.target == lit_b),
            "expected a Free anchored after the loop for the binding's value; schedule = {:?}",
            sc.free_schedule
        );
        assert!(
            !sc.free_schedule
                .iter()
                .any(|fp| fp.after == asg && fp.target == lit_b),
            "the in-loop dead-store Free must move to the loop anchor (it never fires on zero iterations); schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn loop_dead_reassign_with_return_keeps_in_body_free() {
        // When the loop body can `return`, the after-loop anchor
        // is unreachable on the return path — the in-body Free must stay
        // alongside the after-loop one.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit_a, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
        let asg = tb.assign(s, str_ty, lit_b, span);
        let ret = tb.return_void(void, span);
        let wl = tb.while_loop(cond, &[asg, ret], void, span);
        let tir = tb.finish(&[decl, wl]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.after == wl && fp.target == lit_b),
            "expected the after-loop Free; schedule = {:?}",
            sc.free_schedule
        );
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.after == asg && fp.target == lit_b),
            "a returning body keeps the in-body Free (return path); schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn loop_local_dead_value_keeps_in_body_anchor() {
        // Guard: a value DECLARED inside the loop body is not a
        // pre-loop binding — its Free must stay anchored in the body
        // (the binding's StrLocals don't exist on the zero-iteration
        // path).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let t = pool.intern_str("t");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let lit_x = tb.str_const(pool.intern_str("x"), str_ty, span);
        let decl = tb.var_decl(t, false, str_ty, lit_x, span);
        let wl = tb.while_loop(cond, &[decl], void, span);
        let tir = tb.finish(&[wl]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.after == decl && fp.target == lit_x),
            "loop-local dead value keeps its in-body Free; schedule = {:?}",
            sc.free_schedule
        );
        assert!(
            !sc.free_schedule.iter().any(|fp| fp.after == wl),
            "loop-local value must NOT be re-anchored after the loop; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn last_use_inside_loop_anchors_after_loop() {
        // Conditional last use: `mut s = "a";
        // for i in range(0, 3): print(s)`. The last read of `s` is
        // inside the loop body, but freeing there is a UAF — the next
        // iteration reads the freed buffer. The value is dead on ALL
        // paths only at the loop exit, so the Free must anchor after
        // the loop statement.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let i = pool.intern_str("i");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit_a, span);
        let zero = tb.int_const(0, int_ty, span);
        let three = tb.int_const(3, int_ty, span);
        let sv = tb.var(s, str_ty, span);
        let pr = tb.call(print, &[sv], &[ParamMode::Borrow], void, span);
        let fr = tb.for_range(i, zero, three, &[pr], void, span);
        let tir = tb.finish(&[decl, fr]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.after == fr && fp.target == lit_a),
            "the last-use Free must anchor after the loop (in-body anchoring frees s between iterations — UAF); schedule = {:?}",
            sc.free_schedule
        );
        assert!(
            !sc.free_schedule
                .iter()
                .any(|fp| fp.after == sv && fp.target == lit_a),
            "no Free may anchor after the in-loop read itself; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn last_use_inside_if_anchors_after_if() {
        // Same family through an if: `mut s = "a"; if d: print(s)` —
        // anchoring after the in-arm read leaks `s` on the not-taken
        // path; the merge point is where the value is dead on all paths.
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
        let sv = tb.var(s, str_ty, span);
        let pr = tb.call(print, &[sv], &[ParamMode::Borrow], void, span);
        let if_s = tb.if_stmt(cond, &[pr], &[], None, void, span);
        let tir = tb.finish(&[decl, if_s]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.after == if_s && fp.target == lit_a),
            "the last-use Free must anchor after the if (in-arm anchoring leaks on the not-taken path); schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn return_epilogue_frees_live_local() {
        // Return-epilogue: `mut s = "a"; if d: print(s) else: return`.
        // On the else path, `s` is still live at the return and nothing
        // freed it — the last-use Free anchors in the sibling then-arm,
        // which the else path never reaches. An early return must
        // destroy the function's still-owned locals on ITS path.
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
        let sv = tb.var(s, str_ty, span);
        let pr = tb.call(print, &[sv], &[ParamMode::Borrow], void, span);
        let ret = tb.return_void(void, span);
        let if_s = tb.if_stmt(cond, &[pr], &[], Some(&[ret]), void, span);
        let tir = tb.finish(&[decl, if_s]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.after == ret && fp.target == lit_a),
            "the still-live local must be freed on the early-return path; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn return_epilogue_skips_consumed_return_value() {
        // `fn f() -> str: s = "a"; return s` — the returned value moved
        // out; an epilogue Free for it would be a use-after-free in the
        // caller.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{TirBuilder, TirData, TirTag};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let f = pool.intern_str("f");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(f, vec![], str_ty, span);
        let lit = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let sv = tb.var(s, str_ty, span);
        let ret = tb.push_typed(TirTag::Return, TirData::UnOp(sv), str_ty, span);
        let tir = tb.finish(&[decl, ret]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            !sc.free_schedule
                .iter()
                .any(|fp| fp.target == lit && fp.after == ret),
            "the returned value moved out — no epilogue Free for it; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn return_epilogue_skips_dead_store_owned() {
        // `mut s = "a"; if d: return` with s never read: the dead-store
        // drain already frees `s` right after its declaration (covering
        // every path), so no epilogue Free may be added for it.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit_a, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let ret = tb.return_void(void, span);
        let if_s = tb.if_stmt(cond, &[ret], &[], None, void, span);
        let tir = tb.finish(&[decl, if_s]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            !sc.free_schedule
                .iter()
                .any(|fp| fp.target == lit_a && fp.after == ret),
            "dead-store-owned value is already freed at its decl — no epilogue Free; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn return_epilogue_covers_move_param() {
        // `fn f(move s: str): if d: return` — the owned param is still
        // Valid at the early return; the callee must destroy it there
        // (the never-read param's Free anchors after the last body
        // stmt, which the early-return path never reaches).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let f = pool.intern_str("f");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(
            f,
            vec![TirParam {
                name: s,
                ty: str_ty,
                mode: ParamMode::Move,
                span,
            }],
            void,
            span,
        );
        let cond = tb.bool_const(true, bool_ty, span);
        let ret = tb.return_void(void, span);
        let if_s = tb.if_stmt(cond, &[ret], &[], None, void, span);
        let tir = tb.finish(&[if_s]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.after == ret && fp.target == TirRef::param(0)),
            "an owned param must be destroyed on the early-return path; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn temp_last_use_inside_loop_not_reanchored() {
        // Guard: an anonymous temp consumed inside the loop body must
        // keep its per-iteration Free (each iteration allocates a fresh
        // value) — the re-anchor applies only to named pre-branch
        // bindings.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder};
        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let i = pool.intern_str("i");
        let int_to_str = pool.intern_str("int_to_str");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);
        let mut tb = TirBuilder::new(main, vec![], void, span);
        let zero = tb.int_const(0, int_ty, span);
        let three = tb.int_const(3, int_ty, span);
        let iv = tb.var(i, int_ty, span);
        let call = tb.call(int_to_str, &[iv], &[ParamMode::Borrow], str_ty, span);
        let pr = tb.call(print, &[call], &[ParamMode::Borrow], void, span);
        let fr = tb.for_range(i, zero, three, &[pr], void, span);
        let tir = tb.finish(&[fr]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            !sc.free_schedule.iter().any(|fp| fp.after == fr),
            "a loop-local temp must keep its per-iteration Free, not move to the loop anchor; schedule = {:?}",
            sc.free_schedule
        );
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.target == call && fp.after == pr),
            "the temp's Free stays anchored after its consumer; schedule = {:?}",
            sc.free_schedule
        );
    }

    #[test]
    fn conditional_dead_reassign_gated_on_real_else() {
        // `if c: s = "b" else: <no reassign>` with s unread after —
        // the drop must be gated on the REAL else arm's BranchId.
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
        let lit_x = tb.str_const(pool.intern_str("x"), str_ty, span);
        let pr = tb.call(print, &[lit_x], &[ParamMode::Borrow], void, span);
        let if_s = tb.if_stmt(cond, &[asg], &[], Some(&[pr]), void, span);
        let tir = tb.finish(&[decl, if_s]);
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        let else_id = sc
            .if_branches
            .get(&if_s)
            .and_then(|ids| ids.else_branch)
            .expect("else branch id");
        let drops: Vec<_> = sc
            .conditional_dead_drops
            .iter()
            .filter(|d| d.target == lit_a)
            .collect();
        assert_eq!(
            drops.len(),
            1,
            "expected one ConditionalDeadDrop; got {:?}",
            sc.conditional_dead_drops
        );
        assert!(
            drops[0].arms.contains(&else_id),
            "the drop must be gated on the else arm {:?}; got {:?}",
            else_id,
            drops[0].arms
        );
    }

    // ---------- M8.4: projection tracking (final spec §3.2/§3.3) ----------

    #[test]
    fn view_creation_registers_projection() {
        // fn main(): s: str = "hello"; v = s[0:2]; print(v)
        // → no diags; root_owner[v] == s; s's free is anchored after
        //   print(v), not after decl (P5 deferral, final spec §3.2).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let vread = tb.var(v, view_ty, span);
        let call = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, vdecl, stmt]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = take_function_sidecar(&mut sidecar, 0);
        assert!(sink.is_empty(), "expected no diagnostics");

        // The FreePoint targeting s's owner fires after the print(v)
        // statement (anchored on v's read inside the call), not after
        // the slice-base read — the projection keeps the buffer alive.
        assert!(
            sidecar
                .free_schedule
                .iter()
                .any(|fp| fp.target == lit && fp.after == vread && fp.branch.is_none()),
            "expected s's Free anchored after print(v)'s read of v; got: {:?}",
            sidecar.free_schedule
        );
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
    fn freeze_blocks_move_while_view_live() {
        // s: str = "hello"; v = s[0:2]; consume(s)  (move-mode callee)
        // → SourceProjected at the consume call (P2, final spec §3.2).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let sread = tb.var(s, str_ty, span);
        let call = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, vdecl, stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn freeze_blocks_inout_while_view_live() {
        // s: str = "hello"; v = s[0:2]; str_push(&s, "!")
        // → SourceProjected (inout passing mutates the owner; P2).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let str_push = pool.intern_str("str_push");
        let hello = pool.intern_str("hello");
        let bang = pool.intern_str("!");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let sread = tb.var(s, str_ty, span);
        let suffix = tb.str_const(bang, str_ty, span);
        let call = tb.call(
            str_push,
            &[sread, suffix],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, vdecl, stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("mutate"),
            "expected the diagnostic to say `mutate`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn freeze_lifts_after_last_view_use() {
        // s: str = "hello"; v = s[0:2]; print(v); consume(s)
        // → no diags; s moves legally after v's last use (P4).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let sread = tb.var(s, str_ty, span);
        let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
        let tir = tb.finish(&[decl, vdecl, pstmt, cstmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.is_empty(),
            "expected no diagnostics (freeze lifted at v's last use)"
        );
    }

    #[test]
    fn reslice_projects_root_owner() {
        // s: str = "hello"; v = s[0:3]; w = v[1:2]; consume(s) while w live
        // → SourceProjected naming s; root_owner[w] == s (P3).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let w = pool.intern_str("w");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base1 = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i3 = tb.int_const(3, int_ty, span);
        let sl1 = tb.slice(base1, Some(i0), Some(i3), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl1, span);
        let base2 = tb.var(v, view_ty, span);
        let i1 = tb.int_const(1, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl2 = tb.slice(base2, Some(i1), Some(i2), view_ty, span);
        let wdecl = tb.var_decl(w, false, view_ty, sl2, span);
        let sread = tb.var(s, str_ty, span);
        let call = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, vdecl, wdecl, stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.code, DiagCode::SourceProjected) && d.message.contains("`s`")),
            "expected SourceProjected naming `s` (the re-slice projects the root); got: {diags:?}"
        );
    }

    #[test]
    fn view_return_is_escape() {
        // fn bad(s: str) -> strview: return s[0:1]
        // → sema rejects at signature level (Task 5); this is the
        //   ownership backstop: hand-built TIR with a view return
        //   must produce ViewEscape (E1, final spec §3.3).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{TirBuilder, TirParam};

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bad = pool.intern_str("bad");
        let s = pool.intern_str("s");
        let span = SimpleSpan::new((), 0..0);

        let params = vec![TirParam {
            name: s,
            ty: str_ty,
            mode: ParamMode::Borrow,
            span,
        }];
        let mut tb = TirBuilder::new(bad, params, view_ty, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i1 = tb.int_const(1, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i1), view_ty, span);
        let ret = tb.unary(TirTag::Return, view_ty, sl, span);
        let tir = tb.finish(&[ret]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            diags.iter().any(|d| matches!(d.code, DiagCode::ViewEscape)),
            "expected ViewEscape on the view return; got: {diags:?}"
        );
    }

    #[test]
    fn view_across_branches() {
        // s: str = "hello"; v = s[0:2]; if flag: print(v) else: print("x"); consume(s)
        // → legal: v's last use is inside the branch; freeze lifted at
        //   the join; P5 keeps s alive through the branch.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let x = pool.intern_str("x");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let xlit = tb.str_const(x, str_ty, span);
        let xcall = tb.call(print, &[xlit], &all_borrow(&[xlit]), void, span);
        let xstmt = tb.unary(TirTag::ExprStmt, void, xcall, span);
        let ifs = tb.if_stmt(cond, &[pstmt], &[], Some(&[xstmt]), void, span);
        let sread = tb.var(s, str_ty, span);
        let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
        let tir = tb.finish(&[decl, vdecl, ifs, cstmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.is_empty(),
            "expected no diagnostics (freeze lifted at the join)"
        );
    }

    #[test]
    fn freeze_is_per_arm_precise_across_if_arms() {
        // Per-arm view_last_use (author decision): during an arm walk a
        // view's last use for freeze purposes is its last read on the
        // path THROUGH that arm, not the global max over all arms.
        //   s: str = "hello"; v = s[0:2]
        //   if cond: print(v); consume(s)   # v dead before the move, this path
        //   else: print(v)
        // → legal: on the then-path v's last read completes before the
        //   consume; the else-arm read is not on that path.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let vread_t = tb.var(v, view_ty, span);
        let pcall_t = tb.call(print, &[vread_t], &all_borrow(&[vread_t]), void, span);
        let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
        let sread = tb.var(s, str_ty, span);
        let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
        let vread_e = tb.var(v, view_ty, span);
        let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
        let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
        let ifs = tb.if_stmt(cond, &[pstmt_t, cstmt], &[], Some(&[pstmt_e]), void, span);
        let tir = tb.finish(&[decl, vdecl, ifs]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.is_empty(),
            "expected no diagnostics (v is dead on the then-path before the move); got: {:?}",
            sink.into_diags()
        );
    }

    #[test]
    fn per_arm_override_respects_loop_deferral() {
        // Regression (review Critical on 2cbaa06): the per-arm override
        // must not install an arm-local last read that sits inside a
        // loop the creation is outside of — the walk-constant
        // `view_defer_loop` is computed from the GLOBAL max read only,
        // so the death site would drain the projection mid-loop and
        // un-freeze a later owner mutation in the same body.
        //   s: str = "hello"; v = s[0:2]
        //   if true:
        //       while true:
        //           print(v)          # arm-local last read, inside loop
        //           str_push(&s, "xxxx")  # realloc; iteration 2 re-reads v
        //   else:
        //       print(v)              # global max read — outside any loop
        // → exactly one SourceProjected naming `s` (P4 deferral holds
        //   per arm: the in-loop read keeps v live through the loop).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let str_push = pool.intern_str("str_push");
        let hello = pool.intern_str("hello");
        let x4 = pool.intern_str("xxxx");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let if_cond = tb.bool_const(true, bool_ty, span);
        let wcond = tb.bool_const(true, bool_ty, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let sread = tb.var(s, str_ty, span);
        let suffix = tb.str_const(x4, str_ty, span);
        let push = tb.call(
            str_push,
            &[sread, suffix],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
        let wl = tb.while_loop(wcond, &[pstmt, push_stmt], void, span);
        let vread_e = tb.var(v, view_ty, span);
        let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
        let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
        let ifs = tb.if_stmt(if_cond, &[wl], &[], Some(&[pstmt_e]), void, span);
        let tir = tb.finish(&[decl, vdecl, ifs]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn per_arm_kill_respects_loop_deferral() {
        // Kill-side companion to `per_arm_override_respects_loop_deferral`:
        // the arm-kill fires only when the arm has NO reads of the view
        // in its subtree, so there is no deeper arm-local read to
        // strand — and a deeper read in a SIBLING arm is itself the
        // global max read, which the pre-pass deferral already covers.
        //   s: str = "hello"; v = s[0:2]
        //   if true:
        //       consume(s)        # kill candidate: no reads of v here
        //   else:
        //       while true:
        //           print(v)      # global max read, deeper than creation
        //                         #   → loop-deferred → kill/override skipped
        // → exactly one SourceProjected naming `s` (v is live in the
        //   then-arm through the deferral exemption).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let if_cond = tb.bool_const(true, bool_ty, span);
        let sread = tb.var(s, str_ty, span);
        let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
        let wcond = tb.bool_const(true, bool_ty, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let wl = tb.while_loop(wcond, &[pstmt], void, span);
        let ifs = tb.if_stmt(if_cond, &[cstmt], &[], Some(&[wl]), void, span);
        let tir = tb.finish(&[decl, vdecl, ifs]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn per_arm_kill_without_deferral_stays_scoped() {
        // Non-deferred companion to `per_arm_kill_respects_loop_deferral`:
        // the kill needs no loop guard because it fires only in an arm
        // with NO reads of the view — a deeper read in a SIBLING arm is
        // the global max read and lies on a different path, so the view
        // is genuinely dead on the no-read arm's path and moving the
        // owner there is sound. Soundness rests on the per-arm
        // `live_projections` snapshot scoping the kill to that arm's
        // walk: the else arm below must still see the view live.
        //   s: str = "hello"; v = s[0:2]
        //   if c1: print(v)               # sibling read of v
        //   elif c2: consume(s)           # no reads of v → kill → LEGAL
        //   else: consume(s); print(v)    # v live until its read → E0035
        // → exactly one SourceProjected naming `s` (from the else arm):
        //   the elif move is accepted (the kill fired without deferral)
        //   and the else move is rejected (the kill did not leak across
        //   arms — had the elif walk left the projection drained, the
        //   else-arm move would be silently accepted too).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond1 = tb.bool_const(true, bool_ty, span);
        let cond2 = tb.bool_const(true, bool_ty, span);
        let vread_t = tb.var(v, view_ty, span);
        let pcall_t = tb.call(print, &[vread_t], &all_borrow(&[vread_t]), void, span);
        let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
        let sread_elif = tb.var(s, str_ty, span);
        let ccall_elif = tb.call(consume, &[sread_elif], &[ParamMode::Move], void, span);
        let cstmt_elif = tb.unary(TirTag::ExprStmt, void, ccall_elif, span);
        let sread_e = tb.var(s, str_ty, span);
        let ccall_e = tb.call(consume, &[sread_e], &[ParamMode::Move], void, span);
        let cstmt_e = tb.unary(TirTag::ExprStmt, void, ccall_e, span);
        let vread_e = tb.var(v, view_ty, span);
        let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
        let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
        let ifs = tb.if_stmt(
            cond1,
            &[pstmt_t],
            &[(cond2, vec![cstmt_elif])],
            Some(&[cstmt_e, pstmt_e]),
            void,
            span,
        );
        let tir = tb.finish(&[decl, vdecl, ifs]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn per_arm_override_applies_to_elif_arms() {
        // Elif arms go through `refine_view_liveness_for_arm` like the
        // then/else arms (arm index 1 + elif_index, with the pre-pass's
        // `arm_reads` laid out then/elif…/else): a view read in the elif
        // arm whose global last use lies in a LATER arm dies at its
        // elif-local last read, un-freezing the owner for the rest of
        // the elif arm. Were elif arms skipped, the view would stay live
        // to the else-arm read and the move below would be spuriously
        // diagnosed — this is the only per-arm shape that distinguishes
        // a working elif path from a missing one.
        //   s: str = "hello"; v = s[0:2]
        //   if c1: print("t")             # no reads of v (kill — inert)
        //   elif c2: print(v); consume(s) # move AFTER v's elif-local
        //                                 #   last read → LEGAL
        //   else: print(v)                # global max read (later arm)
        // → no diagnostics.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let tee = pool.intern_str("t");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond1 = tb.bool_const(true, bool_ty, span);
        let cond2 = tb.bool_const(true, bool_ty, span);
        let tlit = tb.str_const(tee, str_ty, span);
        let pcall_t = tb.call(print, &[tlit], &all_borrow(&[tlit]), void, span);
        let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
        let vread_elif = tb.var(v, view_ty, span);
        let pcall_elif = tb.call(print, &[vread_elif], &all_borrow(&[vread_elif]), void, span);
        let pstmt_elif = tb.unary(TirTag::ExprStmt, void, pcall_elif, span);
        let sread = tb.var(s, str_ty, span);
        let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
        let vread_e = tb.var(v, view_ty, span);
        let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
        let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
        let ifs = tb.if_stmt(
            cond1,
            &[pstmt_t],
            &[(cond2, vec![pstmt_elif, cstmt])],
            Some(&[pstmt_e]),
            void,
            span,
        );
        let tir = tb.finish(&[decl, vdecl, ifs]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.is_empty(),
            "expected no diagnostics (v dies at its elif-local last read); got: {:?}",
            sink.into_diags()
        );
    }

    #[test]
    fn view_created_in_loop_body_per_arm_kill_applies() {
        // Loop deferral (`view_defer_loop`) covers only views created
        // OUTSIDE the loop their last read sits in (`created_in <
        // read_in`). A view created INSIDE the loop body is re-sliced
        // from the current buffer every iteration, so the back-edge
        // cannot strand a stale read: the deferral does not apply and
        // the per-arm kill fires — on the arm with no reads of v the
        // owner mutation is sound and must NOT be diagnosed.
        //   s: str = "hello"
        //   while true:
        //       v = s[0:2]
        //       if c: str_push(&s, "!")   # no reads of v → kill → LEGAL
        //       else: print(v)
        // → no diagnostics (no spurious SourceProjected).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let str_push = pool.intern_str("str_push");
        let hello = pool.intern_str("hello");
        let bang = pool.intern_str("!");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let wcond = tb.bool_const(true, bool_ty, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sread = tb.var(s, str_ty, span);
        let suffix = tb.str_const(bang, str_ty, span);
        let push = tb.call(
            str_push,
            &[sread, suffix],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let ifs = tb.if_stmt(cond, &[push_stmt], &[], Some(&[pstmt]), void, span);
        let wl = tb.while_loop(wcond, &[vdecl, ifs], void, span);
        let tir = tb.finish(&[decl, wl]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.is_empty(),
            "expected no diagnostics (v is re-sliced each iteration and dead on the push arm); got: {:?}",
            sink.into_diags()
        );
    }

    #[test]
    fn view_created_in_loop_body_freeze_holds_before_read() {
        // Flip side of `view_created_in_loop_body_per_arm_kill_applies`:
        // in-loop creation must not disable the freeze on the arm that
        // DOES read the view — the mutation precedes v's read on the
        // same path, so v is live at the mutation site. This is the
        // no-false-UAF-acceptance direction: the realloc in str_push
        // would leave v pointing at freed memory when the read runs.
        //   s: str = "hello"
        //   while true:
        //       v = s[0:2]
        //       if c: str_push(&s, "!"); print(v)   # mutation while v live
        // → exactly one SourceProjected naming `s`.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let str_push = pool.intern_str("str_push");
        let hello = pool.intern_str("hello");
        let bang = pool.intern_str("!");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let wcond = tb.bool_const(true, bool_ty, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sread = tb.var(s, str_ty, span);
        let suffix = tb.str_const(bang, str_ty, span);
        let push = tb.call(
            str_push,
            &[sread, suffix],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let ifs = tb.if_stmt(cond, &[push_stmt, pstmt], &[], None, void, span);
        let wl = tb.while_loop(wcond, &[vdecl, ifs], void, span);
        let tir = tb.finish(&[decl, wl]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn freeze_holds_before_arm_local_last_read() {
        // Contract: a move of the owner BEFORE the view's arm-local last
        // read stays rejected — per-arm precision does not weaken the
        // intra-arm freeze.
        //   s: str = "hello"; v = s[0:2]
        //   if cond: consume(s); print(v)   # move precedes v's last read
        //   else: print(v)
        // → exactly one SourceProjected naming `s`.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sread = tb.var(s, str_ty, span);
        let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
        let vread_t = tb.var(v, view_ty, span);
        let pcall_t = tb.call(print, &[vread_t], &all_borrow(&[vread_t]), void, span);
        let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
        let vread_e = tb.var(v, view_ty, span);
        let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
        let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
        let ifs = tb.if_stmt(cond, &[cstmt, pstmt_t], &[], Some(&[pstmt_e]), void, span);
        let tir = tb.finish(&[decl, vdecl, ifs]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn freeze_holds_in_arm_when_view_read_after_join() {
        // Contract: a view read AFTER the join keeps the owner frozen in
        // every arm — the post-join read lies on every path, so no arm
        // may refine it away.
        //   s: str = "hello"; v = s[0:2]
        //   if cond: print(v)
        //   else: consume(s)      # v still live (read after the if)
        //   print(v)
        // → exactly one SourceProjected naming `s`.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let vread_t = tb.var(v, view_ty, span);
        let pcall_t = tb.call(print, &[vread_t], &all_borrow(&[vread_t]), void, span);
        let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
        let sread = tb.var(s, str_ty, span);
        let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
        let ifs = tb.if_stmt(cond, &[pstmt_t], &[], Some(&[cstmt]), void, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let tir = tb.finish(&[decl, vdecl, ifs, pstmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn move_in_arm_where_view_is_dead_ok_but_post_join_use_is_uam() {
        // Contract: on a path with no remaining view reads the owner may
        // move — but the conditional-move machinery still guards the
        // join: using the owner after the if is use-after-move.
        //   s: str = "hello"; v = s[0:2]
        //   if cond: consume(s)   # v is never read on this path → legal
        //   else: print(v)
        //   print(s)              # moved on the then-path → E0020
        // → exactly one UseAfterMove (and NO SourceProjected).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let consume = pool.intern_str("consume");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sread = tb.var(s, str_ty, span);
        let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
        let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
        let vread_e = tb.var(v, view_ty, span);
        let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
        let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
        let ifs = tb.if_stmt(cond, &[cstmt], &[], Some(&[pstmt_e]), void, span);
        let sread2 = tb.var(s, str_ty, span);
        let pcall = tb.call(print, &[sread2], &all_borrow(&[sread2]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let tir = tb.finish(&[decl, vdecl, ifs, pstmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::UseAfterMove),
            "expected UseAfterMove; got: {:?}",
            diags[0].code
        );
    }

    #[test]
    fn loop_deferred_view_stays_frozen_across_arm_without_read() {
        // Contract: per-arm refinement must NOT apply to loop-deferred
        // views — a later iteration re-reads the view through the
        // back-edge, so it is live on every arm's path inside the loop.
        //   s: str = "hello"; v = s[0:2]
        //   while true:
        //       if cond: str_push(&s, "!")   # v unread on this arm's path
        //       else: print(v)               # …but re-read next iteration
        // → exactly one SourceProjected naming `s`.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let str_push = pool.intern_str("str_push");
        let hello = pool.intern_str("hello");
        let bang = pool.intern_str("!");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let wcond = tb.bool_const(true, bool_ty, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sread = tb.var(s, str_ty, span);
        let suffix = tb.str_const(bang, str_ty, span);
        let push = tb.call(
            str_push,
            &[sread, suffix],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let ifs = tb.if_stmt(cond, &[push_stmt], &[], Some(&[pstmt]), void, span);
        let wl = tb.while_loop(wcond, &[ifs], void, span);
        let tir = tb.finish(&[decl, vdecl, wl]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn for_range_deferred_view_stays_frozen_across_arm_without_read() {
        // Same contract as loop_deferred_view_stays_frozen_across_arm_without_read
        // but through the ForRange path (analyze_for_range →
        // remove_loop_deferred_views at :3017 instead of the while-loop
        // call at :2994):
        //   s: str = "hello"; v = s[0:2]
        //   for i in range(0, 3):
        //       if cond: str_push(&s, "!")   # v unread on this arm's path
        //       else: print(v)               # …but re-read next iteration
        // → exactly one SourceProjected naming `s`.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let i = pool.intern_str("i");
        let print = pool.intern_str("print");
        let str_push = pool.intern_str("str_push");
        let hello = pool.intern_str("hello");
        let bang = pool.intern_str("!");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sread = tb.var(s, str_ty, span);
        let suffix = tb.str_const(bang, str_ty, span);
        let push = tb.call(
            str_push,
            &[sread, suffix],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let ifs = tb.if_stmt(cond, &[push_stmt], &[], Some(&[pstmt]), void, span);
        let i0b = tb.int_const(0, int_ty, span);
        let i3 = tb.int_const(3, int_ty, span);
        let fr = tb.for_range(i, i0b, i3, &[ifs], void, span);
        let tir = tb.finish(&[decl, vdecl, fr]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn view_in_loop_body_converges() {
        // s: str = "hello"; while true: v = s[0:2]; print(v)
        // → converges (full-tuple comparison); no spurious
        //   SourceProjected on the second iteration; s freed after the
        //   loop.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let wl = tb.while_loop(cond, &[vdecl, pstmt], void, span);
        let tir = tb.finish(&[decl, wl]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sidecar = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sink.is_empty(),
            "expected no diagnostics (no spurious SourceProjected)"
        );
        // s's Free is anchored after the loop (its last use — through
        // the projection — is inside the loop body; the conditional
        // re-anchor moves it to the loop exit).
        assert!(
            sidecar
                .free_schedule
                .iter()
                .any(|fp| fp.target == lit && fp.after == wl && fp.branch.is_none()),
            "expected s's Free anchored after the loop; got: {:?}",
            sidecar.free_schedule
        );
    }

    #[test]
    fn loop_deferred_view_survives_if_join_prune() {
        // Regression test (review Critical): prune_branch_dead_projections
        // must not kill a loop-deferred view at the if join.
        //   s: str = "hello"; v = s[0:2]
        //   while true:
        //       if true: print(v)     # v's last read → loop-deferred (P4)
        //       str_push(&s, "!")     # mutates the owner while v is live
        // → exactly one SourceProjected naming `s` (P2). Before the fix
        //   the if-join prune emptied live_projections[s] mid-loop-body,
        //   silently accepting a mutation whose realloc later iterations
        //   would read through v's stale pointer — the UAF class P2
        //   exists to reject.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let str_push = pool.intern_str("str_push");
        let hello = pool.intern_str("hello");
        let bang = pool.intern_str("!");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let while_cond = tb.bool_const(true, bool_ty, span);
        let if_cond = tb.bool_const(true, bool_ty, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let ifs = tb.if_stmt(if_cond, &[pstmt], &[], None, void, span);
        let sread = tb.var(s, str_ty, span);
        let suffix = tb.str_const(bang, str_ty, span);
        let push = tb.call(
            str_push,
            &[sread, suffix],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
        let wl = tb.while_loop(while_cond, &[ifs, push_stmt], void, span);
        let tir = tb.finish(&[decl, vdecl, wl]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn free_schedule_is_deterministic() {
        // Build a TIR with several owners + views; run `check` twice;
        // assert identical free_schedule.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s1 = pool.intern_str("s1");
        let s2 = pool.intern_str("s2");
        let s3 = pool.intern_str("s3");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        // s1 = "a"; s2 = "b"; v = s1[0:1]; print(v); print(s2);
        // s3 = "c" (dead store); if true: print(s1) else: print("x");
        // print("tmp")
        let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
        let decl1 = tb.var_decl(s1, false, str_ty, lit_a, span);
        let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
        let decl2 = tb.var_decl(s2, false, str_ty, lit_b, span);
        let base = tb.var(s1, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i1 = tb.int_const(1, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i1), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let vread = tb.var(v, view_ty, span);
        let pv = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pv_stmt = tb.unary(TirTag::ExprStmt, void, pv, span);
        let r2 = tb.var(s2, str_ty, span);
        let ps2 = tb.call(print, &[r2], &all_borrow(&[r2]), void, span);
        let ps2_stmt = tb.unary(TirTag::ExprStmt, void, ps2, span);
        let lit_c = tb.str_const(pool.intern_str("c"), str_ty, span);
        let decl3 = tb.var_decl(s3, false, str_ty, lit_c, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let r1 = tb.var(s1, str_ty, span);
        let ps1 = tb.call(print, &[r1], &all_borrow(&[r1]), void, span);
        let ps1_stmt = tb.unary(TirTag::ExprStmt, void, ps1, span);
        let xlit = tb.str_const(pool.intern_str("x"), str_ty, span);
        let px = tb.call(print, &[xlit], &all_borrow(&[xlit]), void, span);
        let px_stmt = tb.unary(TirTag::ExprStmt, void, px, span);
        let ifs = tb.if_stmt(cond, &[ps1_stmt], &[], Some(&[px_stmt]), void, span);
        let tlit = tb.str_const(pool.intern_str("tmp"), str_ty, span);
        let pt = tb.call(print, &[tlit], &all_borrow(&[tlit]), void, span);
        let pt_stmt = tb.unary(TirTag::ExprStmt, void, pt, span);
        let tir = tb.finish(&[decl1, decl2, vdecl, pv_stmt, ps2_stmt, decl3, ifs, pt_stmt]);

        let run = |tir: &Tir| {
            let mut sink = DiagSink::new();
            let mut sidecar = check(std::slice::from_ref(tir), &pool, &mut sink);
            let sc = take_function_sidecar(&mut sidecar, 0);
            sc.free_schedule
                .iter()
                .map(|fp| (fp.after.raw(), fp.target.raw(), fp.branch.map(|b| b.0)))
                .collect::<Vec<_>>()
        };
        let first = run(&tir);
        let second = run(&tir);
        assert!(!first.is_empty(), "expected a non-empty free schedule");
        assert_eq!(
            first, second,
            "free_schedule must be deterministic across runs"
        );
    }

    #[test]
    fn viewofstr_arg_counts_as_borrow_rule7() {
        // T5/T7 carry-forward: fn two(inout a: str, b: strview) called as
        // two(&s, s) — sema wraps the second arg in ViewOfStr; the
        // Rule-7 borrow partition must look through the conversion and
        // diagnose the aliasing (E0032).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let void = pool.void();
        let main = pool.intern_str("main");
        let two = pool.intern_str("two");
        let s = pool.intern_str("s");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let inout_arg = tb.var(s, str_ty, span);
        let view_base = tb.var(s, str_ty, span);
        let view_arg = tb.view_of_str(view_base, view_ty, span);
        let call = tb.call(
            two,
            &[inout_arg, view_arg],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::MutableAliasingViolation),
            "expected E0032 MutableAliasingViolation; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn viewofstr_read_counts_as_use_dead_store() {
        // T5/T7 carry-forward: an owned `str` used ONLY via a ViewOfStr
        // conversion must count as used — no W0001. Here s's sole read
        // is the owned side of the mixed `str == strview` comparison:
        //   s: str = "hi"; other: str = "yo"; if s == other[0:1]: print("x")
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let other = pool.intern_str("other");
        let print = pool.intern_str("print");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_hi = tb.str_const(pool.intern_str("hi"), str_ty, span);
        let decl_s = tb.var_decl(s, false, str_ty, lit_hi, span);
        let lit_yo = tb.str_const(pool.intern_str("yo"), str_ty, span);
        let decl_o = tb.var_decl(other, false, str_ty, lit_yo, span);
        let obase = tb.var(other, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i1 = tb.int_const(1, int_ty, span);
        let sl = tb.slice(obase, Some(i0), Some(i1), view_ty, span);
        let sread = tb.var(s, str_ty, span);
        let vos = tb.view_of_str(sread, view_ty, span);
        let eq = tb.binary(TirTag::StrCmpEq, bool_ty, vos, sl, span);
        let xlit = tb.str_const(pool.intern_str("x"), str_ty, span);
        let pcall = tb.call(print, &[xlit], &all_borrow(&[xlit]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let ifs = tb.if_stmt(eq, &[pstmt], &[], None, void, span);
        let tir = tb.finish(&[decl_s, decl_o, ifs]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.is_empty(),
            "expected no diagnostics — the ViewOfStr read must count as a use of s"
        );
    }

    #[test]
    fn viewasstr_arg_counts_as_borrow_rule7() {
        // P6' carry-forward: fn two(inout a: str, b: str) called as
        // two(&s, s[0:1]) — sema wraps the second arg in ViewAsStr; the
        // Rule-7 borrow partition must look through the conversion and
        // diagnose the aliasing (E0032), same as the ViewOfStr case.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let two = pool.intern_str("two");
        let s = pool.intern_str("s");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let inout_arg = tb.var(s, str_ty, span);
        let slice_base = tb.var(s, str_ty, span);
        let zero = tb.int_const(0, int_ty, span);
        let one = tb.int_const(1, int_ty, span);
        let slice = tb.slice(slice_base, Some(zero), Some(one), view_ty, span);
        let reborrow = tb.view_as_str(slice, str_ty, span);
        let call = tb.call(
            two,
            &[inout_arg, reborrow],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::MutableAliasingViolation),
            "expected E0032 MutableAliasingViolation; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn viewasstr_reborrow_is_call_scoped() {
        // P6': the re-borrow lives only for the call's duration — the
        // root owner can still be moved afterwards (no freeze, no
        // aliasing), unlike a bound slice projection.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let show = pool.intern_str("show");
        let eat = pool.intern_str("eat");
        let s = pool.intern_str("s");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let slice_base = tb.var(s, str_ty, span);
        let zero = tb.int_const(0, int_ty, span);
        let one = tb.int_const(1, int_ty, span);
        let slice = tb.slice(slice_base, Some(zero), Some(one), view_ty, span);
        let reborrow = tb.view_as_str(slice, str_ty, span);
        let show_call = tb.call(show, &[reborrow], &[ParamMode::Borrow], void, span);
        let show_stmt = tb.unary(TirTag::ExprStmt, void, show_call, span);
        // `s` moved later in the caller — fine: the re-borrow ended
        // with the `show` call.
        let moved = tb.var(s, str_ty, span);
        let eat_call = tb.call(eat, &[moved], &[ParamMode::Move], void, span);
        let eat_stmt = tb.unary(TirTag::ExprStmt, void, eat_call, span);
        let tir = tb.finish(&[decl, show_stmt, eat_stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.is_empty(),
            "expected no diagnostics — the re-borrow is call-scoped; got: {:?}",
            sink.into_diags()
        );
    }

    #[test]
    fn str_materialize_result_is_fresh_owner() {
        // M8.4.1.2: `fn show(text: strview) -> str: return str(text)` —
        // the materialized copy is a fresh owner by construction (the
        // str-returning-Call seeding), so returning it is the sanctioned
        // escape fix: no diagnostics, no ownership special-casing.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::{ParamMode, TirBuilder, TirData, TirParam, TirTag};

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let show = pool.intern_str("show");
        let text = pool.intern_str("text");
        let materialize = pool.intern_str("__ryo_str_from_view");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(
            show,
            vec![TirParam {
                name: text,
                ty: view_ty,
                mode: ParamMode::Borrow,
                span,
            }],
            str_ty,
            span,
        );
        let arg = tb.var(text, view_ty, span);
        let copy = tb.call(materialize, &[arg], &[ParamMode::Borrow], str_ty, span);
        let ret = tb.push_typed(TirTag::Return, TirData::UnOp(copy), str_ty, span);
        let tir = tb.finish(&[ret]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.is_empty(),
            "materialize-and-return must be clean — the copy is a fresh owner; got: {:?}",
            sink.into_diags()
        );
    }

    #[test]
    fn str_materialize_arg_counts_as_borrow_rule7() {
        // M8.4.1.2: fn two(inout a: str, b: str) called as
        // two(&s, str(s[0:1])) — the materialization READS the view's
        // buffer at call time, so it counts as an immutable borrow of
        // `s` for the call's duration (E4): the Rule-7 partition must
        // look through `__ryo_str_from_view` to the view's root, exactly
        // like the ViewAsStr case (viewasstr_arg_counts_as_borrow_rule7).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let two = pool.intern_str("two");
        let s = pool.intern_str("s");
        let hello = pool.intern_str("hello");
        let materialize = pool.intern_str("__ryo_str_from_view");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let inout_arg = tb.var(s, str_ty, span);
        let slice_base = tb.var(s, str_ty, span);
        let zero = tb.int_const(0, int_ty, span);
        let one = tb.int_const(1, int_ty, span);
        let slice = tb.slice(slice_base, Some(zero), Some(one), view_ty, span);
        let copy = tb.call(materialize, &[slice], &[ParamMode::Borrow], str_ty, span);
        let call = tb.call(
            two,
            &[inout_arg, copy],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::MutableAliasingViolation),
            "expected E0032 MutableAliasingViolation; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn loop_view_created_after_owner_mutation_dies_within_iteration() {
        // A view VarDecl AFTER an inout-consume of the owner inside the
        // SAME while body:
        //   s: str = "hello"
        //   while true:
        //       str_push(&s, "!")   # mutates the owner
        //       v = s[0:2]          # projection created after the consume
        //       print(v)            # v's only read, same iteration
        // → NO diagnostic. The view is created and read inside the same
        // loop body, so it is NOT loop-deferred (deferral requires the
        // last read in a loop the creation is outside of — see
        // collect_view_liveness); it dies at `print(v)` within pass 1,
        // the back-edge state tuple is unchanged, and no re-walk fires.
        // That is the sound outcome: every iteration's `str_push` runs
        // before that iteration's fresh slice is taken, so no stale
        // view pointer is ever read. (The unsound sibling — view
        // created OUTSIDE the loop, mutated inside — is covered by
        // `loop_deferred_view_survives_if_join_prune`.)
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let str_push = pool.intern_str("str_push");
        let hello = pool.intern_str("hello");
        let bang = pool.intern_str("!");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sread = tb.var(s, str_ty, span);
        let suffix = tb.str_const(bang, str_ty, span);
        let push = tb.call(
            str_push,
            &[sread, suffix],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let wl = tb.while_loop(cond, &[push_stmt, vdecl, pstmt], void, span);
        let tir = tb.finish(&[decl, wl]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(
            sink.is_empty(),
            "expected no diagnostics — the view dies within each iteration, \
             before the next iteration's str_push; got: {:?}",
            sink.into_diags()
        );
    }

    #[test]
    fn loop_view_live_at_back_edge_flags_earlier_mutation_on_rewalk() {
        // Pins the full-tuple convergence check's re-walk DISCOVERY
        // path (the sibling acceptance test above pins the converge
        // side): a body-created view that is STILL LIVE at the
        // back-edge forces pass 2, and only pass 2
        // sees the projection at the earlier owner-consume.
        //   s: str = "hello"
        //   suffix: str = "!"
        //   while true:
        //       str_push(&s, suffix)  # pass 1: no projection exists yet
        //       v = s[0:2]            # registers the projection
        // v is never read: an unread view has no last use, so its
        // projection lives to scope end (P4) and is non-empty at the
        // back-edge → the live-projection leg of the state tuple
        // differs → re-walk → pass 2 walks `str_push` with the
        // projection live → exactly one SourceProjected (E0035) naming
        // `s`. The mutation IS unsound here: iteration 2's push
        // reallocs while v (never killed by a read) points into the
        // old buffer.
        //
        // The suffix is deliberately bound OUTSIDE the loop: a StrConst
        // inside the body would enter `states` as a fresh Valid temp
        // and flip the owner-state leg of the tuple, forcing the
        // re-walk even without the projection-emptiness comparison.
        // With the body kept allocation-free, the projection leg is the
        // ONLY re-walk trigger — verified by mutation: skipping the
        // live-projection comparison in `states_differ_snapshot` makes
        // this test fail with 0 diagnostics. A refactor re-narrowing
        // the convergence comparison (e.g. back to Moved-ness only)
        // drops the re-walk and MUST fail this test.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let suffix = pool.intern_str("suffix");
        let v = pool.intern_str("v");
        let str_push = pool.intern_str("str_push");
        let hello = pool.intern_str("hello");
        let bang = pool.intern_str("!");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let bang_lit = tb.str_const(bang, str_ty, span);
        let suffix_decl = tb.var_decl(suffix, false, str_ty, bang_lit, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let sread = tb.var(s, str_ty, span);
        let suffix_read = tb.var(suffix, str_ty, span);
        let push = tb.call(
            str_push,
            &[sread, suffix_read],
            &[ParamMode::Inout, ParamMode::Borrow],
            void,
            span,
        );
        let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let wl = tb.while_loop(cond, &[push_stmt, vdecl], void, span);
        let tir = tb.finish(&[decl, suffix_decl, wl]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        assert!(
            diags[0].message.contains("`s`"),
            "expected the diagnostic to name `s`; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn diverged_loop_writes_branch_gated_free_exactly_once() {
        // Regression: speculative sidecar writes from the loop
        // fixed-point's first walk must not leak into the real sidecar.
        //   m: str = "m"
        //   while true:
        //       x: str = "x"     # fresh owner every iteration
        //       if true:
        //           consume(x)   # then arm moves x
        //       else:
        //           print(x)     # else arm keeps x Valid
        //       consume(m)       # moved without rebinding → divergence
        //
        // m flips Valid → Moved across the back-edge, so the state
        // tuple differs and the diverged path runs. The if schedules a
        // branch-gated Free for x on the else arm. Before the fix the
        // scratch walk wrote that FreePoint into the REAL sidecar and
        // the re-walk wrote it again under freshly minted BranchIds —
        // two entries for one arm, the first gated on a BranchId that
        // `if_branches` no longer records and codegen never activates.
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let m = pool.intern_str("m");
        let x = pool.intern_str("x");
        let consume = pool.intern_str("consume");
        let print = pool.intern_str("print");
        let m_lit = pool.intern_str("m");
        let x_lit = pool.intern_str("x");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit_m = tb.str_const(m_lit, str_ty, span);
        let decl_m = tb.var_decl(m, false, str_ty, lit_m, span);

        let cond_w = tb.bool_const(true, bool_ty, span);
        let lit_x = tb.str_const(x_lit, str_ty, span);
        let decl_x = tb.var_decl(x, false, str_ty, lit_x, span);
        let cond_i = tb.bool_const(true, bool_ty, span);
        let x_then = tb.var(x, str_ty, span);
        let consume_then = tb.call(consume, &[x_then], &[ParamMode::Move], void, span);
        let then_stmt = tb.unary(TirTag::ExprStmt, void, consume_then, span);
        let x_else = tb.var(x, str_ty, span);
        let print_else = tb.call(print, &[x_else], &all_borrow(&[x_else]), void, span);
        let else_stmt = tb.unary(TirTag::ExprStmt, void, print_else, span);
        let if_inst = tb.if_stmt(cond_i, &[then_stmt], &[], Some(&[else_stmt]), void, span);
        let m_read = tb.var(m, str_ty, span);
        let consume_m = tb.call(consume, &[m_read], &[ParamMode::Move], void, span);
        let consume_m_stmt = tb.unary(TirTag::ExprStmt, void, consume_m, span);
        let wl = tb.while_loop(cond_w, &[decl_x, if_inst, consume_m_stmt], void, span);
        let tir = tb.finish(&[decl_m, wl]);

        let mut sink = DiagSink::new();
        let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sc, 0);

        // Precondition: the loop actually diverged — the re-walk sees
        // `consume(m)` with m already Moved and reports UAM once. If
        // this ever fails, the fixed-point no longer takes the diverged
        // path and the assertions below pass vacuously.
        let uam = sink
            .into_diags()
            .iter()
            .filter(|d| matches!(d.code, DiagCode::UseAfterMove))
            .count();
        assert_eq!(
            uam, 1,
            "expected exactly one UAM from the diverged re-walk; got {uam}"
        );

        // Exactly ONE branch-gated Free for the if's else arm,
        // gated on the BranchId `if_branches` actually records.
        assert_eq!(
            sc.if_branches.len(),
            1,
            "one if → one entry set; got {:?}",
            sc.if_branches
        );
        let ids = sc
            .if_branches
            .get(&if_inst)
            .expect("if_branches entry for the loop-body if");
        let gated: Vec<_> = sc
            .free_schedule
            .iter()
            .filter(|fp| fp.branch.is_some())
            .collect();
        assert_eq!(
            gated.len(),
            1,
            "diverged loop must schedule the else-arm Free exactly once; got {gated:?}"
        );
        assert_eq!(
            gated[0].branch, ids.else_branch,
            "the gated Free must reference the live else BranchId; if_branches = {ids:?}"
        );
    }

    #[test]
    fn view_and_move_args_same_owner_rejected() {
        // fn two(a: strview, move b: str) called as two(s[0:1], s) —
        // both args share root owner `s`. The view arg borrows the root
        // for the whole call (E4), so the move in the same call is a
        // P2 freeze violation: exactly one SourceProjected (E0035).
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let void = pool.void();
        let main = pool.intern_str("main");
        let two = pool.intern_str("two");
        let s = pool.intern_str("s");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i1 = tb.int_const(1, int_ty, span);
        let view_arg = tb.slice(base, Some(i0), Some(i1), view_ty, span);
        let move_arg = tb.var(s, str_ty, span);
        let call = tb.call(
            two,
            &[view_arg, move_arg],
            &[ParamMode::Borrow, ParamMode::Move],
            void,
            span,
        );
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, stmt]);

        let mut sink = DiagSink::new();
        check(std::slice::from_ref(&tir), &pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(
            diags.len(),
            1,
            "expected exactly one diagnostic; got: {diags:?}"
        );
        assert!(
            matches!(diags[0].code, DiagCode::SourceProjected),
            "expected SourceProjected; got: {:?}",
            diags[0].code
        );
        // Diagnostic-quality gap: the message names "value", not `s` —
        // the view/move-overlap path resolves the name via
        // `owner_name_for_diag`, which inspects the owner's initializer
        // (a StrConst) and falls back to "value", unlike the Rule-7
        // E0032 path that scans the call args for the `Var` read
        // (`rule7_owner_name`). Pinned as-is; a future fix should name
        // the binding.
        assert!(
            diags[0].message.contains("value"),
            "expected the current message wording; got: {}",
            diags[0].message
        );
    }

    #[test]
    fn p5_view_read_inside_if_arm_anchors_free_at_if_exit() {
        // P5 deferral ACROSS a branch — distinct from the plain last-use
        // lift in `last_use_inside_if_anchors_after_if`: the owner's own
        // last read is the slice creation OUTSIDE the if; only the
        // projection's last read is inside the then-arm. P5 defers the
        // owner's destruction to that read (final spec §3.2), and the
        // conditional re-anchor must lift the FreePoint to the IfStmt
        // exit — anchoring inside the arm would leak on the not-taken
        // path.
        //   s: str = "hello"
        //   v = s[0:2]
        //   if true: print(v)
        use chumsky::span::{SimpleSpan, Span as _};
        use ryo_core::tir::TirBuilder;

        let mut pool = InternPool::new();
        let str_ty = pool.str_();
        let view_ty = pool.str_view();
        let int_ty = pool.int();
        let bool_ty = pool.bool_();
        let void = pool.void();
        let main = pool.intern_str("main");
        let s = pool.intern_str("s");
        let v = pool.intern_str("v");
        let print = pool.intern_str("print");
        let hello = pool.intern_str("hello");
        let span = SimpleSpan::new((), 0..0);

        let mut tb = TirBuilder::new(main, vec![], void, span);
        let lit = tb.str_const(hello, str_ty, span);
        let decl = tb.var_decl(s, false, str_ty, lit, span);
        let base = tb.var(s, str_ty, span);
        let i0 = tb.int_const(0, int_ty, span);
        let i2 = tb.int_const(2, int_ty, span);
        let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
        let vdecl = tb.var_decl(v, false, view_ty, sl, span);
        let cond = tb.bool_const(true, bool_ty, span);
        let vread = tb.var(v, view_ty, span);
        let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
        let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
        let ifs = tb.if_stmt(cond, &[pstmt], &[], None, void, span);
        let tir = tb.finish(&[decl, vdecl, ifs]);

        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        assert!(
            sink.is_empty(),
            "expected no diagnostics; got: {:?}",
            sink.into_diags()
        );
        assert!(
            sc.free_schedule
                .iter()
                .any(|fp| fp.target == lit && fp.after == ifs && fp.branch.is_none()),
            "expected s's Free anchored at the IfStmt exit (P5 deferral across the branch); schedule = {:?}",
            sc.free_schedule
        );
        assert!(
            !sc.free_schedule
                .iter()
                .any(|fp| fp.target == lit && fp.after != ifs),
            "no Free for s may anchor inside the arm (leaks on the not-taken path); schedule = {:?}",
            sc.free_schedule
        );
    }

    // ---------- W0003 RedundantMaterialize, case B (M8.4.1.2) ----------

    /// Lex + parse + astgen + sema + ownership on a source snippet —
    /// the full front-end, so the case-B tests read like the programs
    /// users write. Returns every diagnostic from all four stages.
    fn check_src(input: &str) -> Vec<Diag> {
        use chumsky::Parser as _;
        use chumsky::input::Input as _;
        let mut pool = InternPool::new();
        let mut lex_sink = DiagSink::new();
        let tokens = crate::lexer::lex(input, &mut pool, &mut lex_sink);
        assert!(
            !lex_sink.has_errors(),
            "lex errors: {:?}",
            lex_sink.into_diags()
        );
        let token_stream = tokens[..].split_token_span((0..input.len()).into());
        let mut ast = ryo_core::ast::Ast::new();
        crate::parser::program_parser()
            .parse_with_state(token_stream, &mut ast)
            .into_result()
            .expect("parse ok");
        let mut sink = DiagSink::new();
        let uir = crate::astgen::generate(&ast, &mut pool, &mut sink);
        let tirs = crate::sema::analyze(
            &uir,
            &mut pool,
            &mut sink,
            input,
            std::path::Path::new("<test>"),
        );
        check(&tirs, &pool, &mut sink);
        sink.into_diags()
    }

    fn w0003_count(diags: &[Diag]) -> usize {
        diags
            .iter()
            .filter(|d| d.code == DiagCode::RedundantMaterialize)
            .count()
    }

    #[test]
    fn w0003_bound_materialize_never_escapes_warns() {
        // W0003 case B: `x = str(view)` where `x` is only borrow-read
        // (`print(x)` — a use the view itself could have served) and the
        // slice's root owner is never touched again: the allocation is
        // redundant.
        let diags =
            check_src("fn main():\n\ts: str = \"hi\"\n\tx: str = str(s[0:1])\n\tprint(x)\n");
        assert_eq!(
            w0003_count(&diags),
            1,
            "expected exactly one W0003; got: {diags:?}"
        );
    }

    #[test]
    fn w0003_defensive_copy_before_source_mutation_does_not_warn() {
        // The defensive-copy exception: the source is mutated AFTER the
        // materialize point, so the owned copy genuinely outlives its
        // view (ring-buffer reuse shape) — no warning.
        let diags = check_src(
            "fn main():\n\tmut s: str = \"hi\"\n\tx: str = str(s[0:1])\n\tstr_push(&s, \"!\")\n\tprint(x)\n",
        );
        assert_eq!(
            w0003_count(&diags),
            0,
            "defensive copy must not warn; got: {diags:?}"
        );
    }

    #[test]
    fn w0003_materialize_returned_does_not_warn() {
        // A bound copy later consumed by `return x` escapes — the
        // `states == Moved` escape check must keep W0003 silent. (The
        // unbound `return str(text)` shape never reaches the case-B
        // analysis: return operands are not collected as materialize
        // sites.)
        let diags =
            check_src("fn first(text: strview) -> str:\n\tx: str = str(text)\n\treturn x\n");
        assert_eq!(
            w0003_count(&diags),
            0,
            "escaping copy must not warn; got: {diags:?}"
        );
    }

    #[test]
    fn w0003_strview_param_root_does_not_warn() {
        // Conservative direction: the view's root owner is the caller's
        // buffer, so `projection_root` yields None for a `strview`
        // parameter and case B must stay silent — the pass cannot judge
        // mutations it cannot see. Unlike
        // `w0003_materialize_returned_does_not_warn`, the copy below only
        // borrow-escapes (`print(x)`), so the escape check does NOT fire
        // first: the unresolvable root is the only thing suppressing the
        // warning.
        let diags = check_src("fn f(text: strview):\n\tx: str = str(text)\n\tprint(x)\n");
        assert_eq!(
            w0003_count(&diags),
            0,
            "strview-parameter root must not warn; got: {diags:?}"
        );
    }

    #[test]
    fn w0003_defensive_copy_before_inout_pass_does_not_warn() {
        // Defensive-copy exception, `inout`-pass hazard kind: the source
        // root is `inout`-passed AFTER the materialize point, so the
        // callee may mutate the buffer the view aliases — the owned copy
        // is a genuine snapshot, not a redundant allocation. (`owner_hazards`
        // records inout passes and mutations alike; the mutation kind is
        // pinned by `w0003_defensive_copy_before_source_mutation_does_not_warn`.)
        let diags = check_src(
            "fn eat(inout a: str):\n\tprint(a)\n\nfn main():\n\tmut s: str = \"hi\"\n\tx: str = str(s[0:1])\n\tprint(x)\n\teat(&s)\n",
        );
        assert_eq!(
            w0003_count(&diags),
            0,
            "defensive copy before an inout pass must not warn; got: {diags:?}"
        );
    }
}
