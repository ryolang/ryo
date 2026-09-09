//! Struct codegen (M9) — split from `expr.rs`/`mod.rs` to keep every
//! file under the 2000-line CI cap (`scripts/check_file_length.sh`).
//!
//! Memory-first model: every struct value lives in a stack slot of
//! `size`/`align` from `pool.struct_view`; `ValueRepr::Struct` carries
//! the slot address and struct-typed locals hold one address-bearing
//! `Variable` per binding (`struct_locals`). Field reads/writes,
//! copies, calls, and drops all go through that pointer. Copies are
//! FIELD-WISE, never byte-wise — a block copy would read uninitialized
//! padding bytes, which the ASan/Valgrind suites flag.

use cranelift::codegen::ir::{MemFlagsData, StackSlot, StackSlotData, StackSlotKind};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::ast::CompoundOp;
use ryo_core::tir::{ParamMode, TirData, TirRef, TirTag};
use ryo_core::types::{StringId, TypeId, TypeKind};

use super::expr::{DIV_OVERFLOW_MSG, DIV_ZERO_MSG, MOD_OVERFLOW_MSG, MOD_ZERO_MSG};
use super::{Codegen, FunctionContext, Terminator, ValueRepr, cranelift_type_for, ranges};

impl<M: Module> Codegen<M> {
    /// Stack slot for a struct value of type `ty`, sized and aligned
    /// from the pool's layout (`StackSlotData`'s align is log2).
    pub(crate) fn struct_slot(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        ty: TypeId,
    ) -> StackSlot {
        let view = ctx.pool.struct_view(ty);
        debug_assert!(view.align.is_power_of_two());
        builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            view.size,
            u8::try_from(view.align.trailing_zeros()).expect("struct align shift out of range"),
        ))
    }

    /// Materialize a struct-typed instruction, returning the address
    /// of its stack slot. Memoized into `inst_values` as
    /// `ValueRepr::Struct`.
    pub(crate) fn eval_inst_struct(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        if let Some(repr) = Self::cached_repr(ctx, r) {
            return match repr {
                ValueRepr::Struct { addr } => Ok(addr),
                _ => Err(format!(
                    "eval_inst_struct: non-struct repr cached for %{}",
                    r.index()
                )),
            };
        }
        let inst = ctx.tir.inst(r);
        let addr = match inst.tag {
            TirTag::StructLit => Self::emit_struct_lit(builder, ctx, r)?,
            TirTag::Var => {
                let name = match inst.data {
                    TirData::Var(name) => name,
                    _ => unreachable!("Var must carry TirData::Var"),
                };
                let var = Self::read_slot(&ctx.struct_locals, name).ok_or_else(|| {
                    format!("Undefined struct variable: '{}'", ctx.pool.str(name))
                })?;
                builder.use_var(var)
            }
            TirTag::FieldAccess => Self::field_addr_of(builder, ctx, r)?.0,
            TirTag::Call => {
                // Struct-returning call: emit_call handles sret and
                // caches ValueRepr::Struct for r.
                Self::emit_call(builder, ctx, r)?;
                match Self::cached_repr(ctx, r) {
                    Some(ValueRepr::Struct { addr }) => return Ok(addr),
                    _ => unreachable!("struct-returning call must cache ValueRepr::Struct"),
                }
            }
            other => {
                return Err(format!(
                    "eval_inst_struct: instruction at %{} is not a struct value (tag={other:?})",
                    r.index()
                ));
            }
        };
        Self::cache_repr(ctx, r, ValueRepr::Struct { addr });
        Ok(addr)
    }

    /// Address + field type of a `FieldAccess` chain: the base
    /// struct's slot address plus the field's byte offset.
    fn field_addr_of(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<(Value, TypeId), String> {
        let inst = ctx.tir.inst(r);
        let TirData::FieldAccess {
            object,
            field_index,
        } = inst.data
        else {
            return Err(format!(
                "field_addr_of: instruction at %{} is not a FieldAccess",
                r.index()
            ));
        };
        let obj_ty = ctx.tir.inst(object).ty;
        let base = Self::eval_inst_struct(builder, ctx, object)?;
        let view = ctx.pool.struct_view(obj_ty);
        let field = view.fields[field_index as usize];
        let addr = if field.offset == 0 {
            base
        } else {
            builder.ins().iadd_imm_s(base, i64::from(field.offset))
        };
        Ok((addr, field.ty))
    }

    /// Address of an inout argument's pointee: a field path (`&p.x`)
    /// resolves through the FieldAccess chain, a whole struct to its
    /// slot. Passed directly to the callee, which mutates in place —
    /// no spill, no reload, no write-back entry.
    pub(crate) fn inout_pointee_addr(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        if matches!(ctx.tir.inst(r).data, TirData::FieldAccess { .. }) {
            Ok(Self::field_addr_of(builder, ctx, r)?.0)
        } else {
            Self::eval_inst_struct(builder, ctx, r)
        }
    }

    /// Struct literal: fresh slot, then store each field at its offset.
    fn emit_struct_lit(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let view = ctx.tir.struct_lit_view(r);
        let slot = Self::struct_slot(builder, ctx, view.ty);
        let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
        let sview = ctx.pool.struct_view(view.ty);
        for (field_index, value) in view.fields {
            let field = sview.fields[field_index as usize];
            Self::store_field_value(builder, ctx, addr, field.offset, field.ty, value)?;
        }
        Ok(addr)
    }

    /// Store value `v` of type `field_ty` at `base + offset`. Scalars
    /// store directly; str/bytes store the (ptr, len, cap) triple;
    /// nested structs copy field-wise; view fields are rejected by
    /// sema (Rule 6) and never reach here.
    fn store_field_value(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        base: Value,
        offset: u32,
        field_ty: TypeId,
        v: TirRef,
    ) -> Result<(), String> {
        let off = i32::try_from(offset).expect("struct field offset exceeds i32");
        match ctx.pool.kind(field_ty) {
            TypeKind::Str | TypeKind::Bytes => {
                let repr = Self::eval_inst_fat(builder, ctx, v)?;
                let (ptr, len, cap) = match repr {
                    ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                        (ptr, len, cap)
                    }
                    _ => unreachable!("str/bytes field value must produce a fat ValueRepr"),
                };
                builder.ins().store(MemFlagsData::trusted(), ptr, base, off);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), len, base, off + 8);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), cap, base, off + 16);
                Ok(())
            }
            TypeKind::View(_) => {
                Err("view struct field reached codegen; sema Rule 6 rejects it".to_string())
            }
            TypeKind::Struct => {
                let src = Self::eval_inst_struct(builder, ctx, v)?;
                let dst = if offset == 0 {
                    base
                } else {
                    builder.ins().iadd_imm_s(base, i64::from(offset))
                };
                Self::emit_struct_copy(builder, ctx, dst, src, field_ty)
            }
            _ => {
                let val = Self::eval_inst(builder, ctx, v)?;
                builder.ins().store(MemFlagsData::trusted(), val, base, off);
                Ok(())
            }
        }
    }

    /// Scalar field read (M9): load the field's value from its
    /// computed address. Backs the `FieldAccess` arm of `eval_inst`.
    pub(crate) fn eval_field_access_scalar(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let (addr, field_ty) = Self::field_addr_of(builder, ctx, r)?;
        debug_assert!(
            !matches!(
                ctx.pool.kind(field_ty),
                TypeKind::Str | TypeKind::Bytes | TypeKind::View(_) | TypeKind::Struct
            ),
            "non-scalar field reached the scalar FieldAccess path"
        );
        let cl_ty = cranelift_type_for(field_ty, ctx.pool, ctx.int_type);
        Ok(builder.ins().load(cl_ty, MemFlagsData::trusted(), addr, 0))
    }

    /// str/bytes field read (M9): load the (ptr, len, cap) triple from
    /// the field's address. Backs the `FieldAccess` arm of
    /// `eval_inst_fat`.
    pub(crate) fn eval_field_access_fat(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<ValueRepr, String> {
        let (addr, field_ty) = Self::field_addr_of(builder, ctx, r)?;
        let ptr = builder
            .ins()
            .load(ctx.int_type, MemFlagsData::trusted(), addr, 0);
        let len = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), addr, 8);
        let cap = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), addr, 16);
        Ok(if matches!(ctx.pool.kind(field_ty), TypeKind::Bytes) {
            ValueRepr::Bytes { ptr, len, cap }
        } else {
            ValueRepr::Str { ptr, len, cap }
        })
    }

    /// Field-wise struct copy (M9): NEVER a byte-wise block copy —
    /// reading uninitialized padding fails the ASan/Valgrind suites.
    /// Sizes and offsets are compile-time constants, so this unrolls
    /// statically into one load+store per scalar field.
    pub(crate) fn emit_struct_copy(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        dst: Value,
        src: Value,
        ty: TypeId,
    ) -> Result<(), String> {
        let view = ctx.pool.struct_view(ty);
        for field in &view.fields {
            let off = i32::try_from(field.offset).expect("struct field offset exceeds i32");
            match ctx.pool.kind(field.ty) {
                TypeKind::Str | TypeKind::Bytes => {
                    let ptr = builder
                        .ins()
                        .load(ctx.int_type, MemFlagsData::trusted(), src, off);
                    let len = builder
                        .ins()
                        .load(types::I64, MemFlagsData::trusted(), src, off + 8);
                    let cap =
                        builder
                            .ins()
                            .load(types::I64, MemFlagsData::trusted(), src, off + 16);
                    builder.ins().store(MemFlagsData::trusted(), ptr, dst, off);
                    builder
                        .ins()
                        .store(MemFlagsData::trusted(), len, dst, off + 8);
                    builder
                        .ins()
                        .store(MemFlagsData::trusted(), cap, dst, off + 16);
                }
                TypeKind::View(_) => {
                    return Err(
                        "view struct field reached codegen; sema Rule 6 rejects it".to_string()
                    );
                }
                TypeKind::Struct => {
                    let s = if field.offset == 0 {
                        src
                    } else {
                        builder.ins().iadd_imm_s(src, i64::from(field.offset))
                    };
                    let d = if field.offset == 0 {
                        dst
                    } else {
                        builder.ins().iadd_imm_s(dst, i64::from(field.offset))
                    };
                    Self::emit_struct_copy(builder, ctx, d, s, field.ty)?;
                }
                _ => {
                    let cl_ty = cranelift_type_for(field.ty, ctx.pool, ctx.int_type);
                    let v = builder.ins().load(cl_ty, MemFlagsData::trusted(), src, off);
                    builder.ins().store(MemFlagsData::trusted(), v, dst, off);
                }
            }
        }
        Ok(())
    }

    /// Recursive field destruction (M9): for each needs-drop field,
    /// free str/bytes allocations via the existing runtime frees and
    /// recurse into nested structs. Copy fields are skipped.
    pub(crate) fn emit_struct_drop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        addr: Value,
        ty: TypeId,
    ) -> Result<(), String> {
        let view = ctx.pool.struct_view(ty);
        for field in &view.fields {
            if !ctx.pool.needs_drop(field.ty) {
                continue;
            }
            Self::emit_field_drop(builder, ctx, addr, field.offset, field.ty)?;
        }
        Ok(())
    }

    /// Drop one needs-drop field at `base + offset`: str/bytes load
    /// (ptr, cap) and call the family free; a nested struct recurses.
    fn emit_field_drop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        base: Value,
        offset: u32,
        field_ty: TypeId,
    ) -> Result<(), String> {
        let off = i32::try_from(offset).expect("struct field offset exceeds i32");
        match ctx.pool.kind(field_ty) {
            TypeKind::Str | TypeKind::Bytes => {
                let ptr = builder
                    .ins()
                    .load(ctx.int_type, MemFlagsData::trusted(), base, off);
                let cap = builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), base, off + 16);
                let free_ref = if matches!(ctx.pool.kind(field_ty), TypeKind::Bytes) {
                    Self::declare_bytes_free(ctx.module, builder, ctx.int_type)?
                } else {
                    Self::declare_str_free(ctx.module, builder, ctx.int_type)?
                };
                builder.ins().call(free_ref, &[ptr, cap]);
                Ok(())
            }
            TypeKind::Struct => {
                let addr = if offset == 0 {
                    base
                } else {
                    builder.ins().iadd_imm_s(base, i64::from(offset))
                };
                Self::emit_struct_drop(builder, ctx, addr, field_ty)
            }
            other => {
                unreachable!("needs_drop field of kind {other:?} is neither str/bytes nor struct")
            }
        }
    }

    /// Lower a struct-typed call argument (M9): every mode passes an
    /// address. Borrow passes the existing slot address; Move/Copy
    /// transfer a field-wise copy in a fresh caller-side slot (the
    /// callee may mutate/free the contents per its mode).
    pub(crate) fn emit_struct_call_arg(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        mode: ParamMode,
    ) -> Result<Value, String> {
        let src = Self::eval_inst_struct(builder, ctx, r)?;
        if mode == ParamMode::Borrow {
            return Ok(src);
        }
        let ty = ctx.tir.inst(r).ty;
        let slot = Self::struct_slot(builder, ctx, ty);
        let dst = builder.ins().stack_addr(ctx.int_type, slot, 0);
        Self::emit_struct_copy(builder, ctx, dst, src, ty)?;
        Ok(dst)
    }

    /// `name = <struct value>`: bind `name` to the value's slot. A
    /// fresh producer (StructLit, struct-returning call) hands its
    /// slot over directly; any other source (a moved/copied Var, a
    /// nested-struct field read) is copied field-wise into a fresh
    /// slot so the binding owns storage independent of the source.
    pub(crate) fn emit_struct_var_decl(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let inst = ctx.tir.inst(r);
        let view = ctx.tir.var_decl_view(r);
        let init_tag = ctx.tir.inst(view.initializer).tag;
        let addr = match init_tag {
            TirTag::StructLit | TirTag::Call => {
                Self::eval_inst_struct(builder, ctx, view.initializer)?
            }
            _ => {
                let src = Self::eval_inst_struct(builder, ctx, view.initializer)?;
                let slot = Self::struct_slot(builder, ctx, inst.ty);
                let dst = builder.ins().stack_addr(ctx.int_type, slot, 0);
                Self::emit_struct_copy(builder, ctx, dst, src, inst.ty)?;
                dst
            }
        };
        let var = builder.declare_var(ctx.int_type);
        builder.def_var(var, addr);
        Self::write_slot(
            &mut ctx.struct_locals,
            &mut ctx.struct_locals_undo,
            view.name,
            Some(var),
        );
        Ok(Terminator::None)
    }

    /// `name = <struct value>` on an existing binding: evaluate the
    /// RHS first (it may borrow the binding being overwritten), drop
    /// the old field values when the ownership pass scheduled it,
    /// then copy the new value field-wise into the existing slot.
    pub(crate) fn emit_struct_assign(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let inst = ctx.tir.inst(r);
        let view = ctx.tir.assign_view(r);
        let var = Self::read_slot(&ctx.struct_locals, view.name).ok_or_else(|| {
            format!(
                "Undefined struct variable in assign: '{}'",
                ctx.pool.str(view.name)
            )
        })?;
        let dst = builder.use_var(var);
        let src = Self::eval_inst_struct(builder, ctx, view.value)?;
        if ctx.sidecar.free_on_reassign[r.index()].is_some() {
            Self::emit_struct_drop(builder, ctx, dst, inst.ty)?;
        }
        Self::emit_struct_copy(builder, ctx, dst, src, inst.ty)?;
        Ok(Terminator::None)
    }

    /// Struct return (M9): sret — copy the value field-wise through
    /// the caller-provided out-pointer, then return no IR values.
    /// Mirrors the fat-return path (mod.rs Return arm).
    pub(crate) fn emit_struct_return(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        operand: TirRef,
    ) -> Result<Terminator, String> {
        let sret = ctx
            .sret_ptr
            .expect("struct-returning fn must have sret_ptr");
        let src = Self::eval_inst_struct(builder, ctx, operand)?;
        let ret_ty = ctx.tir.return_type;
        Self::emit_struct_copy(builder, ctx, sret, src, ret_ty)?;
        Self::emit_due_frees(builder, ctx, r)?;
        Self::emit_return(builder, ctx, &[])?;
        Ok(Terminator::Return)
    }

    /// `path = value` (M9): compute the field address, evaluate the
    /// RHS first (it may borrow the very field being overwritten, e.g.
    /// `p.name = p.name + "x"`), drop the old field value when the
    /// ownership pass scheduled it (`field_free_on_reassign`), then
    /// store the new value as in a struct literal.
    pub(crate) fn emit_field_assign(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let view = ctx.tir.field_assign_view(r);
        let (field_addr, field_ty) = Self::field_addr_of(builder, ctx, view.target)?;
        let drop_old = ctx.sidecar.field_free_on_reassign[r.index()].is_some();
        match ctx.pool.kind(field_ty) {
            TypeKind::Str | TypeKind::Bytes => {
                let repr = Self::eval_inst_fat(builder, ctx, view.value)?;
                let (ptr, len, cap) = match repr {
                    ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                        (ptr, len, cap)
                    }
                    _ => unreachable!("str/bytes field value must produce a fat ValueRepr"),
                };
                if drop_old {
                    Self::emit_field_drop(builder, ctx, field_addr, 0, field_ty)?;
                }
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), ptr, field_addr, 0);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), len, field_addr, 8);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), cap, field_addr, 16);
            }
            TypeKind::View(_) => {
                return Err("view struct field reached codegen; sema Rule 6 rejects it".to_string());
            }
            TypeKind::Struct => {
                let src = Self::eval_inst_struct(builder, ctx, view.value)?;
                if drop_old {
                    Self::emit_field_drop(builder, ctx, field_addr, 0, field_ty)?;
                }
                Self::emit_struct_copy(builder, ctx, field_addr, src, field_ty)?;
            }
            _ => {
                debug_assert!(
                    !drop_old,
                    "field_free_on_reassign on a Copy field is an ownership-pass bug"
                );
                let val = Self::eval_inst(builder, ctx, view.value)?;
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), val, field_addr, 0);
            }
        }
        Ok(Terminator::None)
    }

    /// `path op= value` (M9): load the field, apply the checked
    /// operator (same spec §18 rules as bare CompoundAssign, minus the
    /// range facts — fields have no binding facts), store it back.
    /// Sema restricts compound field assignment to int/float fields,
    /// so no old-value free applies.
    pub(crate) fn emit_compound_field_assign(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let view = ctx.tir.compound_field_assign_view(r);
        debug_assert!(ctx.sidecar.field_free_on_reassign[r.index()].is_none());
        let (field_addr, field_ty) = Self::field_addr_of(builder, ctx, view.target)?;
        let rhs = Self::eval_inst(builder, ctx, view.value)?;
        let cl_ty = cranelift_type_for(field_ty, ctx.pool, ctx.int_type);
        let current = builder
            .ins()
            .load(cl_ty, MemFlagsData::trusted(), field_addr, 0);
        let is_float = field_ty == ctx.pool.float();
        let rhs_range = ranges::int_range_of(ctx.tir, &ctx.range_facts, view.value);
        let result = match (view.op, is_float) {
            (CompoundOp::Add, false) => {
                Self::emit_int_binop(builder, ctx, TirTag::IAdd, None, rhs_range, current, rhs)?
            }
            (CompoundOp::Sub, false) => {
                Self::emit_int_binop(builder, ctx, TirTag::ISub, None, rhs_range, current, rhs)?
            }
            (CompoundOp::Mul, false) => {
                Self::emit_int_binop(builder, ctx, TirTag::IMul, None, rhs_range, current, rhs)?
            }
            (CompoundOp::Div, false) => {
                Self::emit_div_guard(
                    builder,
                    ctx,
                    current,
                    None,
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
                    None,
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
        };
        builder
            .ins()
            .store(MemFlagsData::trusted(), result, field_addr, 0);
        Ok(Terminator::None)
    }

    /// Whole-struct scheduled Free (M9): when `target` is struct-typed,
    /// resolve its slot address — the binding's CURRENT `struct_locals`
    /// entry (same reasoning as the fat `free_binding_names` path) or
    /// the producing inst's cached `ValueRepr::Struct` — and run the
    /// recursive field drop. Returns `Ok(true)` when the target was a
    /// struct (handled), so the caller skips the fat path.
    pub(crate) fn try_emit_struct_free(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        target: TirRef,
    ) -> Result<bool, String> {
        let ty = if let Some(idx) = target.as_param_index() {
            ctx.tir.params[idx as usize].ty
        } else {
            ctx.tir.inst(target).ty
        };
        if !matches!(ctx.pool.kind(ty), TypeKind::Struct) {
            return Ok(false);
        }
        let addr = match Self::free_binding_name(ctx, target)
            .and_then(|name| Self::read_slot(&ctx.struct_locals, name))
        {
            Some(var) => builder.use_var(var),
            None => match Self::cached_repr(ctx, target) {
                Some(ValueRepr::Struct { addr }) => addr,
                _ => {
                    return Err(format!(
                        "ownership pass scheduled Free for struct %{} but no struct local or ValueRepr is available",
                        target.index()
                    ));
                }
            },
        };
        Self::emit_struct_drop(builder, ctx, addr, ty)?;
        Ok(true)
    }

    /// Struct arm of `emit_conditional_dead_drops` (M9): the pre-if
    /// value of a conditionally-reassigned struct binding is dropped
    /// on the arms that kept it. Returns `Ok(true)` when `name` is a
    /// struct binding (handled), so the caller skips the fat path.
    pub(crate) fn try_emit_struct_dead_drop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        name: StringId,
        target: TirRef,
    ) -> Result<bool, String> {
        let Some(var) = Self::read_slot(&ctx.struct_locals, name) else {
            return Ok(false);
        };
        let addr = builder.use_var(var);
        // Conditional dead drops only fire for locals; a param-sentinel
        // target would panic the arena index below.
        debug_assert!(!target.is_param());
        let ty = ctx.tir.inst(target).ty;
        Self::emit_struct_drop(builder, ctx, addr, ty)?;
        Ok(true)
    }
}
