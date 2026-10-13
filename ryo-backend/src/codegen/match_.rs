//! `match` expression codegen (M12) — enum tag dispatch with wildcard
//! routing, anonymous-struct guard chains (int/bool literal guards),
//! per-arm payload binding moves, and scalar/void merges.
//!
//! The typed `TirTag::Match` arrives fully resolved (the typed TIR
//! match view, [`MatchView`], in `ryo_core::tir::match_`): every arm is
//! a [`Discriminant`] plus typed
//! [`MatchBinding`][ryo_core::tir::MatchBinding]s, anon arms carrying
//! their field-equality guards on the wire. The lowering shape:
//!
//! 1. The scrutinee is evaluated into its canonical home — enum and
//!    anon-struct scrutinees into their stack slot (`ValueRepr::Enum` /
//!    `ValueRepr::Struct` address), int/bool scrutinees as the loaded
//!    scalar — and its address/value is the payload base for every arm.
//! 2. A merge block is created up front: scalar results ride one block
//!    param (the `enums.rs` Eq precedent), void matches none, aggregate
//!    results (str/bytes/struct/anon-struct/enum) a pre-allocated result
//!    SLOT — sized from the pool before the dispatch, so the slot
//!    address is a constant dominating every arm and the merge.
//! 3. Dispatch per scrutinee kind: enums load the i32 tag and switch
//!    (`emit_tag_dispatch`), routing each variant's case to the FIRST
//!    arm covering it (written order wins; W0005-dead duplicates stay
//!    unrouted) and the wildcard arm's entry to the switch's `otherwise`
//!    — full coverage without a wildcard keeps the invalid-tag trap.
//!    Anon-struct scrutinees lower to a linear chain of guard
//!    conjunctions: each failed compare short-circuits to the next
//!    arm's test block, and the final wildcard arm is the fallthrough.
//!    Literal scrutinees (int/bool/str) lower to the same chain shape
//!    over `Discriminant::Int/Bool/Str` arms: one compare (`icmp_imm eq`
//!    for int/bool, the runtime's bytewise `ryo_str_eq` for str — the
//!    M9.1 `emit_field_eq` convention) plus `brif` per arm in written
//!    order, the first bare wildcard the fallthrough, and a defensive
//!    trap as the chain tail when no wildcard exists (reachable only
//!    for a violated sema E0129 — a fully `true`/`false`-covered bool
//!    never falls off its last compare).
//! 4. Each arm runs in its own block under the undo-log slot-table
//!    scope (`emit_scoped_body`'s discipline — bindings are arm-local):
//!    arm-start payload drops ([`MatchArmDrop`][ryo_core::ownership::MatchArmDrop],
//!    the consuming arm's unbound needs-drop fields), then the binding
//!    prologue through the shared `bind_field_move` (fat triples,
//!    struct/enum slot copies, scalar loads — the M10 destructure
//!    logic), then the body. Expression arms write the match result —
//!    scalar values jump with the block param, aggregate values write
//!    the result slot through the canonical per-type aggregate writes
//!    (the exact helpers the function-return paths use), void matches
//!    jump bare — and diverging arms (never-typed bodies,
//!    return/break/continue) leave no merge edge and write nothing.
//!    Statement arms go through `emit_scoped_body` and its `Terminator`
//!    sense — only non-terminating arms jump.
//! 5. The merge: scalars read the block param; aggregates read the
//!    result slot (a fat triple load for str/bytes, the slot address
//!    for memory-first structs/enums) and cache the family
//!    `ValueRepr` on the inst, so aggregate consumers reach it through
//!    their own entry points (`eval_inst_struct` / `eval_inst_enum` /
//!    `eval_inst_fat`). Diverging arms leave no edge; when EVERY arm
//!    diverges the merge is unreachable and traps.
//!
//! The post-merge drop of an owned needs-drop scrutinee TEMPORARY needs
//! no codegen of its own: the ownership pass's anon-temp machinery
//! schedules the whole-value Free anchored after this Match inst (the
//! scrutinee's single consumer), and the end-of-statement sweep lands
//! it here — post-merge on every path that reaches the merge, with the
//! `freed_at` table guaranteeing one emission per schedule entry.

use super::{
    Codegen, FunctionContext, STR_SLOT_SIZE, Terminator, ValueRepr, cranelift_type_for,
    is_enum_type, is_struct_type,
};
use cranelift::codegen::ir::{BlockArg, MemFlagsData, StackSlotData, StackSlotKind};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::ast::LitPattern;
use ryo_core::tir::{Discriminant, MatchArmView, MatchBodyView, MatchView, TirData, TirRef};
use ryo_core::types::{InternPool, StringId, TypeId, TypeKind};

/// The merge shape of a lowered match, derived from its result TypeId.
enum MatchShape {
    /// Void match: the merge block carries no param; arms jump bare.
    Void,
    /// Scalar match (int/bool/float): one merge block param.
    Scalar(types::Type),
    /// Every arm diverges: no merge param, and the merge is unreachable.
    Never,
    /// Aggregate match (str/bytes/struct/anon-struct/enum): every value
    /// arm writes the pre-allocated result slot; the merge yields the
    /// slot (never a block param — a value materialized in one arm's
    /// block could not dominate the merge, while the slot address is a
    /// pre-dispatch constant dominating every arm).
    Aggregate(TypeId),
}

/// Where the scrutinee value lives after evaluation.
#[derive(Clone, Copy)]
enum ScrutineeHome {
    /// Enum / anon-struct scrutinee: the address of its stack slot.
    Slot(Value),
    /// int/bool scrutinee: the loaded scalar — the compare operand for
    /// every literal arm (`icmp_imm eq` against the arm's immediate).
    Scalar(Value),
    /// str scrutinee: the extracted `(ptr, len)` content pair — the
    /// bytewise compare operands against each literal arm's `.rodata`
    /// pair (the scrutinee's SSO-inline values were spilled to a
    /// scratch slot by the extraction, so the pair is chain-stable).
    Str { ptr: Value, len: Value },
}

/// The compare operand(s) of a literal-arm chain, fixed by the
/// scrutinee's type kind: int/bool compare the loaded scalar against an
/// immediate; str compares the extracted content pair against each
/// literal through the runtime's `ryo_str_eq`.
#[derive(Clone, Copy)]
enum LiteralScrut {
    Scalar(Value),
    Str { ptr: Value, len: Value },
}

