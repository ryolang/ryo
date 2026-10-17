//! Flow-block emission driver: the backend lowers a function
//! body by walking its canonical Flow IR in DFS pre-order from the entry —
//! one
//! pre-created Cranelift block per Flow block, per-step evaluation
//! through the `eval_inst` family, and an exhaustive terminator
//! handler per block. The TIR statement lists are never traversed to
//! discover executable branches, scopes, or cleanup paths; TIR remains
//! readable for spans, types, and operation payloads.
//!
//! The construct discipline the driver replicates from the old
//! recursive emitters (each now split into a record + prologues +
//! terminator handler):
//!
//! * If-cascades: the cascade record's marks are taken when the
//!   cascade-entry block is seen (before its condition steps). Arm
//!   blocks restore the slot tables to those marks on entry (sibling
//!   arms never observe each other's bindings), seed their
//!   condition's true-polarity facts, and fire block-start drops.
//!   Elif condition blocks and the else arm re-baseline the range
//!   facts to the pre-cascade state and seed every earlier
//!   condition's false polarity. The join restores everything and —
//!   for an else-less cascade whose written arms all terminated —
//!   seeds all negations (the `if n <= 1: return n` fibonacci shape).
//! * Loops: the while header kills body-written facts BEFORE its
//!   condition steps (back-edge rule). The body prologue takes the
//!   post-restore marks and seeds the condition's true polarity; the
//!   exit restores them. `for` binds the induction counter to the
//!   loop variable at the body and restores only that binding at the
//!   exit (body writes to other bindings persist — the legacy shape).
//! * Matches: the Switch handler lowers only the dispatch (scrutinee
//!   home + routing); arm blocks carry their payload drops and
//!   binding moves in their prologue, and their fallthrough Goto
//!   writes the result slot and jumps to the join. The join caches
//!   the result value on the match instruction.
//! * Sealing: a block seals once every Flow predecessor's terminator
//!   has been emitted (loop headers therefore wait for their
//!   backedge); unreachable no-predecessor blocks self-seal after
//!   their own terminator, and any block still unsealed after the
//!   walk (a tail-called arm never emits its Goto edge) is sealed in
//!   the final sweep.

use super::{
    BlockPrologue, Codegen, ForRecord, FunctionContext, IfRecord, LoopRecord, SlotMarks,
    WhileRecord, is_statement_step,
};
use cranelift::codegen::ir::{BlockArg, MemFlagsData};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::flow::{BlockId, Terminator as FlowTerm};
use ryo_core::tir::{TirData, TirRef, TirTag};

impl<M: Module> Codegen<M> {
    // ---------- slot-table mark helpers ----------

    /// Capture the four binding slot tables' undo-log lengths.
    pub(crate) fn take_slot_marks(ctx: &FunctionContext<'_, M>) -> SlotMarks {
        SlotMarks {
            locals: ctx.locals_undo.len(),
            fat_locals: ctx.fat_locals_undo.len(),
            view_locals: ctx.view_locals_undo.len(),
            struct_locals: ctx.struct_locals_undo.len(),
        }
    }

    /// Restore the four binding slot tables to previously taken marks.
    fn restore_slot_marks(ctx: &mut FunctionContext<'_, M>, marks: SlotMarks) {
        Self::restore_slots(&mut ctx.locals, &mut ctx.locals_undo, marks.locals);
        Self::restore_slots(
            &mut ctx.fat_locals,
            &mut ctx.fat_locals_undo,
            marks.fat_locals,
        );
        Self::restore_slots(
            &mut ctx.view_locals,
            &mut ctx.view_locals_undo,
            marks.view_locals,
        );
        Self::restore_slots(
            &mut ctx.struct_locals,
            &mut ctx.struct_locals_undo,
            marks.struct_locals,
        );
    }

    // ---------- the driver ----------

    /// Lower the function body by walking the Flow IR in DFS pre-order
    /// from the entry (see `emission_order` for why). Called with the
    /// entry block current, after the param preamble, the hosted-main
    /// shim, and literal hoisting.
    pub(crate) fn emit_flow_body(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        entry_block: Block,
    ) -> Result<(), String> {
        let flow = ctx.flow;
        let block_count = flow.blocks.len();
        debug_assert_eq!(
            flow.entry.0 as usize, 0,
            "the entry Flow block is block 0 (construction order)"
        );

        // 1. Pre-create one Cranelift block per Flow block. The ONLY
        //    block params: a Flow block whose first step is a
        //    BoolAnd/BoolOr (the short-circuit join) gets one I8
        //    param — the merged boolean.
        let mut mapped: Vec<Block> = Vec::with_capacity(block_count);
        mapped.push(entry_block);
        for _ in 1..block_count {
            mapped.push(builder.create_block());
        }
        let mut bool_join = vec![false; block_count];
        for b in flow.forward_order() {
            let i = b.0 as usize;
            if let Some(first) = flow.block_steps(b).first()
                && matches!(
                    ctx.tir.inst(first.inst).tag,
                    TirTag::BoolAnd | TirTag::BoolOr
                )
            {
                bool_join[i] = true;
                builder.append_block_param(mapped[i], types::I8);
            }
        }
        ctx.flow_blocks = mapped;
        ctx.flow_sealed = vec![false; block_count];
        ctx.flow_sealed[0] = true; // entry is sealed up front
        ctx.flow_preds_emitted = vec![0; block_count];
        #[cfg(debug_assertions)]
        {
            ctx.flow_visit_state = vec![0; block_count];
        }
        ctx.bool_join = bool_join;
        Self::build_flow_maps(ctx);

        // 2. Walk the blocks in DFS pre-order from the entry (see
        //    `emission_order`): each region (cascade arms, loop bodies,
        //    match arms) completes before the continuation that
        //    restores its scope tables, exactly like the old recursive
        //    emitters. Cranelift sealing still waits for every Flow
        //    predecessor (the count rule below), so a continuation
        //    reached early is simply unsealed until its remaining
        //    preds emit.
        //
        //    The id-ordered tail holds the blocks deferred above
        //    (bool-join blocks — no table effects) plus the blocks
        //    unreachable from the entry (dead continuations after
        //    returns and diverging calls): their steps still emit —
        //    mirroring the old emitter, which kept lowering after a
        //    noreturn call — but unreachable blocks skip their
        //    prologues and block-start drops (a dead region's scope
        //    restores and arm drops are meaningless and could free
        //    live buffers). The flow's own reachability flag (set at
        //    build) tells the two apart.
        let order = Self::emission_order(ctx);
        for &b in &order {
            // The flow's own reachability pass decides prologue/drop
            // emission: deferred joins are reachable (their prologues
            // must run); dead continuations are not (their scope
            // restores and arm drops are meaningless and could free
            // live buffers — see `emit_flow_block`).
            let reachable = !flow.blocks[b.0 as usize].unreachable;
            Self::emit_flow_block(builder, ctx, b, reachable)?;
        }

        // 3. Seal anything still unsealed: a tail-called arm never
        //    emits its Goto edge, so its join's predecessor count never
        //    completes. All actual Cranelift edges are in by now.
        for i in 0..block_count {
            if !ctx.flow_sealed[i] {
                let cb = ctx.flow_blocks[i];
                builder.seal_block(cb);
                ctx.flow_sealed[i] = true;
            }
        }
        Ok(())
    }

