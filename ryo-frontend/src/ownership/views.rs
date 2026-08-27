//! M8.4 slice projections and view liveness — split from `mod.rs`.

use super::{
    LoopNesting, Owner, OwnerState, Ownership, format_binding, needs_tracking, underlying_owner,
};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::tir::{Span, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
use std::collections::{HashMap, HashSet};

/// Deterministic iteration order for owners: the post passes
/// push `FreePoint`s while iterating owner-keyed `HashMap`s, whose
/// iteration order varies per run — sort by a stable key first.
pub(crate) fn owner_sort_key(owner: &Owner) -> (u8, u32) {
    match owner {
        Owner::Param(name) => (0, name.raw()),
        Owner::Inst(r) => (1, r.raw()),
    }
}

/// The `Owner` a `strview`-typed binding should point at: through `Var`
/// copies of other views to the original slice (P3), or the
/// initializer instruction itself.
pub(crate) fn resolve_view_alias(own: &Ownership, tir: &Tir, init: TirRef) -> Owner {
    if let TirData::Var(name) = tir.inst(init).data
        && let Some(&owner) = own.current_owner.get(&name)
    {
        return owner;
    }
    Owner::Inst(init)
}

/// The root owner a `strview`-typed value projects (P3, final spec §3.2):
/// a `str` base resolves to its underlying owner; a view base resolves
/// transitively to the original owner. `None` when the view projects
/// storage this function does not own (a `strview` parameter's buffer
/// belongs to the caller) — such projections need no freeze or
/// destruction tracking here.
pub(crate) fn projection_root(
    own: &Ownership,
    tir: &Tir,
    pool: &InternPool,
    r: TirRef,
) -> Option<Owner> {
    let inst = *tir.inst(r);
    // P6': a `strview → str` re-borrow is `str`-typed (so the
    // `needs_tracking` arm below would resolve the conversion inst
    // itself as a bogus fresh owner), but ownership-wise it IS the
    // view's borrow, call-scoped — resolve the operand's root.
    if inst.tag == TirTag::ViewAsStr
        && let TirData::UnOp(inner) = inst.data
    {
        return projection_root(own, tir, pool, inner);
    }
    if needs_tracking(inst.ty, pool) {
        return Some(underlying_owner(own, r));
    }
    if !pool.is_view(inst.ty) {
        return None;
    }
    match inst.data {
        TirData::Var(name) => match own.current_owner.get(&name) {
            Some(owner @ Owner::Inst(_)) => own.root_owner.get(owner).copied(),
            _ => None,
        },
        TirData::Slice { base, .. } => {
            // A slice already registered by its binding resolves to the
            // recorded root; an unbound (transient) slice resolves
            // through its own base.
            if let Some(&root) = own.root_owner.get(&Owner::Inst(r)) {
                return Some(root);
            }
            projection_root(own, tir, pool, base)
        }
        TirData::UnOp(inner) if inst.tag == TirTag::ViewOfStr => {
            projection_root(own, tir, pool, inner)
        }
        _ => None,
    }
}

/// P3 (final spec §3.2): register `view_owner` as a live projection of
/// the root owner its initializer resolves to. Idempotent — loop
/// convergence re-walks and `Var` copies re-register the same view.
pub(crate) fn register_projection(
    own: &mut Ownership,
    tir: &Tir,
    pool: &InternPool,
    init: TirRef,
    view_owner: Owner,
) {
    if let Some(root) = projection_root(own, tir, pool, init) {
        own.root_owner.insert(view_owner, root);
        let projections = own.live_projections.entry(root).or_default();
        if !projections.contains(&view_owner) {
            projections.push(view_owner);
        }
    }
}

/// Remove a view from its root's live set (P4). No-op for views that
/// were never registered (e.g. projections of non-local storage).
pub(crate) fn remove_projection(own: &mut Ownership, view: Owner) {
    if let Some(&root) = own.root_owner.get(&view)
        && let Some(projections) = own.live_projections.get_mut(&root)
    {
        projections.retain(|p| p != &view);
    }
}

/// End-of-statement projection death (P4, final spec §3.2). Runs after
/// a whole statement so a read and a consume within the same statement
/// both see the view as live (borrow-for-the-whole-statement
/// semantics, matching Rule 7).
pub(crate) fn drain_dying_views(own: &mut Ownership) {
    for view in std::mem::take(&mut own.pending_dying) {
        remove_projection(own, view);
    }
}

/// P4 (final spec §3.2): projections whose last read is inside this
/// loop but whose creation is outside it stayed live through the body
/// (a later iteration re-reads them); they die at the loop's exit.
pub(crate) fn remove_loop_deferred_views(own: &mut Ownership, loop_ref: TirRef) {
    let dead: Vec<Owner> = own
        .view_defer_loop
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let &l = slot.as_ref()?;
            (l == loop_ref).then(|| {
                let raw = u32::try_from(i).expect("TIR arena index fits u32");
                Owner::Inst(TirRef::from_raw(raw))
            })
        })
        .collect();
    for view in dead {
        remove_projection(own, view);
    }
}

