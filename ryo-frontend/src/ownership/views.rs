//! M8.4 slice projections and view liveness — split from `mod.rs`.
//!
//! The liveness pre-pass (`collect_view_liveness`) simulates the walk's
//! view-binding discipline (snapshot per branch arm, union at joins;
//! fixed-point merge at loop back-edges) over Flow IR blocks and records
//! each bound view's last read. Per-arm read maps are keyed by the
//! arm's Flow IR entry block as an [`Analysis`] tri-state: a
//! missing/`Unavailable` entry never refines (unavailable information
//! must not justify removing a live projection); a `Present` map
//! overrides/kills per view. Runs before the forward walk so the walk
//! knows when it passes a projection's last use.

use super::aggregates::field_path_of;
use super::{
    LoopNesting, Owner, OwnerState, Ownership, StopKind, collect_region, format_binding,
    needs_tracking,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::flow::{Analysis, BlockId, Flow, PlaceId, Terminator};
use ryo_core::tir::{Span, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
use std::collections::{HashMap, HashSet};

/// Deterministic iteration order for owners: the post passes push
/// `DropOp`s while iterating owner-keyed `HashMap`s, whose iteration
/// order varies per run — sort by the place id (owner identity is the
/// place).
pub(crate) fn owner_sort_key(owner: &Owner) -> (u8, u32) {
    (0, owner.0.0)
}

/// The root owner a `strview`-typed value projects (P3, final spec §3.2)
/// — fact-driven (G2): the inst's owner place is its view place (a
/// slice/conversion) or its read place (a `Var` read, a `ViewAsOwner`
/// re-borrow); a view place names its BASE place, and a base that is
/// itself a view resolves through the registration chain. The base's
/// home yields the seated owner, so reseated bases resolve to the
/// current buffer exactly like reads do.
pub(crate) fn projection_root(
    own: &Ownership,
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    r: TirRef,
) -> Option<Owner> {
    // The projection's place: a slice/conversion's VIEW fact, else the
    // step's READ fact (a `Var` read of a view, a `ViewAsOwner`
    // re-borrow, a FieldAccess chain root), else the owner place.
    let step = flow.step_of(r)?;
    let f = step.facts;
    let p = f.view.or(f.read).or(own.inst_place[r.index()])?;
    // Param-rooted projections: a `move` param's buffer is callee-
    // owned — its projections register, freeze, and defer the exit
    // free exactly like a local's. A borrowed/inout param's (and a
    // `strview` param's) buffer belongs to the caller — the projection
    // resolves to nothing, as before.
    let param_root = |place: PlaceId| -> Option<Owner> {
        match flow.place(place) {
            ryo_core::flow::Place::Param { index, ty, .. } => {
                // Owner-typed (needs-drop) borrow/move params resolve:
                // their slices register, freeze, and defer the exit
                // free exactly like a local's (`move` params defer;
                // borrowed params promote). Inout params and view
                // params (caller-owned storage) resolve to nothing.
                let param = &tir.params[*index as usize];
                if param.mode != ryo_core::tir::ParamMode::Inout && pool.needs_drop(*ty) {
                    Some(Owner(place))
                } else {
                    None
                }
            }
            _ => None,
        }
    };
    if let Some(root) = param_root(p) {
        return Some(root);
    }
    let start = match flow.place(p) {
        ryo_core::flow::Place::View { base, .. } => {
            own.current_owner.get(base).copied().unwrap_or(Owner(*base))
        }
        _ => own.current_owner.get(&p).copied().unwrap_or(Owner(p)),
    };
    param_root(start.0).or_else(|| {
        if matches!(flow.place(start.0), ryo_core::flow::Place::Param { .. }) {
            return None;
        }
        Some(own.root_owner.get(&start).copied().unwrap_or(start))
    })
}

/// The field path a view slices. The chain's field indices are TIR
/// payloads (`field_index` data) — read directly; only the aliasing
/// `Var` arm resolves through the read fact's place.
fn projection_field_path(
    own: &Ownership,
    tir: &Tir,
    pool: &InternPool,
    r: TirRef,
) -> Option<Vec<u32>> {
    let inst = *tir.inst(r);
    if inst.tag == TirTag::ViewAsOwner
        && let TirData::UnOp(inner) = inst.data
    {
        return projection_field_path(own, tir, pool, inner);
    }
    if needs_tracking(inst.ty, pool) {
        if let TirData::FieldAccess { .. } = inst.data {
            return field_path_of(tir, r);
        }
        return None;
    }
    if !pool.is_view(inst.ty) {
        return None;
    }
    match inst.data {
        TirData::Var(_) => own
            .inst_place
            .get(r.index())
            .copied()
            .flatten()
            .map(|p| own.current_owner.get(&p).copied().unwrap_or(Owner(p)))
            .and_then(|owner| own.projection_fields.get(&owner).cloned()),
        TirData::Slice { base, .. } => projection_field_path(own, tir, pool, base),
        TirData::UnOp(inner) if inst.tag == TirTag::ToView => {
            projection_field_path(own, tir, pool, inner)
        }
        _ => None,
    }
}

/// P3 (final spec §3.2): register `view_owner` as a live projection of
/// the root owner its initializer resolves to. Idempotent.
pub(crate) fn register_projection(
    own: &mut Ownership,
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    init: TirRef,
    view_owner: Owner,
) {
    if let Some(root) = projection_root(own, tir, flow, pool, init) {
        own.root_owner.insert(view_owner, root);
        if let Some(path) = projection_field_path(own, tir, pool, init) {
            own.projection_fields.insert(view_owner, path);
        }
        let projections = own.live_projections.entry(root).or_default();
        if !projections.contains(&view_owner) {
            projections.push(view_owner);
        }
    }
}

/// Remove a view from its root's live set (P4). No-op for views that
/// were never registered.
pub(crate) fn remove_projection(own: &mut Ownership, view: Owner) {
    if let Some(&root) = own.root_owner.get(&view)
        && let Some(projections) = own.live_projections.get_mut(&root)
    {
        projections.retain(|p| p != &view);
    }
}

/// End-of-statement projection death (P4, final spec §3.2) — runs after
/// a statement root so a read and a consume within the same statement
/// both see the view as live (borrow-for-the-whole-statement semantics,
/// matching Rule 7).
pub(crate) fn drain_dying_views(own: &mut Ownership) {
    for view in std::mem::take(&mut own.pending_dying) {
        remove_projection(own, view);
    }
}

/// P4 (final spec §3.2): projections whose last read is inside this
/// loop but whose creation is outside it stayed live through the body;
/// they die at the loop's exit.
pub(crate) fn remove_loop_deferred_views(own: &mut Ownership, loop_ref: TirRef) {
    let dead: Vec<Owner> = own
        .view_defer_loop
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let &l = slot.as_ref()?;
            if l != loop_ref {
                return None;
            }
            let raw = u32::try_from(i).expect("TIR arena index fits u32");
            let r = TirRef::from_raw(raw);
            Some(Owner(own.inst_place[r.index()].expect("view owner place")))
        })
        .collect();
    for view in dead {
        remove_projection(own, view);
    }
}