    /// Build the per-block static maps the driver consults: the
    /// region-entry prologue per block (from the recorded cascade /
    /// switch / loop structure — never reconstructed from TIR
    /// position), the bool-join flags, and the tail-candidate flags.
    fn build_flow_maps(ctx: &mut FunctionContext<'_, M>) {
        let flow = ctx.flow;
        let block_count = flow.blocks.len();
        let mut prologue = vec![BlockPrologue::None; block_count];
        let mut match_join: Vec<Option<TirRef>> = vec![None; block_count];
        let mut match_fallthrough: Vec<Option<(TirRef, usize)>> = vec![None; block_count];
        let mut match_region_seen = vec![0u32; block_count];
        let mut match_region_stamp = 0u32;
        let mut if_arm_owner: Vec<Option<TirRef>> = vec![None; block_count];
        for b in flow.forward_order() {
            match flow.term(b) {
                FlowTerm::Branch { .. } => {
                    if let Some(if_inst) = flow.branch_if(b) {
                        let arms = flow.branch_arm_blocks(if_inst);
                        prologue[arms.then_block.0 as usize] = BlockPrologue::IfThen { if_inst };
                        if_arm_owner[arms.then_block.0 as usize] = Some(if_inst);
                        for (idx, &eb) in arms.elif_blocks.iter().enumerate() {
                            prologue[eb.0 as usize] = BlockPrologue::IfElifCond { if_inst };
                            if let FlowTerm::Branch { then_, .. } = flow.term(eb) {
                                prologue[then_.0 as usize] =
                                    BlockPrologue::IfElifBody { if_inst, idx };
                                if_arm_owner[then_.0 as usize] = Some(if_inst);
                            }
                        }
                        prologue[arms.else_block.0 as usize] = BlockPrologue::IfElse { if_inst };
                        if_arm_owner[arms.else_block.0 as usize] = Some(if_inst);
                        prologue[arms.join.0 as usize] = BlockPrologue::IfJoin { if_inst };
                    }
                }
                FlowTerm::Switch {
                    match_inst,
                    arms,
                    join,
                    ..
                } => {
                    let arm_blocks: Vec<BlockId> =
                        arms.as_range().map(|j| BlockId(flow.extra[j])).collect();
                    for (k, &ab) in arm_blocks.iter().enumerate() {
                        prologue[ab.0 as usize] = BlockPrologue::MatchArm {
                            match_inst: *match_inst,
                            arm: k,
                        };
                    }
                    // The join is keyed in the separate `match_join` map:
                    // it can coincide with a region-entry block (an elif
                    // whose condition is a match, a while-header cond),
                    // and the join handling must run before that
                    // prologue.
                    if let Some(j) = join {
                        match_join[j.0 as usize] = Some(*match_inst);
                        // Each falling-through arm contributes exactly
                        // one `Goto { join }` edge, from the last live
                        // block of its region — for an expression arm
                        // that is a nested value match, the source is
                        // the inner match's join, not the arm block.
                        // Walk each arm's region (never crossing the
                        // join) to find it.
                        for (k, &ab) in arm_blocks.iter().enumerate() {
                            let mut stack = vec![ab];
                            match_region_stamp = match_region_stamp.wrapping_add(1);
                            if match_region_stamp == 0 {
                                match_region_seen.fill(0);
                                match_region_stamp = 1;
                            }
                            while let Some(r) = stack.pop() {
                                let idx = r.0 as usize;
                                if match_region_seen[idx] == match_region_stamp {
                                    continue;
                                }
                                match_region_seen[idx] = match_region_stamp;
                                if let FlowTerm::Goto { target } = flow.term(r)
                                    && *target == *j
                                {
                                    match_fallthrough[r.0 as usize] = Some((*match_inst, k));
                                    continue;
                                }
                                if matches!(
                                    flow.term(r),
                                    FlowTerm::Break { .. }
                                        | FlowTerm::Continue { .. }
                                        | FlowTerm::Backedge { .. }
                                ) {
                                    continue;
                                }
                                for &s in flow.successors(r) {
                                    if s != *j {
                                        stack.push(s);
                                    }
                                }
                            }
                        }
                    }
                }
                FlowTerm::WhileTest {
                    loop_inst,
                    body,
                    exit,
                    ..
                } => {
                    prologue[body.0 as usize] = BlockPrologue::WhileBody {
                        loop_inst: *loop_inst,
                    };
                    prologue[exit.0 as usize] = BlockPrologue::WhileExit {
                        loop_inst: *loop_inst,
                    };
                }
                FlowTerm::ForTest {
                    loop_inst,
                    body,
                    exit,
                    ..
                } => {
                    prologue[body.0 as usize] = BlockPrologue::ForBody {
                        loop_inst: *loop_inst,
                    };
                    prologue[exit.0 as usize] = BlockPrologue::ForExit {
                        loop_inst: *loop_inst,
                    };
                }
                _ => {}
            }
        }
        ctx.block_prologue = prologue;
        ctx.match_join = match_join;
        ctx.match_fallthrough = match_fallthrough;
        ctx.if_arm_owner = if_arm_owner;
        let mut tail_candidate = vec![false; block_count];
        for b in flow.forward_order() {
            let i = b.0 as usize;
            if flow.blocks[i].unreachable {
                continue;
            }
            let steps = flow.block_steps(b);
            let Some(&last) = steps.last() else {
                continue;
            };
            let last_inst = ctx.tir.inst(last.inst);
            if last_inst.tag != TirTag::ExprStmt {
                continue;
            }
            let operand = match last_inst.data {
                TirData::UnOp(o) => o,
                _ => unreachable!("ExprStmt must carry TirData::UnOp"),
            };
            if ctx.tir.inst(operand).tag != TirTag::Call {
                continue;
            }
            let tail = match flow.term(b) {
                // Function fall-off (no explicit `return` — the payload
                // marks an explicit one, whose preceding statement is
                // NOT in tail position).
                FlowTerm::ReturnVoid => flow.term_payload(b).is_none(),
                // If-cascade arm fallthrough: tail only when the whole
                // enclosing if chain sits at the end of its body.
                FlowTerm::Goto { .. } => ctx.if_arm_owner[i]
                    .map(|if_inst| Self::construct_is_last(ctx, if_inst))
                    .unwrap_or(false),
                _ => false,
            };
            tail_candidate[i] = tail;
        }
        ctx.tail_candidate = tail_candidate;
    }

    /// Whether the `IfStmt` is the last statement of its enclosing
    /// body: its join block holds no statement steps and the join's
    /// terminator defers to the body end (fall-off return) or to an
    /// enclosing cascade's arm (recursion). Any other terminator —
    /// another construct's entry, a loop backedge — means a statement
    /// or loop follows, so the if is not last.
    fn construct_is_last(ctx: &FunctionContext<'_, M>, if_inst: TirRef) -> bool {
        let join = ctx.flow.branch_arm_blocks(if_inst).join;
        if ctx
            .flow
            .block_steps(join)
            .iter()
            .any(|s| is_statement_step(ctx.tir.inst(s.inst).tag))
        {
            return false;
        }
        match ctx.flow.term(join) {
            FlowTerm::ReturnVoid => ctx.flow.term_payload(join).is_none(),
            FlowTerm::Goto { .. } => ctx.if_arm_owner[join.0 as usize]
                .map(|outer| Self::construct_is_last(ctx, outer))
                .unwrap_or(false),
            _ => false,
        }
    }

