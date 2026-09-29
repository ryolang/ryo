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
use cranelift_module::{DataId, Module};
use ryo_core::tir::{ParamMode, TirData, TirRef, TirTag};
use ryo_core::types::{StringId, TypeId, TypeKind};

use super::bytes::store_string;
use super::{
    Codegen, FunctionContext, STR_SLOT_SIZE, Terminator, ValueRepr, cranelift_type_for, ranges,
};

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
    pub(crate) fn field_addr_of(
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
                    Self::declare_bytes_free(ctx, builder)?
                } else {
                    Self::declare_str_free(ctx, builder)?
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
        Self::emit_due_promo_frees(builder, ctx, r)?;
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
        let result = Self::emit_compound_op(
            builder, ctx, view.op, is_float, None, rhs_range, current, rhs,
        )?;
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

    /// Memberwise struct equality (M9.1): one comparison per field pair
    /// in declaration order — int/bool fields `icmp eq` on the loaded
    /// values, float fields `fcmp eq` (IEEE, no fast-math: a NaN field
    /// makes the struct never equal itself), str/bytes fields the
    /// runtime content compare on both extracted `(ptr, len)` pairs,
    /// nested structs recurse. The per-field results AND-reduce with
    /// `band` — equality has no side effects, so a branchless chain
    /// beats short-circuit branching. `negate` flips the final i8 for
    /// `!=`. Both operands are borrowed, never consumed.
    pub(crate) fn emit_struct_eq(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        lhs_addr: Value,
        rhs_addr: Value,
        ty: TypeId,
        negate: bool,
    ) -> Result<Value, String> {
        let view = ctx.pool.struct_view(ty);
        let mut acc: Option<Value> = None;
        for field in &view.fields {
            let lhs_field = if field.offset == 0 {
                lhs_addr
            } else {
                builder.ins().iadd_imm_s(lhs_addr, i64::from(field.offset))
            };
            let rhs_field = if field.offset == 0 {
                rhs_addr
            } else {
                builder.ins().iadd_imm_s(rhs_addr, i64::from(field.offset))
            };
            let field_kind = ctx.pool.kind(field.ty);
            let field_eq = match field_kind {
                TypeKind::Int | TypeKind::Bool => {
                    let cl_ty = cranelift_type_for(field.ty, ctx.pool, ctx.int_type);
                    let lv = builder
                        .ins()
                        .load(cl_ty, MemFlagsData::trusted(), lhs_field, 0);
                    let rv = builder
                        .ins()
                        .load(cl_ty, MemFlagsData::trusted(), rhs_field, 0);
                    builder.ins().icmp(IntCC::Equal, lv, rv)
                }
                TypeKind::Float => {
                    let lv = builder
                        .ins()
                        .load(types::F64, MemFlagsData::trusted(), lhs_field, 0);
                    let rv = builder
                        .ins()
                        .load(types::F64, MemFlagsData::trusted(), rhs_field, 0);
                    builder.ins().fcmp(FloatCC::Equal, lv, rv)
                }
                k @ (TypeKind::Str | TypeKind::Bytes) => {
                    let is_bytes = matches!(k, TypeKind::Bytes);
                    let (lp, ll, lc) = Self::emit_debug_field_triple(builder, ctx, lhs_field);
                    let (rp, rl, rc) = Self::emit_debug_field_triple(builder, ctx, rhs_field);
                    // Inline (SSO) fields keep their bytes in the struct
                    // slot: extract against the slot address as the
                    // inline home, exactly like the repr path.
                    let (lvp, lvl) =
                        Self::emit_fat_bytes_ptr_len(builder, ctx, lp, ll, lc, Some(lhs_field))?;
                    let (rvp, rvl) =
                        Self::emit_fat_bytes_ptr_len(builder, ctx, rp, rl, rc, Some(rhs_field))?;
                    let fn_name = if is_bytes {
                        "ryo_bytes_eq"
                    } else {
                        "ryo_str_eq"
                    };
                    let eq_ref = Self::declare_runtime_fn(
                        ctx,
                        builder,
                        fn_name,
                        &[ctx.int_type, types::I64, ctx.int_type, types::I64],
                        &[types::I8],
                    )?;
                    let call = builder.ins().call(eq_ref, &[lvp, lvl, rvp, rvl]);
                    builder.inst_results(call)[0]
                }
                TypeKind::Struct => {
                    Self::emit_struct_eq(builder, ctx, lhs_field, rhs_field, field.ty, false)?
                }
                TypeKind::View(_) => {
                    return Err(
                        "view struct field reached codegen; sema Rule 6 rejects it".to_string()
                    );
                }
                other => {
                    return Err(format!(
                        "emit_struct_eq: field '{}' of '{}' has non-comparable type kind {other:?}",
                        ctx.pool.str(field.name),
                        ctx.pool.str(view.name),
                    ));
                }
            };
            acc = Some(match acc {
                None => field_eq,
                Some(prev) => builder.ins().band(prev, field_eq),
            });
        }
        let result = match acc {
            Some(v) => v,
            // A fieldless struct equals itself.
            None => builder.ins().iconst(types::I8, 1),
        };
        // `band` yields i8 0/1 (1 = all fields equal). `!=` is the
        // boolean NOT of that — for a 0/1 value, `result == 0`.
        if negate {
            let zero = builder.ins().iconst(types::I8, 0);
            Ok(builder.ins().icmp(IntCC::Equal, result, zero))
        } else {
            Ok(result)
        }
    }

    /// Debug representation of the struct value at `addr` (M9.1):
    /// builds `Name{f=v, f=v}` in declaration order, no spaces, into a
    /// fresh `RyoStrFat` slot whose address is returned. The string is
    /// assembled in place with an `__ryo_str_push` chain: the result
    /// slot is seeded with the runtime's canonical empty value
    /// (`{null, 0, 0}` — what `ryo_str_concat` writes for two empty
    /// halves; the push ABI never reads the ptr at len 0), then
    /// punctuation and field names come from read-only .rodata while
    /// each field value renders into a temp slot, is pushed, and is
    /// freed immediately (the push copies the bytes first, and
    /// `ryo_str_free` is a runtime no-op for inline/static caps). str
    /// fields are quoted and borrowed straight out of the struct — the
    /// struct keeps owning them, so no free fires. Nested structs
    /// recurse; the nested repr temp frees after its push into the
    /// enclosing result.
    pub(crate) fn emit_debug_repr(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        addr: Value,
        ty: TypeId,
    ) -> Result<Value, String> {
        let view = ctx.pool.struct_view(ty);
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            STR_SLOT_SIZE,
            3,
        ));
        let result = builder.ins().stack_addr(ctx.int_type, slot, 0);
        let zero = builder.ins().iconst(ctx.int_type, 0);
        let zero64 = builder.ins().iconst(types::I64, 0);
        builder
            .ins()
            .store(MemFlagsData::trusted(), zero, result, 0);
        builder
            .ins()
            .store(MemFlagsData::trusted(), zero64, result, 8);
        builder
            .ins()
            .store(MemFlagsData::trusted(), zero64, result, 16);

        Self::push_debug_name(builder, ctx, result, view.name)?;
        Self::push_debug_static(builder, ctx, result, "{")?;
        for (i, field) in view.fields.iter().enumerate() {
            if i > 0 {
                Self::push_debug_static(builder, ctx, result, ", ")?;
            }
            Self::push_debug_name(builder, ctx, result, field.name)?;
            Self::push_debug_static(builder, ctx, result, "=")?;
            let field_addr = if field.offset == 0 {
                addr
            } else {
                builder.ins().iadd_imm_s(addr, i64::from(field.offset))
            };
            match ctx.pool.kind(field.ty) {
                TypeKind::Int | TypeKind::Float | TypeKind::Bool => {
                    let cl_ty = cranelift_type_for(field.ty, ctx.pool, ctx.int_type);
                    let v = builder
                        .ins()
                        .load(cl_ty, MemFlagsData::trusted(), field_addr, 0);
                    let (fn_name, param_ty) = match ctx.pool.kind(field.ty) {
                        TypeKind::Int => ("ryo_int_to_str", ctx.int_type),
                        TypeKind::Float => ("ryo_float_to_str", types::F64),
                        _ => ("ryo_bool_to_str", types::I8),
                    };
                    let (tmp_addr, p, l, c) =
                        Self::emit_debug_render(builder, ctx, fn_name, &[(param_ty, v)])?;
                    Self::emit_debug_push_result(builder, ctx, result, tmp_addr, p, l, c)?;
                }
                TypeKind::Str => {
                    // Quoted, raw (unescaped) content — borrowed from
                    // the struct, which keeps owning the field.
                    let (p, l, c) = Self::emit_debug_field_triple(builder, ctx, field_addr);
                    let (vp, vl) =
                        Self::emit_fat_bytes_ptr_len(builder, ctx, p, l, c, Some(field_addr))?;
                    Self::push_debug_static(builder, ctx, result, "\"")?;
                    Self::emit_debug_push(builder, ctx, result, vp, vl)?;
                    Self::push_debug_static(builder, ctx, result, "\"")?;
                }
                TypeKind::Bytes => {
                    let (p, l, c) = Self::emit_debug_field_triple(builder, ctx, field_addr);
                    let (vp, vl) =
                        Self::emit_fat_bytes_ptr_len(builder, ctx, p, l, c, Some(field_addr))?;
                    let (tmp_addr, p, l, c) = Self::emit_debug_render(
                        builder,
                        ctx,
                        "__ryo_bytes_repr",
                        &[(ctx.int_type, vp), (types::I64, vl)],
                    )?;
                    Self::emit_debug_push_result(builder, ctx, result, tmp_addr, p, l, c)?;
                }
                TypeKind::Struct => {
                    let nested = Self::emit_debug_repr(builder, ctx, field_addr, field.ty)?;
                    let (p, l, c) = Self::emit_debug_field_triple(builder, ctx, nested);
                    let (vp, vl) =
                        Self::emit_fat_bytes_ptr_len(builder, ctx, p, l, c, Some(nested))?;
                    Self::emit_debug_push(builder, ctx, result, vp, vl)?;
                    // The nested repr temp is fully copied into the
                    // enclosing result — release it (a no-op when the
                    // nested render stayed inline).
                    let free_ref = Self::declare_str_free(ctx, builder)?;
                    builder.ins().call(free_ref, &[p, c]);
                }
                TypeKind::View(_) => {
                    return Err(
                        "view struct field reached codegen; sema Rule 6 rejects it".to_string()
                    );
                }
                other => {
                    return Err(format!(
                        "emit_debug_repr: field '{}' of '{}' has non-renderable type kind {other:?}",
                        ctx.pool.str(field.name),
                        ctx.pool.str(view.name),
                    ));
                }
            }
        }
        Self::push_debug_static(builder, ctx, result, "}")?;
        Ok(result)
    }

    /// Load the (ptr, len, cap) triple stored at a struct field's
    /// address (or a finished repr slot). Inline values keep their
    /// bytes and tag in the slot itself; the loaded ptr/len words are
    /// only meaningful for heap/static values, so consumers route the
    /// triple through `emit_fat_bytes_ptr_len` with the slot address
    /// as the inline home.
    fn emit_debug_field_triple(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        field_addr: Value,
    ) -> (Value, Value, Value) {
        let ptr = builder
            .ins()
            .load(ctx.int_type, MemFlagsData::trusted(), field_addr, 0);
        let len = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), field_addr, 8);
        let cap = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), field_addr, 16);
        (ptr, len, cap)
    }

    /// Run one slot-out repr producer (`ryo_*_to_str`, `__ryo_bytes_repr`)
    /// into a fresh temp slot. Returns the temp's slot address plus its
    /// loaded triple — the caller pushes the bytes (extracted against
    /// the slot address: inline renders keep their bytes in the slot)
    /// and then frees the temp.
    fn emit_debug_render(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        fn_name: &'static str,
        args: &[(types::Type, Value)],
    ) -> Result<(Value, Value, Value, Value), String> {
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            STR_SLOT_SIZE,
            3,
        ));
        let tmp_addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
        let (p, l, c) = Self::emit_slot_out_call(builder, ctx, fn_name, args, Some(slot))?;
        Ok((tmp_addr, p, l, c))
    }

    /// Push a freshly rendered temp's bytes onto the repr and free the
    /// temp. The push copies the bytes out of the temp first;
    /// `ryo_str_free` no-ops on inline/static caps.
    fn emit_debug_push_result(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        result: Value,
        tmp_addr: Value,
        p: Value,
        l: Value,
        c: Value,
    ) -> Result<(), String> {
        let (vp, vl) = Self::emit_fat_bytes_ptr_len(builder, ctx, p, l, c, Some(tmp_addr))?;
        Self::emit_debug_push(builder, ctx, result, vp, vl)?;
        let free_ref = Self::declare_str_free(ctx, builder)?;
        builder.ins().call(free_ref, &[p, c]);
        Ok(())
    }

    /// One `__ryo_str_push(result, suffix_ptr, suffix_len)` step of
    /// the repr chain.
    fn emit_debug_push(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        result: Value,
        ptr: Value,
        len: Value,
    ) -> Result<(), String> {
        let push_ref = Self::declare_runtime_fn(
            ctx,
            builder,
            "__ryo_str_push",
            &[ctx.int_type, ctx.int_type, types::I64],
            &[],
        )?;
        builder.ins().call(push_ref, &[result, ptr, len]);
        Ok(())
    }

    /// Push a read-only .rodata piece (already defined) onto the repr.
    fn push_debug_data(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        result: Value,
        data_id: DataId,
        len: usize,
    ) -> Result<(), String> {
        let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
        let ptr = builder.ins().symbol_value(ctx.int_type, data_ref);
        let len_v = builder.ins().iconst(types::I64, len as i64);
        Self::emit_debug_push(builder, ctx, result, ptr, len_v)
    }

    /// Push a compiler-static text piece (punctuation): deduped per
    /// module through the guard-message data cache.
    fn push_debug_static(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        result: Value,
        text: &'static str,
    ) -> Result<(), String> {
        let data_id = Self::store_guard_msg(ctx.module, ctx.data_ctx, ctx.guard_msg_data, text)?;
        Self::push_debug_data(builder, ctx, result, data_id, text.len())
    }

    /// Push a field name: deduped per module through the interned
    /// string-literal data cache, keyed on the field's `StringId`.
    fn push_debug_name(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        result: Value,
        name: StringId,
    ) -> Result<(), String> {
        let text = ctx.pool.str(name);
        let data_id = store_string(name, text, ctx.module, ctx.data_ctx, ctx.string_data)?;
        Self::push_debug_data(builder, ctx, result, data_id, text.len())
    }
}