/// Branch-merge liveness (P4, final spec §3.2): a view whose last read
/// is inside this branch construct (block membership) has no reads
/// after the join on ANY path, so it is dead at the join even though
/// the union rule in `merge_branches` kept it live. Loop-deferred views
/// are exempt (their death is owned by the enclosing loop's exit).
pub(crate) fn prune_branch_dead_projections(
    own: &mut Ownership,
    flow: &Flow,
    blocks: &HashSet<BlockId>,
) {
    // (flow names the places' blocks; kept in the signature.)
    let dead: Vec<Owner> = own
        .root_owner
        .keys()
        .filter(|view| {
            let Some(vi) = view.inst_tirref(flow) else {
                return false;
            };
            // P2/P4: loop-deferred views die at loop exit, not here.
            if Ownership::dense_get(&own.view_defer_loop, vi).is_some() {
                return false;
            }
            Ownership::dense_get(&own.view_last_use, vi)
                .is_some_and(|lu| flow.step_block_of(lu).is_some_and(|b| blocks.contains(&b)))
        })
        .copied()
        .collect();
    for view in dead {
        remove_projection(own, view);
    }
}

/// P2 freeze (final spec §3.2): while any slice projection of an owner
/// is live, moving or mutating the owner is a compile error. `verb` is
/// "move" (consume sites) or "mutate" (`inout` args, reassignment). The
/// diagnostic points at the consume site with a note at the projection's
/// last use; a projection that is never read notes its creation.
/// Suppressed when the owner is already `Moved` (E0020 covers it) or,
/// for "move", when it is `Borrowed` (E0021/E0022 cover it).
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_source_projected(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    own: &Ownership,
    sink: &mut DiagSink,
    owner: Owner,
    span: Span,
    verb: &str,
    name: Option<StringId>,
) {
    let state = own.states.get(&owner);
    if matches!(state, Some(OwnerState::Moved { .. }))
        || (verb == "move" && matches!(state, Some(OwnerState::Borrowed)))
    {
        return;
    }
    let Some(projections) = own.live_projections.get(&owner) else {
        return;
    };
    if projections.is_empty() {
        return;
    }
    // Registration order is walk order, so projections[0] is a
    // deterministic choice for the note's span.
    let (note_span, note_msg) = match projections[0].inst_tirref(flow) {
        Some(vi) => match Ownership::dense_get(&own.view_last_use, vi) {
            Some(lu) => (tir.span(lu), "last slice use here"),
            None => (tir.span(vi), "slice created here"),
        },
        None => (span, "slice projection live here"),
    };
    sink.emit(
        Diag::error(
            span,
            DiagCode::SourceProjected,
            format!(
                "cannot {verb} {} while a slice of it is live",
                format_binding(name, pool)
            ),
        )
        .with_note(Some(note_span), note_msg)
        .with_help(
            "move or mutate the owner before slicing it, or keep all slice uses before this point",
        ),
    );
}

