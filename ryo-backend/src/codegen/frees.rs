//! Free/dead-drop emission and the provably-inline free elision —
//! split from `expr.rs`; see module docs in `mod.rs`.

use super::{Codegen, CodegenNameIds, FunctionContext, ValueRepr, is_fat_type};
use cranelift::codegen::ir::{FuncRef, InstructionData, Opcode, ValueDef};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{ParamMode, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
use std::collections::HashMap;

/// The four tables built by [`Codegen::build_free_binding_names`]:
/// free-target → binding name (dense, per instruction ref), fat-param
/// sentinel → binding name (dense, per param position), free-target →
/// its BINDING identity (dense, per instruction ref — a declaring
/// `VarDecl`'s `TirRef`, or the `Assign` value's
/// `sidecar.assign_binding` entry), and binding identity → its most
/// recent write (see the builder's docs).
type FreeBindingTables = (
    Vec<Option<StringId>>,
    Vec<Option<StringId>>,
    Vec<Option<TirRef>>,
    HashMap<TirRef, TirRef>,
);

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
        ids: &CodegenNameIds,
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
                    // StringId equality against the ids resolved once
                    // per compilation (see `CodegenNameIds`).
                    if (ids.str_push == Some(view.name) || ids.bytes_push == Some(view.name))
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
            // Binding-path redirect: free the binding's CURRENT home
            // value instead of the producing inst's cached repr (the
            // cached triple may be stale across reassigns/merges). The
            // redirect frees the home SLOT's contents, so it must not
            // fire for a stale/superseded target (e.g. the pre-reassign
            // owner of a loop-carried binding, scheduled by a
            // last-use/exit pass against the first-wins merged owner
            // map): that would free the value the binding CURRENTLY
            // holds, double-freeing it with that value's own last-use
            // Free. The redirect fires when the target IS the binding's
            // most recent write, OR when that most recent write has no
            // Free of its own — the branch-divergent reseat shape,
            // where the merge keeps the pre-branch owner as the live
            // owner, the reseated value's Free is skipped as stale, and
            // the pre-branch owner's Free must redirect to free the
            // path-correct buffer. Any other stale target falls through
            // to the cached-repr path (or the static cap==0 elision)
            // below.
            //
            // Both the target's binding and the "most recent write"
            // table are keyed by BINDING identity (the declaring
            // VarDecl's ref; `sidecar.assign_binding` for Assign
            // values), not by name: a same-named shadow writes the same
            // name-keyed slot tables but is a different binding with its
            // own home lineage. Name-keyed lookup let a later shadow's
            // writes clobber the outer binding's "most recent write",
            // misclassifying the outer owner's Free as superseded — the
            // fallback then freed the owner's stale cached triple (an
            // invalid free on the taken path) while the slot's
            // path-correct buffer leaked.
            let binding_name = Self::free_binding_name(ctx, target)
                .filter(|name| Self::read_slot(&ctx.fat_locals, *name).is_some())
                .filter(|_| match Self::free_binding_of(ctx, target) {
                    Some(binding) => match ctx.binding_last_write.get(&binding) {
                        Some(&last) => last == target || !ctx.all_free_targets.contains(&last),
                        None => false,
                    },
                    None => false,
                });
            if let Some(name) = binding_name {
                // Provably-inline elision: the free fires on the
                // binding's CURRENT value, so it is sound only when the
                // initializer is a provably-inline producer AND the
                // binding is never reassigned, pushed, passed inout, or
                // view-promoted (the pre-scan marks those names). The
                // home-provenance flag generalizes this to reassigned
                // bindings: it tracks the value ACTUALLY in the home at
                // this program point, cleared at control-flow joins and
                // in-place mutations.
                let home_inline =
                    Self::read_slot(&ctx.fat_locals, name).is_some_and(|fl| fl.home_inline);
                let elide = home_inline
                    || (!ctx.fat_mutated[name.raw() as usize]
                        && Self::provably_inline_producer(ctx, target));
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
    ///
    /// Consults `dead_drop_by_if` (built once per function) rather than
    /// re-scanning the whole per-function drop list at the start of
    /// every arm — on if-heavy functions with dead drops the old scan
    /// was O(ifs × arms × drops).
    pub(crate) fn emit_conditional_dead_drops(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        if_stmt: TirRef,
        arm: ryo_core::ownership::BranchId,
    ) -> Result<(), String> {
        if ctx.sidecar.conditional_dead_drops.is_empty() {
            return Ok(());
        }
        let pending: Vec<TirRef> = ctx
            .dead_drop_by_if
            .get(if_stmt.index())
            .map(|indices| {
                indices
                    .iter()
                    .filter(|&&i| ctx.sidecar.conditional_dead_drops[i].arms.contains(&arm))
                    .map(|&i| ctx.sidecar.conditional_dead_drops[i].target)
                    .collect()
            })
            .unwrap_or_default();
        for target in pending {
            let Some(name) = Self::free_binding_name(ctx, target) else {
                continue;
            };
            // M9: struct bindings drop their needs-drop fields.
            if Self::try_emit_struct_dead_drop(builder, ctx, name, target)? {
                continue;
            }
            let Some((ptr, cap)) = Self::emit_fat_load_ptr_cap(builder, ctx, name) else {
                continue;
            };
            let free_ref = if Self::free_target_is_bytes(ctx, target) {
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
    /// Returns four tables (see [`FreeBindingTables`]): the first and
    /// third are dense — indexed by `TirRef::index()` for real
    /// instruction refs (slot 0 unused), mapping a free target to its
    /// binding NAME and to its BINDING identity respectively (a VarDecl
    /// initializer's binding is its own `VarDecl` ref; an Assign value's
    /// binding is the `sidecar.assign_binding` entry the ownership walk
    /// stamped). The second is dense by param position for fat-param
    /// sentinel refs. The fourth, `last_write`, is keyed by BINDING
    /// identity (not name — one name can denote a same-named shadow) and
    /// records the `TirRef` of the binding's most recent write in
    /// program order (the `VarDecl` initializer, overwritten by each
    /// `Assign` value; fat params start at their param sentinel ref,
    /// whose binding identity is the sentinel itself). `emit_frees'`
    /// binding-path redirect frees the binding's CURRENT home value,
    /// which is only the FreePoint's buffer when the FreePoint's target
    /// IS that most recent write — a stale/superseded target (e.g. a
    /// pre-reassign owner of a loop-carried binding) would free whatever
    /// value the binding currently holds, so the redirect must not fire
    /// for it.
    pub(crate) fn build_free_binding_names(
        tir: &Tir,
        pool: &InternPool,
        sidecar: &ryo_core::ownership::FunctionSidecar,
    ) -> FreeBindingTables {
        fn walk(
            tir: &Tir,
            sidecar: &ryo_core::ownership::FunctionSidecar,
            stmts: &[TirRef],
            map: &mut [Option<StringId>],
            binding_of: &mut [Option<TirRef>],
            last_write: &mut HashMap<TirRef, TirRef>,
        ) {
            for &r in stmts {
                match tir.inst(r).tag {
                    TirTag::VarDecl => {
                        let view = tir.var_decl_view(r);
                        map[view.initializer.index()] = Some(view.name);
                        // A VarDecl always declares a fresh binding
                        // (possibly shadowing an outer same-named one),
                        // so the binding identity is the VarDecl itself.
                        binding_of[view.initializer.index()] = Some(r);
                        last_write.insert(r, view.initializer);
                    }
                    TirTag::Assign => {
                        let view = tir.assign_view(r);
                        map[view.value.index()] = Some(view.name);
                        // The binding this Assign stores into, recorded
                        // by the ownership walk; missing only for
                        // sema-rejected programs.
                        if let Some(binding) = sidecar.assign_binding[r.index()] {
                            binding_of[view.value.index()] = Some(binding);
                            last_write.insert(binding, view.value);
                        }
                    }
                    TirTag::IfStmt => {
                        let view = tir.if_stmt_view(r);
                        walk(tir, sidecar, &view.then_stmts, map, binding_of, last_write);
                        for elif in &view.elif_branches {
                            walk(tir, sidecar, &elif.body, map, binding_of, last_write);
                        }
                        if let Some(else_stmts) = &view.else_stmts {
                            walk(tir, sidecar, else_stmts, map, binding_of, last_write);
                        }
                    }
                    TirTag::WhileLoop => walk(
                        tir,
                        sidecar,
                        &tir.while_loop_view(r).body,
                        map,
                        binding_of,
                        last_write,
                    ),
                    TirTag::ForRange => walk(
                        tir,
                        sidecar,
                        &tir.for_range_view(r).body,
                        map,
                        binding_of,
                        last_write,
                    ),
                    TirTag::Destructure => {
                        // M10: each bound field's owner token (the
                        // FieldAccess insts emitted immediately before
                        // the Destructure) maps to its binding name, so
                        // a scheduled Free lowers through the binding's
                        // slot instead of the never-evaluated token.
                        // The binding identity is the owner token
                        // itself: destructure bindings are fresh, never
                        // reassigned, and cannot collide with an
                        // in-scope name (sema rejects), so per-binding
                        // keying leaves same-named outer bindings'
                        // lineages untouched.
                        let view = tir.destructure_view(r);
                        let owners = tir.destructure_bound_owner_refs(r);
                        let mut owner_idx = 0;
                        for field in &view.fields {
                            let Some(bind) = field.bind else { continue };
                            let owner = owners[owner_idx];
                            owner_idx += 1;
                            map[owner.index()] = Some(bind);
                            binding_of[owner.index()] = Some(owner);
                            last_write.insert(owner, owner);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut last_write: HashMap<TirRef, TirRef> = HashMap::new();
        let mut param_names = vec![None; tir.params.len()];
        for (idx, param) in tir.params.iter().enumerate() {
            if is_fat_type(param.ty, pool) {
                // A fat param's binding identity is its own sentinel ref
                // (params are bound at entry and never shadowed).
                param_names[idx] = Some(param.name);
                last_write.insert(TirRef::param(idx), TirRef::param(idx));
            }
        }
        let mut inst_names = vec![None; tir.instructions.len()];
        let mut inst_bindings = vec![None; tir.instructions.len()];
        walk(
            tir,
            sidecar,
            &tir.body_stmts(),
            &mut inst_names,
            &mut inst_bindings,
            &mut last_write,
        );
        (inst_names, param_names, inst_bindings, last_write)
    }
}
