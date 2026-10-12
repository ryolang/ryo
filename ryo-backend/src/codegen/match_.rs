//! `match` expression codegen (M12) — enum tag dispatch with wildcard
//! routing, anonymous-struct guard chains (int/bool literal guards),
//! per-arm payload binding moves, and scalar/void merges.
//!
//! The typed `TirTag::Match` arrives fully resolved (Task 4's
//! [`MatchView`]): every arm is a [`Discriminant`] plus typed
//! [`MatchBinding`][ryo_core::tir::MatchBinding]s, anon arms carrying
//! their field-equality guards on the wire. The lowering shape:
//!
//! 1. The scrutinee is evaluated into its canonical home — enum and
//!    anon-struct scrutinees into their stack slot (`ValueRepr::Enum` /
//!    `ValueRepr::Struct` address), int/bool scrutinees as the loaded
//!    scalar — and its address/value is the payload base for every arm.
//! 2. A merge block is created up front: scalar results ride one block
//!    param (the `enums.rs` Eq precedent), void matches none. Aggregate
//!    result types (str/bytes/view/struct/enum) are Task 11 — a
//!    `debug_assert` plus the internal error path, never a silent
//!    miscompile.
//! 3. Dispatch per scrutinee kind: enums load the i32 tag and switch
//!    (`emit_tag_dispatch`), routing each variant's case to the FIRST
//!    arm covering it (written order wins; W0005-dead duplicates stay
//!    unrouted) and the wildcard arm's entry to the switch's `otherwise`
//!    — full coverage without a wildcard keeps the invalid-tag trap.
//!    Anon-struct scrutinees lower to a linear chain of guard
//!    conjunctions: each failed compare short-circuits to the next
//!    arm's test block, and the final wildcard arm is the fallthrough.
//!    Literal-arm chains on int/bool/str scrutinees and str literal
//!    guards land in Task 10; both route through the shared compare
//!    helper so that task extends rather than duplicates.
//! 4. Each arm runs in its own block under the undo-log slot-table
//!    scope (`emit_scoped_body`'s discipline — bindings are arm-local):
//!    arm-start payload drops ([`MatchArmDrop`][ryo_core::ownership::MatchArmDrop],
//!    the consuming arm's unbound needs-drop fields), then the binding
//!    prologue through the shared `bind_field_move` (fat triples,
//!    struct/enum slot copies, scalar loads — the M10 destructure
//!    logic), then the body. Expression arms evaluate and jump to the
//!    merge with the value (scalar) or without (void); statement arms
//!    go through `emit_scoped_body` and its `Terminator` sense — only
//!    non-terminating arms jump. Diverging arms (never-typed bodies,
//!    return/break/continue) leave no merge edge; when EVERY arm
//!    diverges the merge is unreachable and traps.
//!
//! Task 11 completes the memory story: the post-merge drop of an owned
//! needs-drop scrutinee TEMPORARY, and every aggregate-result match.
//! This task's tests use non-needs-drop payloads, so nothing leaks
//! today; a needs-drop scrutinee temporary reaching this code simply
//! has its scheduled drop swept after the enclosing statement (the
//! merge block — the correct post-merge position by construction).

use super::{
    Codegen, FunctionContext, Terminator, cranelift_type_for, is_enum_type, is_struct_type,
};
use cranelift::codegen::ir::{BlockArg, MemFlagsData};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::ast::LitPattern;
use ryo_core::tir::{Discriminant, MatchArmView, MatchBodyView, MatchView, TirRef};
use ryo_core::types::{InternPool, TypeId, TypeKind};

/// The merge shape of a lowered match, derived from its result TypeId.
enum MatchShape {
    /// Void match: the merge block carries no param; arms jump bare.
    Void,
    /// Scalar match (int/bool/float): one merge block param.
    Scalar(types::Type),
    /// Every arm diverges: no merge param, and the merge is unreachable.
    Never,
}

/// Where the scrutinee value lives after evaluation.
#[derive(Clone, Copy)]
enum ScrutineeHome {
    /// Enum / anon-struct scrutinee: the address of its stack slot.
    Slot(Value),
    /// int/bool scrutinee: the loaded scalar. Literal-arm dispatch on
    /// scalar scrutinees is Task 10; today only the pure-wildcard shape
    /// lowers here, and no binding or guard reads the value.
    Scalar,
}

impl<M: Module> Codegen<M> {
    /// Lower a typed `TirTag::Match` (M12); the `eval_inst` dispatch
    /// target. Returns the scalar merge value (the block param), or a
    /// dummy for void/never shapes — aggregate results are Task 11.
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
        // place matters only for the post-merge drop — Task 11.
        let scrut = match ctx.pool.kind(scrut_ty) {
            TypeKind::Enum => {
                ScrutineeHome::Slot(Self::eval_inst_enum(builder, ctx, view.scrutinee)?)
            }
            TypeKind::AnonStruct => {
                ScrutineeHome::Slot(Self::eval_inst_struct(builder, ctx, view.scrutinee)?)
            }
            TypeKind::Int | TypeKind::Bool => {
                let _ = Self::eval_inst(builder, ctx, view.scrutinee)?;
                ScrutineeHome::Scalar
            }
            TypeKind::Str => {
                return Err(
                    "eval_inst_match: str scrutinees (literal-arm chains) are not yet lowered (Task 10)"
                        .to_string(),
                )
            }
            other => {
                return Err(format!(
                    "eval_inst_match: scrutinee type kind {other:?} is not matchable (sema E0136 gates this)"
                ))
            }
        };