/// Branch-merge liveness (P4, final spec §3.2): a view whose last read
/// is inside this `if`'s subtree has no reads after the join on ANY
/// path (the last-use map records the final read), so it is dead at
/// the join even though the union rule in `merge_branches` kept it
/// live. Views bound inside an arm are scope-dropped with it — their
/// last read is necessarily inside the branch — but their P5 deferral
/// on the root survives via `root_owner` (never pruned).
///
/// Loop-deferred views (`view_defer_loop`) are exempt: their last read
/// re-executes on later iterations, so their death is owned by
/// `remove_loop_deferred_views` at the enclosing loop's exit. Pruning
/// one here would lift the root's P2 freeze mid-loop-body and silently
/// accept owner mutations whose realloc a later iteration reads
/// through the view's stale pointer; conservative over-liveness is the
/// sound direction.
pub(crate) fn prune_branch_dead_projections(tir: &Tir, own: &mut Ownership, if_ref: TirRef) {
    let mut subtree: HashSet<TirRef> = HashSet::new();
    tir.collect_reachable(if_ref, &mut subtree);
    let dead: Vec<Owner> = own
        .root_owner
        .keys()
        .filter(|view| {
            let Some(vi) = view.inst_tirref() else {
                return false;
            };
            // P2/P4: loop-deferred views die at loop exit, not here.
            if Ownership::dense_get(&own.view_defer_loop, vi).is_some() {
                return false;
            }
            Ownership::dense_get(&own.view_last_use, vi).is_some_and(|lu| subtree.contains(&lu))
        })
        .copied()
        .collect();
    for view in dead {
        remove_projection(own, view);
    }
}