impl<M: Module> Codegen<M> {
    /// Lower a typed `TirTag::Match` (M12); the `eval_inst` dispatch
    /// target. Returns the scalar merge value (the block param), or a
    /// dummy for void/never/aggregate shapes — aggregate results cache
    /// their family `ValueRepr` on the inst (see [`MatchShape`]).
    pub(crate) fn eval_inst_match(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let view = ctx.tir.match_view(r);
        if view.is_empty() {
            return Err("eval_inst_match: match with no arms reached codegen".to_string());
        }
        let sidecar = ctx.sidecar;
        let shape = Self::match_shape(ctx, view.ty)?;
        let scrut_ty = ctx.tir.inst(view.scrutinee).ty;

        // 1. Scrutinee into its canonical home; the payload base for
        // every arm. Whether the scrutinee is an owned temporary or a
        // place matters only for the scheduled post-merge drop (the
        // end-of-statement sweep fires the ownership pass's Free).
        let scrut = match ctx.pool.kind(scrut_ty) {
            TypeKind::Enum => {
                ScrutineeHome::Slot(Self::eval_inst_enum(builder, ctx, view.scrutinee)?)
            }
            TypeKind::AnonStruct => {
                ScrutineeHome::Slot(Self::eval_inst_struct(builder, ctx, view.scrutinee)?)
            }
            TypeKind::Int | TypeKind::Bool => {
                ScrutineeHome::Scalar(Self::eval_inst(builder, ctx, view.scrutinee)?)
            }
            TypeKind::Str => {
                let repr = Self::eval_inst_fat(builder, ctx, view.scrutinee)?;
                let ValueRepr::Str { ptr, len, cap } = repr else {
                    return Err(
                        "eval_inst_match: str scrutinee did not lower to a str triple".to_string(),
                    );
                };
                // Transient content extraction (the M9.1 Eq discipline):
                // an SSO-inline scrutinee spills to a fresh scratch slot
                // and hands back that address as the content pointer.
                let (ptr, len) = Self::emit_fat_bytes_ptr_len(builder, ctx, ptr, len, cap, None)?;
                ScrutineeHome::Str { ptr, len }
            }
            other => {
                return Err(format!(
                    "eval_inst_match: scrutinee type kind {other:?} is not matchable (sema E0136 gates this)"
                ));
            }
        };

        // 2. Merge block up front; scalar results ride one block param.
        let merge = builder.create_block();
        if let MatchShape::Scalar(cl_ty) = shape {
            builder.append_block_param(merge, cl_ty);
        }
        // Aggregate results ride a pre-allocated result slot instead of
        // a block param: allocated BEFORE the dispatch so the slot
        // address is defined in the scrutinee block, which dominates
        // every arm and the merge — the dominance hazard (a value
        // produced in one arm read from the merge) cannot arise because
        // arms write memory and the merge reads it.
        let result_slot = if let MatchShape::Aggregate(ty) = shape {
            let slot = match ctx.pool.kind(ty) {
                TypeKind::Str | TypeKind::Bytes => builder.create_sized_stack_slot(
                    StackSlotData::new(StackSlotKind::ExplicitSlot, STR_SLOT_SIZE, 3),
                ),
                TypeKind::Struct | TypeKind::AnonStruct => Self::struct_slot(builder, ctx, ty),
                TypeKind::Enum => Self::enum_slot(builder, ctx, ty),
                other => unreachable!("match_shape rejected {other:?} as an aggregate result"),
            };
            Some((ty, builder.ins().stack_addr(ctx.int_type, slot, 0)))
        } else {
            None
        };

        // 3. Dispatch: fill each arm's entry block.
        let arm_count = view.len();
        let mut arm_entry: Vec<Option<Block>> = vec![None; arm_count];
        match scrut {
            ScrutineeHome::Slot(addr) if is_enum_type(scrut_ty, ctx.pool) => {
                Self::route_enum_dispatch(builder, ctx, scrut_ty, &view, addr, &mut arm_entry)?;
            }
            ScrutineeHome::Slot(addr) => {
                debug_assert!(
                    is_struct_type(scrut_ty, ctx.pool),
                    "slot-homed scrutinee must be an enum or anon struct"
                );
                Self::route_anon_dispatch(builder, ctx, scrut_ty, &view, addr, &mut arm_entry)?;
            }
            ScrutineeHome::Scalar(v) => {
                Self::route_literal_dispatch(
                    builder,
                    ctx,
                    &view,
                    LiteralScrut::Scalar(v),
                    &mut arm_entry,
                )?;
            }
            ScrutineeHome::Str { ptr, len } => {
                Self::route_literal_dispatch(
                    builder,
                    ctx,
                    &view,
                    LiteralScrut::Str { ptr, len },
                    &mut arm_entry,
                )?;
            }
        }

        // 4. Arm emission. Facts join bookkeeping mirrors
        // `generate_if_stmt`: arm bodies may seed/kill range facts, so
        // the merge restores the pre-match facts and kills whatever the
        // arms assigned.
        let outer_facts_mark = ctx.range_facts_undo.len();
        let scope_mark = ctx.assigned_log.len();
        let tokens = ctx.tir.match_by_move_owner_refs(r, ctx.pool);
        let mut token_cursor = 0usize;
        let mut all_terminated = true;

        for (i, entry) in arm_entry.iter().enumerate() {
            let arm = view.arm(i);
            // Duplicate arms past a wildcard/route (W0005-dead) get an
            // unreachable block; the emission below is uniform.
            let block = entry.unwrap_or_else(|| builder.create_block());
            builder.seal_block(block);
            builder.switch_to_block(block);
            let branch_id = sidecar
                .match_branches
                .get(r.index())
                .and_then(|ids| ids.as_ref())
                .and_then(|ids| ids.arms.get(i))
                .copied()
                .unwrap_or_default();
            // The arm's BranchId gates branch-gated FreePoints and the
            // arm-start drops while the body lowers (the if-arm
            // discipline).
            ctx.branch_stack.push(branch_id);

            // Binding scope: payload bindings are arm-local (sema child
            // scopes) — roll the slot tables back when the arm ends.
            // `range_facts` joins too: an arm body may seed facts, and no
            // sibling arm (or the post-match code) may observe them.
            let locals_mark = ctx.locals_undo.len();
            let fat_mark = ctx.fat_locals_undo.len();
            let struct_mark = ctx.struct_locals_undo.len();
            let facts_mark = ctx.range_facts_undo.len();

            if let ScrutineeHome::Slot(addr) = scrut {
                // Arm-start drops: the consuming arm's unbound
                // needs-drop payload fields (MatchArmDrop). The tag
                // dispatch guarantees the discriminated variant, so the
                // field address is the scrutinee slot plus the
                // pool-computed absolute offset.
                for drop in sidecar
                    .match_arm_drops
                    .iter()
                    .filter(|d| d.match_inst == r && d.arm == branch_id)
                {
                    let offset = Self::arm_payload_field_offset(
                        ctx.pool,
                        scrut_ty,
                        &arm.discriminant,
                        drop.field_index,
                    )?;
                    Self::emit_field_drop(builder, ctx, addr, offset, drop.ty)?;
                }
                // Binding prologue: per-binding move out of the
                // scrutinee slot through the shared `bind_field_move`
                // (the M10 destructure move logic). Copy bindings pass
                // no owner token — sema's convention emits one token
                // per by-move binding only.
                for b in arm.bindings() {
                    let offset = Self::arm_payload_field_offset(
                        ctx.pool,
                        scrut_ty,
                        &arm.discriminant,
                        b.field_index,
                    )?;
                    let field_addr = if offset == 0 {
                        addr
                    } else {
                        builder.ins().iadd_imm_s(addr, i64::from(offset))
                    };
                    let token = if ctx.pool.is_copy(b.ty) {
                        None
                    } else {
                        let token = tokens[token_cursor];
                        token_cursor += 1;
                        Some(token)
                    };
                    Self::bind_field_move(builder, ctx, b.name, field_addr, b.ty, token)?;
                }
            }

            // Body. Non-terminating arms jump to the merge (with the
            // value for scalar shapes, after writing the result slot for
            // aggregate shapes); diverging arms leave no edge and write
            // nothing.
            let falls_through = match arm.body {
                MatchBodyView::Expr(e) => {
                    let body_ty = ctx.tir.inst(e).ty;
                    if let MatchShape::Aggregate(ty) = shape {
                        if ctx.pool.is_never(body_ty) {
                            // A diverging arm body (a noreturn call)
                            // still evaluates — for its side effects.
                            let _ = Self::eval_inst(builder, ctx, e)?;
                            Self::terminate_dead_arm_block(builder);
                            false
                        } else {
                            let (_, slot_addr) =
                                result_slot.expect("aggregate shape has a result slot");
                            Self::emit_match_result_write(builder, ctx, slot_addr, ty, e)?;
                            builder.ins().jump(merge, &[]);
                            true
                        }
                    } else {
                        let v = Self::eval_inst(builder, ctx, e)?;
                        if ctx.pool.is_never(body_ty) {
                            Self::terminate_dead_arm_block(builder);
                            false
                        } else {
                            // Arm-body frees: fire everything that became
                            // due while the arm expression evaluated —
                            // branch-gated Frees (arm-local dead binds,
                            // divergence frees) and sweep entries whose
                            // anchors the body materialized — BEFORE the
                            // merge jump, while the arm's BranchId is
                            // still on the branch_stack. After the pop
                            // the end-of-statement sweep can never see
                            // branch-gated entries (leak). Mirrors
                            // emit_body's post-statement calls.
                            Self::emit_due_frees(builder, ctx, e)?;
                            Self::emit_due_promo_frees(builder, ctx, e)?;
                            Self::sweep_due_frees(builder, ctx)?;
                            Self::sweep_due_promo_frees(builder, ctx)?;
                            match shape {
                                MatchShape::Scalar(_) => {
                                    builder.ins().jump(merge, &[BlockArg::Value(v)]);
                                }
                                MatchShape::Void | MatchShape::Never => {
                                    builder.ins().jump(merge, &[]);
                                }
                                MatchShape::Aggregate(_) => {
                                    unreachable!("aggregate shapes handled above")
                                }
                            }
                            true
                        }
                    }
                }
                MatchBodyView::Stmts(_) => {
                    let stmts: Vec<TirRef> = arm.body.stmts().collect();
                    let term = Self::emit_scoped_body(builder, ctx, &stmts, false)?;
                    if term != Terminator::None {
                        // Return / Break / Continue — no merge edge.
                        false
                    } else if ctx.tir.block_definitely_terminates(&stmts, ctx.pool) {
                        // Sema-level divergence emit_stmt cannot report:
                        // the body's last statement is never-typed (a bare
                        // `panic(...)` ExprStmt — the shape `stmt_terminates`
                        // counts as terminating, and the noreturn-call path
                        // already switched the builder into its sealed dead
                        // block). The arm diverges like a `return` arm: no
                        // merge edge, and the dead block gets its terminator
                        // so finalize never sees a sealed, unterminated block.
                        Self::terminate_dead_arm_block(builder);
                        false
                    } else {
                        match shape {
                            MatchShape::Void => {
                                builder.ins().jump(merge, &[]);
                            }
                            MatchShape::Scalar(_) | MatchShape::Aggregate(_) => {
                                return Err(
                                    "eval_inst_match: falling statement arm in a value-producing match (sema E0135 gates this)"
                                        .to_string(),
                                )
                            }
                            MatchShape::Never => {
                                return Err(
                                    "eval_inst_match: falling statement arm in a never-typed match (the sema join gates this)"
                                        .to_string(),
                                )
                            }
                        }
                        true
                    }
                }
            };
            all_terminated &= !falls_through;

            Self::restore_slots(&mut ctx.locals, &mut ctx.locals_undo, locals_mark);
            Self::restore_slots(&mut ctx.fat_locals, &mut ctx.fat_locals_undo, fat_mark);
            Self::restore_slots(
                &mut ctx.struct_locals,
                &mut ctx.struct_locals_undo,
                struct_mark,
            );
            Self::restore_slots(&mut ctx.range_facts, &mut ctx.range_facts_undo, facts_mark);
            ctx.branch_stack.pop();
        }
        debug_assert_eq!(
            token_cursor,
            tokens.len(),
            "one owner token consumed per by-move binding"
        );

        // 5. Merge. All arms diverged: the merge is unreachable — trap
        // it (the post-divergence continuation shape) and keep emitting
        // in a fresh dead block, mirroring the never-call path.
        builder.seal_block(merge);
        let merge_unreachable = all_terminated && !matches!(shape, MatchShape::Never);
        if merge_unreachable {
            builder.switch_to_block(merge);
            builder.ins().trap(
                TrapCode::user(1).expect("user trap code 1 is within Cranelift's encodable range"),
            );
            let dead = builder.create_block();
            builder.seal_block(dead);
            builder.switch_to_block(dead);
        } else {
            builder.switch_to_block(merge);
        }

        // Facts join (the if-arm discipline).
        Self::restore_slots(
            &mut ctx.range_facts,
            &mut ctx.range_facts_undo,
            outer_facts_mark,
        );
        Self::invalidate_home_inline_flags(ctx);
        Self::kill_assigned_since(ctx, scope_mark);

        // Diagnose unsupported aggregate value-arm shapes (I-210): the
        // result slot's (shallow) per-type write ALIASES the arm body's
        // buffer, so any scheduled drop that releases that storage
        // cannot be correctly emitted — in the arm block it would free
        // memory the result still aliases, and the end-of-statement
        // sweep's post-merge position reads values that do not dominate
        // the merge block (a Cranelift verifier error in debug builds,
        // a silent miscompile in release). Two arm-body families alias:
        //
        // * Producers (calls, concats, needs-drop literals): the
        //   ownership pass schedules their temporary's Free anchored on
        //   this Match inst (the arm body's single consumer).
        // * By-value reads of tracked bindings (`Var` / `FieldAccess`
        //   arm bodies): the binding's last-use Free lands post-merge
        //   while the result aliases the binding's storage.
        //
        // The expression-arm free flush above deliberately does NOT run
        // for aggregate shapes: firing the binding-read Free inside the
        // arm would free the buffer the result slot then aliases AND
        // mark the schedule entry fired, suppressing this rejection —
        // the loud error would become a silent use-after-free. Aggregate
        // arms are rejected before any of their frees could fire.
        //
        // Ownership must instead consume the arm-body result into the
        // match; until it does, fail loudly. Elided frees (static
        // cap == 0 literals, provably-inline producers) and non-arm-body
        // targets (the scrutinee temporary — its repr dominates the
        // merge) are not this shape; the sweep owns them.
        for arm in view.arms() {
            let MatchBodyView::Expr(e) = arm.body else {
                continue;
            };
            let body_ty = ctx.tir.inst(e).ty;
            if ctx.pool.is_never(body_ty) || !ctx.pool.needs_drop(body_ty) {
                continue;
            }
            let anchored: Vec<usize> = ctx
                .free_by_after
                .get(r.index())
                .map(|indices| {
                    indices
                        .iter()
                        .copied()
                        .filter(|&idx| !ctx.freed_at[idx])
                        .collect()
                })
                .unwrap_or_default();
            if anchored
                .iter()
                .any(|&idx| ctx.sidecar.free_schedule[idx].target == e)
                && Self::free_would_emit(builder, ctx, e)
            {
                return Err(Self::arm_alias_error(ctx, e));
            }
            // By-value read of a tracked binding: resolve the root name
            // and look for a pending drop scheduled against the binding's
            // CURRENT value (the same currency filter `emit_frees`'s
            // binding-path redirect uses — a stale-value free targets
            // superseded storage and does not alias the read).
            let mut root = e;
            while let TirData::FieldAccess { object, .. } = ctx.tir.inst(root).data {
                root = object;
            }
            if let TirData::Var(name) = ctx.tir.inst(root).data {
                let pending_free = ctx.pending_sweep.iter().any(|&idx| {
                    if ctx.freed_at[idx] {
                        return false;
                    }
                    let fp = &ctx.sidecar.free_schedule[idx];
                    if !Self::free_would_emit(builder, ctx, fp.target)
                        || Self::free_binding_name(ctx, fp.target) != Some(name)
                    {
                        return false;
                    }
                    match Self::free_binding_of(ctx, fp.target) {
                        Some(binding) => ctx.binding_last_write.get(&binding) == Some(&fp.target),
                        None => false,
                    }
                });
                if pending_free {
                    return Err(Self::arm_alias_error(ctx, e));
                }
            }
        }

        let value = match shape {
            MatchShape::Scalar(cl_ty) if merge_unreachable => builder.ins().iconst(cl_ty, 0),
            MatchShape::Scalar(_) => builder.block_params(merge)[0],
            MatchShape::Void => builder.ins().iconst(ctx.int_type, 0),
            MatchShape::Never => builder.ins().iconst(types::I8, 0),
            MatchShape::Aggregate(ty) => {
                // The result lives in the pre-allocated slot; cache the
                // family repr so aggregate consumers (var decls, call
                // args, returns, nested aggregate ops) read it through
                // their own entry points. The scalar Value returned
                // here is a never-used dummy for those shapes. An
                // aggregate-typed match always has at least one
                // falling-through value arm (sema's join types the
                // match by its value arms), so the merge is reachable
                // and the slot written on every path here.
                debug_assert!(
                    !merge_unreachable,
                    "an aggregate-typed match always has a value arm"
                );
                let (_, slot_addr) = result_slot.expect("aggregate shape has a result slot");
                let repr = Self::match_result_repr(builder, ctx, slot_addr, ty)?;
                Self::cache_repr(ctx, r, repr);
                builder.ins().iconst(ctx.int_type, 0)
            }
        };
        Ok(value)
    }

