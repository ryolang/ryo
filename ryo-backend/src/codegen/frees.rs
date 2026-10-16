//! Cleanup emission and representation-only release elision.
//!
//! Flow cleanup preparation selects storage and firing positions. The backend
//! supplies registers, stack slots, and inline-materialization facts.

use super::{Codegen, FunctionContext, ValueRepr, is_fat_type};
use cranelift::codegen::ir::{FuncRef, InstructionData, Opcode, ValueDef};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId};

/// Producer and parameter labels used only for representation elision.
type FreeBindingTables = (Vec<Option<StringId>>, Vec<Option<StringId>>);

impl<M: Module> Codegen<M> {
    /// Emit the drops assigned to one resolved firing-point bucket.
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
            if idx >= ctx.sidecar.drops.len() {
                Self::emit_promo_frees(builder, ctx, vec![idx - ctx.sidecar.drops.len()])?;
                continue;
            }
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
        flow: &ryo_core::flow::Flow,
        plan: &ryo_core::flow::cleanup::CleanupPlan,
        func_sidecar: &ryo_core::ownership::FunctionSidecar,
        freed_at: &mut [bool],
    ) {
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
            let binding_name = match plan.releases[idx] {
                ryo_core::flow::cleanup::ReleaseStorage::Binding(place) => {
                    Some(flow.place(place).name())
                }
                ryo_core::flow::cleanup::ReleaseStorage::Value(_) => None,
            };
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
        flow: &ryo_core::flow::Flow,
        pool: &InternPool,
    ) -> (Vec<bool>, Vec<bool>) {
        let mut mutated = vec![false; pool.string_count()];
        let mut view_bases = vec![false; tir.instructions.len()];
        for step in &flow.steps {
            for place in flow.mutations(&step.facts) {
                if is_fat_type(flow.place(place).ty(), pool) {
                    mutated[flow.place(place).name().raw() as usize] = true;
                }
            }
            if let Some(write) = step.facts.write
                && write.kind != ryo_core::flow::WriteKind::Init
                && is_fat_type(flow.place(write.place).ty(), pool)
            {
                mutated[flow.place(write.place).name().raw() as usize] = true;
            }
            if let Some(view) = step.facts.view {
                let ryo_core::flow::Place::View { base, .. } = flow.place(view) else {
                    unreachable!("view fact names a view place");
                };
                if is_fat_type(flow.place(*base).ty(), pool) {
                    mutated[flow.place(*base).name().raw() as usize] = true;
                    let value = match tir.inst(step.inst).data {
                        TirData::Slice { base, .. } | TirData::UnOp(base) => base,
                        _ => unreachable!("view step has a base operand"),
                    };
                    view_bases[value.index()] = true;
                }
            }
        }
        (mutated, view_bases)
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
            let binding = match ctx.cleanup_plan.releases[idx] {
                ryo_core::flow::cleanup::ReleaseStorage::Binding(place) => {
                    Some(match ctx.flow.place(place) {
                        ryo_core::flow::Place::Local { decl, .. } => *decl,
                        ryo_core::flow::Place::Token { inst, .. } => *inst,
                        ryo_core::flow::Place::Param { index, .. } => {
                            TirRef::param(*index as usize)
                        }
                        _ => {
                            return Err(format!(
                                "cleanup binding has no backend storage: {place:?}"
                            ));
                        }
                    })
                }
                ryo_core::flow::cleanup::ReleaseStorage::Value(_) => None,
            };
            // M11: enum-typed targets route to the tag-dispatched
            // payload drop; M9 structs to the recursive field drop;
            // everything below is the str/bytes path.
            if Self::try_emit_enum_free(builder, ctx, target, binding)? {
                ctx.freed_targets.insert(target);
                continue;
            }
            if Self::try_emit_struct_free(builder, ctx, target, binding)? {
                ctx.freed_targets.insert(target);
                continue;
            }
            let is_bytes = Self::free_target_is_bytes(ctx, target);
            // Storage selection is complete before emission. Representation caches
            // supply register values, never the identity of a release.
            if let Some(binding) = binding {
                let storage = ctx
                    .fat_binding_storage
                    .get(&binding)
                    .copied()
                    .ok_or_else(|| {
                        format!("cleanup target {target:?} has no storage for binding {binding:?}")
                    })?;
                Self::require_fat_storage_binding(storage, binding)?;
                // Scoped provenance can elide a call only for this exact storage.
                // Name restoration must never select a different binding's buffer.
                let inline = Self::free_binding_name(ctx, target)
                    .and_then(|name| Self::read_slot(&ctx.fat_locals, name))
                    .is_some_and(|active| {
                        active.ptr == storage.ptr
                            && active.home == storage.home
                            && active.home_inline
                    });
                if ctx.optional_optimizations && inline {
                    continue;
                }
                let (ptr, cap) = Self::load_fat_storage_ptr_cap(builder, ctx.int_type, storage);
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
            if idx >= ctx.sidecar.drops.len() {
                Self::emit_promo_frees(builder, ctx, vec![idx - ctx.sidecar.drops.len()])?;
                continue;
            }
            if ctx.freed_at[idx] {
                continue;
            }
            ctx.freed_at[idx] = true;
            #[cfg(debug_assertions)]
            {
                ctx.fire_marked[idx] = true;
            }
            let target = ctx.sidecar.drops[idx].target;
            if let Some(payload) = ctx.cleanup_plan.payloads[idx].clone() {
                if ctx.freed_targets.contains(&target) {
                    continue;
                }
                Self::emit_arm_start_drops(builder, ctx, &payload)?;
                ctx.freed_targets.insert(target);
            } else {
                if ctx.freed_targets.contains(&target) {
                    continue;
                }
                Self::emit_frees(builder, ctx, vec![(idx, target)])?;
            }
        }
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
        let mut param_names = vec![None; tir.params.len()];
        for (idx, param) in tir.params.iter().enumerate() {
            if is_fat_type(param.ty, pool) {
                // A fat param's binding identity is its own sentinel ref
                // (params are bound at entry and never shadowed).
                param_names[idx] = Some(param.name);
            }
        }
        let mut inst_names = vec![None; tir.instructions.len()];
        for place in &flow.places {
            if let ryo_core::flow::Place::Token { inst, name, .. } = *place {
                inst_names[inst.index()] = Some(name);
            }
        }
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
            inst_names[value.index()] = Some(name);
            // A release may target the
            // declaring `VarDecl` itself — the seat of a binding whose
            // owner is the binding's home place (a moved-through-`Var`
            // value). The decl names its binding exactly like an init
            // value does, so the binding-path redirect resolves it.
            if tir.inst(step.inst).tag == TirTag::VarDecl {
                inst_names[step.inst.index()] = Some(name);
            }
        }
        (inst_names, param_names)
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

    /// Every scheduled promotion release must reach its resolved firing point.
    #[cfg(debug_assertions)]
    fn assert_no_dropped_promo_frees(ctx: &FunctionContext<'_, M>) {
        let unconsumed: Vec<_> = ctx
            .promo_freed_at
            .iter()
            .enumerate()
            .filter_map(|(index, fired)| (!fired).then_some(index))
            .collect();
        debug_assert!(
            unconsumed.is_empty(),
            "promotion releases never consumed: {unconsumed:?}"
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

    /// Check the concrete storage selected by each fired release. Elided releases
    /// must satisfy the same no-allocation predicate used during preparation.
    #[cfg(debug_assertions)]
    fn assert_drop_place_resolution(ctx: &FunctionContext<'_, M>) {
        {
            let (fat_mutated, view_base_insts) =
                Self::build_fat_mutation_tables(ctx.tir, ctx.flow, ctx.pool);
            // Branch-aware elision predicate, mirroring `pre_elide_drops`.
            let predicate_holds = |idx: usize, target: TirRef| {
                let static_literal = !target.is_param()
                    && matches!(
                        ctx.tir.inst(target).tag,
                        ryo_core::tir::TirTag::StrConst | ryo_core::tir::TirTag::BytesConst
                    );
                let inline_producer = !target.is_param()
                    && ctx.tir.inst(target).tag == ryo_core::tir::TirTag::Call
                    && Self::producer_max_output_len(ctx.pool.str(ctx.tir.call_view(target).name))
                        .is_some_and(|max| max as usize <= ryo_runtime::INLINE_CAP);
                let redirect = match ctx.cleanup_plan.releases[idx] {
                    ryo_core::flow::cleanup::ReleaseStorage::Binding(place) => {
                        Some(ctx.flow.place(place).name())
                    }
                    ryo_core::flow::cleanup::ReleaseStorage::Value(_) => None,
                };
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
                    let resolved = match ctx.cleanup_plan.releases[idx] {
                        ryo_core::flow::cleanup::ReleaseStorage::Binding(place) => {
                            let binding = match ctx.flow.place(place) {
                                ryo_core::flow::Place::Local { decl, .. } => Some(*decl),
                                ryo_core::flow::Place::Token { inst, .. } => Some(*inst),
                                ryo_core::flow::Place::Param { index, .. } => {
                                    Some(TirRef::param(*index as usize))
                                }
                                _ => None,
                            };
                            binding.is_some_and(|binding| {
                                ctx.aggregate_binding_storage.contains_key(&binding)
                                    || ctx.fat_binding_storage.get(&binding).is_some_and(
                                        |storage| {
                                            Self::require_fat_storage_binding(*storage, binding)
                                                .is_ok()
                                        },
                                    )
                            })
                        }
                        ryo_core::flow::cleanup::ReleaseStorage::Value(value) => {
                            Self::cached_repr(ctx, value).is_some()
                        }
                    } || ctx.cleanup_plan.payloads[idx].is_some();
                    if !resolved {
                        bad_resolution.push((idx, target.index()));
                    }
                } else if !predicate_holds(idx, target) {
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

    /// Validate the physical representation selected for a declaring binding.
    pub(super) fn require_fat_storage_binding(
        storage: super::FatLocals,
        expected: TirRef,
    ) -> Result<(), String> {
        if storage.binding != Some(expected) {
            return Err(format!(
                "cleanup selected storage for {:?}, expected {expected:?}",
                storage.binding
            ));
        }
        Ok(())
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

    /// Resolve RHS aliasing from Flow reads before backend emission.
    pub(super) fn assignment_aliases(tir: &Tir, flow: &ryo_core::flow::Flow) -> Vec<bool> {
        let mut aliases = vec![false; tir.instructions.len()];
        for step in &flow.steps {
            let Some(read) = step.facts.read else {
                continue;
            };
            let mut cursor = step.inst;
            while let Some(parent) = flow.parent_of(cursor) {
                if tir.inst(parent).tag == TirTag::Assign
                    && flow
                        .facts_of(parent)
                        .and_then(|f| f.write)
                        .is_some_and(|w| w.place == read)
                {
                    aliases[parent.index()] = true;
                }
                cursor = parent;
            }
        }
        aliases
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
