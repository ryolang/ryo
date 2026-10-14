//! Free/dead-drop emission and the provably-inline free elision —
//! split from `expr.rs`; see module docs in `mod.rs`. Also home to the
//! free-routing helpers: the binding-lineage accessors
//! (`free_binding_name` / `free_binding_of`), the aliasing probe
//! (`expr_refs_name`), and the fat-home provenance flags
//! (`set_home_inline` and friends).

use super::{BlockPrologue, Codegen, CodegenNameIds, FunctionContext, ValueRepr, is_fat_type};
use cranelift::codegen::ir::{FuncRef, InstructionData, Opcode, ValueDef};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{ParamMode, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};
use std::collections::HashMap;

/// The four tables built by [`Codegen::build_free_binding_names`]:
/// free-target → binding name (dense, per instruction ref), fat-param
/// sentinel → binding name (dense, per param position), free-target →
/// its BINDING identity (dense, per instruction ref — the written Flow
/// place's declaring `VarDecl` ref or owner-token inst), and binding
/// identity → its most recent write (see the builder's docs).
type FreeBindingTables = (
    Vec<Option<StringId>>,
    Vec<Option<StringId>>,
    Vec<Option<TirRef>>,
    HashMap<TirRef, TirRef>,
);

impl<M: Module> Codegen<M> {
    /// position-mechanical mechanical firing: emit the drops in one fire-map bucket.
    /// Each scheduled drop lives in exactly one bucket (built once per
    /// function from its Flow position), so every drop fires exactly
    /// once — arm/branch conditionality is the anchor step's block
    /// membership, never a cached-value or sweep predicate. Entries the
    /// lowering-time pre-filter marked consumed are skipped.
    pub(crate) fn fire_bucket(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        indices: &[usize],
    ) -> Result<(), String> {
        if indices.is_empty() {
            return Ok(());
        }
        // Consume (mark) every bucket entry, emitting frees only for
        // targets not already destroyed: the ownership pass's
        // double-anchors (consumer + return epilogue) make one
        // same-target duplicate a no-op by design.
        let mut pending: Vec<(usize, TirRef)> = Vec::new();
        for &idx in indices {
            if ctx.freed_at[idx] {
                continue;
            }
            ctx.freed_at[idx] = true;
            #[cfg(debug_assertions)]
            {
                ctx.fire_marked[idx] = true;
            }
            let target = ctx.sidecar.drops[idx].target;
            if !ctx.freed_targets.contains(&target) {
                pending.push((idx, target));
            }
        }
        Self::emit_frees(builder, ctx, pending)
    }

    /// Fire `BeforeStep{inst}` drops: the block driver calls this
    /// immediately before the step emits.
    pub(crate) fn fire_before_step(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        inst: TirRef,
    ) -> Result<(), String> {
        let indices = ctx
            .fire_before_step
            .get(inst.index())
            .cloned()
            .unwrap_or_default();
        Self::fire_bucket(builder, ctx, &indices)
    }

    /// Fire the `AfterStep` drops of the statement rooted at `inst`
    /// (the post-statement hook). Value-step anchors resolve here
    /// through their enclosing statement, so the free lands after the
    /// statement's consumer.
    pub(crate) fn fire_after_statement(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        inst: TirRef,
    ) -> Result<(), String> {
        let indices = ctx
            .fire_after_stmt
            .get(inst.index())
            .cloned()
            .unwrap_or_default();
        Self::fire_bucket(builder, ctx, &indices)
    }

    /// Fire `AfterStep` drops anchored on a terminator payload
    /// (an if/loop/match/return/break/continue instruction). The
    /// construct's handler is the instruction's single emit position —
    /// the join/exit prologue, the terminator arm — so this is the
    /// mechanical fire point; no construct path may skip it.
    pub(crate) fn fire_payload(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        inst: TirRef,
    ) -> Result<(), String> {
        let indices = ctx
            .fire_by_payload
            .get(inst.index())
            .cloned()
            .unwrap_or_default();
        Self::fire_bucket(builder, ctx, &indices)
    }

