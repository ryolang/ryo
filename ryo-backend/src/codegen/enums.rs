//! Enum codegen (M11) — all enum/variant emission: enum construction,
//! tag/union slot layout, tag-dispatched copies and drops, Eq, and
//! Debug. Like structs (see `structs.rs`), enum values are
//! memory-first aggregates: every value lives in a stack slot sized by
//! the pool's enum layout (`i32` discriminant at offset 0, each
//! variant's payload at its pool-computed absolute offset), and
//! `ValueRepr::Enum` carries the slot address. Copies and drops NEVER
//! copy bytes — a block copy would read the inactive variants'
//! uninitialized padding, which the ASan/Valgrind suites flag — so
//! both dispatch on the loaded discriminant (`Switch`) and touch only
//! the active variant's fields, reusing the struct per-field helpers.

use cranelift::codegen::ir::{BlockArg, MemFlagsData, StackSlot, StackSlotData, StackSlotKind};
use cranelift::frontend::Switch;
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{ParamMode, TirData, TirRef, TirTag};
use ryo_core::types::{EnumVariantView, InternPool, StringId, TypeId, TypeKind, VariantKind};

use super::{Codegen, FunctionContext, STR_SLOT_SIZE, Terminator, ValueRepr};

/// True for enum types (M11) — memory-first aggregates exactly like
/// structs: values live in stack slots, `ValueRepr::Enum` carries the
/// slot address, and params/returns ride the slot-address / hidden-sret
/// ABI. Callers gate aggregate paths with this before
/// `cranelift_type_for`, where an enum TypeId is a caller bug (same
/// contract as `is_struct_type`).
pub(crate) fn is_enum_type(ty: TypeId, pool: &InternPool) -> bool {
    matches!(pool.kind(ty), TypeKind::Enum)
}

/// Trap code for the invalid-discriminant arm of every tag dispatch:
/// sema and the ownership pass guarantee the tag is a valid variant
/// index, so an out-of-range tag is a compiler bug, never a runtime
/// path. Distinct from user(1) (the post-panic continuation trap) so
/// the two unreachable-by-construction sites stay distinguishable.
fn invalid_tag_trap(builder: &mut FunctionBuilder) {
    builder
        .ins()
        .trap(TrapCode::user(2).expect("user trap code 2 is within Cranelift's encodable range"));
}

