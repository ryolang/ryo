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
//! See `docs/dev/pl_references/mojo.md`.

use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::flow::{Analysis, BlockId, DropOp, DropPosition, Flow, PlaceId};

mod diag_fmt;
pub(crate) use diag_fmt::*;
mod frees;
pub(crate) use frees::*;
mod loops;
pub(crate) use loops::*;
mod match_;
pub(crate) use match_::*;
mod merge;
pub(crate) use merge::*;
mod structs;
pub(crate) use structs::*;
#[cfg(debug_assertions)]
pub(crate) mod gaps;
#[cfg(debug_assertions)]
mod validate;
mod views;
pub(crate) use views::*;
mod walk;
pub(crate) use walk::*;

pub use ryo_core::ownership::{FunctionSidecar, OwnershipSidecar, PromoFree};
use ryo_core::tir::{ParamMode, Span, Tir, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeId};
use std::collections::{HashMap, HashSet};

// ---------- Classification ----------

/// True for types whose values transfer ownership on `=` and must be
/// tracked through the function body: `str` and `bytes` (M8.4.2), plus
/// non-Copy structs (M9) — a struct with at least one needs-drop field
/// moves as a whole, fields and all. Future heap types (`List[T]`,
/// `Dict[K, V]`) will join this set via `needs_drop` too.
pub(crate) fn is_move_type(ty: TypeId, pool: &InternPool) -> bool {
    pool.needs_drop(ty)
}

/// Predicate the ownership walk uses to decide whether a `TirRef`
/// needs a lattice slot. Currently identical to `is_move_type`, but
/// kept as its own name so the walk reads correctly when borrows
/// land and the answer becomes "move OR borrowed-of-move".
pub(crate) fn needs_tracking(ty: TypeId, pool: &InternPool) -> bool {
    is_move_type(ty, pool)
}

// ---------- Lattice ----------

/// How an owner hazard arose — the validator's transfer-event production
/// discriminates moves (ownership leaves this function: a return, a
/// move argument, a match consume) from non-moves (an `inout` pass —
/// the value comes back; a reassign mutation — discharged by the
/// displacement release, not a transfer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HazardKind {
    /// Ownership moved out (move arg, match/return consume, an
    /// aggregate field seat, an arm-expression by-value read).
    Move,
    /// An `inout` pass: the callee may mutate, the value returns.
    InoutPass,
    /// A reassign mutation: the old value is displaced at the store.
    Mutate,
}

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

/// Ownership identity (G2): the Flow [`PlaceId`] that owns the
/// storage — a `Param`/`Local`/`Token`/`Temp`/`MatchResult` place.
/// Names are diagnostic labels only; the analysis path never keys
/// state by name (a `Var` read resolves through the step's
/// `StepFacts.read` place, a write through `StepFacts.write.place`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Owner(pub(crate) PlaceId);

impl Owner {
    /// The `TirRef` codegen resolves this owner by — the place's
    /// identity instruction: a param's virtual ref, a `Local`'s
    /// declaring `VarDecl`, a `Token`'s inst, a `Temp`'s producer, a
    /// match result's match inst. Every Free target / codegen lookup
    /// goes through here (DropOp.target, free_on_reassign values).
    pub(crate) fn tirref(self, flow: &Flow) -> TirRef {
        match flow.place(self.0) {
            ryo_core::flow::Place::Param { index, .. } => TirRef::param(*index as usize),
            ryo_core::flow::Place::Local { decl, .. }
            | ryo_core::flow::Place::Token { inst: decl, .. }
            | ryo_core::flow::Place::Temp { producer: decl, .. }
            | ryo_core::flow::Place::MatchResult {
                match_inst: decl, ..
            }
            | ryo_core::flow::Place::LoopVar {
                loop_inst: decl, ..
            }
            | ryo_core::flow::Place::View { producer: decl, .. } => *decl,
        }
    }

    /// The owner as an instruction `TirRef`, or `None` for a param
    /// (param identity is its virtual ref, not an instruction). View
    /// owners resolve through their producing slice/conversion inst —
    /// the dense view tables are inst-indexed.
    pub(crate) fn inst_tirref(self, flow: &Flow) -> Option<TirRef> {
        match flow.place(self.0) {
            ryo_core::flow::Place::Param { .. } => None,
            _ => Some(self.tirref(flow)),
        }
    }

    /// The diagnostic span of the owning place's identity instruction
    /// (payload lookup — the place names the inst whose span labels
    /// the owner).
    pub(crate) fn span(self, flow: &Flow, tir: &Tir) -> Span {
        match flow.place(self.0) {
            ryo_core::flow::Place::Param { index, .. } => tir.params[*index as usize].span,
            _ => tir.span(self.tirref(flow)),
        }
    }
}

/// The step instruction a scheduled drop is anchored on. The ownership
/// pass only ever emits step-anchored (`AfterStep`) positions today —
/// conditionality is expressed by the anchor's block membership, so no
/// other `DropPosition` variant reaches the anchor-order scans. The
/// `expect` pins that invariant at every read site.
pub(crate) fn drop_anchor(fp: &DropOp) -> TirRef {
    fp.position.anchor_inst().expect("step-anchored drop")
}

/// The step/terminator-anchored subset of the schedule. The anchor-order
/// scans reason about instruction order; `BlockStart` drops (the unified cleanup schedule — the
/// unified schedule) fire at block starts and are covered by
/// block-membership passes instead, so they are not anchor-scannable.
pub(crate) fn anchored_drops(sidecar: &FunctionSidecar) -> impl Iterator<Item = &DropOp> {
    sidecar
        .drops
        .iter()
        .filter(|fp| fp.position.anchor_inst().is_some())
}

/// Per-function ownership state. `states` is the lattice itself,
/// keyed by the owning Flow place. `current_owner` maps a binding
/// home place (the `Local`/`Param` place a read or write fact names)
/// to whichever owner currently holds its underlying allocation — a
/// `Var` read step resolves its owner as
/// `current_owner[facts.read].unwrap_or(facts.read)`, so
/// reassignment reseats ownership without disturbing the producing
/// value's state.
#[derive(Default, Clone)]
pub(crate) struct Ownership {
    pub states: HashMap<Owner, OwnerState>,
    /// Binding home place → the owner currently seated in it.
    pub current_owner: HashMap<PlaceId, Owner>,
    /// Dense per-instruction owner-place resolution (G2): the place
    /// whose owner an instruction's value IS — the write place for
    /// declarations/assignments, the read place for `Var` reads, the
    /// view place for slices, the temp/token/match-result place for
    /// producers. Built once per body in `analyze_function` from Flow
    /// facts (`temp_of`/`local_of_decl`/`match_result`/`token_places`
    /// plus per-step `StepFacts`); every query of "who owns the value
    /// of inst r" goes through this table instead of re-deriving
    /// storage relationships from TIR shape. `None` for untracked
    /// instructions (slot 0, Copy values, control payloads).
    pub inst_place: Vec<Option<PlaceId>>,
    /// Param index → the param's place (params are the outermost
    /// scope; their places are built first). The `Owner` of a param
    /// value is `Owner(param_places[idx])`.
    pub param_places: Vec<PlaceId>,
    /// Name → the lexical BINDING the name currently resolves to:
    /// the `TirRef` of the declaring `VarDecl` (unique per binding —
    /// one name may denote several bindings via an explicit `mut`
    /// shadow or sibling-scope declarations), or a param's virtual
    /// ref for a param binding. The walk stamps every `Assign` with
    /// its resolved binding into the sidecar's `assign_binding`
    /// table; the free-scheduling passes group reassignments by that
    /// identity so a shadow binding's reassigns cannot influence the
    /// outer binding's cleanup. Non-monotone like `current_owner`:
    /// snapshotted and restored with `BranchState` at branches and
    /// loops, first-wins at merges (a shadow does not survive the
    /// join unless every side took it).
    pub binding_of_name: HashMap<StringId, TirRef>,
    /// Lazily-built document-rank table (the Flow builder's emission
    /// stamp — see `document_rank`), cached so
    /// `record_return_epilogue` does not rebuild the per-function Vec
    /// on every Return (loop convergence re-walks returns several
    /// times). Slot 0 stays 0, so `is_empty` is a safe "not built yet"
    /// sentinel.
    pub order: Vec<u32>,
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
    /// (the `Var` step handler) and consulted by the post-walk
    /// last-use passes instead of resolving through `current_owner`'s
    /// end-of-function state — which would misroute reads that precede
    /// a `mut` reassignment to the post-rebind owner. For Move-typed
    /// reads this anchors the last-use Free to the correct allocation.
    /// Dense per-instruction table, sized to the arena.
    pub owner_at_read: Vec<Option<Owner>>,

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
    /// matching entries into block-keyed `ConditionalDeadDrop`s so the
    /// pre-branch buffer is also freed on the untouched paths.
    pub reseat_drops: Vec<ReseatDrop>,

