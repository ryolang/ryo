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

use crate::diag::{Diag, DiagCode, DiagSink};
use crate::tir::{Span, Tir, TirData, TirRef, TirTag};
use crate::types::{InternPool, StringId, TypeId, TypeKind};
use std::collections::{HashMap, HashSet};

/// Identifies a specific arm of an `IfStmt` (and, future, `Match`).
/// Assigned by the ownership pass; codegen maps each `BranchId` to a
/// concrete Cranelift `Block` as it lowers if/else regions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BranchId(pub u32);

/// One scheduled Free. Codegen emits `ryo_str_free(ptr, cap)` after
/// the instruction at `after`, gated by `branch` (None =
/// unconditional, Some = only inside that arm or a descendant).
#[derive(Clone, Debug)]
#[allow(dead_code)] // fields read by Task 7+ Free emission
pub struct FreePoint {
    pub after: TirRef,
    pub target: TirRef,
    pub span: Span,
    pub branch: Option<BranchId>,
}

/// Per-`IfStmt` mapping from arm position to its assigned [`BranchId`].
/// Codegen uses this to push the right `BranchId` onto `branch_stack`
/// as it lowers each arm, so a branch-gated `FreePoint` only fires
/// inside the arm that ended with the owner still `Valid`.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // fields read by codegen's branch_stack
pub struct IfBranchIds {
    pub then_branch: BranchId,
    pub elif_branches: Vec<BranchId>,
    pub else_branch: Option<BranchId>,
}

/// Side-table produced by the ownership pass alongside diagnostics.
/// Codegen consults it to decide where to emit `ryo_str_free` calls.
/// The TIR itself is never mutated — index stability is load-bearing
/// for `inst_values` memoisation in `codegen.rs`.
#[derive(Default, Debug, Clone)]
#[allow(dead_code)] // fields read by Task 7+ Free emission
pub struct OwnershipSidecar {
    /// Frees anchored after specific instructions.
    pub free_schedule: Vec<FreePoint>,
    /// Reassignment Frees. Key: the `Assign` instruction's `TirRef`.
    /// Value: the `TirRef` whose buffer must be freed *before* the new
    /// fat-pointer triple is stored into the binding's `StrLocals`.
    pub free_on_reassign: HashMap<TirRef, TirRef>,
    /// `BranchId` assignments per `IfStmt`. Codegen consults this when
    /// lowering if/elif/else to know which `BranchId` to push onto
    /// `branch_stack` for each arm.
    pub if_branches: HashMap<TirRef /* IfStmt inst */, IfBranchIds>,
}

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
    pub states: HashMap<TirRef, OwnerState>,
    pub current_owner: HashMap<StringId, TirRef>,
    pub origin: HashMap<TirRef, Option<TirRef>>,
    /// VarDecls of Move-typed values, keyed by the underlying owner
    /// `TirRef`. Cleared when the binding is read (`Var`) or consumed
    /// (move/return). Whatever remains at function end is a dead
    /// store — surfaced as W0001 + a Free anchored after the
    /// declaring/assigning instruction. The third tuple element is
    /// the `VarDecl`/`Assign` instruction's own `TirRef`, used as
    /// the anchor for the dead-store Free.
    pub pending_dead_store: HashMap<TirRef, (StringId, Span, TirRef /* decl_inst */)>,

    /// SSA values that allocated heap-owned strings during the
    /// forward walk: `StrConst`, `StrConcat`, and Move-typed `Call`
    /// results. Used by the anonymous-temporary-free pass to identify
    /// candidates for scheduling. A temp_owner that ends up bound to
    /// a `VarDecl`/`Assign` (see `named_owners`) is freed via the
    /// last-use pass instead.
    pub temp_owners: HashSet<TirRef>,

    /// Subset of `temp_owners` that became the underlying value of a
    /// named binding via `VarDecl` or `Assign`. The anonymous-temp
    /// pass excludes these — they're handled by the named-binding
    /// last-use pass (Task 3).
    pub named_owners: HashSet<TirRef>,

    /// Per-`Var`-read snapshot of the owner that was live at the
    /// program point of the read. Populated during the forward walk
    /// (`visit_expr`'s `Var` arm) and consulted by `collect_last_uses`
    /// instead of resolving through `current_owner`'s end-of-function
    /// state — which would misroute reads that precede a `mut`
    /// reassignment to the post-rebind owner. For Move-typed reads
    /// this anchors the last-use Free to the correct allocation.
    pub owner_at_read: HashMap<TirRef, TirRef>,

    /// Monotonic `BranchId` allocator. Bumped each time
    /// `analyze_if_stmt` enters an arm (then / each elif / else) so
    /// the resulting ids are unique across the function body.
    pub next_branch_id: u32,
}