    /// Block-coverage check. Reachability comes from the Flow's own
    /// successor-BFS flag (`FlowBlock::unreachable`, built at
    /// construction), not construction order: every reachable block was
    /// emitted exactly once (visit count 1) with its terminator
    /// dispatched (bit 7 — a tail call counts, its `return_call`
    /// terminates the block); unreachable blocks are visited at most
    /// once (the driver emits or deliberately skips them).
    #[cfg(debug_assertions)]
    pub(crate) fn assert_block_coverage(ctx: &FunctionContext<'_, M>) {
        {
            let bad: Vec<(u32, u8, bool)> = ctx
                .flow
                .forward_order()
                .map(|b| {
                    (
                        b.0,
                        ctx.flow_visit_state[b.0 as usize],
                        !ctx.flow.blocks[b.0 as usize].unreachable,
                    )
                })
                .filter(|&(_, state, reachable)| {
                    let visits = state & 0x7f;
                    let terminated = state & 0x80 != 0;
                    if reachable {
                        visits != 1 || !terminated
                    } else {
                        visits > 1
                    }
                })
                .collect();
            debug_assert!(
                bad.is_empty(),
                "Flow block coverage violated (block, visit-state, reachable): {:?}",
                bad
            );
        }
    }

    /// The block emission order: DFS pre-order from the entry over the
    /// terminator-successor graph (each region's primary edge before
    /// its continuation), then the remaining blocks in id order.
    ///
    /// Goto edges are not chased: `close_region` is the only Goto
    /// producer, so every Goto target is a join (if/match arm
    /// fallthrough). Chasing a fallthrough would schedule the join
    /// after the first arm's subtree but before its sibling arms, and
    /// the join's scope restore (an undo-log pop) is irreversible — a
    /// coarser restore that runs first destroys the entries a finer
    /// one needs. Instead every table-affected continuation is pushed
    /// by its own construct: the Switch pushes its arms + join, the
    /// cascade-entry Branch (per `branch_if`) pushes then/else + the
    /// cascade join, and WhileTest/ForTest push body + exit. The
    /// id-ordered tail then holds only the blocks with no table
    /// effects (bool-join blocks) and the unreachable ones
    /// (construction order is join-after-arms, which keeps each
    /// region complete before its continuation — exactly like the old
    /// recursive emitters). Loop exits need no deferral: their
    /// WhileTest/ForTest pushes them right after the body subtree,
    /// which is why raw construction order alone was not sufficient
    /// (loop exits are allocated before the body's nested blocks).
    fn emission_order(ctx: &FunctionContext<'_, M>) -> Vec<ryo_core::flow::BlockId> {
        let flow = ctx.flow;
        let block_count = flow.blocks.len();
        let mut visited = vec![false; block_count];
        let mut order = Vec::with_capacity(block_count);
        let mut stack: Vec<ryo_core::flow::BlockId> = vec![flow.entry];
        while let Some(b) = stack.pop() {
            if std::mem::replace(&mut visited[b.0 as usize], true) {
                continue;
            }
            order.push(b);
            // Push successors in reverse so the primary edge pops
            // (and emits) first; continuations (else, loop exit, join)
            // were pushed earlier and pop after the region.
            match *flow.term(b) {
                // Goto targets are joins (`close_region` is the only
                // Goto producer): chasing a fallthrough would schedule
                // the join after the first arm's subtree but before
                // its sibling arms, whose bindings the join's scope
                // restore would then wipe. Defer joins to the
                // id-ordered tail instead.
                FlowTerm::Goto { .. } => {}
                FlowTerm::Branch { then_, else_, .. } => {
                    // A cascade-entry branch also pushes its join: the
                    // join is reached only via arm-fallthrough Gotos
                    // (suppressed above), and its scope restore must
                    // run right after the whole cascade — not in the
                    // deferred tail, where an enclosing loop exit's
                    // restore would already have popped the bindings
                    // its steps need (undo-log pops are irreversible).
                    // Elif/bool-op branches carry no payload and push
                    // nothing extra.
                    if let Some(if_inst) = flow.branch_if(b) {
                        stack.push(flow.branch_arm_blocks(if_inst).join);
                    }
                    stack.push(else_);
                    stack.push(then_);
                }
                FlowTerm::Switch { arms, join, .. } => {
                    if let Some(j) = join {
                        stack.push(j);
                    }
                    for i in arms.as_range().rev() {
                        stack.push(ryo_core::flow::BlockId(flow.extra[i]));
                    }
                }
                FlowTerm::While { header, .. } | FlowTerm::For { header, .. } => stack.push(header),
                FlowTerm::WhileTest { body, exit, .. } | FlowTerm::ForTest { body, exit, .. } => {
                    stack.push(exit);
                    stack.push(body);
                }
                // Break/Continue/Backedge targets are loop exits/headers,
                // already scheduled by the loop's own WhileTest/ForTest
                // (header) / pre-header push — chasing them here would
                // pop an enclosing continuation (the exit's scope
                // restore) in the middle of a sibling region, exactly
                // like the Goto chase above.
                FlowTerm::Backedge { .. } | FlowTerm::Break { .. } | FlowTerm::Continue { .. } => {}
                FlowTerm::Call { next, .. } => stack.push(next),
                FlowTerm::Return { .. } | FlowTerm::ReturnVoid | FlowTerm::Unreachable => {}
            }
        }
        for b in flow.forward_order() {
            if !visited[b.0 as usize] {
                order.push(b);
            }
        }
        order
    }

