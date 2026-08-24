//! Forward statement/expression walk — split from `mod.rs`.

use super::*;

pub(crate) fn analyze_stmt(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    stmt: TirRef,
) {
    let inst = *tir.inst(stmt);
    match inst.tag {
        TirTag::VarDecl => analyze_var_decl(tir, pool, own, sink, sidecar, stmt),
        TirTag::Assign => analyze_assign(tir, pool, own, sink, sidecar, stmt),
        TirTag::Return => {
            analyze_return(tir, pool, own, sink, sidecar, stmt);
            record_return_epilogue(own, stmt);
        }
        TirTag::ReturnVoid => record_return_epilogue(own, stmt),
        TirTag::IfStmt => analyze_if_stmt(tir, pool, own, sink, sidecar, stmt),
        TirTag::WhileLoop => analyze_while_loop(tir, pool, own, sink, sidecar, stmt),
        TirTag::ForRange => analyze_for_range(tir, pool, own, sink, sidecar, stmt),
        TirTag::Break | TirTag::Continue => {
            // 8.1c attaches Free metadata here; 8.1b is a no-op.
        }
        TirTag::CompoundAssign => {
            // Sema rejects compound-assign on Move-typed values today
            // (str doesn't support `+=`/`-=`/etc.). Enforce the invariant
            // here so a future sema relaxation that reaches the ownership
            // pass without ownership-aware handling trips a debug build
            // instead of silently falling through to no analysis.
            let view = tir.compound_assign_view(stmt);
            debug_assert!(
                !needs_tracking(tir.inst(view.value).ty, pool),
                "compound-assign on Move-typed value reached ownership pass; \
                 sema should have rejected",
            );
        }
        TirTag::ExprStmt => {
            if let TirData::UnOp(o) = inst.data {
                visit_expr(tir, pool, own, sink, sidecar, o);
            }
        }
        _ => {
            visit_expr(tir, pool, own, sink, sidecar, stmt);
        }
    }
    // P4 lift (final spec §3.2): projections whose last use this
    // statement contained die here — after the whole statement, so a
    // read and a consume within the same statement both see the view
    // as live.
    drain_dying_views(own);
}

/// Move-typed `VarDecl` is a consumer: the new binding takes
/// ownership of the initializer's underlying value. If the
/// initializer aliases a borrowed parameter, this is the E0021
/// "move out of borrowed parameter" site; if the underlying owner is
/// already `Moved`, this is the E0020 "use after move" site.
pub(crate) fn analyze_var_decl(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.var_decl_view(r);
    let init = view.initializer;
    let init_ty = tir.inst(init).ty;
    visit_expr(tir, pool, own, sink, sidecar, init);
    if needs_tracking(init_ty, pool) {
        let span = tir.span(r);
        let consumed_name = consumed_binding_name(tir, init);
        // P2 freeze (final spec §3.2): the consume moves the owner.
        check_source_projected(
            tir,
            pool,
            own,
            sink,
            underlying_owner(own, init),
            span,
            "move",
            consumed_name,
        );
        consume_for_assignment(tir, pool, own, sink, init, span, consumed_name, r);
        rebind_to_init(own, view.name, init);
        // Register the new binding as pending dead-store. The walk
        // clears this entry on any later read or consumption; a
        // surviving entry at function end fires W0001. Keyed by
        // `init`, the same TirRef `rebind_to_init` stamped Valid.
        register_pending_dead_store(own, init, view.name, span, r);
    } else if pool.is_view(init_ty) {
        // P3 (final spec §3.2): binding a slice registers a projection
        // against the root owner (re-slices resolve transitively).
        // Var copies alias the original slice rather than projecting
        // again.
        let view_owner = resolve_view_alias(own, tir, init);
        own.current_owner.insert(view.name, view_owner);
        register_projection(own, tir, pool, init, view_owner);
    } else {
        own.current_owner.insert(view.name, Owner::Inst(init));
    }
}