impl Ownership {
    /// Conservatively merge per-branch lattices into `self`. For each
    /// tracked `TirRef`, if any branch left it `Moved` the merged
    /// state is `Moved` — that way a value consumed on only one
    /// branch is still treated as moved at the join point and a
    /// post-`if` use trips E0020. Non-`Moved` states pick the first
    /// observed branch state. `current_owner` entries from any branch
    /// are preserved so reseats inside a branch survive the join.
    fn merge_branches(&mut self, branches: &[&Ownership]) {
        // Snapshot pre-branch (name, owner) pairs before we start
        // touching `self.states`. After the per-TirRef merge below
        // we revisit each pre-branch binding and recompute its state
        // through whichever owner each branch ended on, so a branch
        // that reseats `current_owner[name]` to a fresh TirRef still
        // contributes its end-of-branch state to the merged binding.
        // Without this, post-`if` reads of `name` resolve through the
        // pre-branch owner whose state reflects only what happened to
        // *that* TirRef, missing reseats inside branches.
        let pre_branch_owners: Vec<(StringId, TirRef)> =
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
        for b in branches {
            for (name, owner) in &b.current_owner {
                self.current_owner.entry(*name).or_insert(*owner);
            }
            for (k, v) in &b.origin {
                self.origin.entry(*k).or_insert(*v);
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
                merged = Some(match (&merged, &state_b) {
                    (None, _) => state_b,
                    (Some(OwnerState::Moved { .. }), _) => merged.clone().unwrap(),
                    (_, OwnerState::Moved { .. }) => state_b,
                    (Some(_), _) => merged.clone().unwrap(),
                });
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
        let pre_branch_keys: HashSet<TirRef> = self.pending_dead_store.keys().copied().collect();
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
    // Name-keyed lookup so call sites can read the callee's per-
    // parameter `is_move` flags. Builtins (`print`, `assert`,
    // `int_to_str`) are not in this map and default to borrowing.
    let by_name: HashMap<StringId, &Tir> = tirs.iter().map(|t| (t.name, t)).collect();
    let mut sidecar = OwnershipSidecar::default();
    for tir in tirs {
        analyze_function(tir, pool, sink, &by_name, &mut sidecar);
    }
    sidecar
}

fn analyze_function(
    tir: &Tir,
    pool: &InternPool,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
) {
    let mut own = Ownership::default();

    // Initialise per-parameter state. Move-typed params start at
    // `Valid` (the callee owns them); borrowed params start at
    // `Borrowed`. Copy-typed params skip the lattice entirely.
    for param in &tir.params {
        if !needs_tracking(param.ty, pool) {
            continue;
        }
        let synthetic = synthetic_param_ref(param.name);
        let state = if param.is_move {
            OwnerState::Valid
        } else {
            OwnerState::Borrowed
        };
        own.states.insert(synthetic, state);
        own.current_owner.insert(param.name, synthetic);
    }

    for stmt in tir.body_stmts() {
        analyze_stmt(tir, pool, &mut own, sink, by_name, sidecar, stmt);
    }

    // Backward liveness: for every owner still `Valid` at function exit
    // (i.e., not moved out via return / move-typed call argument /
    // reassign), find its last reading instruction and schedule a Free
    // anchored after it. The reverse walk records the *first* time each
    // owner is observed (which is its last use in program order). The
    // forward walk's final `current_owner` map encodes "which TirRef
    // owns the binding at function exit"; reads of a binding in the
    // body always alias *some* owner that the forward walk classified.
    // For any owner whose state is `Moved` at function exit (e.g. the
    // pre-reassign owner of a rebound binding), the final-state filter
    // below skips it — so anchoring last-use via the final
    // `current_owner` resolution is sound for the M8.1 pattern set.
    let body_stmts = tir.body_stmts();
    let mut last_use: HashMap<TirRef, TirRef> = HashMap::new();
    for &stmt in body_stmts.iter().rev() {
        collect_last_uses(tir, pool, &own, stmt, &mut last_use);
    }
    // Owners already covered by `free_on_reassign` must not be
    // scheduled again via the last-use pass — that would double-free
    // the same allocation. (Pre-rebind owners are now reachable from
    // last_use after the `owner_at_read` snapshot fix; without this
    // guard a pre-rebind owner would receive both a reassign-Free and
    // a last-use-Free.)
    let reassign_targets: HashSet<TirRef> = sidecar.free_on_reassign.values().copied().collect();
    for (owner, state) in &own.states {
        if !matches!(state, OwnerState::Valid) {
            continue;
        }
        // Skip parameter synthetic refs (see `synthetic_param_ref`):
        // borrowed/owned-by-callee parameters are the caller's
        // responsibility to free, not ours. Synthetic refs encode
        // names near `u32::MAX`; real instruction refs are
        // structurally `<< u32::MAX / 2`.
        if is_synthetic_param_ref(*owner) {
            continue;
        }
        if reassign_targets.contains(owner) {
            continue;
        }
        if let Some(&after) = last_use.get(owner) {
            sidecar.free_schedule.push(FreePoint {
                after,
                target: *owner,
                span: tir.span(*owner),
                branch: None,
            });
        }
        // Owners with no last_use are dead stores — handled in Task 5.
    }

    // Loop-exit Frees: for every `break`/`continue` inside a loop
    // body, schedule a Free for any pre-loop owner still `Valid` at
    // the jump site (modulo a post-loop last-use, which already
    // schedules its own Free).
    //
    // Known limitation (M8.1): owners *declared inside* the loop body
    // that hit `break`/`continue` mid-iteration are NOT handled here.
    // Their last-use Free (Task 3) is anchored on the last reading
    // instruction; if the jump skips that read, the Free never fires.
    // The M8.1 pattern set doesn't exercise this case — revisit if it
    // arises.
    schedule_loop_exit_frees_in(tir, &own, sidecar, &last_use, &body_stmts, None);

    // Anonymous-temporary frees: temp_owners that didn't become named
    // bindings and are still Valid at function exit need their own
    // Free anchored after their single consumer.
    let mut consumer_of: HashMap<TirRef, TirRef> = HashMap::new();
    for &stmt in &body_stmts {
        find_consumers(tir, stmt, &mut consumer_of);
    }
    for &temp in &own.temp_owners {
        // Skip temps that became named-binding owners — Task 3's last-use
        // pass owns the Free for those. Note: the `Valid`-state filter
        // below would NOT skip them, because `rebind_to_init` resurrects
        // the temp's state to `Valid` after `consume_for_assignment` had
        // stamped it `Moved`. So this `named_owners` filter is
        // load-bearing, not redundant with the `Valid`-state check.
        if own.named_owners.contains(&temp) {
            continue;
        }
        if !matches!(own.states.get(&temp), Some(OwnerState::Valid)) {
            // Already moved (flowed into a `move` arg, return, etc.).
            continue;
        }
        if let Some(&consumer) = consumer_of.get(&temp) {
            sidecar.free_schedule.push(FreePoint {
                after: consumer,
                target: temp,
                span: tir.span(temp),
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
    // guard activates with Task 6.
    let reassign_targets: HashSet<TirRef> = sidecar.free_on_reassign.values().copied().collect();
    for (owner, (name, span, decl_inst)) in own.pending_dead_store.drain() {
        sink.emit(Diag::warning(
            span,
            DiagCode::DeadStore,
            format!("value `{}` is declared but never used", pool.str(name)),
        ));
        if reassign_targets.contains(&owner) {
            // Task 6's reassignment-Free already covers this owner;
            // emitting another dead-store Free would double-free.
            continue;
        }
        sidecar.free_schedule.push(FreePoint {
            after: decl_inst,
            target: owner,
            span,
            branch: None,
        });
    }
}

/// Lower bound of the synthetic-param `TirRef` keyspace. Real
/// instruction refs grow upward from 1; synthetic refs (assigned by
/// `synthetic_param_ref`) live near `u32::MAX`. Anything at or above
/// this threshold is a synthetic param. Co-located with
/// `synthetic_param_ref` so the encoding lives in one place.
const SYNTHETIC_PARAM_REF_THRESHOLD: u32 = u32::MAX / 2;

fn is_synthetic_param_ref(r: TirRef) -> bool {
    r.raw() >= SYNTHETIC_PARAM_REF_THRESHOLD
}

/// Build a stable synthetic [`TirRef`] for a parameter so it can
/// share the `states` map with real instruction refs. Encoded near
/// `u32::MAX` to keep it well clear of real per-function indices
/// (which start at 1 and grow with body size). `TirRef` wraps
/// `NonZeroU32`, so we clamp the lower bound to 1 to defend against
/// the edge case `name.raw() == u32::MAX` (which would otherwise
/// panic at `from_raw`). The collision-free property relies on real
/// instruction refs never reaching `u32::MAX - max_string_id`, which
/// is structurally impossible for any realistic program.
fn synthetic_param_ref(name: StringId) -> TirRef {
    let raw = u32::MAX.saturating_sub(name.raw()).max(1);
    TirRef::from_raw(raw)
}

fn analyze_stmt(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
    stmt: TirRef,
) {
    let inst = *tir.inst(stmt);
    match inst.tag {
        TirTag::VarDecl => analyze_var_decl(tir, pool, own, sink, by_name, sidecar, stmt),
        TirTag::Assign => analyze_assign(tir, pool, own, sink, by_name, sidecar, stmt),
        TirTag::Return => analyze_return(tir, pool, own, sink, by_name, sidecar, stmt),
        TirTag::ReturnVoid => {}
        TirTag::IfStmt => analyze_if_stmt(tir, pool, own, sink, by_name, sidecar, stmt),
        TirTag::WhileLoop => analyze_while_loop(tir, pool, own, sink, by_name, sidecar, stmt),
        TirTag::ForRange => analyze_for_range(tir, pool, own, sink, by_name, sidecar, stmt),
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
                visit_expr(tir, pool, own, sink, by_name, sidecar, o);
            }
        }
        _ => {
            visit_expr(tir, pool, own, sink, by_name, sidecar, stmt);
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
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
    r: TirRef,
) {
    let view = tir.var_decl_view(r);
    let init = view.initializer;
    let init_ty = tir.inst(init).ty;
    visit_expr(tir, pool, own, sink, by_name, sidecar, init);
    if needs_tracking(init_ty, pool) {
        let span = tir.span(r);
        let consumed_name = consumed_binding_name(tir, init);
        consume_for_assignment(pool, own, sink, init, span, consumed_name);
        rebind_to_init(own, view.name, init);
        // Register the new binding as pending dead-store. The walk
        // clears this entry on any later read or consumption; a
        // surviving entry at function end fires W0001. Keyed by
        // `init`, the same TirRef `rebind_to_init` stamped Valid.
        register_pending_dead_store(own, init, view.name, span, r);
    } else {
        own.current_owner.insert(view.name, init);
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
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
    r: TirRef,
) {
    let view = tir.assign_view(r);
    let value_ty = tir.inst(view.value).ty;
    visit_expr(tir, pool, own, sink, by_name, sidecar, view.value);
    if needs_tracking(value_ty, pool) {
        // Capture the old owner before consume_for_assignment / rebind
        // overwrite current_owner[name]. Only emit the Free entry if the
        // old owner is Valid — Borrowed/Moved/missing means there is no
        // live allocation to release. Codegen (Task 8) consults this map
        // when lowering Assign.
        if let Some(&old_owner) = own.current_owner.get(&view.name)
            && matches!(own.states.get(&old_owner), Some(OwnerState::Valid))
        {
            sidecar.free_on_reassign.insert(r, old_owner);
            // Reassignment runs the old value's destructor (the
            // free_on_reassign Free above) — that's an observable use,
            // so the prior VarDecl/Assign isn't a dead store. Drop the
            // pending_dead_store entry so W0001 doesn't fire and the
            // drain block doesn't try to schedule a redundant Free.
            own.pending_dead_store.remove(&old_owner);
        }
        let span = tir.span(r);
        let consumed_name = consumed_binding_name(tir, view.value);
        consume_for_assignment(pool, own, sink, view.value, span, consumed_name);
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
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    let operand = match inst.data {
        TirData::UnOp(o) => o,
        _ => unreachable!("Return must carry TirData::UnOp"),
    };
    let ty = tir.inst(operand).ty;
    visit_expr(tir, pool, own, sink, by_name, sidecar, operand);
    if !needs_tracking(ty, pool) {
        return;
    }
    let span = tir.span(r);
    let consumed_name = consumed_binding_name(tir, operand);
    consume_underlying(
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
    own.states.insert(init, OwnerState::Valid);
    own.current_owner.insert(name, init);
    own.named_owners.insert(init);
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
        .insert(owner, (name, span, decl_inst));
}

/// Walk back from `init` to whichever SSA value currently owns the
/// underlying allocation. `visit_expr` is responsible for populating
/// `origin` for `Var` reads; for fresh producers (`StrConst`,
/// `StrConcat`, `Call`) `init` is itself the owner.
fn underlying_owner(own: &Ownership, init: TirRef) -> TirRef {
    match own.origin.get(&init).copied() {
        Some(Some(owner)) => owner,
        _ => init,
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
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    init: TirRef,
    span: Span,
    name: Option<StringId>,
) {
    consume_underlying(
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
/// * `Moved` → silent (the `Var` arm of `visit_expr` already emitted
///   E0020 for the aliasing read that brought us here; emitting again
///   would double-report a single fault — see commit 2c5985d),
/// * `NotTracked` → no-op.
fn consume_underlying(
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    operand: TirRef,
    span: Span,
    name: Option<StringId>,
    on_borrowed: BorrowedAction,
) {
    let underlying = underlying_owner(own, operand);
    let state = own
        .states
        .get(&underlying)
        .cloned()
        .unwrap_or(OwnerState::NotTracked);
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
            // No diagnostic here — the `Var` arm in `visit_expr`
            // already emits E0020 for any aliasing read of a moved
            // owner, which covers every path that lands here today
            // (the only operands that reach a consume site with
            // `Moved` underlying state arrive via `Var`). Direct
            // producers (StrConst/StrConcat/Call) are stamped
            // `Valid` in `visit_expr` on a freshly-walked branch and
            // cannot be `Moved` here. Emitting again would
            // double-report a single fault.
            //
            // If a future producer pattern bypasses `Var` and lands
            // here in `Moved` state, prefer no diagnostic over a
            // duplicate; revisit then.
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
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
    r: TirRef,
) {
    let view = tir.if_stmt_view(r);
    visit_expr(tir, pool, own, sink, by_name, sidecar, view.cond);

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

    let snapshot = own.clone();

    for stmt in &view.then_stmts {
        analyze_stmt(tir, pool, own, sink, by_name, sidecar, *stmt);
    }
    let then_state = own.clone();

    let mut branch_results = vec![then_state];
    for elif in &view.elif_branches {
        *own = snapshot.clone();
        visit_expr(tir, pool, own, sink, by_name, sidecar, elif.cond);
        for stmt in &elif.body {
            analyze_stmt(tir, pool, own, sink, by_name, sidecar, *stmt);
        }
        branch_results.push(own.clone());
    }

    if let Some(else_stmts) = &view.else_stmts {
        *own = snapshot.clone();
        for stmt in else_stmts {
            analyze_stmt(tir, pool, own, sink, by_name, sidecar, *stmt);
        }
        branch_results.push(own.clone());
    } else {
        branch_results.push(snapshot.clone());
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

    let mut all_keys: HashSet<TirRef> = HashSet::new();
    for arm in &arms {
        for k in arm.state.states.keys() {
            all_keys.insert(*k);
        }
    }
    for owner in all_keys {
        if is_synthetic_param_ref(owner) {
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
                sidecar.free_schedule.push(FreePoint {
                    after,
                    target: owner,
                    span: tir.span(owner),
                    branch: Some(arm.branch_id),
                });
            }
            // Empty arm or sema-rejected case: skip. The M8.1
            // grammar forbids empty arms.
        }
    }

    // Final restore: `snapshot` has no further uses, so move instead
    // of clone.
    *own = snapshot;
    let refs: Vec<&Ownership> = branch_results.iter().collect();
    own.merge_branches(&refs);
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
/// (iterate until `states_differ` returns false).
fn analyze_while_loop(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
    r: TirRef,
) {
    let view = tir.while_loop_view(r);
    let entry = own.clone();

    // First pass into a scratch sink — diagnostics are kept only if
    // a single iteration suffices. If the lattice changes we re-walk
    // and the scratch diags are dropped (the second walk re-emits
    // anything that's still wrong).
    let mut scratch_sink = DiagSink::new();
    visit_expr(
        tir,
        pool,
        own,
        &mut scratch_sink,
        by_name,
        sidecar,
        view.cond,
    );
    for stmt in &view.body {
        analyze_stmt(tir, pool, own, &mut scratch_sink, by_name, sidecar, *stmt);
    }
    let after_first = own.clone();

    if states_differ(&entry, &after_first) {
        *own = merge_two(&entry, &after_first);
        visit_expr(tir, pool, own, sink, by_name, sidecar, view.cond);
        for stmt in &view.body {
            analyze_stmt(tir, pool, own, sink, by_name, sidecar, *stmt);
        }
        let after_second = own.clone();
        *own = merge_two(&entry, &after_second);
    } else {
        for d in scratch_sink.into_diags() {
            sink.emit(d);
        }
        *own = merge_two(&entry, &after_first);
    }
}

/// `for i in range(start, end)` loop var is `int` (Copy), so the
/// induction variable never enters the lattice. The body runs the
/// same fixed-point as `while`.
fn analyze_for_range(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
    r: TirRef,
) {
    let view = tir.for_range_view(r);
    // Start/end are visited unconditionally — they're plain `int`
    // exprs, so they don't move anything, but they may contain nested
    // reads we want to record.
    visit_expr(tir, pool, own, sink, by_name, sidecar, view.start);
    visit_expr(tir, pool, own, sink, by_name, sidecar, view.end);

    let entry = own.clone();

    let mut scratch_sink = DiagSink::new();
    for stmt in &view.body {
        analyze_stmt(tir, pool, own, &mut scratch_sink, by_name, sidecar, *stmt);
    }
    let after_first = own.clone();

    if states_differ(&entry, &after_first) {
        *own = merge_two(&entry, &after_first);
        for stmt in &view.body {
            analyze_stmt(tir, pool, own, sink, by_name, sidecar, *stmt);
        }
        let after_second = own.clone();
        *own = merge_two(&entry, &after_second);
    } else {
        for d in scratch_sink.into_diags() {
            sink.emit(d);
        }
        *own = merge_two(&entry, &after_first);
    }
}

/// Compare two lattices on the Moved-ness of every tracked `TirRef`.
/// Per the M8.1b plan, the only transition that forces a re-walk is a
/// `Valid`/`Borrowed` → `Moved` flip across the back-edge — non-Moved
/// state changes (e.g. fresh definitions added inside the body) are
/// loop-invariant for the use-after-move check.
fn states_differ(a: &Ownership, b: &Ownership) -> bool {
    // Diverge iff some TirRef is Moved in one and not the other.
    // Walk each side once and check the other's matching entry —
    // missing entries default to NotTracked, so a Moved with no
    // counterpart counts as a divergence on its own.
    for (k, av) in &a.states {
        let a_moved = matches!(av, OwnerState::Moved { .. });
        let b_moved = matches!(b.states.get(k), Some(OwnerState::Moved { .. }));
        if a_moved != b_moved {
            return true;
        }
    }
    for (k, bv) in &b.states {
        if !matches!(bv, OwnerState::Moved { .. }) {
            continue;
        }
        // Only need to catch keys exclusive to b that are Moved —
        // keys present in a were handled in the first loop.
        if !a.states.contains_key(k) {
            return true;
        }
    }
    false
}

/// Merge two lattices by reusing the existing branch-merge join: if
/// either input has a TirRef `Moved`, the result is `Moved`. Used to
/// seed the second pass and to compute the post-loop state.
fn merge_two(a: &Ownership, b: &Ownership) -> Ownership {
    let mut merged = a.clone();
    merged.merge_branches(&[a, b]);
    merged
}

/// Backward walk recording each tracked owner's last reading
/// instruction. Called from `analyze_function` after the forward walk
/// completes; iterates statements in reverse program order so the
/// first observed `Var` read of an owner is its last use in source
/// order. Recurses into operands of every TIR shape so reads buried
/// inside calls, loops, and if-arms are still seen.
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
        // First encounter wins (we walk in reverse program order).
        last_use.entry(owner).or_insert(r);
    }
    // Recurse into operands. Mirrors the forward walk's structural
    // recursion so every reachable read is visited exactly once.
    match inst.data {
        TirData::UnOp(o) => collect_last_uses(tir, pool, own, o, last_use),
        TirData::BinOp { lhs, rhs } => {
            // Operand order doesn't affect program order for last-use
            // (sema lowered both before this instruction); recurse rhs
            // first to mirror reverse program order conservatively.
            collect_last_uses(tir, pool, own, rhs, last_use);
            collect_last_uses(tir, pool, own, lhs, last_use);
        }
        TirData::Extra(_) => match inst.tag {
            TirTag::Call => {
                let view = tir.call_view(r);
                for &arg in view.args.iter().rev() {
                    collect_last_uses(tir, pool, own, arg, last_use);
                }
            }
            TirTag::VarDecl => {
                let v = tir.var_decl_view(r);
                collect_last_uses(tir, pool, own, v.initializer, last_use);
            }
            TirTag::Assign => {
                let v = tir.assign_view(r);
                collect_last_uses(tir, pool, own, v.value, last_use);
            }
            TirTag::CompoundAssign => {
                let v = tir.compound_assign_view(r);
                collect_last_uses(tir, pool, own, v.value, last_use);
            }
            TirTag::IfStmt => {
                let v = tir.if_stmt_view(r);
                if let Some(else_stmts) = &v.else_stmts {
                    for &s in else_stmts.iter().rev() {
                        collect_last_uses(tir, pool, own, s, last_use);
                    }
                }
                for elif in v.elif_branches.iter().rev() {
                    for &s in elif.body.iter().rev() {
                        collect_last_uses(tir, pool, own, s, last_use);
                    }
                    collect_last_uses(tir, pool, own, elif.cond, last_use);
                }
                for &s in v.then_stmts.iter().rev() {
                    collect_last_uses(tir, pool, own, s, last_use);
                }
                collect_last_uses(tir, pool, own, v.cond, last_use);
            }
            TirTag::WhileLoop => {
                let v = tir.while_loop_view(r);
                for &s in v.body.iter().rev() {
                    collect_last_uses(tir, pool, own, s, last_use);
                }
                collect_last_uses(tir, pool, own, v.cond, last_use);
            }
            TirTag::ForRange => {
                let v = tir.for_range_view(r);
                for &s in v.body.iter().rev() {
                    collect_last_uses(tir, pool, own, s, last_use);
                }
                collect_last_uses(tir, pool, own, v.end, last_use);
                collect_last_uses(tir, pool, own, v.start, last_use);
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

/// Build a `child_TirRef → parent_TirRef` map. Each temporary owner
/// has at most one direct parent in the TIR (TIR is tree-shaped per
/// function), so `or_insert` correctly preserves the first parent
/// observed. Used by the anonymous-temporary-free pass to anchor
/// each temp's Free after its single consumer.
fn find_consumers(tir: &Tir, r: TirRef, consumer_of: &mut HashMap<TirRef, TirRef>) {
    let inst = *tir.inst(r);
    match inst.data {
        TirData::UnOp(o) => {
            consumer_of.entry(o).or_insert(r);
            find_consumers(tir, o, consumer_of);
        }
        TirData::BinOp { lhs, rhs } => {
            consumer_of.entry(lhs).or_insert(r);
            consumer_of.entry(rhs).or_insert(r);
            find_consumers(tir, lhs, consumer_of);
            find_consumers(tir, rhs, consumer_of);
        }
        TirData::Extra(_) => match inst.tag {
            TirTag::Call => {
                let view = tir.call_view(r);
                for &arg in &view.args {
                    consumer_of.entry(arg).or_insert(r);
                    find_consumers(tir, arg, consumer_of);
                }
            }
            TirTag::VarDecl => {
                let v = tir.var_decl_view(r);
                consumer_of.entry(v.initializer).or_insert(r);
                find_consumers(tir, v.initializer, consumer_of);
            }
            TirTag::Assign => {
                let v = tir.assign_view(r);
                consumer_of.entry(v.value).or_insert(r);
                find_consumers(tir, v.value, consumer_of);
            }
            TirTag::CompoundAssign => {
                let v = tir.compound_assign_view(r);
                consumer_of.entry(v.value).or_insert(r);
                find_consumers(tir, v.value, consumer_of);
            }
            TirTag::IfStmt => {
                let v = tir.if_stmt_view(r);
                consumer_of.entry(v.cond).or_insert(r);
                find_consumers(tir, v.cond, consumer_of);
                for &s in &v.then_stmts {
                    find_consumers(tir, s, consumer_of);
                }
                for elif in &v.elif_branches {
                    consumer_of.entry(elif.cond).or_insert(r);
                    find_consumers(tir, elif.cond, consumer_of);
                    for &s in &elif.body {
                        find_consumers(tir, s, consumer_of);
                    }
                }
                if let Some(else_stmts) = &v.else_stmts {
                    for &s in else_stmts {
                        find_consumers(tir, s, consumer_of);
                    }
                }
            }
            TirTag::WhileLoop => {
                let v = tir.while_loop_view(r);
                consumer_of.entry(v.cond).or_insert(r);
                find_consumers(tir, v.cond, consumer_of);
                for &s in &v.body {
                    find_consumers(tir, s, consumer_of);
                }
            }
            TirTag::ForRange => {
                let v = tir.for_range_view(r);
                consumer_of.entry(v.start).or_insert(r);
                consumer_of.entry(v.end).or_insert(r);
                find_consumers(tir, v.start, consumer_of);
                find_consumers(tir, v.end, consumer_of);
                for &s in &v.body {
                    find_consumers(tir, s, consumer_of);
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

/// Recursively walk `stmts` and, for every `Break`/`Continue` found
/// inside a loop body, schedule unconditional Frees for pre-loop
/// owners still `Valid` at the jump site. `enclosing_loop` is the
/// nearest enclosing `WhileLoop`/`ForRange` instruction reference,
/// or `None` at top-level (where `Break`/`Continue` would already
/// have been rejected by sema).
///
/// "Pre-loop owner" is approximated by comparing the owner's
/// `TirRef` index against the minimum index in the loop's body —
/// see `schedule_break_continue_frees` for the rationale.
fn schedule_loop_exit_frees_in(
    tir: &Tir,
    own: &Ownership,
    sidecar: &mut OwnershipSidecar,
    last_use: &HashMap<TirRef, TirRef>,
    stmts: &[TirRef],
    enclosing_loop: Option<TirRef>,
) {
    for &r in stmts {
        let inst = *tir.inst(r);
        match inst.tag {
            TirTag::Break | TirTag::Continue => {
                if let Some(loop_ref) = enclosing_loop {
                    schedule_break_continue_frees(tir, own, sidecar, last_use, r, loop_ref);
                }
                // Else: outside any loop — sema rejects this with a
                // dedicated diagnostic, so well-formed TIR never
                // reaches here.
            }
            TirTag::WhileLoop => {
                let view = tir.while_loop_view(r);
                schedule_loop_exit_frees_in(tir, own, sidecar, last_use, &view.body, Some(r));
            }
            TirTag::ForRange => {
                let view = tir.for_range_view(r);
                schedule_loop_exit_frees_in(tir, own, sidecar, last_use, &view.body, Some(r));
            }
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                schedule_loop_exit_frees_in(
                    tir,
                    own,
                    sidecar,
                    last_use,
                    &view.then_stmts,
                    enclosing_loop,
                );
                for elif in &view.elif_branches {
                    schedule_loop_exit_frees_in(
                        tir,
                        own,
                        sidecar,
                        last_use,
                        &elif.body,
                        enclosing_loop,
                    );
                }
                if let Some(else_stmts) = &view.else_stmts {
                    schedule_loop_exit_frees_in(
                        tir,
                        own,
                        sidecar,
                        last_use,
                        else_stmts,
                        enclosing_loop,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Schedule one Free per pre-loop owner that's still `Valid` at the
/// jump site and isn't already covered by a post-loop last-use.
///
/// "Pre-loop" is identified by comparing the owner's `TirRef` index
/// against the minimum body-statement index of the enclosing loop —
/// `TirBuilder` emits body statements before the loop instruction
/// itself, so any owner whose index is below the body's minimum was
/// definitely defined before the loop started. Owners with index
/// `>= body_min` were defined inside the body and fall under the
/// inside-loop limitation documented in `analyze_function`.
fn schedule_break_continue_frees(
    tir: &Tir,
    own: &Ownership,
    sidecar: &mut OwnershipSidecar,
    last_use: &HashMap<TirRef, TirRef>,
    jump_inst: TirRef,
    loop_inst: TirRef,
) {
    let view_body_min = match tir.inst(loop_inst).tag {
        TirTag::WhileLoop => tir
            .while_loop_view(loop_inst)
            .body
            .iter()
            .map(|s| s.raw())
            .min(),
        TirTag::ForRange => tir
            .for_range_view(loop_inst)
            .body
            .iter()
            .map(|s| s.raw())
            .min(),
        _ => unreachable!("schedule_break_continue_frees called with non-loop TirRef"),
    };
    // Empty loop body cannot contain a `break`/`continue`, so this
    // function would never be called with an empty body; treat the
    // unreachable case as "everything counts as inside-loop" for
    // safety.
    let body_min = match view_body_min {
        Some(m) => m,
        None => return,
    };

    for (owner, state) in &own.states {
        if is_synthetic_param_ref(*owner) {
            continue;
        }
        if !matches!(state, OwnerState::Valid) {
            // Already moved or borrowed: nothing to free here.
            continue;
        }
        if owner.raw() >= body_min {
            // Inside-loop owner — declared in (or after) the loop's
            // body. Last-use Free (Task 3) covers it; mid-iteration
            // break/continue with an inside-loop owner is the M8.1
            // limitation noted in `analyze_function`.
            //
            // Note: condition-expression refs (e.g. for `while cond:`)
            // also have `raw() < body_min` and would fall through this
            // gate. M8.1 conditions are bool-typed and don't produce
            // heap owners, so this is currently a non-issue — but if
            // a future feature lets conditions allocate, revisit.
            continue;
        }
        if let Some(&lu) = last_use.get(owner)
            && lu.raw() > loop_inst.raw()
        {
            // Last-use is *strictly after* the loop instruction in
            // TirRef order — i.e., a post-loop read. The post-loop
            // last-use Free covers this owner; emitting one here
            // would double-free.
            //
            // `>` rather than `>=`: `lu.raw() == loop_inst.raw()` is
            // unreachable (the loop instruction itself is never a
            // last-use site for an external owner). If a future
            // refactor attaches last-use directly to the loop inst,
            // revisit this check.
            continue;
        }
        sidecar.free_schedule.push(FreePoint {
            after: jump_inst,
            target: *owner,
            span: tir.span(jump_inst),
            branch: None,
        });
    }
}

fn visit_expr(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
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
                own.states.insert(r, OwnerState::Valid);
                own.origin.insert(r, None);
                own.temp_owners.insert(r);
            }
        }
        TirTag::StrConcat => {
            if needs_tracking(inst.ty, pool) {
                own.states.insert(r, OwnerState::Valid);
                own.origin.insert(r, None);
                own.temp_owners.insert(r);
            }
            if let TirData::BinOp { lhs, rhs } = inst.data {
                visit_expr(tir, pool, own, sink, by_name, sidecar, lhs);
                visit_expr(tir, pool, own, sink, by_name, sidecar, rhs);
            }
        }
        TirTag::Call => {
            // A str-returning call (e.g. `int_to_str`) is a producer.
            if needs_tracking(inst.ty, pool) {
                own.states.insert(r, OwnerState::Valid);
                own.origin.insert(r, None);
                own.temp_owners.insert(r);
            }
            let view = tir.call_view(r);
            // Look up callee parameter conventions. Builtins
            // (`print`, `assert`, `int_to_str`) have no entry and
            // default to borrowing all args.
            let callee_params = by_name.get(&view.name).map(|t| t.params.as_slice());
            for (i, arg) in view.args.iter().enumerate() {
                visit_expr(tir, pool, own, sink, by_name, sidecar, *arg);
                let arg_ty = tir.inst(*arg).ty;
                if !needs_tracking(arg_ty, pool) {
                    continue;
                }
                let is_move_param = callee_params
                    .and_then(|ps| ps.get(i))
                    .map(|p| p.is_move)
                    .unwrap_or(false);
                if is_move_param {
                    let consumed_name = consumed_binding_name(tir, *arg);
                    consume_for_assignment(pool, own, sink, *arg, tir.span(r), consumed_name);
                }
                // Borrow path: no extra check here. With the current
                // set of producers (StrConst/StrConcat/Call/Var), every
                // tracked str argument flows through `Var`, and the
                // `Var` arm already fires E0020 for use-after-move.
                // Adding a call-site check would double-report. If a
                // future producer pattern bypasses `Var` (e.g. passing
                // a moved Call result directly), revisit this.
            }
        }
        // ---- Aliasing read ----
        // `Var` is a non-consuming read. Record which SSA value it
        // currently aliases so a later use-after-move diagnostic can
        // walk back to the root owner. Per the design spec, an
        // aliasing read of an already-`Moved` owner is itself an
        // E0020 use-after-move. Reads of `Borrowed` owners are fine
        // (Rule 2 — borrowed parameters can be freely read).
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
                // didn't ignore it.
                own.pending_dead_store.remove(&owner);
                own.origin.insert(r, Some(owner));
                // Snapshot owner-at-read so the post-walk
                // `collect_last_uses` anchors the last-use Free to the
                // owner that was live *at this read*, not whatever
                // `current_owner[name]` happens to be at function exit
                // (which would route pre-rebind reads to the post-
                // rebind owner — wrong target, double-free).
                own.owner_at_read.insert(r, owner);
                if let Some(OwnerState::Moved { moved_at, .. }) = own.states.get(&owner).cloned() {
                    sink.emit(
                        Diag::error(
                            tir.span(r),
                            DiagCode::UseAfterMove,
                            format!("use of moved value `{}`", pool.str(name)),
                        )
                        .with_note(Some(moved_at), "value moved here")
                        .with_help(
                            "consider using the value before the move, or pass by default (borrow) instead of `move`",
                        ),
                    );
                }
            }
        }
        // ---- Everything else: recurse on operands so nested
        // ---- producers/aliases are still observed.
        _ => {
            recurse_operands(tir, pool, own, sink, by_name, sidecar, r);
        }
    }
}

fn recurse_operands(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    by_name: &HashMap<StringId, &Tir>,
    sidecar: &mut OwnershipSidecar,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    match inst.data {
        TirData::UnOp(o) => visit_expr(tir, pool, own, sink, by_name, sidecar, o),
        TirData::BinOp { lhs, rhs } => {
            visit_expr(tir, pool, own, sink, by_name, sidecar, lhs);
            visit_expr(tir, pool, own, sink, by_name, sidecar, rhs);
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
        use crate::tir::TirBuilder;
        use chumsky::span::{SimpleSpan, Span as _};

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
        let sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);

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
    fn last_use_scheduled_for_unmoved_local() {
        use crate::tir::TirBuilder;
        use chumsky::span::{SimpleSpan, Span as _};

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
        let call = b.call(print_name, &[var_read], void, span);
        let stmt = b.unary(TirTag::ExprStmt, void, call, span);
        let tir = b.finish(&[decl, stmt]);

        let mut sink = DiagSink::new();
        let sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
        assert!(sink.is_empty(), "expected no diagnostics");
        assert_eq!(sidecar.free_schedule.len(), 1);
        assert_eq!(sidecar.free_schedule[0].target, lit);
        assert_eq!(sidecar.free_schedule[0].after, var_read);
        assert!(sidecar.free_schedule[0].branch.is_none());
    }

    #[test]
    fn reassignment_records_free_on_old_owner() {
        use crate::tir::TirBuilder;
        use chumsky::span::{SimpleSpan, Span as _};

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
        let call = tb.call(print, &[var_read], void, span);
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[decl, assign, stmt]);

        let mut sink = DiagSink::new();
        let sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
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
        use crate::tir::TirBuilder;
        use chumsky::span::{SimpleSpan, Span as _};
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
        let call = tb.call(print, &[cat], void, span);
        let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
        let tir = tb.finish(&[stmt]);

        let mut sink = DiagSink::new();
        let sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
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
        use crate::tir::TirBuilder;
        use chumsky::span::{SimpleSpan, Span as _};

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
        let call1 = tb.call(print, &[read1], void, span);
        let stmt1 = tb.unary(TirTag::ExprStmt, void, call1, span);
        let bob_lit = tb.str_const(bob, str_ty, span);
        let assign = tb.assign(n, str_ty, bob_lit, span);
        let read2 = tb.var(n, str_ty, span);
        let call2 = tb.call(print, &[read2], void, span);
        let stmt2 = tb.unary(TirTag::ExprStmt, void, call2, span);
        let tir = tb.finish(&[decl, stmt1, assign, stmt2]);

        let mut sink = DiagSink::new();
        let sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
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
        use crate::tir::TirBuilder;
        use chumsky::span::{SimpleSpan, Span as _};

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
}
