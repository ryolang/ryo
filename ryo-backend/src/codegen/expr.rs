//! Expression evaluation and call emission — split from `mod.rs`; see module docs there.

use super::{
    Codegen, DIV_ZERO_MSG, FunctionContext, MOD_ZERO_MSG, OVERFLOW_MSG, STR_SLOT_SIZE, ValueRepr,
    cranelift_type_for, is_str_type, ranges, store_string,
};
use cranelift::codegen::ir::{
    BlockArg, FuncRef, InstructionData, MemFlagsData, Opcode, StackSlot, ValueDef,
};
use cranelift::prelude::*;
use cranelift_module::{DataDescription, DataId, Linkage, Module};
use ryo_core::tir::{ParamMode, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeKind};
use std::collections::HashMap;

/// Cap derivation for the packed-u128 runtime string ABI (Phase 0):
/// string-producing runtime functions return `{ptr, len}` packed in
/// one u128 (lo = ptr, hi = len) — a true register return on every
/// supported target (see `pack_pair` in `runtime/src/lib.rs` for why
/// not a struct). The `cap` word is a codegen-side derivation:
/// `Static` (cap = 0, the .rodata sentinel) for
/// `ryo_str_from_literal`, `LenIsCap` (cap = len) for every
/// allocating producer — the runtime never over-allocates, and
/// `__ryo_str_push` manages growth capacity through its unchanged
/// slot ABI.
#[derive(Clone, Copy)]
enum CapRule {
    Static,
    LenIsCap,
}

