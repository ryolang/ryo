//! `match` expression codegen (M12) — enum tag dispatch with wildcard
//! routing, anonymous-struct guard chains (int/bool literal guards),
//! per-arm payload binding moves/copies, and scalar/void/aggregate merges.
//!
//! The lowering is split across the Flow-block driver:
//!
//! * `emit_switch` (the Switch terminator handler) lowers ONLY the
//!   dispatch: the scrutinee into its canonical home, a pre-allocated
//!   result slot (scalar/aggregate shapes — every value merge rides
//!   memory, never a block param), and the per-scrutinee-kind routing
//!   into the pre-mapped Flow arm blocks (`route_*` below).
//! * Each Flow arm block's prologue (`match_arm_prologue`) fires the
//!   arm-start payload drops and binds payloads from the scrutinee slot.
//!   Shared direct-catch bindings deep-copy; all others move.
//! * A falling arm's `Goto` terminator (`match_arm_fallthrough`) writes
//!   the arm result into the match result slot (scalar values store;
//!   aggregates go through the canonical per-type aggregate writes) and
//!   jumps to the join — firing the arm-body frees first, exactly where
//!   the old arm loop fired them.
//! * The join block's prologue (`match_join_value`) restores the fact
//!   tables and caches the result repr on the match instruction, so the
//!   join's steps read it through their own entry points.
//!
//! The lowering shape:
//!
//! 1. The scrutinee is evaluated into its canonical home — enum and
//!    anon-struct scrutinees into their stack slot (`ValueRepr::Enum` /
//!    `ValueRepr::Struct` address), int/bool scrutinees as the loaded
//!    scalar — and its address/value is the payload base for every arm.
//!    (A str scrutinee extracts its transient `(ptr, len)`.)
//! 2. A result slot is allocated BEFORE the dispatch so the slot
//!    address is a constant dominating every arm and the merge.
//! 3. Dispatch per scrutinee kind: enums load the i32 tag and switch
//!    (`emit_tag_dispatch`), routing each variant's case to the FIRST
//!    arm covering it (written order wins; W0005-dead duplicates stay
//!    unrouted) and the wildcard arm's entry to the switch's
//!    `otherwise`. Anon-struct scrutinees lower to a linear chain of
//!    guard conjunctions: each failed compare short-circuits to the
//!    next arm's test block, and the final wildcard arm is the
//!    fallthrough. Literal scrutinees (int/bool/str) lower to the same
//!    chain shape over `Discriminant::Int/Bool/Str` arms.
//! 4. Diverging arms (never-typed bodies, return/break/continue) are
//!    their blocks' own terminators — they leave no merge edge and
//!    write nothing.
//!
//! The post-merge drop of an owned needs-drop scrutinee TEMPORARY needs
//! no codegen of its own: the ownership pass's anon-temp machinery
//! schedules the whole-value Free anchored after this Match inst (the
//! scrutinee's single consumer), and the end-of-statement sweep lands
//! it here — post-merge on every path that reaches the merge, with the
//! `freed_at` table guaranteeing one emission per schedule entry.

use super::{
    Codegen, FunctionContext, STR_SLOT_SIZE, SlotMarks, ValueRepr, cranelift_type_for,
    is_enum_type, is_struct_type,
};
use cranelift::codegen::ir::{MemFlagsData, StackSlotData, StackSlotKind};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::ast::LitPattern;
use ryo_core::flow::Terminator as FlowTerm;
use ryo_core::tir::{Discriminant, MatchArmView, MatchBodyView, MatchView, TirRef};
use ryo_core::types::{InternPool, StringId, TypeId, TypeKind};

