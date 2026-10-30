//! Error-union construction, active-alternative widening, and error messages.

use cranelift::codegen::ir::{MemFlagsData, StackSlot, StackSlotData, StackSlotKind};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{TirData, TirRef};
use ryo_core::types::{EnumVariantView, StructField, TypeId, TypeKind, VariantKind};

use super::enums::invalid_tag_trap;
use super::{Codegen, FunctionContext, STR_SLOT_SIZE, ValueRepr, cranelift_type_for};

pub(crate) fn emit_error_message<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    r: TirRef,
) -> Result<ValueRepr, String> {
    let inst = ctx.tir.inst(r);
    let TirData::UnOp(operand) = inst.data else {
        unreachable!("ErrorMessage must carry an operand")
    };
    let operand_ty = ctx.tir.inst(operand).ty;
    debug_assert!(matches!(
        ctx.pool.kind(operand_ty),
        TypeKind::DeclaredError | TypeKind::ErrorSet
    ));
    let addr = Codegen::eval_inst_enum(builder, ctx, operand)?;
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

    let variants: Vec<_> = ctx.pool.enum_view(operand_ty).variants().collect();
    let tag = builder
        .ins()
        .load(types::I32, MemFlagsData::trusted(), addr, 0);
    let merge = builder.create_block();
    let (cases, otherwise) = Codegen::<M>::emit_tag_dispatch(builder, variants.len(), tag);
    for (block, variant) in cases.iter().zip(&variants) {
        builder.seal_block(*block);
        builder.switch_to_block(*block);
        emit_active_error_message(builder, ctx, slot, result, addr, variant)?;
        builder.ins().jump(merge, &[]);
    }
    builder.seal_block(otherwise);
    builder.switch_to_block(otherwise);
    invalid_tag_trap(builder);
    builder.seal_block(merge);
    builder.switch_to_block(merge);

    let ptr = builder
        .ins()
        .load(ctx.int_type, MemFlagsData::trusted(), result, 0);
    let len = builder
        .ins()
        .load(types::I64, MemFlagsData::trusted(), result, 8);
    let cap = builder
        .ins()
        .load(types::I64, MemFlagsData::trusted(), result, 16);
    Ok(ValueRepr::Str { ptr, len, cap })
}

fn emit_active_error_message<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    slot: StackSlot,
    result: Value,
    addr: Value,
    variant: &EnumVariantView<'_>,
) -> Result<(), String> {
    if variant.fields.is_empty() {
        return Codegen::push_debug_name(builder, ctx, result, variant.name);
    }

    let string_field =
        if variant.fields.len() == 1 && ctx.pool.kind(variant.fields[0].ty) == TypeKind::Str {
            Some(variant.fields[0])
        } else {
            let message_name = ctx.pool.find_str("message");
            variant
                .fields
                .iter()
                .find(|field| {
                    Some(field.name) == message_name && ctx.pool.kind(field.ty) == TypeKind::Str
                })
                .copied()
        };
    if let Some(field) = string_field {
        return emit_error_string_copy(builder, ctx, slot, addr, field);
    }

    Codegen::push_debug_name(builder, ctx, result, variant.name)?;
    Codegen::push_debug_static(builder, ctx, result, "(")?;
    let named = variant.kind == VariantKind::Named;
    for (index, field) in variant.fields.iter().enumerate() {
        if index > 0 {
            Codegen::push_debug_static(builder, ctx, result, ", ")?;
        }
        if named {
            Codegen::push_debug_name(builder, ctx, result, field.name)?;
            Codegen::push_debug_static(builder, ctx, result, "=")?;
        }
        emit_error_field_value(builder, ctx, result, addr, *field)?;
    }
    Codegen::push_debug_static(builder, ctx, result, ")")
}

