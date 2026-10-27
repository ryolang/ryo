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
//! Flow places identify bindings and produced values. The current-owner
//! table maps each binding place to the value that occupies its storage.
//! Anonymous owning values use their producing Flow places.
//! Names remain diagnostic labels.
//!
//! ## Mojo reference
//!
//! See `docs/dev/pl_references/mojo.md`.

use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::flow::{Analysis, BlockId, DropOp, DropPosition, Flow, PlaceId};

mod diag_fmt;
pub(crate) use diag_fmt::*;
mod cleanup;
pub(crate) use cleanup::*;
mod materialization;
pub(crate) use materialization::*;
mod loops;
pub(crate) use loops::*;
mod match_;
pub(crate) use match_::*;
mod merge;
pub(crate) use merge::*;
mod aggregates;
pub(crate) use aggregates::*;
mod transfers;
pub(crate) use transfers::*;
mod exit_frees;
use exit_frees::{
    anchored_after, binding_value_maps, convert_honored_reseat_drops,
    schedule_return_epilogue_frees, suppress_redundant_exit_frees,
};
#[cfg(debug_assertions)]
mod cleanup_validation;
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
    try_check(tirs, flows, pool, sink).expect("ownership requires compatible built artifacts")
}

/// Reject incompatible analysis inputs before reading body-local indices.
pub fn try_check(
    tirs: &[Tir],
    flows: &[ryo_core::flow::Flow],
    pool: &InternPool,
    sink: &mut DiagSink,
) -> Result<OwnershipSidecar, String> {
    try_check_with_optimizations(tirs, flows, pool, sink, true)
}

/// Analyze required cleanup with optional allocation reuse disabled.
pub fn check_without_optimizations(
    tirs: &[Tir],
    flows: &[Flow],
    pool: &InternPool,
    sink: &mut DiagSink,
) -> Result<OwnershipSidecar, String> {
    try_check_with_optimizations(tirs, flows, pool, sink, false)
}

fn try_check_with_optimizations(
    tirs: &[Tir],
    flows: &[Flow],
    pool: &InternPool,
    sink: &mut DiagSink,
    optional_optimizations: bool,
) -> Result<OwnershipSidecar, String> {
    if tirs.len() != flows.len() {
        return Err("ownership requires one Flow per typed body".into());
    }
    for flow in flows {
        flow.require_layout(pool)?;
    }
    debug_assert_eq!(tirs.len(), flows.len(), "one Flow IR per TIR body");
    #[cfg(all(test, debug_assertions))]
    cleanup_validation::clear_test_ownerships();
    let mut sidecar = OwnershipSidecar::default();
    let synth = materialization::SynthCalleeIds::resolve(pool);
    let mut warnings = DiagSink::new();
    for (tir, flow) in tirs.iter().zip(flows.iter()) {
        let mut func_sidecar = FunctionSidecar::for_flow(tir, flow)?;
        analyze_function(
            tir,
            flow,
            pool,
            &synth,
            sink,
            &mut warnings,
            &mut func_sidecar,
            optional_optimizations,
        );
        sidecar.functions.push(func_sidecar);
    }
    if !sink.has_errors() {
        for d in warnings.into_diags() {
            sink.emit(d);
        }
    }
    Ok(sidecar)
}

#[allow(clippy::too_many_lines)]
fn initialize_function_ownership(tir: &Tir, flow: &Flow, pool: &InternPool) -> Ownership {
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

    own
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn analyze_function(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    synth: &materialization::SynthCalleeIds,
    sink: &mut DiagSink,
    warnings: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    optional_optimizations: bool,
) {
    let mut own = initialize_function_ownership(tir, flow, pool);

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
    let cleanup = CleanupContext {
        tir,
        flow,
        pool,
        body_stmts,
        order: &order,
        projections_of: &projections_of,
        last_use: &last_use,
        last_read_by_name: &last_read_by_name,
        loop_carried_owners: &loop_carried_owners,
        reassign_targets: &reassign_targets,
        live_binding_owners: &live_binding_owners,
        inout_escape_owners: &inout_escape_owners,
        reassign_orders: &reassign_orders,
        decl_of_init: &decl_of_init,
        assign_value_of: &assign_value_of,
    };
    schedule_last_use_frees(&cleanup, &own, sidecar);

    schedule_anonymous_temp_frees(&cleanup, &own, sidecar);

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
                    &own.view_last_use,
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
            } else {
                // No possible binding read reaches this view, so its
                // promotion buffer can be freed at creation. Loop
                // back-edges and zero-iteration exits are included in
                // the view-liveness fixed point.
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
        // The allocation identity — codegen promotes on the
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
    if let Some(last) = cleanup.body_stmts.iter().next_back() {
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
                let body = tir
                    .loop_body_view(loop_stmt)
                    .expect("outermost_loop_of returns loops");
                let may_return = body_may_return(tir, &body);
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
    // Dominators are built on demand (break defensive emit only) and
    // shared across every loop in this function.
    let doms = std::cell::OnceCell::new();
    schedule_loop_exit_frees_in(
        tir,
        flow,
        pool,
        &own,
        sidecar,
        &cleanup.body_stmts,
        None,
        &doms,
    );

    schedule_return_epilogue_frees(
        tir,
        flow,
        &own,
        sidecar,
        &cleanup.body_stmts,
        &order,
        &decl_of_init,
        &assign_value_of,
        &reassign_orders,
    );

    if !optional_optimizations {
        sidecar.consumed_concat_lhs.fill(None);
    }
    sidecar.transfers = own
        .owner_hazards
        .iter()
        .filter_map(|&(owner, site, kind)| {
            (kind == HazardKind::Move).then_some(ryo_core::ownership::StorageTransfer {
                source: owner.0,
                target: owner.tirref(flow),
                site,
            })
        })
        .collect();
    sidecar
        .transfers
        .sort_by_key(|transfer| (transfer.site.raw(), transfer.source.0));
    sidecar.transfers.dedup();

    // backend validators: debug-only cleanup-schedule completeness validator (see
    // `cleanup_validation`). Runs after every scheduling pass so the sidecar is
    // final; any broken invariant is a compiler bug, so panic. The path
    // checks run only on bodies the analysis itself found clean (an
    // errored body's best-effort schedule never reaches codegen).
    #[cfg(debug_assertions)]
    {
        let summary = cleanup_validation::validate_cleanup(
            tir,
            flow,
            pool,
            &own,
            sidecar,
            !sink.has_errors(),
        );
        sidecar.validation = Some(summary);
    }
    sidecar.seal();
}

#[cfg(test)]
mod tests;
