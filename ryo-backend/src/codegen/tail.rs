//! Tail-call emission: the syntactic call-conv pre-pass, the
//! precise eligibility predicate over the ownership side-tables, and
//! the `return_call` emission path. Eligible self-tail-calls run in
//! O(1) stack; everything else silently falls back to `call` + `return`.

use super::{Codegen, FunctionContext};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::extra::ExtraList;
use ryo_core::tir::{ParamMode, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{StringId, TypeKind};
use std::collections::HashSet;

/// Syntactic pre-pass for tail-call emission, run over every TIR body before
/// declaration: collect the names of functions whose bodies contain a
/// tail-position self-call, so `build_signature` compiles them with
/// `CallConv::Tail` — the only convention from which Cranelift allows
/// `return_call`. Deliberately cheap and conservative: over-marking is
/// harmless (a marked function that turns out ineligible at emission
/// still compiles correctly as a plain Tail-conv function), only
/// under-marking would forgo a tail call. `main` is never marked (the
/// C runtime enters it with the C ABI) and neither is any function
/// with `inout` params (a tail call skips the write-back chokepoint).
pub(crate) fn scan_tail_call_candidates(tirs: &[Tir], main: Option<StringId>) -> HashSet<StringId> {
    fn is_self_call(tir: &Tir, r: TirRef, name: StringId) -> bool {
        tir.inst(r).tag == TirTag::Call && tir.call_view(r).name == name
    }

    // A statement list qualifies when its LAST statement is an ExprStmt
    // calling the function itself, or when any Return in it (at any
    // nesting depth — a `return` exits the whole function, so it is a
    // tail context even inside a loop body) returns a self-call. Loop
    // bodies are scanned for Return shapes only: a trailing ExprStmt
    // there is not in tail position (the loop may iterate again), and
    // over-marking a qualifying Return shape is harmless.
    fn list_qualifies(tir: &Tir, stmts: ExtraList<'_, TirRef>, name: StringId) -> bool {
        if let Some(last) = stmts.iter().next_back()
            && tir.inst(last).tag == TirTag::ExprStmt
        {
            let operand = match tir.inst(last).data {
                TirData::UnOp(o) => o,
                _ => unreachable!("ExprStmt must carry TirData::UnOp"),
            };
            if is_self_call(tir, operand, name) {
                return true;
            }
        }
        stmts.iter().any(|r| match tir.inst(r).tag {
            TirTag::Return => {
                let operand = match tir.inst(r).data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("Return must carry TirData::UnOp"),
                };
                is_self_call(tir, operand, name)
            }
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                if list_qualifies(tir, view.then_stmts, name) {
                    return true;
                }
                if view
                    .elif_branches
                    .iter()
                    .any(|elif| list_qualifies(tir, elif.body, name))
                {
                    return true;
                }
                match view.else_stmts {
                    Some(else_stmts) => list_qualifies(tir, else_stmts, name),
                    None => false,
                }
            }
            TirTag::WhileLoop => list_qualifies(tir, tir.while_loop_body_view(r), name),
            TirTag::ForRange => list_qualifies(tir, tir.for_range_body_view(r), name),
            _ => false,
        })
    }

    tirs.iter()
        .filter(|tir| {
            main != Some(tir.name)
                && !tir.params.iter().any(|p| p.mode == ParamMode::Inout)
                && list_qualifies(tir, tir.body_stmts(), tir.name)
        })
        .map(|tir| tir.name)
        .collect()
}

