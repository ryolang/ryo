//! Expression evaluation and call emission — split from `mod.rs`; see module docs there.

mod fat_slot;

use super::arith::{DIV_OVERFLOW_MSG, DIV_ZERO_MSG, MOD_OVERFLOW_MSG, MOD_ZERO_MSG};
use super::literals::store_string;
use super::{
    Codegen, FunctionContext, OVERFLOW_MSG, STR_SLOT_SIZE, Terminator, ValueRepr,
    cranelift_type_for, is_fat_type, ranges,
};
use cranelift::codegen::ir::{FuncRef, MemFlagsData, StackSlot};
use cranelift::prelude::*;
use cranelift_module::{Linkage, Module};
use ryo_core::tir::{ParamMode, TirData, TirRef, TirTag};
use ryo_core::types::{StringId, TypeKind, ViewKind};
use std::collections::HashMap;

/// The result of [`Codegen::marshal_user_call_args`]: the evaluated
/// ABI-form argument list (with the hidden sret pointer already
/// prepended when there is one) plus the inout spill slots the caller
/// must reload once the call returns. The tail-call path
/// requires `sret.is_none()` and an empty `inout_reloads` — eligibility
/// rejects sret callees and inout args up front, so a marshalled tail
/// call never has either.
pub(super) struct MarshalledArgs {
    pub(super) values: Vec<Value>,
    pub(super) sret: Option<Value>,
    pub(super) inout_reloads: Vec<(TirRef, StackSlot)>,
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
                // Fat/view/aggregate-typed values have no scalar stand-in.
                // A multi-word repr reaching the scalar entry point
                // means a consumer forgot to gate through eval_inst_fat
                // / eval_inst_view / eval_inst_struct / eval_inst_enum —
                // reject loudly instead of silently handing out the
                // data pointer.
                ValueRepr::Str { .. }
                | ValueRepr::Bytes { .. }
                | ValueRepr::View { .. }
                | ValueRepr::Struct { .. }
                | ValueRepr::Enum { .. } => Err(format!(
                    "eval_inst: fat/view/struct/enum-typed inst %{} reached the scalar entry point; use eval_inst_fat / eval_inst_view / eval_inst_struct / eval_inst_enum",
                    r.index()
                )),
            };
        }
        let inst = ctx.tir.inst(r);
        // Fat- and view-typed insts are multi-word and have no
        // business on the scalar path. Calls are checked separately in
        // the Call arm below (bare-statement fat calls route through
        // emit_call / eval_inst_fat instead).
        if inst.tag != TirTag::Call && (is_fat_type(inst.ty, ctx.pool) || ctx.pool.is_view(inst.ty))
        {
            return Err(format!(
                "eval_inst: fat/view-typed inst %{} reached the scalar entry point; use eval_inst_fat / eval_inst_view",
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
                    let var = Self::read_slot(&ctx.locals, name)
                        .ok_or_else(|| format!("Undefined variable: '{}'", ctx.pool.str(name)))?;
                    builder.use_var(var)
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
            TirTag::FNeg => match inst.data {
                TirData::UnOp(operand) => {
                    let v = Self::eval_inst(builder, ctx, operand)?;
                    builder.ins().fneg(v)
                }
                _ => unreachable!("FNeg must carry TirData::UnOp"),
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
                Self::emit_numeric_binary_op(builder, ctx, inst.tag, lhs, rhs)?
            }
            TirTag::BoolAnd | TirTag::BoolOr => {
                // The short-circuit control flow is explicit in
                // the Flow IR — the lhs block's Branch terminator
                // carries the constant merge value INTO the join block
                // on the short-circuit edge, the rhs block's Goto
                // carries the rhs value, and this STEP (the join
                // block's first step) reads the join block's single
                // I8 block param: the merged boolean.
                let join_block = ctx
                    .flow
                    .step_block_of(r)
                    .expect("BoolAnd/BoolOr step lives in its short-circuit join block");
                let cb = ctx.flow_blocks[join_block.0 as usize];
                builder.block_params(cb)[0]
            }
            TirTag::Call => {
                // Fat/view-returning calls are multi-word — they
                // must come through eval_inst_fat / eval_inst_view,
                // never the scalar path.
                if is_fat_type(inst.ty, ctx.pool) || ctx.pool.is_view(inst.ty) {
                    return Err(format!(
                        "eval_inst: fat/view-returning call %{} reached the scalar entry point; use eval_inst_fat",
                        r.index()
                    ));
                }
                Self::emit_call(builder, ctx, r)?
            }
            TirTag::IfStmt => {
                // Sema's `assert` desugar puts an IfStmt in
                // expression position (`ExprStmt(IfStmt)`); the flow
                // builder cuts the cascade wherever it stands, so the
                // IfStmt's Branch terminator and arm blocks were
                // dispatched before this step's join block was reached.
                // The old recursive emitter returned the same dummy
                // after lowering the cascade.
                debug_assert!(
                    ctx.if_records.contains_key(&r),
                    "expression-position IfStmt cascade was dispatched before its join"
                );
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
                Self::emit_str_eq(builder, ctx, inst.tag, lhs, rhs)?
            }
            TirTag::BytesCmpEq | TirTag::BytesCmpNe => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!(),
                };
                Self::emit_bytes_eq(builder, ctx, inst.tag, lhs, rhs)?
            }
            TirTag::StructEq | TirTag::StructNe => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("StructEq/StructNe must carry TirData::BinOp"),
                };
                // Operands are struct values: materialize their slot
                // addresses (never the scalar entry — structs are
                // memory-first), then memberwise-compare. The
                // comparison borrows both operands.
                let lv = Self::eval_inst_struct(builder, ctx, lhs)?;
                let rv = Self::eval_inst_struct(builder, ctx, rhs)?;
                let ty = ctx.tir.inst(lhs).ty;
                Self::emit_struct_eq(builder, ctx, lv, rv, ty, inst.tag == TirTag::StructNe)?
            }
            TirTag::EnumEq | TirTag::EnumNe => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("EnumEq/EnumNe must carry TirData::BinOp"),
                };
                // Operands are enum values: materialize their slot
                // addresses (never the scalar entry — enums are
                // memory-first), then compare the i32 tags and the
                // active variant's payload. The comparison borrows
                // both operands.
                let lv = Self::eval_inst_enum(builder, ctx, lhs)?;
                let rv = Self::eval_inst_enum(builder, ctx, rhs)?;
                let ty = ctx.tir.inst(lhs).ty;
                Self::emit_enum_eq(builder, ctx, lv, rv, ty, inst.tag == TirTag::EnumNe)?
            }
            TirTag::BytesIndex => {
                let (base, index) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("BytesIndex must carry TirData::BinOp"),
                };
                // Bounds check + panic are runtime-side, mirroring the
                // inline slice guards — no Cranelift branch needed.
                let (ptr, len) = Self::eval_str_or_view_parts(builder, ctx, base)?;
                let idx = Self::eval_inst(builder, ctx, index)?;
                let index_ref = Self::declare_runtime_fn(
                    ctx,
                    builder,
                    "__ryo_bytes_index",
                    &[ctx.int_type, types::I64, types::I64],
                    &[types::I64],
                )?;
                let call = builder.ins().call(index_ref, &[ptr, len, idx]);
                builder.inst_results(call)[0]
            }
            TirTag::StrConcat => {
                return Err("StrConcat must be materialized through eval_inst_fat".to_string());
            }
            TirTag::BytesConcat => {
                return Err("BytesConcat must be materialized through eval_inst_fat".to_string());
            }
            TirTag::Unreachable => {
                return Err(
                    "codegen reached an Unreachable TIR inst — sema must have errored".to_string(),
                );
            }
            TirTag::FieldAccess => Self::eval_field_access_scalar(builder, ctx, r)?,
            // M12 match expression — enum tag dispatch / guard chains,
            // scalar/void/aggregate merges; lowered in codegen/match_.rs.
            // The match's Switch terminator was dispatched when
            // its block was emitted and the join prologue cached the
            // result repr — a cache HIT is served by the memo check at
            // this function's top, so reaching the arm means the match
            // was never emitted (an invariant violation).
            TirTag::Match => {
                return Err(format!(
                    "eval_inst: match %{} reached before its Switch terminator was emitted",
                    r.index()
                ));
            }
            other => {
                return Err(format!(
                    "eval_inst: instruction at %{} is not a value (tag={:?})",
                    r.index(),
                    other
                ));
            }
        };
        // Scalar-only entry point: fat/view-typed insts are
        // rejected above, so no path here can have cached a non-scalar
        // repr for `r` mid-evaluation — except `TirTag::Match`, whose
        // aggregate results cache their family repr at the join
        // prologue (the scalar Value is a dummy there); keep the family
        // repr and skip the scalar cache in that case.
        if Self::cached_repr(ctx, r).is_none() {
            Self::cache_repr(ctx, r, ValueRepr::Scalar(value));
        }
        Ok(value)
    }

    /// Evaluate and lower a scalar numeric binary instruction. Keep the
    /// checked integer guards beside their operations so range-based guard
    /// elision and division overflow behavior stay aligned.
    fn emit_numeric_binary_op(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        tag: TirTag,
        lhs: TirRef,
        rhs: TirRef,
    ) -> Result<Value, String> {
        let lv = Self::eval_inst(builder, ctx, lhs)?;
        let rv = Self::eval_inst(builder, ctx, rhs)?;
        Ok(match tag {
            // Spec §18: signed +,-,* trap on overflow in all build modes.
            // The s*_overflow ops return the wrapped result plus an i8
            // overflow flag; a set flag branches to ryo_panic.
            TirTag::IAdd | TirTag::ISub | TirTag::IMul => Self::emit_int_binop(
                builder,
                ctx,
                tag,
                ranges::int_range_of(ctx.tir, &ctx.range_facts, lhs),
                ranges::int_range_of(ctx.tir, &ctx.range_facts, rhs),
                lv,
                rv,
            )?,
            TirTag::ISDiv => {
                Self::emit_div_guard(
                    builder,
                    ctx,
                    lv,
                    ranges::int_range_of(ctx.tir, &ctx.range_facts, lhs),
                    rv,
                    DIV_ZERO_MSG,
                    DIV_OVERFLOW_MSG,
                )?;
                builder.ins().sdiv(lv, rv)
            }
            TirTag::IMod => {
                Self::emit_div_guard(
                    builder,
                    ctx,
                    lv,
                    ranges::int_range_of(ctx.tir, &ctx.range_facts, lhs),
                    rv,
                    MOD_ZERO_MSG,
                    MOD_OVERFLOW_MSG,
                )?;
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
            _ => unreachable!("non-numeric tag passed to numeric binary lowering: {tag:?}"),
        })
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

    /// Declare an external runtime function by name and return a
    /// `FuncRef` usable in the current function being built. The
    /// module-level import is cached in `Codegen::runtime_fns` (one
    /// `declare_function` per symbol per module); only the cheap
    /// per-function `FuncRef` is derived per call site.
    pub(crate) fn declare_runtime_fn(
        ctx: &mut FunctionContext<'_, M>,
        builder: &mut FunctionBuilder,
        name: &'static str,
        params: &[types::Type],
        returns: &[types::Type],
    ) -> Result<FuncRef, String> {
        let func_id = match ctx.runtime_fns.get(name) {
            Some(&func_id) => func_id,
            None => {
                let mut sig = ctx.module.make_signature();
                for &p in params {
                    sig.params.push(AbiParam::new(p));
                }
                for &r in returns {
                    sig.returns.push(AbiParam::new(r));
                }
                let func_id = ctx
                    .module
                    .declare_function(name, Linkage::Import, &sig)
                    .map_err(|e| format!("Failed to declare {}: {}", name, e))?;
                ctx.runtime_fns.insert(name, func_id);
                func_id
            }
        };
        Ok(ctx.module.declare_func_in_func(func_id, builder.func))
    }

    /// Declare `extern "C" fn ryo_str_free(ptr: *mut u8, cap: u64)` for
    /// the function being built. Returns a `FuncRef` callable via
    /// `builder.ins().call(_, &[ptr, cap])`. `cap == 0` is a runtime
    /// no-op (covers static `.rodata` strings materialized by
    /// `emit_str_literal_fat`).
    pub(crate) fn declare_str_free(
        ctx: &mut FunctionContext<'_, M>,
        builder: &mut FunctionBuilder,
    ) -> Result<FuncRef, String> {
        let int_type = ctx.int_type;
        Self::declare_runtime_fn(ctx, builder, "ryo_str_free", &[int_type, types::I64], &[])
    }

    /// Read a fat binding's current `(ptr, len, cap)`: three loads
    /// from its stack-slot home when it has one, else `use_var` on the
    /// SSA `Variable`s. Returns `None` when the name has no fat binding.
    pub(crate) fn emit_fat_load(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        name: StringId,
    ) -> Option<(Value, Value, Value)> {
        let sl = Self::read_slot(&ctx.fat_locals, name)?;
        if let Some(home) = sl.home {
            let addr = builder.ins().stack_addr(ctx.int_type, home, 0);
            let ptr = builder
                .ins()
                .load(ctx.int_type, MemFlagsData::trusted(), addr, 0);
            let len = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 8);
            let cap = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 16);
            Some((ptr, len, cap))
        } else {
            Some((
                builder.use_var(sl.ptr),
                builder.use_var(sl.len),
                builder.use_var(sl.cap),
            ))
        }
    }

    /// `(ptr, cap)` half of `emit_fat_load` for the free paths, which
    /// never read the length.
    pub(crate) fn emit_fat_load_ptr_cap(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        name: StringId,
    ) -> Option<(Value, Value)> {
        let sl = Self::read_slot(&ctx.fat_locals, name)?;
        Some(Self::load_fat_storage_ptr_cap(builder, ctx.int_type, sl))
    }

    /// Load the storage selected by binding identity, independent of active names.
    pub(super) fn load_fat_storage_ptr_cap(
        builder: &mut FunctionBuilder,
        int_type: Type,
        sl: super::FatLocals,
    ) -> (Value, Value) {
        if let Some(home) = sl.home {
            let addr = builder.ins().stack_addr(int_type, home, 0);
            let ptr = builder
                .ins()
                .load(int_type, MemFlagsData::trusted(), addr, 0);
            let cap = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 16);
            (ptr, cap)
        } else {
            (builder.use_var(sl.ptr), builder.use_var(sl.cap))
        }
    }

    /// The address of a fat binding's home slot, when it has one.
    /// Producers and in-place mutators (`__ryo_*_push`, inout args,
    /// `__ryo_*_ensure_heap`) write through it directly.
    pub(crate) fn fat_home_addr(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        name: StringId,
    ) -> Option<Value> {
        let home = Self::read_slot(&ctx.fat_locals, name)?.home?;
        Some(builder.ins().stack_addr(ctx.int_type, home, 0))
    }

    /// Extract a readable `(ptr, len)` for the byte content of a fat
    /// value whose words may be tagged-inline (SSO). Inline: spill the
    /// three words to a fresh 24-byte scratch slot and hand back its
    /// address plus the tag-encoded len — or, when `inline_addr` names
    /// the value's existing in-memory home (a home-backed binding), use
    /// that address directly with no spill. Heap/static: pass through
    /// unchanged.
    ///
    /// TRANSIENT CONSUMERS ONLY (print, eq, concat operands, push
    /// suffix, conversion args): the returned ptr for an inline value
    /// addresses the scratch slot (or the binding's home). Each spill
    /// allocates a fresh slot, so extractions never clobber each other
    /// — nested evaluation of the next operand cannot overwrite a
    /// pointer that is still live. A home address is equally stable:
    /// consumers only read through it, and the runtime producers that
    /// also write a slot (`__ryo_*_push` on the same binding) document
    /// the inline copy ranges as disjoint. View-creating ops (slice,
    /// ToView) must go through `__ryo_*_ensure_heap` instead
    /// (promote-on-view).
    pub(crate) fn emit_fat_bytes_ptr_len(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        ptr: Value,
        len: Value,
        cap: Value,
        inline_addr: Option<Value>,
    ) -> Result<(Value, Value), String> {
        // Static (.rodata, cap == 0) values are never inline: pass
        // through with no spill and no tag math.
        if Self::is_static_cap_zero(builder.func, cap) {
            return Ok((ptr, len));
        }
        let addr = match inline_addr {
            Some(addr) => addr,
            None => {
                let scratch = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    STR_SLOT_SIZE,
                    3,
                ));
                let addr = builder.ins().stack_addr(ctx.int_type, scratch, 0);
                // Unconditional spill: three stores are cheaper than a
                // branch, and the scratch is written before either
                // select reads it.
                builder.ins().store(MemFlagsData::trusted(), ptr, addr, 0);
                builder.ins().store(MemFlagsData::trusted(), len, addr, 8);
                builder.ins().store(MemFlagsData::trusted(), cap, addr, 16);
                addr
            }
        };
        let tag = builder.ins().ushr_imm_u(cap, 56);
        let tag_bit = builder.ins().band_imm_u(tag, 0x80);
        let is_in = builder.ins().icmp_imm_u(IntCC::NotEqual, tag_bit, 0);
        let in_len = builder.ins().band_imm_u(tag, 0x7f);
        let out_ptr = builder.ins().select(is_in, addr, ptr);
        let out_len = builder.ins().select(is_in, in_len, len);
        Ok((out_ptr, out_len))
    }

    /// Materialize a fat-typed (`str` or `bytes`, M8.4.2) TIR
    /// instruction, returning the `ValueRepr::Str` / `ValueRepr::Bytes`
    /// triple matching the inst's type. Falls back to scalar
    /// `eval_inst` for non-fat instructions.
    pub(crate) fn eval_inst_fat(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<ValueRepr, String> {
        Self::eval_inst_fat_slot(builder, ctx, r, None)
    }

    /// Lower the builtins that produce owned `str` or `bytes` values.
    /// `None` leaves user-call handling to `eval_inst_fat_slot`.
    fn emit_fat_builtin_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        name_id: StringId,
        args: ryo_core::tir::ExtraList<'_, TirRef>,
        out_slot: Option<StackSlot>,
    ) -> Result<Option<ValueRepr>, String> {
        // These ids are interned once per compilation. Keep the dispatch
        // here so the caller's cache and user-call paths stay centralized.
        let ids = ctx.name_ids;
        let repr = if ids.str_from_view == Some(name_id) {
            // `str(view)` materialization evaluates its argument as a view.
            let ValueRepr::View { ptr, len } = Self::eval_inst_view(builder, ctx, args.at(0))?
            else {
                unreachable!("__ryo_str_from_view argument must produce ValueRepr::View")
            };
            let (ptr, len, cap) = Self::emit_slot_out_call(
                builder,
                ctx,
                "ryo_str_from_view",
                &[(ctx.int_type, ptr), (types::I64, len)],
                out_slot,
            )?;
            ValueRepr::Str { ptr, len, cap }
        } else if ids.bytes_from_view == Some(name_id) {
            // `bytes(bview)` materialization evaluates its argument as a view.
            let ValueRepr::View { ptr, len } = Self::eval_inst_view(builder, ctx, args.at(0))?
            else {
                unreachable!("__ryo_bytes_from_view argument must produce ValueRepr::View")
            };
            let (ptr, len, cap) = Self::emit_slot_out_call(
                builder,
                ctx,
                "ryo_bytes_from_view",
                &[(ctx.int_type, ptr), (types::I64, len)],
                out_slot,
            )?;
            ValueRepr::Bytes { ptr, len, cap }
        } else if ids.str_to_bytes == Some(name_id) {
            let (ptr_arg, len_arg) = Self::eval_str_or_view_parts(builder, ctx, args.at(0))?;
            let (ptr, len, cap) = Self::emit_slot_out_call(
                builder,
                ctx,
                "__ryo_str_to_bytes",
                &[(ctx.int_type, ptr_arg), (types::I64, len_arg)],
                out_slot,
            )?;
            ValueRepr::Bytes { ptr, len, cap }
        } else if ids.bytes_to_str == Some(name_id) {
            let (ptr_arg, len_arg) = Self::eval_str_or_view_parts(builder, ctx, args.at(0))?;
            let (ptr, len, cap) = Self::emit_slot_out_call(
                builder,
                ctx,
                "__ryo_bytes_to_str",
                &[(ctx.int_type, ptr_arg), (types::I64, len_arg)],
                out_slot,
            )?;
            ValueRepr::Str { ptr, len, cap }
        } else if ids.bytes_repr == Some(name_id) {
            let (ptr_arg, len_arg) = Self::eval_str_or_view_parts(builder, ctx, args.at(0))?;
            let (ptr, len, cap) = Self::emit_slot_out_call(
                builder,
                ctx,
                "__ryo_bytes_repr",
                &[(ctx.int_type, ptr_arg), (types::I64, len_arg)],
                out_slot,
            )?;
            ValueRepr::Str { ptr, len, cap }
        } else if ids.bool_to_str == Some(name_id) {
            // This inline selects one of two static literals. It never writes
            // an output slot; reject an unexpected slot to catch ABI drift.
            if out_slot.is_some() {
                return Err("bool_to_str is codegen-inlined but was handed an out slot".to_string());
            }
            let cond = Self::eval_inst(builder, ctx, args.at(0))?;
            let true_id =
                Self::store_guard_msg(ctx.module, ctx.data_ctx, ctx.guard_msg_data, "true")?;
            let false_id =
                Self::store_guard_msg(ctx.module, ctx.data_ctx, ctx.guard_msg_data, "false")?;
            let true_ref = ctx.module.declare_data_in_func(true_id, builder.func);
            let false_ref = ctx.module.declare_data_in_func(false_id, builder.func);
            let true_ptr = builder.ins().symbol_value(ctx.int_type, true_ref);
            let false_ptr = builder.ins().symbol_value(ctx.int_type, false_ref);
            let ptr = builder.ins().select(cond, true_ptr, false_ptr);
            let four = builder.ins().iconst(types::I64, 4);
            let five = builder.ins().iconst(types::I64, 5);
            let len = builder.ins().select(cond, four, five);
            let cap = builder.ins().iconst(types::I64, 0);
            ValueRepr::Str { ptr, len, cap }
        } else if ids.int_to_str == Some(name_id) || ids.float_to_str == Some(name_id) {
            let arg_val = Self::eval_inst(builder, ctx, args.at(0))?;
            let (fn_name, param_ty) = if ids.int_to_str == Some(name_id) {
                ("ryo_int_to_str", ctx.int_type)
            } else {
                ("ryo_float_to_str", types::F64)
            };
            let (ptr, len, cap) =
                Self::emit_slot_out_call(builder, ctx, fn_name, &[(param_ty, arg_val)], out_slot)?;
            ValueRepr::Str { ptr, len, cap }
        } else if ids.process_argv == Some(name_id) {
            // Interim form until process.args replaces process_argv.
            let arg_val = Self::eval_inst(builder, ctx, args.at(0))?;
            let (ptr, len, cap) = Self::emit_process_argv(builder, ctx, arg_val, out_slot)?;
            ValueRepr::Str { ptr, len, cap }
        } else if ids.process_env == Some(name_id) {
            // Interim form until process.env replaces process_env. The key
            // bytes are consumed by ryo_getenv immediately after extraction.
            let (key_ptr, key_len) = Self::eval_str_or_view_parts(builder, ctx, args.at(0))?;
            let (ptr, len, cap) = Self::emit_slot_out_call_out_last(
                builder,
                ctx,
                "ryo_getenv",
                &[(ctx.int_type, key_ptr), (types::I64, key_len)],
                out_slot,
            )?;
            ValueRepr::Str { ptr, len, cap }
        } else if ids.io_read_line == Some(name_id) {
            // Interim arg-less form until io.read_line returns IoError!str.
            let (ptr, len, cap) = Self::emit_io_read_line(builder, ctx, out_slot)?;
            ValueRepr::Str { ptr, len, cap }
        } else {
            return Ok(None);
        };
        Ok(Some(repr))
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
                // Promote-on-view: an inline (SSO) base's bytes live in
                // its slot; the view must point at memory that never
                // moves, so owners go through ensure_heap first.
                let (base_ptr, base_len) =
                    Self::emit_ensure_heap_for_view_base(builder, ctx, base)?;
                let start_v = match start {
                    Some(s) => Self::eval_inst(builder, ctx, s)?,
                    None => builder.ins().iconst(types::I64, 0),
                };
                let end_v = match end {
                    Some(e) => Self::eval_inst(builder, ctx, e)?,
                    None => base_len,
                };
                // M8.4.2: bytes slices skip the UTF-8 boundary check —
                // the inline emission selects on the result view type.
                let is_bytes = matches!(ctx.pool.kind(inst.ty), TypeKind::View(ViewKind::Bytes));
                let (ptr, len) = Self::emit_slice_inline(
                    builder, ctx, base_ptr, base_len, start_v, end_v, is_bytes,
                )?;
                ValueRepr::View { ptr, len }
            }
            TirTag::ToView => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ToView must carry TirData::UnOp"),
                };
                // Promote-on-view for owner operands: the view must
                // address stable memory (see emit_ensure_heap_for_view_base).
                let (ptr, len) = Self::emit_ensure_heap_for_view_base(builder, ctx, operand)?;
                ValueRepr::View { ptr, len }
            }
            TirTag::Var => {
                let name = match inst.data {
                    TirData::Var(name) => name,
                    _ => unreachable!("Var must carry TirData::Var"),
                };
                let locals = Self::read_slot(&ctx.view_locals, name).ok_or_else(|| {
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

    /// Materialize every distinct string/bytes literal exactly once, in
    /// the entry block, and pre-seed the `TirRef → ValueRepr` memo so each
    /// use reads the hoisted triple. A literal is pure .rodata packing
    /// (`symbol_value` + `iconst` constants — no call), so entry-block
    /// materialization is sound — the entry block dominates
    /// every use — and keeps loop bodies from re-packing the same
    /// (ptr, len) per iteration.
    ///
    /// The memo is keyed by `(is_bytes, StringId)`: a `str` `"A"` and a
    /// `bytes` `b"A"` share one `StringId` (same byte content, Task 1
    /// dedup) but need different `ValueRepr` variants.
    ///
    /// `StrConst` args of `__ryo_panic` are excluded: `emit_call`
    /// consumes them through the raw (ptr, len) path and never touches
    /// the memo, so hoisting them would add a dead call to the hot
    /// path of every function that panics.
    ///
    /// Runs while the entry block is still the builder's current
    /// block (called from `compile_function` right before
    /// `emit_body`).
    ///
    /// Cleanup uses Flow positions and remains independent of literal hoisting.
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
            if ctx.name_ids.ryo_panic == Some(view.name) {
                for a in &view.args {
                    panic_args[a.index()] = true;
                }
            }
        }
        let mut hoisted: HashMap<(bool, StringId), ValueRepr> = HashMap::new();
        let tir = ctx.tir;
        for (idx, inst) in tir.instructions.iter().enumerate().skip(1) {
            let is_bytes = match inst.tag {
                TirTag::StrConst => false,
                TirTag::BytesConst => true,
                _ => continue,
            };
            if panic_args[idx] {
                continue;
            }
            let TirData::Str(id) = inst.data else {
                continue;
            };
            let repr = match hoisted.get(&(is_bytes, id)) {
                Some(repr) => *repr,
                None => {
                    let repr = if is_bytes {
                        Self::emit_bytes_literal_fat(builder, ctx, id)?
                    } else {
                        Self::emit_str_literal_fat(builder, ctx, id)?
                    };
                    hoisted.insert((is_bytes, id), repr);
                    repr
                }
            };
            ctx.inst_values[idx] = Some(repr);
        }
        Ok(())
    }

    /// Emit a string literal as a fat pointer triple (ptr, len, cap=0):
    /// the `.rodata` data pointer, the compile-time length, and the
    /// static cap-0 sentinel — pure constants, no runtime call.
    /// `pub(crate)` for the M12 match codegen: literal-arm compares and
    /// anon-arm str guards materialize their literal side through this.
    pub(crate) fn emit_str_literal_fat(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        id: StringId,
    ) -> Result<ValueRepr, String> {
        let content = ctx.pool.str(id);
        let data_id = store_string(id, content, ctx.module, ctx.data_ctx, ctx.string_data)?;
        let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
        let rodata_ptr = builder.ins().symbol_value(ctx.int_type, data_ref);
        let lit_len = builder.ins().iconst(types::I64, content.len() as i64);
        let cap = builder.ins().iconst(types::I64, 0);
        Ok(ValueRepr::Str {
            ptr: rodata_ptr,
            len: lit_len,
            cap,
        })
    }

    pub(super) fn emit_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        Self::emit_call_slot(builder, ctx, r, None)
    }

    /// `emit_call` with a caller-provided sret destination: a
    /// fat-returning call writes `out_slot` directly instead of a
    /// fresh temp slot (see `eval_inst_fat_slot`).
    pub(super) fn emit_call_slot(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        out_slot: Option<StackSlot>,
    ) -> Result<Value, String> {
        let view = ctx.tir.call_view(r);
        let name_id = view.name;
        let name_str = ctx.pool.str(name_id);
        // Call-form dispatch compares interned ids (resolved once per
        // compilation in `CodegenNameIds`); `name_str` remains only for
        // the user-function path below.
        let ids = ctx.name_ids;

        // print and __ryo_panic are ordinary runtime calls. They
        // do NOT use the str-triple expansion that user functions use.
        if ids.ryo_panic == Some(name_id) {
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
                match ctx.tir.inst(arg).data {
                    TirData::Str(id) => {
                        arg_values.push(Self::emit_strconst_rodata_ptr(builder, ctx, id)?)
                    }
                    _ => arg_values.push(Self::eval_inst(builder, ctx, arg)?),
                }
            }
            let panic_ref = Self::declare_runtime_fn(
                ctx,
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

        // TODO(M24): interim call form — replaced by `process.exit`.
        if ids.process_exit == Some(name_id) {
            return Self::emit_process_exit(builder, ctx, r);
        }

        if ids.print == Some(name_id) {
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
                    ctx.pool.kind(ctx.tir.inst(view.args.at(0)).ty),
                    TypeKind::Str | TypeKind::View(_)
                ),
                "sema should reject non-str print() args",
            );
            let (ptr, len) = Self::eval_str_or_view_parts(builder, ctx, view.args.at(0))?;
            let print_ref = Self::declare_runtime_fn(
                ctx,
                builder,
                "ryo_print",
                &[ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(print_ref, &[ptr, len]);
            return Ok(builder.ins().iconst(ctx.int_type, 0));
        }

        // TODO(M24): interim call form — replaced by `io.eprint`.
        if ids.io_eprint == Some(name_id) {
            return Self::emit_io_eprint(builder, ctx, r);
        }

        // TODO(M22): interim call form — replaced by `process.args`.
        if ids.process_argc == Some(name_id) {
            return Self::emit_process_argc(builder, ctx, r);
        }

        if ids.str_push == Some(name_id) {
            // str_push(&s, suffix): spill s's fat pointer to a 24-byte
            // slot, call __ryo_str_push(slot_addr, suffix_ptr, suffix_len),
            // then reload the mutated triple back into s's FatLocals.
            // A home-backed binding skips the spill+reload entirely:
            // the runtime mutates its home slot in place. arg 0 is
            // `&s` (lowered to Var(s)); arg 1 is the suffix str.
            let s_ref = view.args.at(0);
            let suffix_ref = view.args.at(1);
            // A field place (`str_push(&s.field, ...)`) already lives in
            // the root struct's stack slot — pass its address directly
            // (same invariant as the generic inout call path): the
            // runtime mutates the field in place, so no snapshot, spill,
            // or reload is needed.
            if matches!(ctx.tir.inst(s_ref).data, TirData::FieldAccess { .. }) {
                let addr = Self::inout_pointee_addr(builder, ctx, s_ref)?;
                let (suf_ptr, suf_len) = Self::eval_str_or_view_parts(builder, ctx, suffix_ref)?;
                let func_ref = Self::declare_runtime_fn(
                    ctx,
                    builder,
                    "__ryo_str_push",
                    &[ctx.int_type, ctx.int_type, types::I64],
                    &[],
                )?;
                builder.ins().call(func_ref, &[addr, suf_ptr, suf_len]);
                return Ok(builder.ins().iconst(ctx.int_type, 0));
            }
            let home_addr =
                Self::local_name_of(ctx, s_ref).and_then(|n| Self::fat_home_addr(builder, ctx, n));
            let s_repr = Self::eval_inst_fat(builder, ctx, s_ref)?;
            let ValueRepr::Str { ptr, len, cap } = s_repr else {
                unreachable!("str_push target must be a str");
            };
            let s_addr = match home_addr {
                Some(addr) => addr,
                None => {
                    let slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        STR_SLOT_SIZE,
                        3,
                    ));
                    let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                    builder.ins().store(MemFlagsData::trusted(), ptr, addr, 0);
                    builder.ins().store(MemFlagsData::trusted(), len, addr, 8);
                    builder.ins().store(MemFlagsData::trusted(), cap, addr, 16);
                    addr
                }
            };
            // M8.4: the suffix may be either repr — an owned `str`
            // passes its ptr+len, a slice/view passes directly (no
            // ToView wrap: builtins bypass check_call's §3.4
            // conversion, so sema accepts `Str | View(_)` here).
            let (suf_ptr, suf_len) = Self::eval_str_or_view_parts(builder, ctx, suffix_ref)?;
            let func_ref = Self::declare_runtime_fn(
                ctx,
                builder,
                "__ryo_str_push",
                &[ctx.int_type, ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(func_ref, &[s_addr, suf_ptr, suf_len]);
            // The push may have heap-promoted the buffer in place —
            // the home's provenance no longer holds.
            if let Some(name) = Self::local_name_of(ctx, s_ref) {
                Self::set_home_inline(ctx, name, false);
            }
            // Reload the mutated fat pointer back into the caller's
            // FatLocals — home-backed bindings read the home directly,
            // so only the SSA flavor needs the reload.
            if home_addr.is_none() {
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
                    && let Some(sl) = Self::read_slot(&ctx.fat_locals, name)
                {
                    builder.def_var(sl.ptr, np);
                    builder.def_var(sl.len, nl);
                    builder.def_var(sl.cap, nc);
                }
            }
            return Ok(builder.ins().iconst(ctx.int_type, 0));
        }

        if ids.bytes_push == Some(name_id) {
            // bytes_push(&b, x): spill b's fat pointer to a 24-byte
            // slot, call __ryo_bytes_push(slot_addr, x), then reload
            // the mutated triple back into b's FatLocals. A home-backed
            // binding skips the spill+reload: the runtime mutates its
            // home slot in place. arg 0 is `&b` (lowered to Var(b));
            // arg 1 is the int byte value.
            // The 0-255 range check is runtime-side (M8.4.2 stopgap).
            let b_ref = view.args.at(0);
            let x_ref = view.args.at(1);
            // Field place (`bytes_push(&b.field, ...)`) — see the
            // str_push arm: pass the field's address in the root
            // struct's slot directly; the runtime mutates in place.
            if matches!(ctx.tir.inst(b_ref).data, TirData::FieldAccess { .. }) {
                let addr = Self::inout_pointee_addr(builder, ctx, b_ref)?;
                let x_val = Self::eval_inst(builder, ctx, x_ref)?;
                let func_ref = Self::declare_runtime_fn(
                    ctx,
                    builder,
                    "__ryo_bytes_push",
                    &[ctx.int_type, types::I64],
                    &[],
                )?;
                builder.ins().call(func_ref, &[addr, x_val]);
                return Ok(builder.ins().iconst(ctx.int_type, 0));
            }
            let home_addr =
                Self::local_name_of(ctx, b_ref).and_then(|n| Self::fat_home_addr(builder, ctx, n));
            let b_repr = Self::eval_inst_fat(builder, ctx, b_ref)?;
            let ValueRepr::Bytes { ptr, len, cap } = b_repr else {
                unreachable!("bytes_push target must be bytes");
            };
            let b_addr = match home_addr {
                Some(addr) => addr,
                None => {
                    let slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        STR_SLOT_SIZE,
                        3,
                    ));
                    let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                    builder.ins().store(MemFlagsData::trusted(), ptr, addr, 0);
                    builder.ins().store(MemFlagsData::trusted(), len, addr, 8);
                    builder.ins().store(MemFlagsData::trusted(), cap, addr, 16);
                    addr
                }
            };
            let x_val = Self::eval_inst(builder, ctx, x_ref)?;
            let func_ref = Self::declare_runtime_fn(
                ctx,
                builder,
                "__ryo_bytes_push",
                &[ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(func_ref, &[b_addr, x_val]);
            // The push may have heap-promoted the buffer in place —
            // the home's provenance no longer holds.
            if let Some(name) = Self::local_name_of(ctx, b_ref) {
                Self::set_home_inline(ctx, name, false);
            }
            // Reload the mutated fat pointer back into the caller's
            // FatLocals — home-backed bindings read the home directly.
            if home_addr.is_none() {
                let np = builder
                    .ins()
                    .load(ctx.int_type, MemFlagsData::trusted(), b_addr, 0);
                let nl = builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), b_addr, 8);
                let nc = builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), b_addr, 16);
                if let Some(name) = Self::local_name_of(ctx, b_ref)
                    && let Some(sl) = Self::read_slot(&ctx.fat_locals, name)
                {
                    builder.def_var(sl.ptr, np);
                    builder.def_var(sl.len, nl);
                    builder.def_var(sl.cap, nc);
                }
            }
            return Ok(builder.ins().iconst(ctx.int_type, 0));
        }

        let (callee_id, return_abi) = ctx
            .func_ids
            .get(&name_id)
            .map(|(id, abi)| (*id, abi.result))
            .ok_or_else(|| format!("Undefined function: '{}'", name_str))?;

        let marshalled = Self::marshal_user_call_args(builder, ctx, r, out_slot)?;
        // Guard recursive (call-graph cycle) calls with the
        // stack-limit check, right before the call instruction.
        Self::maybe_emit_stack_check(builder, ctx, name_id)?;
        let callee_ref = ctx.module.declare_func_in_func(callee_id, builder.func);
        let ret_ty = ctx.tir.inst(r).ty;

        // If the callee returns never (e.g. __ryo_panic), the call is
        // a terminator. Emit a trap + dead block for subsequent IR.
        if ctx.pool.is_never(ret_ty) {
            builder.ins().call(callee_ref, &marshalled.values);
            // Reload inout slots before the trap: Cranelift models the
            // callee as an ordinary (returning) call, so the mutations
            // must be visible on the path where control resumes.
            Self::reload_inout_args(builder, ctx, &marshalled.inout_reloads)?;
            builder.ins().trap(
                TrapCode::user(1).expect("user trap code 1 is within Cranelift's encodable range"),
            );
            let dead = builder.create_block();
            builder.seal_block(dead);
            builder.switch_to_block(dead);
            let dummy_ty = cranelift_type_for(ret_ty, ctx.pool, ctx.int_type);
            return Ok(builder.ins().iconst(dummy_ty, 0));
        }

        if return_abi == super::ValueAbi::Fat {
            // sret: the 24-byte slot (or the caller-provided binding
            // home) was allocated by marshalling; the pointer is already
            // prepended to `values`.
            let out = marshalled
                .sret
                .expect("fat-returning call must marshal an sret pointer");

            builder.ins().call(callee_ref, &marshalled.values);
            Self::reload_inout_args(builder, ctx, &marshalled.inout_reloads)?;

            let ptr = builder
                .ins()
                .load(ctx.int_type, MemFlagsData::trusted(), out, 0);
            let len = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), out, 8);
            let cap = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), out, 16);
            let repr = if matches!(ctx.pool.kind(ret_ty), TypeKind::Bytes) {
                ValueRepr::Bytes { ptr, len, cap }
            } else {
                ValueRepr::Str { ptr, len, cap }
            };
            Self::cache_repr(ctx, r, repr);
            return Ok(ptr); // dummy scalar — consumers use eval_inst_fat
        }

        if return_abi == super::ValueAbi::Struct {
            // M9 named / M10 anon sret: the struct's slot was allocated
            // by marshalling; the slot address is the result (mirrors
            // the fat sret path above).
            let out = marshalled
                .sret
                .expect("struct-returning call must marshal an sret pointer");

            builder.ins().call(callee_ref, &marshalled.values);
            Self::reload_inout_args(builder, ctx, &marshalled.inout_reloads)?;

            Self::cache_repr(ctx, r, ValueRepr::Struct { addr: out });
            return Ok(out); // dummy scalar — consumers use eval_inst_struct
        }

        if return_abi == super::ValueAbi::Enum {
            // M11 enum sret: the enum's slot was allocated by
            // marshalling; the slot address is the result (mirrors the
            // struct sret path above).
            let out = marshalled
                .sret
                .expect("enum-returning call must marshal an sret pointer");

            builder.ins().call(callee_ref, &marshalled.values);
            Self::reload_inout_args(builder, ctx, &marshalled.inout_reloads)?;

            Self::cache_repr(ctx, r, ValueRepr::Enum { addr: out });
            return Ok(out); // dummy scalar — consumers use eval_inst_enum
        }

        let call = builder.ins().call(callee_ref, &marshalled.values);
        Self::reload_inout_args(builder, ctx, &marshalled.inout_reloads)?;
        let results = builder.inst_results(call);

        if results.is_empty() {
            Ok(builder.ins().iconst(ctx.int_type, 0))
        } else {
            Ok(results[0])
        }
    }

    /// Arg marshalling for the user-function call path of
    /// [`Self::emit_call_slot`], extracted so the tail-call path
    /// can marshal the same way and then emit `return_call` instead of
    /// `call`. Evaluates every arg into its ABI form (scalar value,
    /// fat triple, view pair, struct slot address, or inout spill slot)
    /// and prepends the hidden sret pointer for fat/struct-returning
    /// callees. Behavior is identical to the inline sequence it
    /// replaces.
    pub(super) fn marshal_user_call_args(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        out_slot: Option<StackSlot>,
    ) -> Result<MarshalledArgs, String> {
        let view = ctx.tir.call_view(r);
        let name_id = view.name;
        let name_str = ctx.pool.str(name_id);
        // Defensive duplicate of the undefined-function check: the
        // tail path (try_emit_tail_call) marshals before its own
        // func_ids lookup, so the error must surface here, with the
        // same message. emit_call_slot also checks beforehand.
        let abi = &ctx
            .func_ids
            .get(&name_id)
            .map(|(_, abi)| abi)
            .ok_or_else(|| format!("Undefined function: '{}'", name_str))?;

        let mut values = Vec::with_capacity(view.args.len() * 3 + 1);
        // inout args: spill the current value to a stack slot, pass the
        // slot address, then reload after the call. Scalar spills one
        // field; fat owners spill the fat-pointer triple.
        let mut inout_reloads: Vec<(TirRef, StackSlot)> = Vec::new();
        for (i, arg) in view.args.iter().enumerate() {
            let mode = view.modes.get(i).ok_or_else(|| {
                format!(
                    "internal error: call '{name_str}' has {} args but {} modes",
                    view.args.len(),
                    view.modes.len()
                )
            })?;
            let arg_ty = ctx.tir.inst(arg).ty;
            let lowered = abi
                .params
                .get(i)
                .ok_or_else(|| format!("call '{name_str}' has too many arguments"))?;
            if lowered.ty != arg_ty || lowered.mode != mode {
                return Err(format!(
                    "call '{name_str}' disagrees with the lowered callee ABI"
                ));
            }
            if mode == ParamMode::Inout {
                if matches!(ctx.tir.inst(arg).data, TirData::FieldAccess { .. })
                    || lowered.value == super::ValueAbi::Struct
                    || lowered.value == super::ValueAbi::Enum
                {
                    // M9 inout field path (`&p.x`), whole-struct inout
                    // (`&p`), or whole-enum inout (M11, `&e`): the
                    // pointee already lives in the caller's stack slot —
                    // pass its address directly so the callee mutates
                    // in place. No spill, no reload, no write-back.
                    let addr = Self::inout_pointee_addr(builder, ctx, arg)?;
                    values.push(addr);
                } else if lowered.value == super::ValueAbi::Fat {
                    let repr = Self::eval_inst_fat(builder, ctx, arg)?;
                    let (ptr, len, cap) = match repr {
                        ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                            (ptr, len, cap)
                        }
                        _ => unreachable!("inout fat arg must produce a fat ValueRepr"),
                    };
                    // Home-backed binding: the callee mutates the home
                    // slot in place — no spill, no reload.
                    let arg_name = Self::local_name_of(ctx, arg);
                    let home_addr = arg_name.and_then(|n| Self::fat_home_addr(builder, ctx, n));
                    match home_addr {
                        Some(addr) => {
                            // The callee may have written anything
                            // through the pointer — the binding's range
                            // fact dies here (same as the reload path),
                            // and the home's provenance no longer holds.
                            if let Some(name) = arg_name {
                                Self::kill_fact(ctx, name);
                                Self::set_home_inline(ctx, name, false);
                            }
                            values.push(addr);
                        }
                        None => {
                            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                                StackSlotKind::ExplicitSlot,
                                STR_SLOT_SIZE,
                                3,
                            ));
                            let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                            builder.ins().store(MemFlagsData::trusted(), ptr, addr, 0);
                            builder.ins().store(MemFlagsData::trusted(), len, addr, 8);
                            builder.ins().store(MemFlagsData::trusted(), cap, addr, 16);
                            values.push(addr);
                            inout_reloads.push((arg, slot));
                        }
                    }
                } else {
                    let cl_ty = cranelift_type_for(arg_ty, ctx.pool, ctx.int_type);
                    let bytes = cl_ty.bytes().max(8);
                    let slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        bytes,
                        3,
                    ));
                    let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                    let cur = Self::eval_inst(builder, ctx, arg)?;
                    builder.ins().store(MemFlagsData::trusted(), cur, addr, 0);
                    values.push(addr);
                    inout_reloads.push((arg, slot));
                }
            } else if lowered.value == super::ValueAbi::Fat {
                let repr = Self::eval_inst_fat(builder, ctx, arg)?;
                match repr {
                    ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                        values.push(ptr);
                        values.push(len);
                        values.push(cap);
                    }
                    _ => unreachable!("fat-typed arg must produce a fat ValueRepr"),
                }
            } else if lowered.value == super::ValueAbi::View {
                // `strview` arg → 2-word ABI (ptr, len), matching the
                // callee's build_signature. Sema has already inserted
                // ToView for owned-str actuals (§3.4).
                let (ptr, len) = Self::eval_str_or_view_parts(builder, ctx, arg)?;
                values.push(ptr);
                values.push(len);
            } else if lowered.value == super::ValueAbi::Struct {
                // M9 named / M10 anon struct arg: a single slot
                // address — the existing slot for Borrow, a fresh
                // field-wise copy for Move/Copy.
                let addr = Self::emit_struct_call_arg(builder, ctx, arg, mode)?;
                values.push(addr);
            } else if lowered.value == super::ValueAbi::Enum {
                // M11 enum arg: a single slot address — the existing
                // slot for Borrow, a fresh tag + active-payload copy
                // for Move/Copy.
                let addr = Self::emit_enum_call_arg(builder, ctx, arg, mode)?;
                values.push(addr);
            } else {
                values.push(Self::eval_inst(builder, ctx, arg)?);
            }
        }

        // sret prepend: fat-returning callees write the (ptr, len, cap)
        // triple through a hidden first pointer (the caller-provided
        // binding home when there is one); struct-returning callees
        // write the aggregate through a fresh struct slot. Scalar and
        // void callees have no sret word.
        let ret_ty = ctx.tir.inst(r).ty;
        let sret = if abi.result == super::ValueAbi::Fat {
            let slot = out_slot.unwrap_or_else(|| {
                builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    STR_SLOT_SIZE,
                    3,
                ))
            });
            let out = builder.ins().stack_addr(ctx.int_type, slot, 0);
            values.insert(0, out);
            Some(out)
        } else if abi.result == super::ValueAbi::Struct {
            let slot = Self::struct_slot(builder, ctx, ret_ty);
            let out = builder.ins().stack_addr(ctx.int_type, slot, 0);
            values.insert(0, out);
            Some(out)
        } else if abi.result == super::ValueAbi::Enum {
            let slot = Self::enum_slot(builder, ctx, ret_ty);
            let out = builder.ins().stack_addr(ctx.int_type, slot, 0);
            values.insert(0, out);
            Some(out)
        } else {
            None
        };
        if values.len() != abi.signature.params.len() || ret_ty != abi.result_type {
            return Err(format!(
                "call '{name_str}' has an incompatible argument or return layout"
            ));
        }
        Ok(MarshalledArgs {
            values,
            sret,
            inout_reloads,
        })
    }

    /// Consuming-concat fast path: `s = s + suffix` where the ownership
    /// pass has proven the lhs binding dies at this reassign (Valid
    /// owner, no live views, rhs not aliasing). Append the rhs onto the
    /// lhs buffer in place via `__ryo_str_push` and reload — no fresh
    /// allocation, and no free of the old buffer (it was CONSUMED:
    /// `free_on_reassign` for this Assign is deliberately skipped by
    /// never reaching the shared Assign code).
    pub(crate) fn emit_consuming_concat_assign(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        assign_ref: TirRef,
        concat_ref: TirRef,
    ) -> Result<Terminator, String> {
        let view = ctx.tir.assign_view(assign_ref);
        let concat = ctx.tir.inst(concat_ref);
        let (lhs, rhs) = match concat.data {
            TirData::BinOp { lhs, rhs } => (lhs, rhs),
            _ => unreachable!("consumed_concat_lhs must key a BinOp concat"),
        };
        let lhs_name = match ctx.tir.inst(lhs).data {
            TirData::Var(n) => n,
            _ => unreachable!("sidecar guarantees a Var lhs"),
        };
        debug_assert_eq!(
            lhs_name, view.name,
            "consuming concat must target the reassigned binding itself"
        );
        // The rhs bytes are consumed by the push call — transient
        // extraction is sound (eval_str_or_view_parts contract).
        let (r_ptr, r_len) = Self::eval_str_or_view_parts(builder, ctx, rhs)?;
        let locals = Self::read_slot(&ctx.fat_locals, lhs_name).ok_or_else(|| {
            format!(
                "Undefined fat variable in consuming concat: '{}'",
                ctx.pool.str(lhs_name)
            )
        })?;
        let binding = ctx.sidecar.assign_binding[assign_ref.index()]
            .ok_or_else(|| "fat assignment has no declaring binding".to_string())?;
        Self::require_fat_storage_binding(locals, binding)?;
        // Home-backed binding: push mutates the home in place — no
        // spill, and the "reload" loads below only feed the cached
        // representation that later consumers read.
        let addr = match Self::fat_home_addr(builder, ctx, lhs_name) {
            Some(addr) => addr,
            None => {
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    STR_SLOT_SIZE,
                    3,
                ));
                let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                let old_ptr = builder.use_var(locals.ptr);
                let old_len = builder.use_var(locals.len);
                let old_cap = builder.use_var(locals.cap);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), old_ptr, addr, 0);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), old_len, addr, 8);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), old_cap, addr, 16);
                addr
            }
        };
        // __ryo_str_push serves both families: the tagged-slot layout
        // is shared, and appending valid-UTF-8 + valid-UTF-8 stays
        // valid (no boundary check needed).
        let push_ref = Self::declare_runtime_fn(
            ctx,
            builder,
            "__ryo_str_push",
            &[ctx.int_type, ctx.int_type, types::I64],
            &[],
        )?;
        builder.ins().call(push_ref, &[addr, r_ptr, r_len]);
        // Appending may heap-promote in place — the home's provenance
        // no longer holds.
        Self::set_home_inline(ctx, lhs_name, false);
        let np = builder
            .ins()
            .load(ctx.int_type, MemFlagsData::trusted(), addr, 0);
        let nl = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), addr, 8);
        let nc = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), addr, 16);
        // Home-backed bindings hold the mutated triple in the slot
        // already; only the SSA flavor needs the reload.
        if locals.home.is_none() {
            builder.def_var(locals.ptr, np);
            builder.def_var(locals.len, nl);
            builder.def_var(locals.cap, nc);
        }
        // The concat inst stands in for the value the binding now
        // holds. Consumers read this representation; Flow positions fire cleanup.
        let repr = if matches!(ctx.pool.kind(concat.ty), TypeKind::Bytes) {
            ValueRepr::Bytes {
                ptr: np,
                len: nl,
                cap: nc,
            }
        } else {
            ValueRepr::Str {
                ptr: np,
                len: nl,
                cap: nc,
            }
        };
        Self::cache_repr(ctx, concat_ref, repr);
        Self::kill_fact(ctx, view.name);
        Ok(Terminator::None)
    }

    /// Reload each inout slot after a call and write the updated value
    /// back into the caller's local. The inout arg was sema-lowered to
    /// its inner `Var(name)` ref, so `*arg_ref` is that `Var` inst —
    /// read its binding name to find the local. Scalar args reload one
    /// field into `locals`; fat args reload the fat-pointer triple into
    /// `fat_locals`.
    fn reload_inout_args(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        reloads: &[(TirRef, StackSlot)],
    ) -> Result<(), String> {
        for (arg_ref, slot) in reloads {
            let addr = builder.ins().stack_addr(ctx.int_type, *slot, 0);
            let arg_ty = ctx.tir.inst(*arg_ref).ty;
            if is_fat_type(arg_ty, ctx.pool) {
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
                    if let Some(sl) = Self::read_slot(&ctx.fat_locals, name) {
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
                    if let Some(var) = Self::read_slot(&ctx.locals, name) {
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
    pub(crate) fn local_name_of(ctx: &FunctionContext<'_, M>, r: TirRef) -> Option<StringId> {
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