/// P2 freeze (final spec §3.2): while any slice projection of an owner
/// is live, moving or mutating the owner is a compile error. `verb` is
/// "move" (consume sites) or "mutate" (`inout` args, reassignment).
/// The diagnostic points at the consume site with a note at the
/// projection's last use (post-liveness, §3.5.3); a projection that is
/// never read notes its creation instead. Suppressed when the owner is
/// already `Moved` (E0020 covers it) or, for "move", when it is
/// `Borrowed` (E0021/E0022 cover it).
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_source_projected(
    tir: &Tir,
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
    let (note_span, note_msg) = match projections[0].inst_tirref() {
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

/// Pre-walk liveness for bound views (P4, final spec §3.2). See
/// [`collect_view_liveness`]. `last_use` / `defer_to_loop` are dense
/// per-instruction tables sized to `tir.instructions.len()` (slot 0,
/// the reserved sentinel, stays empty) — the walk writes them into
/// `Ownership` wholesale.
pub(crate) struct ViewLiveness {
    /// Bound view instruction → its last reading instruction.
    pub(crate) last_use: Vec<Option<TirRef>>,
    /// View instruction → the loop at whose exit the projection dies
    /// (its last read sits inside a loop the creation is outside of).
    pub(crate) defer_to_loop: Vec<Option<TirRef>>,
    /// Per-`if` arm-local reads (P4 per-arm refinement): if stmt →
    /// per-arm (view instruction → its last read within that arm's
    /// subtree), in walk order [then, elif..., else]. Consulted by
    /// `analyze_if_stmt` to refine `view_last_use` during arm walks.
    pub(crate) arm_last_reads: HashMap<TirRef, Vec<HashMap<TirRef, TirRef>>>,
}

/// Simulates the walk's `current_owner` discipline for `strview`-typed
/// bindings only (snapshot per branch arm, first-wins merge;
/// entry-first-wins at loop back-edges) and records each bound view's
/// last read. Runs before the forward walk so the walk knows when it
/// passes a projection's last use. A read inside a loop that does not
/// contain the view's creation re-executes on later iterations, so
/// the projection's death is deferred to that loop's exit.
pub(crate) fn collect_view_liveness(
    tir: &Tir,
    pool: &InternPool,
    nesting: &LoopNesting,
) -> ViewLiveness {
    let mut bindings: HashMap<StringId, TirRef> = HashMap::new();
    let mut last_use: HashMap<TirRef, TirRef> = HashMap::new();
    let mut arm_last_reads: HashMap<TirRef, Vec<HashMap<TirRef, TirRef>>> = HashMap::new();
    let body = tir.body_stmts();
    view_liveness_stmts(
        tir,
        pool,
        &body,
        &mut bindings,
        &mut last_use,
        &mut arm_last_reads,
    );
    let mut defer_to_loop = vec![None; tir.instructions.len()];
    for (view, read) in &last_use {
        let created_depth = nesting.depth_of(*view);
        let read_depth = nesting.depth_of(*read);
        // Scope rules guarantee the creation's nesting chain is a
        // prefix of the read's (a view's reads cannot escape its
        // binding's scope). A strictly deeper read re-executes on
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
        arm_last_reads,
    }
}

pub(crate) fn view_liveness_stmts(
    tir: &Tir,
    pool: &InternPool,
    stmts: &[TirRef],
    bindings: &mut HashMap<StringId, TirRef>,
    last_use: &mut HashMap<TirRef, TirRef>,
    arm_last_reads: &mut HashMap<TirRef, Vec<HashMap<TirRef, TirRef>>>,
) {
    for &s in stmts {
        view_liveness_stmt(tir, pool, s, bindings, last_use, arm_last_reads);
    }
}

pub(crate) fn view_liveness_stmt(
    tir: &Tir,
    pool: &InternPool,
    r: TirRef,
    bindings: &mut HashMap<StringId, TirRef>,
    last_use: &mut HashMap<TirRef, TirRef>,
    arm_last_reads: &mut HashMap<TirRef, Vec<HashMap<TirRef, TirRef>>>,
) {
    match tir.inst(r).tag {
        TirTag::VarDecl => {
            let view = tir.var_decl_view(r);
            record_view_reads(tir, pool, view.initializer, bindings, last_use);
            if pool.is_view(tir.inst(r).ty)
                && let Some(target) = view_binding_target(tir, bindings, view.initializer)
            {
                bindings.insert(view.name, target);
            }
        }
        TirTag::Assign => {
            let view = tir.assign_view(r);
            record_view_reads(tir, pool, view.value, bindings, last_use);
            if pool.is_view(tir.inst(r).ty) {
                match view_binding_target(tir, bindings, view.value) {
                    Some(target) => {
                        bindings.insert(view.name, target);
                    }
                    None => {
                        bindings.remove(&view.name);
                    }
                }
            }
        }
        TirTag::IfStmt => {
            let view = tir.if_stmt_view(r);
            record_view_reads(tir, pool, view.cond, bindings, last_use);
            // Snapshot per arm; merge first-wins, mirroring the walk's
            // `current_owner` discipline. Each arm body walks with a
            // fresh read map so the per-arm table can distinguish
            // arm-local reads; the records are then merged into
            // `last_use` in walk order (overwriting insert), leaving
            // the global map identical to a single shared walk.
            let pre = bindings.clone();
            let mut arm_maps = Vec::with_capacity(2 + view.elif_branches.len());
            let mut arm_reads: Vec<HashMap<TirRef, TirRef>> = Vec::with_capacity(
                1 + view.elif_branches.len() + usize::from(view.else_stmts.is_some()),
            );
            let mut then_reads = HashMap::new();
            view_liveness_stmts(
                tir,
                pool,
                &view.then_stmts,
                bindings,
                &mut then_reads,
                arm_last_reads,
            );
            for (k, v) in &then_reads {
                last_use.insert(*k, *v);
            }
            arm_reads.push(then_reads);
            arm_maps.push(std::mem::replace(bindings, pre.clone()));
            for elif in &view.elif_branches {
                record_view_reads(tir, pool, elif.cond, bindings, last_use);
                let mut body_reads = HashMap::new();
                view_liveness_stmts(
                    tir,
                    pool,
                    &elif.body,
                    bindings,
                    &mut body_reads,
                    arm_last_reads,
                );
                for (k, v) in &body_reads {
                    last_use.insert(*k, *v);
                }
                arm_reads.push(body_reads);
                arm_maps.push(std::mem::replace(bindings, pre.clone()));
            }
            if let Some(else_stmts) = &view.else_stmts {
                let mut else_reads = HashMap::new();
                view_liveness_stmts(
                    tir,
                    pool,
                    else_stmts,
                    bindings,
                    &mut else_reads,
                    arm_last_reads,
                );
                for (k, v) in &else_reads {
                    last_use.insert(*k, *v);
                }
                arm_reads.push(else_reads);
                arm_maps.push(std::mem::replace(bindings, pre.clone()));
            }
            arm_last_reads.insert(r, arm_reads);
            let mut merged = pre;
            for arm in arm_maps {
                for (k, v) in arm {
                    merged.entry(k).or_insert(v);
                }
            }
            *bindings = merged;
        }
        TirTag::WhileLoop => {
            let view = tir.while_loop_view(r);
            record_view_reads(tir, pool, view.cond, bindings, last_use);
            view_liveness_loop_body(tir, pool, &view.body, bindings, last_use, arm_last_reads);
        }
        TirTag::ForRange => {
            let view = tir.for_range_view(r);
            record_view_reads(tir, pool, view.start, bindings, last_use);
            record_view_reads(tir, pool, view.end, bindings, last_use);
            view_liveness_loop_body(tir, pool, &view.body, bindings, last_use, arm_last_reads);
        }
        _ => record_view_reads(tir, pool, r, bindings, last_use),
    }
}

/// Loop-body liveness with entry-first-wins at the back-edge (one pass
/// suffices: `last_use` records are monotone).
pub(crate) fn view_liveness_loop_body(
    tir: &Tir,
    pool: &InternPool,
    body: &[TirRef],
    bindings: &mut HashMap<StringId, TirRef>,
    last_use: &mut HashMap<TirRef, TirRef>,
    arm_last_reads: &mut HashMap<TirRef, Vec<HashMap<TirRef, TirRef>>>,
) {
    let pre = bindings.clone();
    view_liveness_stmts(tir, pool, body, bindings, last_use, arm_last_reads);
    let after = std::mem::replace(bindings, pre);
    for (k, v) in after {
        bindings.entry(k).or_insert(v);
    }
}

/// Record every `strview`-typed `Var` read within expression `r` as the
/// (latest) last use of the view it currently aliases. Overwriting
/// insert: the latest forward-order read wins.
pub(crate) fn record_view_reads(
    tir: &Tir,
    pool: &InternPool,
    r: TirRef,
    bindings: &HashMap<StringId, TirRef>,
    last_use: &mut HashMap<TirRef, TirRef>,
) {
    let inst = *tir.inst(r);
    if inst.tag == TirTag::Var
        && pool.is_view(inst.ty)
        && let TirData::Var(name) = inst.data
        && let Some(&view_inst) = bindings.get(&name)
    {
        last_use.insert(view_inst, r);
    }
    tir.walk_operands(r, &mut |_parent, child, _kind| {
        record_view_reads(tir, pool, child, bindings, last_use);
    });
}

/// The view instruction a `strview`-typed binding aliases: through `Var`
/// copies to the original slice (P3), or the initializer itself.
/// `None` for views of non-local storage (e.g. a `strview` parameter).
pub(crate) fn view_binding_target(
    tir: &Tir,
    bindings: &HashMap<StringId, TirRef>,
    init: TirRef,
) -> Option<TirRef> {
    match tir.inst(init).data {
        TirData::Var(name) => bindings.get(&name).copied(),
        TirData::Slice { .. } => Some(init),
        // A bound ViewOfStr (`u: strview = s`) projects the operand's
        // owner full-range; the conversion inst stands in as the view.
        _ if tir.inst(init).tag == TirTag::ViewOfStr => Some(init),
        _ => None,
    }
}

/// Per-arm freeze refinement (P2/P4, final spec §3.2): during an
/// if-arm walk, a view's last use for freeze purposes is its last
/// read on the path THROUGH this arm, not the global max over all
/// arms. Applied before an arm body walks; two cases:
///
/// * Override: a view read in this arm whose global last use lies in
///   a DIFFERENT arm of this if dies at its arm-local last read. The
///   replaced entries are returned for `restore_view_last_use` (the
///   walk-constant map must be whole again before the next arm and
///   before the join-time prune).
/// * Kill: a view whose every remaining read lies in OTHER arms of
///   this if (global last use inside the if's subtree, none in this
///   arm's) is already dead on this arm's path — its projection is
///   removed for the duration of the arm walk. `analyze_if_stmt`'s
///   per-arm snapshot/restore of `live_projections` scopes the
///   removal to this arm.
///
/// Both cases skip loop-deferred views (`view_defer_loop`): a later
/// iteration re-reads them through the back-edge, so they are live on
/// every arm's path regardless of this arm's reads. The deferral table
/// is computed from the GLOBAL max read only, so the override applies
/// the same deferral test per candidate: an arm-local last read inside
/// a loop the creation is outside of (`created_in < read_in`, the
/// pre-pass's condition) blocks the override — installing it would let
/// the death site drain the projection mid-loop and un-freeze a later
/// owner mutation in the same body. (Skipped, the view stays live to
/// the join: conservative.) The kill needs no such per-candidate test:
/// it fires only when the arm has NO reads in its subtree, so there is
/// no deeper arm-local read to strand; a deeper read in a SIBLING arm
/// is itself a global-max read the pre-pass deferral already covers.
/// Neither case applies when the global last use is OUTSIDE the if's
/// subtree — a post-join read lies on every path and keeps the view
/// live in every arm.
pub(crate) fn refine_view_liveness_for_arm(
    own: &mut Ownership,
    if_ref: TirRef,
    arm_index: usize,
    if_subtree: &HashSet<TirRef>,
    arm_subtree: &HashSet<TirRef>,
) -> Vec<(TirRef, TirRef)> {
    let arm_reads = own
        .if_arm_last_reads
        .get(&if_ref)
        .and_then(|arms| arms.get(arm_index));
    let actions: Vec<(TirRef, Option<TirRef>, TirRef)> = own
        .view_last_use
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| {
            let &global_lu = slot.as_ref()?;
            let vi = TirRef::from_raw(u32::try_from(i).expect("TIR arena index fits u32"));
            (if_subtree.contains(&global_lu)
                && !arm_subtree.contains(&global_lu)
                && Ownership::dense_get(&own.view_defer_loop, vi).is_none())
            .then_some((
                vi,
                arm_reads.and_then(|reads| reads.get(&vi)).copied(),
                global_lu,
            ))
        })
        .collect();
    let mut saved = Vec::new();
    for (vi, arm_lu, global_lu) in actions {
        match arm_lu {
            Some(lu) => {
                // P4 deferral, per candidate: `view_defer_loop` covers
                // only the global max read, so re-apply the pre-pass's
                // `created_depth < read_depth` test to the arm-local read the
                // override would install.
                if own.loop_nesting.depth_of(vi) < own.loop_nesting.depth_of(lu) {
                    continue;
                }
                saved.push((vi, global_lu));
                Ownership::dense_set(&mut own.view_last_use, vi, lu);
            }
            None => remove_projection(own, Owner::Inst(vi)),
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