    /// Emit one Flow block: region prologue, block-start drops, steps
    /// (with tail/return operand deferral), terminator, sealing.
    /// Unreachable blocks skip the prologue and block-start drops (the
    /// dead region's scope restores and arm drops are meaningless) but
    /// still emit steps and terminators, mirroring the old emitter.
    fn emit_flow_block(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
        reachable: bool,
    ) -> Result<(), String> {
        let flow = ctx.flow;
        let cb = ctx.flow_blocks[b.0 as usize];
        let term = *flow.term(b);
        // Debug-only visit accounting: count the emission (the
        // driver must visit each block at most once).
        #[cfg(debug_assertions)]
        {
            let state = &mut ctx.flow_visit_state[b.0 as usize];
            *state = state.wrapping_add(1) & 0x7f | (*state & 0x80);
        }
        // Per-path target dedup scope (position-mechanical): the once-per-target
        // collapse applies within one block's fires only.
        ctx.freed_targets.clear();

        // Pre-step hooks whose effects must land BEFORE this block's
        // steps: the while header's back-edge fact kills. (Cascade and
        // match records are created by their terminator handlers, after
        // this block's own steps.)
        if reachable {
            Self::pre_step_hook(ctx, b, &term);
        }

        // Switch to the mapped block. The entry block is already
        // current (the param preamble, hosted-main shim, and literal
        // hoisting emit into it) and is unfilled — Cranelift forbids
        // switching away from an unfilled block, so the entry visit
        // skips the switch. Every other visit switches away from the
        // previous block, which its terminator handler just filled.
        if b != flow.entry {
            builder.switch_to_block(cb);
        }
        if reachable {
            // A match join block (its result caching + fact/slot
            // restores) always runs before the region prologue: the
            // join can coincide with another region entry — an elif
            // whose condition is a match holds its Branch in the
            // condition's join block, and a `while match …:` header is
            // the match's join.
            if let Some(match_inst) = ctx.match_join[b.0 as usize] {
                Self::run_match_join(builder, ctx, match_inst)?;
            }
            // The block-start bucket fires AFTER the region prologue for
            // non-arm blocks: conditional dead drops resolve bindings
            // through the slot tables, which the prologue restores to
            // the construct's marks first (firing before the restore
            // would read a sibling region's binding state — e.g. a
            // same-named shadow's uninitialized home). Match-arm blocks
            // fired theirs inside the prologue, right after the restore
            // and before the binding moves — the arm-start position.
            Self::run_block_prologue(builder, ctx, b)?;
            if !matches!(
                ctx.block_prologue[b.0 as usize],
                BlockPrologue::MatchArm { .. }
            ) {
                Self::fire_block_start(builder, ctx, b)?;
            }
        }

        // Steps, deferring a tail-candidate ExprStmt's operand subtree
        // (or a Return's operand subtree) to the handler below so the
        // tail-eligibility cache state matches the old emitter.
        let steps = flow.block_steps(b);
        let (emit_until, deferred_exprstmt) = Self::step_defer_plan(ctx, b, steps, &term);
        for step in &steps[..emit_until] {
            Self::fire_before_step(builder, ctx, step.inst)?;
            Self::emit_step(builder, ctx, step.inst)?;
        }
        let mut tail_terminated = false;
        if let Some(es) = deferred_exprstmt {
            let operand = match ctx.tir.inst(es).data {
                TirData::UnOp(o) => o,
                _ => unreachable!("ExprStmt must carry TirData::UnOp"),
            };
            tail_terminated = Self::emit_expr_stmt_operand(builder, ctx, es, operand, true)?;
            if !tail_terminated {
                // No tail call: the operand evaluated as a plain
                // statement — fire its statement's drop bucket exactly
                // like `emit_step` does (the deferral bypassed the step
                // dispatcher). The tail-call path fires the scope
                // buckets inside `try_emit_tail_call` instead.
                Self::fire_after_statement(builder, ctx, es)?;
            }
        }
        if !tail_terminated {
            Self::emit_terminator(builder, ctx, b, &term)?;
        }
        // Debug-only: the block ends terminated either way — the
        // terminator handler ran, or the tail call's `return_call`
        // terminated the block (the handler is skipped by design).
        #[cfg(debug_assertions)]
        {
            ctx.flow_visit_state[b.0 as usize] |= 0x80;
        }
        Self::seal_after_terminator(builder, ctx, b);
        Ok(())
    }

    /// The deferral plan for a block's steps: `(emit_until,
    /// deferred_exprstmt)`. Steps in `..emit_until` emit eagerly;
    /// a `Some(exprstmt)` hands the operand subtree to
    /// `emit_expr_stmt_operand` with tail position.
    fn step_defer_plan(
        ctx: &FunctionContext<'_, M>,
        b: BlockId,
        steps: &[ryo_core::flow::Step],
        term: &FlowTerm,
    ) -> (usize, Option<TirRef>) {
        let last_stmt = steps
            .iter()
            .rposition(|s| is_statement_step(ctx.tir.inst(s.inst).tag));
        match term {
            FlowTerm::Return { .. } => (last_stmt.map_or(0, |i| i + 1), None),
            // A tail-candidate block (trailing self-call ExprStmt at
            // function fall-off or an if-arm fallthrough): defer its
            // operand subtree (the steps between the previous statement
            // boundary and the ExprStmt) so `try_emit_tail_call` sees
            // the old emitter's eligibility cache state.
            _ if ctx.tail_candidate[b.0 as usize] => {
                let es_idx = steps.len() - 1;
                debug_assert_eq!(
                    ctx.tir.inst(steps[es_idx].inst).tag,
                    TirTag::ExprStmt,
                    "tail-candidate block ends in an ExprStmt step"
                );
                let prev = steps[..es_idx]
                    .iter()
                    .rposition(|s| is_statement_step(ctx.tir.inst(s.inst).tag));
                (prev.map_or(0, |i| i + 1), Some(steps[es_idx].inst))
            }
            _ => (steps.len(), None),
        }
    }

    /// Pre-step construct bookkeeping: the while header's back-edge
    /// fact kills, which must land BEFORE its condition steps.
    /// (Cascade/match records are created by their terminator handlers,
    /// after this block's own steps.)
    fn pre_step_hook(ctx: &mut FunctionContext<'_, M>, _b: BlockId, term: &FlowTerm) {
        if let FlowTerm::WhileTest {
            loop_inst, cond, ..
        } = *term
        {
            let tir = ctx.tir;
            let view = tir.while_loop_view(loop_inst);
            // Back-edge rule: kill facts on bindings the body
            // writes BEFORE the condition steps emit — the
            // condition re-evaluates every iteration, so a fact it
            // consults must hold on every one. The undo-log mark is
            // taken after this kill (at the body prologue), so the
            // post-loop restore keeps these names dead.
            Self::kill_loop_writes(ctx, Some(cond), view.body);
            // Back-edge rule for home provenance: a flag set
            // pre-loop says nothing about a value the body stored
            // on a later iteration.
            Self::invalidate_home_inline_flags(ctx);
        }
    }

    /// Run a match join block's handling: the fact/slot restores (the
    /// if-arm discipline), the merged result caching, and the frees
    /// anchored after the match itself (the owned scrutinee temporary's
    /// whole-value Free — the old arm loop's caller fired them in the
    /// merge block as its post-statement pass, post-merge on every
    /// path). Runs before the block's region prologue when the join
    /// coincides with one.
    fn run_match_join(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        match_inst: TirRef,
    ) -> Result<(), String> {
        let (outer_facts_mark, scope_mark, slot_marks) = {
            let rec = ctx
                .match_records
                .get(&match_inst)
                .expect("match record created at the switch block");
            (rec.outer_facts_mark, rec.scope_mark, rec.slot_marks)
        };
        Self::restore_slot_marks(ctx, slot_marks);
        Self::restore_slots(
            &mut ctx.range_facts,
            &mut ctx.range_facts_undo,
            outer_facts_mark,
        );
        Self::invalidate_home_inline_flags(ctx);
        Self::kill_assigned_since(ctx, scope_mark);
        Self::match_join_value(builder, ctx, match_inst)?;
        Self::fire_payload(builder, ctx, match_inst)?;
        Ok(())
    }