/// Reassignment to a Move-typed binding. Same consumption rules as
/// `VarDecl`; the existing binding name is reseated to whichever
/// SSA value owns the new underlying allocation.
pub(crate) fn analyze_assign(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.assign_view(r);
    let value_ty = tir.inst(view.value).ty;
    visit_expr(tir, pool, own, sink, sidecar, view.value);
    if needs_tracking(value_ty, pool) {
        // Capture the old owner before consume_for_assignment / rebind
        // overwrite current_owner[name]. Only emit the Free entry if the
        // old owner is Valid — Borrowed/Moved/missing means there is no
        // live allocation to release. EXCEPTION: an `inout str` param's
        // current owner is Borrowed, yet the callee must drop the old
        // pointee when reassigning the param (the write-back ABI hands
        // the caller whatever triple the param holds at exit — like
        // `*x = new` dropping the old value in Rust). Codegen (Task 8)
        // consults this map when lowering Assign.
        if let Some(&old_owner) = own.current_owner.get(&view.name) {
            let old_droppable = match own.states.get(&old_owner) {
                Some(OwnerState::Valid) => true,
                Some(OwnerState::Borrowed) => own.inout_str_params.contains(&view.name),
                _ => false,
            };
            if old_droppable {
                // P2 freeze (final spec §3.2): reassignment mutates the
                // owner — illegal while a slice of it is live.
                check_source_projected(
                    tir,
                    pool,
                    own,
                    sink,
                    old_owner,
                    tir.span(r),
                    "mutate",
                    Some(view.name),
                );
                // `tirref` (not `inst_tirref`): an inout param's old owner
                // is a `Param`, resolved here to its virtual ref — codegen
                // caches that ref's repr at the prologue.
                sidecar
                    .free_on_reassign
                    .insert(r, old_owner.tirref(&own.param_index));
                // W0003 case-B support: reassignment mutates the binding's
                // owner — a defensive-copy hazard on it.
                own.owner_hazards.push((old_owner, r));
                // Reassignment runs the old value's destructor (the
                // free_on_reassign Free above) — that's an observable use,
                // so the prior VarDecl/Assign isn't a dead store. Drop the
                // pending_dead_store entry so W0001 doesn't fire and the
                // drain block doesn't try to schedule a redundant Free.
                own.pending_dead_store.remove(&old_owner);
            }
        }
        let span = tir.span(r);
        let consumed_name = consumed_binding_name(tir, view.value);
        // P2 freeze (final spec §3.2): the consume moves the owner.
        check_source_projected(
            tir,
            pool,
            own,
            sink,
            underlying_owner(own, view.value),
            span,
            "move",
            consumed_name,
        );
        consume_for_assignment(tir, pool, own, sink, view.value, span, consumed_name, r);
        rebind_to_init(own, view.name, view.value);
        register_pending_dead_store(own, view.value, view.name, span, r);
    } else if pool.is_view(value_ty) {
        // P3/P4 (final spec §3.2): rebinding a view registers the new
        // projection and ends the old binding's — unless another
        // binding still aliases the old view (a Var copy keeps it
        // alive).
        let old_owner = own.current_owner.get(&view.name).copied();
        let view_owner = resolve_view_alias(own, tir, view.value);
        register_projection(own, tir, pool, view.value, view_owner);
        own.current_owner.insert(view.name, view_owner);
        if let Some(old) = old_owner
            && old != view_owner
            && own.root_owner.contains_key(&old)
            && !own.current_owner.values().any(|o| *o == old)
        {
            own.pending_dying.push(old);
        }
    }
}

/// Move-typed `Return` is a consumer: the returned value flows out of
/// the function and the caller takes ownership. Borrowed parameters
/// cannot be returned (E0022). If the underlying owner is already
/// `Moved`, this is the E0020 "use after move" site.
pub(crate) fn analyze_return(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    let operand = match inst.data {
        TirData::UnOp(o) => o,
        _ => unreachable!("Return must carry TirData::UnOp"),
    };
    let ty = tir.inst(operand).ty;
    visit_expr(tir, pool, own, sink, sidecar, operand);
    if pool.is_view(ty) {
        // E1 (final spec §3.3): slices cannot be returned from
        // functions. Backstop to sema's signature-level rejection.
        sink.emit(
            Diag::error(
                tir.span(r),
                DiagCode::ViewEscape,
                "cannot return a slice from a function",
            )
            .with_help("slices are non-escaping; return an owned `str` instead"),
        );
        return;
    }
    if !needs_tracking(ty, pool) {
        return;
    }
    let span = tir.span(r);
    let consumed_name = consumed_binding_name(tir, operand);
    // P2 freeze (final spec §3.2): returning moves the value out.
    check_source_projected(
        tir,
        pool,
        own,
        sink,
        underlying_owner(own, operand),
        span,
        "move",
        consumed_name,
    );
    consume_underlying(
        tir,
        pool,
        own,
        sink,
        operand,
        span,
        consumed_name,
        BorrowedAction::ReturnBorrowed,
        r,
    );
}

/// Reseat `name` to point at `init` as its current owner. After a
/// consume, the source binding's underlying value is `Moved`; the
/// new binding takes a fresh, independent slot in the lattice so
/// subsequent reads of `name` resolve to a `Valid` owner instead of
/// tripping over the just-moved underlying. We do this by severing
/// `init`'s `origin` link (if any) and stamping it `Valid`.
pub(crate) fn rebind_to_init(own: &mut Ownership, name: StringId, init: TirRef) {
    own.origin.insert(init, None);
    own.states.insert(Owner::Inst(init), OwnerState::Valid);
    own.current_owner.insert(name, Owner::Inst(init));
}

/// Register a Move-typed binding into `pending_dead_store`. The owner
/// key (`init`/`value` TirRef) is whatever currently owns the
/// allocation; `decl_inst` is the `VarDecl`/`Assign` instruction's own
/// TirRef and serves as the Free anchor if the binding turns out to be
/// a dead store. Single source of truth for `analyze_var_decl` and
/// `analyze_assign` — kept in one place so the two registration paths
/// cannot drift apart.
pub(crate) fn register_pending_dead_store(
    own: &mut Ownership,
    owner: TirRef,
    name: StringId,
    span: Span,
    decl_inst: TirRef,
) {
    own.pending_dead_store
        .insert(Owner::Inst(owner), (name, span, decl_inst));
}

/// Walk back from `init` to whichever SSA value currently owns the
/// underlying allocation. `visit_expr` is responsible for populating
/// `origin` for `Var` reads; for fresh producers (`StrConst`,
/// `StrConcat`, `Call`) `init` is itself the owner.
pub(crate) fn underlying_owner(own: &Ownership, init: TirRef) -> Owner {
    match own.origin.get(&init).copied() {
        Some(Some(owner)) => owner,
        _ => Owner::Inst(init),
    }
}