    /// Fire the block's at-terminator bucket (`PreTerminator` drops and
    /// `AfterStep` drops whose anchor has no enclosing statement — a
    /// match-arm body expression, a deferred return operand). Called by
    /// the terminator handler at its canonical point: after the
    /// operand/body evaluation, before the jump/return.
    pub(crate) fn fire_at_terminator(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        block: ryo_core::flow::BlockId,
    ) -> Result<(), String> {
        let indices = ctx.fire_at_term.get(&block).cloned().unwrap_or_default();
        Self::fire_bucket(builder, ctx, &indices)
    }

    /// position-mechanical lowering-time provably-inline pre-filter: mark scheduled
    /// drops consumed when the runtime free is a guaranteed no-op, so
    /// the emission paths never re-derive the predicate. This is a
    /// lowering-time no-op-free decision, NOT an ownership decision —
    /// the ownership schedule (`sidecar.drops`) stays untouched and the
    /// `freed_at` mark plays the role the static-cap-zero skip always
    /// did: the call would be dead at the emission site.
    ///
    /// Two static families, mirroring the two emission paths they
    /// replace:
    /// * static `.rodata` literals (`StrConst`/`BytesConst` targets) —
    ///   cap == 0 is the static sentinel, so `ryo_*_free` returns
    ///   immediately (this subsumes the old `is_static_cap_zero`
    ///   probe for literal producers);
    /// * provably-inline producers (runtime builtins whose output never
    ///   exceeds `INLINE_CAP`; mirrored from `builtins::max_output_len`
    ///   — the backend agreement test pins the two), elided on the
    ///   binding path when the binding is never mutated and on the
    ///   temp path when the producer is not a view base (promotion
    ///   would swap the cached triple for a heap one).
    ///
    /// The home-provenance elision (`home_inline`) stays at emission:
    /// it is genuine codegen state (the value actually in the home at
    /// the fire point), not a lowering-time fact.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn pre_elide_drops(
        tir: &Tir,
        pool: &InternPool,
        fat_mutated: &[bool],
        view_base_insts: &[bool],
        free_binding_names: &[Option<StringId>],
        free_binding_param_names: &[Option<StringId>],
        free_binding_of_insts: &[Option<TirRef>],
        binding_last_write: &HashMap<TirRef, TirRef>,
        func_sidecar: &ryo_core::ownership::FunctionSidecar,
        freed_at: &mut [bool],
    ) {
        let all_free_targets: std::collections::HashSet<TirRef> =
            func_sidecar.drops.iter().map(|d| d.target).collect();
        for (idx, drop) in func_sidecar.drops.iter().enumerate() {
            if freed_at[idx] {
                continue;
            }
            if matches!(
                drop.position,
                ryo_core::flow::DropPosition::BlockStart { .. }
            ) {
                // Block-start drops fire through their own emission
                // paths (`emit_dead_drop` / `emit_arm_start_drops`),
                // which have NO static elision and no redirect-lineage
                // gate — the pre-filter's predicates (built for the
                // `emit_frees` path) do not apply to them.
                continue;
            }
            let target = drop.target;
            let static_literal = !target.is_param()
                && matches!(tir.inst(target).tag, TirTag::StrConst | TirTag::BytesConst);
            // Provably-inline producer predicate (mirrors
            // `provably_inline_producer`; the producer set is pinned
            // against the frontend registry by the backend test).
            let inline_producer = !target.is_param()
                && tir.inst(target).tag == TirTag::Call
                && Self::producer_max_output_len(pool.str(tir.call_view(target).name))
                    .is_some_and(|max| max as usize <= ryo_runtime::INLINE_CAP);
            // Mirror `emit_frees`' two branches exactly (the redirect's
            // applicability is a lowering-time lineage fact), so a mark
            // here always agrees with the branch the emission path
            // would take.
            let binding_name = Self::free_binding_name_tables(
                free_binding_names,
                free_binding_param_names,
                target,
            )
            .filter(|_| {
                match Self::free_binding_of_tables(free_binding_of_insts, target) {
                    Some(binding) => match binding_last_write.get(&binding) {
                        Some(&last) => last == target || !all_free_targets.contains(&last),
                        None => false,
                    },
                    None => false,
                }
            });
            match binding_name {
                Some(name) => {
                    // Binding-redirect path: the free releases the
                    // binding's CURRENT home contents, so NO
                    // static-literal elision is sound — in-place
                    // mutation (push, ensure-heap promotion) may have
                    // replaced the literal's inline buffer with a heap
                    // one. The old runtime check skipped these via
                    // `is_static_cap_zero` on the LOADED home cap, which
                    // stays at emission. Only the provably-inline
                    // producer elision is lowering-time derivable here,
                    // and only for never-mutated bindings.
                    if !fat_mutated[name.raw() as usize] && inline_producer {
                        freed_at[idx] = true;
                    }
                }
                None => {
                    // Temp path: the cached triple is the producer's
                    // own, never mutated in place — a static .rodata
                    // literal or a provably-inline producer temp cannot
                    // own a heap buffer.
                    if static_literal || (inline_producer && !view_base_insts[target.index()]) {
                        freed_at[idx] = true;
                    }
                }
            }
        }
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
    pub(crate) fn producer_max_output_len(name: &str) -> Option<u8> {
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
    /// cap==0 static-literal elision in `is_static_cap_zero`). This
    /// infers a property of the produced VALUE (its runtime SSO
    /// inline-ability, mirrored from the frontend's
    /// `builtins::max_output_len`) — not a Flow IR or ownership-pass
    /// fact.
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

    /// Shared emission body for the fire-map buckets (position-mechanical): each bucket
    /// is one Flow fire position, so a drop is emitted at most once —
    /// the `freed_at` mark guards the lowering-time pre-filter against
    /// the fire paths and backs the all-consumed debug assertion.
    /// Given the already-filtered `(drops index, target)` pairs, declare
    /// the family-appropriate free (`ryo_str_free` / `ryo_bytes_free`,
    /// selected per target via `free_target_is_bytes`) and emit one call
    /// per pair, marking each index as fired in `ctx.freed_at`. A
    /// `Scalar`-cached target (borrowed-scalar ABI, never heap-owned)
    /// returns an error and aborts code generation — the ABI registry is
    /// supposed to keep such args out of `temp_owners`.
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
            // M11: enum-typed targets route to the tag-dispatched
            // payload drop; M9 structs to the recursive field drop;
            // everything below is the str/bytes path.
            if Self::try_emit_enum_free(builder, ctx, target)? {
                ctx.freed_targets.insert(target);
                continue;
            }
            if Self::try_emit_struct_free(builder, ctx, target)? {
                ctx.freed_targets.insert(target);
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
            // table are keyed by BINDING identity (the written Flow
            // place's declaring `VarDecl` ref — one identity for both
            // initializer and `Assign` value writes), not by name: a
            // same-named shadow writes the same
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
                // Home-provenance elision: fire only when the value
                // ACTUALLY in the home at this program point is
                // provably free-noop (the flag tracks it through
                // reassigns and is cleared at control-flow joins and
                // in-place mutations). This is genuine codegen state —
                // the home's runtime contents — not a lowering-time
                // fact, so it stays at emission while the static
                // provably-inline predicates moved to the lowering-time
                // pre-filter.
                let elide = Self::read_slot(&ctx.fat_locals, name).is_some_and(|fl| fl.home_inline);
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
                        ctx.freed_targets.insert(target);
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
            // position-mechanical: a scheduled drop whose target has no storage — the
            // producer was never emitted and the lowering-time
            // pre-filter did not elide it — is a compiler bug, not a
            // silently-skippable case: the free would leak. The cached
            // lookup errors out loudly above (debug builds assert in
            // `assert_no_dropped_frees`).
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
                    if !Self::is_static_cap_zero(builder.func, cap) {
                        let free_ref = Self::free_ref_for(
                            builder,
                            ctx,
                            &mut str_free_ref,
                            &mut bytes_free_ref,
                            is_bytes,
                        )?;
                        builder.ins().call(free_ref, &[ptr, cap]);
                        ctx.freed_targets.insert(target);
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
                ValueRepr::Enum { .. } => {
                    return Err(format!(
                        "ownership pass scheduled Free for enum %{} but try_emit_enum_free did not claim it",
                        target.index()
                    ));
                }
            }
        }
        Ok(())
    }

    /// Fire the block-start drop bucket (position-mechanical unification):
    /// the driver calls this once per Flow block, after switching into
    /// the mapped Cranelift block and BEFORE the block's steps. Two
    /// kinds share the bucket, told apart by target identity:
    ///
    /// * Match-arm-start payload destruction — the block is an arm of
    ///   a match and the drop's target is that match's scrutinee. The
    ///   consuming arm's unbound needs-drop payload fields are destroyed
    ///   (the same `unbound_payload_fields` derivation the frontend
    ///   registered from); the tag dispatch guarantees the
    ///   discriminated variant, so the field address is the scrutinee
    ///   slot plus the pool-computed absolute offset.
    /// * Conditional dead drops — the target is a pre-branch owner;
    ///   resolved through `free_binding_names` (the init→name map), so
    ///   the freed buffer is the binding's current triple at that
    ///   program point, family-appropriate (enum tag-dispatched /
    ///   struct recursive / fat triple — the exact emission the old
    ///   block-start hook performed).
    ///
    /// Consumption is marked for every entry (the backend validators debug assertions
    /// read the split), and the per-path same-target dedup applies.
    pub(crate) fn fire_block_start(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        block: ryo_core::flow::BlockId,
    ) -> Result<(), String> {
        let empty = Vec::new();
        let indices = ctx
            .fire_at_block_start
            .get(&block)
            .cloned()
            .unwrap_or_else(|| empty.to_vec());
        if indices.is_empty() {
            return Ok(());
        }
        // Match-arm-start drops resolve their fields through the owning
        // match (target == scrutinee); group them by that match so each
        // drop emits exactly its arm's field set.
        for idx in indices {
            if ctx.freed_at[idx] {
                continue;
            }
            ctx.freed_at[idx] = true;
            #[cfg(debug_assertions)]
            {
                ctx.fire_marked[idx] = true;
            }
            let target = ctx.sidecar.drops[idx].target;
            if let Some((match_inst, arm_idx)) = Self::block_start_arm_drop(ctx, block, target) {
                if ctx.freed_targets.contains(&target) {
                    continue;
                }
                Self::emit_arm_start_drops(builder, ctx, match_inst, arm_idx)?;
                ctx.freed_targets.insert(target);
            } else {
                if ctx.freed_targets.contains(&target) {
                    continue;
                }
                Self::emit_dead_drop(builder, ctx, target)?;
                ctx.freed_targets.insert(target);
            }
        }
        Ok(())
    }

    /// If `block` is the arm block of a match whose scrutinee is
    /// `target`, return that (match, arm) — the drop is arm-start
    /// payload destruction. Conditional dead drops never alias a match
    /// scrutinee (their targets are pre-branch owner insts, and an
    /// arm-start block is not a reseat arm), so the identity test is
    /// exact.
    fn block_start_arm_drop(
        ctx: &FunctionContext<'_, M>,
        block: ryo_core::flow::BlockId,
        target: TirRef,
    ) -> Option<(TirRef, usize)> {
        let BlockPrologue::MatchArm { match_inst, arm } = ctx.block_prologue[block.0 as usize]
        else {
            return None;
        };
        let view = ctx.tir.match_view(match_inst);
        (view.scrutinee == target).then_some((match_inst, arm))
    }

    /// The conditional-dead-drop emission: resolve the pre-branch
    /// owner through the binding tables and free the binding's CURRENT
    /// storage (enum bindings drop the active variant's needs-drop
    /// fields tag-dispatched; struct bindings recursively; fat bindings
    /// free the home triple — `ryo_str_free` / `ryo_bytes_free`,
    /// selected per target via `free_target_is_bytes`). Mirrors the old
    /// `emit_block_start_drops` body exactly.
    fn emit_dead_drop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        target: TirRef,
    ) -> Result<(), String> {
        let Some(name) = Self::free_binding_name(ctx, target) else {
            return Ok(());
        };
        // M11: enum bindings drop the active variant's needs-drop
        // fields (tag-dispatched).
        if Self::try_emit_enum_dead_drop(builder, ctx, name, target)? {
            return Ok(());
        }
        // M9: struct bindings drop their needs-drop fields.
        if Self::try_emit_struct_dead_drop(builder, ctx, name, target)? {
            return Ok(());
        }
        let Some((ptr, cap)) = Self::emit_fat_load_ptr_cap(builder, ctx, name) else {
            return Ok(());
        };
        let free_ref = if Self::free_target_is_bytes(ctx, target) {
            Self::declare_bytes_free(ctx, builder)?
        } else {
            Self::declare_str_free(ctx, builder)?
        };
        builder.ins().call(free_ref, &[ptr, cap]);
        Ok(())
    }

    /// Build the free-routing tables (see [`FreeBindingTables`]) from
    /// the Flow IR's place table — one pass over the Flow IR's steps, no
    /// recursive TIR walk. Every storage-replacing write (a `VarDecl`
    /// initializer or an `Assign`
    /// value, anywhere in the Flow IR — including match arms, nested
    /// scopes, and dead blocks, which the Flow IR's coverage invariant
    /// enumerates exactly once) registers its producer inst as a free
    /// target of the written place:
    ///
    /// * names come from the write instruction (as before);
    /// * binding identity comes from the [`Place`] (the declaring
    ///   `VarDecl` ref, the param sentinel, or the match/destructure
    ///   owner token) — never from a name lookup, so same-named
    ///   shadows stay distinct;
    /// * the per-binding most-recent write is the last registering
    ///   step in emission order.
    ///
    /// This replaces the previous backend-side recursive walk (with
    /// its statement/value-position match special-casing and its
    /// dependency on `sidecar.assign_binding`): the Flow IR carries the
    /// same facts structurally.
    pub(crate) fn build_free_binding_names(
        tir: &Tir,
        flow: &ryo_core::flow::Flow,
        pool: &InternPool,
    ) -> FreeBindingTables {
        fn place_identity(flow: &ryo_core::flow::Flow, place: ryo_core::flow::PlaceId) -> TirRef {
            match flow.place(place) {
                ryo_core::flow::Place::Param { index, .. } => TirRef::param(*index as usize),
                ryo_core::flow::Place::Local { decl, .. } => *decl,
                ryo_core::flow::Place::Token { inst, .. } => *inst,
                other => unreachable!("write to a non-binding place: {other:?}"),
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
        for step in &flow.steps {
            let Some(write) = step.facts.write else {
                continue;
            };
            // In-place mutations (compound assign, push, field writes)
            // preserve storage identity — they register nothing, as
            // before.
            let (name, value) = match (tir.inst(step.inst).tag, write.kind) {
                (TirTag::VarDecl, ryo_core::flow::WriteKind::Init) => {
                    let view = tir.var_decl_view(step.inst);
                    (view.name, view.initializer)
                }
                (TirTag::Assign, ryo_core::flow::WriteKind::Replace) => {
                    let view = tir.assign_view(step.inst);
                    (view.name, view.value)
                }
                _ => continue,
            };
            let identity = place_identity(flow, write.place);
            inst_names[value.index()] = Some(name);
            inst_bindings[value.index()] = Some(identity);
            last_write.insert(identity, value);
            // G2 (place-keyed ownership): a free may target the
            // declaring `VarDecl` itself — the seat of a binding whose
            // owner is the binding's home place (a moved-through-`Var`
            // value). The decl names its binding exactly like an init
            // value does, so the binding-path redirect resolves it.
            if tir.inst(step.inst).tag == TirTag::VarDecl {
                inst_names[step.inst.index()] = Some(name);
                inst_bindings[step.inst.index()] = Some(identity);
            }
        }
        (inst_names, param_names, inst_bindings, last_write)
    }
}

impl<M: Module> Codegen<M> {
    /// Backend lowering validators — debug-only invariants of the
    /// Flow→Cranelift lowering, run once per function after emission
    /// (release builds compile them out entirely; the few Vecs in the
    /// failure messages are built only on the assertion path):
    /// (1) drop consumption — every scheduled drop consumed exactly
    ///     once (the fire maps fully drained);
    /// (2) block coverage — every REACHABLE Flow block was emitted
    ///     exactly once with its terminator dispatched (unreachable
    ///     blocks are visited at most once — the driver traps/emits
    ///     them deliberately, never drops them by accident);
    /// (3) place resolution — every consumed drop's target resolved to
    ///     storage (a binding-table entry or a cached repr), with the
    ///     lowering-time pre-filter the only legitimate non-resolver.
    /// Backend lowering validators — debug-only invariants of the
    /// Flow→Cranelift lowering, run once per function after emission.
    /// Release builds compile the bodies out entirely (the failure
    /// Vecs and the visit-state reads exist only under
    /// `debug_assertions`).
    pub(crate) fn assert_backend_invariants(ctx: &FunctionContext<'_, M>) {
        #[cfg(debug_assertions)]
        {
            Self::assert_no_dropped_frees(ctx);
            Self::assert_no_dropped_promo_frees(ctx);
            Self::assert_block_coverage(ctx);
            Self::assert_drop_place_resolution(ctx);
        }
        #[cfg(not(debug_assertions))]
        let _ = ctx;
    }

    /// position-mechanical/promo-consumption check (Gap 5): a
    /// promotion whose ALLOC ran (the promote-on-view path recorded
    /// the base) must have at least one FIRED release of that base.
    /// An unfired promo release is legitimate when its anchor hook
    /// never ran — the alloc is flag-conditional on the same path, so
    /// nothing was promoted to leak. A fired alloc with no fired
    /// release leaks the promoted buffer — a compiler bug.
    #[cfg(debug_assertions)]
    fn assert_no_dropped_promo_frees(ctx: &FunctionContext<'_, M>) {
        let alloced: std::collections::HashSet<TirRef> =
            ctx.promo_alloc_fired.iter().copied().collect();
        let mut released: std::collections::HashSet<TirRef> = std::collections::HashSet::new();
        for (idx, pf) in ctx.sidecar.promotion_frees.iter().enumerate() {
            if ctx.promo_freed_at[idx] {
                released.insert(pf.base);
            }
        }
        let leaked: Vec<TirRef> = alloced.difference(&released).copied().collect();
        debug_assert!(
            leaked.is_empty(),
            "promoted bases with no fired promotion free (the promoted buffer leaks): {:?}",
            leaked.iter().map(|b| b.index()).collect::<Vec<_>>()
        );
    }

    /// position-mechanical/Drop-consumption check: every scheduled drop was consumed exactly once —
    /// fired at its Flow fire position or marked consumed by the
    /// lowering-time pre-filter. A drop left unconsumed means its
    /// fire-map bucket never ran (a construct path skipped its
    /// mechanical fire point) — a compiler bug, since the bucket exists
    /// by construction and each fires once. The assertion states the
    /// unconsumed count and the first few offending positions.
    #[cfg(debug_assertions)]
    fn assert_no_dropped_frees(ctx: &FunctionContext<'_, M>) {
        {
            let unconsumed: Vec<usize> = ctx
                .freed_at
                .iter()
                .enumerate()
                .filter(|&(_, &consumed)| !consumed)
                .map(|(idx, _)| idx)
                .collect();
            debug_assert!(
                unconsumed.is_empty(),
                "{} scheduled drops never consumed (fire-map bucket never fired); first few (idx, target, anchor): {:?}",
                unconsumed.len(),
                unconsumed
                    .iter()
                    .take(5)
                    .map(|&idx| {
                        (
                            idx,
                            ctx.sidecar.drops[idx].target.index(),
                            ctx.sidecar.drops[idx]
                                .position
                                .anchor_inst()
                                .map(|a| a.index()),
                        )
                    })
                    .collect::<Vec<_>>()
            );
        }
    }

    /// Place-resolution check. Every consumed drop's target must
    /// resolve to storage — a free-binding-table entry (the binding
    /// redirect) or a cached repr (the temp path). The sole legitimate
    /// non-resolver is the lowering-time pre-filter: a free elided as
    /// a guaranteed runtime no-op never needed storage. Targets that
    /// resolve through neither leg (and match no pre-filter predicate)
    /// reached the no-storage error path or were silently skipped —
    /// both compiler bugs. The mutation tables are recomputed here
    /// (debug-only; not stored on the context since position-mechanical).
    /// Place-resolution check, over BOTH consumption paths. The
    /// lowering-time pre-filter consumes drops WITHOUT storage (the
    /// free is a guaranteed no-op); the fire-map buckets consume drops
    /// THROUGH storage. Every consumed drop must pass the leg matching
    /// how it was consumed:
    /// * pre-elided (`freed_at` without `fire_marked`) — the elision
    ///   predicate must hold, re-derived branch-aware exactly as
    ///   `pre_elide_drops` does. The branch distinction is the
    ///   load-bearing part: the binding-redirect path frees the
    ///   binding's CURRENT home contents, so a STATIC LITERAL target
    ///   is never elidable there (in-place mutation — push, ensure-heap
    ///   promotion — may have replaced the literal's inline buffer with
    ///   a heap one; the runtime cap==0 check on the loaded home stays
    ///   at emission). Eliding a literal-binding drop there silently
    ///   leaked exactly the Linux LSan suite's push/promotion buffers.
    /// * fired (marked at a fire point) — the target must resolve to
    ///   storage in the end-of-function tables (binding entry or
    ///   cached repr); the fire itself needed it.
    #[cfg(debug_assertions)]
    fn assert_drop_place_resolution(ctx: &FunctionContext<'_, M>) {
        {
            let (fat_mutated, view_base_insts) =
                Self::build_fat_mutation_tables(ctx.tir, ctx.pool, &ctx.name_ids);
            let all_free_targets: std::collections::HashSet<TirRef> =
                ctx.sidecar.drops.iter().map(|d| d.target).collect();
            // Branch-aware elision predicate, mirroring `pre_elide_drops`.
            let predicate_holds = |target: TirRef| {
                let static_literal = !target.is_param()
                    && matches!(
                        ctx.tir.inst(target).tag,
                        ryo_core::tir::TirTag::StrConst | ryo_core::tir::TirTag::BytesConst
                    );
                let inline_producer = !target.is_param()
                    && ctx.tir.inst(target).tag == ryo_core::tir::TirTag::Call
                    && Self::producer_max_output_len(ctx.pool.str(ctx.tir.call_view(target).name))
                        .is_some_and(|max| max as usize <= ryo_runtime::INLINE_CAP);
                let redirect = Self::free_binding_name(ctx, target).filter(|_| {
                    match Self::free_binding_of(ctx, target) {
                        Some(binding) => match ctx.binding_last_write.get(&binding) {
                            Some(&last) => last == target || !all_free_targets.contains(&last),
                            None => false,
                        },
                        None => false,
                    }
                });
                match redirect {
                    Some(name) => inline_producer && !fat_mutated[name.raw() as usize],
                    None => static_literal || (inline_producer && !view_base_insts[target.index()]),
                }
            };
            let mut bad_elision: Vec<(usize, usize)> = Vec::new();
            let mut bad_resolution: Vec<(usize, usize)> = Vec::new();
            for (idx, drop) in ctx.sidecar.drops.iter().enumerate() {
                if !ctx.freed_at[idx] {
                    continue; // the consumption assert reports it
                }
                let target = drop.target;
                if ctx.fire_marked[idx] {
                    let resolved = Self::free_binding_name(ctx, target).is_some()
                        || Self::cached_repr(ctx, target).is_some()
                        || matches!(
                            drop.position,
                            ryo_core::flow::DropPosition::BlockStart { .. }
                        );
                    if !resolved {
                        bad_resolution.push((idx, target.index()));
                    }
                } else if !predicate_holds(target) {
                    bad_elision.push((idx, target.index()));
                }
            }
            debug_assert!(
                bad_elision.is_empty(),
                "pre-elided drops outside the elision predicate (idx, target): {:?}",
                bad_elision
            );
            debug_assert!(
                bad_resolution.is_empty(),
                "fired drops whose target resolved to no storage (idx, target): {:?}",
                bad_resolution
            );
        }
    }

    /// Read the free-target → binding-name map. Param sentinel refs
    /// are served from `free_binding_param_names`; real instruction
    /// refs from `free_binding_names`. Same dispatch shape as
    /// `cached_repr`.
    pub(crate) fn free_binding_name(ctx: &FunctionContext<'_, M>, r: TirRef) -> Option<StringId> {
        Self::free_binding_name_tables(&ctx.free_binding_names, &ctx.free_binding_param_names, r)
    }

    /// Table form of [`Self::free_binding_name`] — usable before the
    /// `FunctionContext` exists (the lowering-time pre-filter).
    fn free_binding_name_tables(
        free_binding_names: &[Option<StringId>],
        free_binding_param_names: &[Option<StringId>],
        r: TirRef,
    ) -> Option<StringId> {
        if let Some(idx) = r.as_param_index() {
            free_binding_param_names[idx as usize]
        } else {
            free_binding_names.get(r.index()).copied().flatten()
        }
    }

    /// Read the free-target → BINDING identity map: the written Flow
    /// place's declaring `VarDecl`'s `TirRef` for both initializer and
    /// `Assign` value targets (both write the same `Local` place), or
    /// the owner-token inst for by-move payload bindings. A fat param's
    /// binding identity IS its own sentinel ref (params are never
    /// shadowed). Same dispatch shape as `free_binding_name`;
    /// `emit_frees` consults it so the redirect's "most recent write"
    /// lookup is per-binding, not per-name (a same-named shadow is a
    /// different binding and must not clobber the outer binding's
    /// lineage).
    pub(crate) fn free_binding_of(ctx: &FunctionContext<'_, M>, r: TirRef) -> Option<TirRef> {
        Self::free_binding_of_tables(&ctx.free_binding_of_insts, r)
    }

    /// Table form of [`Self::free_binding_of`] — usable before the
    /// `FunctionContext` exists (the lowering-time pre-filter).
    fn free_binding_of_tables(
        free_binding_of_insts: &[Option<TirRef>],
        r: TirRef,
    ) -> Option<TirRef> {
        if r.as_param_index().is_some() {
            Some(r)
        } else {
            free_binding_of_insts.get(r.index()).copied().flatten()
        }
    }

    /// True if any instruction reachable from `root` (transitive
    /// operands and nested body statements) is a `Var` read of `name`.
    /// Conservative aliasing probe for the producer-into-home Assign
    /// path: an RHS that mentions the target binding (including a
    /// shadowed same-name read — the probe cannot distinguish shadowing)
    /// forbids the free-before-overwrite order.
    pub(super) fn expr_refs_name(tir: &Tir, root: TirRef, name: StringId) -> bool {
        let mut stack = vec![root];
        while let Some(r) = stack.pop() {
            if let TirData::Var(n) = tir.inst(r).data
                && n == name
            {
                return true;
            }
            tir.walk_operands(r, &mut |_parent, child, _kind| stack.push(child));
        }
        false
    }

    /// Provenance of a freshly stored home value: true iff the value
    /// is provably free-noop — a provably-inline producer result or a
    /// static (cap == 0) literal — so a later free on the home may be
    /// elided.
    pub(super) fn home_value_provably_inline(
        ctx: &FunctionContext<'_, M>,
        func: &cranelift::codegen::ir::Function,
        value: TirRef,
        cap: Value,
    ) -> bool {
        Self::provably_inline_producer(ctx, value) || Self::is_static_cap_zero(func, cap)
    }

    /// Set the home-provenance flag on a fat binding. No-op for
    /// home-less bindings (the flag is meaningless without a home).
    pub(crate) fn set_home_inline(ctx: &mut FunctionContext<'_, M>, name: StringId, inline: bool) {
        if let Some(mut fl) = Self::read_slot(&ctx.fat_locals, name)
            && fl.home.is_some()
            && fl.home_inline != inline
        {
            fl.home_inline = inline;
            Self::write_slot(
                &mut ctx.fat_locals,
                &mut ctx.fat_locals_undo,
                name,
                Some(fl),
            );
        }
    }

    /// Clear every home-provenance flag. Called at control-flow joins
    /// (if merges, loop headers and exits): the home slot is memory,
    /// so stores inside an arm or iteration persist while the scoped
    /// table restore reverts the flag — a join must not trust
    /// pre-branch provenance.
    pub(crate) fn invalidate_home_inline_flags(ctx: &mut FunctionContext<'_, M>) {
        let flagged: Vec<StringId> = (0..ctx.fat_locals.len())
            .filter(|&i| ctx.fat_locals[i].is_some_and(|fl| fl.home.is_some() && fl.home_inline))
            .map(|i| StringId::from_raw(u32::try_from(i).expect("StringId index out of range")))
            .collect();
        for name in flagged {
            Self::set_home_inline(ctx, name, false);
        }
    }
}
