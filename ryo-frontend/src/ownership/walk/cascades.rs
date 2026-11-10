//! `if` / `elif` / `else` cascade analysis and the operand/step
//! helpers the arm walks share.

use super::super::{
    BranchState, Owner, OwnerState, Ownership, RegionStop, ReseatDrop, UNTRACKED_PLACE,
    check_use_moved, collect_region, drain_dying_views, flow_owner_place, needs_tracking,
    owner_sort_key, param_idx, prune_branch_dead_projections,
};
use super::Walk;
use ryo_core::diag::DiagSink;
use ryo_core::flow::{BlockId, DropOp, DropPosition, Flow, PlaceId, Terminator};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{Tir, TirData, TirRef, TirTag};
use ryo_core::types::InternPool;
use std::collections::HashSet;

/// The `check_use_moved` half of the old `recurse_operands`: operand
/// steps already ran (block iteration order), so only the parent's
/// use-safety checks remain. Mirrors the old recursion's shapes.
pub(super) fn check_operands_moved(
    tir: &Tir,
    pool: &InternPool,
    own: &Ownership,
    sink: &mut DiagSink,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    match inst.data {
        TirData::UnOp(o) => {
            if needs_tracking(tir.inst(o).ty, pool) {
                check_use_moved(tir, pool, own, sink, o, tir.span(o));
            }
        }
        TirData::BinOp { lhs, rhs } => {
            for op in [lhs, rhs] {
                if needs_tracking(tir.inst(op).ty, pool) {
                    check_use_moved(tir, pool, own, sink, op, tir.span(op));
                }
            }
        }
        TirData::Slice { base, start, end } => {
            if needs_tracking(tir.inst(base).ty, pool) {
                check_use_moved(tir, pool, own, sink, base, tir.span(base));
            }
            for bound in [start, end].into_iter().flatten() {
                if needs_tracking(tir.inst(bound).ty, pool) {
                    check_use_moved(tir, pool, own, sink, bound, tir.span(bound));
                }
            }
        }
        TirData::FieldAccess { object, .. } => {
            if needs_tracking(tir.inst(object).ty, pool) {
                check_use_moved(tir, pool, own, sink, object, tir.span(object));
            }
        }
        TirData::Extra(_)
        | TirData::None
        | TirData::Int(_)
        | TirData::Float(_)
        | TirData::Str(_)
        | TirData::Bool(_)
        | TirData::Var(_) => {}
    }
}

pub(crate) struct IfFrame {
    if_inst: TirRef,
    pub(crate) join: BlockId,
    pub(crate) parent_region: Option<usize>,
    pub(crate) region_exit: Option<BlockId>,
    arm_entries: Vec<BlockId>,
    arm_regions: Vec<Vec<BlockId>>,
    cond_chains: Vec<(BlockId, BlockId)>,
    cascade_set: HashSet<BlockId>,
    snap: BranchState,
    branch_results: Vec<BranchState>,
    falls_through: Vec<bool>,
    arm_lasts: Vec<Option<TirRef>>,
    else_present: bool,
    stage: IfStage,
    current_arm: Option<IfArm>,
    epoch: u32,
}

enum IfStage {
    Then,
    ElifChain(usize),
    ElifBody(usize),
    Else,
    Done,
}

struct IfArm {
    index: usize,
    saved: Vec<(TirRef, TirRef)>,
    saved_last: Option<TirRef>,
}