fn emit_error_string_copy<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    slot: StackSlot,
    addr: Value,
    field: StructField,
) -> Result<(), String> {
    let field_addr = offset_addr(builder, addr, field.offset);
    let ptr = builder
        .ins()
        .load(ctx.int_type, MemFlagsData::trusted(), field_addr, 0);
    let len = builder
        .ins()
        .load(types::I64, MemFlagsData::trusted(), field_addr, 8);
    let cap = builder
        .ins()
        .load(types::I64, MemFlagsData::trusted(), field_addr, 16);
    let (ptr, len) =
        Codegen::emit_fat_bytes_ptr_len(builder, ctx, ptr, len, cap, Some(field_addr))?;
    Codegen::emit_slot_out_call(
        builder,
        ctx,
        "ryo_str_from_view",
        &[(ctx.int_type, ptr), (types::I64, len)],
        Some(slot),
    )?;
    Ok(())
}

fn emit_error_field_value<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    result: Value,
    addr: Value,
    field: StructField,
) -> Result<(), String> {
    if ctx.pool.kind(field.ty) != TypeKind::Str {
        let field_addr = offset_addr(builder, addr, field.offset);
        return Codegen::emit_debug_field_value(builder, ctx, result, field_addr, field.ty);
    }

    let field_addr = offset_addr(builder, addr, field.offset);
    let ptr = builder
        .ins()
        .load(ctx.int_type, MemFlagsData::trusted(), field_addr, 0);
    let len = builder
        .ins()
        .load(types::I64, MemFlagsData::trusted(), field_addr, 8);
    let cap = builder
        .ins()
        .load(types::I64, MemFlagsData::trusted(), field_addr, 16);
    let (ptr, len) =
        Codegen::emit_fat_bytes_ptr_len(builder, ctx, ptr, len, cap, Some(field_addr))?;
    Codegen::emit_debug_push(builder, ctx, result, ptr, len)
}

pub(crate) fn emit_error_union_coerce<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    r: TirRef,
) -> Result<Value, String> {
    let inst = ctx.tir.inst(r);
    let TirData::UnOp(operand) = inst.data else {
        unreachable!("ErrorUnionCoerce must carry an operand")
    };
    let target = inst.ty;
    let source = ctx.tir.inst(operand).ty;
    let slot = Codegen::enum_slot(builder, ctx, target);
    let dst = builder.ins().stack_addr(ctx.int_type, slot, 0);
    match ctx.pool.kind(source) {
        TypeKind::ErrorUnion => emit_union_widening(builder, ctx, operand, dst, source, target)?,
        TypeKind::DeclaredError | TypeKind::ErrorSet => {
            let target_err = union_variant_fields(ctx, target).1;
            store_union_tag(builder, dst, 1);
            let src = Codegen::eval_inst_enum(builder, ctx, operand)?;
            let dst_err = offset_addr(builder, dst, target_err.offset);
            emit_error_set_convert(builder, ctx, dst_err, src, source, target_err.ty)?;
        }
        TypeKind::InferredErrorUnion => {
            return Err("unresolved inferred error union reached codegen".into());
        }
        _ => {
            let target_ok = union_variant_fields(ctx, target).0;
            store_union_tag(builder, dst, 0);
            if target_ok.ty != ctx.pool.void() {
                Codegen::store_field_value(
                    builder,
                    ctx,
                    dst,
                    target_ok.offset,
                    target_ok.ty,
                    operand,
                )?;
            }
        }
    }
    Ok(dst)
}

fn union_variant_fields<M: Module>(
    ctx: &FunctionContext<'_, M>,
    ty: TypeId,
) -> (StructField, StructField) {
    let view = ctx.pool.enum_view(ty);
    let mut variants = view.variants();
    let ok = variants.next().expect("error union has Ok").fields[0];
    let err = variants.next().expect("error union has Err").fields[0];
    (ok, err)
}

