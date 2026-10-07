//! Tail-call emission: the syntactic call-conv pre-pass, the
//! precise eligibility predicate over the ownership side-tables, and
//! the `return_call` emission path. Eligible self-tail-calls run in
//! O(1) stack; everything else silently falls back to `call` + `return`.

use super::{Codegen, FunctionContext};
use cranelift::prelude::*;
use cranelift_module::Module;
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
    fn list_qualifies(tir: &Tir, stmts: &[TirRef], name: StringId) -> bool {
        if let Some(&last) = stmts.last()
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
        stmts.iter().any(|&r| match tir.inst(r).tag {
            TirTag::Return => {
                let operand = match tir.inst(r).data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("Return must carry TirData::UnOp"),
                };
                is_self_call(tir, operand, name)
            }
            TirTag::IfStmt => {
                let view = tir.if_stmt_view(r);
                if list_qualifies(tir, &view.then_stmts, name) {
                    return true;
                }
                if view
                    .elif_branches
                    .iter()
                    .any(|elif| list_qualifies(tir, &elif.body, name))
                {
                    return true;
                }
                match &view.else_stmts {
                    Some(else_stmts) => list_qualifies(tir, else_stmts, name),
                    None => false,
                }
            }
            TirTag::WhileLoop => list_qualifies(tir, &tir.while_loop_view(r).body, name),
            TirTag::ForRange => list_qualifies(tir, &tir.for_range_view(r).body, name),
            _ => false,
        })
    }

    tirs.iter()
        .filter(|tir| {
            main != Some(tir.name)
                && !tir.params.iter().any(|p| p.mode == ParamMode::Inout)
                && list_qualifies(tir, &tir.body_stmts(), tir.name)
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
    /// 4. No frees are scheduled with `after == call_ref` — a
    ///    `return_call` never returns to this frame, so a free anchored
    ///    on the call would never fire.
    /// 5. No frees are scheduled with `after == enclosing_return` (the
    ///    ownership pass double-anchors frees at the enclosing Return
    ///    because codegen cannot sweep after a terminator), and no
    ///    pending free a sweep would fire right now — mirroring the
    ///    exact condition `sweep_due_frees` consults.
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
        for (i, &arg) in view.args.iter().enumerate() {
            if view.modes.get(i) == Some(&ParamMode::Inout) {
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
        if !ctx.free_by_after[call_ref.index()].is_empty() {
            return false;
        }
        if let Some(r) = enclosing_return
            && !ctx.free_by_after[r.index()].is_empty()
        {
            return false;
        }
        let pending_fireable = ctx.pending_sweep.iter().any(|&idx| {
            let fp = &ctx.sidecar.free_schedule[idx];
            Self::branch_active(fp.branch, &ctx.branch_stack)
                && Self::cached_repr(ctx, fp.after).is_some()
                && Self::cached_repr(ctx, fp.target).is_some()
        });
        !pending_fireable
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
    /// * Sweep-eligible frees: `tail_call_eligible`'s pending-sweep
    ///   mirror runs BEFORE arg marshalling, when the call's arg
    ///   sub-expressions have no cached reprs yet — a last-use free
    ///   anchored on a `Var` read inside an arg (the normal body-level
    ///   anchor) becomes sweep-eligible only when marshalling evaluates
    ///   that arg, and `emit_body` never sweeps after a Return
    ///   terminator. Sweeping here (frame still alive; the marshalled
    ///   scalars are the freed values' last use) is the only sound way
    ///   to keep such a free. The plain path fires exactly this free
    ///   via the end-of-statement sweep.
    ///
    /// free_schedule frees at the anchor itself are already excluded by
    /// `tail_call_eligible` (conditions 4/5), so the due-frees call is
    /// normally a no-op; firing it keeps the tail path a faithful
    /// mirror of the plain path.
    pub(crate) fn try_emit_tail_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        call_ref: TirRef,
        enclosing_return: Option<TirRef>,
        anchor: TirRef,
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
        Self::emit_due_frees(builder, ctx, anchor)?;
        Self::emit_due_promo_frees(builder, ctx, anchor)?;
        Self::sweep_due_frees(builder, ctx)?;
        Self::sweep_due_promo_frees(builder, ctx)?;
        let callee_ref = ctx.module.declare_func_in_func(callee_id, builder.func);
        builder.ins().return_call(callee_ref, &marshalled.values);
        Ok(true)
    }
}
