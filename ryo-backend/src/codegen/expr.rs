//! Expression evaluation and call emission — split from `mod.rs`; see module docs there.

use super::arith::{DIV_OVERFLOW_MSG, DIV_ZERO_MSG, MOD_OVERFLOW_MSG, MOD_ZERO_MSG};
use super::bytes::store_string;
use super::{
    Codegen, FunctionContext, OVERFLOW_MSG, STR_SLOT_SIZE, Terminator, ValueRepr,
    cranelift_type_for, is_fat_type, ranges,
};
use cranelift::codegen::ir::{BlockArg, FuncRef, MemFlagsData, StackSlot};
use cranelift::prelude::*;
use cranelift_module::{Linkage, Module};
use ryo_core::tir::{ParamMode, TirData, TirRef, TirTag};
use ryo_core::types::{StringId, TypeKind, ViewKind};
use std::collections::HashMap;

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
                // Fat/view-typed values have no scalar stand-in.
                // A multi-word repr reaching the scalar entry point
                // means a consumer forgot to gate through eval_inst_fat
                // / eval_inst_view — reject loudly instead of silently
                // handing out the data pointer.
                ValueRepr::Str { .. }
                | ValueRepr::Bytes { .. }
                | ValueRepr::View { .. }
                | ValueRepr::Struct { .. } => Err(format!(
                    "eval_inst: fat/view/struct-typed inst %{} reached the scalar entry point; use eval_inst_fat / eval_inst_view / eval_inst_struct",
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
                Self::emit_str_eq(builder, ctx, inst.tag, lhs, rhs)?
            }
            TirTag::BytesCmpEq | TirTag::BytesCmpNe => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!(),
                };
                Self::emit_bytes_eq(builder, ctx, inst.tag, lhs, rhs)?
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
        if let Some(home) = sl.home {
            let addr = builder.ins().stack_addr(ctx.int_type, home, 0);
            let ptr = builder
                .ins()
                .load(ctx.int_type, MemFlagsData::trusted(), addr, 0);
            let cap = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 16);
            Some((ptr, cap))
        } else {
            Some((builder.use_var(sl.ptr), builder.use_var(sl.cap)))
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

    /// Call a slot-out runtime producer: allocate a 24-byte slot (or
    /// use the caller-provided `out_slot` — a fat binding's canonical
    /// home when the result initializes one), pass its address as
    /// arg 0, then load the tagged (ptr, len, cap) triple. The runtime
    /// writes the full slot (SSO tag, headroom cap) — codegen never
    /// derives cap anymore.
    ///
    /// The reload loads run either way: their values feed the
    /// `ValueRepr` cache the free sweep keys on. For a home-backed
    /// binding they are short-lived (never `def_var`'d), so regalloc
    /// never grows a second spill slot next to the home.
    pub(crate) fn emit_slot_out_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        fn_name: &'static str,
        args: &[(Type, Value)],
        out_slot: Option<StackSlot>,
    ) -> Result<(Value, Value, Value), String> {
        let slot = out_slot.unwrap_or_else(|| {
            builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                STR_SLOT_SIZE,
                3,
            ))
        });
        let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
        let mut param_tys = Vec::with_capacity(args.len() + 1);
        param_tys.push(ctx.int_type);
        param_tys.extend(args.iter().map(|(ty, _)| *ty));
        let func_ref = Self::declare_runtime_fn(ctx, builder, fn_name, &param_tys, &[])?;
        let mut call_args = Vec::with_capacity(args.len() + 1);
        call_args.push(addr);
        call_args.extend(args.iter().map(|(_, v)| *v));
        builder.ins().call(func_ref, &call_args);
        let ptr = builder
            .ins()
            .load(ctx.int_type, MemFlagsData::trusted(), addr, 0);
        let len = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), addr, 8);
        let cap = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), addr, 16);
        Ok((ptr, len, cap))
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

    /// `eval_inst_fat` with a caller-provided slot-out destination:
    /// when `r` is a producer call (runtime slot-out producer, concat,
    /// or fat-returning user call) the producer writes `out_slot`
    /// directly instead of a fresh temp. Used by the fat VarDecl/Assign
    /// paths to write a home-backed binding in a single store round.
    /// Ignored for non-producer instructions (literals, Var reads,
    /// view-as-owner) — they never emit a slot-out call.
    pub(crate) fn eval_inst_fat_slot(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        out_slot: Option<StackSlot>,
    ) -> Result<ValueRepr, String> {
        if let Some(repr) = Self::cached_repr(ctx, r) {
            return Ok(repr);
        }
        let inst = ctx.tir.inst(r);
        let repr = match inst.tag {
            TirTag::StrConst => {
                let id = match inst.data {
                    TirData::Str(id) => id,
                    _ => unreachable!("StrConst must carry TirData::Str"),
                };
                Self::emit_str_literal_fat(builder, ctx, id)?
            }
            TirTag::BytesConst => {
                let id = match inst.data {
                    TirData::Str(id) => id,
                    _ => unreachable!("BytesConst must carry TirData::Str"),
                };
                Self::emit_bytes_literal_fat(builder, ctx, id)?
            }
            TirTag::Var => {
                let name = match inst.data {
                    TirData::Var(name) => name,
                    _ => unreachable!(),
                };
                if Self::read_slot(&ctx.fat_locals, name).is_some() {
                    let (ptr, len, cap) = Self::emit_fat_load(builder, ctx, name)
                        .expect("fat_locals entry checked above");
                    // The slot table is family-agnostic; the TIR type
                    // picks the repr so downstream type-keyed dispatch
                    // (frees, call ABI) sees the right variant.
                    if matches!(ctx.pool.kind(inst.ty), TypeKind::Bytes) {
                        ValueRepr::Bytes { ptr, len, cap }
                    } else {
                        ValueRepr::Str { ptr, len, cap }
                    }
                } else {
                    // Not a fat local — fall through to scalar
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
                    let (ptr, len, cap) = Self::emit_slot_out_call(
                        builder,
                        ctx,
                        "ryo_str_from_view",
                        &[(ctx.int_type, v_ptr), (types::I64, v_len)],
                        out_slot,
                    )?;
                    ValueRepr::Str { ptr, len, cap }
                } else if name_str == "__ryo_bytes_from_view" {
                    // M8.4.2 `bytes(bview)` materialization: the
                    // argument is a view pair via `eval_inst_view`.
                    let ValueRepr::View {
                        ptr: v_ptr,
                        len: v_len,
                    } = Self::eval_inst_view(builder, ctx, view.args[0])?
                    else {
                        unreachable!("__ryo_bytes_from_view argument must produce ValueRepr::View")
                    };
                    let (ptr, len, cap) = Self::emit_slot_out_call(
                        builder,
                        ctx,
                        "ryo_bytes_from_view",
                        &[(ctx.int_type, v_ptr), (types::I64, v_len)],
                        out_slot,
                    )?;
                    ValueRepr::Bytes { ptr, len, cap }
                } else if name_str == "__ryo_str_to_bytes" {
                    // `str.to_bytes()` / `strview.to_bytes()` — only
                    // (ptr, len) is read.
                    let (p, l) = Self::eval_str_or_view_parts(builder, ctx, view.args[0])?;
                    let (ptr, len, cap) = Self::emit_slot_out_call(
                        builder,
                        ctx,
                        "__ryo_str_to_bytes",
                        &[(ctx.int_type, p), (types::I64, l)],
                        out_slot,
                    )?;
                    ValueRepr::Bytes { ptr, len, cap }
                } else if name_str == "__ryo_bytes_to_str" {
                    // `bytes.to_str()` / `bytesview.to_str()` — returns
                    // an owned str (validated copy; panics on bad UTF-8).
                    let (p, l) = Self::eval_str_or_view_parts(builder, ctx, view.args[0])?;
                    let (ptr, len, cap) = Self::emit_slot_out_call(
                        builder,
                        ctx,
                        "__ryo_bytes_to_str",
                        &[(ctx.int_type, p), (types::I64, l)],
                        out_slot,
                    )?;
                    ValueRepr::Str { ptr, len, cap }
                } else if name_str == "__ryo_bytes_repr" {
                    // print(bytes) rewrite (sema, M8.4.2) — returns the
                    // escaped-repr str.
                    let (p, l) = Self::eval_str_or_view_parts(builder, ctx, view.args[0])?;
                    let (ptr, len, cap) = Self::emit_slot_out_call(
                        builder,
                        ctx,
                        "__ryo_bytes_repr",
                        &[(ctx.int_type, p), (types::I64, l)],
                        out_slot,
                    )?;
                    ValueRepr::Str { ptr, len, cap }
                } else if name_str == "bool_to_str" {
                    // Provably-inline producer taken all the way: the
                    // result is one of two static literals, so select
                    // between their .rodata pointers — no runtime call,
                    // no slot-out, and the cap=0 static sentinel keeps
                    // the dead-free elision firing.
                    if out_slot.is_some() {
                        // The inline never writes a slot; a Some here
                        // means `writes_out_slot` failed to exclude this
                        // builtin — fail loudly instead of leaving the
                        // home slot unwritten (silent miscompile).
                        return Err(
                            "bool_to_str is codegen-inlined but was handed an out slot".to_string()
                        );
                    }
                    let cond = Self::eval_inst(builder, ctx, view.args[0])?;
                    let true_id = Self::store_guard_msg(
                        ctx.module,
                        ctx.data_ctx,
                        ctx.guard_msg_data,
                        "true",
                    )?;
                    let false_id = Self::store_guard_msg(
                        ctx.module,
                        ctx.data_ctx,
                        ctx.guard_msg_data,
                        "false",
                    )?;
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
                } else if name_str == "int_to_str" || name_str == "float_to_str" {
                    let arg_val = Self::eval_inst(builder, ctx, view.args[0])?;
                    let (fn_name, param_ty) = match name_str {
                        "int_to_str" => ("ryo_int_to_str", ctx.int_type),
                        "float_to_str" => ("ryo_float_to_str", types::F64),
                        _ => unreachable!(),
                    };
                    let (ptr, len, cap) = Self::emit_slot_out_call(
                        builder,
                        ctx,
                        fn_name,
                        &[(param_ty, arg_val)],
                        out_slot,
                    )?;
                    ValueRepr::Str { ptr, len, cap }
                } else {
                    // User call — emit_call handles sret for fat-returning
                    // calls and caches the triple. Called directly
                    // (not via eval_inst): the scalar path rejects
                    // fat-returning calls.
                    Self::emit_call_slot(builder, ctx, r, out_slot)?;
                    if let Some(repr) = Self::cached_repr(ctx, r) {
                        return Ok(repr);
                    }
                    unreachable!(
                        "fat-returning user call must cache a fat ValueRepr via emit_call"
                    );
                }
            }
            TirTag::StrConcat => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!(),
                };
                // Transient extraction (the helper inside
                // eval_str_or_view_parts) is sound here: each extraction
                // spills to its own fresh scratch slot and the pointers
                // are consumed by the concat call itself.
                let (l_ptr, l_len) = Self::eval_str_or_view_parts(builder, ctx, lhs)?;
                let (r_ptr, r_len) = Self::eval_str_or_view_parts(builder, ctx, rhs)?;

                let (ptr, len, cap) = Self::emit_slot_out_call(
                    builder,
                    ctx,
                    "ryo_str_concat",
                    &[
                        (ctx.int_type, l_ptr),
                        (types::I64, l_len),
                        (ctx.int_type, r_ptr),
                        (types::I64, r_len),
                    ],
                    out_slot,
                )?;
                ValueRepr::Str { ptr, len, cap }
            }
            TirTag::BytesConcat => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!(),
                };
                // Transient extraction, as in StrConcat above.
                let (l_ptr, l_len) = Self::eval_str_or_view_parts(builder, ctx, lhs)?;
                let (r_ptr, r_len) = Self::eval_str_or_view_parts(builder, ctx, rhs)?;

                let (ptr, len, cap) = Self::emit_slot_out_call(
                    builder,
                    ctx,
                    "ryo_bytes_concat",
                    &[
                        (ctx.int_type, l_ptr),
                        (types::I64, l_len),
                        (ctx.int_type, r_ptr),
                        (types::I64, r_len),
                    ],
                    out_slot,
                )?;
                ValueRepr::Bytes { ptr, len, cap }
            }
            TirTag::FieldAccess => Self::eval_field_access_fat(builder, ctx, r)?,
            TirTag::ViewAsOwner => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ViewAsOwner must carry TirData::UnOp"),
                };
                // Re-borrow into the fat triple: cap=0 static sentinel,
                // identical to literals. No allocation.
                let ValueRepr::View { ptr, len } = Self::eval_inst_view(builder, ctx, operand)?
                else {
                    unreachable!("ViewAsOwner operand must produce ValueRepr::View")
                };
                let cap = builder.ins().iconst(types::I64, 0);
                if matches!(ctx.pool.kind(inst.ty), TypeKind::Bytes) {
                    ValueRepr::Bytes { ptr, len, cap }
                } else {
                    ValueRepr::Str { ptr, len, cap }
                }
            }
            _ => {
                // Delegate to scalar eval_inst for non-fat instructions
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

    /// Evaluate a `str`/`bytes`/`strview`/`bytesview`-typed operand and
    /// hand back its `(ptr, len)` words regardless of representation —
    /// owned triple or borrowed view pair (M8.4/M8.4.2). Owned triples
    /// extract through the SSO-aware `emit_fat_bytes_ptr_len`, which
    /// spills a tagged-inline value's words to a fresh scratch slot and
    /// passes heap/static values through unchanged. Consumers that
    /// only need the viewed bytes (`print`, `StrLen`, `StrCmpEq/Ne`,
    /// `BytesCmpEq/Ne`, the `__ryo_str_push` suffix, the bytes
    /// conversion calls) use this; anything needing the cap must stay
    /// on `eval_inst_fat`.
    ///
    /// TRANSIENT CONSUMERS ONLY: for an inline value the returned ptr
    /// addresses a scratch slot private to this extraction. View-
    /// creating ops (slice, ToView) must not use it.
    pub(super) fn eval_str_or_view_parts(
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
        match Self::eval_inst_fat(builder, ctx, r)? {
            ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                // A home-backed Var's inline bytes already sit in the
                // home slot — extract against that address, no spill.
                let inline_addr =
                    Self::local_name_of(ctx, r).and_then(|n| Self::fat_home_addr(builder, ctx, n));
                Self::emit_fat_bytes_ptr_len(builder, ctx, ptr, len, cap, inline_addr)
            }
            ValueRepr::View { ptr, len } => Ok((ptr, len)),
            ValueRepr::Scalar(_) | ValueRepr::Struct { .. } => Err(format!(
                "eval_str_or_view_parts: instruction at %{} is not a fat/view value",
                r.index()
            )),
        }
    }

    /// The `len` word of a `str`/`bytes`/`strview`/`bytesview`-typed
    /// operand, from either representation (M8.4/M8.4.2). Backs the
    /// `StrLen` arm.
    fn eval_str_or_view_len(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let (_, len) = Self::eval_str_or_view_parts(builder, ctx, r)?;
        Ok(len)
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
                ctx,
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
            // then reload the mutated triple back into s's FatLocals.
            // A home-backed binding skips the spill+reload entirely:
            // the runtime mutates its home slot in place. arg 0 is
            // `&s` (lowered to Var(s)); arg 1 is the suffix str.
            let s_ref = view.args[0];
            let suffix_ref = view.args[1];
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

        if name_str == "bytes_push" {
            // bytes_push(&b, x): spill b's fat pointer to a 24-byte
            // slot, call __ryo_bytes_push(slot_addr, x), then reload
            // the mutated triple back into b's FatLocals. A home-backed
            // binding skips the spill+reload: the runtime mutates its
            // home slot in place. arg 0 is `&b` (lowered to Var(b));
            // arg 1 is the int byte value.
            // The 0-255 range check is runtime-side (M8.4.2 stopgap).
            let b_ref = view.args[0];
            let x_ref = view.args[1];
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

        let callee_id = *ctx
            .func_ids
            .get(&name_id)
            .ok_or_else(|| format!("Undefined function: '{}'", name_str))?;

        let mut arg_values = Vec::with_capacity(view.args.len() * 3 + 1);
        // inout args: spill the current value to a stack slot, pass the
        // slot address, then reload after the call. Scalar spills one
        // field; fat owners spill the fat-pointer triple.
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
                if matches!(ctx.tir.inst(*arg).data, TirData::FieldAccess { .. })
                    || matches!(ctx.pool.kind(arg_ty), TypeKind::Struct)
                {
                    // M9 inout field path (`&p.x`) or whole-struct inout
                    // (`&p`): the pointee already lives in the root
                    // struct's stack slot — pass its address directly so
                    // the callee mutates in place. No spill, no reload,
                    // no write-back.
                    let addr = Self::inout_pointee_addr(builder, ctx, *arg)?;
                    arg_values.push(addr);
                } else if is_fat_type(arg_ty, ctx.pool) {
                    let repr = Self::eval_inst_fat(builder, ctx, *arg)?;
                    let (ptr, len, cap) = match repr {
                        ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                            (ptr, len, cap)
                        }
                        _ => unreachable!("inout fat arg must produce a fat ValueRepr"),
                    };
                    // Home-backed binding: the callee mutates the home
                    // slot in place — no spill, no reload.
                    let home_addr = Self::local_name_of(ctx, *arg)
                        .and_then(|n| Self::fat_home_addr(builder, ctx, n));
                    match home_addr {
                        Some(addr) => {
                            // The callee may have written anything
                            // through the pointer — the binding's range
                            // fact dies here (same as the reload path),
                            // and the home's provenance no longer holds.
                            if let Some(name) = Self::local_name_of(ctx, *arg) {
                                Self::kill_fact(ctx, name);
                                Self::set_home_inline(ctx, name, false);
                            }
                            arg_values.push(addr);
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
                            arg_values.push(addr);
                            inout_reloads.push((*arg, slot));
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
                    let cur = Self::eval_inst(builder, ctx, *arg)?;
                    builder.ins().store(MemFlagsData::trusted(), cur, addr, 0);
                    arg_values.push(addr);
                    inout_reloads.push((*arg, slot));
                }
            } else if is_fat_type(arg_ty, ctx.pool) {
                let repr = Self::eval_inst_fat(builder, ctx, *arg)?;
                match repr {
                    ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                        arg_values.push(ptr);
                        arg_values.push(len);
                        arg_values.push(cap);
                    }
                    _ => unreachable!("fat-typed arg must produce a fat ValueRepr"),
                }
            } else if ctx.pool.is_view(arg_ty) {
                // `strview` arg → 2-word ABI (ptr, len), matching the
                // callee's build_signature. Sema has already inserted
                // ToView for owned-str actuals (§3.4).
                let (ptr, len) = Self::eval_str_or_view_parts(builder, ctx, *arg)?;
                arg_values.push(ptr);
                arg_values.push(len);
            } else if matches!(ctx.pool.kind(arg_ty), TypeKind::Struct) {
                // M9 struct arg: a single slot address — the existing
                // slot for Borrow, a fresh field-wise copy for Move/Copy.
                let addr = Self::emit_struct_call_arg(builder, ctx, *arg, mode)?;
                arg_values.push(addr);
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

        if is_fat_type(ret_ty, ctx.pool) {
            // sret: allocate 24-byte slot (or use the caller-provided
            // binding home), prepend pointer to args
            let slot = out_slot.unwrap_or_else(|| {
                builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    STR_SLOT_SIZE,
                    3,
                ))
            });
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
            let repr = if matches!(ctx.pool.kind(ret_ty), TypeKind::Bytes) {
                ValueRepr::Bytes { ptr, len, cap }
            } else {
                ValueRepr::Str { ptr, len, cap }
            };
            Self::cache_repr(ctx, r, repr);
            return Ok(ptr); // dummy scalar — consumers use eval_inst_fat
        }

        if matches!(ctx.pool.kind(ret_ty), TypeKind::Struct) {
            // M9 sret: allocate the struct's slot, prepend its address
            // to the args, and treat the slot as the result (mirrors
            // the fat sret path above).
            let slot = Self::struct_slot(builder, ctx, ret_ty);
            let out = builder.ins().stack_addr(ctx.int_type, slot, 0);

            let mut all_args = Vec::with_capacity(arg_values.len() + 1);
            all_args.push(out);
            all_args.extend(arg_values);

            builder.ins().call(callee_ref, &all_args);
            Self::reload_inout_args(builder, ctx, &inout_reloads)?;

            Self::cache_repr(ctx, r, ValueRepr::Struct { addr: out });
            return Ok(out); // dummy scalar — consumers use eval_inst_struct
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
        // Home-backed binding: push mutates the home in place — no
        // spill, and the "reload" loads below only feed the cached
        // repr the free sweep keys on.
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
        // holds. Caching its repr lets the end-of-statement sweep fire
        // Frees anchored on the concat (e.g. a heap rhs temp) exactly
        // as the allocating path does.
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