pub(crate) fn emit_utf8_error_union<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    r: TirRef,
    success: Value,
    valid: Value,
) -> Result<Value, String> {
    const MESSAGE: &str = "bytes are not valid UTF-8";

    let target = ctx.tir.inst(r).ty;
    let (ok, err) = union_variant_fields(ctx, target);
    let slot = Codegen::enum_slot(builder, ctx, target);
    let dst = builder.ins().stack_addr(ctx.int_type, slot, 0);
    let ok_block = builder.create_block();
    let err_block = builder.create_block();
    let merge = builder.create_block();
    builder.ins().brif(valid, ok_block, &[], err_block, &[]);

    builder.seal_block(ok_block);
    builder.switch_to_block(ok_block);
    store_union_tag(builder, dst, 0);
    let ptr = builder
        .ins()
        .load(ctx.int_type, MemFlagsData::trusted(), success, 0);
    let len = builder
        .ins()
        .load(types::I64, MemFlagsData::trusted(), success, 8);
    let cap = builder
        .ins()
        .load(types::I64, MemFlagsData::trusted(), success, 16);
    let ok_addr = offset_addr(builder, dst, ok.offset);
    builder
        .ins()
        .store(MemFlagsData::trusted(), ptr, ok_addr, 0);
    builder
        .ins()
        .store(MemFlagsData::trusted(), len, ok_addr, 8);
    builder
        .ins()
        .store(MemFlagsData::trusted(), cap, ok_addr, 16);
    builder.ins().jump(merge, &[]);

    builder.seal_block(err_block);
    builder.switch_to_block(err_block);
    store_union_tag(builder, dst, 1);
    let err_addr = offset_addr(builder, dst, err.offset);
    store_union_tag(builder, err_addr, 0);
    let variant = ctx
        .pool
        .enum_view(err.ty)
        .variants()
        .next()
        .expect("Utf8Error variant");
    debug_assert_eq!(variant.fields.len(), 1);
    debug_assert_eq!(variant.fields[0].ty, ctx.pool.str_());
    let message_addr = offset_addr(builder, err_addr, variant.fields[0].offset);
    let data_id = Codegen::store_guard_msg(ctx.module, ctx.data_ctx, ctx.guard_msg_data, MESSAGE)?;
    let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
    let message_ptr = builder.ins().symbol_value(ctx.int_type, data_ref);
    let message_len = builder.ins().iconst(types::I64, MESSAGE.len() as i64);
    let static_cap = builder.ins().iconst(types::I64, 0);
    builder
        .ins()
        .store(MemFlagsData::trusted(), message_ptr, message_addr, 0);
    builder
        .ins()
        .store(MemFlagsData::trusted(), message_len, message_addr, 8);
    builder
        .ins()
        .store(MemFlagsData::trusted(), static_cap, message_addr, 16);
    builder.ins().jump(merge, &[]);

    builder.seal_block(merge);
    builder.switch_to_block(merge);
    Ok(dst)
}

fn store_union_tag(builder: &mut FunctionBuilder, dst: Value, tag: i64) {
    let tag = builder.ins().iconst(types::I32, tag);
    builder.ins().store(MemFlagsData::trusted(), tag, dst, 0);
}

fn offset_addr(builder: &mut FunctionBuilder, base: Value, offset: u32) -> Value {
    if offset == 0 {
        base
    } else {
        builder.ins().iadd_imm_s(base, i64::from(offset))
    }
}

fn emit_union_widening<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    operand: TirRef,
    dst: Value,
    source: TypeId,
    target: TypeId,
) -> Result<(), String> {
    let (source_ok, source_err) = union_variant_fields(ctx, source);
    let (target_ok, target_err) = union_variant_fields(ctx, target);
    let src = Codegen::eval_inst_enum(builder, ctx, operand)?;
    let tag = builder
        .ins()
        .load(types::I32, MemFlagsData::trusted(), src, 0);
    let merge = builder.create_block();
    let (cases, otherwise) = Codegen::<M>::emit_tag_dispatch(builder, 2, tag);

    builder.seal_block(cases[0]);
    builder.switch_to_block(cases[0]);
    store_union_tag(builder, dst, 0);
    if target_ok.ty != ctx.pool.void() {
        emit_field_copy_at(builder, ctx, dst, src, target_ok, source_ok)?;
    }
    builder.ins().jump(merge, &[]);

    builder.seal_block(cases[1]);
    builder.switch_to_block(cases[1]);
    store_union_tag(builder, dst, 1);
    let src_err = offset_addr(builder, src, source_err.offset);
    let dst_err = offset_addr(builder, dst, target_err.offset);
    emit_error_set_convert(builder, ctx, dst_err, src_err, source_err.ty, target_err.ty)?;
    builder.ins().jump(merge, &[]);

    builder.seal_block(otherwise);
    builder.switch_to_block(otherwise);
    invalid_tag_trap(builder);
    builder.seal_block(merge);
    builder.switch_to_block(merge);
    Ok(())
}

