//! Checked arithmetic emission (spec §18): overflow-guarded integer
//! binops, zero/overflow division guards, the shared compound-assign
//! op dispatch, and the deferred cold panic blocks those guards
//! branch to. Split out of `expr.rs` to keep both files under the
//! 2000-line file-length limit.

use super::{Codegen, FunctionContext, OVERFLOW_MSG, ranges};
use cranelift::codegen::ir::{InstructionData, Opcode, ValueDef};
use cranelift::prelude::*;
use cranelift_module::{DataDescription, DataId, Module};
use ryo_core::ast::CompoundOp;
use ryo_core::tir::TirTag;
use std::collections::HashMap;

/// Zero-divisor guard messages, written verbatim by `ryo_panic`
/// (raw bytes, trailing newline — same convention as the runtime's
/// slice-failure messages).
pub(crate) const DIV_ZERO_MSG: &str = "integer division by zero\n";
pub(crate) const MOD_ZERO_MSG: &str = "integer modulo by zero\n";
pub(crate) const DIV_OVERFLOW_MSG: &str = "integer division overflow\n";
pub(crate) const MOD_OVERFLOW_MSG: &str = "integer modulo overflow\n";

impl<M: Module> Codegen<M> {
    /// Define a compiler-generated message as a read-only data object,
    /// deduped per module through `Codegen::guard_msg_data`.
    fn store_guard_msg(
        module: &mut M,
        data_ctx: &mut DataDescription,
        cache: &mut HashMap<&'static str, DataId>,
        msg: &'static str,
    ) -> Result<DataId, String> {
        if let Some(&data_id) = cache.get(msg) {
            return Ok(data_id);
        }
        let data_id = module
            .declare_anonymous_data(false, false)
            .map_err(|e| format!("Failed to declare guard message data: {}", e))?;
        data_ctx.clear();
        data_ctx.define(msg.as_bytes().into());
        module
            .define_data(data_id, data_ctx)
            .map_err(|e| format!("Failed to define guard message data: {}", e))?;
        cache.insert(msg, data_id);
        Ok(data_id)
    }

    /// The immediate behind `v` when it was produced by an `iconst`
    /// in the function being built, otherwise `None`. The checked
    /// arithmetic guards use it to drop a check the constant makes
    /// unreachable (`x + 0`, `x * 1`, a non-zero constant divisor).
    /// Sema only const-folds when *every* operand is constant, so
    /// these mixed const/runtime shapes reach codegen intact.
    fn const_int(builder: &FunctionBuilder, v: Value) -> Option<i64> {
        let ValueDef::Result(inst, _) = builder.func.dfg.value_def(v) else {
            return None;
        };
        match builder.func.dfg.insts[inst] {
            InstructionData::UnaryImm {
                opcode: Opcode::Iconst,
                imm,
            } => Some(imm.bits()),
            _ => None,
        }
    }