/// FieldAssign-target freeze: `p.f = v` frees the old buffer of field
/// `f` only, so it threatens exactly the live projections whose field
/// path lies at or below `target_path`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_field_target_projected(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    own: &Ownership,
    sink: &mut DiagSink,
    root: Owner,
    target_path: &[u32],
    span: Span,
    name: Option<StringId>,
) {
    if matches!(own.states.get(&root), Some(OwnerState::Moved { .. })) {
        return;
    }
    let Some(projections) = own.live_projections.get(&root) else {
        return;
    };
    let threatened: Vec<Owner> = projections
        .iter()
        .copied()
        .filter(|p| match own.projection_fields.get(p) {
            Some(path) => path.starts_with(target_path),
            None => true,
        })
        .collect();
    let Some(first) = threatened.first() else {
        return;
    };
    let (note_span, note_msg) = match first.inst_tirref(flow) {
        Some(vi) => match Ownership::dense_get(&own.view_last_use, vi) {
            Some(lu) => (tir.span(lu), "last slice use here"),
            None => (tir.span(vi), "slice created here"),
        },
        None => (span, "slice projection live here"),
    };
    sink.emit(
        Diag::error(
            span,
            DiagCode::SourceProjected,
            format!(
                "cannot mutate {} while a slice of it is live",
                format_binding(name, pool)
            ),
        )
        .with_note(Some(note_span), note_msg)
        .with_help(
            "reassign the field before slicing it, or keep all slice uses before this point",
        ),
    );
}

/// Pre-walk liveness for bound views (P4, final spec §3.2). See the
/// module header. `last_use` / `defer_to_loop` are dense
/// per-instruction tables sized to `tir.instructions.len()` (slot 0,
/// the reserved sentinel, stays empty) — the walk writes them into
/// `Ownership` wholesale.
pub(crate) struct ViewLiveness {
    /// Bound view instruction → its last reading instruction.
    pub(crate) last_use: Vec<Option<TirRef>>,
    /// View instruction → the loop at whose exit the projection dies.
    pub(crate) defer_to_loop: Vec<Option<TirRef>>,
    /// Per-arm view reads keyed by the arm's Flow IR entry block:
    /// `Present` arms override/kill during the per-arm refinement; a
    /// missing/`Unavailable` entry never refines.
    pub(crate) arm_last_reads: HashMap<BlockId, Analysis<HashMap<TirRef, TirRef>>>,
}

/// View-binding tracker for the liveness pre-walk: a `StringId → TirRef`
/// map plus an undo log, so branch arms and loop bodies restore the
/// pre-branch state by replaying the log instead of cloning the whole
/// map per arm.
type UndoEntry = (
    StringId,
    Option<HashSet<Option<TirRef>>>,
    Option<HashSet<Option<TirRef>>>,
);

#[derive(Default)]
pub(crate) struct Bindings {
    map: HashMap<StringId, HashSet<Option<TirRef>>>,
    log: Vec<UndoEntry>,
}

impl Bindings {
    fn insert_all(&mut self, k: StringId, values: HashSet<Option<TirRef>>) {
        let old = self.map.insert(k, values.clone());
        self.log.push((k, old, Some(values)));
    }

    fn mark(&self) -> usize {
        self.log.len()
    }

    /// Restore the map to `mark` and return the segment's write set.
    fn rollback(&mut self, mark: usize) -> HashMap<StringId, Option<HashSet<Option<TirRef>>>> {
        let mut writes: HashMap<StringId, Option<HashSet<Option<TirRef>>>> = HashMap::new();
        for (k, _, new) in &self.log[mark..] {
            writes.insert(*k, new.clone());
        }
        while self.log.len() > mark {
            let (k, old, _) = self.log.pop().expect("log.len() > mark");
            match old {
                Some(prev) => {
                    self.map.insert(k, prev);
                }
                None => {
                    self.map.remove(&k);
                }
            }
        }
        writes
    }
}