    /// Owners still `Valid` at each `Return`/`ReturnVoid`, snapshotted
    /// mid-walk while the lattice state is path-correct for that exit
    /// point. The function must destroy those values on that path —
    /// they are dead at the return, but the last-use / temp / drain
    /// passes (which run at function exit) anchor their Frees on OTHER
    /// paths or program points the early return never reaches.
    /// Monotone-accumulating; loop convergence re-walks may record the
    /// same return twice — deduped at scheduling time. Only owners
    /// whose producer is ranked at or before the return in program
    /// order are recorded (`record_return_epilogue`): loop fixed-point
    /// seeding can leave a body-local owner `Valid` at a return it
    /// lexically precedes, and such a value cannot exist on the
    /// return's path.
    pub return_epilogue: Vec<(TirRef, Vec<Owner>)>,

    /// P3 (final spec §3.2): each bound view → the root owner it
    /// projects (re-slices resolve transitively to the original
    /// owner). Monotone (insert-only): a view's root never changes,
    /// and branch-scope-dropped views keep their entry so the P5
    /// deferral on the root survives the merge. Merges first-wins. Sparse keys, like `states`.
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

    /// Field path (indices from the struct root, see `field_path_of`)
    /// of the str/bytes field a view slices, keyed by view owner.
    /// Present only for field-base projections; whole-struct
    /// operations (drop/move/reassign) check `live_projections` by
    /// root alone, while a `FieldAssign` target check matches the
    /// assigned field's path against these. Monotone like
    /// `root_owner`: a view owner's path never changes, so branch
    /// arms accumulate in place and merges need no rule for it.
    /// Entries for dead views are left behind harmlessly — lookups
    /// only happen for views present in `live_projections`.
    pub projection_fields: HashMap<Owner, Vec<u32>>,

    /// Walk-constant pre-pass liveness (P4): bound view instruction →
    /// its last reading instruction. Views with no entry are never
    /// read — their projection lives to scope end. Constant per
    /// function (computed before the walk), so branch merges need no
    /// per-field rule for it. The branch analysis temporarily refines
    /// entries per arm (see `arm_last_reads`) and restores them at
    /// each arm's end. Dense per-instruction table, sized like
    /// `origin`.
    pub view_last_use: Vec<Option<TirRef>>,

    /// Walk-constant pre-pass liveness (P4 per-arm refinement): the
    /// arm's Flow IR entry block → per-arm (view instruction → its
    /// last read within that arm's blocks), recorded by the liveness
    /// pre-pass for every if/match arm. Consulted via
    /// `refine_view_liveness_for_arm`; [`Analysis`] tri-state: a
    /// missing/`Unavailable` entry never refines (conservative
    /// over-liveness), a `Present` map overrides/kills per view.
    /// Constant per function, so branch merges need no per-field rule.
    pub arm_last_reads: HashMap<BlockId, Analysis<HashMap<TirRef, TirRef>>>,

    /// Walk-constant pre-pass liveness (P4): view instruction → the
    /// loop at whose exit the projection dies. A view whose last read
    /// sits inside a loop its creation is outside of re-executes on
    /// later iterations, so it stays live through the whole loop.
    /// Dense per-instruction table, sized to the arena.
    pub view_defer_loop: Vec<Option<TirRef>>,

    /// Walk-constant pre-pass structure: per-instruction loop nesting
    /// as parent-pointer chains (`inst` → innermost enclosing
    /// `WhileLoop`/`ForRange`, plus the enclosing-loop count).
    /// Computed in one traversal before the walk so the liveness
    /// passes and the redundant-materialize pass look nesting up
    /// instead of re-walking the body per query. Constant per
    /// function, so branch merges need no per-field rule for it.
    /// Dense per-instruction tables, sized like `origin`; `None` /
    /// depth 0 means "not nested".
    pub loop_nesting: LoopNesting,

    /// Views whose projection ends at the current statement's end
    /// (P4). Drained at statement-root steps, so a read and a consume
    /// within the same statement both see the view as live
    /// (borrow-for-the-whole-statement semantics, matching Rule 7).
    pub pending_dying: Vec<Owner>,

    /// W0003 case-B support (M8.4.1.2): every move, mutation
    /// (reassign), or `inout` pass of a tracked owner the walk
    /// observed, as `(owner, site, kind)` triples. Monotone-accumulating
    /// like `reseat_drops` — loop convergence re-walks may push duplicates
    /// (queries are `any()`-shaped, so no dedup is needed). Read by
    /// the post-walk redundant-materialize pass to classify escapes of
    /// the copy and defensive-copy hazards on the view's root owner, by
    /// the return-epilogue pass (consumed bindings), and by the cleanup
    /// validator, which turns `Move` hazards at Return sites into
    /// return-TRANSFER events and `Move` hazards at call/match sites
    /// into call-transfer events (the validator's per-path coverage
    /// discharges obligations only through transfers and releases —
    /// `InoutPass`/`Mutate` hazards are neither: the value returns from
    /// the pass, and a reassign's discharge is its displacement release).
    pub owner_hazards: Vec<(Owner, TirRef, HazardKind)>,

    /// View-creating insts over borrowed-param bases, recorded by the
    /// walk (`record_promo_candidate`, anchored to its enclosing
    /// statement via the walk's `stmt_of` table). Consumed by the
    /// post-walk promotion-free scheduling pass in `analyze_function`.
    pub(crate) promo_candidates: Vec<PromoCandidate>,

    /// Match-arm bindings (M12) never read nor consumed inside their
    /// arm, swept by the match analysis at arm end: the name/span pairs
    /// for the W0001 drain in `analyze_function` (the pass cannot emit
    /// warnings — only `analyze_function` holds the warnings sink).
    /// The matching Frees are already scheduled arm-gated at the arm's
    /// end. Monotone-accumulating: loop convergence re-walks can record
    /// a binding twice — the drain dedups.
    pub arm_dead_stores: Vec<(StringId, Span)>,
}

impl Ownership {
    /// Read a dense per-instruction slot. Refs outside the arena read
    /// as no entry; param sentinels never key these tables (the
    /// `debug_assert!` pins that invariant).
    pub(crate) fn dense_get<V: Copy>(table: &[Option<V>], r: TirRef) -> Option<V> {
        debug_assert!(!r.is_param());
        table.get(r.index()).copied().flatten()
    }

    /// Write a dense per-instruction slot (overwriting, matching the
    /// old `HashMap::insert` semantics).
    pub(crate) fn dense_set<V: Copy>(table: &mut [Option<V>], r: TirRef, v: V) {
        debug_assert!(!r.is_param());
        table[r.index()] = Some(v);
    }

    /// The owner whose storage an instruction's value IS (G2): the
    /// place recorded in `inst_place`, resolved through the binding
    /// home map when the place is a binding someone may have reseated
    /// (a `Var` read of `x` resolves to the owner currently seated in
    /// `x`'s home; a fresh temp/token/result resolves to itself). An
    /// inst with no place (error-shaped TIR, unregistered reads) maps
    /// to the shared `UNTRACKED` owner — `NotTracked` in the lattice,
    /// exactly the old `Owner::Inst` fallback's behavior.
    pub(crate) fn underlying_owner(&self, r: TirRef) -> Owner {
        match self.inst_place[r.index()] {
            Some(p) => self.current_owner.get(&p).copied().unwrap_or(Owner(p)),
            None => Owner(UNTRACKED_PLACE),
        }
    }

    /// The owner identity for a value-producing instruction that is
    /// its own storage (fresh temps, tokens, match results): the
    /// inst's own place, panicking if the inst is not an owner.
    pub(crate) fn owner_of_inst(&self, r: TirRef) -> Owner {
        Owner(self.inst_place[r.index()].expect("inst has an owner place"))
    }
}

/// The shared owner for instructions with no resolvable place
/// (error-shaped TIR) — never seated, so `states` reads `NotTracked`
/// and every transition no-ops, mirroring the old `Owner::Inst`
/// fallback for unregistered values.
pub(crate) const UNTRACKED_PLACE: PlaceId = PlaceId(u32::MAX);

/// Param-name → index lookup for PAYLOAD queries (param modes, param
/// spans/types) — not an analysis-identity lookup (owner identity is
/// the param's place, via `param_places`).
pub(crate) fn param_idx(param_index: &HashMap<StringId, usize>, name: StringId) -> usize {
    match param_index.get(&name) {
        Some(idx) => *idx,
        None => unreachable!("param {:?} missing from param_index", name),
    }
}