impl IfFrame {
    pub(crate) fn new(
        w: &mut Walk<'_>,
        own: &mut Ownership,
        _sink: &mut DiagSink,
        _sidecar: &mut FunctionSidecar,
        fb: BlockId,
        if_inst: TirRef,
        epoch: u32,
    ) -> (Self, BlockId) {
        let view = w.tir.if_stmt_view(if_inst);
        let arms = w.flow.branch_arm_blocks(if_inst);
        let join = arms.join;
        drain_dying_views(own);
        let mut arm_entries: Vec<BlockId> = Vec::with_capacity(2 + view.elif_branches.len());
        arm_entries.push(arms.then_block);
        for &holder in &arms.elif_blocks {
            let Terminator::Branch { then_, .. } = *w.flow.term(holder) else {
                unreachable!("recorded elif block holds a Branch")
            };
            arm_entries.push(then_);
        }
        arm_entries.push(arms.else_block);
        let mut cascade_blocks: Vec<BlockId> = vec![fb];
        let mut arm_regions: Vec<Vec<BlockId>> = Vec::with_capacity(arm_entries.len());
        for &entry in &arm_entries {
            let (region, _) = collect_region(w.flow, &mut w.region_scratch, entry, |b| {
                RegionStop::At(join).kind(b)
            });
            cascade_blocks.extend_from_slice(&region);
            arm_regions.push(region);
        }
        let mut cond_chains: Vec<(BlockId, BlockId)> = Vec::with_capacity(arms.elif_blocks.len());
        let Terminator::Branch { else_, .. } = *w.flow.term(fb) else {
            unreachable!("cascade entry block holds a Branch")
        };
        let mut prev_else = else_;
        for &holder in &arms.elif_blocks {
            let (chain, _) = collect_region(w.flow, &mut w.region_scratch, prev_else, |b| {
                RegionStop::Through(holder).kind(b)
            });
            cascade_blocks.extend_from_slice(&chain);
            cond_chains.push((prev_else, holder));
            let Terminator::Branch { else_, .. } = *w.flow.term(holder) else {
                unreachable!("recorded elif block holds a Branch")
            };
            prev_else = else_;
        }
        debug_assert_eq!(prev_else, arms.else_block, "cascade else chain");
        let cascade_set: HashSet<BlockId> = cascade_blocks.iter().copied().collect();
        let snap = own.snapshot_branch();
        (
            Self {
                if_inst,
                join,
                parent_region: None,
                region_exit: None,
                arm_entries,
                arm_regions,
                cond_chains,
                cascade_set,
                snap,
                branch_results: Vec::with_capacity(2 + view.elif_branches.len()),
                falls_through: Vec::with_capacity(2 + view.elif_branches.len()),
                arm_lasts: Vec::with_capacity(2 + view.elif_branches.len()),
                else_present: view.else_stmts.is_some(),
                stage: IfStage::Then,
                current_arm: None,
                epoch,
            },
            join,
        )
    }

    pub(crate) fn advance(
        &mut self,
        w: &mut Walk<'_>,
        own: &mut Ownership,
        _sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
    ) -> super::machine::WalkAdvance {
        if let Some(arm) = self.current_arm.take() {
            self.finish_arm(w, own, arm);
        }
        loop {
            match self.stage {
                IfStage::Then => {
                    self.stage = IfStage::ElifChain(0);
                    self.start_arm(w, own, 0, Some(self.arm_entries[0]));
                    return super::machine::WalkAdvance::seen(
                        self.arm_regions[0].clone(),
                        self.epoch,
                    );
                }
                IfStage::ElifChain(index) => {
                    if index == self.cond_chains.len() {
                        self.stage = IfStage::Else;
                        continue;
                    }
                    self.stage = IfStage::ElifBody(index);
                    let (entry, holder) = self.cond_chains[index];
                    return super::machine::WalkAdvance::Open {
                        entry,
                        stop: RegionStop::Through(holder),
                        epoch: self.epoch,
                    };
                }
                IfStage::ElifBody(index) => {
                    drain_dying_views(own);
                    self.stage = IfStage::ElifChain(index + 1);
                    let arm_index = index + 1;
                    self.start_arm(w, own, arm_index, Some(self.arm_entries[arm_index]));
                    return super::machine::WalkAdvance::seen(
                        self.arm_regions[arm_index].clone(),
                        self.epoch,
                    );
                }
                IfStage::Else => {
                    self.stage = IfStage::Done;
                    let index = self.arm_entries.len() - 1;
                    let key = self.else_present.then_some(self.arm_entries[index]);
                    self.start_arm(w, own, index, key);
                    return super::machine::WalkAdvance::seen(
                        self.arm_regions[index].clone(),
                        self.epoch,
                    );
                }
                IfStage::Done => {
                    self.finish(w, own, sidecar);
                    return super::machine::WalkAdvance::Done;
                }
            }
        }
    }

