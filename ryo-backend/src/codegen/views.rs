//! View-creation codegen (M8.4) — split from `expr.rs` to keep every
//! file under the 2000-line CI cap (`scripts/check_file_length.sh`).
//!
//! Central entry point: `emit_ensure_heap_for_view_base`, the single
//! choke point every view-creating op (slice, `ToView`) uses to turn an
//! owner-typed base into stable, never-moving memory.

use cranelift::codegen::ir::{BlockArg, FuncRef, MemFlagsData, StackSlotData, StackSlotKind};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{TirRef, TirTag};
use ryo_core::types::TypeKind;

use super::{Codegen, FunctionContext, STR_SLOT_SIZE, ValueRepr};

impl<M: Module> Codegen<M> {
    /// Materialize an owner-typed (`str`/`bytes`) value for VIEW
    /// CREATION: return a stable `(ptr, len)` that outlives this
    /// expression. Views into `.rodata`/heap were already stable; the
    /// inline case is handled by branching on the runtime tag — an
    /// inline base is promoted in place via the family `ensure_heap`,
    /// a heap or static base passes through with no call.
    ///
    /// The promotion must land where the owner's eventual free reads
    /// it, or the fresh heap buffer leaks while the owner's stale
    /// inline tag makes its free a no-op:
    /// - Field bases promote in place at the field address — the
    ///   struct's own slot is the storage its drop glue reads.
    /// - Named bindings spill → call → reload → `def_var` back into
    ///   their `FatLocals` (the str_push write-back shape; SSA-correct
    ///   at every later program point, including branch joins).
    ///   Borrowed `str`/`bytes` params are the exception: their
    ///   promoted triple goes into a per-base scratch slot (flag +
    ///   triple) instead of `FatLocals`, so the param keeps its
    ///   original inline triple and the caller's heap buffer is never
    ///   freed by the callee; the ownership pass schedules the free.
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
        // Borrowed-param base: the ownership pass scheduled a promotion
        // free for this base. The promoted triple goes into the scratch
        // slot — NOT back into the param's FatLocals, which must keep
        // the original inline triple so reads of the param after the
        // view's death stay valid, and so the caller's heap buffer is
        // never freed by the callee.
        let promo_slot = ctx.promo_slots.get(&r).copied();
        if promo_slot.is_some() {
            // The ownership pass only schedules promotion frees for
            // bases that are Vars of borrowed str/bytes params.
            debug_assert!(
                matches!(ctx.tir.inst(r).tag, TirTag::Var),
                "promotion base must be a named param Var"
            );
        }
        // Branch on the runtime tag: only an inline base needs the
        // spill/promote/reload round trip. A heap base is already
        // stable, so its (ptr, len) flows straight to the merge —
        // the extern call, three stores, and three loads all drop
        // off that path. The tag test mirrors the runtime's
        // `is_inline`: the cap word's top byte is 0x80|len for
        // inline strings.
        let tag = builder.ins().ushr_imm_u(cap, 56);
        let tag_bit = builder.ins().band_imm_u(tag, 0x80);
        let is_inline = builder.ins().icmp_imm_u(IntCC::NotEqual, tag_bit, 0);
        let inline_block = builder.create_block();
        let merge_block = builder.create_block();
        // The merge carries the full triple: an anonymous temporary's
        // scheduled Free reads its cached cap later, so the cap value
        // must dominate both paths, not just the inline one.
        builder.append_block_param(merge_block, ctx.int_type);
        builder.append_block_param(merge_block, types::I64);
        builder.append_block_param(merge_block, types::I64);
        let heap_block = if promo_slot.is_some() {
            Some(builder.create_block())
        } else {
            None
        };
        match heap_block {
            Some(hb) => builder.ins().brif(is_inline, inline_block, &[], hb, &[]),
            None => builder.ins().brif(
                is_inline,
                inline_block,
                &[],
                merge_block,
                &[
                    BlockArg::Value(ptr),
                    BlockArg::Value(len),
                    BlockArg::Value(cap),
                ],
            ),
        };
        // Single predecessor (the brif above) — seal immediately.
        builder.seal_block(inline_block);
        if let Some(hb) = heap_block {
            // Single predecessor (the brif above) — seal immediately.
            builder.seal_block(hb);
            builder.switch_to_block(hb);
            // Pass-through: record flag=0 + the caller's triple so the
            // scheduled free no-ops.
            let slot = promo_slot.expect("heap block exists only with a promo slot");
            let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
            let zero = builder.ins().iconst(types::I64, 0);
            builder.ins().store(MemFlagsData::trusted(), zero, addr, 0);
            builder.ins().store(MemFlagsData::trusted(), ptr, addr, 8);
            builder.ins().store(MemFlagsData::trusted(), len, addr, 16);
            builder.ins().store(MemFlagsData::trusted(), cap, addr, 24);
            builder.ins().jump(
                merge_block,
                &[
                    BlockArg::Value(ptr),
                    BlockArg::Value(len),
                    BlockArg::Value(cap),
                ],
            );
        }
        builder.switch_to_block(inline_block);
        if let Some(slot) = promo_slot {
            // Free-before-overwrite: a prior iteration's promotion
            // buffer is still recorded here when the view outlives one
            // loop iteration (loop-deferred death). The flag is zeroed
            // at function entry and after every free, so this is exact.
            let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
            let old_flag = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 0);
            let oldfree_block = builder.create_block();
            let spill_block = builder.create_block();
            builder
                .ins()
                .brif(old_flag, oldfree_block, &[], spill_block, &[]);
            // Single predecessor (the brif above) — seal immediately.
            builder.seal_block(oldfree_block);
            builder.switch_to_block(oldfree_block);
            let old_ptr = builder
                .ins()
                .load(ctx.int_type, MemFlagsData::trusted(), addr, 8);
            let old_cap = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 24);
            let free_callee = if is_bytes {
                "ryo_bytes_free"
            } else {
                "ryo_str_free"
            };
            let free_ref = Self::declare_runtime_fn(
                ctx.module,
                builder,
                free_callee,
                &[ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(free_ref, &[old_ptr, old_cap]);
            builder.ins().jump(spill_block, &[]);
            // Two predecessors (the brif else-edge and the oldfree
            // jump) — seal only now that both are emitted.
            builder.seal_block(spill_block);
            builder.switch_to_block(spill_block);
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
        if let Some(slot) = promo_slot {
            // Record flag=1 + the promoted triple in the scratch slot:
            // the ownership-pass-scheduled promo free releases this
            // buffer at the view's last use.
            let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
            let one = builder.ins().iconst(types::I64, 1);
            builder.ins().store(MemFlagsData::trusted(), one, addr, 0);
            builder
                .ins()
                .store(MemFlagsData::trusted(), out_ptr, addr, 8);
            builder
                .ins()
                .store(MemFlagsData::trusted(), out_len, addr, 16);
            builder
                .ins()
                .store(MemFlagsData::trusted(), out_cap, addr, 24);
        }
        // Write the promoted triple back into owner-side storage so
        // the owner's free releases the heap buffer. Only the inline
        // path needs this — on the heap path the binding's fat locals
        // already hold the identical bits. Borrowed-param bases skip
        // the write-back: their promoted triple lives in the promo
        // scratch slot above.
        let local_name = Self::local_name_of(ctx, r);
        if promo_slot.is_none()
            && let Some(name) = local_name
        {
            // Every fat binding gets FatLocals at the param/local
            // preamble, so a missing entry would be an invariant
            // violation; the silent fall-through is defensive only.
            if let Some(sl) = Self::read_slot(&ctx.fat_locals, name) {
                builder.def_var(sl.ptr, out_ptr);
                builder.def_var(sl.len, out_len);
                builder.def_var(sl.cap, out_cap);
            }
            // Invariant: after this write-back, the cached repr of the
            // binding's Var inst is STALE (it holds the pre-promotion
            // inline triple) — consumers must read the binding through
            // `fat_locals`, never through `cached_repr`. Latent, not
            // live: TIR is tree-shaped today, so each Var inst is
            // evaluated once at its own use site.
        }
        builder.ins().jump(
            merge_block,
            &[
                BlockArg::Value(out_ptr),
                BlockArg::Value(out_len),
                BlockArg::Value(out_cap),
            ],
        );
        builder.seal_block(merge_block);
        builder.switch_to_block(merge_block);
        let params = builder.block_params(merge_block);
        let (m_ptr, m_len, m_cap) = (params[0], params[1], params[2]);
        // Anonymous temporary: re-cache the merged triple (dominating
        // both paths) — its scheduled Free reads `cached_repr`.
        if local_name.is_none() {
            let repr = if is_bytes {
                ValueRepr::Bytes {
                    ptr: m_ptr,
                    len: m_len,
                    cap: m_cap,
                }
            } else {
                ValueRepr::Str {
                    ptr: m_ptr,
                    len: m_len,
                    cap: m_cap,
                }
            };
            Self::cache_repr(ctx, r, repr);
        }
        Ok((m_ptr, m_len))
    }

    /// Fire promotion frees anchored after `tir_ref`. Mirrors
    /// `emit_due_frees`.
    pub(crate) fn emit_due_promo_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        tir_ref: TirRef,
    ) -> Result<(), String> {
        if ctx.sidecar.promotion_frees.is_empty() {
            return Ok(());
        }
        let Some(indices) = ctx.promo_free_by_after.get(tir_ref.index()) else {
            return Ok(());
        };
        let pending: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&idx| {
                let pf = &ctx.sidecar.promotion_frees[idx];
                Self::branch_active(pf.branch, &ctx.branch_stack) && !ctx.promo_freed_at[idx]
            })
            .collect();
        Self::emit_promo_frees(builder, ctx, pending)
    }

    /// End-of-statement sweep for promotion frees whose anchor passed
    /// without an `emit_due_promo_frees` call. Mirrors `sweep_due_frees`.
    pub(crate) fn sweep_due_promo_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
    ) -> Result<(), String> {
        if ctx.pending_promo_sweep.is_empty() {
            return Ok(());
        }
        let pending: Vec<usize> = ctx
            .pending_promo_sweep
            .iter()
            .copied()
            .filter(|&idx| {
                let pf = &ctx.sidecar.promotion_frees[idx];
                Self::branch_active(pf.branch, &ctx.branch_stack)
                    && ctx.promo_slots.contains_key(&pf.base)
                    && Self::cached_repr(ctx, pf.after).is_some()
            })
            .collect();
        Self::emit_promo_frees(builder, ctx, pending)
    }

    /// Emit one flag-conditional free per pending promotion free: load
    /// the flag from the base's scratch slot; only on the promoted
    /// path free the recorded triple and clear the flag (so a later
    /// free-before-overwrite or duplicate anchor no-ops).
    fn emit_promo_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        pending: Vec<usize>,
    ) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let mut str_free_ref: Option<FuncRef> = None;
        let mut bytes_free_ref: Option<FuncRef> = None;
        for idx in pending {
            if ctx.promo_freed_at[idx] {
                continue;
            }
            let pf = ctx.sidecar.promotion_frees[idx].clone();
            // Invariant: compile_function creates a scratch slot for
            // every scheduled base — a missing entry would silently
            // drop the free, so fail loudly like emit_frees does.
            let slot = *ctx.promo_slots.get(&pf.base).ok_or_else(|| {
                format!(
                    "ownership pass scheduled promotion free for base %{} but no scratch slot was created",
                    pf.base.index()
                )
            })?;
            ctx.promo_freed_at[idx] = true;
            let is_bytes = matches!(ctx.pool.kind(ctx.tir.inst(pf.base).ty), TypeKind::Bytes);
            let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
            let flag = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 0);
            let free_block = builder.create_block();
            let done_block = builder.create_block();
            builder.ins().brif(flag, free_block, &[], done_block, &[]);
            builder.seal_block(free_block);
            builder.switch_to_block(free_block);
            let ptr = builder
                .ins()
                .load(ctx.int_type, MemFlagsData::trusted(), addr, 8);
            let cap = builder
                .ins()
                .load(types::I64, MemFlagsData::trusted(), addr, 24);
            let free_ref = Self::free_ref_for(
                builder,
                ctx,
                &mut str_free_ref,
                &mut bytes_free_ref,
                is_bytes,
            )?;
            builder.ins().call(free_ref, &[ptr, cap]);
            let zero = builder.ins().iconst(types::I64, 0);
            builder.ins().store(MemFlagsData::trusted(), zero, addr, 0);
            builder.ins().jump(done_block, &[]);
            builder.seal_block(done_block);
            builder.switch_to_block(done_block);
        }
        ctx.pending_promo_sweep
            .retain(|&idx| !ctx.promo_freed_at[idx]);
        Ok(())
    }
}