/// The owner place an instruction's value IS (G2), derived from Flow
/// facts alone: the write place for declarations/assignments, the read
/// place for `Var`/`FieldAccess` reads (the chain's root — Flow
/// resolves it at build), the view place for slices/conversions, the
/// temp place for producers. `None` for untracked instructions.
pub(crate) fn flow_owner_place(flow: &Flow, r: TirRef) -> Option<PlaceId> {
    if let Some(p) = flow.temp_of(r) {
        return Some(p);
    }
    let step = flow.step_of(r)?;
    let f = step.facts;
    // View before read: a `ToView` step's read names its BASE, but the
    // step's value IS the new view.
    f.write.map(|w| w.place).or(f.view).or(f.read)
}

/// One conditional-reseat observation, recorded by
/// `analyze_if_stmt` after walking an if's arms. `reseat_owners` is the
/// set of owners the binding was reseated TO across the arms;
/// `untouched_arms` are the arms (by their Flow IR [`BlockId`]s) that kept
/// the pre-branch owner — including the synthetic fall-through block of
/// an else-less if. Codegen fires the recorded `ConditionalDeadDrop` at
/// the start of each listed block.
#[derive(Clone, Debug)]
pub(crate) struct ReseatDrop {
    pub if_stmt: TirRef,
    pub name: StringId,
    pub pre_owner: Owner,
    pub reseat_owners: HashSet<Owner>,
    pub untouched_arms: Vec<BlockId>,
}

/// A `Slice`/`ToView` instruction whose base is a borrowed `str`/`bytes`
/// parameter. Codegen promotes the callee's inline copy of the param to
/// heap for such bases; the post-walk pass schedules the free.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PromoCandidate {
    /// The `Slice`/`ToView` instruction.
    pub(crate) view_inst: TirRef,
    /// Its base operand (a `Var` of the borrowed param).
    pub(crate) base: TirRef,
    /// The statement being walked when the inst was visited: the
    /// binding statement for bound views, the enclosing statement for
    /// transient slices.
    pub(crate) stmt: TirRef,
}

/// Validate move safety for every function body. Emits diagnostics
/// into `sink`. Returns an [`OwnershipSidecar`] that codegen consults
/// to decide where to emit `ryo_str_free` calls. The TIR itself is
/// never mutated. The sidecar is positional with `tirs`: entry `i`
/// belongs to `tirs[i]`. `flows` is the canonical Flow IR per body, built
/// once per pipeline run right after sema and shared with codegen.
///
/// Warnings (W0001/W0003/W0004) are buffered and flushed only if the
/// whole unit compiles without errors — an error that fired anywhere,
/// in any function and from any stage, makes every derived warning
/// noise ("declared but never used" for a binding whose use failed to
/// compile, etc.). Analysis itself always runs to completion so the
/// sidecar (frees, anchors) is complete for tooling that reads it on
/// the error path.
pub fn check(
    tirs: &[Tir],
    flows: &[ryo_core::flow::Flow],
    pool: &InternPool,
    sink: &mut DiagSink,
) -> OwnershipSidecar {
    debug_assert_eq!(tirs.len(), flows.len(), "one Flow IR per TIR body");
    #[cfg(all(test, debug_assertions))]
    validate::clear_test_ownerships();
    let mut sidecar = OwnershipSidecar::default();
    let synth = frees::SynthCalleeIds::resolve(pool);
    let mut warnings = DiagSink::new();
    for (tir, flow) in tirs.iter().zip(flows.iter()) {
        let mut func_sidecar = FunctionSidecar::new(tir.name, tir.instructions.len());
        analyze_function(
            tir,
            flow,
            pool,
            &synth,
            sink,
            &mut warnings,
            &mut func_sidecar,
        );
        sidecar.functions.push(func_sidecar);
    }
    if !sink.has_errors() {
        for d in warnings.into_diags() {
            sink.emit(d);
        }
    }
    sidecar
}

