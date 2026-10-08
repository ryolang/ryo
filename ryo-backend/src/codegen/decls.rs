//! Variable declaration and reassignment emission — split from
//! `mod.rs`'s `emit_stmt` so the statement dispatcher stays under the
//! workspace's small-function ratchet (`clippy.toml`'s
//! `too-many-lines-threshold`). Each entry dispatches on the binding's
//! type family (fat owner, view, struct, enum, scalar) and delegates
//! to the family's emitter; the family arms keep their original
//! comments.

use super::{
    Codegen, FatLocals, FunctionContext, STR_SLOT_SIZE, Terminator, ValueRepr, ViewLocals,
    cranelift_type_for, is_enum_type, is_fat_type, is_struct_type, structs,
};
use cranelift::codegen::ir::{MemFlagsData, StackSlotData, StackSlotKind};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::TirRef;
use ryo_core::types::TypeKind;

impl<M: Module> Codegen<M> {
    /// `TirTag::VarDecl` arm of `emit_stmt`: bind `name` to its
    /// initializer, in the type family's shape — fat home slot, view
    /// pair, struct/enum slot address (both delegated), or scalar
    /// `Variable`.
    pub(crate) fn emit_var_decl(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let inst = ctx.tir.inst(r);
        let view = ctx.tir.var_decl_view(r);
        if is_fat_type(inst.ty, ctx.pool) {
            // Producer-into-home: a slot-out producer
            // initializer (runtime producer call, concat, or
            // fat-returning user call) writes the binding's
            // canonical 24-byte home slot directly — no temp
            // slot, no reload-to-SSA, no second spill slot.
            // All later reads/writes go through the home.
            // Mutable bindings get a home even when the
            // initializer is not a producer: only they can be
            // reassigned, and a later slot-out reassign then
            // writes the home directly (and the home-provenance
            // free elision applies) instead of paying a temp
            // slot + triple store at every reassign.
            let produces_slot =
                structs::writes_out_slot_ids(ctx.tir, &ctx.name_ids, view.initializer);
            let home = if produces_slot || view.mutable {
                Some(builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    STR_SLOT_SIZE,
                    3,
                )))
            } else {
                None
            };
            let repr = Self::eval_inst_fat_slot(
                builder,
                ctx,
                view.initializer,
                home.filter(|_| produces_slot),
            )?;
            match repr {
                ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                    let var_ptr = builder.declare_var(ctx.int_type);
                    let var_len = builder.declare_var(types::I64);
                    let var_cap = builder.declare_var(types::I64);
                    // Home-backed bindings keep the triple in the
                    // slot only; the SSA Variables stay unused.
                    // A non-producer initializer needs the triple
                    // stored into the home by hand.
                    match home {
                        Some(home) if !produces_slot => {
                            let addr = builder.ins().stack_addr(ctx.int_type, home, 0);
                            builder.ins().store(MemFlagsData::trusted(), ptr, addr, 0);
                            builder.ins().store(MemFlagsData::trusted(), len, addr, 8);
                            builder.ins().store(MemFlagsData::trusted(), cap, addr, 16);
                        }
                        Some(_) => {}
                        None => {
                            builder.def_var(var_ptr, ptr);
                            builder.def_var(var_len, len);
                            builder.def_var(var_cap, cap);
                        }
                    }
                    let home_inline = home.is_some()
                        && Self::home_value_provably_inline(
                            ctx,
                            builder.func,
                            view.initializer,
                            cap,
                        );
                    Self::write_slot(
                        &mut ctx.fat_locals,
                        &mut ctx.fat_locals_undo,
                        view.name,
                        Some(FatLocals {
                            ptr: var_ptr,
                            len: var_len,
                            cap: var_cap,
                            home,
                            home_inline,
                        }),
                    );
                }
                _ => unreachable!("fat-typed initializer should produce a fat ValueRepr"),
            }
            return Ok(Terminator::None);
        }
        if ctx.pool.is_view(inst.ty) {
            let repr = Self::eval_inst_view(builder, ctx, view.initializer)?;
            match repr {
                ValueRepr::View { ptr, len } => {
                    let var_ptr = builder.declare_var(ctx.int_type);
                    let var_len = builder.declare_var(types::I64);
                    builder.def_var(var_ptr, ptr);
                    builder.def_var(var_len, len);
                    Self::write_slot(
                        &mut ctx.view_locals,
                        &mut ctx.view_locals_undo,
                        view.name,
                        Some(ViewLocals {
                            ptr: var_ptr,
                            len: var_len,
                        }),
                    );
                }
                _ => unreachable!("view-typed initializer should produce ValueRepr::View"),
            }
            // Same defensive fact removal as the scalar path
            // below: a same-scope redefinition must not inherit
            // a stale fact from the shadowed binding.
            Self::write_slot(
                &mut ctx.range_facts,
                &mut ctx.range_facts_undo,
                view.name,
                None,
            );
            return Ok(Terminator::None);
        }
        if is_struct_type(inst.ty, ctx.pool) {
            return Self::emit_struct_var_decl(builder, ctx, r);
        }
        if is_enum_type(inst.ty, ctx.pool) {
            return Self::emit_enum_var_decl(builder, ctx, r);
        }
        let val = Self::eval_inst(builder, ctx, view.initializer)?;
        // The variable's resolved type lives in the VarDecl
        // inst's `ty` slot directly — no side-table lookup.
        let cl_ty = cranelift_type_for(inst.ty, ctx.pool, ctx.int_type);
        let var = builder.declare_var(cl_ty);
        builder.def_var(var, val);
        // Defensive: a same-scope redefinition must not inherit
        // a stale fact from the shadowed binding. (No seeding
        // from constant initializers — explicit non-goal.)
        Self::write_slot(
            &mut ctx.range_facts,
            &mut ctx.range_facts_undo,
            view.name,
            None,
        );
        Self::write_slot(&mut ctx.locals, &mut ctx.locals_undo, view.name, Some(var));
        Ok(Terminator::None)
    }

    /// `TirTag::Assign` arm of `emit_stmt`: rebind an existing
    /// binding, in the type family's shape (fat home with the
    /// free-before-overwrite elisions, view reseat, struct/enum slot
    /// copy, scalar `def_var`).
    pub(crate) fn emit_assign(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let inst = ctx.tir.inst(r);
        let view = ctx.tir.assign_view(r);
        if is_fat_type(inst.ty, ctx.pool) {
            return Self::emit_fat_assign(builder, ctx, r);
        }
        if ctx.pool.is_view(inst.ty) {
            let repr = Self::eval_inst_view(builder, ctx, view.value)?;
            let ValueRepr::View { ptr, len } = repr else {
                unreachable!("view-typed assign should produce ValueRepr::View");
            };
            let locals = Self::read_slot(&ctx.view_locals, view.name).ok_or_else(|| {
                format!(
                    "Undefined strview variable in assign: '{}'",
                    ctx.pool.str(view.name)
                )
            })?;
            // Views are borrows — no free-on-reassign; just
            // reseat the pair.
            builder.def_var(locals.ptr, ptr);
            builder.def_var(locals.len, len);
            Self::kill_fact(ctx, view.name);
            return Ok(Terminator::None);
        }
        if is_struct_type(inst.ty, ctx.pool) {
            return Self::emit_struct_assign(builder, ctx, r);
        }
        if is_enum_type(inst.ty, ctx.pool) {
            return Self::emit_enum_assign(builder, ctx, r);
        }
        let val = Self::eval_inst(builder, ctx, view.value)?;
        // Kill AFTER evaluating the RHS: `x = x + 1` must still
        // see the old fact while its right-hand side is emitted.
        Self::kill_fact(ctx, view.name);
        let var = Self::read_slot(&ctx.locals, view.name).ok_or_else(|| {
            format!(
                "Undefined variable in assign: '{}'",
                ctx.pool.str(view.name)
            )
        })?;
        builder.def_var(var, val);
        Ok(Terminator::None)
    }

    /// Fat (`str`/`bytes`) reassignment: the producer-into-home fast
    /// paths, the consuming-concat shortcut, and the free-before-
    /// overwrite ordering rules (an RHS aliasing the binding frees
    /// after evaluation instead — freeing first would be a
    /// use-after-free). Split from `emit_assign` to stay under the
    /// workspace's small-function ratchet.
    fn emit_fat_assign(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let inst = ctx.tir.inst(r);
        let view = ctx.tir.assign_view(r);
        // Consuming reassign-concat fast path: the ownership
        // pass proved the lhs binding dies at this reassign, so
        // codegen appends in place and skips the
        // free_on_reassign free below by never reaching it.
        if let Some(concat_ref) = ctx.sidecar.consumed_concat_lhs[r.index()] {
            return Self::emit_consuming_concat_assign(builder, ctx, r, concat_ref);
        }
        // `read_slot` copies the FatLocals out (three Cranelift
        // `Variable` newtypes + home), so no table borrow
        // survives into the free declaration below, which
        // needs &mut ctx.module.
        let locals = Self::read_slot(&ctx.fat_locals, view.name).ok_or_else(|| {
            format!(
                "Undefined fat variable in assign: '{}'",
                ctx.pool.str(view.name)
            )
        })?;
        let is_bytes = matches!(ctx.pool.kind(inst.ty), TypeKind::Bytes);
        // Home-backed target whose RHS cannot reference the
        // binding: free the old buffer FIRST, then let the
        // producer write the home directly (free-before-
        // overwrite). An RHS that reads the binding (e.g. a
        // non-consuming `s = s + "x"`) takes the eval-then-
        // free-then-store order instead — freeing first
        // would be a use-after-free.
        let direct = locals.home.is_some()
            && structs::writes_out_slot_ids(ctx.tir, &ctx.name_ids, view.value)
            && !Self::expr_refs_name(ctx.tir, view.value, view.name);
        let mut old_freed = false;
        // Elision, same predicate as the scheduled-free
        // path: when the home's CURRENT contents are
        // provably free-noop (`locals.home_inline`), the
        // old-value free is a guaranteed no-op either way.
        if direct && !locals.home_inline && ctx.sidecar.free_on_reassign[r.index()].is_some() {
            let (old_ptr, old_cap) = Self::emit_fat_load_ptr_cap(builder, ctx, view.name)
                .expect("fat_locals entry read above");
            if !Self::is_static_cap_zero(builder.func, old_cap) {
                let free_ref = if is_bytes {
                    Self::declare_bytes_free(ctx, builder)?
                } else {
                    Self::declare_str_free(ctx, builder)?
                };
                builder.ins().call(free_ref, &[old_ptr, old_cap]);
            }
            old_freed = true;
        }
        let repr = Self::eval_inst_fat_slot(
            builder,
            ctx,
            view.value,
            if direct { locals.home } else { None },
        )?;
        let (ptr, len, cap) = match repr {
            ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                (ptr, len, cap)
            }
            _ => unreachable!("fat-typed assign should produce a fat ValueRepr"),
        };
        // Free the old allocation before overwriting locals.
        // sidecar.free_on_reassign[r] is set whenever the
        // ownership pass observed a Valid old owner at this
        // Assign. The old (ptr, cap) live in the binding's
        // current storage (home slot or FatLocals Variables)
        // — NOT in inst_values[old_owner], which holds the
        // literal's original (ptr, cap) at its emission point
        // and may be stale across reassigns.
        if !old_freed
            && ctx.sidecar.free_on_reassign[r.index()].is_some()
            && !(locals.home.is_some() && locals.home_inline)
        {
            let (old_ptr, old_cap) = Self::emit_fat_load_ptr_cap(builder, ctx, view.name)
                .expect("fat_locals entry read above");
            if !Self::is_static_cap_zero(builder.func, old_cap) {
                let free_ref = if is_bytes {
                    Self::declare_bytes_free(ctx, builder)?
                } else {
                    Self::declare_str_free(ctx, builder)?
                };
                builder.ins().call(free_ref, &[old_ptr, old_cap]);
            }
        }
        match locals.home {
            // In direct mode the producer already wrote the
            // home; only the aliasing fallback stores here.
            Some(home) if !direct => {
                let addr = builder.ins().stack_addr(ctx.int_type, home, 0);
                builder.ins().store(MemFlagsData::trusted(), ptr, addr, 0);
                builder.ins().store(MemFlagsData::trusted(), len, addr, 8);
                builder.ins().store(MemFlagsData::trusted(), cap, addr, 16);
            }
            Some(_) => {}
            None => {
                builder.def_var(locals.ptr, ptr);
                builder.def_var(locals.len, len);
                builder.def_var(locals.cap, cap);
            }
        }
        if locals.home.is_some() {
            let inline = Self::home_value_provably_inline(ctx, builder.func, view.value, cap);
            Self::set_home_inline(ctx, view.name, inline);
        }
        Self::kill_fact(ctx, view.name);
        Ok(Terminator::None)
    }
}