    /// Write one aggregate arm-body value into the match's result slot
    /// through the canonical per-type aggregate writes — the exact
    /// internals the function-return paths use (`emit_struct_return` /
    /// `emit_enum_return` copy field-wise through the sret pointer; the
    /// fat Return arm stores the triple), so a match result can never
    /// drift from the return ABI's layout conventions. The copy is
    /// family-appropriate (fat triple store / field-wise struct copy /
    /// tag + active-payload enum copy), never a byte-wise block copy.
    fn emit_match_result_write(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        slot_addr: Value,
        ty: TypeId,
        body: TirRef,
    ) -> Result<(), String> {
        match ctx.pool.kind(ty) {
            TypeKind::Str | TypeKind::Bytes => {
                let repr = Self::eval_inst_fat(builder, ctx, body)?;
                let (ptr, len, cap) = match repr {
                    ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                        (ptr, len, cap)
                    }
                    _ => unreachable!("str/bytes arm body must produce a fat ValueRepr"),
                };
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), ptr, slot_addr, 0);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), len, slot_addr, 8);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), cap, slot_addr, 16);
                Ok(())
            }
            TypeKind::Struct | TypeKind::AnonStruct => {
                let src = Self::eval_inst_struct(builder, ctx, body)?;
                Self::emit_struct_copy(builder, ctx, slot_addr, src, ty)
            }
            TypeKind::Enum => {
                let src = Self::eval_inst_enum(builder, ctx, body)?;
                Self::emit_enum_copy(builder, ctx, slot_addr, src, ty)
            }
            other => Err(format!(
                "eval_inst_match: {other:?} arm result is not an aggregate (sema gates match result types)"
            )),
        }
    }

    /// The merge-block `ValueRepr` of an aggregate match result at
    /// `slot_addr`: str/bytes load the fat triple from the slot (the
    /// `eval_field_access_fat` convention); memory-first structs and
    /// enums ARE their slot address.
    fn match_result_repr(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        slot_addr: Value,
        ty: TypeId,
    ) -> Result<ValueRepr, String> {
        match ctx.pool.kind(ty) {
            TypeKind::Str | TypeKind::Bytes => {
                let ptr = builder
                    .ins()
                    .load(ctx.int_type, MemFlagsData::trusted(), slot_addr, 0);
                let len = builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), slot_addr, 8);
                let cap = builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), slot_addr, 16);
                Ok(if matches!(ctx.pool.kind(ty), TypeKind::Bytes) {
                    ValueRepr::Bytes { ptr, len, cap }
                } else {
                    ValueRepr::Str { ptr, len, cap }
                })
            }
            TypeKind::Struct | TypeKind::AnonStruct => Ok(ValueRepr::Struct { addr: slot_addr }),
            TypeKind::Enum => Ok(ValueRepr::Enum { addr: slot_addr }),
            other => Err(format!(
                "eval_inst_match: {other:?} is not a lowered aggregate match result"
            )),
        }
    }

    /// The I-210 diagnosis for an aggregate value arm whose result
    /// aliases storage a scheduled drop will release outside the arm.
    fn arm_alias_error(ctx: &FunctionContext<'_, M>, body: TirRef) -> String {
        let ty = ctx.tir.inst(body).ty;
        format!(
            "eval_inst_match: a value arm yields an owned {} value whose drop is scheduled outside the arm (I-210): the result slot aliases the buffer, so lowering it would double-free or read arm-local values from the merge. Use a mutable binding assigned inside an if/else instead, until the ownership pass consumes arm-body results",
            ctx.pool.display(ty)
        )
    }

    /// Give the builder's current block a terminator when a diverging
    /// arm body leaves it sealed-but-unterminated: the noreturn-call
    /// path emits its trap and switches into a fresh SEALED dead block,
    /// and a diverging arm must leave no merge edge — so the abandoned
    /// dead block needs an explicit terminator before finalize (the
    /// merge-unreachable path's user trap code, the file convention).
    fn terminate_dead_arm_block(builder: &mut FunctionBuilder) {
        builder.ins().trap(
            TrapCode::user(1).expect("user trap code 1 is within Cranelift's encodable range"),
        );
    }

    /// True when emitting the scheduled Free for `target` would produce
    /// an actual drop call — the mirror of `emit_frees`' elision
    /// predicates (`is_static_cap_zero`, the provably-inline producer
    /// exemption), so the I-210 diagnosis above only fires for frees that
    /// would really run. Struct/enum drops are scheduled only for
    /// needs-drop values, where the drop always emits real field frees.
    fn free_would_emit(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        target: TirRef,
    ) -> bool {
        let Some(repr) = Self::cached_repr(ctx, target) else {
            return false;
        };
        match repr {
            ValueRepr::Str { cap, .. } | ValueRepr::Bytes { cap, .. } => {
                !Self::is_static_cap_zero(builder.func, cap)
                    && !(Self::provably_inline_producer(ctx, target)
                        && !ctx.view_base_insts[target.index()])
            }
            ValueRepr::Struct { .. } | ValueRepr::Enum { .. } => true,
            ValueRepr::Scalar(_) | ValueRepr::View { .. } => false,
        }
    }

    /// The merge shape for a match result type: void, scalar (one
    /// Cranelift type), never, or aggregate (str/bytes/struct/
    /// anon-struct/enum — anything `cranelift_type_for` rejects). A
    /// view-typed result (and any other unexpected kind) is a sema
    /// contract violation: reported, never silently miscompiled.
    fn match_shape(ctx: &FunctionContext<'_, M>, ty: TypeId) -> Result<MatchShape, String> {
        match ctx.pool.kind(ty) {
            TypeKind::Void => Ok(MatchShape::Void),
            TypeKind::Int | TypeKind::Bool | TypeKind::Float => Ok(MatchShape::Scalar(
                cranelift_type_for(ty, ctx.pool, ctx.int_type),
            )),
            TypeKind::Never => Ok(MatchShape::Never),
            TypeKind::Str
            | TypeKind::Bytes
            | TypeKind::Struct
            | TypeKind::AnonStruct
            | TypeKind::Enum => Ok(MatchShape::Aggregate(ty)),
            other => Err(format!(
                "eval_inst_match: match result type kind {other:?} is not lowerable (sema gates match result types)"
            )),
        }
    }

    /// Enum scrutinee: i32 tag load plus `emit_tag_dispatch`. Each
    /// variant's case block jumps to the FIRST arm covering it (written
    /// order wins; duplicate arms past the first are W0005-dead and get
    /// no case edge), and the wildcard arm's entry takes the switch's
    /// `otherwise` — full coverage without a wildcard keeps the
    /// invalid-tag trap, exactly the M11 invariant semantics.
    fn route_enum_dispatch(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        scrut_ty: TypeId,
        view: &MatchView<'_>,
        scrutinee_addr: Value,
        arm_entry: &mut [Option<Block>],
    ) -> Result<(), String> {
        let variant_count = ctx.pool.enum_view(scrut_ty).variants().count();
        let tag = builder
            .ins()
            .load(types::I32, MemFlagsData::trusted(), scrutinee_addr, 0);
        let (cases, otherwise) = Self::emit_tag_dispatch(builder, variant_count, tag);

        let mut case_arm: Vec<Option<usize>> = vec![None; variant_count];
        // True when the case block IS the routed arm's entry block
        // (filled at arm emission); false when it only jumps to a
        // wildcard's shared entry.
        let mut case_is_entry = vec![false; variant_count];
        let mut wildcard: Option<usize> = None;
        for (i, arm) in view.arms().enumerate() {
            match arm.discriminant {
                Discriminant::Variant(vidx) => {
                    let v = vidx as usize;
                    if v >= variant_count {
                        return Err(format!(
                            "eval_inst_match: arm variant index {vidx} out of range for the scrutinee enum"
                        ));
                    }
                    if arm.guard_count() != 0 {
                        return Err(
                            "eval_inst_match: variant arm carries guards (sema invariant)"
                                .to_string(),
                        );
                    }
                    if case_arm[v].is_none() {
                        case_arm[v] = Some(i);
                        case_is_entry[v] = true;
                        arm_entry[i] = Some(cases[v]);
                    }
                }
                Discriminant::Wildcard => {
                    if arm.guard_count() != 0 {
                        return Err(
                            "eval_inst_match: wildcard arm with guards requires an anon-struct scrutinee (sema E0131 gates this)"
                                .to_string(),
                        );
                    }
                    if wildcard.is_none() {
                        wildcard = Some(i);
                    }
                    // The wildcard covers every variant no earlier arm
                    // routed — all such cases share the wildcard's
                    // single entry block.
                    for routed in case_arm.iter_mut() {
                        if routed.is_none() {
                            *routed = Some(i);
                            arm_entry[i].get_or_insert_with(|| builder.create_block());
                        }
                    }
                }
                other => {
                    return Err(format!(
                        "eval_inst_match: {other:?} arm on an enum scrutinee is rejected by sema (E0131)"
                    ));
                }
            }
        }

        for (v, case) in cases.iter().enumerate() {
            if case_is_entry[v] {
                // The case block IS the routed arm's entry: it is sealed
                // and filled during arm emission below.
                continue;
            }
            builder.seal_block(*case);
            builder.switch_to_block(*case);
            match case_arm[v] {
                // Wildcard-routed: jump to the wildcard's shared entry.
                Some(i) => {
                    let entry = arm_entry[i].expect("a routed arm always has an entry block");
                    builder.ins().jump(entry, &[]);
                }
                // No arm routes here: with a wildcard present every case
                // is routed, so this is a compiler-invariant violation
                // (partial coverage without a wildcard is sema E0129).
                None => super::enums::invalid_tag_trap(builder),
            }
        }
        builder.seal_block(otherwise);
        builder.switch_to_block(otherwise);
        match wildcard {
            Some(i) => {
                let entry = *arm_entry[i].get_or_insert_with(|| builder.create_block());
                builder.ins().jump(entry, &[]);
            }
            None => super::enums::invalid_tag_trap(builder),
        }
        Ok(())
    }

    /// Anon-struct scrutinee: a linear chain of guard conjunctions over
    /// the arm list in written order. Every arm has one RECEIVING block
    /// — a guarded arm's first test block, an unguarded arm's body —
    /// and arm `i`'s failed guards branch to arm `i+1`'s receiver. The
    /// scrutinee block jumps to receiver 0; the first bare wildcard's
    /// body is the chain's fallthrough (sema E0129 guarantees one; a
    /// violated shape fails down to a defensive trap instead). Arms
    /// with an empty guard list cannot arrive (sema's anon-arm lowering
    /// guarantees guards; a bare wildcard is the fallthrough) — one
    /// reaching a chain slot is an invariant trap in `emit_guard_chain`.
    fn route_anon_dispatch(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        scrut_ty: TypeId,
        view: &MatchView<'_>,
        scrutinee_addr: Value,
        arm_entry: &mut [Option<Block>],
    ) -> Result<(), String> {
        let arm_count = view.len();
        let arms: Vec<MatchArmView<'_>> = view.arms().collect();
        for (i, arm) in arms.iter().enumerate() {
            if !matches!(arm.discriminant, Discriminant::Wildcard) {
                return Err(format!(
                    "eval_inst_match: {:?} arm on an anon-struct scrutinee is rejected by sema (E0131)",
                    arm.discriminant
                ));
            }
            arm_entry[i] = Some(builder.create_block());
        }

        // Receiver per arm: the test block for a guarded arm (created
        // lazily), the body block for an unguarded one.
        let guarded: Vec<bool> = arms.iter().map(|a| a.guard_count() > 0).collect();
        let mut recv: Vec<Block> = Vec::with_capacity(arm_count);
        for (i, entry) in arm_entry.iter().enumerate() {
            let block = if guarded[i] {
                builder.create_block()
            } else {
                entry.expect("every anon arm entry was just created")
            };
            recv.push(block);
        }
        builder.ins().jump(recv[0], &[]);

        // The failure target of the LAST arm exists only when that arm
        // is guarded — under sema's E0129 wildcard guarantee that is a
        // W0005-dead arm after the wildcard, so the trap (and the dead
        // arm's whole test chain) is unreachable by construction. It is
        // sealed and filled AFTER the loop: the last compare's `brif` is
        // a predecessor, and Cranelift rejects edges into sealed blocks.
        let last_trap = if guarded[arm_count - 1] {
            Some(builder.create_block())
        } else {
            None
        };

        for (i, arm) in arms.iter().enumerate() {
            if !guarded[i] {
                continue;
            }
            let body = arm_entry[i].expect("every anon arm entry was just created");
            let fail = if i + 1 < arm_count {
                recv[i + 1]
            } else {
                last_trap.expect("a guarded last arm always created the trap block")
            };
            builder.seal_block(recv[i]);
            builder.switch_to_block(recv[i]);
            Self::emit_guard_chain(builder, ctx, scrutinee_addr, scrut_ty, arm, body, fail)?;
        }

        if let Some(trap) = last_trap {
            builder.seal_block(trap);
            builder.switch_to_block(trap);
            super::enums::invalid_tag_trap(builder);
        }
        Ok(())
    }

    /// int/bool/str scrutinee: a linear chain of literal compares over
    /// the arm list in written order — the same receiver-block shape as
    /// `route_anon_dispatch`, with the scrutinee value as the compare
    /// operand. Routing mirrors the enum case: the FIRST arm claiming a
    /// literal wins (duplicates are W0005-dead and receive no chain
    /// edge), the first bare wildcard is the fallthrough receiver, and
    /// every arm after that wildcard is W0005-dead ("a previous `_` arm
    /// matches everything", sema). The scrutinee block jumps into the
    /// chain head; each failed compare branches to the next routed
    /// arm's receiver (a literal arm's test block, the wildcard's body
    /// block); the chain tail is the wildcard body, or — only when no
    /// wildcard exists, i.e. a fully `true`/`false`-covered bool — a
    /// defensive trap unreachable by construction (sema E0129
    /// guarantees a wildcard for int/str scrutinees).
    fn route_literal_dispatch(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        view: &MatchView<'_>,
        scrut: LiteralScrut,
        arm_entry: &mut [Option<Block>],
    ) -> Result<(), String> {
        let mut seen: Vec<Discriminant> = Vec::new();
        let mut fallthrough: Option<usize> = None;
        // Routed literal arms in written order: (arm index, test block).
        let mut tests: Vec<(usize, Block)> = Vec::new();
        for (i, arm) in view.arms().enumerate() {
            match arm.discriminant {
                Discriminant::Wildcard => {
                    if arm.guard_count() != 0 {
                        return Err(
                            "eval_inst_match: wildcard arm with guards requires an anon-struct scrutinee (sema E0131 gates this)"
                                .to_string(),
                        );
                    }
                    // First wildcard claims the fallthrough; everything
                    // after it is W0005-dead and stays unrouted.
                    if fallthrough.is_none() {
                        fallthrough = Some(i);
                        arm_entry[i] = Some(builder.create_block());
                    }
                }
                disc @ (Discriminant::Int(_) | Discriminant::Bool(_) | Discriminant::Str(_)) => {
                    if arm.guard_count() != 0 {
                        return Err(
                            "eval_inst_match: literal arm carries guards (sema invariant)"
                                .to_string(),
                        );
                    }
                    if fallthrough.is_some() || seen.contains(&disc) {
                        continue;
                    }
                    seen.push(disc);
                    arm_entry[i] = Some(builder.create_block());
                    tests.push((i, builder.create_block()));
                }
                Discriminant::Variant(_) => {
                    return Err(
                        "eval_inst_match: variant arm on a literal scrutinee is rejected by sema (E0131)"
                            .to_string(),
                    );
                }
            }
        }

        // No literal arms: pure-wildcard shape — straight into the body.
        let Some(&(_, head)) = tests.first() else {
            let w = fallthrough.expect(
                "eval_inst_match: no live arm in a literal scrutinee's chain (sema E0129 gates this)",
            );
            let entry = arm_entry[w].expect("the fallthrough arm was routed");
            builder.ins().jump(entry, &[]);
            return Ok(());
        };
        builder.ins().jump(head, &[]);

        // Chain tail: the wildcard body, or — only when no wildcard
        // exists (bool full coverage) — a fresh trap block, sealed and
        // filled AFTER the loop: the last compare's `brif` is a
        // predecessor, and Cranelift rejects edges into sealed blocks.
        let (tail, tail_trap) = match fallthrough {
            Some(w) => (arm_entry[w].expect("the fallthrough arm was routed"), None),
            None => {
                let trap = builder.create_block();
                (trap, Some(trap))
            }
        };

        for (k, &(i, test)) in tests.iter().enumerate() {
            let fail = if k + 1 < tests.len() {
                tests[k + 1].1
            } else {
                tail
            };
            builder.seal_block(test);
            builder.switch_to_block(test);
            let arm = view.arm(i);
            let cmp = Self::emit_literal_compare(builder, ctx, scrut, &arm.discriminant)?;
            let body = arm_entry[i].expect("a routed literal arm always has an entry block");
            builder.ins().brif(cmp, body, &[], fail, &[]);
        }

        if let Some(trap) = tail_trap {
            builder.seal_block(trap);
            builder.switch_to_block(trap);
            super::enums::invalid_tag_trap(builder);
        }
        Ok(())
    }

    /// One literal-arm compare: the scrutinee scalar against the arm's
    /// immediate (`icmp_imm eq`, bool lowered to I8 with 0/1), or the
    /// scrutinee's extracted content pair against the literal's static
    /// `.rodata` pair through the runtime's bytewise `ryo_str_eq` — the
    /// exact `emit_field_eq` str-arm convention. Returns the i8 equality
    /// feeding the chain's `brif`.
    fn emit_literal_compare(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        scrut: LiteralScrut,
        disc: &Discriminant,
    ) -> Result<Value, String> {
        match (scrut, disc) {
            (LiteralScrut::Scalar(v), Discriminant::Int(lit)) => {
                Ok(builder.ins().icmp_imm_s(IntCC::Equal, v, *lit))
            }
            (LiteralScrut::Scalar(v), Discriminant::Bool(b)) => {
                Ok(builder.ins().icmp_imm_s(IntCC::Equal, v, i64::from(*b)))
            }
            (LiteralScrut::Str { ptr, len }, Discriminant::Str(lit_id)) => {
                let lit = Self::emit_str_lit_pair(builder, ctx, *lit_id)?;
                Self::emit_ryo_str_eq(builder, ctx, (ptr, len), lit)
            }
            (_, other) => Err(format!(
                "eval_inst_match: {other:?} arm on a literal scrutinee of another kind is rejected by sema (E0131)"
            )),
        }
    }

    /// A str literal's static `.rodata` (ptr, len) content pair: the
    /// `emit_str_literal_fat` triple (cap == 0 is the static sentinel, so
    /// the extraction passes it through unspilled) — the literal side of
    /// the `emit_field_eq` str-arm convention.
    fn emit_str_lit_pair(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        id: StringId,
    ) -> Result<(Value, Value), String> {
        let ValueRepr::Str { ptr, len, cap } = Self::emit_str_literal_fat(builder, ctx, id)? else {
            unreachable!("emit_str_literal_fat always returns the str triple");
        };
        Self::emit_fat_bytes_ptr_len(builder, ctx, ptr, len, cap, None)
    }

    /// Bytewise str content equality through the runtime's
    /// `ryo_str_eq(ptr, len, ptr, len) -> bool` — the M9.1
    /// `emit_field_eq` str-arm call convention, shared by literal-arm
    /// chains and anon-arm str literal guards.
    fn emit_ryo_str_eq(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        lhs: (Value, Value),
        rhs: (Value, Value),
    ) -> Result<Value, String> {
        let eq_ref = Self::declare_runtime_fn(
            ctx,
            builder,
            "ryo_str_eq",
            &[ctx.int_type, types::I64, ctx.int_type, types::I64],
            &[types::I8],
        )?;
        let call = builder.ins().call(eq_ref, &[lhs.0, lhs.1, rhs.0, rhs.1]);
        Ok(builder.inst_results(call)[0])
    }

    /// One guarded arm's conjunction: guards are field-equality
    /// compares ANDed by short-circuit — each failed compare branches
    /// to `fail` (the next arm's test entry, or the chain's
    /// fallthrough); every guard passing branches to the arm's `body`.
    /// The field offset comes from the scrutinee's anon-struct shape.
    fn emit_guard_chain(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        scrutinee_addr: Value,
        scrut_ty: TypeId,
        arm: &MatchArmView<'_>,
        body: Block,
        fail: Block,
    ) -> Result<(), String> {
        let guards: Vec<(u32, LitPattern)> = arm.guards().collect();
        if guards.is_empty() {
            // Invariant (sema anon-arm lowering): every non-fallthrough
            // anon arm carries at least one literal guard.
            unreachable!("anon match arm with an empty guard list reached codegen");
        }
        for (k, (field_index, lit)) in guards.iter().enumerate() {
            let offset = Self::arm_payload_field_offset(
                ctx.pool,
                scrut_ty,
                &Discriminant::Wildcard,
                *field_index,
            )?;
            let cmp = Self::emit_arm_guard_compare(builder, ctx, scrutinee_addr, offset, lit)?;
            if k + 1 == guards.len() {
                builder.ins().brif(cmp, body, &[], fail, &[]);
            } else {
                let next = builder.create_block();
                builder.ins().brif(cmp, next, &[], fail, &[]);
                builder.seal_block(next);
                builder.switch_to_block(next);
            }
        }
        Ok(())
    }

    /// Compare one anon-scrutinee field against a literal guard,
    /// returning the i8 equality value. Int and bool literals lower to a
    /// scalar load plus `icmp`; str literals load the field's fat triple
    /// and compare content bytewise through the runtime's `ryo_str_eq`
    /// — the exact `emit_field_eq` str-arm convention (the field's own
    /// address is the SSO inline home).
    fn emit_arm_guard_compare(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        scrutinee_addr: Value,
        field_offset: u32,
        lit: &LitPattern,
    ) -> Result<Value, String> {
        let off =
            i32::try_from(field_offset).map_err(|_| "match guard field offset exceeds i32")?;
        match lit {
            LitPattern::Int(v) => {
                let field_val =
                    builder
                        .ins()
                        .load(ctx.int_type, MemFlagsData::trusted(), scrutinee_addr, off);
                let lit_val = builder.ins().iconst(ctx.int_type, *v);
                Ok(builder.ins().icmp(IntCC::Equal, field_val, lit_val))
            }
            LitPattern::Bool(b) => {
                let field_val =
                    builder
                        .ins()
                        .load(types::I8, MemFlagsData::trusted(), scrutinee_addr, off);
                let lit_val = builder.ins().iconst(types::I8, i64::from(*b));
                Ok(builder.ins().icmp(IntCC::Equal, field_val, lit_val))
            }
            LitPattern::Str(id) => {
                let field_addr = if off == 0 {
                    scrutinee_addr
                } else {
                    builder.ins().iadd_imm_s(scrutinee_addr, i64::from(off))
                };
                let ptr = builder
                    .ins()
                    .load(ctx.int_type, MemFlagsData::trusted(), field_addr, 0);
                let len = builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), field_addr, 8);
                let cap = builder
                    .ins()
                    .load(types::I64, MemFlagsData::trusted(), field_addr, 16);
                let field =
                    Self::emit_fat_bytes_ptr_len(builder, ctx, ptr, len, cap, Some(field_addr))?;
                let lit_pair = Self::emit_str_lit_pair(builder, ctx, *id)?;
                Self::emit_ryo_str_eq(builder, ctx, field, lit_pair)
            }
        }
    }

    /// The byte offset of one payload field for an arm's discriminated
    /// shape: the variant's payload fields for a variant arm (offsets
    /// absolute from the enum base, the `EnumVariantView` contract) or
    /// the struct's fields for an anon-pattern arm (a Wildcard coverage
    /// placeholder over an anon-struct scrutinee).
    fn arm_payload_field_offset(
        pool: &InternPool,
        scrutinee_ty: TypeId,
        discriminant: &Discriminant,
        field_index: u32,
    ) -> Result<u32, String> {
        let offset = match discriminant {
            Discriminant::Variant(vidx) => {
                let view = pool.enum_view(scrutinee_ty);
                view.variants()
                    .nth(*vidx as usize)
                    .and_then(|variant| variant.fields.get(field_index as usize))
                    .map(|f| f.offset)
            }
            Discriminant::Wildcard => pool
                .struct_view(scrutinee_ty)
                .fields
                .get(field_index as usize)
                .map(|f| f.offset),
            other => {
                return Err(format!(
                    "eval_inst_match: {other:?} arm carries no payload fields"
                ));
            }
        };
        offset.ok_or_else(|| {
            format!("eval_inst_match: payload field index {field_index} out of range for the arm's shape")
        })
    }
}
