//! Control-flow statement codegen (`if`/`while`/`for range`) — split
//! from `mod.rs`; see module docs there.

use super::{Codegen, FunctionContext, LoopContext, Terminator};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::TirRef;

impl<M: Module> Codegen<M> {
    pub(crate) fn generate_if_stmt(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let view = ctx.tir.if_stmt_view(r);
        let outer_facts_mark = ctx.range_facts_undo.len();
        let scope_mark = ctx.assigned_log.len();
        // Conditions whose FALSE path dominates each subsequent block
        // (elif cond blocks, the else arm, and — when every written arm
        // terminates — the merge block).
        let mut negated_conds: Vec<TirRef> = vec![view.cond];
        let merge_block = builder.create_block();

        // Pull the BranchId assignments allocated by the ownership
        // pass for this if. Default-empty if the sidecar has no entry
        // (e.g. an if with no Move-typed bindings live across it):
        // unconditional Frees still fire because their `branch` is
        // `None`, and there are no branch-gated entries to gate.
        let branch_ids = ctx.sidecar.if_branches[r.index()]
            .clone()
            .unwrap_or_default();

        let cond_val = Self::eval_inst(builder, ctx, view.cond)?;
        let then_block = builder.create_block();

        let elif_count = view.elif_branches.len();
        let has_else = view.else_stmts.is_some();
        // An else-less if whose arms conditionally reseated a
        // binding needs a REAL fall-through block so the arm-gated
        // DeadDrops have somewhere to fire.
        let needs_fallthrough_block = !has_else
            && ctx
                .sidecar
                .conditional_dead_drops
                .iter()
                .any(|d| d.if_stmt == r);
        let capacity = elif_count + usize::from(has_else || needs_fallthrough_block);
        let mut next_blocks: Vec<Block> = Vec::with_capacity(capacity);
        for _ in 0..elif_count {
            next_blocks.push(builder.create_block());
        }
        let else_or_merge = if has_else || needs_fallthrough_block {
            let eb = builder.create_block();
            next_blocks.push(eb);
            eb
        } else {
            merge_block
        };

        let first_fallthrough = next_blocks.first().copied().unwrap_or(else_or_merge);

        builder
            .ins()
            .brif(cond_val, then_block, &[], first_fallthrough, &[]);

        builder.seal_block(then_block);
        builder.switch_to_block(then_block);
        Self::seed_cond_facts(ctx, view.cond, true);
        // Manual push/pop (not RAII) — `?` propagation interacts
        // poorly with a scope-guard holding `&mut ctx`. We pop on
        // both Ok and Err paths by binding the result first.
        ctx.branch_stack.push(branch_ids.then_branch);
        Self::emit_conditional_dead_drops(builder, ctx, r, branch_ids.then_branch)?;
        let then_term_result = Self::emit_scoped_body(builder, ctx, &view.then_stmts);
        ctx.branch_stack.pop();
        let then_term = then_term_result?;
        if then_term == Terminator::None {
            builder.ins().jump(merge_block, &[]);
        }

        // Two separate questions the old bool conflated —
        // `all_terminated` (every arm ends the block, so the merge
        // block is unreachable) and `all_return` (every arm actually
        // returns, which is what the if reports to its caller).
        let mut all_terminated = then_term != Terminator::None;
        let mut all_return = then_term == Terminator::Return;
        for (i, elif) in view.elif_branches.iter().enumerate() {
            let elif_cond_block = next_blocks[i];
            builder.seal_block(elif_cond_block);
            builder.switch_to_block(elif_cond_block);
            // Re-baseline: true-polarity seeds live only inside their
            // own arm (emit_scoped_body's restore would resurrect them).
            // This block is dominated by the FALSE path of every
            // earlier condition — and by nothing else.
            Self::restore_slots(
                &mut ctx.range_facts,
                &mut ctx.range_facts_undo,
                outer_facts_mark,
            );
            for &prev in &negated_conds {
                Self::seed_cond_facts(ctx, prev, false);
            }
            // The restore above predates every earlier condition's
            // evaluation — an inout call in one of them killed its
            // binding's fact via the reload path, and the re-baseline
            // (or a negation seed on the same name) would resurrect it.
            // Re-apply every kill logged since scope_mark.
            Self::kill_assigned_since(ctx, scope_mark);

            let elif_cond_val = Self::eval_inst(builder, ctx, elif.cond)?;
            let elif_body_block = builder.create_block();

            let elif_fallthrough = if i + 1 < next_blocks.len() {
                next_blocks[i + 1]
            } else {
                merge_block
            };

            builder
                .ins()
                .brif(elif_cond_val, elif_body_block, &[], elif_fallthrough, &[]);

            builder.seal_block(elif_body_block);
            builder.switch_to_block(elif_body_block);
            Self::seed_cond_facts(ctx, elif.cond, true);
            let elif_branch_id = branch_ids.elif_branches.get(i).copied().unwrap_or_default();
            ctx.branch_stack.push(elif_branch_id);
            Self::emit_conditional_dead_drops(builder, ctx, r, elif_branch_id)?;
            let elif_term_result = Self::emit_scoped_body(builder, ctx, &elif.body);
            ctx.branch_stack.pop();
            let elif_term = elif_term_result?;
            if elif_term == Terminator::None {
                builder.ins().jump(merge_block, &[]);
            }
            all_terminated = all_terminated && elif_term != Terminator::None;
            all_return = all_return && elif_term == Terminator::Return;
            negated_conds.push(elif.cond);
        }

        // Whether every written arm (then + elifs) ends the block. With
        // no else arm, the merge is then reachable ONLY via the
        // fall-through edge, where every condition is provably false —
        // the `if n <= 1: return n` fibonacci shape.
        let written_arms_terminated = all_terminated;

        if let Some(else_stmts) = &view.else_stmts {
            builder.seal_block(else_or_merge);
            builder.switch_to_block(else_or_merge);
            // Same re-baseline as the elif cond blocks, seeded with
            // every condition's FALSE polarity — the else arm is
            // dominated by the all-conditions-false path.
            Self::restore_slots(
                &mut ctx.range_facts,
                &mut ctx.range_facts_undo,
                outer_facts_mark,
            );
            for &cond in &negated_conds {
                Self::seed_cond_facts(ctx, cond, false);
            }
            // Same re-application of cond-eval kills as the elif cond
            // blocks above.
            Self::kill_assigned_since(ctx, scope_mark);
            let else_branch_id = branch_ids.else_branch.unwrap_or_default();
            ctx.branch_stack.push(else_branch_id);
            Self::emit_conditional_dead_drops(builder, ctx, r, else_branch_id)?;
            let else_term_result = Self::emit_scoped_body(builder, ctx, else_stmts);
            ctx.branch_stack.pop();
            let else_term = else_term_result?;
            if else_term == Terminator::None {
                builder.ins().jump(merge_block, &[]);
            }
            all_terminated = all_terminated && else_term != Terminator::None;
            all_return = all_return && else_term == Terminator::Return;
        } else if needs_fallthrough_block {
            // The synthetic fall-through — emit the arm-gated
            // DeadDrops for the paths where no arm reseated the binding.
            builder.seal_block(else_or_merge);
            builder.switch_to_block(else_or_merge);
            let fallthrough_id = branch_ids.else_branch.unwrap_or_default();
            ctx.branch_stack.push(fallthrough_id);
            Self::emit_conditional_dead_drops(builder, ctx, r, fallthrough_id)?;
            ctx.branch_stack.pop();
            builder.ins().jump(merge_block, &[]);
            all_terminated = false;
            all_return = false;
        } else {
            all_terminated = false;
            all_return = false;
        }

        builder.seal_block(merge_block);
        if !all_terminated {
            builder.switch_to_block(merge_block);
        }

        // Range-fact join. Arm-body seeds were already rolled back by
        // emit_scoped_body; cond-block seeds are rolled back here by
        // restoring the pre-if facts. A binding assigned in ANY arm loses
        // its fact (predecessors disagree). Only when there is no else
        // and every written arm terminated is the merge dominated by
        // the fall-through edge alone — seed all negations there.
        Self::restore_slots(
            &mut ctx.range_facts,
            &mut ctx.range_facts_undo,
            outer_facts_mark,
        );
        // Home-provenance join: arm stores into home slots persist in
        // memory while the table restore reverted the flags — no arm's
        // provenance may survive the merge.
        Self::invalidate_home_inline_flags(ctx);
        if !has_else && written_arms_terminated {
            for &cond in &negated_conds {
                Self::seed_cond_facts(ctx, cond, false);
            }
        }
        // Re-apply kills AFTER the fall-through seeding: a condition's
        // inout call wrote through its pointer on EVERY path past it,
        // so a negation seed from another condition must not resurrect
        // that binding's fact here.
        Self::kill_assigned_since(ctx, scope_mark);

        // The if terminates the block only when every arm does; it
        // counts as a Return for the caller only when every arm
        // actually returns. For mixed all-terminating shapes (e.g.
        // break in one arm, return in another) the Break variant is a
        // stand-in: callers only distinguish None / Return /
        // "terminated some other way".
        Ok(if all_return {
            Terminator::Return
        } else if all_terminated {
            Terminator::Break
        } else {
            Terminator::None
        })
    }