#[allow(clippy::too_many_lines)]
fn analyze_function(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    synth: &frees::SynthCalleeIds,
    sink: &mut DiagSink,
    warnings: &mut DiagSink,
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
        // Dense per-instruction tables, sized to the arena (slot 0 is
        // the reserved sentinel and stays empty). `loop_nesting` /
        // `view_last_use` / `view_defer_loop` are initialized by the
        // pre-pass assignments below instead.
        owner_at_read: vec![None; tir.instructions.len()],
        ..Ownership::default()
    };

    // M8.4: view-liveness pre-pass (P4, final spec §3.2). The walk
    // consults these walk-constant tables to know when it passes a
    // projection's last use (and which loop exit defers it). The
    // nesting map the deferral table is derived from is computed first;
    // both are built from Flow IR blocks (see views.rs / loops.rs).
    own.loop_nesting = collect_loop_nesting(tir, flow);
    let liveness = collect_view_liveness(tir, flow, pool, &own.loop_nesting);
    own.view_last_use = liveness.last_use;
    own.view_defer_loop = liveness.defer_to_loop;
    own.arm_last_reads = liveness.arm_last_reads;

    // G2: dense owner-place resolution, built once per body from Flow
    // facts — the single source of "which place owns inst r's value".
    // Param places are minted first (the outermost scope), then
    // temp/token/match-result producers, then per-step facts (write
    // beats read beats view).
    own.inst_place = vec![None; tir.instructions.len()];
    own.param_places = vec![PlaceId(u32::MAX); tir.params.len()];
    for (i, place) in flow.places.iter().enumerate() {
        if let ryo_core::flow::Place::Param { index, .. } = place {
            own.param_places[*index as usize] = PlaceId(i as u32);
        }
    }
    debug_assert!(
        own.param_places.iter().all(|p| p.0 != u32::MAX),
        "every param has a place"
    );
    for inst in 1..tir.instructions.len() {
        let r = TirRef::from_raw(u32::try_from(inst).expect("TIR arena index fits u32"));
        if let Some(p) = flow.temp_of(r) {
            own.inst_place[inst] = Some(p);
        } else if let Some(p) = flow.match_result(r) {
            own.inst_place[inst] = Some(p);
        } else if let Some(p) = flow_owner_place(flow, r) {
            own.inst_place[inst] = Some(p);
        }
    }
    for (owner_inst, places) in flow.token_places_iter() {
        // The token refs come from the producing instruction's wire —
        // a match's by-move refs, a destructure's bound-owner refs.
        let refs = match tir.inst(owner_inst).tag {
            TirTag::Match => tir.match_by_move_owner_refs(owner_inst, pool),
            TirTag::Destructure => tir.destructure_bound_owner_refs(owner_inst),
            other => unreachable!("token places only for matches/destructures: {other:?}"),
        };
        debug_assert_eq!(refs.len(), places.len(), "token/place mismatch");
        for (r, p) in refs.iter().zip(places) {
            own.inst_place[r.index()] = Some(*p);
        }
    }

    // Initialise per-parameter state. Move-typed params start at
    // `Valid` (the callee owns them); borrowed and inout params start
    // at `Borrowed` (the callee does not own the buffer — inout adds
    // mutability, not ownership). Copy-typed params skip the lattice
    // entirely. Owner identity is the param's place; names are labels.
    for (idx, param) in tir.params.iter().enumerate() {
        if !needs_tracking(param.ty, pool) {
            continue;
        }
        let owner = Owner(own.param_places[idx]);
        let state = match param.mode {
            ParamMode::Move => OwnerState::Valid,
            ParamMode::Borrow | ParamMode::Inout => OwnerState::Borrowed,
        };
        own.states.insert(owner, state);
        own.current_owner.insert(own.param_places[idx], owner);
        // The param is the current binding for its name until a
        // shadowing `VarDecl` installs a different one.
        own.binding_of_name.insert(param.name, TirRef::param(idx));
        if param.mode == ParamMode::Inout {
            own.inout_str_params.insert(param.name);
        }
    }

    // The forward walk, driven by Flow IR: the driver iterates blocks
    // in construction order; each block's steps run in evaluation
    // order, then the terminator dispatches (if-cascade / match /
    // loop / return epilogue). Construct handlers walk their sub-
    // regions and mark them processed, so the id-ordered driver never
    // re-processes them; join/exit blocks stay unmarked and are
    // processed later with the merged state. Dead continuations the
    // regions never reach (statements after a return/break) are
    // processed inline by the driver in block order.
    let mut w = Walk::new(tir, flow, pool);
    let epoch = w.next_epoch();
    for b in flow.forward_order() {
        if w.is_processed(b) {
            continue;
        }
        w.walk_region(
            &mut own,
            sink,
            sidecar,
            b,
            RegionStop::At(BlockId::NONE),
            epoch,
        );
    }
    // Owner-keyed last-use reads and per-name latest reads were
    // recorded at `Var` steps during the walk (see `Walk::last_use`) —
    // the Flow IR replacement for the old post-walk `collect_last_uses`
    // scan, with identical recording rules (by-name latest read; the
    // per-read `owner_at_read` snapshot keys the owner map).
    let last_use = std::mem::take(&mut w.last_use);
    let last_read_by_name = std::mem::take(&mut w.last_read_by_name);
    let body_stmts = tir.body_stmts();
    // P5 (final spec §3.2): root owner → every view that ever
    // projected it (sorted for deterministic iteration).
    // Program-order ranks, built once per function and shared by the
    // P5 deferral (`defer_anchor`) and the redundant-materialize pass.
    // G2: the document rank table is the Flow builder's emission
    // stamp, dense per instruction (0 = unstamped). Same shape as the
    // old `program_order` table — every consumer's comparisons carry
    // over.
    let order: Vec<u32> = (0..tir.instructions.len() as u32)
        .map(|i| {
            if i == 0 {
                0
            } else {
                flow.order_of(TirRef::from_raw(i)).unwrap_or(0)
            }
        })
        .collect();
    // Values assigned inside a loop to a binding declared BEFORE it
    // survive the loop — driven by the walk-recorded body regions and
    // document-order ranks (see loops.rs).
    let loop_carried_owners: HashSet<TirRef> = collect_loop_carried_owners(tir, flow, &w);
    let mut projections_of: HashMap<Owner, Vec<TirRef>> = HashMap::new();
    for (view, root) in &own.root_owner {
        if let Some(vi) = view.inst_tirref(flow) {
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
    // current `FatLocals`, which is the path-correct buffer.
    let reassign_targets: HashSet<Owner> = sidecar
        .free_on_reassign
        .iter()
        .flatten()
        .map(|t| {
            // Inout-param reseats store the param's virtual ref.
            if let Some(idx) = t.as_param_index() {
                Owner(own.param_places[idx as usize])
            } else {
                Owner(own.inst_place[t.index()].expect("reassign target owner place"))
            }
        })
        .collect();
    // Reassign points per binding name, and owner-value → binding-name
    // maps (see the helpers for what the suppressions below use them).
    let (reassign_orders, decl_of_init, assign_value_of) = binding_value_maps(tir, sidecar, &order);
    let live_binding_owners: HashSet<Owner> = own.current_owner.values().copied().collect();
    // Owners that escape through an `inout str` param's write-back
    // pointer at function exit: whatever value is CURRENTLY bound to each
    // inout param name leaves the function alive, so neither the last-use
    // pass nor the dead-store drain may free it (or warn about it).
    let inout_escape_owners: HashSet<Owner> = own
        .inout_str_params
        .iter()
        .filter_map(|n| {
            own.current_owner
                .get(&own.param_places[own.param_index[n]])
                .copied()
        })
        .collect();
    // Iterate owners in a sorted order so `drops` push
    // order does not depend on HashMap iteration order.
    let mut sorted_states: Vec<(Owner, OwnerState)> =
        own.states.iter().map(|(o, s)| (*o, s.clone())).collect();
    sorted_states.sort_by_key(|(o, _)| owner_sort_key(o));
    for (owner, state) in &sorted_states {
        if !matches!(state, OwnerState::Valid) {
            continue;
        }
        match owner.inst_tirref(flow) {
            Some(r) => {
                let stale_reassign_target =
                    reassign_targets.contains(owner) && !live_binding_owners.contains(owner);
                if stale_reassign_target || inout_escape_owners.contains(owner) {
                    continue;
                }
                if let Some(&after) = last_use.get(&r) {
                    // P5 (final spec §3.2): defer the destruction to
                    // the last use of any projection of this owner.
                    let after = defer_anchor(after, owner, &projections_of, &last_use, &order);
                    // Conditional last use: a named binding whose LAST
                    // READ is inside a branch/loop is re-anchored by
                    // `last_use_anchor` (see loops.rs) — normally to
                    // the branch exit, the earliest point where the
                    // value is dead on ALL paths, EXCEPT a loop-carried
                    // owner whose binding is read again after the loop.
                    let anchor = last_use_anchor(
                        tir,
                        r,
                        after,
                        &order,
                        &last_read_by_name,
                        &loop_carried_owners,
                        own.owner_hazards.iter().any(|&(o, site, kind)| {
                            o == *owner
                                && matches!(kind, HazardKind::Move)
                                && matches!(tir.inst(site).tag, TirTag::Return | TirTag::ReturnVoid)
                        }),
                    );
                    // Anchor-order suppression: the owner is its
                    // binding's current owner (the stale-target check
                    // above did not fire — a loop merge's first-wins
                    // reseat leaves the PRE-reassign owner seated), but
                    // the binding is reassigned AFTER this anchor. The
                    // reassign-Free then releases this exact buffer at
                    // the first store that displaces it, and codegen's
                    // binding-path redirect frees the slot's CURRENT
                    // content wherever a later Free targets this owner —
                    // so scheduling a last-use Free here double-frees.
                    // Whatever the binding holds after its last
                    // reassign is released by that value's own final
                    // Free (last-use / dead-store / loop-exit), which
                    // redirects to the same home slot. The reassign set
                    // is the owner's OWN binding (its declaring
                    // VarDecl): a same-named shadow's reassigns target
                    // a different binding and home slot, so they must
                    // not suppress this owner — when they did, the
                    // outer buffer leaked on every path where the
                    // outer reassign never ran.
                    // A value belongs to exactly one binding's
                    // home-slot lineage — either a declaring VarDecl's
                    // init or a single reseating Assign's value, never
                    // both — so the `.or()` below is unambiguous.
                    debug_assert!(
                        !(decl_of_init.contains_key(&r) && assign_value_of.contains_key(&r)),
                        "owner {r:?} is both a VarDecl init and an Assign value"
                    );
                    if reassign_targets.contains(owner)
                        && let Some(&binding) = decl_of_init.get(&r).or(assign_value_of.get(&r))
                        && reassign_orders.get(&binding).is_some_and(|v| {
                            v.iter().any(|&rr| anchored_after(tir, &order, rr, anchor))
                        })
                    {
                        continue;
                    }
                    sidecar.drops.push(DropOp {
                        position: DropPosition::AfterStep { inst: anchor },
                        target: r,
                        span: tir.span(r),
                    });
                }
            }
            None => {
                // A param owner: an `inout str` param's value escapes
                // through the write-back pointer — never freed by the
                // callee, even if a branch merge left its owner stamped
                // Valid.
                let name = flow.place(owner.0).name();
                if own.inout_str_params.contains(&name) {
                    continue;
                }
                let idx = param_idx(&own.param_index, name);
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
                        // arms (leak); arms that `return` are covered
                        // by the return epilogue. The declared-before
                        // check locals need is trivially true here:
                        // params precede the body.
                        Some(outermost_branch_of(tir, after).unwrap_or(after))
                    }
                    None => body_stmts.last().copied(),
                }) else {
                    continue;
                };
                sidecar.drops.push(DropOp {
                    position: DropPosition::AfterStep { inst: after },
                    target: TirRef::param(idx),
                    span: tir.params[idx].span,
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
    let named_inits: HashSet<TirRef> = collect_named_inits(tir, flow);
    let mut consumer_of: HashMap<TirRef, TirRef> = HashMap::new();
    find_consumers(flow, tir.instructions.len(), &mut consumer_of);
    let mut sorted_temps: Vec<Owner> = own.temp_owners.iter().copied().collect();
    sorted_temps.sort_by_key(owner_sort_key);
    for temp in sorted_temps {
        // Temps always name an instruction, never a param.
        let Some(t) = temp.inst_tirref(flow) else {
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
            // A temp PRODUCED in an if's main condition exists on every
            // path through the branch (the condition is always
            // evaluated), so anchor its Free after the if itself: codegen
            // then emits it in the merge block on all paths. Anchoring
            // after the consumer lets the end-of-statement sweep fire
            // inside the taken arm only, leaking the temp on every
            // not-taken path. Skip when no path reaches the merge block
            // (every arm returns or jumps out) — the anchor would never
            // fire there; the return-epilogue / loop-exit passes own
            // those paths.
            let anchor = match enclosing_if_main_cond(tir, t) {
                Some(if_stmt) if if_may_fall_through(tir, if_stmt) => if_stmt,
                _ => anchor,
            };
            sidecar.drops.push(DropOp {
                position: DropPosition::AfterStep { inst: anchor },
                target: t,
                span: tir.span(t),
            });
        }
        // No consumer = unreachable from any body statement; can't
        // happen in well-formed TIR. Don't emit (no consumer means
        // codegen's inst_values won't have ptr/cap either).
    }

    // Promotion-buffer frees for borrowed-param view bases: codegen
    // promotes the callee's inline copy of the param to heap so the
    // view addresses stable memory; the buffer is freed at the view's
    // death. Only bases recorded by the walk qualify (borrowed,
    // owner-typed, param-rooted).
    let candidates = std::mem::take(&mut own.promo_candidates);
    // (base, normal anchor) pairs, kept for the return epilogue below.
    let mut promo_anchors: Vec<(TirRef, TirRef)> = Vec::with_capacity(candidates.len());
    for cand in candidates {
        let rank = |r: TirRef| order.get(r.index()).copied().unwrap_or(0);
        let bound = own.root_owner.contains_key(&Owner(
            own.inst_place[cand.view_inst.index()].expect("view owner place"),
        ));
        let after = if bound {
            if let Some(loop_ref) = Ownership::dense_get(&own.view_defer_loop, cand.view_inst) {
                // Created outside a loop, last read inside it: dies at
                // the loop's exit. The creation dominates the loop, so
                // the scratch slot is initialized on every path here.
                loop_ref
            } else if let Some(lu) = Ownership::dense_get(&own.view_last_use, cand.view_inst) {
                // P5: a reslice of this view (e.g. `w = s[0:2];
                // v = w[0:1]`) keeps the same promotion buffer alive —
                // defer to the last use of any projection of this view,
                // exactly as owner frees do.
                let lu = defer_anchor(
                    lu,
                    &Owner(own.inst_place[cand.view_inst.index()].expect("view owner place")),
                    &projections_of,
                    &last_use,
                    &order,
                );
                // Conditional last use: re-anchor to the branch exit —
                // but only when the view's creation dominates the
                // branch, else the slot may be uninitialized on the
                // skipped path (mirrors the declared-before check of
                // the Owner::Param free path).
                match outermost_branch_of(tir, lu) {
                    Some(branch_stmt)
                        if branch_may_not_return(tir, branch_stmt)
                            && rank(cand.view_inst) < rank(branch_stmt) =>
                    {
                        branch_stmt
                    }
                    _ => lu,
                }
            } else if own
                .loop_nesting
                .ancestors_innermost_first(cand.stmt)
                .next()
                .is_some()
            {
                // Bound but never read, with the slice inside a loop.
                // The liveness pre-pass's first-wins back-edge merge
                // attributes in-loop reads of a loop-rebound view to
                // the PRE-loop slice inst, leaving the in-loop slice
                // with no recorded last use. Anchoring at the rebind
                // statement fires every iteration, freeing the buffer
                // the just-rebound view still points into. Anchoring
                // at the enclosing loop's exit is equally unsound: the
                // binding's slot holds the in-loop slice's final
                // buffer, which a read after the loop still reaches —
                // the loop-exit free releases it first (UAF). Anchor
                // at the end of the function body instead, the same
                // anchor the Owner::Param never-read path uses: views
                // cannot escape the function, free-before-overwrite at
                // the promotion site releases intermediate iterations,
                // the entry-zeroed flag covers zero iterations, and
                // the return-epilogue anchors below cover early exits.
                match body_stmts.last().copied() {
                    Some(last) => last,
                    None => continue,
                }
            } else {
                // Bound but never read, outside any loop: free right
                // after the statement that created the slice.
                cand.stmt
            }
        } else {
            // Transient slice (e.g. `print(s[0:1])`): the projection
            // dies with its enclosing statement.
            cand.stmt
        };
        sidecar.promotion_frees.push(PromoFree {
            position: DropPosition::AfterStep { inst: after },
            base: cand.base,
            span: tir.span(cand.view_inst),
        });
        // Gap 5: the allocation identity — codegen promotes on the
        // view write (flag-guarded conditional heap triple); the
        // validator pairs this point with the base's releases.
        sidecar.promo_allocs.push(PromoFree {
            position: DropPosition::AfterStep {
                inst: cand.view_inst,
            },
            base: cand.base,
            span: tir.span(cand.view_inst),
        });
        promo_anchors.push((cand.base, after));
    }

    // Return epilogue for promotion buffers, mirroring the owner
    // return-epilogue pass below: each promo free above has exactly one
    // anchor, so any path that returns before it leaks the buffer (the
    // last use inside the return operand lands the anchor on a sub-inst
    // the end-of-statement sweep skips on terminators; a loop-deferred
    // anchor is bypassed by a `return` inside the loop). Anchor a copy
    // of every candidate's free at every Return/ReturnVoid — the
    // flag-conditional, flag-clearing emission makes the extra anchors
    // harmless no-ops on paths that already freed (or never promoted)
    // the buffer. Codegen's Return arms fire due promo frees before the
    // `return_` terminator. Deduped against the candidate's normal
    // anchor when that anchor IS the return statement.
    let mut return_stmts: Vec<TirRef> = Vec::new();
    collect_return_stmts(flow, &w.block_order, &mut return_stmts);
    let mut promo_epilogue_emitted: HashSet<(TirRef, TirRef)> = HashSet::new();
    for return_stmt in return_stmts {
        for &(base, normal_anchor) in &promo_anchors {
            if normal_anchor == return_stmt {
                continue;
            }
            if !promo_epilogue_emitted.insert((return_stmt, base)) {
                continue;
            }
            sidecar.promotion_frees.push(PromoFree {
                position: DropPosition::AfterStep { inst: return_stmt },
                base,
                span: tir.span(return_stmt),
            });
        }
    }

    // Fallthrough backstop: a body that can reach its end without an
    // explicit return exits through codegen's synthesized return, which
    // has no TIR statement for the return epilogue above to anchor on —
    // e.g. a view whose only use is inside a returning if-arm keeps its
    // in-arm anchor, and the not-taken path falls through with the
    // promotion buffer still live. Anchor a copy of every candidate's
    // free after the final body statement; flag-conditional emission
    // keeps it a harmless no-op on paths that already freed (or never
    // promoted) the buffer.
    if let Some(&last) = body_stmts.last() {
        let may_fall_through = match tir.inst(last).tag {
            TirTag::Return | TirTag::ReturnVoid => false,
            TirTag::IfStmt => if_may_fall_through(tir, last),
            _ => true,
        };
        if may_fall_through {
            for &(base, normal_anchor) in &promo_anchors {
                if normal_anchor == last {
                    continue;
                }
                if !promo_epilogue_emitted.insert((last, base)) {
                    continue;
                }
                sidecar.promotion_frees.push(PromoFree {
                    position: DropPosition::AfterStep { inst: last },
                    base,
                    span: tir.span(last),
                });
            }
        }
    }

    // Warnings (W0001 below, W0003/W0004 in frees.rs) go to the
    // `warnings` buffer, never `sink`: `check` flushes the buffer only
    // when the whole unit is error-free, so an error from any function
    // — analyzed before or after this one — silences them all.

    // Dead-store survivors: emit W0001 and schedule a Free anchored
    // after the declaring instruction. Skip owners already covered by
    // `free_on_reassign` to avoid double-freeing the same allocation.
    // (`reassign_targets` was computed above for the last-use pass.)
    let mut sorted_dead: Vec<(Owner, (StringId, Span, TirRef))> = own
        .pending_dead_store
        .iter()
        .map(|(o, v)| (*o, *v))
        // The shared untracked sentinel (error-shaped TIR seats) owns
        // nothing schedulable; the unit's own diagnostic carries it.
        .filter(|(o, _)| o.0 != UNTRACKED_PLACE)
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
        // Never pile a warning onto an already-failing unit — `check`
        // drops this buffer if any error fired anywhere in the unit.
        warnings.emit(Diag::warning(
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
        // binding's CURRENT FatLocals (the init→name map): the final iteration's
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
        sidecar.drops.push(DropOp {
            position: DropPosition::AfterStep { inst: anchor },
            target: owner.tirref(flow),
            span: *span,
        });
        if also_in_body {
            sidecar.drops.push(DropOp {
                position: DropPosition::AfterStep { inst: *decl_inst },
                target: owner.tirref(flow),
                span: *span,
            });
        }
    }

    // Match-arm bindings never read inside their arm (M12): the walk
    // already scheduled their arm-gated Frees at the arm ends; only the
    // W0001 belongs here. Dedup — loop convergence re-walks can record
    // the same binding twice. Unlike the drain above, no inout /
    // escape checks: an arm-local bind is a fresh value that dies with
    // the arm regardless of any same-named outer binding.
    let mut seen_arm_dead: HashSet<(u32, usize, usize)> = HashSet::new();
    for (name, span) in &own.arm_dead_stores {
        if !seen_arm_dead.insert((name.raw(), span.start, span.end)) {
            continue;
        }
        warnings.emit(Diag::warning(
            *span,
            DiagCode::DeadStore,
            format!("value `{}` is declared but never used", pool.str(*name)),
        ));
    }

    // W0003 case B (M8.4.1.2): redundant bound materializations. Runs
    // after the walk so the escape classification it reuses — final
    // lattice states plus the hazard log — is complete.
    warn_redundant_materialize(tir, flow, pool, synth, &own, &order, warnings);

    // W0004: bound `to_bytes()` copies that never mutate nor escape.
    // Same post-walk shape as W0003 case B, plus the last-use map to
    // bound the window where a receiver hazard collides with the
    // suggested view.
    warn_redundant_to_bytes(tir, flow, pool, synth, &own, &order, &last_use, warnings);

    // Convert honored reseat records into block-start DropOps (see the
    // helper for the honoring rules).
    convert_honored_reseat_drops(tir, &own, flow, sidecar, &decl_of_init, &assign_value_of);

    // Drop redundant branch-exit Frees of branch-reseated values before
    // the loop-exit / return-epilogue passes dedup against the schedule.
    suppress_redundant_exit_frees(
        tir,
        flow,
        sidecar,
        &order,
        &decl_of_init,
        &assign_value_of,
        &reassign_orders,
    );

    // Loop-exit Frees run LAST so they can inspect the now-complete
    // `drops` schedule and only add jump-anchored Frees for inside-loop
    // owners that no earlier pass already covered.
    schedule_loop_exit_frees_in(tir, flow, &own, sidecar, &body_stmts, None);

    schedule_return_epilogue_frees(
        tir,
        flow,
        &own,
        sidecar,
        &body_stmts,
        &order,
        &decl_of_init,
        &assign_value_of,
        &reassign_orders,
    );

    // backend validators: debug-only cleanup-schedule completeness validator (see
    // `validate`). Runs after every scheduling pass so the sidecar is
    // final; any broken invariant is a compiler bug, so panic. The path
    // checks run only on bodies the analysis itself found clean (an
    // errored body's best-effort schedule never reaches codegen).
    #[cfg(debug_assertions)]
    {
        validate::validate_cleanup(tir, flow, pool, &own, sidecar, !sink.has_errors());
    }
}

/// Return epilogue: destroy locals still live at an early return.
/// Runs LAST so every other Free pass has populated the `drops`
/// schedule and we can dedup against it — a value is skipped when another
/// Free already fires on the return's path, or the dead-store drain
/// owns it (its after-decl Free covers every path). Codegen emits
/// due Frees before every `return_`, so anchoring at the return
/// statement itself fires exactly on that exit path.
#[allow(clippy::too_many_arguments)]
fn schedule_return_epilogue_frees(
    tir: &Tir,
    flow: &Flow,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    body_stmts: &[TirRef],
    order: &[u32],
    decl_of_init: &HashMap<TirRef, TirRef>,
    assign_value_of: &HashMap<TirRef, TirRef>,
    reassign_orders: &HashMap<TirRef, Vec<TirRef>>,
) {
    let mut epilogue_emitted: HashSet<(TirRef, TirRef)> = HashSet::new();
    for (return_stmt, owners) in &own.return_epilogue {
        let mut on_path: HashSet<TirRef> = HashSet::new();
        let _ = tir.collect_jump_path(body_stmts, *return_stmt, &mut on_path);
        // A Free anchored after a branch CONTAINING the return never
        // fires on the return's path — the branch statement does not
        // complete before the return exits. Exclude ancestors from the
        // covering set (the path walk counts them as "passed through",
        // which is true for evaluation but false for after-anchoring).
        let ancestors: HashSet<TirRef> = ancestor_branches_of(tir, *return_stmt)
            .into_iter()
            .collect();
        // Bindings whose current value THIS return moves out (recorded
        // by the walk's consume pass, keyed by the consumed owner — see
        // below for why the owner named there is not the only one that
        // must be suppressed).
        let binding_of = |t: TirRef| -> Option<TirRef> {
            decl_of_init.get(&t).or(assign_value_of.get(&t)).copied()
        };
        let consumed_bindings: HashSet<TirRef> = own
            .owner_hazards
            .iter()
            .filter(|&&(_, site, _)| site == *return_stmt)
            .filter_map(|&(o, _, _)| binding_of(o.tirref(flow)))
            .collect();
        // Epilogue Frees decided for THIS return, staged before they
        // join the `drops` schedule. The binding-covering scan must see
        // them: two owners of the SAME binding live at one return all
        // lower through the home-slot redirect, so the first decision
        // covers the rest — but only if the scan can observe it.
        let mut staged: Vec<DropOp> = Vec::new();
        for owner in owners {
            if own.pending_dead_store.contains_key(owner) {
                continue;
            }
            let r = owner.tirref(flow);
            // A return that moves a binding's value hands the home
            // slot's content to the caller (sret / register copy). The
            // consume record names the owner the read resolved to —
            // under a first-wins loop/branch merge that is the
            // pre-reassign owner, while the buffer physically leaving
            // is a DIFFERENT, still-`Valid` owner of the same binding
            // (the in-loop reseated value). Destroying either here
            // frees the returned buffer: skip every owner of a binding
            // this return consumes.
            if binding_of(r).is_some_and(|b| consumed_bindings.contains(&b)) {
                continue;
            }
            if !epilogue_emitted.insert((*return_stmt, r)) {
                continue;
            }
            // A covering Free must fire on a path that REACHES this
            // return, at an anchor that CAN fire. Two anchor shapes
            // never do: a terminator jump inside a traversed subtree
            // (a `return`/`break`/`continue` of its own — codegen
            // cannot sweep after a terminator, so the anchor only
            // services its own exit), and — the dead-anchor
            // family — any anchor inside THIS return's own operand
            // subtree: the statement-end sweep that would fire it is
            // skipped after the Return terminator, so a last-use Free
            // anchored on the returned expression's reads never fires.
            // `collect_jump_path` counts both shapes as on-path.
            // Excluding them keeps the covering sound; without it a
            // dead anchor suppresses this epilogue Free and codegen's
            // leak-direction assert fires.
            let fires_on_this_path = |after: TirRef| -> bool {
                !tir.contains_reachable(*return_stmt, after)
                    && !matches!(
                        tir.inst(after).tag,
                        TirTag::Return | TirTag::ReturnVoid | TirTag::Break | TirTag::Continue
                    )
            };
            let covered = anchored_drops(sidecar).any(|fp| {
                fp.target == r
                    && on_path.contains(&drop_anchor(fp))
                    && !ancestors.contains(&drop_anchor(fp))
                    && fires_on_this_path(drop_anchor(fp))
            });
            if covered {
                continue;
            }
            // Superseded-by-reassign: the owner is a value of a binding
            // that an on-path Assign BEFORE this return reseated away
            // (to a DIFFERENT value), and that Assign is certain to have
            // executed — it is not inside a loop the return sits
            // outside of (a zero-iteration path skips the reassign, so
            // the owner may still hold this value at the return). The
            // reassign's displacement Free (`free_on_reassign`) already
            // released this buffer at the store, so an epilogue Free
            // here would double-free it. Identity is by binding: a
            // same-named shadow's reassigns target a different binding
            // and must not suppress this owner's cleanup.
            let ret_loops: Vec<TirRef> = own
                .loop_nesting
                .ancestors_innermost_first(*return_stmt)
                .collect();
            let certainly_executed = |a: TirRef| {
                on_path.contains(&a)
                    && !own
                        .loop_nesting
                        .ancestors_innermost_first(a)
                        .any(|l| !ret_loops.contains(&l))
            };
            let superseded = binding_of(r).is_some_and(|binding| {
                reassign_orders.get(&binding).is_some_and(|assigns| {
                    assigns.iter().any(|&a| {
                        order[a.index()] < order[return_stmt.index()]
                            && certainly_executed(a)
                            && tir.assign_view(a).value != r
                    })
                })
            });
            if superseded {
                continue;
            }
            // Binding-covering: codegen's binding-path redirect lowers
            // a Free for ANY of a binding's values to "free the home
            // slot's CURRENT content". Two frees that redirect to the
            // same slot — e.g. a loop-exit Free targeting the in-loop
            // reseated value and this epilogue Free targeting the
            // merge-seated pre-reassign owner — release the same
            // runtime buffer twice. Covered when an on-path Free
            // targets another value of the SAME binding and anchors
            // after that binding's LAST reassign: whatever the binding
            // holds at the return is exactly what that Free released.
            // Identity is the declaring VarDecl (never the name): a
            // same-named shadow is a different binding whose frees
            // redirect to a different home slot.
            if let Some(binding) = binding_of(r) {
                // Covered only when the covering Free anchors after
                // EVERY reassign of the binding — otherwise the return
                // may hold a value the covering Free did not release.
                let covers = |fp: &DropOp| {
                    binding_of(fp.target) == Some(binding)
                        && reassign_orders.get(&binding).is_some_and(|v| {
                            !v.is_empty()
                                && v.iter()
                                    .all(|&rr| anchored_after(tir, order, drop_anchor(fp), rr))
                        })
                };
                // Staged epilogue Frees (anchored at THIS return) skip
                // the on-path / terminator gates: they are on this
                // return by construction and Return-anchored epilogue
                // Frees do fire (codegen emits them before the
                // terminator). Schedule frees must pass the gates —
                // an anchor inside this return's own operand subtree
                // or on a different exit never fires here.
                let binding_covered = anchored_drops(sidecar).any(|fp| {
                    on_path.contains(&drop_anchor(fp))
                        && !ancestors.contains(&drop_anchor(fp))
                        && fires_on_this_path(drop_anchor(fp))
                        && covers(fp)
                }) || staged.iter().any(covers);
                if binding_covered {
                    continue;
                }
            }
            staged.push(DropOp {
                position: DropPosition::AfterStep { inst: *return_stmt },
                target: r,
                span: tir.span(*return_stmt),
            });
        }
        sidecar.drops.append(&mut staged);
    }
}

/// Reassign points per BINDING, plus owner-value → binding maps (see
/// the helpers for what the suppressions below use them). A binding is
/// identified by its declaring `VarDecl`'s `TirRef` (or a param's
/// virtual ref) — never by name: one name can denote several bindings
/// (`mut` shadow, sibling scopes), and name-keyed grouping let a
/// shadow's reassigns suppress the outer binding's cleanup.
type BindingValueMaps = (
    HashMap<TirRef, Vec<TirRef>>,
    HashMap<TirRef, TirRef>,
    HashMap<TirRef, TirRef>,
);

/// Maps built by [`binding_value_maps`]: reassign points per binding,
/// VarDecl-init → declaring inst, and Assign-value → binding. The
/// anchor-order suppression and the epilogue's binding-covering check
/// need "is this binding reassigned AFTER some program point" (not just
/// "reassigned somewhere") and "which binding's home slot aliases this
/// owner's buffer" — a Free that lowers through codegen's binding-path
/// redirect releases the slot's CURRENT content, so two frees resolving
/// to the same binding can double-free even with distinct targets.
/// Grouping is by lexical binding (the walk-recorded
/// `sidecar.assign_binding` / the declaring VarDecl), NOT by name: a
/// same-named shadow's reassigns must not enter the outer binding's
/// reassign set — anchored after the outer owner's last use, they
/// suppressed the outer owner's only Free and leaked the outer buffer
/// on every path where the outer reassign did not run.
fn binding_value_maps(tir: &Tir, sidecar: &FunctionSidecar, order: &[u32]) -> BindingValueMaps {
    let mut reassign_orders: HashMap<TirRef, Vec<TirRef>> = HashMap::new();
    for (slot, t) in sidecar.free_on_reassign.iter().enumerate() {
        if t.is_none() {
            continue;
        }
        let r = TirRef::from_raw(slot as u32);
        if order[r.index()] == 0 {
            continue;
        }
        let Some(binding) = sidecar.assign_binding[slot] else {
            continue;
        };
        reassign_orders.entry(binding).or_default().push(r);
    }
    // VarDecl init value → the declaring instruction (that VarDecl IS
    // the binding's identity).
    let mut decl_of_init: HashMap<TirRef, TirRef> = HashMap::new();
    // Assign value → the binding the Assign targeted (recorded by the
    // walk), same purpose for reseated values.
    let mut assign_value_of: HashMap<TirRef, TirRef> = HashMap::new();
    for r in 1..tir.instructions.len() {
        if tir.instructions[r].tag == TirTag::VarDecl {
            let rr = TirRef::from_raw(r as u32);
            let view = tir.var_decl_view(rr);
            decl_of_init.entry(view.initializer).or_insert(rr);
        } else if tir.instructions[r].tag == TirTag::Assign {
            let Some(binding) = sidecar.assign_binding[r] else {
                continue;
            };
            let view = tir.assign_view(TirRef::from_raw(r as u32));
            assign_value_of.entry(view.value).or_insert(binding);
        }
    }
    (reassign_orders, decl_of_init, assign_value_of)
}

/// Convert honored reseat records into block-start DropOps (the unified
/// cleanup schedule): one `DropOp { position: BlockStart { block } }`
/// per untouched arm block, target = the pre-branch owner.
/// A record is honored when a pending entry for one of its reseated
/// owners survived to the drain — i.e. the reassigned value is never
/// read afterwards — so the pre-branch buffer would leak on the paths
/// where the reassign did not happen. (Reads-after clear the pending
/// entry by name, so honored records never collide with the last-use
/// machinery.) Deduped by record: several pending entries can match one
/// record.
///
/// The pending owner must belong to the SAME binding as the record's
/// pre-branch owner. A same-named shadow declared inside an arm
/// re-binds the record's name: the arm-end owner (and its pending
/// entry) then belongs to the shadow binding, and honoring the
/// record on it minted a dead drop against the OUTER binding's home
/// slot — a slot the outer owner's own Free already released, so the
/// drop double-freed the slot's current buffer on every path where
/// it fired. The shadow's reseat value needs no fall-through drop
/// either: the shadow binding does not exist on the arms where its
/// reassign never ran.
fn convert_honored_reseat_drops(
    tir: &Tir,
    own: &Ownership,
    flow: &Flow,
    sidecar: &mut FunctionSidecar,
    decl_of_init: &HashMap<TirRef, TirRef>,
    assign_value_of: &HashMap<TirRef, TirRef>,
) {
    let binding_of_owner = |o: &Owner| -> Option<TirRef> {
        match o.inst_tirref(flow) {
            Some(r) => decl_of_init.get(&r).or(assign_value_of.get(&r)).copied(),
            None => None,
        }
    };
    let mut honored: HashSet<usize> = HashSet::new();
    for (owner, (name, _, _)) in &own.pending_dead_store {
        for (idx, drop) in own.reseat_drops.iter().enumerate() {
            if drop.name == *name && drop.reseat_owners.contains(owner) {
                match (binding_of_owner(&drop.pre_owner), binding_of_owner(owner)) {
                    (Some(pre_binding), Some(owner_binding)) if pre_binding == owner_binding => {
                        honored.insert(idx);
                    }
                    _ => {}
                }
            }
        }
    }
    // Sorted iteration for deterministic sidecar emission order.
    let mut honored: Vec<usize> = honored.into_iter().collect();
    honored.sort_unstable();
    for idx in honored {
        let drop = &own.reseat_drops[idx];
        let target = drop.pre_owner.tirref(flow);
        let span = match drop.pre_owner.inst_tirref(flow) {
            Some(r) => tir.span(r),
            None => tir.span(drop.if_stmt),
        };
        for &block in &drop.untouched_arms {
            sidecar.drops.push(DropOp {
                position: DropPosition::BlockStart { block },
                target,
                span,
            });
        }
    }
}

/// Branch-exit Free redundancy suppression. When a `mut` binding
/// declared before a branch is reseated inside one or more arms and the
/// reseated values are read only inside those arms, `last_use_anchor`
/// re-anchors every such value's last-use Free to the branch exit, and
/// codegen lowers each through the binding's home-slot redirect ("free
/// whatever the slot currently holds"). But the slot holds exactly ONE
/// buffer per path at the exit, so:
///
/// * N reseated values across N arms mint N exit Frees that all release
///   the same slot — N-1 of them are pure double-frees, and the ones
///   whose target is not the binding's last write fail codegen's
///   stale-target redirect filter and fall back to cached arm-local
///   values that do not dominate the merge block (Cranelift verifier
///   error: "uses value from non-dominating inst").
/// * A read of the binding AFTER the branch gives the merge-seated
///   pre-branch owner its own last-use Free, anchored after that read —
///   also slot-redirected — so it releases exactly the buffer the exit
///   Frees would, on every path, later and once. Firing the exit Free
///   additionally frees the slot before the post-branch read (UAF) or
///   double-frees it with the later Free.
///
/// The same argument covers sequential branches (a later branch's exit
/// Free releases the slot on every path that passed the earlier exit)
/// and frees displaced meanwhile (`free_on_reassign` frees the displaced
/// buffer at each reassign store). So an exit-anchored Free of a binding
/// value is dropped when the same binding has another non-gated,
/// non-terminator-anchored Free at-or-after the exit, anchored OUTSIDE
/// the exit's subtree (an in-subtree anchor misses the not-taken /
/// zero-iteration paths), or when several exit Frees share one (binding,
/// exit) anchor — in that
/// case exactly one survives, preferring the Free whose target IS the
/// binding's last write so codegen's redirect filter
/// (`last == target || !all_free_targets.contains(last)`) passes
/// unconditionally. Runs after the last-use / anon-temp / dead-store
/// passes and before the loop-exit / return-epilogue dedups so those
/// passes see the final schedule.
fn suppress_redundant_exit_frees(
    tir: &Tir,
    flow: &Flow,
    sidecar: &mut FunctionSidecar,
    order: &[u32],
    decl_of_init: &HashMap<TirRef, TirRef>,
    assign_value_of: &HashMap<TirRef, TirRef>,
    reassign_orders: &HashMap<TirRef, Vec<TirRef>>,
) {
    let binding_of = |t: TirRef| decl_of_init.get(&t).or(assign_value_of.get(&t)).copied();
    let _ = flow;
    let is_exit_anchor = |after: TirRef, target: TirRef| {
        matches!(
            tir.inst(after).tag,
            TirTag::IfStmt | TirTag::WhileLoop | TirTag::ForRange
        ) && tir.contains_reachable(after, target)
    };
    // The binding's program-order-last write: the last reseating
    // Assign's value, or the declaring VarDecl's initializer.
    let last_write_target = |binding: TirRef| -> Option<TirRef> {
        if let Some(assigns) = reassign_orders.get(&binding)
            && let Some(&slot) = assigns.last()
        {
            return Some(tir.assign_view(slot).value);
        }
        if tir.inst(binding).tag == TirTag::VarDecl {
            return Some(tir.var_decl_view(binding).initializer);
        }
        None
    };

    // Exit-anchored Frees of named binding values, in schedule order.
    let candidates: Vec<(usize, TirRef, TirRef)> = anchored_drops(sidecar)
        .enumerate()
        .filter(|(_, fp)| is_exit_anchor(drop_anchor(fp), fp.target))
        .filter_map(|(i, fp)| binding_of(fp.target).map(|b| (i, drop_anchor(fp), b)))
        .collect();

    let mut drop: HashSet<usize> = HashSet::new();
    // Pass 1: an exit Free covered by a same-binding Free anchored
    // strictly after the exit is redundant on every path. The covering
    // anchor must lie OUTSIDE the exit's own subtree: an anchor inside
    // it (an in-arm read, an in-body Assign) fires only on the paths
    // that reach it — never on the zero-iteration loop path or the
    // not-taken arms — so it cannot release the buffer the exit Free
    // would.
    for &(i, after, binding) in &candidates {
        let covered = anchored_drops(sidecar).enumerate().any(|(j, fp2)| {
            j != i
                && !drop.contains(&j)
                && !matches!(
                    tir.inst(drop_anchor(fp2)).tag,
                    TirTag::Return | TirTag::ReturnVoid | TirTag::Break | TirTag::Continue
                )
                && binding_of(fp2.target) == Some(binding)
                && !tir.contains_reachable(after, drop_anchor(fp2))
                && anchored_after(tir, order, drop_anchor(fp2), after)
        });
        if covered {
            drop.insert(i);
        }
    }
    // Pass 1b: an exit Free whose target was ALREADY released by
    // a reassign displacement before the exit must not keep targeting
    // that owner. Codegen's binding-path redirect filter
    // (`last == target || !all_free_targets.contains(last)`) rejects
    // the redirect for such a stale target, and the cached-value
    // fallback frees the displacement-released buffer a second time on
    // every path where the reassign ran (valgrind: Invalid free). The
    // exit Free's job on every path is "release the slot's current
    // content", which is exactly the redirect — so re-aim the Free at
    // the binding's LAST WRITE, for which the filter passes
    // unconditionally. On paths where the reassign ran the redirect
    // frees the reseated buffer (which has no other Free on the
    // fall-through), on the others it frees the pre-branch buffer.
    //
    // Conservative by construction: only branch exits (loop exits
    // interact with break-anchored Frees), only when a displacing
    // reassign precedes the exit, and only when every Free of the last
    // write either shares THIS exit anchor (pass 2 dedups the pair) or
    // is Return-anchored INSIDE the exit's subtree (those paths never
    // reach the merge, so the redirect cannot double-free with them).
    // Any other anchor could fire before the exit on a reaching path
    // and make the redirect a double free — leave the schedule as-is.
    let displaced_targets: HashSet<TirRef> =
        sidecar.free_on_reassign.iter().flatten().copied().collect();
    for i in 0..sidecar.drops.len() {
        if drop.contains(&i) {
            continue;
        }
        let fp = &sidecar.drops[i];
        // Block-start drops (the unified cleanup schedule) fire at block starts, not on an
        // instruction anchor — the anchor-order scan skips them.
        let Some(anchor) = fp.position.anchor_inst() else {
            continue;
        };
        // A last-use Free conditionally re-anchored by `last_use_anchor`
        // lands on a branch statement whose subtree does NOT contain the
        // target (unlike the arm-value exit Frees in `candidates`), so
        // this scan is schedule-wide.
        if tir.inst(anchor).tag != TirTag::IfStmt {
            continue;
        }
        let Some(binding) = binding_of(fp.target) else {
            continue;
        };
        if !displaced_targets.contains(&fp.target) {
            continue;
        }
        let after = drop_anchor(fp);
        let displaced_before_exit = reassign_orders.get(&binding).is_some_and(|assigns| {
            assigns
                .iter()
                .any(|&r| order[r.index()] < order[after.index()])
        });
        if !displaced_before_exit {
            continue;
        }
        let Some(last_write) = last_write_target(binding) else {
            continue;
        };
        if last_write == fp.target {
            continue;
        }
        // A same-binding Free already at this anchor lowers through the
        // same home-slot redirect and covers this anchor's paths — this
        // one is redundant (and retargeting would mint a double
        // redirect).
        let redundant_at_anchor = anchored_drops(sidecar).enumerate().any(|(j, fp2)| {
            j != i
                && !drop.contains(&j)
                && drop_anchor(fp2) == after
                && binding_of(fp2.target) == Some(binding)
        });
        if redundant_at_anchor {
            drop.insert(i);
            continue;
        }
        let redirect_safe = anchored_drops(sidecar)
            .filter(|fp2| fp2.target == last_write)
            .all(|fp2| {
                drop_anchor(fp2) == after
                    || (tir.contains_reachable(after, drop_anchor(fp2))
                        && matches!(
                            tir.inst(drop_anchor(fp2)).tag,
                            TirTag::Return | TirTag::ReturnVoid
                        ))
            });
        if redirect_safe {
            sidecar.drops[i].target = last_write;
        }
    }
    // Pass 2: same (binding, exit) anchor — keep one Free, preferring
    // the Free whose target is the binding's last write (codegen's
    // redirect filter then passes unconditionally), then the
    // latest-ranked target, then the latest schedule index.
    let mut groups: HashMap<(TirRef, TirRef), Vec<usize>> = HashMap::new();
    for &(i, after, binding) in &candidates {
        if !drop.contains(&i) {
            groups.entry((binding, after)).or_default().push(i);
        }
    }
    let mut group_keys: Vec<(TirRef, TirRef)> = groups.keys().copied().collect();
    group_keys.sort_by_key(|&(binding, after)| (binding.raw(), after.raw()));
    for key in group_keys {
        let members = groups[&key].clone();
        if members.len() < 2 {
            continue;
        }
        let binding = key.0;
        let keep = last_write_target(binding)
            .and_then(|lw| {
                members
                    .iter()
                    .copied()
                    .find(|&m| sidecar.drops[m].target == lw)
            })
            .unwrap_or_else(|| {
                members
                    .iter()
                    .copied()
                    .max_by_key(|&m| (order[sidecar.drops[m].target.index()], m))
                    .expect("group has members")
            });
        for &m in &members {
            if m != keep {
                drop.insert(m);
            }
        }
    }
    if drop.is_empty() {
        return;
    }
    let mut kept = Vec::with_capacity(sidecar.drops.len());
    for (i, fp) in sidecar.drops.drain(..).enumerate() {
        if !drop.contains(&i) {
            kept.push(fp);
        }
    }
    sidecar.drops = kept;
}

/// True when `anchor` fires at or after `point` in control flow:
/// a strictly-later program rank, or `anchor` is a loop/branch
/// statement whose body CONTAINS `point` (containers rank before
/// their bodies in the document rank, so the rank test alone
/// under-orders them).
fn anchored_after(tir: &Tir, order: &[u32], anchor: TirRef, point: TirRef) -> bool {
    order[anchor.index()] > order[point.index()]
        || matches!(
            tir.inst(anchor).tag,
            TirTag::WhileLoop | TirTag::ForRange | TirTag::IfStmt
        ) && tir.contains_reachable(anchor, point)
}

#[cfg(test)]
mod tests;