impl<M: Module> Codegen<M> {
    /// Precise emission-time eligibility for lowering `call_ref` to a
    /// Cranelift `return_call`. True iff ALL hold:
    ///
    /// 1. `call_ref` is a call to the function currently being
    ///    compiled (`func_ids` is keyed by interned name, so a name
    ///    match is a `FuncId` match).
    /// 2. The current function is not `main` and has no `inout` params
    ///    (the tail path skips `emit_return`'s write-back chokepoint).
    /// 3. Call args and the return are scalar-only (i64/f64/bool by
    ///    value, or void): no fat/view/struct args, no inout modes, no
    ///    sret. The deliberate v1 scope; the eager_destruction
    ///    benchmark (`recursive(x: int)`, void return) is covered.
    /// 4. (position-mechanical, mechanical) No unconsumed drop in the call's fire scope
    ///    — the `AfterStep` bucket of the statement containing the
    ///    call, the block's at-terminator bucket, and the enclosing
    ///    Return's payload bucket — is anchored at/after the call's
    ///    step. A `return_call` never returns to this frame, so a drop
    ///    firing at/after it would never fire: one anchored ON the call
    ///    (its result temp) or on the Return blocks; one anchored in the
    ///    operand subtree (an argument read) still fires — marshalling
    ///    has evaluated the argument, making the marshalled scalars the
    ///    freed value's last use — so `try_emit_tail_call` fires the
    ///    scope buckets between marshalling and the `return_call`,
    ///    exactly where the old post-statement sweep fired them.
    ///
    /// Rejecting is always a silent fallback to `call` + `return`,
    /// never an error.
    fn tail_call_eligible(
        ctx: &FunctionContext<'_, M>,
        call_ref: TirRef,
        enclosing_return: Option<TirRef>,
    ) -> bool {
        // The caller must have been compiled with CallConv::Tail —
        // Cranelift only allows `return_call` from a `tail`-convention
        // function (machinst ABI debug_assert). The pre-pass decided
        // this at declaration time.
        if !ctx.tail_conv_candidate {
            return false;
        }
        let view = ctx.tir.call_view(call_ref);
        if view.name != ctx.tir.name {
            return false;
        }
        if ctx.is_main {
            return false;
        }
        if ctx.tir.params.iter().any(|p| p.mode == ParamMode::Inout) {
            return false;
        }
        let pool = ctx.pool;
        for (i, arg) in view.args.iter().enumerate() {
            if view.modes.get(i) == Some(ParamMode::Inout) {
                return false;
            }
            if !matches!(
                pool.kind(ctx.tir.inst(arg).ty),
                TypeKind::Int | TypeKind::Float | TypeKind::Bool
            ) {
                return false;
            }
        }
        if !matches!(
            pool.kind(ctx.tir.inst(call_ref).ty),
            TypeKind::Int | TypeKind::Float | TypeKind::Bool | TypeKind::Void
        ) {
            return false;
        }
        !Self::tail_scope_has_blocking_drops(ctx, call_ref, enclosing_return)
    }

    /// The position-mechanical mechanical tail scope: drop indices in the call's fire
    /// scope — the `AfterStep` bucket of the statement containing the
    /// call (the trailing `ExprStmt`, or no statement when the call is
    /// a deferred return operand), the block's at-terminator bucket,
    /// and the enclosing Return's payload bucket. Returns the indices
    /// of the UNCONSUMED drops in that scope whose anchor is at/after
    /// the call's step — the drops a `return_call` would strand.
    fn tail_blocking_drops(
        ctx: &FunctionContext<'_, M>,
        call_ref: TirRef,
        enclosing_return: Option<TirRef>,
    ) -> Vec<usize> {
        let mut scope: Vec<usize> = Vec::new();
        let Some(block) = ctx.flow.step_block_of(call_ref) else {
            return Vec::new();
        };
        let steps = ctx.flow.block_steps(block);
        let Some(call_pos) = steps.iter().position(|s| s.inst == call_ref) else {
            return Vec::new();
        };
        if let Some(stmt) = steps[call_pos..]
            .iter()
            .find(|s| super::is_statement_step(ctx.tir.inst(s.inst).tag))
        {
            scope.extend(ctx.fire_after_stmt[stmt.inst.index()].iter().copied());
        }
        scope.extend(ctx.fire_at_term.get(&block).into_iter().flatten().copied());
        if let Some(r) = enclosing_return {
            scope.extend(ctx.fire_by_payload[r.index()].iter().copied());
        }
        scope
            .into_iter()
            .filter(|&idx| {
                let position = if idx >= ctx.sidecar.drops.len() {
                    let promotion = idx - ctx.sidecar.drops.len();
                    if ctx.promo_freed_at[promotion] {
                        return false;
                    }
                    ctx.sidecar.promotion_frees[promotion].position
                } else {
                    if ctx.freed_at[idx] {
                        return false;
                    }
                    ctx.sidecar.drops[idx].position
                };
                match position.anchor_inst() {
                    // PreTerminator{block}: fires with the block's
                    // terminator — always at/after the call.
                    None => true,
                    Some(anchor) => match ctx.flow.step_of(anchor) {
                        None => true,
                        Some(_) => steps
                            .iter()
                            .position(|s| s.inst == anchor)
                            .is_none_or(|ap| ap >= call_pos),
                    },
                }
            })
            .collect()
    }