    pub(crate) fn generate_while_loop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let view = ctx.tir.while_loop_view(r);

        let header_block = builder.create_block();
        let body_block = builder.create_block();
        let exit_block = builder.create_block();

        builder.ins().jump(header_block, &[]);

        builder.switch_to_block(header_block);
        // Back-edge rule: kill facts on bindings the body writes BEFORE
        // emitting the condition — the condition re-evaluates every
        // iteration, so a fact it consults must hold on every one.
        // The undo-log mark is taken after this kill, so the post-loop
        // restore keeps these names dead (a body-written binding's
        // pre-loop fact does not hold at the exit either). The
        // cond-true seeds applied below stay sound: the header's brif
        // re-establishes the condition on every iteration.
        Self::kill_loop_writes(ctx, Some(view.cond), &view.body);
        // Back-edge rule for home provenance: a flag set pre-loop says
        // nothing about a value the body stored on a later iteration.
        Self::invalidate_home_inline_flags(ctx);
        let cond_val = Self::eval_inst(builder, ctx, view.cond)?;
        builder
            .ins()
            .brif(cond_val, body_block, &[], exit_block, &[]);

        builder.seal_block(body_block);
        builder.switch_to_block(body_block);

        // The condition holds at every body entry (the header's brif
        // guards it). Assignments inside the body kill facts in place;
        // the seeds themselves must NOT survive the loop — the exit
        // block is also reached on the zero-iteration path.
        let pre_loop_facts_mark = ctx.range_facts_undo.len();
        let scope_mark = ctx.assigned_log.len();
        Self::seed_cond_facts(ctx, view.cond, true);

