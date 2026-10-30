//! Error-union construction and active-alternative widening.

use cranelift::codegen::ir::MemFlagsData;
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{TirData, TirRef};
use ryo_core::types::{StructField, TypeId, TypeKind};

use super::enums::invalid_tag_trap;
use super::{Codegen, FunctionContext, cranelift_type_for};

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