/// Aliasing identity of an `inout` (or Copy borrow) call argument for the
/// Rule 7 overlap check (M8.3). Tracked (Move-typed) args resolve to the
/// usual underlying owner. Copy scalars never enter the lattice, so their
/// stable identity is the binding's `current_owner` slot (seeded at the
/// VarDecl) — two `&c` reads of one `mut c` must collide even though each
/// read is its own SSA value. An unregistered name (a Copy-typed
/// parameter, which the param loop skips) falls back to
/// `Owner::Param(name)` — the correct per-binding key for exactly that
/// case.
pub(crate) fn inout_owner(own: &Ownership, tir: &Tir, arg: TirRef) -> Owner {
    match tir.inst(arg).data {
        TirData::Var(name) => own
            .current_owner
            .get(&name)
            .copied()
            .unwrap_or(Owner::Param(name)),
        _ => underlying_owner(own, arg),
    }
}

/// True if `owner` is the value currently bound to an `inout str`
/// parameter name. That value ESCAPES through the write-back pointer at
/// function exit, so it can be reassigned (dropping the old pointee) but
/// never moved out of the function — not even after a reassign replaced
/// the original (Borrowed) param owner with a fresh Valid one.
pub(crate) fn inout_escape_owner(own: &Ownership, owner: Owner) -> bool {
    own.inout_str_params
        .iter()
        .any(|n| own.current_owner.get(n) == Some(&owner))
}

/// Use-site use-after-move authority (spec §5.3 Rule 1 — a move
/// invalidates the original binding). Resolve the operand's
/// underlying owner and emit E0020 if it is `Moved`. Called from every
/// use site: consume sites, borrow-arg paths, and operand-read sites.
pub(crate) fn check_use_moved(
    tir: &Tir,
    pool: &InternPool,
    own: &Ownership,
    sink: &mut DiagSink,
    operand: TirRef,
    span: Span,
) {
    let owner = underlying_owner(own, operand);
    if let Some(OwnerState::Moved { moved_at }) = own.states.get(&owner).cloned() {
        let name = consumed_binding_name(tir, operand);
        sink.emit(
            Diag::error(span, DiagCode::UseAfterMove,
                format!("use of moved value {}", format_binding(name, pool)))
                .with_note(Some(moved_at), "value moved here")
                .with_help("consider using the value before the move, or pass by default (borrow) instead of `move`"),
        );
    }
}

/// Which diagnostic the shared `consume_underlying` helper should
/// emit on the `Borrowed` arm. `consume_for_assignment` and
/// `analyze_return` run identical state transitions otherwise — the
/// only thing that diverges is the error code / wording / help when
/// the underlying owner is borrowed.
pub(crate) enum BorrowedAction {
    /// E0021 — `consume_for_assignment` site (VarDecl / Assign /
    /// move-typed Call argument).
    MoveOutOfParam,
    /// E0022 — `analyze_return` site.
    ReturnBorrowed,
}

/// Apply the consumption transition for a Move-typed initializer.
/// Caller must have already populated origin/state for `init` via
/// `visit_expr`. `site` is the consuming instruction (VarDecl /
/// Assign / Return / move-typed Call), recorded in the W0003 hazard
/// log when the consume lands.
#[allow(clippy::too_many_arguments)]
pub(crate) fn consume_for_assignment(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    init: TirRef,
    span: Span,
    name: Option<StringId>,
    site: TirRef,
) {
    consume_underlying(
        tir,
        pool,
        own,
        sink,
        init,
        span,
        name,
        BorrowedAction::MoveOutOfParam,
        site,
    );
}

/// Shared transition for every consume site (assignment, return,
/// move-typed Call argument). Walks back to the underlying owner,
/// reads its state, and either:
///
/// * `Valid` → stamp `Moved { moved_at: span }`, clear any pending
///   dead-store entry, and log a W0003 move-hazard at `site`,
/// * `Borrowed` → emit E0021 or E0022 per `on_borrowed`,
/// * `Moved` → emit E0020 (this site is the use-after-move check
///   authority).
/// * `NotTracked` → no-op.
#[allow(clippy::too_many_arguments)]
pub(crate) fn consume_underlying(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    operand: TirRef,
    span: Span,
    name: Option<StringId>,
    on_borrowed: BorrowedAction,
    site: TirRef,
) {
    let underlying = underlying_owner(own, operand);
    let mut state = own
        .states
        .get(&underlying)
        .cloned()
        .unwrap_or(OwnerState::NotTracked);
    // A Valid value currently bound to an `inout str` param still escapes
    // through the write-back pointer — moving it out would leave the slot
    // holding a stale triple. Treat it as borrowed at consume sites.
    if matches!(state, OwnerState::Valid) && inout_escape_owner(own, underlying) {
        state = OwnerState::Borrowed;
    }
    match state {
        OwnerState::Valid => {
            own.pending_dead_store.remove(&underlying);
            own.states
                .insert(underlying, OwnerState::Moved { moved_at: span });
            // W0003 case-B support: the consume is a move-hazard on the
            // owner, anchored at the consuming instruction.
            own.owner_hazards.push((underlying, site));
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
            check_use_moved(tir, pool, own, sink, operand, span);
        }
        OwnerState::NotTracked => {}
    }
}