    /// True when `tail_blocking_drops` is non-empty (see
    /// `tail_call_eligible` condition 4).
    fn tail_scope_has_blocking_drops(
        ctx: &FunctionContext<'_, M>,
        call_ref: TirRef,
        enclosing_return: Option<TirRef>,
    ) -> bool {
        !Self::tail_blocking_drops(ctx, call_ref, enclosing_return).is_empty()
    }

    /// Emit `call_ref` as a Cranelift `return_call` when
    /// [`Self::tail_call_eligible`] accepts it, returning `true`. The
    /// args are marshalled exactly as the plain-call path marshals
    /// them; `return_call` replaces `call` + `return` with a jump that
    /// reuses the current frame, so tail recursion runs in O(1) stack.
    /// Ineligible calls return `false` and emit nothing, letting the
    /// caller fall through to the existing `call` + `return` path.
    ///
    /// `anchor` is the enclosing statement (the `Return`, or the
    /// trailing `ExprStmt`). Two free families must fire before the
    /// `return_call`, exactly as the plain path fires them before its
    /// `return_`:
    ///
    /// * Due frees at the anchor: the ownership pass's return-epilogue
    ///   anchors promo frees at every Return/ReturnVoid and the
    ///   fallthrough backstop anchors them at the final body statement.
    /// * The drop fire scope (position-mechanical): `tail_call_eligible` proved every
    ///   unconsumed drop in the scope is anchored inside the operand
    ///   subtree (an argument read) — marshalling has now evaluated
    ///   those arguments, making the marshalled scalars the freed
    ///   values' last use, so firing the scope buckets here (frame
    ///   still alive) lands each free exactly where the mechanical
    ///   post-statement fire point would have. The plain path fires
    ///   the same drops via the statement's fire bucket.
    pub(crate) fn try_emit_tail_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        call_ref: TirRef,
        enclosing_return: Option<TirRef>,
        _anchor: TirRef,
    ) -> Result<bool, String> {
        if !Self::tail_call_eligible(ctx, call_ref, enclosing_return) {
            return Ok(false);
        }
        let view = ctx.tir.call_view(call_ref);
        let callee_id = ctx
            .func_ids
            .get(&view.name)
            .map(|(id, _)| *id)
            .ok_or_else(|| format!("Undefined function: '{}'", ctx.pool.str(view.name)))?;
        let marshalled = Self::marshal_user_call_args(builder, ctx, call_ref, None)?;
        debug_assert!(
            marshalled.sret.is_none(),
            "tail_call_eligible rejects sret-returning callees"
        );
        debug_assert!(
            marshalled.inout_reloads.is_empty(),
            "tail_call_eligible rejects inout args"
        );
        // Fire the scope's non-consumed drops (all anchored in the
        // operand subtree by eligibility). Reuse the blocking-drop
        // scan's scope: the same buckets, filtered to unconsumed.
        let block = ctx.flow.step_block_of(call_ref).expect("call is a step");
        let steps = ctx.flow.block_steps(block);
        let call_pos = steps
            .iter()
            .position(|s| s.inst == call_ref)
            .expect("call step is in its block");
        if let Some(stmt) = steps[call_pos..]
            .iter()
            .find(|s| super::is_statement_step(ctx.tir.inst(s.inst).tag))
        {
            let indices = ctx.fire_after_stmt[stmt.inst.index()].clone();
            Self::fire_bucket(builder, ctx, &indices)?;
        }
        let term_indices = ctx.fire_at_term.get(&block).cloned().unwrap_or_default();
        Self::fire_bucket(builder, ctx, &term_indices)?;
        if let Some(r) = enclosing_return {
            let indices = ctx.fire_by_payload[r.index()].clone();
            Self::fire_bucket(builder, ctx, &indices)?;
        }
        let callee_ref = ctx.module.declare_func_in_func(callee_id, builder.func);
        builder.ins().return_call(callee_ref, &marshalled.values);
        Ok(true)
    }
}
