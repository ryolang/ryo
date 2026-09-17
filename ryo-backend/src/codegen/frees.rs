//! Free/dead-drop emission and the provably-inline free elision —
//! split from `expr.rs`; see module docs in `mod.rs`.

use super::{Codegen, FunctionContext, ValueRepr, is_fat_type};
use cranelift::codegen::ir::{FuncRef, InstructionData, Opcode, ValueDef};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{ParamMode, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};

impl<M: Module> Codegen<M> {
    /// True if a `FreePoint` with the given `branch` tag is eligible
    /// to fire at the current point in codegen. Unconditional entries
    /// (`branch == None`) always pass; branch-gated entries fire only
    /// when their `BranchId` is on `branch_stack`. We use `contains`
    /// rather than `last() == Some(&b)` so a Free anchored to a
    /// parent arm still fires when codegen is inside a nested child
    /// arm of that parent.
    pub(crate) fn branch_active(
        branch: Option<ryo_core::ownership::BranchId>,
        stack: &[ryo_core::ownership::BranchId],
    ) -> bool {
        match branch {
            None => true,
            Some(b) => stack.contains(&b),
        }
    }

    /// Emit the family-appropriate free (`ryo_str_free` /
    /// `ryo_bytes_free`, selected per target via `free_target_is_bytes`)
    /// for any scheduled Free whose
    /// anchor is `tir_ref` and whose `branch` tag is active on the
    /// current `branch_stack`. Called at the end of each
    /// materialisation (`eval_inst` / `eval_inst_fat`) so that Task
    /// 4's anonymous-temporary Frees, anchored on the consuming
    /// `Call`, fire after the consumer has emitted its IR.
    ///
    /// Scheduled Frees only target `Str`-/`Bytes`-cached owners. A
    /// `Scalar`-cached target is an ownership-pass bug — the
    /// borrowed-scalar ABI never owns its argument and the ownership
    /// pass excludes such args from `temp_owners`. If a
    /// `Scalar` target is observed here, this function returns `Err`.
    ///
    /// `freed_at` (a per-`free_schedule`-index flag table) guards against
    /// double-emission across the eval-end hooks and the end-of-stmt
    /// sweep.
    pub(crate) fn emit_due_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        tir_ref: TirRef,
    ) -> Result<(), String> {
        if ctx.sidecar.free_schedule.is_empty() {
            return Ok(());
        }
        let Some(indices) = ctx.free_by_after.get(tir_ref.index()) else {
            return Ok(());
        };
        let pending: Vec<(usize, TirRef)> = indices
            .iter()
            .copied()
            .filter(|&idx| {
                let fp = &ctx.sidecar.free_schedule[idx];
                Self::branch_active(fp.branch, &ctx.branch_stack) && !ctx.freed_at[idx]
            })
            .map(|idx| (idx, ctx.sidecar.free_schedule[idx].target))
            .collect();
        Self::emit_frees(builder, ctx, pending)
    }

    /// End-of-statement sweep: fire any scheduled Free whose anchor
    /// was materialised within the just-emitted statement but hasn't
    /// been emitted yet. This covers Task 3's last-use Frees where
    /// `after` is a sub-expression `Var` read — by the time the
    /// statement finishes, the consumer has already issued its IR,
    /// so a Free here lands after the consumer's use of the buffer.
    /// Eager firing during the inner `eval_inst_fat(Var)` would have
    /// dropped the allocation before the consumer (e.g. `print`'s
    /// `write` syscall) finished reading from it.
    ///
    /// Branch-gated entries are filtered through `branch_active`, so
    /// only Frees whose `BranchId` is on the current `branch_stack`
    /// fire here.
    pub(crate) fn sweep_due_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
    ) -> Result<(), String> {
        if ctx.pending_sweep.is_empty() {
            return Ok(());
        }
        let pending: Vec<(usize, TirRef)> = ctx
            .pending_sweep
            .iter()
            .copied()
            .filter(|&idx| {
                let fp = &ctx.sidecar.free_schedule[idx];
                Self::branch_active(fp.branch, &ctx.branch_stack)
                    && Self::cached_repr(ctx, fp.after).is_some()
                    && Self::cached_repr(ctx, fp.target).is_some()
            })
            .map(|idx| (idx, ctx.sidecar.free_schedule[idx].target))
            .collect();
        Self::emit_frees(builder, ctx, pending)
    }

    /// True when `cap` is a materialized `iconst 0` — the static
    /// .rodata sentinel. `ryo_str_free` returns immediately for
    /// cap == 0, so the call is dead at the emission site and can be
    /// skipped; the ownership schedule itself stays untouched.
    pub(crate) fn is_static_cap_zero(func: &cranelift::codegen::ir::Function, cap: Value) -> bool {
        let ValueDef::Result(inst, _) = func.dfg.value_def(cap) else {
            return false;
        };
        let InstructionData::UnaryImm { opcode, imm } = &func.dfg.insts[inst] else {
            return false;
        };
        *opcode == Opcode::Iconst && imm.bits() == 0
    }

    /// Statically-known upper bound on a producer builtin's output
    /// bytes. `int_to_str(i64)` is at most 20 chars; `bool_to_str` 5.
    /// `float_to_str` is deliberately absent: ryu's f64 worst case is
    /// 24 bytes, one over `INLINE_CAP`. Mirrors the frontend registry
    /// (`builtins.rs::max_output_len`) — the backend cannot depend on
    /// the frontend, so a ryo-backend test asserts the two agree.
    fn producer_max_output_len(name: &str) -> Option<u8> {
        match name {
            "int_to_str" => Some(20),
            "bool_to_str" => Some(5),
            _ => None,
        }
    }

    /// True when `target` is a call to a producer whose output is
    /// provably always SSO-inline (max ≤ `INLINE_CAP`): the runtime
    /// tags the cap word inline, so `ryo_*_free` on the value is a
    /// guaranteed no-op and codegen may elide it (generalizing the
    /// cap==0 static-literal elision in `is_static_cap_zero`).
    pub(crate) fn provably_inline_producer(ctx: &FunctionContext<'_, M>, target: TirRef) -> bool {
        if target.is_param() {
            return false;
        }
        let inst = ctx.tir.inst(target);
        if inst.tag != TirTag::Call {
            return false;
        }
        let name = ctx.pool.str(ctx.tir.call_view(target).name);
        Self::producer_max_output_len(name)
            .is_some_and(|max| max as usize <= ryo_runtime::INLINE_CAP)
    }

    /// Per-function mutation pre-scan backing the provably-inline free
    /// elision. Returns two dense tables:
    ///
    /// * `fat_mutated`, keyed by `StringId::raw()`: bindings whose
    ///   storage can change after initialization — `Assign` targets,
    ///   `str_push`/`bytes_push` targets, fat `inout` call args, and
    ///   slice/`ToView` bases (promote-on-view writes the heap triple
    ///   back into the binding). A binding on this list may hold a heap
    ///   buffer at free time even when its initializer was provably
    ///   inline, so its frees must stay.
    /// * `view_base_insts`, keyed by `TirRef::index()`: fat-typed
    ///   instructions used as a slice/`ToView` base. Promotion swaps
    ///   such a temp's cached triple for a heap one, so its scheduled
    ///   free is real and must stay.
    pub(crate) fn build_fat_mutation_tables(
        tir: &Tir,
        pool: &InternPool,
    ) -> (Vec<bool>, Vec<bool>) {
        let mut fat_mutated = vec![false; pool.string_count()];
        let mut view_base_insts = vec![false; tir.instructions.len()];
        let mark_var = |mutated: &mut Vec<bool>, tir: &Tir, r: TirRef| {
            if let TirData::Var(n) = tir.inst(r).data {
                mutated[n.raw() as usize] = true;
            }
        };
        for i in 1..tir.instructions.len() {
            let r = TirRef::from_raw(i as u32);
            let inst = tir.inst(r);
            match inst.tag {
                TirTag::Assign => {
                    let view = tir.assign_view(r);
                    fat_mutated[view.name.raw() as usize] = true;
                }
                TirTag::Call => {
                    let view = tir.call_view(r);
                    let name = pool.str(view.name);
                    if (name == "str_push" || name == "bytes_push")
                        && let Some(&target) = view.args.first()
                    {
                        mark_var(&mut fat_mutated, tir, target);
                    }
                    for (i, &arg) in view.args.iter().enumerate() {
                        if view.modes.get(i) == Some(&ParamMode::Inout)
                            && is_fat_type(tir.inst(arg).ty, pool)
                        {
                            mark_var(&mut fat_mutated, tir, arg);
                        }
                    }
                }
                TirTag::Slice => {
                    let base = match inst.data {
                        TirData::Slice { base, .. } => base,
                        _ => unreachable!("Slice must carry TirData::Slice"),
                    };
                    if is_fat_type(tir.inst(base).ty, pool) {
                        view_base_insts[base.index()] = true;
                        mark_var(&mut fat_mutated, tir, base);
                    }
                }
                TirTag::ToView => {
                    let operand = match inst.data {
                        TirData::UnOp(o) => o,
                        _ => unreachable!("ToView must carry TirData::UnOp"),
                    };
                    if is_fat_type(tir.inst(operand).ty, pool) {
                        view_base_insts[operand.index()] = true;
                        mark_var(&mut fat_mutated, tir, operand);
                    }
                }
                _ => {}
            }
        }
        (fat_mutated, view_base_insts)
    }

    /// Shared emission body for `emit_due_frees` / `sweep_due_frees`.
    /// Given the already-filtered `(free_schedule index, target)`
    /// pairs, declare the family-appropriate free (`ryo_str_free` /
    /// `ryo_bytes_free`, selected per target via `free_target_is_bytes`)
    /// and emit one call per pair, marking each index as fired in
    /// `ctx.freed_at`. A `Scalar`-cached target
    /// (borrowed-scalar ABI, never heap-owned) returns an error and aborts
    /// code generation — the ABI registry is supposed to keep such args out
    /// of `temp_owners`.
    ///
    /// When the target is a named binding's initializer/value (or a fat
    /// param's virtual ref), the Free is emitted from the binding's
    /// CURRENT `FatLocals` instead of the producing inst's cached repr:
    /// after a reassign, a branch merge, or an `inout` write-back the
    /// cached triple may be stale (freed/replaced), while the binding's
    /// `Variable`s are SSA-correct at every program point (the
    /// same reasoning the `free_on_reassign` path documents).
    /// Lazily declare and cache the family-appropriate free `FuncRef`
    /// (`ryo_bytes_free` when `is_bytes`, else `ryo_str_free`).
    /// Resolved only at call sites that survive the cap==0 elision, so
    /// an all-static schedule never declares an unused import.
    pub(crate) fn free_ref_for(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        str_free_ref: &mut Option<FuncRef>,
        bytes_free_ref: &mut Option<FuncRef>,
        is_bytes: bool,
    ) -> Result<FuncRef, String> {
        let slot = if is_bytes {
            bytes_free_ref
        } else {
            str_free_ref
        };
        if let Some(f) = slot {
            return Ok(*f);
        }
        let f = if is_bytes {
            Self::declare_bytes_free(ctx, builder)?
        } else {
            Self::declare_str_free(ctx, builder)?
        };
        *slot = Some(f);
        Ok(f)
    }

    fn emit_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        pending: Vec<(usize, TirRef)>,
    ) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let mut str_free_ref: Option<FuncRef> = None;
        let mut bytes_free_ref: Option<FuncRef> = None;
        for (idx, target) in pending {
            ctx.freed_at[idx] = true;
            // M9: struct-typed targets route to the recursive field
            // drop; everything below is the str/bytes path.
            if Self::try_emit_struct_free(builder, ctx, target)? {
                continue;
            }
            let is_bytes = Self::free_target_is_bytes(ctx, target);
            let binding_name = Self::free_binding_name(ctx, target)
                .filter(|name| Self::read_slot(&ctx.fat_locals, *name).is_some());
            if let Some(name) = binding_name {
                // Provably-inline elision: the free fires on the
                // binding's CURRENT value, so it is sound only when the
                // initializer is a provably-inline producer AND the
                // binding is never reassigned, pushed, passed inout, or
                // view-promoted (the pre-scan marks those names).
                let elide = !ctx.fat_mutated[name.raw() as usize]
                    && Self::provably_inline_producer(ctx, target);
                if !elide {
                    let (ptr, cap) = Self::emit_fat_load_ptr_cap(builder, ctx, name)
                        .expect("fat_locals entry checked above");
                    if !Self::is_static_cap_zero(builder.func, cap) {
                        let free_ref = Self::free_ref_for(
                            builder,
                            ctx,
                            &mut str_free_ref,
                            &mut bytes_free_ref,
                            is_bytes,
                        )?;
                        builder.ins().call(free_ref, &[ptr, cap]);
                    }
                }
                continue;
            }
            let repr = Self::cached_repr(ctx, target).ok_or_else(|| {
                format!(
                    "ownership pass scheduled Free for %{} but no ValueRepr cached",
                    target.index()
                )
            })?;
            // M8.4: views are borrows, never owners — the ownership pass
            // must never schedule a Free for one. The
            // repr check below doubles as the release-mode guard.
            debug_assert!(
                !matches!(repr, ValueRepr::View { .. }),
                "ownership pass scheduled Free for strview %{}; views are never freed",
                target.index()
            );
            match repr {
                ValueRepr::Str { ptr, cap, .. } | ValueRepr::Bytes { ptr, cap, .. } => {
                    // Provably-inline elision for temporaries: sound
                    // unless the temp is a view base — promotion swaps
                    // its cached triple for a heap one, making the free
                    // real.
                    let elide = Self::provably_inline_producer(ctx, target)
                        && !ctx.view_base_insts[target.index()];
                    if !elide && !Self::is_static_cap_zero(builder.func, cap) {
                        let free_ref = Self::free_ref_for(
                            builder,
                            ctx,
                            &mut str_free_ref,
                            &mut bytes_free_ref,
                            is_bytes,
                        )?;
                        builder.ins().call(free_ref, &[ptr, cap]);
                    }
                }
                ValueRepr::View { .. } => {
                    return Err(format!(
                        "ownership pass scheduled Free for non-owning strview %{}; views are never owners",
                        target.index()
                    ));
                }
                ValueRepr::Scalar(_) => {
                    return Err(format!(
                        "ownership pass scheduled Free for borrowed-scalar value %{}; the ABI registry should have excluded it.",
                        target.index()
                    ));
                }
                ValueRepr::Struct { .. } => {
                    return Err(format!(
                        "ownership pass scheduled Free for struct %{} but try_emit_struct_free did not claim it",
                        target.index()
                    ));
                }
            }
        }
        ctx.pending_sweep.retain(|&idx| !ctx.freed_at[idx]);
        Ok(())
    }

    /// Emit conditional DeadDrops for (`if_stmt`, `arm`): frees of
    /// the pre-if buffer of a conditionally-reassigned binding on the
    /// paths where the reassign did NOT happen. Fired at the START of an
    /// untouched arm, where the binding's `FatLocals` still hold the
    /// pre-if value. Resolves `target` through `free_binding_names` (the
    /// init→name map), so the freed buffer is the binding's
    /// current triple at that program point. The free is
    /// family-appropriate (`ryo_str_free` / `ryo_bytes_free`, selected
    /// per target via `free_target_is_bytes`).
    pub(crate) fn emit_conditional_dead_drops(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        if_stmt: TirRef,
        arm: ryo_core::ownership::BranchId,
    ) -> Result<(), String> {
        for drop in ctx.sidecar.conditional_dead_drops.iter() {
            if drop.if_stmt != if_stmt || !drop.arms.contains(&arm) {
                continue;
            }
            let Some(name) = Self::free_binding_name(ctx, drop.target) else {
                continue;
            };
            // M9: struct bindings drop their needs-drop fields.
            if Self::try_emit_struct_dead_drop(builder, ctx, name, drop.target)? {
                continue;
            }
            let Some((ptr, cap)) = Self::emit_fat_load_ptr_cap(builder, ctx, name) else {
                continue;
            };
            let free_ref = if Self::free_target_is_bytes(ctx, drop.target) {
                Self::declare_bytes_free(ctx, builder)?
            } else {
                Self::declare_str_free(ctx, builder)?
            };
            builder.ins().call(free_ref, &[ptr, cap]);
        }
        Ok(())
    }

    /// Map every fat-producing named initializer to its binding: VarDecl
    /// initializers, Assign values, and fat (str/bytes) params' virtual
    /// refs. Built
    /// once per function; `emit_frees` consults it to free a binding's
    /// current `FatLocals` rather than a stale cached repr.
    ///
    /// Returns two dense tables: the first indexed by `TirRef::index()`
    /// for real instruction refs (slot 0 unused), the second indexed by
    /// param position for fat-param sentinel refs — queried together via
    /// `Codegen::free_binding_name`.
    pub(crate) fn build_free_binding_names(
        tir: &Tir,
        pool: &InternPool,
    ) -> (Vec<Option<StringId>>, Vec<Option<StringId>>) {
        fn walk(tir: &Tir, stmts: &[TirRef], map: &mut [Option<StringId>]) {
            for &r in stmts {
                match tir.inst(r).tag {
                    TirTag::VarDecl => {
                        let view = tir.var_decl_view(r);
                        map[view.initializer.index()] = Some(view.name);
                    }
                    TirTag::Assign => {
                        let view = tir.assign_view(r);
                        map[view.value.index()] = Some(view.name);
                    }
                    TirTag::IfStmt => {
                        let view = tir.if_stmt_view(r);
                        walk(tir, &view.then_stmts, map);
                        for elif in &view.elif_branches {
                            walk(tir, &elif.body, map);
                        }
                        if let Some(else_stmts) = &view.else_stmts {
                            walk(tir, else_stmts, map);
                        }
                    }
                    TirTag::WhileLoop => walk(tir, &tir.while_loop_view(r).body, map),
                    TirTag::ForRange => walk(tir, &tir.for_range_view(r).body, map),
                    _ => {}
                }
            }
        }
        let mut param_names = vec![None; tir.params.len()];
        for (idx, param) in tir.params.iter().enumerate() {
            if is_fat_type(param.ty, pool) {
                param_names[idx] = Some(param.name);
            }
        }
        let mut inst_names = vec![None; tir.instructions.len()];
        walk(tir, &tir.body_stmts(), &mut inst_names);
        (inst_names, param_names)
    }
}