    /// Run the region-entry prologue recorded for block `b`: scope
    /// restores, condition-fact seeding, loop-variable binding, match
    /// arm setup / merge handling.
    fn run_block_prologue(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
    ) -> Result<(), String> {
        match ctx.block_prologue[b.0 as usize] {
            BlockPrologue::None => Ok(()),
            BlockPrologue::IfThen { if_inst } => {
                let (slot_marks, cond) = {
                    let rec = ctx
                        .if_records
                        .get(&if_inst)
                        .expect("cascade record created at the entry block");
                    (rec.slot_marks, rec.negated_conds[0])
                };
                Self::restore_slot_marks(ctx, slot_marks);
                Self::seed_cond_facts(ctx, cond, true);
                Ok(())
            }
            BlockPrologue::IfElifCond { if_inst, .. } => {
                let (outer_facts_mark, scope_mark, slot_marks, negs) = {
                    let rec = ctx
                        .if_records
                        .get(&if_inst)
                        .expect("cascade record created at the entry block");
                    (
                        rec.outer_facts_mark,
                        rec.scope_mark,
                        rec.slot_marks,
                        rec.negated_conds.clone(),
                    )
                };
                Self::restore_slot_marks(ctx, slot_marks);
                // Re-baseline: true-polarity seeds live only inside
                // their own arm. This block is dominated by the FALSE
                // path of every earlier condition — and by nothing
                // else.
                Self::restore_slots(
                    &mut ctx.range_facts,
                    &mut ctx.range_facts_undo,
                    outer_facts_mark,
                );
                for &prev in &negs {
                    Self::seed_cond_facts(ctx, prev, false);
                }
                // The restore above predates every earlier condition's
                // evaluation — an inout call in one killed its binding's
                // fact via the reload path; re-apply every kill since
                // the scope mark.
                Self::kill_assigned_since(ctx, scope_mark);
                Ok(())
            }
            BlockPrologue::IfElifBody { if_inst, idx } => {
                let slot_marks = ctx
                    .if_records
                    .get(&if_inst)
                    .expect("cascade record created at the entry block")
                    .slot_marks;
                // The elif condition from the cascade view (a payload
                // read): the block holding this elif's Branch may be
                // the condition's join block (match/and/or cond), whose
                // `negated_conds` bookkeeping is not positionally
                // aligned with the arm index.
                let tir = ctx.tir;
                let cond = tir
                    .if_stmt_view(if_inst)
                    .elif_branches
                    .iter()
                    .nth(idx)
                    .expect("elif index comes from the cascade")
                    .cond;
                Self::restore_slot_marks(ctx, slot_marks);
                Self::seed_cond_facts(ctx, cond, true);
                Ok(())
            }
            BlockPrologue::IfElse { if_inst } => {
                let (outer_facts_mark, scope_mark, slot_marks, negs) = {
                    let rec = ctx
                        .if_records
                        .get(&if_inst)
                        .expect("cascade record created at the entry block");
                    (
                        rec.outer_facts_mark,
                        rec.scope_mark,
                        rec.slot_marks,
                        rec.negated_conds.clone(),
                    )
                };
                Self::restore_slot_marks(ctx, slot_marks);
                // Same re-baseline as the elif condition blocks,
                // seeded with every condition's FALSE polarity — the
                // else arm is dominated by the all-conditions-false
                // path.
                Self::restore_slots(
                    &mut ctx.range_facts,
                    &mut ctx.range_facts_undo,
                    outer_facts_mark,
                );
                for &cond in &negs {
                    Self::seed_cond_facts(ctx, cond, false);
                }
                Self::kill_assigned_since(ctx, scope_mark);
                Ok(())
            }
            BlockPrologue::IfJoin { if_inst } => {
                let (outer_facts_mark, scope_mark, slot_marks, negs, has_else) = {
                    let rec = ctx
                        .if_records
                        .get(&if_inst)
                        .expect("cascade record created at the entry block");
                    (
                        rec.outer_facts_mark,
                        rec.scope_mark,
                        rec.slot_marks,
                        rec.negated_conds.clone(),
                        rec.has_else,
                    )
                };
                Self::restore_slot_marks(ctx, slot_marks);
                // Range-fact join: arm-body seeds were rolled back by
                // the join restore; a binding assigned in ANY arm loses
                // its fact (predecessors disagree).
                Self::restore_slots(
                    &mut ctx.range_facts,
                    &mut ctx.range_facts_undo,
                    outer_facts_mark,
                );
                // Home-provenance join: arm stores into home slots
                // persist in memory while the table restore reverted
                // the flags — no arm's provenance may survive.
                Self::invalidate_home_inline_flags(ctx);
                // Only when there is no else and every written arm
                // terminated is the merge dominated by the
                // fall-through edge alone — seed all negations there.
                if !has_else && Self::written_arms_terminated(ctx, if_inst) {
                    for &cond in &negs {
                        Self::seed_cond_facts(ctx, cond, false);
                    }
                }
                // Re-apply kills AFTER the fall-through seeding: a
                // condition's inout call wrote through its pointer on
                // EVERY path past it.
                Self::kill_assigned_since(ctx, scope_mark);
                // Frees anchored AFTER the IfStmt itself (values whose
                // last use is the cascade.s condition) fire from the
                // payload bucket at the join — the mechanical
                // fire point for terminator-payload anchors.
                Self::fire_payload(builder, ctx, if_inst)?;
                Ok(())
            }
            BlockPrologue::MatchArm { match_inst, arm } => {
                let slot_marks = ctx
                    .match_records
                    .get(&match_inst)
                    .expect("match record created at the switch block")
                    .slot_marks;
                Self::restore_slot_marks(ctx, slot_marks);
                // Arm-start payload drops fire at their original
                // position — after the slot restore, before the
                // binding moves. The bucket's dead-drop entries never
                // share a block with a match-arm start (dead drops key
                // on if-cascade arms), so this fires the block's whole
                // bucket exactly once.
                Self::fire_block_start(builder, ctx, b)?;
                Self::match_arm_prologue(builder, ctx, match_inst, arm)
            }
            BlockPrologue::WhileBody { loop_inst } => {
                let tir = ctx.tir;
                let view = tir.while_loop_view(loop_inst);
                if !ctx.loop_records.contains_key(&loop_inst) {
                    ctx.loop_records.insert(
                        loop_inst,
                        LoopRecord::While(WhileRecord {
                            pre_facts_mark: ctx.range_facts_undo.len(),
                            scope_mark: ctx.assigned_log.len(),
                            slot_marks: Self::take_slot_marks(ctx),
                        }),
                    );
                    // The condition holds at every body entry (the
                    // header's brif guards it). Assignments inside the
                    // body kill facts in place; the seeds themselves
                    // must NOT survive the loop — the exit block is
                    // also reached on the zero-iteration path.
                    Self::seed_cond_facts(ctx, view.cond, true);
                }
                Ok(())
            }
            BlockPrologue::WhileExit { loop_inst } => {
                let (pre_facts_mark, scope_mark, slot_marks) = match ctx
                    .loop_records
                    .get(&loop_inst)
                    .expect("while record created at the body block")
                {
                    LoopRecord::While(rec) => (rec.pre_facts_mark, rec.scope_mark, rec.slot_marks),
                    LoopRecord::For(_) => unreachable!("while exit of a for loop"),
                };
                Self::restore_slot_marks(ctx, slot_marks);
                Self::restore_slots(
                    &mut ctx.range_facts,
                    &mut ctx.range_facts_undo,
                    pre_facts_mark,
                );
                Self::kill_assigned_since(ctx, scope_mark);
                // The body's home stores persist in memory but the
                // scoped restore reverted the flags — post-loop code
                // must not trust pre-loop (or in-body) provenance.
                Self::invalidate_home_inline_flags(ctx);
                // Frees anchored AFTER the WhileLoop instruction
                // (loop-exit drops) fire from the payload bucket at the
                // exit — the mechanical fire point.
                Self::fire_payload(builder, ctx, loop_inst)?;
                Ok(())
            }
            BlockPrologue::ForBody { loop_inst } => {
                let tir = ctx.tir;
                let view = tir.for_range_view(loop_inst);
                let counter = match ctx
                    .loop_records
                    .get(&loop_inst)
                    .expect("for record created at the pre-header")
                {
                    LoopRecord::For(rec) => rec.counter,
                    LoopRecord::While(_) => unreachable!("for body of a while loop"),
                };
                // Scope the loop variable: bind var_name to the counter
                // Variable. Body writes to OTHER bindings persist past
                // the loop (the legacy shape) — only this one slot is
                // restored, by hand at the exit.
                let shadowed_var = Self::read_slot(&ctx.locals, view.var_name);
                let shadowed_fact = Self::read_slot(&ctx.range_facts, view.var_name);
                Self::write_slot(
                    &mut ctx.locals,
                    &mut ctx.locals_undo,
                    view.var_name,
                    Some(counter),
                );
                // The loop variable is a different quantity than any
                // shadowed outer binding — its fact must not leak onto
                // the counter.
                Self::write_slot(
                    &mut ctx.range_facts,
                    &mut ctx.range_facts_undo,
                    view.var_name,
                    None,
                );
                match ctx.loop_records.get_mut(&loop_inst) {
                    Some(LoopRecord::For(rec)) => {
                        rec.saved_local = shadowed_var;
                        rec.saved_fact = shadowed_fact;
                    }
                    _ => unreachable!("for record checked above"),
                }
                // Back-edge rule (see the while pre-step hook): the
                // bounds were evaluated once pre-loop, but a fact
                // consulted inside the body must hold on every
                // iteration. There is no post-loop restore for these
                // kills — they persist past the loop, as before.
                Self::kill_loop_writes(ctx, None, view.body);
                Self::invalidate_home_inline_flags(ctx);
                Ok(())
            }
            BlockPrologue::ForExit { loop_inst } => {
                let (var_name, saved_local, saved_fact) = match ctx
                    .loop_records
                    .get(&loop_inst)
                    .expect("for record created at the pre-header")
                {
                    LoopRecord::For(rec) => (rec.var_name, rec.saved_local, rec.saved_fact),
                    LoopRecord::While(_) => unreachable!("for exit of a while loop"),
                };
                // The loop variable goes out of scope; restore the
                // shadowed binding (write the saved slot back
                // unconditionally — None clears it).
                Self::write_slot(&mut ctx.locals, &mut ctx.locals_undo, var_name, saved_local);
                Self::write_slot(
                    &mut ctx.range_facts,
                    &mut ctx.range_facts_undo,
                    var_name,
                    saved_fact,
                );
                // The exit is reached from the header on EVERY path
                // (including zero iterations), so neither pre-loop nor
                // last-iteration home provenance holds here.
                Self::invalidate_home_inline_flags(ctx);
                // Frees anchored AFTER the ForRange instruction
                // (loop-exit drops), exactly like the while exit.
                Self::fire_payload(builder, ctx, loop_inst)?;
                Ok(())
            }
        }
    }