/// The liveness pre-pass driver: walks Flow blocks like the ownership
/// walk (open id-ordered driver + construct dispatch), recording view
/// reads and per-arm read maps. Runs before the forward walk.
pub(crate) fn collect_view_liveness(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    nesting: &LoopNesting,
) -> ViewLiveness {
    let mut lw = LivenessWalk {
        bindings: Bindings::default(),
        arm_last_reads: HashMap::new(),
        processed: vec![false; flow.blocks.len()],
        epoch: vec![0; flow.blocks.len()],
        epoch_next: 0,
        region_scratch: Vec::new(),
    };
    let mut last_use: HashMap<TirRef, TirRef> = HashMap::new();
    let epoch = lw.next_epoch();
    for b in flow.forward_order() {
        if lw.processed[b.0 as usize] {
            continue;
        }
        lw.walk_open(tir, flow, pool, b, epoch, &mut last_use);
    }
    let mut defer_to_loop = vec![None; tir.instructions.len()];
    for (view, read) in &last_use {
        let created_depth = nesting.depth_of(*view);
        let read_depth = nesting.depth_of(*read);
        // Scope rules guarantee the creation's nesting chain is a
        // prefix of the read's. A strictly deeper read re-executes on
        // later iterations of the first loop beyond the creation's
        // nesting — the enclosing loop whose own depth is
        // `created_depth`.
        if created_depth < read_depth {
            defer_to_loop[view.index()] = Some(nesting.ancestor_at_depth(*read, created_depth));
        }
    }
    let mut last_use_dense = vec![None; tir.instructions.len()];
    for (view, read) in last_use {
        last_use_dense[view.index()] = Some(read);
    }
    ViewLiveness {
        last_use: last_use_dense,
        defer_to_loop,
        arm_last_reads: lw.arm_last_reads,
    }
}

/// State for the liveness pre-pass (the walk's `Bindings` plus visit
/// marks). A separate lightweight driver from `Walk` — no lattice, no
/// diagnostics.
struct LivenessWalk {
    bindings: Bindings,
    arm_last_reads: HashMap<BlockId, Analysis<HashMap<TirRef, TirRef>>>,
    processed: Vec<bool>,
    epoch: Vec<u32>,
    epoch_next: u32,
    region_scratch: Vec<bool>,
}

impl LivenessWalk {
    fn next_epoch(&mut self) -> u32 {
        self.epoch_next += 1;
        self.epoch_next
    }

    /// Open-ended id-ordered walk (the driver): DFS preorder following
    /// continuations, like `Walk::walk_region`.
    fn walk_open(
        &mut self,
        tir: &Tir,
        flow: &Flow,
        pool: &InternPool,
        entry: BlockId,
        epoch: u32,
        reads: &mut HashMap<TirRef, TirRef>,
    ) {
        let mut stack = vec![entry];
        while let Some(b) = stack.pop() {
            if self.processed[b.0 as usize] || self.epoch[b.0 as usize] == epoch {
                continue;
            }
            self.epoch[b.0 as usize] = epoch;
            self.processed[b.0 as usize] = true;
            let conts = self.visit_block(tir, flow, pool, b, epoch, reads);
            for c in conts.into_iter().flatten().rev() {
                stack.push(c);
            }
        }
    }

    /// Bounded region walk: exactly `list`, continuations ignored
    /// (every reachable block is a list member).
    fn walk_list(
        &mut self,
        tir: &Tir,
        flow: &Flow,
        pool: &InternPool,
        list: &[BlockId],
        epoch: u32,
        reads: &mut HashMap<TirRef, TirRef>,
    ) {
        for &b in list {
            if self.epoch[b.0 as usize] == epoch {
                continue;
            }
            self.epoch[b.0 as usize] = epoch;
            self.processed[b.0 as usize] = true;
            let _ = self.visit_block(tir, flow, pool, b, epoch, reads);
        }
    }

    /// Steps + terminator dispatch for one block.
    fn visit_block(
        &mut self,
        tir: &Tir,
        flow: &Flow,
        pool: &InternPool,
        b: BlockId,
        epoch: u32,
        reads: &mut HashMap<TirRef, TirRef>,
    ) -> [Option<BlockId>; 2] {
        for step in flow.block_steps(b) {
            liveness_step(tir, flow, pool, step.inst, &mut self.bindings, reads);
        }
        match *flow.term(b) {
            Terminator::Goto { target } => [Some(target), None],
            Terminator::Branch { then_, else_, .. } => match flow.branch_if(b) {
                Some(if_inst) => [
                    liveness_if(self, tir, flow, pool, b, if_inst, epoch, reads),
                    None,
                ],
                None => [Some(then_), Some(else_)],
            },
            Terminator::Switch { match_inst, .. } => [
                liveness_match(self, tir, flow, pool, b, match_inst, epoch, reads),
                None,
            ],
            Terminator::While { .. } | Terminator::For { .. } => {
                [liveness_loop(self, tir, flow, pool, b, epoch, reads), None]
            }
            _ => [None, None],
        }
    }
}

