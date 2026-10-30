//! `eval_inst_fat_slot` — the fat-slot mirror of `eval_inst`, split from
//! `expr.rs`; see module docs there.

use super::{Codegen, FunctionContext, ValueRepr};
use cranelift::codegen::ir::{MemFlagsData, StackSlot};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{TirData, TirRef, TirTag};
use ryo_core::types::TypeKind;

impl<M: Module> Codegen<M> {
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
                if let Some(repr) =
                    Self::emit_fat_builtin_call(builder, ctx, view.name, view.args, out_slot)?
                {
                    repr
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
            TirTag::ErrorMessage => {
                debug_assert!(
                    out_slot.is_none(),
                    "ErrorMessage manages its own result slot"
                );
                super::super::errors::emit_error_message(builder, ctx, r)?
            }
            TirTag::DebugRepr => {
                // M9.1/M11 print() gate: render the operand's Debug
                // representation into a fresh owned str. Primitive
                // operands render bare through the ryo_*_to_str
                // family (no braces); struct operands recurse through
                // `emit_debug_repr` (structs.rs); enum operands through
                // `eval_enum_debug_repr` (enums.rs). The cached triple
                // feeds print's `eval_str_or_view_parts` like any
                // other str temp, and the ownership pass's scheduled
                // Free releases the buffer after the statement.
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("DebugRepr must carry TirData::UnOp"),
                };
                let operand_ty = ctx.tir.inst(operand).ty;
                match ctx.pool.kind(operand_ty) {
                    TypeKind::Int | TypeKind::Float | TypeKind::Bool => {
                        let v = Self::eval_inst(builder, ctx, operand)?;
                        let (fn_name, param_ty) = match ctx.pool.kind(operand_ty) {
                            TypeKind::Int => ("ryo_int_to_str", ctx.int_type),
                            TypeKind::Float => ("ryo_float_to_str", types::F64),
                            _ => ("ryo_bool_to_str", types::I8),
                        };
                        let (ptr, len, cap) = Self::emit_slot_out_call(
                            builder,
                            ctx,
                            fn_name,
                            &[(param_ty, v)],
                            out_slot,
                        )?;
                        ValueRepr::Str { ptr, len, cap }
                    }
                    TypeKind::Struct | TypeKind::AnonStruct => {
                        // emit_debug_repr allocates its own result slot; a
                        // caller-provided out_slot would silently be ignored.
                        debug_assert!(
                            out_slot.is_none(),
                            "DebugRepr struct operand manages its own repr slot"
                        );
                        let addr = Self::eval_inst_struct(builder, ctx, operand)?;
                        let repr_addr = Self::emit_debug_repr(builder, ctx, addr, operand_ty)?;
                        let ptr =
                            builder
                                .ins()
                                .load(ctx.int_type, MemFlagsData::trusted(), repr_addr, 0);
                        let len =
                            builder
                                .ins()
                                .load(types::I64, MemFlagsData::trusted(), repr_addr, 8);
                        let cap =
                            builder
                                .ins()
                                .load(types::I64, MemFlagsData::trusted(), repr_addr, 16);
                        ValueRepr::Str { ptr, len, cap }
                    }
                    TypeKind::Enum
                    | TypeKind::DeclaredError
                    | TypeKind::ErrorSet
                    | TypeKind::ErrorUnion => {
                        // M11: enum operands render through the
                        // tag-dispatched enum Debug repr (unit
                        // `Color.Red`, tuple `Result.Success(5)`,
                        // named `Shape.Rectangle{...}`). The repr
                        // allocates its own result slot; a
                        // caller-provided out_slot would silently be
                        // ignored.
                        debug_assert!(
                            out_slot.is_none(),
                            "DebugRepr enum operand manages its own repr slot"
                        );
                        let addr = Self::eval_inst_enum(builder, ctx, operand)?;
                        let repr_addr = Self::eval_enum_debug_repr(builder, ctx, addr, operand_ty)?;
                        let ptr =
                            builder
                                .ins()
                                .load(ctx.int_type, MemFlagsData::trusted(), repr_addr, 0);
                        let len =
                            builder
                                .ins()
                                .load(types::I64, MemFlagsData::trusted(), repr_addr, 8);
                        let cap =
                            builder
                                .ins()
                                .load(types::I64, MemFlagsData::trusted(), repr_addr, 16);
                        ValueRepr::Str { ptr, len, cap }
                    }
                    other => {
                        return Err(format!(
                            "eval_inst_fat_slot: DebugRepr operand at %{} has non-renderable type kind {other:?}",
                            operand.index()
                        ));
                    }
                }
            }
            TirTag::Match => {
                // M12 aggregate-result match: the Switch handler writes
                // each value arm's result into the match's own result
                // slot and the join prologue caches the loaded fat
                // triple. An out_slot cannot be honored here — the
                // match lowering allocates its result slot before the
                // dispatch — and VarDecl/Assign only pass out_slot to
                // slot-out producers, which a Match never is.
                debug_assert!(
                    out_slot.is_none(),
                    "aggregate match lowering manages its own result slot"
                );
                Self::cached_repr(ctx, r).ok_or_else(|| {
                    format!(
                        "eval_inst_fat_slot: match at %{} reached before its Switch terminator cached a fat repr",
                        r.index()
                    )
                })?
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
}