    /// Whether every written arm (then + elifs) of the cascade ends
    /// the block — the merge is then dominated by the fall-through
    /// edge alone. Termination mirrors the old emitter's report:
    /// return / bare return / break / continue count; a diverging
    /// noreturn call did NOT (the old `ExprStmt` arm reported None).
    fn written_arms_terminated(ctx: &FunctionContext<'_, M>, if_inst: TirRef) -> bool {
        fn arm_terminated<M: Module>(ctx: &FunctionContext<'_, M>, b: BlockId) -> bool {
            matches!(
                ctx.flow.term(b),
                FlowTerm::Return { .. }
                    | FlowTerm::ReturnVoid
                    | FlowTerm::Break { .. }
                    | FlowTerm::Continue { .. }
            )
        }
        let arms = ctx.flow.branch_arm_blocks(if_inst);
        if !arm_terminated(ctx, arms.then_block) {
            return false;
        }
        arms.elif_blocks.iter().all(|&eb| {
            matches!(ctx.flow.term(eb), FlowTerm::Branch { then_, .. } if arm_terminated(ctx, *then_))
        })
    }

    // ---------- terminator handlers ----------

    fn emit_terminator(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
        term: &FlowTerm,
    ) -> Result<(), String> {
        match *term {
            // The at-terminator bucket (PreTerminator drops and
            // arm-body/return-operand AfterSteps) fires inside every
            // handler at its canonical point; Return/ReturnVoid fire it
            // after their operand evaluation (see term_return), the
            // break/continue arms fire it before the jump, and the
            // remaining handlers have no mid-handler evaluation the
            // frees must follow — fire it up front.
            // Handlers with no lazy materialization after the fire
            // point fire the at-terminator bucket up front; Switch /
            // Call / Goto materialize skipped slot-out producers inside
            // the handler, so their bucket fires after that (see the
            // handlers).
            FlowTerm::While { .. }
            | FlowTerm::For { .. }
            | FlowTerm::WhileTest { .. }
            | FlowTerm::ForTest { .. }
            | FlowTerm::Backedge { .. }
            | FlowTerm::Unreachable => {
                Self::fire_at_terminator(builder, ctx, b)?;
                match *term {
                    FlowTerm::While { header, .. } => {
                        let hb = ctx.flow_blocks[header.0 as usize];
                        builder.ins().jump(hb, &[]);
                        Ok(())
                    }
                    FlowTerm::For { loop_inst, header } => {
                        Self::term_for(builder, ctx, loop_inst, header)
                    }
                    FlowTerm::WhileTest {
                        cond, body, exit, ..
                    } => {
                        let cond_val = Self::eval_inst(builder, ctx, cond)?;
                        let bb = ctx.flow_blocks[body.0 as usize];
                        let eb = ctx.flow_blocks[exit.0 as usize];
                        builder.ins().brif(cond_val, bb, &[], eb, &[]);
                        Ok(())
                    }
                    FlowTerm::ForTest {
                        loop_inst,
                        end,
                        body,
                        exit,
                        ..
                    } => Self::term_for_test(builder, ctx, loop_inst, end, body, exit),
                    FlowTerm::Backedge { to, inc } => Self::term_backedge(builder, ctx, to, inc),
                    FlowTerm::Unreachable => {
                        // Dead continuation (after a return / diverging
                        // call, or a violated-exhaustiveness path): trap
                        // so the block is terminated. Unreachable in
                        // practice — sema has already rejected
                        // break-outside-loop et al.
                        builder.ins().trap(
                            TrapCode::user(1)
                                .expect("user trap code 1 is within Cranelift's encodable range"),
                        );
                        Ok(())
                    }
                    _ => unreachable!("arms handled above"),
                }
            }
            FlowTerm::Branch { cond, then_, else_ } => {
                Self::term_branch(builder, ctx, b, cond, then_, else_)
            }
            FlowTerm::Goto { target } => Self::term_goto(builder, ctx, b, target),
            FlowTerm::Switch { match_inst, .. } => Self::emit_switch(builder, ctx, match_inst),
            FlowTerm::Call { call, .. } => Self::term_call(builder, ctx, b, call),
            FlowTerm::Break { target } => Self::term_break(builder, ctx, b, target),
            FlowTerm::Continue { target } => Self::term_continue(builder, ctx, b, target),
            FlowTerm::Return { value } => Self::term_return(builder, ctx, b, value),
            FlowTerm::ReturnVoid => Self::term_return_void(builder, ctx, b),
        }
    }