/// One pre-pass step: record a view-typed `Var` read against the view
/// it aliases, then apply view-binding updates at declaration steps
/// (the initializer/value steps ran earlier — block iteration order).
fn liveness_step(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    r: TirRef,
    bindings: &mut Bindings,
    reads: &mut HashMap<TirRef, TirRef>,
) {
    let inst = *tir.inst(r);
    if inst.tag == TirTag::Var
        && pool.is_view(inst.ty)
        && let TirData::Var(name) = inst.data
        && let Some(view_insts) = bindings.map.get(&name)
    {
        // Latest DOCUMENT-order read wins — emission order, not visit
        // order: dead continuation blocks walk late but emitted in
        // place, and must not displace a live last use.
        let order = flow.order_of(r).unwrap_or(0);
        for &view_inst in view_insts.iter().flatten() {
            let later = match reads.get(&view_inst) {
                Some(&cur) => flow.order_of(cur).unwrap_or(0) < order,
                None => true,
            };
            if later {
                reads.insert(view_inst, r);
            }
        }
    }
    match inst.tag {
        TirTag::VarDecl => {
            let view = tir.var_decl_view(r);
            if pool.is_view(inst.ty)
                && let Some(targets) = view_binding_targets(tir, &bindings.map, view.initializer)
            {
                bindings.insert_all(view.name, targets);
            }
        }
        TirTag::Assign => {
            let view = tir.assign_view(r);
            if pool.is_view(inst.ty) {
                match view_binding_targets(tir, &bindings.map, view.value) {
                    Some(targets) => {
                        bindings.insert_all(view.name, targets);
                    }
                    None => {
                        bindings.insert_all(view.name, HashSet::from([None]));
                    }
                }
            }
        }
        _ => {}
    }
}

fn record_arm_reads(
    arm_last_reads: &mut HashMap<BlockId, Analysis<HashMap<TirRef, TirRef>>>,
    arm: BlockId,
    reads: HashMap<TirRef, TirRef>,
    flow: &Flow,
) {
    let current = arm_last_reads.remove(&arm);
    let mut merged = match current {
        Some(Analysis::Present(reads)) => reads,
        _ => HashMap::new(),
    };
    merge_reads(&mut merged, reads, flow);
    arm_last_reads.insert(arm, Analysis::Present(merged));
}

fn merge_reads(target: &mut HashMap<TirRef, TirRef>, reads: HashMap<TirRef, TirRef>, flow: &Flow) {
    for (view, read) in reads {
        let later = target.get(&view).is_none_or(|&current| {
            flow.order_of(current).unwrap_or(0) < flow.order_of(read).unwrap_or(0)
        });
        if later {
            target.insert(view, read);
        }
    }
}

fn union_binding_states(
    base: &HashMap<StringId, HashSet<Option<TirRef>>>,
    additional: &HashMap<StringId, HashSet<Option<TirRef>>>,
) -> HashMap<StringId, HashSet<Option<TirRef>>> {
    let mut merged = base.clone();
    for (&name, targets) in additional {
        merged.entry(name).or_default().extend(targets);
    }
    merged
}