        ctx.loop_stack.push(LoopContext {
            exit_block,
            continue_target: header_block,
        });
        let body_term = Self::emit_scoped_body(builder, ctx, &view.body)?;
        ctx.loop_stack.pop();

        Self::restore_slots(
            &mut ctx.range_facts,
            &mut ctx.range_facts_undo,
            pre_loop_facts_mark,
        );
        Self::kill_assigned_since(ctx, scope_mark);
        // The body's home stores persist in memory but the scoped
        // restore reverted the flags — post-loop code must not trust
        // pre-loop (or in-body) provenance.
        Self::invalidate_home_inline_flags(ctx);

        if body_term == Terminator::None {
            builder.ins().jump(header_block, &[]);
        }

        // Header has two predecessors: entry fallthrough and body back-edge.
        // Seal it last because the back-edge didn't exist until the body emitted.
        builder.seal_block(header_block);
        builder.seal_block(exit_block);
        builder.switch_to_block(exit_block);

        Ok(Terminator::None)
    }

    pub(crate) fn generate_for_range(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let view = ctx.tir.for_range_view(r);

        // 1. Create all blocks up front
        let header_block = builder.create_block();
        let body_block = builder.create_block();
        let increment_block = builder.create_block();
        let exit_block = builder.create_block();

        // 2. Evaluate bounds once, create hidden counter
        let start_val = Self::eval_inst(builder, ctx, view.start)?;
        let end_val = Self::eval_inst(builder, ctx, view.end)?;
        let counter = builder.declare_var(ctx.int_type);
        builder.def_var(counter, start_val);
        builder.ins().jump(header_block, &[]);

        // 3. Header — DO NOT seal yet (back-edge from increment not emitted)
        builder.switch_to_block(header_block);
        let i = builder.use_var(counter);
        let cond = builder.ins().icmp(IntCC::SignedLessThan, i, end_val);
        builder.ins().brif(cond, body_block, &[], exit_block, &[]);

        // Push loop context: continue targets increment
        ctx.loop_stack.push(LoopContext {
            exit_block,
            continue_target: increment_block,
        });

        // 4. Body — seal immediately (only predecessor is header's brif true-arm)
        builder.seal_block(body_block);
        builder.switch_to_block(body_block);

        // Scope the loop variable: bind var_name to the counter Variable.
        // We deliberately use emit_body rather than emit_scoped_body here
        // because we need to insert the counter binding between the save
        // and the emit; emit_scoped_body's internal save would shadow our
        // insertion. The undo log is NOT replayed at loop exit — only
        // this one slot is restored by hand below, so body writes to
        // other bindings persist past the loop exactly as before.
        let shadowed_var = Self::read_slot(&ctx.locals, view.var_name);
        Self::write_slot(
            &mut ctx.locals,
            &mut ctx.locals_undo,
            view.var_name,
            Some(counter),
        );
        // The loop variable is a different quantity than any shadowed
        // outer binding — its fact must not leak onto the counter.
        let shadowed_fact = Self::read_slot(&ctx.range_facts, view.var_name);
        Self::write_slot(
            &mut ctx.range_facts,
            &mut ctx.range_facts_undo,
            view.var_name,
            None,
        );

        // Back-edge rule (see generate_while_loop): the bounds were
        // evaluated once pre-loop, so pre-loop facts were valid there —
        // but a fact consulted inside the body must hold on every
        // iteration. Kill every binding the body writes before
        // emitting it. There is no post-loop restore here, so the
        // kills simply persist past the loop.
        Self::kill_loop_writes(ctx, None, &view.body);
        // Back-edge rule for home provenance (see generate_while_loop):
        // a flag set pre-loop says nothing about a value the body
        // stored on a later iteration.
        Self::invalidate_home_inline_flags(ctx);

        let body_term = Self::emit_body(builder, ctx, &view.body)?;

        // Restore locals (loop variable goes out of scope)
        Self::write_slot(
            &mut ctx.locals,
            &mut ctx.locals_undo,
            view.var_name,
            shadowed_var,
        );
        // The loop variable's facts die with its scope whether or not
        // the shadowed outer binding had one — write the saved slot back
        // unconditionally (None clears it), discarding whatever the body
        // left on this slot.
        Self::write_slot(
            &mut ctx.range_facts,
            &mut ctx.range_facts_undo,
            view.var_name,
            shadowed_fact,
        );

        if body_term == Terminator::None {
            builder.ins().jump(increment_block, &[]);
        }

        ctx.loop_stack.pop();

        // 5. Increment — seal after body
        builder.seal_block(increment_block);
        builder.switch_to_block(increment_block);
        let i_current = builder.use_var(counter);
        let one = builder.ins().iconst(ctx.int_type, 1);
        let i_next = builder.ins().iadd(i_current, one);
        builder.def_var(counter, i_next);
        builder.ins().jump(header_block, &[]);

        // 6. Seal header (predecessors: entry jump + increment back-edge)
        builder.seal_block(header_block);

        // 7. Exit — always reachable
        builder.seal_block(exit_block);
        builder.switch_to_block(exit_block);
        // The exit is reached from the header on EVERY path (including
        // zero iterations), so neither pre-loop nor last-iteration home
        // provenance holds here.
        Self::invalidate_home_inline_flags(ctx);

        Ok(Terminator::None)
    }
}