    fn start_arm(
        &mut self,
        w: &mut Walk<'_>,
        own: &mut Ownership,
        index: usize,
        refine_key: Option<BlockId>,
    ) {
        let saved = refine_key.map(|key| {
            let arm_set: HashSet<BlockId> = self.arm_regions[index].iter().copied().collect();
            let analysis = own.arm_last_reads.get(&key).cloned();
            super::super::refine_view_liveness_for_arm(
                own,
                w.flow,
                &self.cascade_set,
                &arm_set,
                analysis.as_ref(),
            )
        });
        let saved_last = w.last_inst;
        w.last_inst = None;
        self.current_arm = Some(IfArm {
            index,
            saved: saved.unwrap_or_default(),
            saved_last,
        });
    }

    fn finish_arm(&mut self, w: &mut Walk<'_>, own: &mut Ownership, arm: IfArm) {
        let arm_last = w.last_inst;
        w.last_inst = arm.saved_last;
        super::super::restore_view_last_use(own, arm.saved);
        if self.region_exit.is_none() {
            self.region_exit = self.arm_regions[arm.index].iter().find_map(|&block| {
                matches!(
                    w.flow.term(block),
                    Terminator::Return { .. }
                        | Terminator::ReturnVoid
                        | Terminator::Break { .. }
                        | Terminator::Continue { .. }
                )
                .then_some(block)
            });
        }
        self.branch_results.push(own.take_branch(self.snap.clone()));
        self.falls_through
            .push(arm_reaches_join(w, &self.arm_regions[arm.index], self.join));
        self.arm_lasts.push(arm_last);
    }

    fn finish(&mut self, w: &mut Walk<'_>, own: &mut Ownership, sidecar: &mut FunctionSidecar) {
        let arms = w.flow.branch_arm_blocks(self.if_inst);
        let is_tracked = |own: &Ownership, owner: Owner| match owner.inst_tirref(w.flow) {
            Some(_) => true,
            None => {
                let idx = param_idx(&own.param_index, w.flow.place(owner.0).name());
                needs_tracking(w.tir.params[idx].ty, w.pool)
            }
        };
        let span_of = |_: &Ownership, owner: Owner| owner.span(w.flow, w.tir);
        let mut all_keys: HashSet<Owner> = self
            .branch_results
            .iter()
            .flat_map(|s| s.states.keys().copied())
            .collect();
        let mut all_keys: Vec<Owner> = all_keys.drain().collect();
        all_keys.sort_by_key(owner_sort_key);
        for owner in all_keys {
            if !is_tracked(own, owner) {
                continue;
            }
            let any_moved = self
                .branch_results
                .iter()
                .zip(&self.falls_through)
                .any(|(s, &ft)| {
                    ft && matches!(s.states.get(&owner), Some(OwnerState::Moved { .. }))
                });
            if !any_moved {
                continue;
            }
            for (i, s) in self.branch_results.iter().enumerate() {
                if !self.falls_through[i] {
                    continue;
                }
                if matches!(s.states.get(&owner), Some(OwnerState::Valid))
                    && let Some(after) = self.arm_lasts[i]
                {
                    sidecar.drops.push(DropOp {
                        position: DropPosition::AfterStep { inst: after },
                        target: owner.tirref(w.flow),
                        span: span_of(own, owner),
                    });
                }
            }
        }
        let mut arm_states: Vec<(BlockId, &BranchState, &[BlockId])> =
            Vec::with_capacity(self.branch_results.len());
        for (i, state) in self.branch_results.iter().enumerate() {
            let key = if i == 0 {
                arms.then_block
            } else if i <= arms.elif_blocks.len() {
                arms.elif_blocks[i - 1]
            } else {
                arms.else_block
            };
            arm_states.push((key, state, &self.arm_regions[i]));
        }
        for (home, owner_pre) in &self.snap.current_owner {
            let home_ty = match w.flow.place(*home) {
                ryo_core::flow::Place::Local { ty, .. } => *ty,
                ryo_core::flow::Place::Param { ty, .. } => *ty,
                _ => continue,
            };
            if !needs_tracking(home_ty, w.pool) {
                continue;
            }
            let name = w.flow.place(*home).name();
            let pre_binding = self.snap.binding_of_name.get(&name).copied();
            let mut reseat_owners: HashSet<Owner> = HashSet::new();
            let mut untouched_arms: Vec<BlockId> = Vec::new();
            for (block, b, region) in &arm_states {
                let owner_b = b.current_owner.get(home).copied().unwrap_or(*owner_pre);
                let binding_b = b.binding_of_name.get(&name).copied().or(pre_binding);
                if binding_b == pre_binding {
                    if owner_b == *owner_pre {
                        untouched_arms.push(*block);
                    } else {
                        reseat_owners.insert(owner_b);
                    }
                } else if let Some(binding) = pre_binding {
                    collect_arm_reseats(
                        w.tir,
                        w.pool,
                        sidecar,
                        w.flow,
                        region,
                        binding,
                        &mut reseat_owners,
                    );
                }
            }
            if !reseat_owners.is_empty()
                && !untouched_arms.is_empty()
                && !own
                    .reseat_drops
                    .iter()
                    .any(|d| d.if_stmt == self.if_inst && d.name == name)
            {
                own.reseat_drops.push(ReseatDrop {
                    if_stmt: self.if_inst,
                    name,
                    pre_owner: *owner_pre,
                    reseat_owners,
                    untouched_arms,
                });
            }
        }
        own.merge_branches(
            std::mem::take(&mut self.branch_results),
            &self.falls_through,
        );
        prune_branch_dead_projections(own, w.flow, &self.cascade_set);
        w.last_inst = Some(self.if_inst);
    }
}