/// If-cascade liveness: the mark/rollback arm discipline of the old
/// `view_liveness_stmt`, keyed by Flow IR arm blocks. Each arm's reads
/// become a `Present` entry keyed by the arm's entry block; the cond
/// chains between arms record against the enclosing scope.
#[allow(clippy::too_many_arguments)]
fn liveness_if(
    lw: &mut LivenessWalk,
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    fb: BlockId,
    if_inst: TirRef,
    epoch: u32,
    reads: &mut HashMap<TirRef, TirRef>,
) -> Option<BlockId> {
    let view = tir.if_stmt_view(if_inst);
    let arms = flow.branch_arm_blocks(if_inst);
    let join = arms.join;
    let mut arm_entries: Vec<BlockId> = Vec::with_capacity(2 + view.elif_branches.len());
    arm_entries.push(arms.then_block);
    for &holder in &arms.elif_blocks {
        let Terminator::Branch { then_, .. } = *flow.term(holder) else {
            unreachable!("recorded elif block holds a Branch")
        };
        arm_entries.push(then_);
    }
    arm_entries.push(arms.else_block);
    let arm_regions: Vec<Vec<BlockId>> = arm_entries
        .iter()
        .map(|&entry| {
            collect_region(flow, &mut lw.region_scratch, entry, |b| {
                super::RegionStop::At(join).kind(b)
            })
            .0
        })
        .collect();

    let mark = lw.bindings.mark();
    let mut arm_maps: Vec<HashMap<StringId, Option<HashSet<Option<TirRef>>>>> = Vec::new();
    // Then arm.
    let mut then_reads = HashMap::new();
    lw.walk_list(tir, flow, pool, &arm_regions[0], epoch, &mut then_reads);
    merge_reads(reads, then_reads.clone(), flow);
    record_arm_reads(&mut lw.arm_last_reads, arm_entries[0], then_reads, flow);
    arm_maps.push(lw.bindings.rollback(mark));
    // Elif arms: cond chain first (enclosing scope), then the body.
    let Terminator::Branch { else_, .. } = *flow.term(fb) else {
        unreachable!("cascade entry block holds a Branch")
    };
    let mut prev_else = else_;
    for e in 0..view.elif_branches.len() {
        let holder = arms.elif_blocks[e];
        let (chain, _) = collect_region(flow, &mut lw.region_scratch, prev_else, |b| {
            super::RegionStop::Through(holder).kind(b)
        });
        lw.walk_list(tir, flow, pool, &chain, epoch, reads);
        let mut body_reads = HashMap::new();
        lw.walk_list(tir, flow, pool, &arm_regions[1 + e], epoch, &mut body_reads);
        merge_reads(reads, body_reads.clone(), flow);
        record_arm_reads(&mut lw.arm_last_reads, arm_entries[1 + e], body_reads, flow);
        arm_maps.push(lw.bindings.rollback(mark));
        let Terminator::Branch { else_, .. } = *flow.term(holder) else {
            unreachable!("recorded elif block holds a Branch")
        };
        prev_else = else_;
    }
    // Else arm — walked uniformly; for an else-less if the region is
    // the empty synthetic fallthrough, whose (absent) reads simply fold.
    let last = arm_regions.len() - 1;
    let mut else_reads = HashMap::new();
    lw.walk_list(tir, flow, pool, &arm_regions[last], epoch, &mut else_reads);
    merge_reads(reads, else_reads.clone(), flow);
    if view.else_stmts.is_some() {
        record_arm_reads(&mut lw.arm_last_reads, arm_entries[last], else_reads, flow);
    }
    arm_maps.push(lw.bindings.rollback(mark));
    // Keep every possible incoming view binding. The enclosing state is
    // one possible path when an arm does not write the binding.
    let entry = lw.bindings.map.clone();
    let names: HashSet<StringId> = arm_maps.iter().flat_map(|m| m.keys().copied()).collect();
    for name in names {
        let mut values = HashSet::new();
        for arm in &arm_maps {
            match arm.get(&name) {
                Some(Some(targets)) => values.extend(targets.iter().copied()),
                Some(None) => {}
                None => values.extend(entry.get(&name).into_iter().flatten().copied()),
            }
        }
        if !values.is_empty() {
            lw.bindings.insert_all(name, values);
        }
    }
    Some(join)
}

/// Match liveness: the same mark/rollback arm discipline; the
/// scrutinee's steps already ran against the enclosing scope (like an
/// if condition). Statement and value positions are the same Switch.
#[allow(clippy::too_many_arguments)]
fn liveness_match(
    lw: &mut LivenessWalk,
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    sw: BlockId,
    match_inst: TirRef,
    epoch: u32,
    reads: &mut HashMap<TirRef, TirRef>,
) -> Option<BlockId> {
    let view = tir.match_view(match_inst);
    let Terminator::Switch {
        arms: rng, join, ..
    } = *flow.term(sw)
    else {
        unreachable!("match is a switch")
    };
    let arm_blocks: Vec<BlockId> = rng.as_range().map(|i| BlockId(flow.extra[i])).collect();
    let stop = match join {
        Some(j) => super::RegionStop::At(j),
        None => super::RegionStop::At(BlockId::NONE),
    };
    let mark = lw.bindings.mark();
    let mut arm_maps: Vec<HashMap<StringId, Option<HashSet<Option<TirRef>>>>> =
        Vec::with_capacity(arm_blocks.len());
    for (i, _arm) in view.arms().enumerate() {
        let (region, _) = collect_region(flow, &mut lw.region_scratch, arm_blocks[i], |b| {
            stop.kind(b)
        });
        let mut body_reads = HashMap::new();
        lw.walk_list(tir, flow, pool, &region, epoch, &mut body_reads);
        merge_reads(reads, body_reads.clone(), flow);
        record_arm_reads(&mut lw.arm_last_reads, arm_blocks[i], body_reads, flow);
        arm_maps.push(lw.bindings.rollback(mark));
    }
    let entry = lw.bindings.map.clone();
    let names: HashSet<StringId> = arm_maps.iter().flat_map(|m| m.keys().copied()).collect();
    for name in names {
        let mut values = HashSet::new();
        for arm in &arm_maps {
            match arm.get(&name) {
                Some(Some(targets)) => values.extend(targets.iter().copied()),
                Some(None) => {}
                None => values.extend(entry.get(&name).into_iter().flatten().copied()),
            }
        }
        if !values.is_empty() {
            lw.bindings.insert_all(name, values);
        }
    }
    join
}