impl<M: Module> Codegen<M> {
    /// Stack slot for an enum value of type `ty`, sized and aligned
    /// from the pool's enum layout (`StackSlotData`'s align is log2).
    pub(crate) fn enum_slot(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        ty: TypeId,
    ) -> StackSlot {
        let (size, align) = ctx.pool.enum_view(ty).size_align();
        debug_assert!(align.is_power_of_two());
        builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size,
            u8::try_from(align.trailing_zeros()).expect("enum align shift out of range"),
        ))
    }

    /// Materialize an enum-typed instruction, returning the address of
    /// its stack slot. Memoized into `inst_values` as
    /// `ValueRepr::Enum`. Mirrors `eval_inst_struct`: enum bindings
    /// share the family-agnostic `struct_locals` slot-address table
    /// (keyed by binding name; every reader gates on the value's type
    /// kind), so scoping restores work unchanged.
    pub(crate) fn eval_inst_enum(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        if let Some(repr) = Self::cached_repr(ctx, r) {
            return match repr {
                ValueRepr::Enum { addr } => Ok(addr),
                _ => Err(format!(
                    "eval_inst_enum: non-enum repr cached for %{}",
                    r.index()
                )),
            };
        }
        let inst = ctx.tir.inst(r);
        let addr = match inst.tag {
            TirTag::EnumLit => Self::emit_enum_lit(builder, ctx, r)?,
            TirTag::Var => {
                let name = match inst.data {
                    TirData::Var(name) => name,
                    _ => unreachable!("Var must carry TirData::Var"),
                };
                let var = Self::read_slot(&ctx.struct_locals, name)
                    .ok_or_else(|| format!("Undefined enum variable: '{}'", ctx.pool.str(name)))?;
                builder.use_var(var)
            }
            TirTag::FieldAccess => Self::field_addr_of(builder, ctx, r)?.0,
            TirTag::Call => {
                // Enum-returning call: emit_call handles sret and
                // caches ValueRepr::Enum for r.
                Self::emit_call(builder, ctx, r)?;
                match Self::cached_repr(ctx, r) {
                    Some(ValueRepr::Enum { addr }) => return Ok(addr),
                    _ => unreachable!("enum-returning call must cache ValueRepr::Enum"),
                }
            }
            other => {
                return Err(format!(
                    "eval_inst_enum: instruction at %{} is not an enum value (tag={other:?})",
                    r.index()
                ));
            }
        };
        Self::cache_repr(ctx, r, ValueRepr::Enum { addr });
        Ok(addr)
    }

    /// Enum variant construction `Name::Variant(args...)` (M11):
    /// fresh slot sized by the enum layout, store the i32 discriminant
    /// at offset 0, then store each payload field at its pool-computed
    /// absolute offset. Payloads sit at `align_up(4, payload_align)`
    /// (natural alignment, no small-payload exception) — offsets always
    /// come from `EnumVariantView`, never from re-deriving the layout.
    /// Payload stores reuse the struct field-store helper
    /// (`store_field_value`) so str/bytes triples and nested aggregates
    /// store correctly.
    fn emit_enum_lit(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let view = ctx.tir.enum_lit_view(r);
        let slot = Self::enum_slot(builder, ctx, view.ty);
        let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
        let tag = builder
            .ins()
            .iconst(types::I32, i64::from(view.variant_index));
        builder.ins().store(MemFlagsData::trusted(), tag, addr, 0);
        let variant = ctx
            .pool
            .enum_view(view.ty)
            .variants()
            .nth(view.variant_index as usize)
            .unwrap_or_else(|| unreachable!("EnumLit variant index out of range"));
        for (field_index, value) in view.fields() {
            let field = variant.fields[field_index as usize];
            Self::store_field_value(builder, ctx, addr, field.offset, field.ty, value)?;
        }
        Ok(addr)
    }

    /// Tag-dispatch skeleton shared by `emit_enum_copy`,
    /// `emit_enum_drop`, `emit_enum_eq`, and `eval_enum_debug_repr`:
    /// one case block per variant plus an `otherwise` block that traps
    /// (the discriminant is always a valid variant index — a
    /// compiler-invariant violation, never a runtime path). Emits the
    /// `Switch` in the current block (terminating it) and returns the
    /// case blocks in variant declaration order; the caller fills each
    /// arm and jumps to its own merge block.
    fn emit_tag_dispatch(
        builder: &mut FunctionBuilder,
        case_count: usize,
        tag: Value,
    ) -> (Vec<Block>, Block) {
        let mut switch = Switch::new();
        let cases: Vec<Block> = (0..case_count).map(|_| builder.create_block()).collect();
        for (index, block) in cases.iter().enumerate() {
            switch.set_entry(index as u128, *block);
        }
        let otherwise = builder.create_block();
        switch.emit(builder, tag, otherwise);
        (cases, otherwise)
    }

    /// Field-wise enum copy (M11): NEVER a byte-wise block copy —
    /// reading the inactive variants' uninitialized padding fails the
    /// ASan/Valgrind suites. The discriminant is copied
    /// unconditionally, then a tag switch copies only the ACTIVE
    /// variant's payload fields, reusing the struct per-field copy
    /// (`emit_field_copy`) so str/bytes triples and nested aggregates
    /// copy exactly like struct fields.
    pub(crate) fn emit_enum_copy(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        dst: Value,
        src: Value,
        ty: TypeId,
    ) -> Result<(), String> {
        let tag = builder
            .ins()
            .load(types::I32, MemFlagsData::trusted(), src, 0);
        builder.ins().store(MemFlagsData::trusted(), tag, dst, 0);
        let variants: Vec<_> = ctx.pool.enum_view(ty).variants().collect();
        let merge = builder.create_block();
        let (cases, otherwise) = Self::emit_tag_dispatch(builder, variants.len(), tag);
        for (block, variant) in cases.iter().zip(&variants) {
            builder.seal_block(*block);
            builder.switch_to_block(*block);
            for field in variant.fields {
                Self::emit_field_copy(builder, ctx, dst, src, field)?;
            }
            builder.ins().jump(merge, &[]);
        }
        builder.seal_block(otherwise);
        builder.switch_to_block(otherwise);
        invalid_tag_trap(builder);
        builder.seal_block(merge);
        builder.switch_to_block(merge);
        Ok(())
    }

    /// Whole-enum destruction (M11): load the discriminant, switch,
    /// and drop each needs-drop payload field of the ACTIVE variant
    /// via the struct field-drop helper (`emit_field_drop`, which
    /// recurses into nested structs and enums). Copy-only variants
    /// fall through to the merge with no work.
    pub(crate) fn emit_enum_drop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        addr: Value,
        ty: TypeId,
    ) -> Result<(), String> {
        let tag = builder
            .ins()
            .load(types::I32, MemFlagsData::trusted(), addr, 0);
        let variants: Vec<_> = ctx.pool.enum_view(ty).variants().collect();
        let merge = builder.create_block();
        let (cases, otherwise) = Self::emit_tag_dispatch(builder, variants.len(), tag);
        for (block, variant) in cases.iter().zip(&variants) {
            builder.seal_block(*block);
            builder.switch_to_block(*block);
            for field in variant.fields {
                if ctx.pool.needs_drop(field.ty) {
                    Self::emit_field_drop(builder, ctx, addr, field.offset, field.ty)?;
                }
            }
            builder.ins().jump(merge, &[]);
        }
        builder.seal_block(otherwise);
        builder.switch_to_block(otherwise);
        invalid_tag_trap(builder);
        builder.seal_block(merge);
        builder.switch_to_block(merge);
        Ok(())
    }

    /// Discriminant-then-payload enum equality (M11): the i32 tags
    /// compare first — differing tags short-circuit to false without
    /// touching payload memory. The same-tag arm switches on the tag
    /// (the operands' tags are equal there) and AND-reduces one
    /// field compare per payload field via `emit_field_eq`, the shared
    /// struct memberwise helper. Per-variant results merge through one
    /// i8 block param; `negate` flips the merged value for `!=`,
    /// exactly like `emit_struct_eq`. Both operands are borrowed,
    /// never consumed.
    pub(crate) fn emit_enum_eq(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        lhs_addr: Value,
        rhs_addr: Value,
        ty: TypeId,
        negate: bool,
    ) -> Result<Value, String> {
        let ltag = builder
            .ins()
            .load(types::I32, MemFlagsData::trusted(), lhs_addr, 0);
        let rtag = builder
            .ins()
            .load(types::I32, MemFlagsData::trusted(), rhs_addr, 0);
        let same_tag = builder.ins().icmp(IntCC::Equal, ltag, rtag);
        let same_block = builder.create_block();
        let diff_block = builder.create_block();
        let merge = builder.create_block();
        builder.append_block_param(merge, types::I8);
        builder
            .ins()
            .brif(same_tag, same_block, &[], diff_block, &[]);

        // Tags differ: never equal, regardless of payload padding.
        builder.seal_block(diff_block);
        builder.switch_to_block(diff_block);
        let zero = builder.ins().iconst(types::I8, 0);
        builder.ins().jump(merge, &[BlockArg::Value(zero)]);

        // Tags equal: dispatch on the (equal) discriminant and
        // memberwise-compare the active variant's payload.
        builder.seal_block(same_block);
        builder.switch_to_block(same_block);
        let variants: Vec<_> = ctx.pool.enum_view(ty).variants().collect();
        let (cases, otherwise) = Self::emit_tag_dispatch(builder, variants.len(), ltag);
        for (block, variant) in cases.iter().zip(&variants) {
            builder.seal_block(*block);
            builder.switch_to_block(*block);
            let mut acc: Option<Value> = None;
            for field in variant.fields {
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
                let field_eq = Self::emit_field_eq(builder, ctx, lhs_field, rhs_field, field)?;
                acc = Some(match acc {
                    None => field_eq,
                    Some(prev) => builder.ins().band(prev, field_eq),
                });
            }
            let result = match acc {
                Some(v) => v,
                // Two unit values of the same variant are equal.
                None => builder.ins().iconst(types::I8, 1),
            };
            builder.ins().jump(merge, &[BlockArg::Value(result)]);
        }
        builder.seal_block(otherwise);
        builder.switch_to_block(otherwise);
        invalid_tag_trap(builder);
        builder.seal_block(merge);
        builder.switch_to_block(merge);
        let value = builder.block_params(merge)[0];
        if negate {
            let zero = builder.ins().iconst(types::I8, 0);
            Ok(builder.ins().icmp(IntCC::Equal, value, zero))
        } else {
            Ok(value)
        }
    }

    /// Debug representation of the enum value at `addr` (M11): builds
    /// `Name.Unit`, `Name.Tuple(v, v)`, or `Name.Named{f=v, f=v}` into
    /// a fresh `RyoStrFat` slot whose address is returned. Unit
    /// variants render bare (`Color.Red`); tuple variants render in
    /// parens (`Result.Success(5)`); named variants render in braces
    /// with `=` separators and `, ` field separators, exactly matching
    /// the struct repr (`Shape.Rectangle{width=1.0, height=2.0}`). The
    /// enum name pushes before the dispatch; each variant arm renders
    /// its payload fields through the shared struct field-value helper
    /// (`emit_debug_field_value`), so str fields render quoted and
    /// nested structs/enums recurse via the existing Debug paths.
    /// Borrows the operand — the enum keeps owning its fields.
    pub(crate) fn eval_enum_debug_repr(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        addr: Value,
        ty: TypeId,
    ) -> Result<Value, String> {
        let view = ctx.pool.enum_view(ty);
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

        Self::push_debug_name(builder, ctx, result, view.name())?;
        let tag = builder
            .ins()
            .load(types::I32, MemFlagsData::trusted(), addr, 0);
        let variants: Vec<_> = view.variants().collect();
        let merge = builder.create_block();
        let (cases, otherwise) = Self::emit_tag_dispatch(builder, variants.len(), tag);
        for (block, variant) in cases.iter().zip(&variants) {
            builder.seal_block(*block);
            builder.switch_to_block(*block);
            // `Name.Unit`, `Name.Tuple(...)`, `Name.Named{...}` — the
            // `.Variant` suffix pushes inside every arm (unit arms
            // included); only the payload punctuation differs.
            Self::push_debug_static(builder, ctx, result, ".")?;
            Self::push_debug_name(builder, ctx, result, variant.name)?;
            match variant.kind {
                VariantKind::Unit => {}
                VariantKind::Tuple => {
                    Self::push_debug_static(builder, ctx, result, "(")?;
                    Self::emit_debug_variant_fields(builder, ctx, result, addr, variant, false)?;
                    Self::push_debug_static(builder, ctx, result, ")")?;
                }
                VariantKind::Named => {
                    Self::push_debug_static(builder, ctx, result, "{")?;
                    Self::emit_debug_variant_fields(builder, ctx, result, addr, variant, true)?;
                    Self::push_debug_static(builder, ctx, result, "}")?;
                }
            }
            builder.ins().jump(merge, &[]);
        }
        builder.seal_block(otherwise);
        builder.switch_to_block(otherwise);
        invalid_tag_trap(builder);
        builder.seal_block(merge);
        builder.switch_to_block(merge);
        Ok(result)
    }

    /// Push one variant payload's Debug fields onto `result`: `v, v`
    /// for a tuple variant, `f=v, f=v` for a named one (`, ` separator
    /// and `=` bindings, matching the struct repr). Each field renders
    /// through the shared struct field-value helper
    /// (`emit_debug_field_value`), so rendered temps free after their
    /// push and nested aggregates recurse.
    fn emit_debug_variant_fields(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        result: Value,
        addr: Value,
        variant: &EnumVariantView<'_>,
        named: bool,
    ) -> Result<(), String> {
        for (i, field) in variant.fields.iter().enumerate() {
            if i > 0 {
                Self::push_debug_static(builder, ctx, result, ", ")?;
            }
            if named {
                Self::push_debug_name(builder, ctx, result, field.name)?;
                Self::push_debug_static(builder, ctx, result, "=")?;
            }
            let field_addr = if field.offset == 0 {
                addr
            } else {
                builder.ins().iadd_imm_s(addr, i64::from(field.offset))
            };
            Self::emit_debug_field_value(builder, ctx, result, field_addr, field.ty)?;
        }
        Ok(())
    }

    /// `name = <enum value>` (M11): bind `name` to the value's slot. A
    /// fresh producer (EnumLit, enum-returning call) hands its slot
    /// over directly; any other source (a moved/copied Var) is copied
    /// tag + active payload into a fresh slot so the binding owns
    /// storage independent of the source. Mirrors
    /// `emit_struct_var_decl`.
    pub(crate) fn emit_enum_var_decl(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let inst = ctx.tir.inst(r);
        let view = ctx.tir.var_decl_view(r);
        let init_tag = ctx.tir.inst(view.initializer).tag;
        let addr = match init_tag {
            TirTag::EnumLit | TirTag::Call => Self::eval_inst_enum(builder, ctx, view.initializer)?,
            _ => {
                let src = Self::eval_inst_enum(builder, ctx, view.initializer)?;
                let slot = Self::enum_slot(builder, ctx, inst.ty);
                let dst = builder.ins().stack_addr(ctx.int_type, slot, 0);
                Self::emit_enum_copy(builder, ctx, dst, src, inst.ty)?;
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

    /// `name = <enum value>` on an existing binding (M11): evaluate
    /// the RHS first (it may borrow the binding being overwritten),
    /// drop the old payload when the ownership pass scheduled it
    /// (`free_on_reassign`), then copy the new value (tag + active
    /// payload) into the existing slot. Mirrors `emit_struct_assign`.
    pub(crate) fn emit_enum_assign(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let inst = ctx.tir.inst(r);
        let view = ctx.tir.assign_view(r);
        let var = Self::read_slot(&ctx.struct_locals, view.name).ok_or_else(|| {
            format!(
                "Undefined enum variable in assign: '{}'",
                ctx.pool.str(view.name)
            )
        })?;
        let dst = builder.use_var(var);
        let src = Self::eval_inst_enum(builder, ctx, view.value)?;
        if ctx.sidecar.free_on_reassign[r.index()].is_some() {
            Self::emit_enum_drop(builder, ctx, dst, inst.ty)?;
        }
        Self::emit_enum_copy(builder, ctx, dst, src, inst.ty)?;
        Ok(Terminator::None)
    }

    /// Enum return (M11): sret — copy the value (tag + active payload)
    /// field-wise through the caller-provided out-pointer, then return
    /// no IR values. Mirrors `emit_struct_return` and the fat-return
    /// path (mod.rs Return arm).
    pub(crate) fn emit_enum_return(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        operand: TirRef,
    ) -> Result<Terminator, String> {
        let sret = ctx.sret_ptr.expect("enum-returning fn must have sret_ptr");
        let src = Self::eval_inst_enum(builder, ctx, operand)?;
        let ret_ty = ctx.tir.return_type;
        Self::emit_enum_copy(builder, ctx, sret, src, ret_ty)?;
        Self::emit_due_frees(builder, ctx, r)?;
        Self::emit_due_promo_frees(builder, ctx, r)?;
        Self::emit_return(builder, ctx, &[])?;
        Ok(Terminator::Return)
    }

    /// Lower an enum-typed call argument (M11): every mode passes one
    /// pointer, exactly like structs. Borrow passes the existing slot
    /// address; Move/Copy transfer a tag + active-payload copy in a
    /// fresh caller-side slot (the callee may drop the contents per
    /// its mode).
    pub(crate) fn emit_enum_call_arg(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        mode: ParamMode,
    ) -> Result<Value, String> {
        let src = Self::eval_inst_enum(builder, ctx, r)?;
        if mode == ParamMode::Borrow {
            return Ok(src);
        }
        let ty = ctx.tir.inst(r).ty;
        let slot = Self::enum_slot(builder, ctx, ty);
        let dst = builder.ins().stack_addr(ctx.int_type, slot, 0);
        Self::emit_enum_copy(builder, ctx, dst, src, ty)?;
        Ok(dst)
    }

    /// Whole-enum scheduled Free (M11): when `target` is enum-typed,
    /// resolve its slot address — the binding's CURRENT
    /// `struct_locals` entry (same reasoning as the fat
    /// `free_binding_names` path; enum bindings share the
    /// family-agnostic table) or the producing inst's cached
    /// `ValueRepr::Enum` — and run the tag-dispatched payload drop.
    /// Returns `Ok(true)` when the target was an enum (handled), so
    /// the caller skips the struct and fat paths.
    pub(crate) fn try_emit_enum_free(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        target: TirRef,
    ) -> Result<bool, String> {
        let ty = if let Some(idx) = target.as_param_index() {
            ctx.tir.params[idx as usize].ty
        } else {
            ctx.tir.inst(target).ty
        };
        if !matches!(ctx.pool.kind(ty), TypeKind::Enum) {
            return Ok(false);
        }
        let addr = match Self::free_binding_name(ctx, target)
            .and_then(|name| Self::read_slot(&ctx.struct_locals, name))
        {
            Some(var) => builder.use_var(var),
            None => match Self::cached_repr(ctx, target) {
                Some(ValueRepr::Enum { addr }) => addr,
                _ => {
                    return Err(format!(
                        "ownership pass scheduled Free for enum %{} but no enum local or ValueRepr is available",
                        target.index()
                    ));
                }
            },
        };
        Self::emit_enum_drop(builder, ctx, addr, ty)?;
        Ok(true)
    }

    /// Enum arm of `emit_conditional_dead_drops` (M11): the pre-if
    /// value of a conditionally-reassigned enum binding is dropped on
    /// the arms that kept it (tag-dispatched payload drop). Returns
    /// `Ok(true)` when `target` is an enum (handled), so the caller
    /// skips the struct and fat paths.
    pub(crate) fn try_emit_enum_dead_drop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        name: StringId,
        target: TirRef,
    ) -> Result<bool, String> {
        debug_assert!(!target.is_param());
        let ty = ctx.tir.inst(target).ty;
        if !matches!(ctx.pool.kind(ty), TypeKind::Enum) {
            return Ok(false);
        }
        let Some(var) = Self::read_slot(&ctx.struct_locals, name) else {
            return Ok(false);
        };
        let addr = builder.use_var(var);
        Self::emit_enum_drop(builder, ctx, addr, ty)?;
        Ok(true)
    }
}