/// The merge shape of a lowered match, derived from its result TypeId.
#[derive(Clone, Copy)]
pub(crate) enum MatchShape {
    /// Void match: arms jump bare; the join caches a dummy.
    Void,
    /// Scalar match (int/bool/float): every value arm stores into the
    /// 8-byte result slot; the join loads and caches it.
    Scalar(types::Type),
    /// Every arm diverges: no join, no result (the continuation block
    /// is unreachable).
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
pub(crate) enum ScrutineeHome {
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

/// Per-match Flow-driver record: the fact/slot marks (taken at the
/// pre-step hook, before the scrutinee steps), the scrutinee home, the
/// merge shape, the pre-allocated result slot address, and per-arm
/// token-cursor offsets. Consumed by the arm prologues, the arm
/// fallthroughs, and the join prologue.
pub(crate) struct MatchRecord {
    pub(crate) outer_facts_mark: usize,
    pub(crate) scope_mark: usize,
    pub(crate) slot_marks: SlotMarks,
    /// Filled by `emit_switch`.
    pub(crate) scrut: Option<ScrutineeHome>,
    /// Filled by `emit_switch`.
    pub(crate) shape: Option<MatchShape>,
    /// Filled by `emit_switch` for scalar/aggregate shapes.
    pub(crate) result_slot_addr: Option<Value>,
    /// Per-arm token-cursor offsets into `flow.token_places(match)`.
    pub(crate) token_offsets: Vec<usize>,
}

impl MatchRecord {
    pub(crate) fn new(outer_facts_mark: usize, scope_mark: usize, slot_marks: SlotMarks) -> Self {
        MatchRecord {
            outer_facts_mark,
            scope_mark,
            slot_marks,
            scrut: None,
            shape: None,
            result_slot_addr: None,
            token_offsets: Vec::new(),
        }
    }