/// Loop liveness: solve view bindings to a fixed point across the
/// back-edge. The union includes the entry state, which preserves the
/// zero-iteration path for reads after the loop.
fn liveness_loop(
    lw: &mut LivenessWalk,
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    ph: BlockId,
    _epoch: u32,
    reads: &mut HashMap<TirRef, TirRef>,
) -> Option<BlockId> {
    let (loop_inst, header) = match *flow.term(ph) {
        Terminator::While { loop_inst, header } | Terminator::For { loop_inst, header } => {
            (loop_inst, header)
        }
        _ => unreachable!("liveness_loop at a non-loop terminator"),
    };
    let test_block = super::loop_test_block(flow, loop_inst, header);
    let (body, exit) = match *flow.term(test_block) {
        Terminator::WhileTest { body, exit, .. } | Terminator::ForTest { body, exit, .. } => {
            (body, exit)
        }
        _ => unreachable!("loop test block holds the test"),
    };
    // Condition region (header + any complex-cond cascade, ending at
    // the test block).
    let (cond_region, _) = collect_region(flow, &mut lw.region_scratch, header, |b| {
        super::RegionStop::Through(test_block).kind(b)
    });
    let (region, _) = collect_region(flow, &mut lw.region_scratch, body, |b| {
        if b == exit || b == test_block {
            StopKind::Skip
        } else {
            StopKind::Visit
        }
    });
    let mark = lw.bindings.mark();
    let entry = lw.bindings.map.clone();
    let mut header_bindings = entry.clone();
    loop {
        lw.bindings.map.clone_from(&header_bindings);
        let pass_epoch = lw.next_epoch();
        lw.walk_list(tir, flow, pool, &cond_region, pass_epoch, reads);
        lw.walk_list(tir, flow, pool, &region, pass_epoch, reads);
        let body_exit = lw.bindings.map.clone();
        lw.bindings.rollback(mark);

        let next = union_binding_states(&entry, &body_exit);
        if next == header_bindings {
            header_bindings = next;
            break;
        }
        header_bindings = next;
    }
    // Post-loop code may see either the original binding (zero
    // iterations) or a value produced by the body. The fixed-point
    // header state contains both possibilities.
    for (name, targets) in header_bindings {
        if entry.get(&name) != Some(&targets) {
            lw.bindings.insert_all(name, targets);
        }
    }
    Some(exit)
}

/// The view instruction a `strview`-typed binding aliases: through
/// `Var` copies to its possible local slices (P3), or the initializer
/// itself. `None` inside the set represents non-local storage (e.g. a
/// `strview` parameter) and is ignored when recording local last uses.
pub(crate) fn view_binding_targets(
    tir: &Tir,
    bindings: &HashMap<StringId, HashSet<Option<TirRef>>>,
    init: TirRef,
) -> Option<HashSet<Option<TirRef>>> {
    match tir.inst(init).data {
        TirData::Var(name) => Some(
            bindings
                .get(&name)
                .cloned()
                .unwrap_or_else(|| HashSet::from([None])),
        ),
        TirData::Slice { .. } => Some(HashSet::from([Some(init)])),
        // A bound ToView (`u: strview = s`) projects the operand's
        // owner full-range; the conversion inst stands in as the view.
        _ if tir.inst(init).tag == TirTag::ToView => Some(HashSet::from([Some(init)])),
        _ => None,
    }
}