/// Subtree TirRef set of a statement list — every ref reachable from
/// each statement, matching `prune_branch_dead_projections`'s notion
/// of "inside the branch".
pub(crate) fn stmts_subtree(tir: &Tir, stmts: &[TirRef]) -> HashSet<TirRef> {
    let mut set = HashSet::new();
    for &s in stmts {
        tir.collect_reachable(s, &mut set);
    }
    set
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
pub(crate) fn analyze_if_stmt(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let view = tir.if_stmt_view(r);
    visit_expr(tir, pool, own, sink, sidecar, view.cond);
    // P4 lift (final spec §3.2): a projection whose last use is the
    // condition is dead before any arm runs — drain now so every arm
    // starts from the same freeze state. (A consume inside the
    // condition itself was already checked above, before the drain.)
    drain_dying_views(own);
    // Subtree sets for the per-arm freeze refinement: the whole if
    // (same set `prune_branch_dead_projections` uses at the join) and,
    // per arm, the arm's body statements (conditions belong to the
    // shared flow, not to a specific arm).
    let mut if_subtree: HashSet<TirRef> = HashSet::new();
    tir.collect_reachable(r, &mut if_subtree);

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
    // Mint a BranchId for the else/fall-through arm ALWAYS — an
    // else-less if's fall-through still needs an id so the
    // conditional DeadDrops can gate on it.
    let else_branch = {
        let id = BranchId(own.next_branch_id);
        own.next_branch_id += 1;
        Some(id)
    };
    sidecar.if_branches.insert(
        r,
        IfBranchIds {
            then_branch,
            elif_branches: elif_branches.clone(),
            else_branch,
        },
    );

    let snap_states = own.states.clone();
    let snap_current_owner = own.current_owner.clone();
    let snap_pending_dead_store = own.pending_dead_store.clone();
    // P2 freeze ranges are non-monotone (projections die at their last
    // use), so they join the per-arm snapshot/restore set like the
    // other non-monotone fields. `root_owner` is insert-only and stays
    // live across arms, mirroring `origin`.
    let snap_live_projections = own.live_projections.clone();

    let then_subtree = stmts_subtree(tir, &view.then_stmts);
    let saved = refine_view_liveness_for_arm(own, r, 0, &if_subtree, &then_subtree);
    for stmt in &view.then_stmts {
        analyze_stmt(tir, pool, own, sink, sidecar, *stmt);
    }
    restore_view_last_use(own, saved);
    let then_state = own.clone();

    let mut branch_results = vec![then_state];
    for (elif_index, elif) in view.elif_branches.iter().enumerate() {
        own.states = snap_states.clone();
        own.current_owner = snap_current_owner.clone();
        own.pending_dead_store = snap_pending_dead_store.clone();
        own.live_projections = snap_live_projections.clone();

        visit_expr(tir, pool, own, sink, sidecar, elif.cond);
        // See the then-cond note above: projections dying at an elif
        // condition lift before its body runs.
        drain_dying_views(own);
        let elif_subtree = stmts_subtree(tir, &elif.body);
        let saved =
            refine_view_liveness_for_arm(own, r, 1 + elif_index, &if_subtree, &elif_subtree);
        for stmt in &elif.body {
            analyze_stmt(tir, pool, own, sink, sidecar, *stmt);
        }
        restore_view_last_use(own, saved);
        branch_results.push(own.clone());
    }

    if let Some(else_stmts) = &view.else_stmts {
        own.states = snap_states.clone();
        own.current_owner = snap_current_owner.clone();
        own.pending_dead_store = snap_pending_dead_store.clone();
        own.live_projections = snap_live_projections.clone();

        let else_subtree = stmts_subtree(tir, else_stmts);
        let saved = refine_view_liveness_for_arm(
            own,
            r,
            1 + view.elif_branches.len(),
            &if_subtree,
            &else_subtree,
        );
        for stmt in else_stmts {
            analyze_stmt(tir, pool, own, sink, sidecar, *stmt);
        }
        restore_view_last_use(own, saved);
        branch_results.push(own.clone());
    } else {
        let mut else_snap = own.clone();
        else_snap.states = snap_states.clone();
        else_snap.current_owner = snap_current_owner.clone();
        else_snap.pending_dead_store = snap_pending_dead_store.clone();
        else_snap.live_projections = snap_live_projections.clone();
        branch_results.push(else_snap);
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

    let all_keys: HashSet<Owner> = arms
        .iter()
        .flat_map(|a| a.state.states.keys().copied())
        .collect();
    // Sorted iteration for deterministic free_schedule order.
    let mut all_keys: Vec<Owner> = all_keys.into_iter().collect();
    all_keys.sort_by_key(owner_sort_key);
    for owner in all_keys {
        let is_tracked = match owner {
            Owner::Inst(_) => true,
            Owner::Param(name) => {
                let idx = *own.param_index.get(&name).expect("param exists");
                needs_tracking(tir.params[idx].ty, pool)
            }
        };
        if !is_tracked {
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
                let span = match owner {
                    Owner::Inst(r) => tir.span(r),
                    Owner::Param(name) => {
                        let idx = *own.param_index.get(&name).expect("param exists");
                        tir.params[idx].span
                    }
                };
                sidecar.free_schedule.push(FreePoint {
                    after,
                    target: owner.tirref(&own.param_index),
                    span,
                    branch: Some(arm.branch_id),
                });
            }
            // Empty arm or sema-rejected case: skip. The M8.1
            // grammar forbids empty arms.
        }
    }

    // Record conditional reseats — bindings that SOME arm
    // reseated while other arms kept the pre-if owner. The dead-store
    // drain (function exit) converts a record into arm-gated
    // `ConditionalDeadDrop`s when the reassigned value is never read
    // afterwards, so the pre-if buffer is also freed on the paths where
    // the reassign did not happen. Includes the implicit fall-through
    // pseudo-arm of an else-less if (its BranchId was minted above).
    {
        let mut arm_states: Vec<(BranchId, &Ownership)> = Vec::with_capacity(branch_results.len());
        arm_states.push((then_branch, &branch_results[0]));
        for (i, _) in view.elif_branches.iter().enumerate() {
            arm_states.push((elif_branches[i], &branch_results[1 + i]));
        }
        arm_states.push((
            else_branch.expect("else/fall-through arm id minted"),
            branch_results
                .last()
                .expect("else/fall-through snapshot pushed"),
        ));
        for (name, owner_pre) in &snap_current_owner {
            // Only tracked (Move-typed) locals need drops; Copy values
            // have no buffer, and params are covered by the exit-time
            // param free.
            let Owner::Inst(pre_ref) = owner_pre else {
                continue;
            };
            if !needs_tracking(tir.inst(*pre_ref).ty, pool) {
                continue;
            }
            let mut reseat_owners: HashSet<Owner> = HashSet::new();
            let mut untouched_arms: Vec<BranchId> = Vec::new();
            for (bid, b) in &arm_states {
                let owner_b = b.current_owner.get(name).copied().unwrap_or(*owner_pre);
                if owner_b == *owner_pre {
                    untouched_arms.push(*bid);
                } else {
                    reseat_owners.insert(owner_b);
                }
            }
            // Dedup: loop-convergence re-walks revisit this if.
            if !reseat_owners.is_empty()
                && !untouched_arms.is_empty()
                && !own
                    .reseat_drops
                    .iter()
                    .any(|d| d.if_stmt == r && d.name == *name)
            {
                own.reseat_drops.push(ReseatDrop {
                    if_stmt: r,
                    name: *name,
                    pre_owner: *owner_pre,
                    reseat_owners,
                    untouched_arms,
                });
            }
        }
    }

    // Final restore: restore only the non-monotone fields.
    own.states = snap_states;
    own.current_owner = snap_current_owner;
    own.pending_dead_store = snap_pending_dead_store;
    own.live_projections = snap_live_projections;
    let refs: Vec<&Ownership> = branch_results.iter().collect();
    own.merge_branches(&refs);
    // P4 (final spec §3.2): a view whose last use is inside this if is
    // dead at the join on every path — prune it from the merged freeze
    // ranges (see prune_branch_dead_projections).
    prune_branch_dead_projections(tir, own, r);
}

pub(crate) fn visit_expr(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
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
                own.states.insert(Owner::Inst(r), OwnerState::Valid);
                own.origin.insert(r, None);
                own.temp_owners.insert(Owner::Inst(r));
            }
        }
        TirTag::StrConcat => {
            if needs_tracking(inst.ty, pool) {
                own.states.insert(Owner::Inst(r), OwnerState::Valid);
                own.origin.insert(r, None);
                own.temp_owners.insert(Owner::Inst(r));
            }
            if let TirData::BinOp { lhs, rhs } = inst.data {
                visit_expr(tir, pool, own, sink, sidecar, lhs);
                visit_expr(tir, pool, own, sink, sidecar, rhs);
                for op in [lhs, rhs] {
                    if needs_tracking(tir.inst(op).ty, pool) {
                        check_use_moved(tir, pool, own, sink, op, tir.span(op));
                    }
                }
            }
        }
        TirTag::Call => {
            // A str-returning call (e.g. `int_to_str`) is a producer.
            if needs_tracking(inst.ty, pool) {
                own.states.insert(Owner::Inst(r), OwnerState::Valid);
                own.origin.insert(r, None);
                own.temp_owners.insert(Owner::Inst(r));
            }
            let view = tir.call_view(r);

            // Phase 1 — materialise every arg's owner/state.
            for arg in &view.args {
                visit_expr(tir, pool, own, sink, sidecar, *arg);
            }

            // Phase 2 — use-safety check + borrow/move/inout partition.
            let mut borrowed: Vec<Owner> = Vec::new();
            let mut moved: Vec<Owner> = Vec::new();
            // E4 (final spec §3.3): a view argument borrows its ROOT
            // owner for the duration of the call — tracked separately
            // from `borrowed` so a same-call move of the root is
            // reported as a P2 freeze violation (SourceProjected)
            // rather than as a plain borrow/move overlap (E0031).
            let mut view_borrowed: Vec<Owner> = Vec::new();
            // inout occurrences: (owner, arg span) — a Vec, not a Set, so
            // a double-inout of one owner is detectable by count (Rule 7).
            let mut inout_uses: Vec<(Owner, Span)> = Vec::new();
            for (i, arg) in view.args.iter().enumerate() {
                if is_borrowed_scalar_param(view.name, pool, i)
                    && matches!(tir.inst(*arg).tag, TirTag::StrConst)
                {
                    // Undo the temp_owners insertion seeded by visit_expr's
                    // StrConst arm. `states`, `origin`, and `owner_at_read`
                    // entries are harmless to leave populated — only
                    // `temp_owners` is consulted by the anonymous-temp Free
                    // pass.
                    own.temp_owners.remove(&Owner::Inst(*arg));
                }
                let mode = view.modes.get(i).copied().unwrap_or(ParamMode::Borrow);
                let arg_ty = tir.inst(*arg).ty;
                if mode == ParamMode::Inout {
                    // Rule 7 (M8.3): resolve an aliasing identity even for
                    // Copy scalars, which never enter the lattice.
                    let owner = inout_owner(own, tir, *arg);
                    check_use_moved(tir, pool, own, sink, *arg, tir.span(*arg));
                    if needs_tracking(arg_ty, pool) {
                        // P2 freeze (final spec §3.2): `inout` passing
                        // mutates the owner.
                        check_source_projected(
                            tir,
                            pool,
                            own,
                            sink,
                            owner,
                            tir.span(*arg),
                            "mutate",
                            consumed_binding_name(tir, *arg),
                        );
                    }
                    inout_uses.push((owner, tir.span(*arg)));
                    // W0003 case-B support: an `inout` pass mutates the
                    // owner — a defensive-copy hazard on it.
                    own.owner_hazards.push((owner, r));
                    continue;
                }
                if !needs_tracking(arg_ty, pool) {
                    if mode == ParamMode::Borrow && pool.is_view(arg_ty) {
                        // A view arg borrows its root owner for the
                        // call's duration (E4). `projection_root` looks
                        // through ViewOfStr conversions (the implicit
                        // str → strview coercion) to the underlying owner
                        // — without this, `two(&s, s)` with an
                        // (inout, strview) signature would escape the
                        // Rule-7 partition.
                        if let Some(root) = projection_root(own, tir, pool, *arg) {
                            push_unique(&mut view_borrowed, root);
                        }
                    } else if mode == ParamMode::Borrow && matches!(tir.inst(*arg).tag, TirTag::Var)
                    {
                        // A Copy borrow is a no-op for liveness, but it still
                        // aliases an `inout` of the same binding in this call —
                        // record Var reads by name for the Rule 7 overlap check.
                        // (A Copy `move` arg is rejected by sema's RedundantMove,
                        // so only the Borrow arm is reachable from real code.)
                        push_unique(&mut borrowed, inout_owner(own, tir, *arg));
                    }
                    continue;
                }
                // M8.4.1.2: a `str(view)` materialization call passed as
                // a borrow-mode arg READS the view's buffer at call time
                // — the view's ROOT owner is immutably borrowed for the
                // outer call's duration (E4), exactly like passing the
                // view itself. The ABI registry (`view_borrow_params`)
                // names the callees this applies to; the copy result is a
                // fresh owner with no aliasing identity, so it is not
                // itself recorded in `borrowed`.
                if mode == ParamMode::Borrow && tir.inst(*arg).tag == TirTag::Call {
                    let inner = tir.call_view(*arg);
                    let vb_params = view_borrow_params(inner.name, pool);
                    if !vb_params.is_empty() {
                        for &idx in vb_params {
                            if let Some(&varg) = inner.args.get(idx)
                                && let Some(root) = projection_root(own, tir, pool, varg)
                            {
                                push_unique(&mut view_borrowed, root);
                            }
                        }
                        continue;
                    }
                }
                // P6': a view re-borrowed into a `str` arg (ViewAsStr)
                // borrows the view's ROOT owner for the call's duration
                // — look through the conversion exactly like the
                // str → strview direction above, or `two(&s, s[0:1])`
                // would escape the Rule-7 partition.
                let owner = if mode == ParamMode::Borrow && tir.inst(*arg).tag == TirTag::ViewAsStr
                {
                    projection_root(own, tir, pool, *arg)
                        .unwrap_or_else(|| underlying_owner(own, *arg))
                } else {
                    underlying_owner(own, *arg)
                };
                if mode == ParamMode::Borrow {
                    check_use_moved(tir, pool, own, sink, *arg, tir.span(*arg));
                    push_unique(&mut borrowed, owner);
                } else {
                    push_unique(&mut moved, owner);
                }
            }
            // Rule 7 (M8.3): at most one mutable borrow per owner in a call,
            // and no immutable borrow / move alongside it. Each DISTINCT
            // inout owner is handled exactly once (the prefix scan skips
            // repeat occurrences) so cases 2/3 don't re-fire per occurrence.
            for (pos, (owner, _span)) in inout_uses.iter().enumerate() {
                if inout_uses[..pos].iter().any(|(o, _)| o == owner) {
                    continue;
                }
                let name = rule7_owner_name(own, tir, pool, &view.args, *owner);

                // (1) The same owner is mutably borrowed more than once.
                // Collect every occurrence so the two notes point at
                // DISTINCT arg spans — the loop item's own span is
                // occurrence 0, so reusing it for the "second" note would
                // duplicate the "first" note's span.
                let occurrences: Vec<Span> = inout_uses
                    .iter()
                    .filter(|(o, _)| o == owner)
                    .map(|(_, s)| *s)
                    .collect();
                if occurrences.len() > 1 {
                    let mut diag = Diag::error(
                        tir.span(r),
                        DiagCode::MutableAliasingViolation,
                        format!(
                            "cannot borrow {} as mutable more than once in the same call",
                            name
                        ),
                    );
                    diag = diag.with_note(Some(occurrences[0]), "first mutable borrow here");
                    diag = diag.with_note(Some(occurrences[1]), "second mutable borrow here");
                    diag = diag.with_help("a value can have one mutable borrow OR many immutable borrows in a call, never both (Rule 7)");
                    sink.emit(diag);
                }
                // (2) inout ∩ borrowed (a view arg counts as an
                // immutable borrow of its root, E4).
                if borrowed.contains(owner) || view_borrowed.contains(owner) {
                    sink.emit(
                        Diag::error(
                            tir.span(r),
                            DiagCode::MutableAliasingViolation,
                            format!(
                                "cannot borrow {} as immutable while it is mutably borrowed in the same call",
                                name
                            ),
                        )
                        .with_help("a value can have one mutable borrow OR many immutable borrows in a call, never both (Rule 7)"),
                    );
                }
                // (3) inout ∩ moved.
                if moved.contains(owner) {
                    sink.emit(
                        Diag::error(
                            tir.span(r),
                            DiagCode::MutableAliasingViolation,
                            format!(
                                "cannot move {} while it is mutably borrowed in the same call",
                                name
                            ),
                        )
                        .with_help("a value can have one mutable borrow OR many immutable borrows in a call, never both (Rule 7)"),
                    );
                }
            }
            // Overlap — same owner borrowed AND moved in one call.
            for owner in borrowed.iter().filter(|o| moved.contains(o)) {
                let name = owner_name_for_diag(*owner, tir, pool);

                // Find spans of the conflicting arguments for this owner
                let mut borrow_span = None;
                let mut move_span = None;
                for (i, arg) in view.args.iter().enumerate() {
                    if !needs_tracking(tir.inst(*arg).ty, pool) {
                        continue;
                    }
                    let mode = view.modes.get(i).copied().unwrap_or(ParamMode::Borrow);
                    // P6' (mirrors the Rule-7 partition above): a
                    // view re-borrowed into a `str` arg via ViewAsStr
                    // borrows the view's ROOT owner — look through the
                    // conversion or the "borrowed here" note is lost.
                    let arg_owner =
                        if mode == ParamMode::Borrow && tir.inst(*arg).tag == TirTag::ViewAsStr {
                            projection_root(own, tir, pool, *arg)
                                .unwrap_or_else(|| underlying_owner(own, *arg))
                        } else {
                            underlying_owner(own, *arg)
                        };
                    if arg_owner == *owner {
                        match mode {
                            ParamMode::Borrow => borrow_span = Some(tir.span(*arg)),
                            ParamMode::Move => move_span = Some(tir.span(*arg)),
                            // inout overlaps are reported as E0032 above —
                            // never as the "moved here" half of E0031.
                            ParamMode::Inout => {}
                        }
                    }
                }

                let mut diag = Diag::error(
                    tir.span(r),
                    DiagCode::MoveWhileBorrowedInCall,
                    format!("cannot move {} while it is borrowed in the same call", name),
                );
                if let Some(b_span) = borrow_span {
                    diag = diag.with_note(Some(b_span), "borrowed here");
                }
                if let Some(m_span) = move_span {
                    diag = diag.with_note(Some(m_span), "moved here");
                }
                diag = diag.with_help("borrows are live for the whole call; pass by `move` on a separate statement, or borrow in both positions");
                sink.emit(diag);
            }

            // P2 freeze (final spec §3.2): a view argument keeps its
            // root owner live for the whole call, so a `move` of the
            // same owner in the same call is a freeze violation. The
            // projection's own last use may be this very call — the
            // borrow is call-bounded (E4) but the move is not.
            let mut view_move_overlap: Vec<Owner> = view_borrowed
                .iter()
                .filter(|o| moved.contains(o))
                .copied()
                .collect();
            view_move_overlap.sort_by_key(owner_sort_key);
            for owner in view_move_overlap {
                // E0020/E0021 already cover owners that are `Moved` or
                // `Borrowed` — don't double-report (mirrors
                // check_source_projected's "move" suppression).
                if matches!(
                    own.states.get(&owner),
                    Some(OwnerState::Moved { .. }) | Some(OwnerState::Borrowed)
                ) {
                    continue;
                }
                let name = owner_name_for_diag(owner, tir, pool);
                let mut diag = Diag::error(
                    tir.span(r),
                    DiagCode::SourceProjected,
                    format!("cannot move {name} while a slice of it is live"),
                );
                // Note the view arg that keeps the owner live.
                for arg in &view.args {
                    if pool.is_view(tir.inst(*arg).ty)
                        && projection_root(own, tir, pool, *arg) == Some(owner)
                    {
                        diag = diag.with_note(Some(tir.span(*arg)), "slice passed here");
                        break;
                    }
                }
                sink.emit(diag);
            }

            // Phase 3 — commit the moves.
            for (i, arg) in view.args.iter().enumerate() {
                let mode = view.modes.get(i).copied().unwrap_or(ParamMode::Borrow);
                let arg_ty = tir.inst(*arg).ty;
                // E2 (final spec §3.3): slices cannot be passed to
                // `move` parameters. Backstop — sema rejects `move` on
                // view-typed parameters already.
                if mode == ParamMode::Move && pool.is_view(arg_ty) {
                    sink.emit(
                        Diag::error(
                            tir.span(r),
                            DiagCode::ViewEscape,
                            "cannot pass a slice to a `move` parameter",
                        )
                        .with_help("slices are non-escaping; pass by default (borrow), or take an owned `str` parameter"),
                    );
                    continue;
                }
                if mode == ParamMode::Move && needs_tracking(arg_ty, pool) {
                    let owner = underlying_owner(own, *arg);
                    // P2 freeze (final spec §3.2) — unless the view
                    // partition above already reported this owner.
                    if !view_borrowed.contains(&owner) {
                        check_source_projected(
                            tir,
                            pool,
                            own,
                            sink,
                            owner,
                            tir.span(r),
                            "move",
                            consumed_binding_name(tir, *arg),
                        );
                    }
                    let consumed_name = consumed_binding_name(tir, *arg);
                    consume_for_assignment(
                        tir,
                        pool,
                        own,
                        sink,
                        *arg,
                        tir.span(r),
                        consumed_name,
                        r,
                    );
                }
            }

            // `inout` args need no ownership transition: the callee only
            // borrows the slot, and the binding keeps its pre-call owner.
            // The stale-triple hazard (callee realloc'd/replaced the
            // buffer) is handled in CODEGEN, where named-binding Frees
            // emit the binding's CURRENT `StrLocals` instead of the
            // producing inst's cached repr — the same pattern
            // `free_on_reassign` already used.
        }
        // ---- Aliasing read ----
        // `Var` is a non-consuming read. Record which SSA value it
        // currently aliases so a later use-after-move diagnostic can
        // walk back to the root owner. Reads of `Borrowed` owners are
        // fine (Rule 2 — borrowed parameters can be freely read).
        TirTag::Var => {
            let name = match inst.data {
                TirData::Var(n) => n,
                _ => unreachable!("Var must carry TirData::Var"),
            };
            if let Some(&owner) = own.current_owner.get(&name) {
                if needs_tracking(inst.ty, pool) {
                    // Any read counts as "used" for dead-store purposes,
                    // even if it ultimately fires E0020 — once the
                    // programmer's code looked at the value, they
                    // didn't ignore it. Clear by NAME, not by current-owner
                    // key: a reseat inside a branch (Assign) is discarded
                    // by the branch merge, so the pending entry can survive
                    // under a branch-local owner key while the binding
                    // itself is provably read afterwards.
                    own.pending_dead_store.retain(|_, (n, _, _)| *n != name);
                    own.origin.insert(r, Some(owner));
                    // Snapshot owner-at-read so the post-walk
                    // `collect_last_uses` anchors the last-use Free to the
                    // owner that was live *at this read*, not whatever
                    // `current_owner[name]` happens to be at function exit
                    // (which would route pre-rebind reads to the post-
                    // rebind owner — wrong target, double-free).
                    own.owner_at_read.insert(r, owner);
                } else if pool.is_view(inst.ty) {
                    // P4 lift (final spec §3.2): record the read for
                    // `collect_last_uses`; when it is the projection's
                    // precomputed last use (and not loop-deferred), the
                    // projection dies at the end of this statement.
                    own.owner_at_read.insert(r, owner);
                    if let Owner::Inst(vi) = owner
                        && !own.view_defer_loop.contains_key(&vi)
                        && own.view_last_use.get(&vi) == Some(&r)
                    {
                        own.pending_dying.push(owner);
                    }
                }
            }
        }
        // ---- Everything else: recurse on operands so nested
        // ---- producers/aliases are still observed.
        _ => {
            recurse_operands(tir, pool, own, sink, sidecar, r);
        }
    }
}