    /// Read the record's filled fields (everything the Switch handler
    /// stamps) — every consumer runs after `emit_switch`.
    fn filled(&self) -> (ScrutineeHome, MatchShape, Option<Value>) {
        (
            self.scrut
                .expect("match record filled by the switch handler"),
            self.shape
                .expect("match record filled by the switch handler"),
            self.result_slot_addr,
        )
    }
}

impl<M: Module> Codegen<M> {
    /// Lower the dispatch of a typed `TirTag::Match` (M12) — the
    /// `Switch` terminator handler. The scrutinee is a step in this
    /// block (already cached, except the str extraction which emits
    /// here); the routing lands in the pre-mapped Flow arm blocks. Arm
    /// bodies, arm drops/bindings, result writes, and the merge are the
    /// driver's job (arm prologues, arm fallthroughs, join prologue).
    pub(crate) fn emit_switch(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<(), String> {
        let view = ctx.tir.match_view(r);
        if view.is_empty() {
            return Err("eval_inst_match: match with no arms reached codegen".to_string());
        }
        let shape = Self::match_shape(ctx, view.ty)?;
        let scrut_ty = ctx.tir.inst(view.scrutinee).ty;

        // Create the match record with marks taken NOW — after this
        // block's own steps (the scrutinee's condition-position writes
        // belong INSIDE the marks, exactly where the old emitter took
        // them when the match statement was reached).
        ctx.match_records.insert(
            r,
            super::match_::MatchRecord::new(
                ctx.range_facts_undo.len(),
                ctx.assigned_log.len(),
                Self::take_slot_marks(ctx),
            ),
        );

        // 1. Scrutinee into its canonical home; the payload base for
        // every arm. Whether the scrutinee is an owned temporary or a
        // place matters only for the scheduled post-merge drop (the
        // end-of-statement sweep fires the ownership pass's Free).
        let scrut = match ctx.pool.kind(scrut_ty) {
            TypeKind::Enum
            | TypeKind::DeclaredError
            | TypeKind::ErrorSet
            | TypeKind::ErrorUnion => {
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

        // 2. Result slot BEFORE the dispatch: the slot address is
        // defined in the scrutinee block, which dominates every arm
        // and the merge — the dominance hazard (a value produced in
        // one arm read from the merge) cannot arise because arms write
        // memory and the merge reads it.
        let result_slot_addr = match shape {
            MatchShape::Scalar(_) => {
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                Some(builder.ins().stack_addr(ctx.int_type, slot, 0))
            }
            MatchShape::Aggregate(ty) => {
                let slot = match ctx.pool.kind(ty) {
                    TypeKind::Str | TypeKind::Bytes => builder.create_sized_stack_slot(
                        StackSlotData::new(StackSlotKind::ExplicitSlot, STR_SLOT_SIZE, 3),
                    ),
                    TypeKind::Struct | TypeKind::AnonStruct => Self::struct_slot(builder, ctx, ty),
                    TypeKind::Enum
                    | TypeKind::DeclaredError
                    | TypeKind::ErrorSet
                    | TypeKind::ErrorUnion => Self::enum_slot(builder, ctx, ty),
                    other => unreachable!("match_shape rejected {other:?} as an aggregate result"),
                };
                Some(builder.ins().stack_addr(ctx.int_type, slot, 0))
            }
            MatchShape::Void | MatchShape::Never => None,
        };

        // Per-arm token offsets: one owner token per by-move binding,
        // in arm/binding order (the sema owner-token convention).
        let mut token_offsets = Vec::with_capacity(view.len());
        let mut cursor = 0usize;
        for arm in view.arms() {
            token_offsets.push(cursor);
            cursor += arm.bindings().filter(|b| !ctx.pool.is_copy(b.ty)).count();
        }

        // 3. Stamp the record created at the pre-step hook.
        {
            let rec = ctx
                .match_records
                .get_mut(&r)
                .expect("match record created at the pre-step hook");
            rec.scrut = Some(scrut);
            rec.shape = Some(shape);
            rec.result_slot_addr = result_slot_addr;
            rec.token_offsets = token_offsets;
        }

        // No join (every arm diverged): the continuation is an
        // unreachable block whose ExprStmt step still evaluates this
        // match — cache the merge-unreachable dummy now (the value the
        // old arm loop returned), so that evaluation hits the memo.
        // Emitted BEFORE the routing below, which leaves its last
        // internal block filled.
        // The scrutinee block's at-terminator bucket (value-step
        // anchors with no enclosing statement — e.g. a last-use read
        // inside the scrutinee expression) fires here: the scrutinee
        // home is established, including any lazily materialized
        // slot-out producer the anchors' targets may alias.
        let sw_block = ctx.flow.term_block_of(r).expect("match is a switch");
        Self::fire_at_terminator(builder, ctx, sw_block)?;
        let sw_join = match ctx.flow.term(sw_block) {
            FlowTerm::Switch { join, .. } => *join,
            _ => unreachable!("match is a switch"),
        };
        if sw_join.is_none() {
            let dummy_ty = match shape {
                MatchShape::Scalar(cl_ty) => cl_ty,
                MatchShape::Void | MatchShape::Never => types::I8,
                MatchShape::Aggregate(_) => {
                    unreachable!("an aggregate-typed match always has a value arm")
                }
            };
            let dummy = builder.ins().iconst(dummy_ty, 0);
            Self::cache_repr(ctx, r, ValueRepr::Scalar(dummy));
        }

        // 4. Routing: the arm entries are the pre-mapped Flow arm
        // blocks (Switch arms in wire order, wildcard included).
        let FlowTerm::Switch { arms: rng, .. } = ctx.flow.term(sw_block) else {
            unreachable!("match is a switch")
        };
        let arm_entry: Vec<Option<Block>> = rng
            .as_range()
            .map(|i| {
                let ab = ryo_core::flow::BlockId(ctx.flow.extra[i]);
                Some(ctx.flow_blocks[ab.0 as usize])
            })
            .collect();
        match scrut {
            ScrutineeHome::Slot(addr) if is_enum_type(scrut_ty, ctx.pool) => {
                Self::route_enum_dispatch(builder, ctx, scrut_ty, &view, addr, &arm_entry)?;
            }
            ScrutineeHome::Slot(addr) => {
                debug_assert!(
                    is_struct_type(scrut_ty, ctx.pool),
                    "slot-homed scrutinee must be an enum or anon struct"
                );
                Self::route_anon_dispatch(builder, ctx, scrut_ty, &view, addr, &arm_entry)?;
            }
            ScrutineeHome::Scalar(v) => {
                Self::route_literal_dispatch(
                    builder,
                    ctx,
                    &view,
                    LiteralScrut::Scalar(v),
                    &arm_entry,
                )?;
            }
            ScrutineeHome::Str { ptr, len } => {
                Self::route_literal_dispatch(
                    builder,
                    ctx,
                    &view,
                    LiteralScrut::Str { ptr, len },
                    &arm_entry,
                )?;
            }
        }
        Ok(())
    }

    /// Emit the scheduled drops for unbound needs-drop payload fields
    /// at the start of a consuming match arm.
    pub(crate) fn emit_arm_start_drops(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        payload: &ryo_core::flow::cleanup::PayloadRelease,
    ) -> Result<(), String> {
        let scrut = ctx
            .match_records
            .get(&payload.match_inst)
            .and_then(|record| record.scrut)
            .ok_or_else(|| "payload release has no scrutinee storage".to_string())?;
        let ScrutineeHome::Slot(addr) = scrut else {
            return Err("payload release requires aggregate storage".into());
        };
        for field in &payload.fields {
            Self::emit_field_drop(builder, ctx, addr, field.offset, field.ty)?;
        }
        Ok(())
    }

    /// The Flow arm block's prologue: arm-start payload drops (the
    /// consuming arm's unbound needs-drop fields) and payload binding.
    /// Ordinary matches move; shared direct catches deep-copy so their
    /// original error remains valid. Only slot-homed scrutinees carry
    /// payload drops; literal scrutinees bind nothing.
    pub(crate) fn match_arm_prologue(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        arm_idx: usize,
    ) -> Result<(), String> {
        let view = ctx.tir.match_view(r);
        let arms: Vec<MatchArmView<'_>> = view.arms().collect();
        let arm = arms[arm_idx];
        let scrut_ty = ctx.tir.inst(view.scrutinee).ty;
        let (scrut, token_offset) = {
            let rec = ctx
                .match_records
                .get(&r)
                .expect("match record filled by the switch handler");
            (
                rec.scrut.expect("scrutinee home filled"),
                rec.token_offsets[arm_idx],
            )
        };
        let ScrutineeHome::Slot(addr) = scrut else {
            return Ok(());
        };
        // Binding prologue: per-binding move out of the scrutinee slot.
        // Copy bindings pass no owner token — sema's convention emits
        // one token per by-move binding only.
        let tokens = ctx.tir.match_by_move_owner_refs(r, ctx.pool);
        let mut cursor = token_offset;
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
                let token = tokens[cursor];
                cursor += 1;
                Some(token)
            };
            if b.ty == ctx.pool.void() {
                // Void uses a dummy zero scalar; its payload has no bytes
                // to load, copy, or store.
                let val = builder.ins().iconst(ctx.int_type, 0);
                let var = builder.declare_var(ctx.int_type);
                builder.def_var(var, val);
                Self::write_slot(
                    &mut ctx.range_facts,
                    &mut ctx.range_facts_undo,
                    b.name,
                    None,
                );
                Self::write_slot(&mut ctx.locals, &mut ctx.locals_undo, b.name, Some(var));
                if let Some(token) = token {
                    ctx.aggregate_binding_storage.insert(token, var);
                    Self::cache_repr(ctx, token, ValueRepr::Scalar(val));
                }
                continue;
            }
            if token.is_some() && ctx.tir.is_direct_catch_match(r) {
                Self::bind_field_deep_copy(builder, ctx, b.name, field_addr, b.ty, token)?;
            } else {
                Self::bind_field_move(builder, ctx, b.name, field_addr, b.ty, token)?;
            }
        }
        Ok(())
    }

    /// A falling match arm's `Goto` terminator: write the arm result
    /// into the match's result slot (scalar store / canonical aggregate
    /// writes) and jump to the join. Fires the arm-body frees BEFORE
    /// the jump — after it the end-of-statement sweep would run in the
    /// merge block, where arm-anchored entries have no cached repr and
    /// can never fire (leak). Diverging arms have no Goto edge and
    /// never reach here.
    pub(crate) fn match_arm_fallthrough(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        arm_idx: usize,
        from: ryo_core::flow::BlockId,
    ) -> Result<(), String> {
        let view = ctx.tir.match_view(r);
        let arms: Vec<MatchArmView<'_>> = view.arms().collect();
        let arm = arms[arm_idx];
        let sw_block = ctx.flow.term_block_of(r).expect("match is a switch");
        let join = match ctx.flow.term(sw_block) {
            FlowTerm::Switch { join: Some(j), .. } => *j,
            _ => unreachable!("a falling arm implies a join"),
        };
        let jb = ctx.flow_blocks[join.0 as usize];
        let (_scrut, shape, slot_addr) = {
            let rec = ctx
                .match_records
                .get(&r)
                .expect("match record filled by the switch handler");
            rec.filled()
        };
        // The arm-end block's at-terminator bucket (arm-body value
        // anchors — e.g. a moved payload's token free, whose producer
        // the result write may lazily materialize) fires after the
        // body evaluation and result write, before the join jump.
        let fire_join_bucket =
            |builder: &mut FunctionBuilder,
             ctx: &mut FunctionContext<'_, M>|
             -> Result<(), String> { Self::fire_at_terminator(builder, ctx, from) };
        match arm.body {
            MatchBodyView::Expr(e) => {
                match shape {
                    MatchShape::Aggregate(ty) => {
                        let slot_addr = slot_addr.expect("aggregate shape has a result slot");
                        Self::emit_match_result_write(builder, ctx, slot_addr, ty, e)?;
                        // Arm-body frees fire from the at-terminator
                        // bucket (mechanical, fired by the driver before
                        // this handler); the promotion family keeps its
                        // own hook.
                        fire_join_bucket(builder, ctx)?;
                        builder.ins().jump(jb, &[]);
                    }
                    MatchShape::Scalar(_) => {
                        // The arm body expression was a step in this
                        // block — its value is cached.
                        let v = Self::eval_inst(builder, ctx, e)?;
                        // Arm-body frees fire from the at-terminator
                        // bucket (mechanical, fired by the driver before
                        // this handler); the promotion family keeps its
                        // own hook.
                        let slot_addr = slot_addr.expect("scalar shape has a result slot");
                        builder
                            .ins()
                            .store(MemFlagsData::trusted(), v, slot_addr, 0);
                        fire_join_bucket(builder, ctx)?;
                        builder.ins().jump(jb, &[]);
                    }
                    MatchShape::Void => {
                        let _ = Self::eval_inst(builder, ctx, e)?;
                        // Arm-body frees fire from the at-terminator
                        // bucket (mechanical, fired by the driver before
                        // this handler); the promotion family keeps its
                        // own hook.
                        fire_join_bucket(builder, ctx)?;
                        builder.ins().jump(jb, &[]);
                    }
                    MatchShape::Never => {
                        unreachable!("never match has no falling arm")
                    }
                }
            }
            MatchBodyView::Stmts(_) => {
                // Falling statement arms exist only in void matches
                // (sema E0135 gates value matches); the arm's statement
                // steps already swept their frees.
                match shape {
                    MatchShape::Void => {
                        fire_join_bucket(builder, ctx)?;
                        builder.ins().jump(jb, &[]);
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
            }
        }
        Ok(())
    }

    /// The match join block's prologue (after the fact/slot restores,
    /// which the driver runs first): cache the merged result value on
    /// the match instruction so the join's steps read it through their
    /// own entry points. Scalars load the result slot; aggregates read
    /// the slot (a fat triple load for str/bytes, the slot address for
    /// memory-first structs/enums); void caches a dummy.
    pub(crate) fn match_join_value(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<(), String> {
        let (_scrut, shape, slot_addr) = {
            let rec = ctx
                .match_records
                .get(&r)
                .expect("match record filled by the switch handler");
            rec.filled()
        };
        match shape {
            MatchShape::Scalar(cl_ty) => {
                let slot_addr = slot_addr.expect("scalar shape has a result slot");
                let v = builder
                    .ins()
                    .load(cl_ty, MemFlagsData::trusted(), slot_addr, 0);
                Self::cache_repr(ctx, r, ValueRepr::Scalar(v));
            }
            MatchShape::Void => {
                let v = builder.ins().iconst(ctx.int_type, 0);
                Self::cache_repr(ctx, r, ValueRepr::Scalar(v));
            }
            MatchShape::Never => {
                // No join exists for an all-diverging match — the
                // driver never routes a block here.
                unreachable!("never match has no join block")
            }
            MatchShape::Aggregate(ty) => {
                // An aggregate-typed match always has at least one
                // falling-through value arm (sema's join types the match
                // by its value arms), so the merge is reachable and the
                // slot written on every path here.
                let slot_addr = slot_addr.expect("aggregate shape has a result slot");
                let repr = Self::match_result_repr(builder, ctx, slot_addr, ty)?;
                Self::cache_repr(ctx, r, repr);
            }
        }
        Ok(())
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
            TypeKind::Enum
            | TypeKind::DeclaredError
            | TypeKind::ErrorSet
            | TypeKind::ErrorUnion => {
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
            TypeKind::Enum
            | TypeKind::DeclaredError
            | TypeKind::ErrorSet
            | TypeKind::ErrorUnion => Ok(ValueRepr::Enum { addr: slot_addr }),
            other => Err(format!(
                "eval_inst_match: {other:?} is not a lowered aggregate match result"
            )),
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
            | TypeKind::Enum
            | TypeKind::DeclaredError
            | TypeKind::ErrorSet
            | TypeKind::ErrorUnion => Ok(MatchShape::Aggregate(ty)),
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
    /// invalid-tag trap, exactly the M11 invariant semantics. The case
    /// blocks are pure dispatch: the arm bodies are the pre-mapped Flow
    /// arm blocks, filled by the driver.
    fn route_enum_dispatch(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        scrut_ty: TypeId,
        view: &MatchView<'_>,
        scrutinee_addr: Value,
        arm_entry: &[Option<Block>],
    ) -> Result<(), String> {
        let variant_count = ctx.pool.enum_view(scrut_ty).variants().count();
        let tag = builder
            .ins()
            .load(types::I32, MemFlagsData::trusted(), scrutinee_addr, 0);
        let (cases, otherwise) = Self::emit_tag_dispatch(builder, variant_count, tag);

        let mut case_arm: Vec<Option<usize>> = vec![None; variant_count];
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
            // Pure dispatch: jump to the routed arm's pre-mapped Flow
            // block (the driver fills it), or trap on a violated-
            // exhaustiveness path (with a wildcard present every case
            // is routed, so `None` is a compiler invariant).
            builder.seal_block(*case);
            builder.switch_to_block(*case);
            match case_arm[v] {
                Some(i) => {
                    let entry = arm_entry[i].expect("a routed arm always has an entry block");
                    builder.ins().jump(entry, &[]);
                }
                None => super::enums::invalid_tag_trap(builder),
            }
        }
        builder.seal_block(otherwise);
        builder.switch_to_block(otherwise);
        match wildcard {
            Some(i) => {
                let entry = *arm_entry[i]
                    .as_ref()
                    .expect("the wildcard arm's mapped block exists");
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
        arm_entry: &[Option<Block>],
    ) -> Result<(), String> {
        let arm_count = view.len();
        let arms: Vec<MatchArmView<'_>> = view.arms().collect();
        for arm in arms.iter() {
            if !matches!(arm.discriminant, Discriminant::Wildcard) {
                return Err(format!(
                    "eval_inst_match: {:?} arm on an anon-struct scrutinee is rejected by sema (E0131)",
                    arm.discriminant
                ));
            }
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
        arm_entry: &[Option<Block>],
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
                    // after it is W0005-dead and stays unrouted. Its
                    // entry is the pre-mapped Flow arm block.
                    if fallthrough.is_none() {
                        fallthrough = Some(i);
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

        // Arm views materialized once: the chain below indexes them by
        // arm position (`view.arm(i)` rescans the wire from the start
        // per call — O(A²) for A arms).
        let arms: Vec<MatchArmView<'_>> = view.arms().collect();
        for (k, &(i, test)) in tests.iter().enumerate() {
            let fail = if k + 1 < tests.len() {
                tests[k + 1].1
            } else {
                tail
            };
            builder.seal_block(test);
            builder.switch_to_block(test);
            let arm = arms[i];
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

/// The compare operand(s) of a literal-arm chain, fixed by the
/// scrutinee's type kind: int/bool compare the loaded scalar against an
/// immediate; str compares the extracted content pair against each
/// literal through the runtime's `ryo_str_eq`.
#[derive(Clone, Copy)]
enum LiteralScrut {
    Scalar(Value),
    Str { ptr: Value, len: Value },
}
