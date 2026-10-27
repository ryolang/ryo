//! `if` / `elif` / `else` cascade analysis and the operand/step
//! helpers the arm walks share: the CFG-join cascade driver
//! (`analyze_if_cascade` — snapshot, per-arm walks, any-Moved-wins
//! merge, arm-conditional Frees, join-time projection prune), the
//! arm-reseat collector, the operand use-safety checks
//! (`check_operands_moved`), and the small binding helpers the VarDecl
//! /Assign steps seat values through (`rebind_to_init`,
//! `register_pending_dead_store`, `inout_escape_owner`).

use super::super::{
    BranchState, Owner, OwnerState, Ownership, RegionStop, ReseatDrop, UNTRACKED_PLACE,
    check_use_moved, collect_region, drain_dying_views, flow_owner_place, needs_tracking,
    owner_sort_key, param_idx, prune_branch_dead_projections, walk_if_arm,
};
use super::Walk;
use ryo_core::diag::DiagSink;
use ryo_core::flow::{BlockId, DropOp, DropPosition, Flow, PlaceId, Terminator};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{Span, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
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
            // M8.4: slicing is a non-consuming read of the base.
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
            // M9: a field access reads its object (non-consuming).
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

/// CFG join for `if` / `elif` / `else`, driven by the recorded Flow IR
/// arm blocks. Semantics identical to the old statement-recursive
/// `analyze_if_stmt`: snapshot, walk each arm from the snapshot, merge
/// (any-Moved-wins over arms that reach the join), schedule
/// arm-conditional Frees for owners that diverge across arms, record
/// conditional reseats, prune projections whose last use is inside the
/// cascade. Block membership replaces the old TIR subtree sets; the
/// else-less fallthrough is the (empty) recorded else block, walked
/// uniformly. Returns the cascade's join block (continuation).
#[allow(clippy::too_many_arguments)]
pub(super) fn analyze_if_cascade(
    w: &mut Walk<'_>,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    fb: BlockId,
    if_inst: TirRef,
    epoch: u32,
) -> Option<BlockId> {
    let tir = w.tir;
    let view = tir.if_stmt_view(if_inst);
    let arms = w.flow.branch_arm_blocks(if_inst);
    let join = arms.join;
    // P4 lift (final spec §3.2): a projection whose last use is the
    // condition is dead before any arm runs — drain now so every arm
    // starts from the same freeze state. (The condition's steps ran in
    // `fb` before this dispatch.)
    drain_dying_views(own);

    // Arm body entries: then body, each elif's `then_` successor, the
    // else/fallthrough block (empty for an else-less if).
    let mut arm_entries: Vec<BlockId> = Vec::with_capacity(2 + view.elif_branches.len());
    arm_entries.push(arms.then_block);
    for &holder in &arms.elif_blocks {
        let Terminator::Branch { then_, .. } = *w.flow.term(holder) else {
            unreachable!("recorded elif block holds a Branch")
        };
        arm_entries.push(then_);
    }
    arm_entries.push(arms.else_block);

    // The full cascade block set (cond chains + arm regions) precomputed
    // for the per-arm freeze refinement and the join-time prune — the
    // Flow IR replacement for the old `collect_reachable` subtree sets.
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
    let mut branch_results: Vec<BranchState> = Vec::with_capacity(arm_entries.len());
    let mut falls_through: Vec<bool> = Vec::with_capacity(arm_entries.len());
    let mut arm_lasts: Vec<Option<TirRef>> = Vec::with_capacity(arm_entries.len());

    // Then arm (index 0).
    let (state, falls, last) = walk_if_arm(
        w,
        own,
        sink,
        sidecar,
        &arm_regions,
        &cascade_set,
        &snap,
        0,
        Some(arms.then_block),
        join,
        epoch,
    );
    branch_results.push(state);
    falls_through.push(falls);
    arm_lasts.push(last);

    // Elif arms: the cond chain walks first (with the snapshot state,
    // like the old visit_expr(elif.cond)), then the body.
    for e in 0..view.elif_branches.len() {
        let (chain_entry, holder) = cond_chains[e];
        w.walk_region(
            own,
            sink,
            sidecar,
            chain_entry,
            RegionStop::Through(holder),
            epoch,
        );
        drain_dying_views(own);
        let body = arm_entries[1 + e];
        let (state, falls, last) = walk_if_arm(
            w,
            own,
            sink,
            sidecar,
            &arm_regions,
            &cascade_set,
            &snap,
            1 + e,
            Some(body),
            join,
            epoch,
        );
        branch_results.push(state);
        falls_through.push(falls);
        arm_lasts.push(last);
    }

    // Else arm — uniform walk; for an else-less if the region is the
    // empty synthetic fallthrough block, which contributes the
    // pre-branch state and reaches the join (the old pseudo-arm).
    let else_key = if view.else_stmts.is_some() {
        Some(arms.else_block)
    } else {
        None
    };
    let last_index = arm_entries.len() - 1;
    let (state, falls, last) = walk_if_arm(
        w,
        own,
        sink,
        sidecar,
        &arm_regions,
        &cascade_set,
        &snap,
        last_index,
        else_key,
        join,
        epoch,
    );
    branch_results.push(state);
    falls_through.push(falls);
    arm_lasts.push(last);

    // Schedule arm-conditional Frees for owners that diverge across
    // arms (Valid in some, Moved in others): for each Valid arm, anchor
    // a Free after that arm's last instruction — the anchor's block
    // membership gates it to the arm's path. The else-less fallthrough
    // pseudo-arm deliberately schedules nothing (the post-merge
    // last-use pass covers owners still Valid at the join).
    {
        let is_tracked = |own: &Ownership, owner: Owner| match owner.inst_tirref(w.flow) {
            Some(_) => true,
            None => {
                let idx = param_idx(&own.param_index, w.flow.place(owner.0).name());
                needs_tracking(w.tir.params[idx].ty, w.pool)
            }
        };
        let span_of = |_: &Ownership, owner: Owner| owner.span(w.flow, w.tir);
        let mut all_keys: HashSet<Owner> = branch_results
            .iter()
            .flat_map(|s| s.states.keys().copied())
            .collect();
        let mut all_keys: Vec<Owner> = all_keys.drain().collect();
        all_keys.sort_by_key(owner_sort_key);
        for owner in all_keys {
            if !is_tracked(own, owner) {
                continue;
            }
            let any_moved = branch_results.iter().zip(&falls_through).any(|(s, &ft)| {
                ft && matches!(s.states.get(&owner), Some(OwnerState::Moved { .. }))
            });
            if !any_moved {
                continue;
            }
            for (i, s) in branch_results.iter().enumerate() {
                if !falls_through[i] {
                    continue;
                }
                if matches!(s.states.get(&owner), Some(OwnerState::Valid))
                    && let Some(after) = arm_lasts[i]
                {
                    sidecar.drops.push(DropOp {
                        position: DropPosition::AfterStep { inst: after },
                        target: owner.tirref(w.flow),
                        span: span_of(own, owner),
                    });
                }
            }
        }
    }

    // Record conditional reseats — bindings that SOME arm reseated
    // while other arms kept the pre-if owner. The dead-store drain
    // converts a record into block-keyed `ConditionalDeadDrop`s when
    // the reassigned value is never read afterwards.
    {
        let mut arm_states: Vec<(BlockId, &BranchState, &[BlockId])> =
            Vec::with_capacity(branch_results.len());
        for (i, state) in branch_results.iter().enumerate() {
            // The reseat record keys: then body block, the elif
            // BRANCH-HOLDER blocks (codegen fires dead drops when it
            // enters the arm), and the else/fallthrough block.
            let key = if i == 0 {
                arms.then_block
            } else if i <= arms.elif_blocks.len() {
                arms.elif_blocks[i - 1]
            } else {
                arms.else_block
            };
            arm_states.push((key, state, &arm_regions[i]));
        }
        for (home, owner_pre) in &snap.current_owner {
            // Only tracked (Move-typed) locals need drops; Copy values
            // have no buffer, and params are covered by the exit-time
            // param free. The home's own type decides (params'
            // home places are their own).
            let home_ty = match w.flow.place(*home) {
                ryo_core::flow::Place::Local { ty, .. } => *ty,
                ryo_core::flow::Place::Param { ty, .. } => *ty,
                _ => continue,
            };
            if !needs_tracking(home_ty, w.pool) {
                continue;
            }
            let name = w.flow.place(*home).name();
            let pre_binding = snap.binding_of_name.get(&name).copied();
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
                } else {
                    // The arm shadows `name` with an arm-local binding
                    // (a different home place). An earlier reseat of
                    // the PRE-branch binding may hide anywhere in the
                    // arm (including nested conditionals): scan the
                    // arm's blocks for Assigns attributed to the
                    // pre-branch binding.
                    if let Some(binding) = pre_binding {
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
            }
            // Dedup: loop-convergence re-walks revisit this if.
            if !reseat_owners.is_empty()
                && !untouched_arms.is_empty()
                && !own
                    .reseat_drops
                    .iter()
                    .any(|d| d.if_stmt == if_inst && d.name == name)
            {
                own.reseat_drops.push(ReseatDrop {
                    if_stmt: if_inst,
                    name,
                    pre_owner: *owner_pre,
                    reseat_owners,
                    untouched_arms,
                });
            }
        }
    }

    // `own` already holds the pre-if non-monotone fields (installed by
    // the last take_branch) — merge every arm's end state into them.
    own.merge_branches(branch_results, &falls_through);
    // P4 (final spec §3.2): a view whose last use is inside this if is
    // dead at the join on every path — prune it from the merged freeze
    // ranges.
    prune_branch_dead_projections(own, w.flow, &cascade_set);
    w.last_inst = Some(if_inst);
    Some(join)
}

/// Collect every `Assign` value in the arm's blocks (including nested
/// conditionals) that stores into `binding` — the pre-branch binding a
/// shadowing arm took the name from. Flow IR replacement for the old
/// statement-recursive `collect_arm_reseats`: block membership covers
/// exactly the arm's reachable blocks, and `sidecar.assign_binding`
/// (stamped when each Assign step ran) attributes the reseat.
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
/// binding home place (the `VarDecl`/`Assign` step's write fact). The
/// seat is the RESOLVED owner (a fresh temp for producers, the source
/// binding's seated owner for `x = y`), never the home itself — free
/// targets derived from it must map through the binding tables to the
/// right home slot. After a consume, the source owner is `Moved`; the
/// re-validated seat gives subsequent reads a `Valid` owner instead of
/// tripping over the just-moved value.
pub(crate) fn rebind_to_init(own: &mut Ownership, home: PlaceId, init: TirRef) -> Owner {
    // The seat is the initializer EXPRESSION's own place: fresh for
    // every rebind (a `Var` init seats the read's place — a slot
    // distinct from the source binding's seated owner, so the source's
    // `Moved` state is never resurrected), and value-identity for the
    // free targets derived from it. A place-less initializer
    // (error-shaped TIR) seats the shared untracked owner.
    let owner = match own.inst_place[init.index()] {
        Some(p) => Owner(p),
        None => Owner(UNTRACKED_PLACE),
    };
    // The untracked sentinel never enters the lattice (nothing owns
    // it; stamping it Valid would put it in the exit-time passes).
    if owner.0 != UNTRACKED_PLACE {
        own.states.insert(owner, OwnerState::Valid);
    }
    own.current_owner.insert(home, owner);
    owner
}

/// Register a binding into `pending_dead_store`. Owned values key by
/// their current owner; non-owning values key by binding home.
/// `decl_inst` serves as the Free anchor when a dead store owns memory.
pub(crate) fn register_pending_dead_store(
    own: &mut Ownership,
    owner: Owner,
    name: StringId,
    span: Span,
    decl_inst: TirRef,
) {
    own.pending_dead_store
        .insert(owner, (name, span, decl_inst));
}

/// True if `owner` is the value currently seated in an `inout str`
/// parameter's home place (escapes through the write-back pointer at
/// exit).
pub(crate) fn inout_escape_owner(own: &Ownership, owner: Owner) -> bool {
    own.inout_str_params.iter().any(|n| {
        let idx = own.param_index[n];
        own.current_owner.get(&own.param_places[idx]) == Some(&owner)
    })
}