    /// Goto: plain jump — except the two value-carrying shapes: a
    /// match arm's fallthrough writes the arm result into the match
    /// result slot before jumping to the join, and a BoolAnd/BoolOr
    /// rhs edge jumps to the short-circuit join with the rhs value
    /// (the join's I8 param). The SOURCE block disambiguates: a match
    /// join's first step could itself be a BoolAnd (the statement
    /// after the match), so the target's first step alone is not
    /// enough.
    fn term_goto(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        from: BlockId,
        target: BlockId,
    ) -> Result<(), String> {
        if let Some((match_inst, arm)) = ctx.match_fallthrough[from.0 as usize] {
            debug_assert_eq!(
                ctx.match_join[target.0 as usize],
                Some(match_inst),
                "arm fallthrough targets its own join"
            );
            return Self::match_arm_fallthrough(builder, ctx, match_inst, arm, from);
        }
        if ctx.bool_join[target.0 as usize] {
            let join_inst = ctx.flow.block_steps(target)[0].inst;
            let rhs = match ctx.tir.inst(join_inst).data {
                TirData::BinOp { rhs, .. } => rhs,
                _ => unreachable!("BoolAnd/BoolOr must carry TirData::BinOp"),
            };
            // Only the rhs block's Goto carries the value; the
            // short-circuit edge arrives via the Branch terminator.
            if ctx.flow.step_block_of(rhs) == Some(from) {
                let v = Self::eval_inst(builder, ctx, rhs)?;
                let jb = ctx.flow_blocks[target.0 as usize];
                builder.ins().jump(jb, &[BlockArg::Value(v)]);
                return Ok(());
            }
        }
        // Plain jump: the block's at-terminator bucket fires before
        // the jump (its steps — including any lazy materialization —
        // are complete).
        Self::fire_at_terminator(builder, ctx, from)?;
        let tb = ctx.flow_blocks[target.0 as usize];
        builder.ins().jump(tb, &[]);
        Ok(())
    }

    /// Branch: `brif` on the cached condition value. A cascade-entry
    /// branch (recorded via `branch_if`) creates the cascade record
    /// with marks taken NOW — after this block's own steps, before any
    /// arm block emits (the old recursive emitter took them when the
    /// IfStmt statement was reached). An elif branch appends its
    /// condition to the record; a BoolAnd/BoolOr split passes the
    /// constant merge value on the edge INTO the short-circuit join.
    fn term_branch(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
        cond: TirRef,
        then_: BlockId,
        else_: BlockId,
    ) -> Result<(), String> {
        let cond_val = Self::eval_inst(builder, ctx, cond)?;
        // At-terminator bucket for the cond block: value-step anchors
        // in the condition (e.g. a last-use read feeding the compare)
        // have no enclosing statement step — the cascade's Branch is
        // the block's terminator — so they fire here, after the
        // condition's steps and before the brif.
        Self::fire_at_terminator(builder, ctx, b)?;
        if let Some(if_inst) = ctx.flow.branch_if(b) {
            let tir = ctx.tir;
            let view = tir.if_stmt_view(if_inst);
            ctx.if_records.insert(
                if_inst,
                IfRecord {
                    outer_facts_mark: ctx.range_facts_undo.len(),
                    scope_mark: ctx.assigned_log.len(),
                    slot_marks: Self::take_slot_marks(ctx),
                    negated_conds: vec![view.cond],
                    has_else: view.else_stmts.is_some(),
                },
            );
        }
        let mut then_args = Vec::new();
        let mut else_args = Vec::new();
        for (target, args) in [(then_, &mut then_args), (else_, &mut else_args)] {
            if ctx.bool_join[target.0 as usize] {
                let join_inst = ctx.flow.block_steps(target)[0].inst;
                let constant = match ctx.tir.inst(join_inst).tag {
                    // `or`: the lhs-true edge merges constant 1.
                    TirTag::BoolOr => 1,
                    // `and`: the lhs-false edge merges constant 0.
                    TirTag::BoolAnd => 0,
                    _ => unreachable!("bool join first step is BoolAnd/BoolOr"),
                };
                args.push(BlockArg::Value(builder.ins().iconst(types::I8, constant)));
            }
        }
        // Elif bookkeeping: this condition's FALSE path dominates the
        // next cascade block, so it joins the negation list after its
        // own branch emits.
        if let BlockPrologue::IfElifCond { if_inst, .. } = ctx.block_prologue[b.0 as usize] {
            ctx.if_records
                .get_mut(&if_inst)
                .expect("cascade record created at the entry block")
                .negated_conds
                .push(cond);
        }
        let tb = ctx.flow_blocks[then_.0 as usize];
        let eb = ctx.flow_blocks[else_.0 as usize];
        builder.ins().brif(cond_val, tb, &then_args, eb, &else_args);
        Ok(())
    }

    /// For pre-header: evaluate the (cached) start bound into the
    /// hidden induction counter and jump to the header. The end bound
    /// is a pre-header step, already emitted.
    fn term_for(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        loop_inst: TirRef,
        header: BlockId,
    ) -> Result<(), String> {
        let tir = ctx.tir;
        let view = tir.for_range_view(loop_inst);
        let start_val = Self::eval_inst(builder, ctx, view.start)?;
        let counter = builder.declare_var(ctx.int_type);
        builder.def_var(counter, start_val);
        ctx.loop_records.insert(
            loop_inst,
            LoopRecord::For(ForRecord {
                counter,
                var_name: view.var_name,
                saved_local: None,
                saved_fact: None,
            }),
        );
        let hb = ctx.flow_blocks[header.0 as usize];
        builder.ins().jump(hb, &[]);
        Ok(())
    }

    /// For header test: compare the induction counter against the
    /// (cached) end bound and branch to body / exit.
    fn term_for_test(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        loop_inst: TirRef,
        end: TirRef,
        body: BlockId,
        exit: BlockId,
    ) -> Result<(), String> {
        let counter = match ctx
            .loop_records
            .get(&loop_inst)
            .expect("for record created at the pre-header")
        {
            LoopRecord::For(rec) => rec.counter,
            LoopRecord::While(_) => unreachable!("for test of a while loop"),
        };
        let i = builder.use_var(counter);
        let end_val = Self::eval_inst(builder, ctx, end)?;
        let cond = builder.ins().icmp(IntCC::SignedLessThan, i, end_val);
        let bb = ctx.flow_blocks[body.0 as usize];
        let eb = ctx.flow_blocks[exit.0 as usize];
        builder.ins().brif(cond, bb, &[], eb, &[]);
        Ok(())
    }

    /// Loop backedge: jump to the header; a for-latch increments the
    /// induction counter on the edge (the shape the old increment
    /// block synthesized). The loop's record is found through the
    /// header block's `ForTest` terminator — exact under nesting,
    /// where a linear record search could find an outer loop's counter.
    fn term_backedge(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        to: BlockId,
        inc: Option<ryo_core::flow::PlaceId>,
    ) -> Result<(), String> {
        if inc.is_some() {
            let loop_inst = match ctx.flow.term(to) {
                FlowTerm::ForTest { loop_inst, .. } => *loop_inst,
                _ => unreachable!("a for backedge targets its ForTest header"),
            };
            let counter = match ctx
                .loop_records
                .get(&loop_inst)
                .expect("for record created at the pre-header")
            {
                LoopRecord::For(rec) => rec.counter,
                LoopRecord::While(_) => unreachable!("for record checked above"),
            };
            let i_current = builder.use_var(counter);
            let one = builder.ins().iconst(ctx.int_type, 1);
            let i_next = builder.ins().iadd(i_current, one);
            builder.def_var(counter, i_next);
        }
        let hb = ctx.flow_blocks[to.0 as usize];
        builder.ins().jump(hb, &[]);
        Ok(())
    }