fn emit_error_set_convert<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    dst: Value,
    src: Value,
    source: TypeId,
    target: TypeId,
) -> Result<(), String> {
    if source == target {
        return Codegen::emit_enum_copy(builder, ctx, dst, src, source);
    }

    let source_members = ctx.pool.error_set_view(source).members;
    let target_members = ctx.pool.error_set_view(target).members;
    let source_variants: Vec<_> = ctx.pool.enum_view(source).variants().collect();
    let target_variants: Vec<_> = ctx.pool.enum_view(target).variants().collect();
    let tag = builder
        .ins()
        .load(types::I32, MemFlagsData::trusted(), src, 0);
    let merge = builder.create_block();
    let (cases, otherwise) = Codegen::<M>::emit_tag_dispatch(builder, source_variants.len(), tag);

    for (source_index, source_variant) in source_variants.iter().enumerate() {
        let member = source_members[source_index];
        let target_index = target_members
            .iter()
            .position(|candidate| *candidate == member)
            .ok_or_else(|| "error-set widening lost a nominal member".to_string())?;
        let target_variant = &target_variants[target_index];
        let block = cases[source_index];
        builder.seal_block(block);
        builder.switch_to_block(block);
        store_union_tag(builder, dst, target_index as i64);
        if source_variant.fields.len() != target_variant.fields.len() {
            return Err("nominal error payload shape changed during widening".into());
        }
        for (source_field, target_field) in source_variant
            .fields
            .iter()
            .zip(target_variant.fields.iter())
        {
            if source_field.ty != target_field.ty {
                return Err("nominal error payload type changed during widening".into());
            }
            emit_field_copy_at(builder, ctx, dst, src, *target_field, *source_field)?;
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

fn emit_field_copy_at<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    dst: Value,
    src: Value,
    target: StructField,
    source: StructField,
) -> Result<(), String> {
    debug_assert_eq!(source.ty, target.ty);
    let src_offset = i32::try_from(source.offset).expect("source field offset exceeds i32");
    let dst_offset = i32::try_from(target.offset).expect("target field offset exceeds i32");
    match ctx.pool.kind(source.ty) {
        TypeKind::Void => Ok(()),
        TypeKind::Str | TypeKind::Bytes => {
            let ptr = builder
                .ins()
                .load(ctx.int_type, MemFlagsData::trusted(), src, src_offset);
            let len = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), src, src_offset + 8);
            let cap = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), src, src_offset + 16);
            builder
                .ins()
                .store(MemFlagsData::trusted(), ptr, dst, dst_offset);
            builder
                .ins()
                .store(MemFlagsData::trusted(), len, dst, dst_offset + 8);
            builder
                .ins()
                .store(MemFlagsData::trusted(), cap, dst, dst_offset + 16);
            Ok(())
        }
        TypeKind::View(_) => Err("view error payload reached codegen".into()),
        TypeKind::Struct | TypeKind::AnonStruct => {
            let src = offset_addr(builder, src, source.offset);
            let dst = offset_addr(builder, dst, target.offset);
            Codegen::emit_struct_copy(builder, ctx, dst, src, source.ty)
        }
        TypeKind::Enum | TypeKind::DeclaredError | TypeKind::ErrorSet | TypeKind::ErrorUnion => {
            let src = offset_addr(builder, src, source.offset);
            let dst = offset_addr(builder, dst, target.offset);
            Codegen::emit_enum_copy(builder, ctx, dst, src, source.ty)
        }
        _ => {
            let ty = cranelift_type_for(source.ty, ctx.pool, ctx.int_type);
            let value = builder
                .ins()
                .load(ty, MemFlagsData::trusted(), src, src_offset);
            builder
                .ins()
                .store(MemFlagsData::trusted(), value, dst, dst_offset);
            Ok(())
        }
    }
}