/// Per-arm freeze refinement (P2/P4, final spec §3.2): during an arm
/// walk, a view's last use for freeze purposes is its last read on the
/// path THROUGH this arm, not the global max over all arms. Applied
/// before an arm body walks; two cases:
///
/// * Override: a view read in this arm whose global last use lies in a
///   DIFFERENT arm of this construct (block membership: the global
///   last-use step's block is inside the construct's blocks but outside
///   this arm's) dies at its arm-local last read. The replaced entries
///   are returned for `restore_view_last_use`.
/// * Kill: a view whose every remaining read lies in OTHER arms is
///   already dead on this arm's path — its projection is removed for
///   the duration of the arm walk.
///
/// Both cases skip loop-deferred views (`view_defer_loop`); the
/// per-candidate deferral test re-applies the pre-pass's
/// `created_depth < read_depth` rule to the arm-local read. Neither
/// case applies when the global last use is OUTSIDE the construct's
/// blocks — a post-join read lies on every path and keeps the view live
/// in every arm.
///
/// The analysis is [`Analysis`] tri-state: a missing/`Unavailable`
/// per-arm entry refines nothing (conservative over-liveness; a killed
/// projection that hides a real freeze violation is the unrecoverable
/// direction).
pub(crate) fn refine_view_liveness_for_arm(
    own: &mut Ownership,
    flow: &Flow,
    branch_blocks: &HashSet<BlockId>,
    arm_blocks: &HashSet<BlockId>,
    analysis: Option<&Analysis<HashMap<TirRef, TirRef>>>,
) -> Vec<(TirRef, TirRef)> {
    // Missing analysis information must preserve a live projection.
    let Some(Analysis::Present(reads)) = analysis else {
        return Vec::new();
    };
    let actions: Vec<(TirRef, Option<TirRef>, TirRef)> = own
        .view_last_use
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let &global_lu = slot.as_ref()?;
            let vi = TirRef::from_raw(u32::try_from(i).expect("TIR arena index fits u32"));
            let lu_block = flow.step_block_of(global_lu)?;
            if !(branch_blocks.contains(&lu_block) && !arm_blocks.contains(&lu_block))
                || Ownership::dense_get(&own.view_defer_loop, vi).is_some()
            {
                return None;
            }
            // Known arm: no reads of this view in it → kill candidate
            // (None); an arm-local read → override candidate (Some).
            Some((vi, reads.get(&vi).copied(), global_lu))
        })
        .collect();
    let mut saved = Vec::new();
    for (vi, arm_lu, global_lu) in actions {
        match arm_lu {
            Some(lu) => {
                // P4 deferral, per candidate: `view_defer_loop` covers
                // only the global max read, so re-apply the pre-pass's
                // test to the arm-local read the override would install.
                if own.loop_nesting.depth_of(vi) < own.loop_nesting.depth_of(lu) {
                    continue;
                }
                saved.push((vi, global_lu));
                Ownership::dense_set(&mut own.view_last_use, vi, lu);
            }
            None => remove_projection(
                own,
                Owner(own.inst_place[vi.index()].expect("view owner place")),
            ),
        }
    }
    saved
}

/// Undo `refine_view_liveness_for_arm`'s overrides after an arm walk:
/// the walk-constant global max is back in place for the next arm and
/// for the join-time `prune_branch_dead_projections`.
pub(crate) fn restore_view_last_use(own: &mut Ownership, saved: Vec<(TirRef, TirRef)>) {
    for (vi, global_lu) in saved {
        Ownership::dense_set(&mut own.view_last_use, vi, global_lu);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nested mark/rollback invariant: an inner if's write set, merged
    /// back with a logged insert, must be picked up by the enclosing
    /// arm's rollback; an explicit unknown binding must also survive.
    #[test]
    fn bindings_nested_rollback_picks_up_inner_merge_writes() {
        let mut bindings = Bindings::default();
        let (a, b, c) = (
            StringId::from_raw(1),
            StringId::from_raw(2),
            StringId::from_raw(3),
        );
        let (va, vb, vc) = (
            TirRef::from_raw(10),
            TirRef::from_raw(11),
            TirRef::from_raw(12),
        );

        bindings.insert_all(a, HashSet::from([Some(va)]));
        let outer = bindings.mark();
        bindings.insert_all(b, HashSet::from([Some(vb)]));
        let inner = bindings.mark();
        // An inner if's logged merge write.
        if !bindings.map.contains_key(&c) {
            bindings.insert_all(c, HashSet::from([Some(vc)]));
        }
        let inner_writes = bindings.rollback(inner);
        assert_eq!(
            inner_writes,
            HashMap::from([(c, Some(HashSet::from([Some(vc)])))])
        );
        assert_eq!(
            bindings.map,
            HashMap::from([
                (a, HashSet::from([Some(va)])),
                (b, HashSet::from([Some(vb)]))
            ])
        );
        // Merge the inner write set the way the merge sites do.
        for (k, v) in inner_writes {
            if let Some(values) = v {
                bindings.insert_all(k, values);
            }
        }
        bindings.insert_all(b, HashSet::from([None]));
        let outer_writes = bindings.rollback(outer);
        assert_eq!(
            outer_writes,
            HashMap::from([
                (b, Some(HashSet::from([None]))),
                (c, Some(HashSet::from([Some(vc)])))
            ])
        );
        assert_eq!(
            bindings.map,
            HashMap::from([(a, HashSet::from([Some(va)]))])
        );
    }
}