    /// Break: fire the loop-exit Frees anchored on the Break
    /// instruction (they must land BEFORE the jump — the sweep cannot
    /// run after a terminator), then jump to the recorded exit block.
    fn term_break(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
        target: BlockId,
    ) -> Result<(), String> {
        let payload = ctx
            .flow
            .term_payload(b)
            .expect("break carries its instruction");
        Self::fire_payload(builder, ctx, payload)?;
        Self::fire_at_terminator(builder, ctx, b)?;
        let tb = ctx.flow_blocks[target.0 as usize];
        builder.ins().jump(tb, &[]);
        Ok(())
    }

    /// Continue: fire the anchored Frees (see `term_break`), then jump
    /// to the recorded loop header. A `for` header (ForTest) receives
    /// the induction increment on EVERY incoming edge — the flow
    /// builder routes `continue` straight to the header, so the
    /// increment the backedge synthesizes must be duplicated here (the
    /// old emitter's continue targeted the increment block).
    fn term_continue(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
        target: BlockId,
    ) -> Result<(), String> {
        let payload = ctx
            .flow
            .term_payload(b)
            .expect("continue carries its instruction");
        Self::fire_payload(builder, ctx, payload)?;
        Self::fire_at_terminator(builder, ctx, b)?;
        if let FlowTerm::ForTest { loop_inst, .. } = ctx.flow.term(target) {
            let counter = match ctx
                .loop_records
                .get(loop_inst)
                .expect("for record created at the pre-header")
            {
                LoopRecord::For(rec) => rec.counter,
                LoopRecord::While(_) => unreachable!("for test of a while loop"),
            };
            let i_current = builder.use_var(counter);
            let one = builder.ins().iconst(ctx.int_type, 1);
            let i_next = builder.ins().iadd(i_current, one);
            builder.def_var(counter, i_next);
        }
        let tb = ctx.flow_blocks[target.0 as usize];
        builder.ins().jump(tb, &[]);
        Ok(())
    }

    /// Diverging call (`panic`, …): the call instruction is the
    /// terminator payload, never a step. `eval_inst` emits the call
    /// plus its trap and switches into its own sealed dead block;
    /// terminate that block — the Flow continuation (`next`) is an
    /// unreachable block the driver emits separately.
    fn term_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
        call: TirRef,
    ) -> Result<(), String> {
        let _ = Self::eval_inst(builder, ctx, call)?;
        // The at-terminator bucket fires after the (lazy) call
        // emission: its drops' targets may be the call's own argument
        // buffers, read by the call instruction.
        Self::fire_at_terminator(builder, ctx, b)?;
        builder.ins().trap(
            TrapCode::user(1).expect("user trap code 1 is within Cranelift's encodable range"),
        );
        Ok(())
    }

    /// `return expr`: the operand subtree was deferred to this handler
    /// (the steps after the block's last statement boundary), so the
    /// tail-eligibility cache state matches the old recursive emitter.
    /// A call operand may lower to `return_call`; everything else
    /// evaluates, fires the Return-anchored frees, and returns.
    fn term_return(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
        value: TirRef,
    ) -> Result<(), String> {
        let r = ctx
            .flow
            .term_payload(b)
            .expect("return carries its instruction");
        if ctx.abi.result == super::ValueAbi::Fat {
            let sret = ctx.sret_ptr.expect("fat-returning fn must have sret_ptr");
            let repr = Self::eval_inst_fat(builder, ctx, value)?;
            let (ptr, len, cap) = match repr {
                super::ValueRepr::Str { ptr, len, cap }
                | super::ValueRepr::Bytes { ptr, len, cap } => (ptr, len, cap),
                _ => unreachable!("fat return must produce a fat ValueRepr"),
            };
            builder.ins().store(MemFlagsData::trusted(), ptr, sret, 0);
            builder.ins().store(MemFlagsData::trusted(), len, sret, 8);
            builder.ins().store(MemFlagsData::trusted(), cap, sret, 16);
            Self::fire_payload(builder, ctx, r)?;
            Self::fire_at_terminator(builder, ctx, b)?;
            Self::emit_return(builder, ctx, &[])?;
        } else if ctx.abi.result == super::ValueAbi::Struct {
            Self::emit_struct_return(builder, ctx, r, value)?;
        } else if ctx.abi.result == super::ValueAbi::Enum {
            Self::emit_enum_return(builder, ctx, r, value)?;
        } else {
            // A Return whose operand is a call is a tail context
            // regardless of position. An eligible self-call is emitted
            // as `return_call` (firing the statement's due frees first)
            // and `emit_return` below is skipped — eligibility proved
            // no inout write-back is due and no free is anchored on the
            // call or this Return.
            let tail_emitted = matches!(ctx.tir.inst(value).tag, TirTag::Call)
                && Self::try_emit_tail_call(builder, ctx, value, Some(r), r)?;
            if !tail_emitted {
                let val = Self::eval_inst(builder, ctx, value)?;
                Self::fire_payload(builder, ctx, r)?;
                Self::fire_at_terminator(builder, ctx, b)?;
                Self::emit_return(builder, ctx, &[val])?;
            }
        }
        Ok(())
    }

    /// `return` (void) and the synthesized fall-off. The explicit form
    /// fires its anchored frees; the fall-off is the function-end
    /// return the old emitter synthesized when the body did not
    /// terminate (sema E0036 guarantees it only arises for void/main).
    fn term_return_void(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
    ) -> Result<(), String> {
        match ctx.flow.term_payload(b) {
            Some(r) => {
                // Bare `return` in a void function. If this is `main`,
                // the C ABI demands an int return value.
                if ctx.is_main {
                    let zero = builder.ins().iconst(ctx.int_type, 0);
                    Self::fire_payload(builder, ctx, r)?;
                    Self::fire_at_terminator(builder, ctx, b)?;
                    Self::emit_return(builder, ctx, &[zero])?;
                } else {
                    Self::fire_payload(builder, ctx, r)?;
                    Self::fire_at_terminator(builder, ctx, b)?;
                    Self::emit_return(builder, ctx, &[])?;
                }
            }
            None => {
                if ctx.is_main {
                    let zero = builder.ins().iconst(ctx.int_type, 0);
                    Self::emit_return(builder, ctx, &[zero])?;
                } else if ctx.has_sret || ctx.tir.return_type == ctx.pool.void() {
                    Self::emit_return(builder, ctx, &[])?;
                } else {
                    let zero = builder.ins().iconst(ctx.int_type, 0);
                    Self::emit_return(builder, ctx, &[zero])?;
                }
            }
        }
        Ok(())
    }

    /// Post-terminator sealing: every Flow successor's predecessor
    /// count advances; a block seals once ALL its Flow predecessors
    /// have emitted (loop headers therefore wait for their backedge —
    /// Cranelift dominance is preserved). Unreachable
    /// no-predecessor blocks self-seal: live blocks never target them,
    /// so they can gain no further predecessors.
    fn seal_after_terminator(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        b: BlockId,
    ) {
        for &succ in ctx.flow.successors(b) {
            let i = succ.0 as usize;
            ctx.flow_preds_emitted[i] += 1;
            if !ctx.flow_sealed[i]
                && ctx.flow_preds_emitted[i] as usize == ctx.flow.predecessors(succ).len()
            {
                let cb = ctx.flow_blocks[i];
                builder.seal_block(cb);
                ctx.flow_sealed[i] = true;
            }
        }
        let i = b.0 as usize;
        if !ctx.flow_sealed[i]
            && ctx.flow.blocks[i].unreachable
            && ctx.flow.predecessors(b).is_empty()
        {
            let cb = ctx.flow_blocks[i];
            builder.seal_block(cb);
            ctx.flow_sealed[i] = true;
        }
    }
}
