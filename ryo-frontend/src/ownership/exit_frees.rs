//! Return-epilogue and exit-path free scheduling: the frees that fire
//! at early `return`s (anchored at the return statement — codegen emits
//! due Frees before every `return_`), plus the exit-free post-processing
//! passes — honored-reseat conversion, covered-later suppression,
//! displaced-exit-free retargeting, redundancy suppression, and the
//! exit-group dedup. Runs LAST in `analyze_function`, so every other
//! Free pass has populated the `drops` schedule and these passes can
//! dedup against it.

use super::{Owner, Ownership, TirStmtList, ancestor_branches_of, anchored_drops, drop_anchor};
use ryo_core::flow::fire_points::{FirePoints, FireVerdict};
use ryo_core::flow::{BlockId, DropOp, DropPosition, Flow};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::tir::{Tir, TirRef, TirTag};
use std::collections::{HashMap, HashSet};
/// Return epilogue: destroy locals still live at an early return.
/// Runs LAST so every other Free pass has populated the `drops`
/// schedule and we can dedup against it — a value is skipped when another
/// Free already fires on the return's path, or the dead-store drain
/// owns it (its after-decl Free covers every path). Codegen emits
/// due Frees before every `return_`, so anchoring at the return
/// statement itself fires exactly on that exit path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn schedule_return_epilogue_frees<S: TirStmtList + ?Sized>(
    tir: &Tir,
    flow: &Flow,
    own: &Ownership,
    sidecar: &mut FunctionSidecar,
    body_stmts: &S,
    order: &[u32],
    decl_of_init: &HashMap<TirRef, TirRef>,
    assign_value_of: &HashMap<TirRef, TirRef>,
    reassign_orders: &HashMap<TirRef, Vec<TirRef>>,
) {
    let mut epilogue_emitted: HashSet<(TirRef, TirRef)> = HashSet::new();
    let fire_points = FirePoints::new(flow);
    for (return_stmt, owners) in &own.return_epilogue {
        let path = ReturnPath::new(
            tir,
            flow,
            own,
            body_stmts,
            *return_stmt,
            decl_of_init,
            assign_value_of,
        );
        // Epilogue Frees decided for THIS return, staged before they
        // join the `drops` schedule. The binding-covering scan must see
        // them: two owners of the SAME binding live at one return all
        // lower through the home-slot redirect, so the first decision
        // covers the rest — but only if the scan can observe it.
        let mut staged: Vec<DropOp> = Vec::new();
        for owner in owners {
            let r = owner.tirref(flow);
            if !r.is_param() && own.pending_dead_store.contains_key(owner) {
                continue;
            }
            // A return that moves a binding's value hands the home
            // slot's content to the caller (sret / register copy). The
            // consume record names the owner the read resolved to —
            // under a first-wins loop/branch merge that is the
            // pre-reassign owner, while the buffer physically leaving
            // is a DIFFERENT, still-`Valid` owner of the same binding
            // (the in-loop reseated value). Destroying either here
            // frees the returned buffer: skip every owner of a binding
            // this return consumes.
            if path
                .binding_of(r)
                .is_some_and(|b| path.consumed_bindings.contains(&b))
            {
                continue;
            }
            if !epilogue_emitted.insert((*return_stmt, r)) {
                continue;
            }
            // A covering Free must fire on a path that REACHES this
            // return, at an anchor that CAN fire. A terminator jump
            // inside a traversed subtree (`return`/`break`/`continue`)
            // only services its own exit. A last-use drop inside this
            // return's operand fires only when its Flow point is before
            // the Return block, as for a match arm; a drop in the Return
            // block cannot sweep after the terminator.
            let covered = return_has_direct_covering_drop(
                tir,
                flow,
                sidecar,
                &fire_points,
                &path,
                *return_stmt,
                r,
            );
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
            let superseded = return_owner_was_superseded(
                tir,
                own,
                &path,
                order,
                *return_stmt,
                r,
                reassign_orders,
            );
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
            if let Some(binding) = path.binding_of(r) {
                // Covered only when the covering Free anchors after
                // EVERY reassign of the binding — otherwise the return
                // may hold a value the covering Free did not release.
                let covers = |fp: &DropOp| {
                    path.binding_of(fp.target) == Some(binding)
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
                    path.on_path.contains(&drop_anchor(fp))
                        && !path.ancestors.contains(&drop_anchor(fp))
                        && drop_fires_on_return_path(
                            tir,
                            flow,
                            &fire_points,
                            &path,
                            *return_stmt,
                            fp,
                        )
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

/// Facts used to decide whether an existing Free covers one return path.
struct ReturnPath<'a> {
    on_path: HashSet<TirRef>,
    ancestors: HashSet<TirRef>,
    consumed_bindings: HashSet<TirRef>,
    return_block: Option<BlockId>,
    decl_of_init: &'a HashMap<TirRef, TirRef>,
    assign_value_of: &'a HashMap<TirRef, TirRef>,
}

impl<'a> ReturnPath<'a> {
    fn new<S: TirStmtList + ?Sized>(
        tir: &Tir,
        flow: &Flow,
        own: &Ownership,
        body_stmts: &S,
        return_stmt: TirRef,
        decl_of_init: &'a HashMap<TirRef, TirRef>,
        assign_value_of: &'a HashMap<TirRef, TirRef>,
    ) -> Self {
        let mut on_path = HashSet::new();
        let _ = tir.collect_jump_path_iter(body_stmts.iter_refs(), return_stmt, &mut on_path);
        // Collect the return expression too: value-match arms execute
        // before control reaches the return block.
        tir.collect_reachable(return_stmt, &mut on_path);
        let ancestors = ancestor_branches_of(tir, return_stmt).into_iter().collect();
        let binding_of = |t: TirRef| decl_of_init.get(&t).or(assign_value_of.get(&t)).copied();
        let consumed_bindings = own
            .owner_hazards
            .iter()
            .filter(|&&(_, site, _)| site == return_stmt)
            .filter_map(|&(owner, _, _)| binding_of(owner.tirref(flow)))
            .collect();
        Self {
            on_path,
            ancestors,
            consumed_bindings,
            return_block: flow.term_block_of(return_stmt),
            decl_of_init,
            assign_value_of,
        }
    }

    fn binding_of(&self, value: TirRef) -> Option<TirRef> {
        self.decl_of_init
            .get(&value)
            .or(self.assign_value_of.get(&value))
            .copied()
    }
}

fn return_block_reachable(flow: &Flow, start: BlockId, target: BlockId) -> bool {
    let mut seen = HashSet::new();
    let mut pending = vec![start];
    while let Some(block) = pending.pop() {
        if block == target {
            return true;
        }
        if seen.insert(block) {
            pending.extend_from_slice(flow.successors(block));
        }
    }
    false
}

fn drop_precedes_return(
    flow: &Flow,
    fire_points: &FirePoints,
    drop: &DropOp,
    return_block: BlockId,
) -> bool {
    let reaches_return = |point: ryo_core::flow::fire_points::FirePoint| {
        point.block != return_block && return_block_reachable(flow, point.block, return_block)
    };
    match fire_points.resolve(drop.position) {
        FireVerdict::Resolved(point) | FireVerdict::NonExact(point) => reaches_return(point),
        FireVerdict::ResolvedPerArm(points) => points.into_iter().any(reaches_return),
        FireVerdict::Unresolved => false,
    }
}

fn drop_fires_on_return_path(
    tir: &Tir,
    flow: &Flow,
    fire_points: &FirePoints,
    path: &ReturnPath<'_>,
    return_stmt: TirRef,
    drop: &DropOp,
) -> bool {
    let anchor = drop_anchor(drop);
    if matches!(
        tir.inst(anchor).tag,
        TirTag::Return | TirTag::ReturnVoid | TirTag::Break | TirTag::Continue
    ) {
        return false;
    }
    if !tir.contains_reachable(return_stmt, anchor) {
        return true;
    }
    path.return_block
        .is_some_and(|return_block| drop_precedes_return(flow, fire_points, drop, return_block))
}

fn return_has_direct_covering_drop(
    tir: &Tir,
    flow: &Flow,
    sidecar: &FunctionSidecar,
    fire_points: &FirePoints,
    path: &ReturnPath<'_>,
    return_stmt: TirRef,
    owner: TirRef,
) -> bool {
    anchored_drops(sidecar).any(|drop| {
        let anchor = drop_anchor(drop);
        let match_payload_drop = !owner.is_param()
            && tir.inst(owner).tag == TirTag::FieldAccess
            && path.return_block.is_some_and(|return_block| {
                drop_precedes_return(flow, fire_points, drop, return_block)
            });
        drop.target == owner
            && (path.on_path.contains(&anchor) || match_payload_drop)
            && !path.ancestors.contains(&anchor)
            && drop_fires_on_return_path(tir, flow, fire_points, path, return_stmt, drop)
    })
}

fn return_owner_was_superseded(
    tir: &Tir,
    own: &Ownership,
    path: &ReturnPath<'_>,
    order: &[u32],
    return_stmt: TirRef,
    owner: TirRef,
    reassign_orders: &HashMap<TirRef, Vec<TirRef>>,
) -> bool {
    let return_loops: Vec<TirRef> = own
        .loop_nesting
        .ancestors_innermost_first(return_stmt)
        .collect();
    let certainly_executed = |assign: TirRef| {
        path.on_path.contains(&assign)
            && !own
                .loop_nesting
                .ancestors_innermost_first(assign)
                .any(|loop_ref| !return_loops.contains(&loop_ref))
    };
    path.binding_of(owner).is_some_and(|binding| {
        reassign_orders.get(&binding).is_some_and(|assigns| {
            assigns.iter().any(|&assign| {
                order[assign.index()] < order[return_stmt.index()]
                    && certainly_executed(assign)
                    && tir.assign_view(assign).value != owner
            })
        })
    })
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
pub(crate) fn binding_value_maps(
    tir: &Tir,
    sidecar: &FunctionSidecar,
    order: &[u32],
) -> BindingValueMaps {
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
pub(crate) fn convert_honored_reseat_drops(
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

fn suppress_exit_frees_covered_later(
    tir: &Tir,
    order: &[u32],
    sidecar: &FunctionSidecar,
    candidates: &[(usize, TirRef, TirRef)],
    binding_of: &impl Fn(TirRef) -> Option<TirRef>,
    drop: &mut HashSet<usize>,
) {
    // An exit Free covered by a same-binding Free anchored strictly
    // after the exit is redundant on every path. The covering anchor
    // must lie outside the exit's subtree: an anchor inside it fires
    // only on paths that reach it, so it cannot cover skipped arms or
    // zero-iteration loops.
    for &(i, after, binding) in candidates {
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
}

fn retarget_displaced_exit_frees(
    tir: &Tir,
    order: &[u32],
    sidecar: &mut FunctionSidecar,
    decl_of_init: &HashMap<TirRef, TirRef>,
    assign_value_of: &HashMap<TirRef, TirRef>,
    reassign_orders: &HashMap<TirRef, Vec<TirRef>>,
    drop: &mut HashSet<usize>,
) {
    // Pass 1b: an exit Free whose target was already released by a
    // reassign displacement before the exit must target the binding's
    // last write instead. This makes codegen release the slot's current
    // content without freeing the displaced buffer twice.
    let binding_of = |t: TirRef| decl_of_init.get(&t).or(assign_value_of.get(&t)).copied();
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
    let displaced_targets: HashSet<TirRef> =
        sidecar.free_on_reassign.iter().flatten().copied().collect();
    for i in 0..sidecar.drops.len() {
        if drop.contains(&i) {
            continue;
        }
        let fp = &sidecar.drops[i];
        // Block-start drops fire at block starts, not instruction anchors.
        let Some(anchor) = fp.position.anchor_inst() else {
            continue;
        };
        // This scan also covers last-use Frees re-anchored to a branch.
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
        // A same-binding Free at this anchor already covers these paths.
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
pub(crate) fn suppress_redundant_exit_frees(
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
    suppress_exit_frees_covered_later(tir, order, sidecar, &candidates, &binding_of, &mut drop);
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
    retarget_displaced_exit_frees(
        tir,
        order,
        sidecar,
        decl_of_init,
        assign_value_of,
        reassign_orders,
        &mut drop,
    );
    deduplicate_exit_free_groups(order, sidecar, &candidates, &last_write_target, &mut drop);
}

fn deduplicate_exit_free_groups(
    order: &[u32],
    sidecar: &mut FunctionSidecar,
    candidates: &[(usize, TirRef, TirRef)],
    last_write_target: &impl Fn(TirRef) -> Option<TirRef>,
    drop: &mut HashSet<usize>,
) {
    // Pass 2: same (binding, exit) anchor — keep one Free, preferring
    // the Free whose target is the binding's last write (codegen's
    // redirect filter then passes unconditionally), then the
    // latest-ranked target, then the latest schedule index.
    let mut groups: HashMap<(TirRef, TirRef), Vec<usize>> = HashMap::new();
    for &(i, after, binding) in candidates {
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
pub(crate) fn anchored_after(tir: &Tir, order: &[u32], anchor: TirRef, point: TirRef) -> bool {
    order[anchor.index()] > order[point.index()]
        || matches!(
            tir.inst(anchor).tag,
            TirTag::WhileLoop | TirTag::ForRange | TirTag::IfStmt
        ) && tir.contains_reachable(anchor, point)
}
