//! View-creation codegen (M8.4) — split from `expr.rs` to keep every
//! file under the 2000-line CI cap (`scripts/check_file_length.sh`).
//!
//! Central entry point: `emit_ensure_heap_for_view_base`, the single
//! choke point every view-creating op (slice, `ToView`) uses to turn an
//! owner-typed base into stable, never-moving memory.

use cranelift::codegen::ir::{MemFlagsData, StackSlotData, StackSlotKind};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{TirRef, TirTag};
use ryo_core::types::TypeKind;

use super::{Codegen, FunctionContext, STR_SLOT_SIZE, ValueRepr};

impl<M: Module> Codegen<M> {
    /// Materialize an owner-typed (`str`/`bytes`) value for VIEW
    /// CREATION: call the family `ensure_heap` (promotes inline → heap
    /// in place) on owner-side storage and return a stable `(ptr, len)`
    /// that outlives this expression. Views into `.rodata`/heap were
    /// already stable; this adds the inline case (promote-on-view).
    ///
    /// The promotion must land where the owner's eventual free reads
    /// it, or the fresh heap buffer leaks while the owner's stale
    /// inline tag makes its free a no-op:
    /// - Field bases promote in place at the field address — the
    ///   struct's own slot is the storage its drop glue reads.
    /// - Named bindings spill → call → reload → `def_var` back into
    ///   their `FatLocals` (the str_push write-back shape; SSA-correct
    ///   at every later program point, including branch joins).
    /// - Anonymous temporaries spill into a scratch slot and re-cache
    ///   the promoted triple — their scheduled Free reads `cached_repr`.
    pub(crate) fn emit_ensure_heap_for_view_base(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<(Value, Value), String> {
        // A view-typed base (reslice, strview of a view param) already
        // addresses stable memory — pass it through untouched.
        if ctx.pool.is_view(ctx.tir.inst(r).ty) {
            let ValueRepr::View { ptr, len } = Self::eval_inst_view(builder, ctx, r)? else {
                unreachable!("eval_inst_view must produce ValueRepr::View");
            };
            return Ok((ptr, len));
        }

        // Field base: promote in place — the field's slot inside the
        // struct IS the owner-side storage (its drop glue loads the
        // field triple from this address).
        if matches!(ctx.tir.inst(r).tag, TirTag::FieldAccess) {
            let (addr, field_ty) = Self::field_addr_of(builder, ctx, r)?;
            let is_bytes = matches!(ctx.pool.kind(field_ty), TypeKind::Bytes);
            let callee = if is_bytes {
                "__ryo_bytes_ensure_heap"
            } else {
                "__ryo_str_ensure_heap"
            };
            let func_ref =
                Self::declare_runtime_fn(ctx.module, builder, callee, &[ctx.int_type], &[])?;
            builder.ins().call(func_ref, &[addr]);
            let out_ptr = builder
                .ins()
                .load(ctx.int_type, MemFlagsData::trusted(), addr, 0);
            let out_len = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 8);
            let out_cap = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 16);
            let repr = if is_bytes {
                ValueRepr::Bytes {
                    ptr: out_ptr,
                    len: out_len,
                    cap: out_cap,
                }
            } else {
                ValueRepr::Str {
                    ptr: out_ptr,
                    len: out_len,
                    cap: out_cap,
                }
            };
            Self::cache_repr(ctx, r, repr);
            return Ok((out_ptr, out_len));
        }

        let (ptr, len, cap, is_bytes) = match Self::eval_inst_fat(builder, ctx, r)? {
            ValueRepr::Str { ptr, len, cap } => (ptr, len, cap, false),
            ValueRepr::Bytes { ptr, len, cap } => (ptr, len, cap, true),
            _ => unreachable!("view base must be fat or view typed"),
        };
        // Static .rodata bases are already stable: skip the
        // spill/call/reload entirely so the cached cap stays an
        // `iconst 0` and downstream dead-free elision keeps firing.
        if Self::is_static_cap_zero(builder.func, cap) {
            return Ok((ptr, len));
        }
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            STR_SLOT_SIZE,
            3,
        ));
        let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
        builder.ins().store(MemFlagsData::trusted(), ptr, addr, 0);
        builder.ins().store(MemFlagsData::trusted(), len, addr, 8);
        builder.ins().store(MemFlagsData::trusted(), cap, addr, 16);
        let callee = if is_bytes {
            "__ryo_bytes_ensure_heap"
        } else {
            "__ryo_str_ensure_heap"
        };
        let func_ref = Self::declare_runtime_fn(ctx.module, builder, callee, &[ctx.int_type], &[])?;
        builder.ins().call(func_ref, &[addr]);
        let out_ptr = builder
            .ins()
            .load(ctx.int_type, MemFlagsData::trusted(), addr, 0);
        let out_len = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), addr, 8);
        let out_cap = builder
            .ins()
            .load(types::I64, MemFlagsData::trusted(), addr, 16);
        // Write the (possibly promoted) triple back into owner-side
        // storage so the owner's free releases the heap buffer.
        match Self::local_name_of(ctx, r) {
            Some(name) => {
                if let Some(sl) = Self::read_slot(&ctx.fat_locals, name) {
                    builder.def_var(sl.ptr, out_ptr);
                    builder.def_var(sl.len, out_len);
                    builder.def_var(sl.cap, out_cap);
                }
            }
            None => {
                let repr = if is_bytes {
                    ValueRepr::Bytes {
                        ptr: out_ptr,
                        len: out_len,
                        cap: out_cap,
                    }
                } else {
                    ValueRepr::Str {
                        ptr: out_ptr,
                        len: out_len,
                        cap: out_cap,
                    }
                };
                Self::cache_repr(ctx, r, repr);
            }
        }
        Ok((out_ptr, out_len))
    }
}