        // 2. Merge block up front; scalar results ride one block param.
        let merge = builder.create_block();
        if let MatchShape::Scalar(cl_ty) = shape {
            builder.append_block_param(merge, cl_ty);
        }

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
            ScrutineeHome::Scalar => {
                Self::route_scalar_dispatch(builder, &view, &mut arm_entry)?;
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
            // value for scalar shapes); diverging arms leave no edge.
            let falls_through = match arm.body {
                MatchBodyView::Expr(e) => {
                    let body_ty = ctx.tir.inst(e).ty;
                    let v = Self::eval_inst(builder, ctx, e)?;
                    if ctx.pool.is_never(body_ty) {
                        false
                    } else {
                        match shape {
                            MatchShape::Scalar(_) => {
                                builder.ins().jump(merge, &[BlockArg::Value(v)]);
                            }
                            MatchShape::Void | MatchShape::Never => {
                                builder.ins().jump(merge, &[]);
                            }
                        }
                        true
                    }
                }
                MatchBodyView::Stmts(_) => {
                    let stmts: Vec<TirRef> = arm.body.stmts().collect();
                    let term = Self::emit_scoped_body(builder, ctx, &stmts, false)?;
                    if term == Terminator::None {
                        match shape {
                            MatchShape::Void => {
                                builder.ins().jump(merge, &[]);
                            }
                            MatchShape::Scalar(_) => {
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
                    } else {
                        false
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

        let value = match shape {
            MatchShape::Scalar(cl_ty) if merge_unreachable => builder.ins().iconst(cl_ty, 0),
            MatchShape::Scalar(_) => builder.block_params(merge)[0],
            MatchShape::Void => builder.ins().iconst(ctx.int_type, 0),
            MatchShape::Never => builder.ins().iconst(types::I8, 0),
        };
        Ok(value)
    }

    /// The merge shape for a match result type: void, scalar (one
    /// Cranelift type), or never. Aggregate results (str/bytes/view/
    /// struct/enum — anything `cranelift_type_for` rejects) are Task
    /// 11: a `debug_assert` pins the contract, and the error path
    /// reports instead of silently miscompiling.
    fn match_shape(ctx: &FunctionContext<'_, M>, ty: TypeId) -> Result<MatchShape, String> {
        match ctx.pool.kind(ty) {
            TypeKind::Void => Ok(MatchShape::Void),
            TypeKind::Int | TypeKind::Bool | TypeKind::Float => Ok(MatchShape::Scalar(
                cranelift_type_for(ty, ctx.pool, ctx.int_type),
            )),
            TypeKind::Never => Ok(MatchShape::Never),
            other => {
                debug_assert!(
                    matches!(
                        other,
                        TypeKind::Str
                            | TypeKind::Bytes
                            | TypeKind::View(_)
                            | TypeKind::Struct
                            | TypeKind::AnonStruct
                            | TypeKind::Enum
                    ),
                    "aggregate match results land in Task 11"
                );
                Err(format!(
                    "eval_inst_match: aggregate match result type {other:?} is not yet lowered (Task 11 owns aggregate-result matches)"
                ))
            }
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
    /// with an empty guard list cannot arrive (Task 6: anon arms carry
    /// guards; a bare wildcard is the fallthrough) — one reaching a
    /// chain slot is an invariant trap in `emit_guard_chain`.
    fn route_anon_dispatch(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
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
        // is guarded — i.e. exactly the shapes sema's E0129 wildcard
        // guarantee excludes (the trap is unreachable by construction).
        let last_trap = if guarded[arm_count - 1] {
            let trap = builder.create_block();
            builder.seal_block(trap);
            builder.switch_to_block(trap);
            super::enums::invalid_tag_trap(builder);
            Some(trap)
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
        Ok(())
    }

    /// int/bool scrutinee today: every arm must be a bare wildcard (at
    /// most one is reachable — duplicates are W0005-dead), and the
    /// scrutinee block jumps straight into the first arm's body.
    /// Literal-arm chains on scalar scrutinees land in Task 10.
    fn route_scalar_dispatch(
        builder: &mut FunctionBuilder,
        view: &MatchView<'_>,
        arm_entry: &mut [Option<Block>],
    ) -> Result<(), String> {
        for (i, arm) in view.arms().enumerate() {
            if !matches!(arm.discriminant, Discriminant::Wildcard) || arm.guard_count() != 0 {
                return Err(format!(
                    "eval_inst_match: literal-arm dispatch on a scalar scrutinee is not yet lowered (Task 10) — arm {i} is not a bare wildcard"
                ));
            }
        }
        arm_entry[0] = Some(builder.create_block());
        let entry = arm_entry[0].expect("just created");
        builder.ins().jump(entry, &[]);
        Ok(())
    }

    /// One guarded arm's conjunction: guards are field-equality
    /// compares ANDed by short-circuit — each failed compare branches
    /// to `fail` (the next arm's test entry, or the chain's
    /// fallthrough); every guard passing branches to the arm's `body`.
    /// The field offset comes from the scrutinee's anon-struct shape.
    fn emit_guard_chain(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
        scrutinee_addr: Value,
        scrut_ty: TypeId,
        arm: &MatchArmView<'_>,
        body: Block,
        fail: Block,
    ) -> Result<(), String> {
        let guards: Vec<(u32, LitPattern)> = arm.guards().collect();
        if guards.is_empty() {
            // Invariant (Task 6): every non-fallthrough anon arm carries
            // at least one literal guard.
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
    /// scalar load plus `icmp`; str literals need a runtime strcmp and
    /// land in Task 10 (sema-legal today — routed to a clear internal
    /// error). Task 10's scrutinee literal chains extend this helper
    /// rather than duplicating it.
    fn emit_arm_guard_compare(
        builder: &mut FunctionBuilder,
        ctx: &FunctionContext<'_, M>,
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
            LitPattern::Str(_) => {
                Err("eval_inst_match: str literal guards are not yet lowered (Task 10)".to_string())
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