/// Collect every `Assign` value in the arm's blocks (including nested
/// conditionals) that stores into `binding` — the pre-branch binding a
/// shadowing arm took the name from.
pub(crate) fn collect_arm_reseats(
    tir: &Tir,
    pool: &InternPool,
    sidecar: &FunctionSidecar,
    flow: &Flow,
    region: &[BlockId],
    binding: TirRef,
    reseat_owners: &mut HashSet<Owner>,
) {
    for &b in region {
        for step in flow.block_steps(b) {
            if tir.inst(step.inst).tag != TirTag::Assign {
                continue;
            }
            let av = tir.assign_view(step.inst);
            if sidecar.assign_binding[step.inst.index()] == Some(binding)
                && needs_tracking(tir.inst(av.value).ty, pool)
                && let Some(p) = flow_owner_place(flow, av.value)
            {
                reseat_owners.insert(Owner(p));
            }
        }
    }
}

/// Seat the value `init` resolves to as the current owner of the
/// binding home place (the `VarDecl`/`Assign` step's write fact).
pub(crate) fn rebind_to_init(own: &mut Ownership, home: PlaceId, init: TirRef) -> Owner {
    let owner = match own.inst_place[init.index()] {
        Some(p) => Owner(p),
        None => Owner(UNTRACKED_PLACE),
    };
    if owner.0 != UNTRACKED_PLACE {
        own.states.insert(owner, OwnerState::Valid);
    }
    own.current_owner.insert(home, owner);
    owner
}

/// True if `owner` is the value currently seated in an `inout str`
/// parameter's home place (escapes through the write-back pointer at
/// function exit).
pub(crate) fn inout_escape_owner(own: &Ownership, owner: Owner) -> bool {
    own.inout_str_params.iter().any(|n| {
        let idx = own.param_index[n];
        own.current_owner.get(&own.param_places[idx]) == Some(&owner)
    })
}

/// True when the walked arm region's exit reaches `join`.
pub(crate) fn arm_reaches_join(w: &Walk<'_>, region: &[BlockId], join: BlockId) -> bool {
    region.iter().any(|&b| {
        matches!(
            w.flow.term(b),
            Terminator::Goto { target } if *target == join
        )
    })
}