    /// Spec §18 checked `+`/`-`/`*` with value-range elision: when both
    /// operands' bounds prove the result fits in `i64`, emit the raw op
    /// and skip the overflow guard entirely. Any unknown side falls
    /// back to the checked helpers.
    pub(crate) fn emit_int_binop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        tag: TirTag,
        lhs_range: Option<ranges::IntRange>,
        rhs_range: Option<ranges::IntRange>,
        lv: Value,
        rv: Value,
    ) -> Result<Value, String> {
        // Both dispatch sites below assume IMul in their `_` arm.
        debug_assert!(matches!(tag, TirTag::IAdd | TirTag::ISub | TirTag::IMul));
        let fits = lhs_range.zip(rhs_range).and_then(|(a, b)| match tag {
            TirTag::IAdd => a.checked_add(b),
            TirTag::ISub => a.checked_sub(b),
            TirTag::IMul => a.checked_mul(b),
            _ => unreachable!("emit_int_binop: not an int arith tag"),
        });
        if fits.is_some() {
            return Ok(match tag {
                TirTag::IAdd => builder.ins().iadd(lv, rv),
                TirTag::ISub => builder.ins().isub(lv, rv),
                _ => builder.ins().imul(lv, rv),
            });
        }
        match tag {
            TirTag::IAdd => Self::emit_checked_iadd(builder, ctx, lv, rv),
            TirTag::ISub => Self::emit_checked_isub(builder, ctx, lv, rv),
            _ => Self::emit_checked_imul(builder, ctx, lv, rv),
        }
    }

    /// Checked signed addition (spec §18): `sadd_overflow` plus the
    /// `ryo_panic` guard, except when a constant operand makes the
    /// operation exact. `x + 0` is `x` for every `x`, so the guard —
    /// and the add itself — is dropped.
    pub(crate) fn emit_checked_iadd(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        lhs: Value,
        rhs: Value,
    ) -> Result<Value, String> {
        // Addition commutes, so either side may carry the zero.
        if Self::const_int(builder, rhs) == Some(0) {
            return Ok(lhs);
        }
        if Self::const_int(builder, lhs) == Some(0) {
            return Ok(rhs);
        }
        let (sum, of) = builder.ins().sadd_overflow(lhs, rhs);
        Self::emit_panic_guard(builder, ctx, of, OVERFLOW_MSG)?;
        Ok(sum)
    }

    /// Checked signed subtraction (spec §18). `x - 0` is exact for
    /// every `x`; a constant minuend has no such shortcut (`0 - x`
    /// overflows at `INT_MIN`), so it keeps the guard.
    pub(crate) fn emit_checked_isub(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        lhs: Value,
        rhs: Value,
    ) -> Result<Value, String> {
        if Self::const_int(builder, rhs) == Some(0) {
            return Ok(lhs);
        }
        let (diff, of) = builder.ins().ssub_overflow(lhs, rhs);
        Self::emit_panic_guard(builder, ctx, of, OVERFLOW_MSG)?;
        Ok(diff)
    }

    /// Checked signed multiplication (spec §18). `x * 0` and `x * 1`
    /// are exact for every `x`, so those drop the guard — `x * -1`
    /// does not, since `INT_MIN * -1` overflows.
    pub(crate) fn emit_checked_imul(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        lhs: Value,
        rhs: Value,
    ) -> Result<Value, String> {
        let konst = Self::const_int(builder, rhs).or_else(|| Self::const_int(builder, lhs));
        if matches!(konst, Some(0) | Some(1)) {
            return Ok(builder.ins().imul(lhs, rhs));
        }
        let (prod, of) = builder.ins().smul_overflow(lhs, rhs);
        Self::emit_panic_guard(builder, ctx, of, OVERFLOW_MSG)?;
        Ok(prod)
    }

    /// Guards for `sdiv`/`srem`, which are UB in Cranelift when the
    /// divisor is zero (`idiv` traps on x86-64; `sdiv` silently
    /// returns garbage on aarch64) and on signed overflow:
    /// `INT_MIN / -1` (and `% -1`) has no representable result.
    /// `dividend_range` lets a dividend proven not to be `i64::MIN`
    /// skip the overflow check.
    pub(crate) fn emit_div_guard(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        dividend: Value,
        dividend_range: Option<ranges::IntRange>,
        divisor: Value,
        zero_msg: &'static str,
        overflow_msg: &'static str,
    ) -> Result<(), String> {
        // A constant divisor outside {0, -1} can trip neither guard.
        // The zero constant keeps its guard: sema rejects the literal
        // forms, so anything reaching here must still panic at runtime.
        let divisor_const = Self::const_int(builder, divisor);
        if divisor_const.is_some_and(|c| c != 0 && c != -1) {
            return Ok(());
        }
        if divisor_const != Some(-1) {
            let zero = builder.ins().iconst(ctx.int_type, 0);
            let is_zero = builder.ins().icmp(IntCC::Equal, divisor, zero);
            Self::emit_panic_guard(builder, ctx, is_zero, zero_msg)?;
        }
        // Overflow needs dividend == i64::MIN and divisor == -1; a
        // constant or range-bounded dividend that excludes i64::MIN
        // makes the check unreachable.
        let dividend_safe = Self::const_int(builder, dividend).is_some_and(|c| c != i64::MIN)
            || dividend_range.is_some_and(|r| r.lo > i64::MIN);
        if !dividend_safe {
            let min = builder.ins().iconst(ctx.int_type, i64::MIN);
            let neg_one = builder.ins().iconst(ctx.int_type, -1);
            let d_is_min = builder.ins().icmp(IntCC::Equal, dividend, min);
            let r_is_neg_one = builder.ins().icmp(IntCC::Equal, divisor, neg_one);
            let overflow = builder.ins().band(d_is_min, r_is_neg_one);
            Self::emit_panic_guard(builder, ctx, overflow, overflow_msg)?;
        }
        Ok(())
    }

    /// Checked `current op= rhs` arithmetic (spec §18), shared by the
    /// bare `CompoundAssign` arm and compound field assignment.
    /// `lhs_range` carries the target's known range when it has one
    /// (bindings do; struct fields don't).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_compound_op(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        op: CompoundOp,
        is_float: bool,
        lhs_range: Option<ranges::IntRange>,
        rhs_range: Option<ranges::IntRange>,
        current: Value,
        rhs: Value,
    ) -> Result<Value, String> {
        Ok(match (op, is_float) {
            (CompoundOp::Add, false) => Self::emit_int_binop(
                builder,
                ctx,
                TirTag::IAdd,
                lhs_range,
                rhs_range,
                current,
                rhs,
            )?,
            (CompoundOp::Sub, false) => Self::emit_int_binop(
                builder,
                ctx,
                TirTag::ISub,
                lhs_range,
                rhs_range,
                current,
                rhs,
            )?,
            (CompoundOp::Mul, false) => Self::emit_int_binop(
                builder,
                ctx,
                TirTag::IMul,
                lhs_range,
                rhs_range,
                current,
                rhs,
            )?,
            (CompoundOp::Div, false) => {
                Self::emit_div_guard(
                    builder,
                    ctx,
                    current,
                    lhs_range,
                    rhs,
                    DIV_ZERO_MSG,
                    DIV_OVERFLOW_MSG,
                )?;
                builder.ins().sdiv(current, rhs)
            }
            (CompoundOp::Mod, false) => {
                Self::emit_div_guard(
                    builder,
                    ctx,
                    current,
                    lhs_range,
                    rhs,
                    MOD_ZERO_MSG,
                    MOD_OVERFLOW_MSG,
                )?;
                builder.ins().srem(current, rhs)
            }
            (CompoundOp::Add, true) => builder.ins().fadd(current, rhs),
            (CompoundOp::Sub, true) => builder.ins().fsub(current, rhs),
            (CompoundOp::Mul, true) => builder.ins().fmul(current, rhs),
            (CompoundOp::Div, true) => builder.ins().fdiv(current, rhs),
            (CompoundOp::Mod, true) => return Err("float modulo not supported".to_string()),
        })
    }

    /// Branch to a shared cold block that calls `ryo_panic` — stderr
    /// message + exit 101, the same contract as the `panic()` builtin —
    /// when `flag` is set; otherwise fall through. Shared by the
    /// zero-divisor guard and the spec §18 overflow checks.
    ///
    /// The panic block is NOT emitted here: it is deferred to
    /// end-of-function (`emit_deferred_panic_blocks`) so the hot path
    /// falls through the `brif` and all cold code sits out of line,
    /// after the function body. Guards with the same message share one
    /// panic block.
    pub(crate) fn emit_panic_guard(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        flag: Value,
        msg: &'static str,
    ) -> Result<(), String> {
        let panic_block = match ctx.panic_blocks.iter().find(|(m, _)| *m == msg) {
            Some(&(_, block)) => block,
            None => {
                let block = builder.create_block();
                ctx.panic_blocks.push((msg, block));
                block
            }
        };
        let ok_block = builder.create_block();
        builder.ins().brif(flag, panic_block, &[], ok_block, &[]);

        // `ok_block` has exactly one predecessor (the brif above), so
        // it can be sealed immediately. The shared panic block gains a
        // predecessor per guard and is sealed when emitted.
        builder.seal_block(ok_block);
        builder.switch_to_block(ok_block);
        Ok(())
    }

    /// Emit the deferred guard-failure blocks collected in
    /// `ctx.panic_blocks` after the function body. Must be called once
    /// per function, after `emit_body`, before `builder.finalize()`.
    pub(crate) fn emit_deferred_panic_blocks(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
    ) -> Result<(), String> {
        let panic_blocks = std::mem::take(&mut ctx.panic_blocks);
        for (msg, block) in panic_blocks {
            builder.seal_block(block);
            builder.switch_to_block(block);
            let data_id = Self::store_guard_msg(ctx.module, ctx.data_ctx, ctx.guard_msg_data, msg)?;
            let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
            let ptr = builder.ins().symbol_value(ctx.int_type, data_ref);
            let len = builder.ins().iconst(types::I64, msg.len() as i64);
            let panic_ref = Self::declare_runtime_fn(
                ctx.module,
                builder,
                "ryo_panic",
                // Runtime contract: ryo_panic(ptr, len: u64) — the
                // length is fixed I64 regardless of target pointer width.
                &[ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(panic_ref, &[ptr, len]);
            // Unreachable in practice (ryo_panic never returns); keeps
            // Cranelift honest about the block having a terminator.
            builder.ins().trap(
                TrapCode::user(1).expect("user trap code 1 is within Cranelift's encodable range"),
            );
        }
        Ok(())
    }
}