pub(crate) fn recurse_operands(
    tir: &Tir,
    pool: &InternPool,
    own: &mut Ownership,
    sink: &mut DiagSink,
    sidecar: &mut FunctionSidecar,
    r: TirRef,
) {
    let inst = *tir.inst(r);
    match inst.data {
        TirData::UnOp(o) => {
            visit_expr(tir, pool, own, sink, sidecar, o);
            if needs_tracking(tir.inst(o).ty, pool) {
                check_use_moved(tir, pool, own, sink, o, tir.span(o));
            }
        }
        TirData::BinOp { lhs, rhs } => {
            visit_expr(tir, pool, own, sink, sidecar, lhs);
            if needs_tracking(tir.inst(lhs).ty, pool) {
                check_use_moved(tir, pool, own, sink, lhs, tir.span(lhs));
            }
            visit_expr(tir, pool, own, sink, sidecar, rhs);
            if needs_tracking(tir.inst(rhs).ty, pool) {
                check_use_moved(tir, pool, own, sink, rhs, tir.span(rhs));
            }
        }
        TirData::Slice { base, start, end } => {
            // M8.4: slicing is a non-consuming read of the base
            // (final spec §3.2 P1); slicing a moved `str` is a
            // use-after-move like any other read. Bounds are ints.
            visit_expr(tir, pool, own, sink, sidecar, base);
            if needs_tracking(tir.inst(base).ty, pool) {
                check_use_moved(tir, pool, own, sink, base, tir.span(base));
            }
            for bound in [start, end].into_iter().flatten() {
                visit_expr(tir, pool, own, sink, sidecar, bound);
                if needs_tracking(tir.inst(bound).ty, pool) {
                    check_use_moved(tir, pool, own, sink, bound, tir.span(bound));
                }
            }
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