impl<M: Module> Codegen<M> {
    /// Materialize an instruction's value, recursively materializing
    /// operand `TirRef`s as needed. Memoized: a second visit hands
    /// back the cached `Value`.
    pub(crate) fn eval_inst(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        if let Some(repr) = Self::cached_repr(ctx, r) {
            return match repr {
                ValueRepr::Scalar(v) => Ok(v),
                // Str/view-typed values have no scalar stand-in.
                // A multi-word repr reaching the scalar entry point
                // means a consumer forgot to gate through eval_inst_str
                // / eval_inst_view — reject loudly instead of silently
                // handing out the data pointer.
                ValueRepr::Str { .. } | ValueRepr::View { .. } => Err(format!(
                    "eval_inst: str/view-typed inst %{} reached the scalar entry point; use eval_inst_str / eval_inst_view",
                    r.index()
                )),
            };
        }
        let inst = ctx.tir.inst(r);
        // Str- and view-typed insts are multi-word and have no
        // business on the scalar path. Calls are checked separately in
        // the Call arm below (bare-statement str calls route through
        // emit_call / eval_inst_str instead).
        if inst.tag != TirTag::Call && (is_str_type(inst.ty, ctx.pool) || ctx.pool.is_view(inst.ty))
        {
            return Err(format!(
                "eval_inst: str/view-typed inst %{} reached the scalar entry point; use eval_inst_str / eval_inst_view",
                r.index()
            ));
        }
        let value = match inst.tag {
            TirTag::IntConst => match inst.data {
                TirData::Int(v) => builder.ins().iconst(ctx.int_type, v),
                _ => unreachable!("IntConst must carry TirData::Int"),
            },
            TirTag::BoolConst => match inst.data {
                TirData::Bool(b) => builder.ins().iconst(types::I8, if b { 1 } else { 0 }),
                _ => unreachable!("BoolConst must carry TirData::Bool"),
            },
            TirTag::FloatConst => match inst.data {
                TirData::Float(v) => builder.ins().f64const(v),
                _ => unreachable!("FloatConst must carry TirData::Float"),
            },
            TirTag::StrConst => {
                // Unreachable — the entry guard above rejects str-typed
                // insts. __ryo_panic's message pointer goes through
                // emit_strconst_rodata_ptr instead.
                Err(format!(
                    "eval_inst: StrConst %{} reached the scalar entry point",
                    r.index()
                ))?
            }
            TirTag::Var => match inst.data {
                TirData::Var(name) => {
                    let var = ctx
                        .locals
                        .get(&name)
                        .ok_or_else(|| format!("Undefined variable: '{}'", ctx.pool.str(name)))?;
                    builder.use_var(*var)
                }
                _ => unreachable!("Var must carry TirData::Var"),
            },
            TirTag::INeg => match inst.data {
                TirData::UnOp(operand) => {
                    let v = Self::eval_inst(builder, ctx, operand)?;
                    // Spec §18 checked negation: `-(x)` as `0 - x` so
                    // `-(i64::MIN)` sets the overflow flag and panics.
                    // Elide when the operand's bounds exclude i64::MIN —
                    // the only input whose negation overflows.
                    let zero = builder.ins().iconst(ctx.int_type, 0);
                    if ranges::int_range_of(ctx.tir, &ctx.range_facts, operand)
                        .and_then(|r| r.checked_neg())
                        .is_some()
                    {
                        builder.ins().isub(zero, v)
                    } else {
                        let (r, of) = builder.ins().ssub_overflow(zero, v);
                        Self::emit_panic_guard(builder, ctx, of, OVERFLOW_MSG)?;
                        r
                    }
                }
                _ => unreachable!("INeg must carry TirData::UnOp"),
            },
            TirTag::BoolNot => match inst.data {
                TirData::UnOp(operand) => {
                    let v = Self::eval_inst(builder, ctx, operand)?;
                    let one = builder.ins().iconst(types::I8, 1);
                    builder.ins().bxor(v, one)
                }
                _ => unreachable!("BoolNot must carry TirData::UnOp"),
            },
            TirTag::IAdd
            | TirTag::ISub
            | TirTag::IMul
            | TirTag::ISDiv
            | TirTag::IMod
            | TirTag::ICmpEq
            | TirTag::ICmpNe
            | TirTag::ICmpLt
            | TirTag::ICmpLe
            | TirTag::ICmpGt
            | TirTag::ICmpGe
            | TirTag::FAdd
            | TirTag::FSub
            | TirTag::FMul
            | TirTag::FDiv
            | TirTag::FCmpEq
            | TirTag::FCmpNe
            | TirTag::FCmpLt
            | TirTag::FCmpLe
            | TirTag::FCmpGt
            | TirTag::FCmpGe => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("binary op must carry TirData::BinOp"),
                };
                let lv = Self::eval_inst(builder, ctx, lhs)?;
                let rv = Self::eval_inst(builder, ctx, rhs)?;
                match inst.tag {
                    // Spec §18: signed +,-,* trap on overflow in all
                    // build modes. The s*_overflow ops return the
                    // wrapped result plus an i8 overflow flag; a set
                    // flag branches to ryo_panic.
                    TirTag::IAdd | TirTag::ISub | TirTag::IMul => Self::emit_int_binop(
                        builder,
                        ctx,
                        inst.tag,
                        ranges::int_range_of(ctx.tir, &ctx.range_facts, lhs),
                        ranges::int_range_of(ctx.tir, &ctx.range_facts, rhs),
                        lv,
                        rv,
                    )?,
                    TirTag::ISDiv => {
                        Self::emit_div_zero_guard(builder, ctx, rv, DIV_ZERO_MSG)?;
                        builder.ins().sdiv(lv, rv)
                    }
                    TirTag::IMod => {
                        Self::emit_div_zero_guard(builder, ctx, rv, MOD_ZERO_MSG)?;
                        builder.ins().srem(lv, rv)
                    }
                    TirTag::ICmpEq => builder.ins().icmp(IntCC::Equal, lv, rv),
                    TirTag::ICmpNe => builder.ins().icmp(IntCC::NotEqual, lv, rv),
                    TirTag::ICmpLt => builder.ins().icmp(IntCC::SignedLessThan, lv, rv),
                    TirTag::ICmpLe => builder.ins().icmp(IntCC::SignedLessThanOrEqual, lv, rv),
                    TirTag::ICmpGt => builder.ins().icmp(IntCC::SignedGreaterThan, lv, rv),
                    TirTag::ICmpGe => builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, lv, rv),
                    TirTag::FAdd => builder.ins().fadd(lv, rv),
                    TirTag::FSub => builder.ins().fsub(lv, rv),
                    TirTag::FMul => builder.ins().fmul(lv, rv),
                    TirTag::FDiv => builder.ins().fdiv(lv, rv),
                    TirTag::FCmpEq => builder.ins().fcmp(FloatCC::Equal, lv, rv),
                    TirTag::FCmpNe => builder.ins().fcmp(FloatCC::NotEqual, lv, rv),
                    TirTag::FCmpLt => builder.ins().fcmp(FloatCC::LessThan, lv, rv),
                    TirTag::FCmpLe => builder.ins().fcmp(FloatCC::LessThanOrEqual, lv, rv),
                    TirTag::FCmpGt => builder.ins().fcmp(FloatCC::GreaterThan, lv, rv),
                    TirTag::FCmpGe => builder.ins().fcmp(FloatCC::GreaterThanOrEqual, lv, rv),
                    _ => unreachable!(),
                }
            }
            TirTag::BoolAnd => {
                let (lhs_ref, rhs_ref) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("BoolAnd must carry TirData::BinOp"),
                };

                let lhs_val = Self::eval_inst(builder, ctx, lhs_ref)?;

                let rhs_block = builder.create_block();
                let false_block = builder.create_block();
                let merge_block = builder.create_block();
                builder.append_block_param(merge_block, types::I8);

                builder
                    .ins()
                    .brif(lhs_val, rhs_block, &[], false_block, &[]);

                builder.seal_block(rhs_block);
                builder.switch_to_block(rhs_block);
                let rhs_val = Self::eval_inst(builder, ctx, rhs_ref)?;
                builder.ins().jump(merge_block, &[BlockArg::Value(rhs_val)]);

                builder.seal_block(false_block);
                builder.switch_to_block(false_block);
                let false_val = builder.ins().iconst(types::I8, 0);
                builder
                    .ins()
                    .jump(merge_block, &[BlockArg::Value(false_val)]);

                builder.seal_block(merge_block);
                builder.switch_to_block(merge_block);
                builder.block_params(merge_block)[0]
            }
            TirTag::BoolOr => {
                let (lhs_ref, rhs_ref) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("BoolOr must carry TirData::BinOp"),
                };

                let lhs_val = Self::eval_inst(builder, ctx, lhs_ref)?;

                let true_block = builder.create_block();
                let rhs_block = builder.create_block();
                let merge_block = builder.create_block();
                builder.append_block_param(merge_block, types::I8);

                builder.ins().brif(lhs_val, true_block, &[], rhs_block, &[]);

                builder.seal_block(true_block);
                builder.switch_to_block(true_block);
                let true_val = builder.ins().iconst(types::I8, 1);
                builder
                    .ins()
                    .jump(merge_block, &[BlockArg::Value(true_val)]);

                builder.seal_block(rhs_block);
                builder.switch_to_block(rhs_block);
                let rhs_val = Self::eval_inst(builder, ctx, rhs_ref)?;
                builder.ins().jump(merge_block, &[BlockArg::Value(rhs_val)]);

                builder.seal_block(merge_block);
                builder.switch_to_block(merge_block);
                builder.block_params(merge_block)[0]
            }
            TirTag::Call => {
                // Str/view-returning calls are multi-word — they
                // must come through eval_inst_str / eval_inst_view,
                // never the scalar path.
                if is_str_type(inst.ty, ctx.pool) || ctx.pool.is_view(inst.ty) {
                    return Err(format!(
                        "eval_inst: str/view-returning call %{} reached the scalar entry point; use eval_inst_str",
                        r.index()
                    ));
                }
                Self::emit_call(builder, ctx, r)?
            }
            TirTag::IfStmt => {
                Self::generate_if_stmt(builder, ctx, r)?;
                builder.ins().iconst(ctx.int_type, 0)
            }
            TirTag::StrLen => {
                let operand = match inst.data {
                    TirData::UnOp(r) => r,
                    _ => unreachable!("StrLen must carry TirData::UnOp"),
                };
                Self::eval_str_or_view_len(builder, ctx, operand)?
            }
            TirTag::StrCmpEq | TirTag::StrCmpNe => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!(),
                };
                // M8.4 §3.3: operands may be owned str triples or strview
                // view pairs (mixed equality wraps the owned side in
                // ViewOfStr); ryo_str_eq only needs (ptr, len).
                let (l_ptr, l_len) = Self::eval_str_or_view_parts(builder, ctx, lhs)?;
                let (r_ptr, r_len) = Self::eval_str_or_view_parts(builder, ctx, rhs)?;

                let eq_ref = Self::declare_runtime_fn(
                    ctx.module,
                    builder,
                    "ryo_str_eq",
                    &[ctx.int_type, types::I64, ctx.int_type, types::I64],
                    &[types::I8],
                )?;
                let call = builder.ins().call(eq_ref, &[l_ptr, l_len, r_ptr, r_len]);
                let result = builder.inst_results(call)[0];

                if inst.tag == TirTag::StrCmpNe {
                    let one = builder.ins().iconst(types::I8, 1);
                    builder.ins().bxor(result, one)
                } else {
                    result
                }
            }
            TirTag::StrConcat => {
                return Err("StrConcat must be materialized through eval_inst_str".to_string());
            }
            TirTag::Unreachable => {
                return Err(
                    "codegen reached an Unreachable TIR inst — sema must have errored".to_string(),
                );
            }
            other => {
                return Err(format!(
                    "eval_inst: instruction at %{} is not a value (tag={:?})",
                    r.index(),
                    other
                ));
            }
        };
        // Scalar-only entry point: str/view-typed insts are
        // rejected above, so no path here can have cached a non-scalar
        // repr for `r` mid-evaluation.
        Self::cache_repr(ctx, r, ValueRepr::Scalar(value));
        Ok(value)
    }

    /// Emit a string literal's raw `.rodata` pointer (no fat-pointer
    /// triple). Used by `__ryo_panic`'s scalar (ptr, len) ABI — the one
    /// deliberate exception to the rule that str-typed insts never
    /// flow through the scalar entry point.
    fn emit_strconst_rodata_ptr(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        id: StringId,
    ) -> Result<Value, String> {
        let content = ctx.pool.str(id);
        let data_id = store_string(id, content, ctx.module, ctx.data_ctx, ctx.string_data)?;
        let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
        Ok(builder.ins().symbol_value(ctx.int_type, data_ref))
    }

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

    /// Zero-divisor guard for `sdiv`/`srem`, which are UB in Cranelift
    /// when the divisor is zero (`idiv` traps on x86-64; `sdiv`
    /// silently returns garbage on aarch64). Only the divisor is
    /// checked: `INT_MIN / -1` overflow remains UB (out of scope).
    pub(crate) fn emit_div_zero_guard(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        divisor: Value,
        msg: &'static str,
    ) -> Result<(), String> {
        // A non-zero constant divisor can never trip the guard. The
        // zero constant keeps it: sema rejects the literal forms, so
        // anything reaching here must still panic at runtime.
        if Self::const_int(builder, divisor).is_some_and(|c| c != 0) {
            return Ok(());
        }
        let zero = builder.ins().iconst(ctx.int_type, 0);
        let is_zero = builder.ins().icmp(IntCC::Equal, divisor, zero);
        Self::emit_panic_guard(builder, ctx, is_zero, msg)
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
    fn emit_panic_guard(
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

    /// Declare an external runtime function by name and return a
    /// `FuncRef` usable in the current function being built.
    fn declare_runtime_fn(
        module: &mut M,
        builder: &mut FunctionBuilder,
        name: &str,
        params: &[types::Type],
        returns: &[types::Type],
    ) -> Result<FuncRef, String> {
        let mut sig = module.make_signature();
        for &p in params {
            sig.params.push(AbiParam::new(p));
        }
        for &r in returns {
            sig.returns.push(AbiParam::new(r));
        }
        let func_id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| format!("Failed to declare {}: {}", name, e))?;
        Ok(module.declare_func_in_func(func_id, builder.func))
    }

    /// True if a `FreePoint` with the given `branch` tag is eligible
    /// to fire at the current point in codegen. Unconditional entries
    /// (`branch == None`) always pass; branch-gated entries fire only
    /// when their `BranchId` is on `branch_stack`. We use `contains`
    /// rather than `last() == Some(&b)` so a Free anchored to a
    /// parent arm still fires when codegen is inside a nested child
    /// arm of that parent.
    fn branch_active(
        branch: Option<ryo_core::ownership::BranchId>,
        stack: &[ryo_core::ownership::BranchId],
    ) -> bool {
        match branch {
            None => true,
            Some(b) => stack.contains(&b),
        }
    }

    /// Emit `ryo_str_free(ptr, cap)` for any scheduled Free whose
    /// anchor is `tir_ref` and whose `branch` tag is active on the
    /// current `branch_stack`. Called at the end of each
    /// materialisation (`eval_inst` / `eval_inst_str`) so that Task
    /// 4's anonymous-temporary Frees, anchored on the consuming
    /// `Call`, fire after the consumer has emitted its IR.
    ///
    /// Scheduled Frees only target `Str`-cached owners. A
    /// `Scalar`-cached target is an ownership-pass bug — the
    /// borrowed-scalar ABI never owns its argument and the ownership
    /// pass excludes such args from `temp_owners`. If a
    /// `Scalar` target is observed here, this function returns `Err`.
    ///
    /// `freed_at` (a per-`free_schedule`-index flag table) guards against
    /// double-emission across the eval-end hooks and the end-of-stmt
    /// sweep.
    pub(crate) fn emit_due_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        tir_ref: TirRef,
    ) -> Result<(), String> {
        if ctx.sidecar.free_schedule.is_empty() {
            return Ok(());
        }
        let Some(indices) = ctx.free_by_after.get(tir_ref.index()) else {
            return Ok(());
        };
        let pending: Vec<(usize, TirRef)> = indices
            .iter()
            .copied()
            .filter(|&idx| {
                let fp = &ctx.sidecar.free_schedule[idx];
                Self::branch_active(fp.branch, &ctx.branch_stack) && !ctx.freed_at[idx]
            })
            .map(|idx| (idx, ctx.sidecar.free_schedule[idx].target))
            .collect();
        Self::emit_frees(builder, ctx, pending)
    }

    /// End-of-statement sweep: fire any scheduled Free whose anchor
    /// was materialised within the just-emitted statement but hasn't
    /// been emitted yet. This covers Task 3's last-use Frees where
    /// `after` is a sub-expression `Var` read — by the time the
    /// statement finishes, the consumer has already issued its IR,
    /// so a Free here lands after the consumer's use of the buffer.
    /// Eager firing during the inner `eval_inst_str(Var)` would have
    /// dropped the allocation before the consumer (e.g. `print`'s
    /// `write` syscall) finished reading from it.
    ///
    /// Branch-gated entries are filtered through `branch_active`, so
    /// only Frees whose `BranchId` is on the current `branch_stack`
    /// fire here.
    pub(crate) fn sweep_due_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
    ) -> Result<(), String> {
        if ctx.pending_sweep.is_empty() {
            return Ok(());
        }
        let pending: Vec<(usize, TirRef)> = ctx
            .pending_sweep
            .iter()
            .copied()
            .filter(|&idx| {
                let fp = &ctx.sidecar.free_schedule[idx];
                Self::branch_active(fp.branch, &ctx.branch_stack)
                    && Self::cached_repr(ctx, fp.after).is_some()
                    && Self::cached_repr(ctx, fp.target).is_some()
            })
            .map(|idx| (idx, ctx.sidecar.free_schedule[idx].target))
            .collect();
        Self::emit_frees(builder, ctx, pending)
    }

    /// True when `cap` is a materialized `iconst 0` — the static
    /// .rodata sentinel. `ryo_str_free` returns immediately for
    /// cap == 0, so the call is dead at the emission site and can be
    /// skipped; the ownership schedule itself stays untouched.
    fn is_static_cap_zero(func: &cranelift::codegen::ir::Function, cap: Value) -> bool {
        let ValueDef::Result(inst, _) = func.dfg.value_def(cap) else {
            return false;
        };
        let InstructionData::UnaryImm { opcode, imm } = &func.dfg.insts[inst] else {
            return false;
        };
        *opcode == Opcode::Iconst && imm.bits() == 0
    }

    /// Shared emission body for `emit_due_frees` / `sweep_due_frees`.
    /// Given the already-filtered `(free_schedule index, target)`
    /// pairs, declare `ryo_str_free` and emit one call per pair, marking
    /// each index as fired in `ctx.freed_at`. A `Scalar`-cached target
    /// (borrowed-scalar ABI, never heap-owned) returns an error and aborts
    /// code generation — the ABI registry is supposed to keep such args out
    /// of `temp_owners`.
    ///
    /// When the target is a named binding's initializer/value (or a str
    /// param's virtual ref), the Free is emitted from the binding's
    /// CURRENT `StrLocals` instead of the producing inst's cached repr:
    /// after a reassign, a branch merge, or an `inout` write-back the
    /// cached triple may be stale (freed/replaced), while the binding's
    /// `Variable`s are SSA-correct at every program point (the
    /// same reasoning the `free_on_reassign` path documents).
    fn emit_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        pending: Vec<(usize, TirRef)>,
    ) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let free_ref = Self::declare_str_free(ctx.module, builder, ctx.int_type)?;
        for (idx, target) in pending {
            ctx.freed_at[idx] = true;
            let binding = Self::free_binding_name(ctx, target)
                .and_then(|name| ctx.str_locals.get(&name).cloned());
            if let Some(sl) = binding {
                let ptr = builder.use_var(sl.ptr);
                let cap = builder.use_var(sl.cap);
                if !Self::is_static_cap_zero(builder.func, cap) {
                    builder.ins().call(free_ref, &[ptr, cap]);
                }
                continue;
            }
            let repr = Self::cached_repr(ctx, target).ok_or_else(|| {
                format!(
                    "ownership pass scheduled Free for %{} but no ValueRepr cached",
                    target.index()
                )
            })?;
            // M8.4: views are borrows, never owners — the ownership pass
            // must never schedule a Free for one (Task 9 invariant). The
            // repr check below doubles as the release-mode guard.
            debug_assert!(
                !matches!(repr, ValueRepr::View { .. }),
                "ownership pass scheduled Free for strview %{}; views are never freed",
                target.index()
            );
            match repr {
                ValueRepr::Str { ptr, cap, .. } => {
                    if !Self::is_static_cap_zero(builder.func, cap) {
                        builder.ins().call(free_ref, &[ptr, cap]);
                    }
                }
                ValueRepr::View { .. } => {
                    return Err(format!(
                        "ownership pass scheduled Free for non-owning strview %{}; views are never owners",
                        target.index()
                    ));
                }
                ValueRepr::Scalar(_) => {
                    return Err(format!(
                        "ownership pass scheduled Free for borrowed-scalar value %{}; the ABI registry should have excluded it.",
                        target.index()
                    ));
                }
            }
        }
        ctx.pending_sweep.retain(|&idx| !ctx.freed_at[idx]);
        Ok(())
    }

    /// Emit conditional DeadDrops for (`if_stmt`, `arm`): frees of
    /// the pre-if buffer of a conditionally-reassigned binding on the
    /// paths where the reassign did NOT happen. Fired at the START of an
    /// untouched arm, where the binding's `StrLocals` still hold the
    /// pre-if value. Resolves `target` through `free_binding_names` (the
    /// init→name map), so the freed buffer is the binding's
    /// current triple at that program point.
    pub(crate) fn emit_conditional_dead_drops(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        if_stmt: TirRef,
        arm: ryo_core::ownership::BranchId,
    ) -> Result<(), String> {
        for drop in ctx.sidecar.conditional_dead_drops.iter() {
            if drop.if_stmt != if_stmt || !drop.arms.contains(&arm) {
                continue;
            }
            let Some(name) = Self::free_binding_name(ctx, drop.target) else {
                continue;
            };
            let Some(sl) = ctx.str_locals.get(&name).cloned() else {
                continue;
            };
            let free_ref = Self::declare_str_free(ctx.module, builder, ctx.int_type)?;
            let ptr = builder.use_var(sl.ptr);
            let cap = builder.use_var(sl.cap);
            builder.ins().call(free_ref, &[ptr, cap]);
        }
        Ok(())
    }

    /// Map every str-producing named initializer to its binding: VarDecl
    /// initializers, Assign values, and str params' virtual refs. Built
    /// once per function; `emit_frees` consults it to free a binding's
    /// current `StrLocals` rather than a stale cached repr.
    ///
    /// Returns two dense tables: the first indexed by `TirRef::index()`
    /// for real instruction refs (slot 0 unused), the second indexed by
    /// param position for str-param sentinel refs — queried together via
    /// `Codegen::free_binding_name`.
    pub(crate) fn build_free_binding_names(
        tir: &Tir,
        pool: &InternPool,
    ) -> (Vec<Option<StringId>>, Vec<Option<StringId>>) {
        fn walk(tir: &Tir, stmts: &[TirRef], map: &mut [Option<StringId>]) {
            for &r in stmts {
                match tir.inst(r).tag {
                    TirTag::VarDecl => {
                        let view = tir.var_decl_view(r);
                        map[view.initializer.index()] = Some(view.name);
                    }
                    TirTag::Assign => {
                        let view = tir.assign_view(r);
                        map[view.value.index()] = Some(view.name);
                    }
                    TirTag::IfStmt => {
                        let view = tir.if_stmt_view(r);
                        walk(tir, &view.then_stmts, map);
                        for elif in &view.elif_branches {
                            walk(tir, &elif.body, map);
                        }
                        if let Some(else_stmts) = &view.else_stmts {
                            walk(tir, else_stmts, map);
                        }
                    }
                    TirTag::WhileLoop => walk(tir, &tir.while_loop_view(r).body, map),
                    TirTag::ForRange => walk(tir, &tir.for_range_view(r).body, map),
                    _ => {}
                }
            }
        }
        let mut param_names = vec![None; tir.params.len()];
        for (idx, param) in tir.params.iter().enumerate() {
            if is_str_type(param.ty, pool) {
                param_names[idx] = Some(param.name);
            }
        }
        let mut inst_names = vec![None; tir.instructions.len()];
        walk(tir, &tir.body_stmts(), &mut inst_names);
        (inst_names, param_names)
    }

    /// Declare `extern "C" fn ryo_str_free(ptr: *mut u8, cap: u64)` for
    /// the function being built. Returns a `FuncRef` callable via
    /// `builder.ins().call(_, &[ptr, cap])`. `cap == 0` is a runtime
    /// no-op (covers static `.rodata` strings emitted by
    /// `ryo_str_from_literal`).
    pub(crate) fn declare_str_free(
        module: &mut M,
        builder: &mut FunctionBuilder,
        int_type: types::Type,
    ) -> Result<FuncRef, String> {
        Self::declare_runtime_fn(
            module,
            builder,
            "ryo_str_free",
            &[int_type, types::I64],
            &[],
        )
    }

    /// Materialize a str-typed TIR instruction, returning a
    /// `ValueRepr::Str` triple. Falls back to scalar `eval_inst`
    /// for non-str instructions.
    /// Emit a call to a runtime function that returns a (ptr, len) pair
    /// packed as `u128` (lo = ptr, hi = len), and unpack both halves
    /// into SSA values — no stack slot, no out-pointer, no reload at
    /// the call site. `ushr`'s shift amount is any integer type
    /// (masked to the value width), so a plain i64 constant works.
    fn emit_rv_pair_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        fn_name: &str,
        args: &[(Type, Value)],
    ) -> Result<(Value, Value), String> {
        let param_tys: Vec<Type> = args.iter().map(|(ty, _)| *ty).collect();
        let func_ref =
            Self::declare_runtime_fn(ctx.module, builder, fn_name, &param_tys, &[types::I128])?;
        let call_args: Vec<Value> = args.iter().map(|(_, val)| *val).collect();
        let call = builder.ins().call(func_ref, &call_args);
        let pair = builder.inst_results(call)[0];
        let ptr = builder.ins().ireduce(ctx.int_type, pair);
        let shift = builder.ins().iconst(types::I64, 64);
        let hi = builder.ins().ushr(pair, shift);
        let len = builder.ins().ireduce(types::I64, hi);
        Ok((ptr, len))
    }

    /// String-producing variant of `emit_rv_pair_call`: appends the
    /// derived `cap` word so the triple lands entirely in SSA values.
    /// Shared by every str-producing runtime call site so they cannot
    /// drift. Does NOT touch `ctx.inst_values` — caching is the
    /// caller's job.
    fn emit_rv_str_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        fn_name: &str,
        args: &[(Type, Value)],
        cap_rule: CapRule,
    ) -> Result<ValueRepr, String> {
        let (ptr, len) = Self::emit_rv_pair_call(builder, ctx, fn_name, args)?;
        let cap = match cap_rule {
            CapRule::Static => builder.ins().iconst(types::I64, 0),
            CapRule::LenIsCap => len,
        };
        Ok(ValueRepr::Str { ptr, len, cap })
    }

    pub(crate) fn eval_inst_str(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<ValueRepr, String> {
        if let Some(repr) = Self::cached_repr(ctx, r) {
            return Ok(repr);
        }
        let inst = ctx.tir.inst(r);
        let repr = match inst.tag {
            TirTag::StrConst => {
                let id = match inst.data {
                    TirData::Str(id) => id,
                    _ => unreachable!(),
                };
                Self::emit_str_literal_fat(builder, ctx, id)?
            }
            TirTag::Var => {
                let name = match inst.data {
                    TirData::Var(name) => name,
                    _ => unreachable!(),
                };
                if let Some(locals) = ctx.str_locals.get(&name) {
                    ValueRepr::Str {
                        ptr: builder.use_var(locals.ptr),
                        len: builder.use_var(locals.len),
                        cap: builder.use_var(locals.cap),
                    }
                } else {
                    // Not a str local — fall through to scalar
                    let val = Self::eval_inst(builder, ctx, r)?;
                    return Ok(ValueRepr::Scalar(val));
                }
            }
            TirTag::Call => {
                let view = ctx.tir.call_view(r);
                let name_str = ctx.pool.str(view.name);
                if name_str == "__ryo_str_from_view" {
                    // M8.4.1.2 `str(view)` materialization: the argument
                    // is a view pair evaluated via `eval_inst_view`.
                    let ValueRepr::View {
                        ptr: v_ptr,
                        len: v_len,
                    } = Self::eval_inst_view(builder, ctx, view.args[0])?
                    else {
                        unreachable!("__ryo_str_from_view argument must produce ValueRepr::View")
                    };
                    Self::emit_rv_str_call(
                        builder,
                        ctx,
                        "ryo_str_from_view",
                        &[(ctx.int_type, v_ptr), (types::I64, v_len)],
                        CapRule::LenIsCap,
                    )?
                } else if name_str == "int_to_str"
                    || name_str == "float_to_str"
                    || name_str == "bool_to_str"
                {
                    let arg_val = Self::eval_inst(builder, ctx, view.args[0])?;
                    let (fn_name, param_ty) = match name_str {
                        "int_to_str" => ("ryo_int_to_str", ctx.int_type),
                        "float_to_str" => ("ryo_float_to_str", types::F64),
                        "bool_to_str" => ("ryo_bool_to_str", types::I8),
                        _ => unreachable!(),
                    };
                    Self::emit_rv_str_call(
                        builder,
                        ctx,
                        fn_name,
                        &[(param_ty, arg_val)],
                        CapRule::LenIsCap,
                    )?
                } else {
                    // User call — emit_call handles sret for str-returning
                    // calls and caches ValueRepr::Str. Called directly
                    // (not via eval_inst): the scalar path rejects
                    // str-returning calls.
                    Self::emit_call(builder, ctx, r)?;
                    if let Some(repr) = Self::cached_repr(ctx, r) {
                        return Ok(repr);
                    }
                    unreachable!("str-returning user call must cache ValueRepr::Str via emit_call");
                }
            }
            TirTag::StrConcat => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!(),
                };
                let l_repr = Self::eval_inst_str(builder, ctx, lhs)?;
                let r_repr = Self::eval_inst_str(builder, ctx, rhs)?;
                let (l_ptr, l_len) = match l_repr {
                    ValueRepr::Str { ptr, len, .. } => (ptr, len),
                    _ => unreachable!(),
                };
                let (r_ptr, r_len) = match r_repr {
                    ValueRepr::Str { ptr, len, .. } => (ptr, len),
                    _ => unreachable!(),
                };

                Self::emit_rv_str_call(
                    builder,
                    ctx,
                    "ryo_str_concat",
                    &[
                        (ctx.int_type, l_ptr),
                        (types::I64, l_len),
                        (ctx.int_type, r_ptr),
                        (types::I64, r_len),
                    ],
                    CapRule::LenIsCap,
                )?
            }
            TirTag::ViewAsStr => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ViewAsStr must carry TirData::UnOp"),
                };
                // Re-borrow into the fat triple: cap=0 static sentinel,
                // identical to string literals. No allocation.
                let ValueRepr::View { ptr, len } = Self::eval_inst_view(builder, ctx, operand)?
                else {
                    unreachable!("ViewAsStr operand must produce ValueRepr::View")
                };
                let cap = builder.ins().iconst(types::I64, 0);
                ValueRepr::Str { ptr, len, cap }
            }
            _ => {
                // Delegate to scalar eval_inst for non-str instructions
                let val = Self::eval_inst(builder, ctx, r)?;
                return Ok(ValueRepr::Scalar(val));
            }
        };
        Self::cache_repr(ctx, r, repr);
        Ok(repr)
    }

    /// Materialize a `strview`-typed TIR instruction as a `ValueRepr::View`
    /// pair (M8.4). Views are 16-byte non-owning `{ptr, len}` values —
    /// they never materialize into the 24-byte str triple and never
    /// enter the free schedule. Views do NOT go through `eval_inst`'s
    /// dummy-scalar pattern: only view-aware consumers
    /// (`print`, `StrLen`, `StrCmpEq/Ne`, call args, view bindings)
    /// reach them, via `eval_str_or_view_parts` or directly.
    pub(crate) fn eval_inst_view(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<ValueRepr, String> {
        if let Some(repr) = Self::cached_repr(ctx, r) {
            return Ok(repr);
        }
        let inst = ctx.tir.inst(r);
        let repr = match inst.tag {
            TirTag::Slice => {
                let (base, start, end) = match inst.data {
                    TirData::Slice { base, start, end } => (base, start, end),
                    _ => unreachable!("Slice must carry TirData::Slice"),
                };
                // Base may be an owned str (triple) or a view (pair).
                let (base_ptr, base_len) = Self::eval_str_or_view_parts(builder, ctx, base)?;
                let start_v = match start {
                    Some(s) => Self::eval_inst(builder, ctx, s)?,
                    None => builder.ins().iconst(types::I64, 0),
                };
                let end_v = match end {
                    Some(e) => Self::eval_inst(builder, ctx, e)?,
                    None => base_len,
                };
                let (ptr, len) = Self::emit_rv_pair_call(
                    builder,
                    ctx,
                    "__ryo_slice",
                    &[
                        (ctx.int_type, base_ptr),
                        (types::I64, base_len),
                        (types::I64, start_v),
                        (types::I64, end_v),
                    ],
                )?;
                ValueRepr::View { ptr, len }
            }
            TirTag::ViewOfStr => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ViewOfStr must carry TirData::UnOp"),
                };
                // Representation conversion only: drop the cap word.
                let ValueRepr::Str { ptr, len, .. } = Self::eval_inst_str(builder, ctx, operand)?
                else {
                    unreachable!("ViewOfStr operand must produce ValueRepr::Str")
                };
                ValueRepr::View { ptr, len }
            }
            TirTag::Var => {
                let name = match inst.data {
                    TirData::Var(name) => name,
                    _ => unreachable!("Var must carry TirData::Var"),
                };
                let locals = ctx.view_locals.get(&name).ok_or_else(|| {
                    format!("Undefined strview variable: '{}'", ctx.pool.str(name))
                })?;
                ValueRepr::View {
                    ptr: builder.use_var(locals.ptr),
                    len: builder.use_var(locals.len),
                }
            }
            TirTag::Call => {
                // Sema rejects `strview` return types (Rule 5), so no call
                // can produce a view today.
                return Err(
                    "eval_inst_view: calls returning strview are rejected by sema (Rule 5)"
                        .to_string(),
                );
            }
            other => {
                return Err(format!(
                    "eval_inst_view: instruction at %{} is not a strview value (tag={:?})",
                    r.index(),
                    other
                ));
            }
        };
        Self::cache_repr(ctx, r, repr);
        Ok(repr)
    }

    /// Evaluate a `str`/`strview`-typed operand and hand back its
    /// `(ptr, len)` words regardless of representation — owned triple
    /// or borrowed view pair (M8.4). Consumers that only need the
    /// viewed bytes (`print`, `StrLen`, `StrCmpEq/Ne`, the
    /// `__ryo_str_push` suffix, the `__ryo_slice` base) use this;
    /// anything needing the cap must stay on `eval_inst_str`.
    fn eval_str_or_view_parts(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<(Value, Value), String> {
        let ty = ctx.tir.inst(r).ty;
        if ctx.pool.is_view(ty) {
            let ValueRepr::View { ptr, len } = Self::eval_inst_view(builder, ctx, r)? else {
                unreachable!("eval_inst_view must produce ValueRepr::View");
            };
            return Ok((ptr, len));
        }
        match Self::eval_inst_str(builder, ctx, r)? {
            ValueRepr::Str { ptr, len, .. } => Ok((ptr, len)),
            ValueRepr::View { ptr, len } => Ok((ptr, len)),
            ValueRepr::Scalar(_) => Err(format!(
                "eval_str_or_view_parts: instruction at %{} is not a str/strview value",
                r.index()
            )),
        }
    }

    /// The `len` word of a `str`/`strview`-typed operand, from either
    /// representation (M8.4). Backs the `StrLen` arm.
    fn eval_str_or_view_len(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let (_, len) = Self::eval_str_or_view_parts(builder, ctx, r)?;
        Ok(len)
    }

    /// Materialize every distinct string literal exactly once, in the
    /// entry block, and pre-seed the `TirRef → ValueRepr` memo so each
    /// use reads the hoisted triple. A literal is pure .rodata packing
    /// (`symbol_value` + `iconst` + the side-effect-free
    /// `ryo_str_from_literal` call), so entry-block materialization is
    /// sound — the entry block dominates every use — and keeps loop
    /// bodies from re-packing the same (ptr, len) per iteration.
    ///
    /// `StrConst` args of `__ryo_panic` are excluded: `emit_call`
    /// consumes them through the raw (ptr, len) path and never touches
    /// the memo, so hoisting them would add a dead call to the hot
    /// path of every function that panics.
    ///
    /// Runs while the entry block is still the builder's current
    /// block (called from `compile_function` right before
    /// `emit_body`).
    pub(crate) fn hoist_str_literals(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
    ) -> Result<(), String> {
        let mut panic_args = vec![false; ctx.tir.instructions.len()];
        for idx in 1..ctx.tir.instructions.len() {
            if ctx.tir.instructions[idx].tag != TirTag::Call {
                continue;
            }
            let r = TirRef::from_raw(u32::try_from(idx).expect("TirRef index out of range"));
            let view = ctx.tir.call_view(r);
            if ctx.pool.str(view.name) == "__ryo_panic" {
                for a in &view.args {
                    panic_args[a.index()] = true;
                }
            }
        }
        let mut hoisted: HashMap<StringId, ValueRepr> = HashMap::new();
        let tir = ctx.tir;
        for (idx, inst) in tir.instructions.iter().enumerate().skip(1) {
            if inst.tag != TirTag::StrConst || panic_args[idx] {
                continue;
            }
            let TirData::Str(id) = inst.data else {
                continue;
            };
            let repr = match hoisted.get(&id) {
                Some(repr) => *repr,
                None => {
                    let repr = Self::emit_str_literal_fat(builder, ctx, id)?;
                    hoisted.insert(id, repr);
                    repr
                }
            };
            ctx.inst_values[idx] = Some(repr);
        }
        Ok(())
    }

    /// Emit a string literal as a fat pointer triple (ptr, len, cap)
    /// by calling `ryo_str_from_literal` at runtime.
    fn emit_str_literal_fat(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        id: StringId,
    ) -> Result<ValueRepr, String> {
        let content = ctx.pool.str(id);
        let data_id = store_string(id, content, ctx.module, ctx.data_ctx, ctx.string_data)?;
        let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
        let rodata_ptr = builder.ins().symbol_value(ctx.int_type, data_ref);
        let lit_len = builder.ins().iconst(types::I64, content.len() as i64);

        Self::emit_rv_str_call(
            builder,
            ctx,
            "ryo_str_from_literal",
            &[(ctx.int_type, rodata_ptr), (types::I64, lit_len)],
            CapRule::Static,
        )
    }

    fn emit_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let view = ctx.tir.call_view(r);
        let name_id = view.name;
        let name_str = ctx.pool.str(name_id);

        // print and __ryo_panic are ordinary runtime calls. They
        // do NOT use the str-triple expansion that user functions use.
        if name_str == "__ryo_panic" {
            // __ryo_panic(ptr, len) keeps its raw scalar ABI — the StrConst
            // .rodata pointer and an int len — now backed by ryo_panic in
            // the runtime (stderr + exit 101). The trap after the call is
            // unreachable in practice; it keeps Cranelift honest about the
            // never-returns contract.
            let mut arg_values = Vec::with_capacity(view.args.len());
            for arg in &view.args {
                // The message is a StrConst whose .rodata pointer the
                // scalar (ptr, len) ABI consumes directly — the one
                // deliberate exception to the scalar-path rule.
                match ctx.tir.inst(*arg).data {
                    TirData::Str(id) => {
                        arg_values.push(Self::emit_strconst_rodata_ptr(builder, ctx, id)?)
                    }
                    _ => arg_values.push(Self::eval_inst(builder, ctx, *arg)?),
                }
            }
            let panic_ref = Self::declare_runtime_fn(
                ctx.module,
                builder,
                "ryo_panic",
                // Runtime contract: ryo_panic(ptr, len: u64) — the
                // length is fixed I64 regardless of target pointer width.
                &[ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(panic_ref, &arg_values);
            builder.ins().trap(
                TrapCode::user(1).expect("user trap code 1 is within Cranelift's encodable range"),
            );
            let dead = builder.create_block();
            builder.seal_block(dead);
            builder.switch_to_block(dead);
            return Ok(builder.ins().iconst(types::I8, 0));
        }

        if name_str == "print" {
            // print is an ordinary runtime call. Accepts either
            // repr — owned str triple or strview pair; ryo_print(ptr,
            // len) only needs the viewed bytes.
            debug_assert_eq!(
                view.args.len(),
                1,
                "sema should reject print() arity errors"
            );
            debug_assert!(
                matches!(
                    ctx.pool.kind(ctx.tir.inst(view.args[0]).ty),
                    TypeKind::Str | TypeKind::View(_)
                ),
                "sema should reject non-str print() args",
            );
            let (ptr, len) = Self::eval_str_or_view_parts(builder, ctx, view.args[0])?;
            let print_ref = Self::declare_runtime_fn(
                ctx.module,
                builder,
                "ryo_print",
                &[ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(print_ref, &[ptr, len]);
            return Ok(builder.ins().iconst(ctx.int_type, 0));
        }

        if name_str == "str_push" {
            // str_push(&s, suffix): spill s's fat pointer to a 24-byte
            // slot, call __ryo_str_push(slot_addr, suffix_ptr, suffix_len),
            // then reload the mutated triple back into s's StrLocals.
            // arg 0 is `&s` (lowered to Var(s)); arg 1 is the suffix str.
            let s_ref = view.args[0];
            let suffix_ref = view.args[1];
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                STR_SLOT_SIZE,
                3,
            ));
            let s_addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
            let s_repr = Self::eval_inst_str(builder, ctx, s_ref)?;
            let ValueRepr::Str { ptr, len, cap } = s_repr else {
                unreachable!("str_push target must be a str");
            };
            builder.ins().store(MemFlagsData::trusted(), ptr, s_addr, 0);
            builder.ins().store(MemFlagsData::trusted(), len, s_addr, 8);
            builder
                .ins()
                .store(MemFlagsData::trusted(), cap, s_addr, 16);
            // M8.4: the suffix may be either repr — an owned `str`
            // passes its ptr+len, a slice/view passes directly (no
            // ViewOfStr wrap: builtins bypass check_call's §3.4
            // conversion, so sema accepts `Str | View(_)` here).
            let (suf_ptr, suf_len) = Self::eval_str_or_view_parts(builder, ctx, suffix_ref)?;
            let func_ref = Self::declare_runtime_fn(
                ctx.module,
                builder,
                "__ryo_str_push",
                &[ctx.int_type, ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(func_ref, &[s_addr, suf_ptr, suf_len]);
            // Reload the mutated fat pointer back into the caller's StrLocals.
            let np = builder
                .ins()
                .load(ctx.int_type, MemFlagsData::trusted(), s_addr, 0);
            let nl = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), s_addr, 8);
            let nc = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), s_addr, 16);
            if let Some(name) = Self::local_name_of(ctx, s_ref)
                && let Some(sl) = ctx.str_locals.get(&name).cloned()
            {
                builder.def_var(sl.ptr, np);
                builder.def_var(sl.len, nl);
                builder.def_var(sl.cap, nc);
            }
            return Ok(builder.ins().iconst(ctx.int_type, 0));
        }

        let callee_id = *ctx
            .func_ids
            .get(&name_id)
            .ok_or_else(|| format!("Undefined function: '{}'", name_str))?;

        let mut arg_values = Vec::with_capacity(view.args.len() * 3 + 1);
        // inout args: spill the current value to a stack slot, pass the
        // slot address, then reload after the call. Scalar spills one
        // field; str spills the fat-pointer triple.
        let mut inout_reloads: Vec<(TirRef, StackSlot)> = Vec::new();
        for (i, arg) in view.args.iter().enumerate() {
            let mode = view.modes.get(i).copied().ok_or_else(|| {
                format!(
                    "internal error: call '{name_str}' has {} args but {} modes",
                    view.args.len(),
                    view.modes.len()
                )
            })?;
            let arg_ty = ctx.tir.inst(*arg).ty;
            if mode == ParamMode::Inout {
                if is_str_type(arg_ty, ctx.pool) {
                    let slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        STR_SLOT_SIZE,
                        3,
                    ));
                    let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                    let repr = Self::eval_inst_str(builder, ctx, *arg)?;
                    let ValueRepr::Str { ptr, len, cap } = repr else {
                        unreachable!("inout str arg must produce ValueRepr::Str");
                    };
                    builder.ins().store(MemFlagsData::trusted(), ptr, addr, 0);
                    builder.ins().store(MemFlagsData::trusted(), len, addr, 8);
                    builder.ins().store(MemFlagsData::trusted(), cap, addr, 16);
                    arg_values.push(addr);
                    inout_reloads.push((*arg, slot));
                } else {
                    let cl_ty = cranelift_type_for(arg_ty, ctx.pool, ctx.int_type);
                    let bytes = cl_ty.bytes().max(8);
                    let slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        bytes,
                        3,
                    ));
                    let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                    let cur = Self::eval_inst(builder, ctx, *arg)?;
                    builder.ins().store(MemFlagsData::trusted(), cur, addr, 0);
                    arg_values.push(addr);
                    inout_reloads.push((*arg, slot));
                }
            } else if is_str_type(arg_ty, ctx.pool) {
                let repr = Self::eval_inst_str(builder, ctx, *arg)?;
                match repr {
                    ValueRepr::Str { ptr, len, cap } => {
                        arg_values.push(ptr);
                        arg_values.push(len);
                        arg_values.push(cap);
                    }
                    _ => unreachable!("str-typed arg must produce ValueRepr::Str"),
                }
            } else if ctx.pool.is_view(arg_ty) {
                // `strview` arg → 2-word ABI (ptr, len), matching the
                // callee's build_signature. Sema has already inserted
                // ViewOfStr for owned-str actuals (§3.4).
                let (ptr, len) = Self::eval_str_or_view_parts(builder, ctx, *arg)?;
                arg_values.push(ptr);
                arg_values.push(len);
            } else {
                arg_values.push(Self::eval_inst(builder, ctx, *arg)?);
            }
        }

        let callee_ref = ctx.module.declare_func_in_func(callee_id, builder.func);

        let ret_ty = ctx.tir.inst(r).ty;

        // If the callee returns never (e.g. __ryo_panic), the call is
        // a terminator. Emit a trap + dead block for subsequent IR.
        if ctx.pool.is_never(ret_ty) {
            builder.ins().call(callee_ref, &arg_values);
            // Reload inout slots before the trap: Cranelift models the
            // callee as an ordinary (returning) call, so the mutations
            // must be visible on the path where control resumes.
            Self::reload_inout_args(builder, ctx, &inout_reloads)?;
            builder.ins().trap(
                TrapCode::user(1).expect("user trap code 1 is within Cranelift's encodable range"),
            );
            let dead = builder.create_block();
            builder.seal_block(dead);
            builder.switch_to_block(dead);
            let dummy_ty = cranelift_type_for(ret_ty, ctx.pool, ctx.int_type);
            return Ok(builder.ins().iconst(dummy_ty, 0));
        }

        if is_str_type(ret_ty, ctx.pool) {
            // sret: allocate 24-byte slot, prepend pointer to args
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                STR_SLOT_SIZE,
                3,
            ));
            let out = builder.ins().stack_addr(ctx.int_type, slot, 0);

            let mut all_args = Vec::with_capacity(arg_values.len() + 1);
            all_args.push(out);
            all_args.extend(arg_values);

            builder.ins().call(callee_ref, &all_args);
            Self::reload_inout_args(builder, ctx, &inout_reloads)?;

            let ptr = builder
                .ins()
                .load(ctx.int_type, MemFlagsData::trusted(), out, 0);
            let len = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), out, 8);
            let cap = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), out, 16);
            Self::cache_repr(ctx, r, ValueRepr::Str { ptr, len, cap });
            return Ok(ptr); // dummy scalar — consumers use eval_inst_str
        }

        let call = builder.ins().call(callee_ref, &arg_values);
        Self::reload_inout_args(builder, ctx, &inout_reloads)?;
        let results = builder.inst_results(call);

        if results.is_empty() {
            Ok(builder.ins().iconst(ctx.int_type, 0))
        } else {
            Ok(results[0])
        }
    }

    /// Reload each inout slot after a call and write the updated value
    /// back into the caller's local. The inout arg was sema-lowered to
    /// its inner `Var(name)` ref, so `*arg_ref` is that `Var` inst —
    /// read its binding name to find the local. Scalar args reload one
    /// field into `locals`; str args reload the fat-pointer triple into
    /// `str_locals`.
    fn reload_inout_args(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        reloads: &[(TirRef, StackSlot)],
    ) -> Result<(), String> {
        for (arg_ref, slot) in reloads {
            let addr = builder.ins().stack_addr(ctx.int_type, *slot, 0);
            let arg_ty = ctx.tir.inst(*arg_ref).ty;
            if is_str_type(arg_ty, ctx.pool) {
                let np = builder
                    .ins()
                    .load(ctx.int_type, MemFlagsData::trusted(), addr, 0);
                let nl = builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), addr, 8);
                let nc = builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), addr, 16);
                if let Some(name) = Self::local_name_of(ctx, *arg_ref) {
                    // The callee may have written anything through the
                    // pointer — the binding's range fact dies here.
                    Self::kill_fact(ctx, name);
                    if let Some(sl) = ctx.str_locals.get(&name).cloned() {
                        builder.def_var(sl.ptr, np);
                        builder.def_var(sl.len, nl);
                        builder.def_var(sl.cap, nc);
                    }
                }
            } else {
                let cl_ty = cranelift_type_for(arg_ty, ctx.pool, ctx.int_type);
                let updated = builder.ins().load(cl_ty, MemFlagsData::trusted(), addr, 0);
                if let Some(name) = Self::local_name_of(ctx, *arg_ref) {
                    Self::kill_fact(ctx, name);
                    if let Some(var) = ctx.locals.get(&name).copied() {
                        builder.def_var(var, updated);
                    }
                }
            }
        }
        Ok(())
    }

    /// Returns the binding name when `r` is a `TirTag::Var` inst, else
    /// `None`. Used to resolve an inout arg (lowered to its inner
    /// `Var(name)`) back to the caller local that must receive the
    /// reloaded value.
    fn local_name_of(ctx: &FunctionContext<'_, M>, r: TirRef) -> Option<StringId> {
        let inst = ctx.tir.inst(r);
        match inst.tag {
            TirTag::Var => match inst.data {
                TirData::Var(name) => Some(name),
                _ => None,
            },
            _ => None,
        }
    }
}
