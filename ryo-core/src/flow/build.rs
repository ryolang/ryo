//! [`Flow`] construction from a finished [`Tir`].
//!
//! The builder performs exactly one walk over the TIR forest, emitting
//! steps in the same post-order evaluation sequence the backend uses,
//! and cutting blocks at control-flow constructs. Each TIR instruction
//! becomes exactly one of: a [`Step`] in exactly one block, a
//! [`Terminator`] payload (match/if/loop/return/…), or a registered
//! by-move owner token (never evaluated by the backend).

use std::collections::HashMap;
use std::ops::Range;

use super::{
    BlockId, Flow, FlowBlock, InstMeta, Place, PlaceId, PlaceWrite, Step, StepFacts, Terminator,
    WriteKind,
};
use crate::extra::ExtraRange;
use crate::tir::{MatchBodyView, Tir, TirRef, TirTag};
use crate::types::{InternPool, StringId, TypeId};

pub(super) struct FlowBuilder<'a> {
    tir: &'a Tir,
    pool: &'a InternPool,
    blocks: Vec<FlowBlock>,
    /// Under construction; `None` until the block is terminated.
    terms: Vec<Option<Terminator>>,
    /// Whether a block has a predecessor edge (set when terminators
    /// are wired). Dead continuations created after a terminator have
    /// none — `close_region` gives them `Unreachable`, not a join edge.
    has_pred: Vec<bool>,
    steps: Vec<Step>,
    places: Vec<Place>,
    /// Variable-length payloads (switch arm block lists).
    extra: Vec<u32>,
    scopes: Vec<Vec<(StringId, PlaceId)>>,
    loops: Vec<LoopFrame>,
    /// Dense per-instruction metadata (single arena alloc, R3).
    inst_meta: Vec<InstMeta>,
    token_places: HashMap<TirRef, Box<[PlaceId]>>,
    /// Block → terminator payload instruction (see [`Flow::term_payload`]).
    term_payload: Vec<Option<TirRef>>,
    /// Arm blocks recorded per `IfStmt` (see [`super::BranchArms`]).
    branch_arms: HashMap<TirRef, super::BranchArms>,
    /// Flat (place, write inst) pairs, counting-sorted into per-place
    /// lineage at finish — one Vec total instead of one Vec per place.
    writes_flat: Vec<(PlaceId, TirRef)>,
    /// Monotonic emission-order counter: every stamped instruction
    /// (step or terminator payload) gets a unique value reflecting the
    /// build sequence. Distinct from the step index — terminators emit
    /// no step but still consume an order.
    next_order: u32,
    cur: BlockId,
}

struct LoopFrame {
    header: BlockId,
    exit: BlockId,
}

pub(super) fn build_flow(tir: &Tir, pool: &InternPool) -> Flow {
    let arena_len = tir.instructions.len();
    let fb = FlowBuilder {
        tir,
        pool,
        blocks: Vec::new(),
        terms: Vec::new(),
        has_pred: Vec::new(),
        // Steps and places never exceed the instruction count (each
        // instruction is stepped or registered at most once) — reserve
        // upfront so the small-function case pays no growth reallocs.
        steps: Vec::with_capacity(arena_len),
        places: Vec::with_capacity(arena_len),
        extra: Vec::new(),
        scopes: vec![Vec::new()],
        loops: Vec::new(),
        inst_meta: vec![InstMeta::default(); arena_len],
        token_places: HashMap::new(),
        term_payload: Vec::new(),
        branch_arms: HashMap::new(),
        writes_flat: Vec::new(),
        next_order: 0,
        cur: BlockId::NONE,
    };
    fb.run()
}

impl FlowBuilder<'_> {
    fn run(mut self) -> Flow {
        let entry = self.new_block(false);
        self.cur = entry;

        // Params are the outermost scope, identified by index.
        for (i, p) in self.tir.params.iter().enumerate() {
            let place = self.push_place(Place::Param {
                index: i as u32,
                name: p.name,
                ty: p.ty,
            });
            self.scopes[0].push((p.name, place));
        }

        let tir = self.tir;
        for stmt in tir.body_stmts_view() {
            self.emit_stmt(stmt);
        }

        // Void functions (and `main`) fall off the end; sema's E0036
        // guarantees non-void bodies end in an explicit return.
        if !self.terminated(self.cur) {
            self.set_term(self.cur, Terminator::ReturnVoid, None);
        }

        self.finish(entry)
    }

    // ---------- block plumbing ----------

    fn new_block(&mut self, unreachable: bool) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(FlowBlock {
            // Set on the first step emission, not here: blocks created
            // up front in a batch (match arms, elif condition blocks)
            // would otherwise all record the same creation-time start
            // and their ranges would overlap once steps land.
            steps_start: u32::MAX,
            steps_len: 0,
            unreachable,
        });
        self.terms.push(None);
        self.has_pred.push(false);
        self.term_payload.push(None);
        id
    }

    fn terminated(&self, b: BlockId) -> bool {
        self.terms[b.0 as usize].is_some()
    }

    /// Terminate `b`. `payload` is the structural TIR instruction the
    /// terminator implements (IfStmt/WhileLoop/Return/…), when one
    /// exists — it is stamped with the block for coverage queries.
    fn set_term(&mut self, b: BlockId, t: Terminator, payload: Option<TirRef>) {
        if self.terminated(b) {
            // Error-shaped bodies can place a second terminator where
            // one already stands (e.g. an unreachable sentinel plus a
            // return in a broken destructure). Sema has already
            // diagnosed the unit; keep the Flow IR well-formed by moving
            // the continuation into a fresh dead block.
            self.cur = self.new_block(true);
            return self.set_term(self.cur, t, payload);
        }
        match &t {
            Terminator::Switch { arms, join, .. } => {
                for i in arms.as_range() {
                    let target = self.extra[i] as usize;
                    self.has_pred[target] = true;
                }
                if let Some(j) = join {
                    self.has_pred[j.0 as usize] = true;
                }
            }
            _ => {
                for target in term_targets(&t) {
                    self.has_pred[target.0 as usize] = true;
                }
            }
        }
        self.terms[b.0 as usize] = Some(t);
        if let Some(inst) = payload {
            self.stamp_order(inst);
            self.inst_meta[inst.index()].term_block = Some(b.0);
            self.term_payload[b.0 as usize] = Some(inst);
        }
    }

    /// Close a region whose last live block is `b`: a live block jumps
    /// to `target`; a dead continuation (no predecessors — e.g. the
    /// unreachable block created after a diverging call) gets
    /// `Unreachable` so no spurious edge reaches the join.
    fn close_region(&mut self, b: BlockId, target: BlockId) {
        if !self.terminated(b) {
            if self.has_pred[b.0 as usize] {
                self.set_term(b, Terminator::Goto { target }, None);
            } else {
                self.set_term(b, Terminator::Unreachable, None);
            }
        }
    }

    /// The block receiving steps: after a terminator the continuation
    /// is a fresh unreachable-entry block.
    fn open_block(&mut self) -> BlockId {
        if self.terminated(self.cur) {
            self.cur = self.new_block(true);
        }
        self.cur
    }

    fn stamp_order(&mut self, inst: TirRef) {
        self.inst_meta[inst.index()].order = Some(self.next_order);
        self.next_order += 1;
    }

    fn push_step(&mut self, inst: TirRef, facts: StepFacts) {
        let b = self.open_block();
        debug_assert!(
            self.inst_meta[inst.index()].step.is_none()
                && self.inst_meta[inst.index()].term_block.is_none(),
            "instruction {:?} emitted twice",
            inst
        );
        self.inst_meta[inst.index()].step = Some(self.steps.len() as u32);
        self.inst_meta[inst.index()].block = Some(b.0);
        self.stamp_order(inst);
        self.steps.push(Step { inst, facts });
        let blk = &mut self.blocks[b.0 as usize];
        if blk.steps_len == 0 {
            blk.steps_start = self.steps.len() as u32 - 1;
        }
        blk.steps_len += 1;
    }

    fn push_place(&mut self, p: Place) -> PlaceId {
        let id = PlaceId(self.places.len() as u32);
        self.places.push(p);
        id
    }

    fn record_write(&mut self, place: PlaceId, inst: TirRef) {
        self.writes_flat.push((place, inst));
    }

    // ---------- scopes ----------

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind(&mut self, name: StringId, place: PlaceId) {
        self.scopes
            .last_mut()
            .expect("scope stack")
            .push((name, place));
    }

    fn resolve(&self, name: StringId) -> Option<PlaceId> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev())
            .find(|(n, _)| *n == name)
            .map(|(_, p)| *p)
    }

    // ---------- statements ----------

    fn emit_stmts(&mut self, stmts: &[TirRef]) {
        for &stmt in stmts {
            self.emit_stmt(stmt);
        }
    }

    fn emit_stmt(&mut self, stmt: TirRef) {
        // Stamp the single-parent consumer edge for the statement's
        // operands, exactly as `emit_expr` does for expression
        // internals (see `Flow::parent_of`).
        self.tir.walk_operands(stmt, &mut |_p, child, kind| {
            if kind == crate::tir::ChildKind::Operand {
                self.inst_meta[child.index()].parent = Some(stmt.raw());
            }
        });
        let inst = *self.tir.inst(stmt);
        match inst.tag {
            TirTag::VarDecl => {
                let v = self.tir.var_decl_view(stmt);
                self.emit_expr(v.initializer);
                let place = self.push_place(Place::Local {
                    decl: stmt,
                    name: v.name,
                    ty: inst.ty,
                    mutable: v.mutable,
                });
                self.inst_meta[stmt.index()].local = Some(place);
                self.record_write(place, stmt);
                self.push_step(
                    stmt,
                    StepFacts {
                        write: Some(PlaceWrite {
                            place,
                            kind: WriteKind::Init,
                        }),
                        ..StepFacts::default()
                    },
                );
                self.bind(v.name, place);
            }
            TirTag::Assign => {
                let v = self.tir.assign_view(stmt);
                self.emit_expr(v.value);
                let place = self
                    .resolve(v.name)
                    .expect("assign to unresolved binding (post-sema TIR)");
                self.record_write(place, stmt);
                self.push_step(
                    stmt,
                    StepFacts {
                        write: Some(PlaceWrite {
                            place,
                            kind: WriteKind::Replace,
                        }),
                        ..StepFacts::default()
                    },
                );
            }
            TirTag::CompoundAssign => {
                let v = self.tir.compound_assign_view(stmt);
                self.emit_expr(v.value);
                let place = self
                    .resolve(v.name)
                    .expect("compound-assign to unresolved binding (post-sema TIR)");
                self.push_step(
                    stmt,
                    StepFacts {
                        read: Some(place),
                        write: Some(PlaceWrite {
                            place,
                            kind: WriteKind::Mutate,
                        }),
                        ..StepFacts::default()
                    },
                );
            }
            TirTag::FieldAssign | TirTag::CompoundFieldAssign => {
                let (target, value, is_compound) = match inst.tag {
                    TirTag::FieldAssign => {
                        let v = self.tir.field_assign_view(stmt);
                        (v.target, v.value, false)
                    }
                    _ => {
                        let v = self.tir.compound_field_assign_view(stmt);
                        (v.target, v.value, true)
                    }
                };
                self.emit_expr(target);
                self.emit_expr(value);
                let (base, field) = self
                    .resolve_field_target(target)
                    .expect("field-assign target is not a place projection");
                self.push_step(
                    stmt,
                    StepFacts {
                        read: is_compound.then_some(base),
                        write: Some(PlaceWrite {
                            place: base,
                            kind: WriteKind::FieldReplace { field },
                        }),
                        ..StepFacts::default()
                    },
                );
            }
            TirTag::IfStmt => self.build_if(stmt),
            TirTag::WhileLoop => self.build_while(stmt),
            TirTag::ForRange => self.build_for(stmt),
            TirTag::Destructure => {
                let v = self.tir.destructure_view(stmt);
                self.emit_expr(v.rhs);
                let refs = self.tir.destructure_bound_owner_refs(stmt);
                let names: Vec<StringId> = v.fields.iter().filter_map(|f| f.bind).collect();
                debug_assert_eq!(refs.len(), names.len(), "token/binding mismatch");
                self.register_tokens(stmt, &refs, &names);
                // Destructure bindings persist in the current scope.
                let places = self.token_places_of(stmt).to_vec();
                for place in places {
                    if let Place::Token { name, .. } = self.places[place.0 as usize] {
                        self.bind(name, place);
                    }
                }
                self.push_step(stmt, StepFacts::default());
            }
            TirTag::Return => match inst.data {
                crate::tir::TirData::UnOp(value) => {
                    self.emit_expr(value);
                    self.set_term(self.cur, Terminator::Return { value }, Some(stmt));
                }
                _ => unreachable!("Return carries UnOp"),
            },
            TirTag::ReturnVoid => self.set_term(self.cur, Terminator::ReturnVoid, Some(stmt)),
            TirTag::Break | TirTag::Continue => {
                // Outside a loop, sema has already emitted E0024/E0025
                // and the unit will not codegen; keep the analysis
                // well-formed with an unreachable terminator rather
                // than aborting the whole diagnostic pass.
                let term = match (inst.tag, self.loops.last()) {
                    (TirTag::Break, Some(f)) => Terminator::Break { target: f.exit },
                    (TirTag::Continue, Some(f)) => Terminator::Continue { target: f.header },
                    _ => Terminator::Unreachable,
                };
                self.set_term(self.cur, term, Some(stmt));
            }
            TirTag::Unreachable => {
                self.set_term(self.cur, Terminator::Unreachable, Some(stmt));
            }
            TirTag::Match => {
                // Sema ExprStmt-wraps statement-level matches; a bare
                // Match statement is tolerated the same way (the match
                // itself is the Switch terminator — no step).
                self.emit_expr(stmt);
            }
            TirTag::ExprStmt => match inst.data {
                crate::tir::TirData::UnOp(e) => {
                    self.emit_expr(e);
                    // Marks the statement boundary for consumers that
                    // sweep at statement end; carries no facts of its own.
                    self.push_step(stmt, StepFacts::default());
                }
                _ => unreachable!("ExprStmt carries UnOp"),
            },
            other => {
                // Hand-built or future statement shapes that produce a
                // value (a bare call, a literal, …): evaluate and mark
                // the statement boundary, exactly like ExprStmt.
                debug_assert!(
                    !matches!(other, TirTag::Return | TirTag::ReturnVoid),
                    "returns are handled above"
                );
                self.emit_expr(stmt);
                if self.inst_meta[stmt.index()].step.is_none() {
                    // Value-producing statements were stepped by the
                    // evaluation itself; structural ones (match/if)
                    // need the boundary marker.
                    self.push_step(stmt, StepFacts::default());
                }
            }
        }
    }

    // ---------- expressions ----------

    /// Evaluate `r` and its operands into the current block. After a
    /// match or a diverging call, `self.cur` is the continuation block
    /// (join, or an unreachable block).
    fn emit_expr(&mut self, r: TirRef) {
        // Stamp the single-parent consumer edge for every operand
        // (the TIR is tree-shaped): consumers of Flow facts read
        // `parent_of` instead of re-walking TIR operand trees. Only
        // `Operand`-kind edges count — body statements are reachable,
        // not consumed.
        self.tir.walk_operands(r, &mut |_p, child, kind| {
            if kind == crate::tir::ChildKind::Operand {
                self.inst_meta[child.index()].parent = Some(r.raw());
            }
        });
        let inst = *self.tir.inst(r);
        match inst.tag {
            // Control constructs are position-agnostic: sema lowers
            // some calls (assert) to an IfStmt in expression position,
            // and a match is equally at home in an initializer or a
            // statement. The builders cut blocks wherever they stand.
            TirTag::Match => self.build_match(r),
            TirTag::IfStmt => self.build_if(r),
            TirTag::WhileLoop => self.build_while(r),
            TirTag::ForRange => self.build_for(r),
            TirTag::BoolAnd | TirTag::BoolOr => {
                // Short-circuit control flow made explicit: the RHS is
                // reachable only through the taken edge. `lhs` is
                // evaluated in the current block, which the Branch then
                // terminates; the merged value is a step in the join
                // block (its operands dominate it by emission order).
                let is_and = inst.tag == TirTag::BoolAnd;
                let (lhs, rhs) = match inst.data {
                    crate::tir::TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("BoolAnd/BoolOr must carry TirData::BinOp"),
                };
                self.emit_expr(lhs);
                let rhs_block = self.new_block(false);
                let join = self.new_block(false);
                let (then_, else_) = if is_and {
                    (rhs_block, join)
                } else {
                    (join, rhs_block)
                };
                self.set_term(
                    self.cur,
                    Terminator::Branch {
                        cond: lhs,
                        then_,
                        else_,
                    },
                    None,
                );
                self.cur = rhs_block;
                self.emit_expr(rhs);
                self.close_region(self.cur, join);
                self.cur = join;
                self.push_step(r, StepFacts::default());
            }
            TirTag::Call if self.pool.is_never(inst.ty) => {
                let view = self.tir.call_view(r);
                for arg in &view.args {
                    self.emit_expr(arg);
                }
                let next = self.new_block(true);
                self.set_term(self.cur, Terminator::Call { call: r, next }, Some(r));
                self.cur = next;
            }
            _ => {
                let tir = self.tir;
                tir.walk_operands(r, &mut |_p, child, kind| {
                    debug_assert!(
                        kind == crate::tir::ChildKind::Operand,
                        "expression insts announce operands only"
                    );
                    self.emit_expr(child);
                });
                let facts = self.facts_for(r);
                self.push_step(r, facts);
            }
        }
    }

    /// Storage facts for a value-producing instruction.
    fn facts_for(&mut self, r: TirRef) -> StepFacts {
        let inst = *self.tir.inst(r);
        let mut facts = StepFacts::default();
        match inst.data {
            crate::tir::TirData::Var(name) => {
                facts.read = self.resolve(name);
            }
            crate::tir::TirData::FieldAccess { object, .. } => {
                facts.read = self.resolve_view_base(object);
            }
            crate::tir::TirData::Slice { base, .. } => {
                facts.view = Some(self.mint_view(r, inst.ty, base));
            }
            _ => {}
        }
        match inst.tag {
            TirTag::ToView => {
                if let crate::tir::TirData::UnOp(base) = inst.data {
                    facts.read = self.resolve_view_base(base);
                    facts.view = Some(self.mint_view(r, inst.ty, base));
                } else {
                    unreachable!("ToView carries UnOp")
                }
            }
            TirTag::ViewAsOwner => {
                if let crate::tir::TirData::UnOp(operand) = inst.data {
                    facts.read = self.resolve_view_base(operand);
                    if self.pool.needs_drop(inst.ty) {
                        self.mint_temp(r, inst.ty);
                    }
                } else {
                    unreachable!("ViewAsOwner carries UnOp")
                }
            }
            // `StrConst`/`BytesConst`/`DebugRepr` join the producer
            // set (G2): the ownership pass keys owner state by place,
            // so every tracked producer needs a temp place — a
            // literal's place is its storage identity even though
            // codegen materializes it inline.
            TirTag::StrConst
            | TirTag::BytesConst
            | TirTag::DebugRepr
            | TirTag::StrConcat
            | TirTag::BytesConcat
            | TirTag::Call
            | TirTag::StructLit
            | TirTag::EnumLit
                if self.pool.needs_drop(inst.ty) =>
            {
                self.mint_temp(r, inst.ty);
            }
            _ => {}
        }
        if inst.tag == TirTag::Call {
            let call = self.tir.call_view(r);
            let offset = self.extra.len() as u32;
            let push = matches!(self.pool.str(call.name), "str_push" | "bytes_push");
            for (index, arg) in call.args.iter().enumerate() {
                if (call.modes.get(index) == Some(crate::tir::ParamMode::Inout)
                    || (push && index == 0))
                    && let Some(place) = self.resolve_view_base(arg)
                {
                    self.extra.push(place.0);
                }
            }
            let len = self.extra.len() as u32 - offset;
            if len != 0 {
                facts.mutations = Some(ExtraRange { offset, len });
            }
        }
        facts
    }

    fn mint_temp(&mut self, producer: TirRef, ty: TypeId) -> PlaceId {
        let place = self.push_place(Place::Temp { producer, ty });
        self.inst_meta[producer.index()].temp = Some(place);
        place
    }

    fn mint_view(&mut self, producer: TirRef, ty: TypeId, base: TirRef) -> PlaceId {
        let base_place = self.resolve_view_base(base).unwrap_or(PlaceId(u32::MAX));
        self.push_place(Place::View {
            producer,
            base: base_place,
            ty,
        })
    }

    /// Resolve an operand to the place it reads, through projections,
    /// view conversions, and anonymous temporaries. The explicit
    /// analogue of the ownership pass's `projection_root`.
    fn resolve_view_base(&self, r: TirRef) -> Option<PlaceId> {
        let inst = self.tir.inst(r);
        match inst.data {
            crate::tir::TirData::Var(name) => self.resolve(name),
            crate::tir::TirData::FieldAccess { object, .. } => self.resolve_view_base(object),
            crate::tir::TirData::UnOp(operand)
                if matches!(inst.tag, TirTag::ViewAsOwner | TirTag::ToView) =>
            {
                self.resolve_view_base(operand)
            }
            crate::tir::TirData::Slice { base, .. } => self.resolve_view_base(base),
            _ => self.inst_meta.get(r.index()).and_then(|m| m.temp),
        }
    }

    /// Resolve a `FieldAssign` target chain to (base place, field).
    fn resolve_field_target(&self, r: TirRef) -> Option<(PlaceId, u32)> {
        match self.tir.inst(r).data {
            crate::tir::TirData::FieldAccess {
                object,
                field_index,
            } => self
                .resolve_view_base(object)
                .map(|base| (base, field_index)),
            _ => None,
        }
    }

    // ---------- control constructs ----------

    fn build_if(&mut self, stmt: TirRef) {
        let view = self.tir.if_stmt_view(stmt);
        self.emit_expr(view.cond);

        let then_block = self.new_block(false);
        // One condition block per elif, an else/fallthrough block, and
        // the join. Ids are allocated up front so the cascade's
        // terminators can reference them before the bodies are built.
        let elif_blocks: Vec<BlockId> = (0..view.elif_branches.len())
            .map(|_| self.new_block(false))
            .collect();
        let else_block = self.new_block(false);
        let join = self.new_block(false);

        let first_else = elif_blocks.first().copied().unwrap_or(else_block);
        self.set_term(
            self.cur,
            Terminator::Branch {
                cond: view.cond,
                then_: then_block,
                else_: first_else,
            },
            Some(stmt),
        );
        let mut elif_branch_blocks: Vec<BlockId> = Vec::with_capacity(view.elif_branches.len());

        self.cur = then_block;
        self.push_scope();
        self.emit_stmts(&view.then_stmts);
        self.pop_scope();
        self.close_region(self.cur, join);

        for (i, elif) in view.elif_branches.iter().enumerate() {
            let cond_block = elif_blocks[i];
            self.cur = cond_block;
            self.emit_expr(elif.cond);
            // The block holding this elif's Branch: the condition block
            // itself for a simple condition, the condition's join block
            // when the condition is a match/`and`/`or`. Recorded —
            // never reconstructed from the condition entry.
            let branch_block = self.cur;
            elif_branch_blocks.push(branch_block);
            let body_block = self.new_block(false);
            let next = elif_blocks.get(i + 1).copied().unwrap_or(else_block);
            self.set_term(
                self.cur,
                Terminator::Branch {
                    cond: elif.cond,
                    then_: body_block,
                    else_: next,
                },
                None,
            );
            self.cur = body_block;
            self.push_scope();
            self.emit_stmts(&elif.body);
            self.pop_scope();
            self.close_region(self.cur, join);
        }

        // Else arm — or a synthetic empty fallthrough for else-less ifs
        // (arm-start drops and dead drops need a real block to fire in).
        self.cur = else_block;
        if let Some(else_stmts) = &view.else_stmts {
            self.push_scope();
            self.emit_stmts(else_stmts);
            self.pop_scope();
        }
        self.close_region(self.cur, join);

        self.branch_arms.insert(
            stmt,
            super::BranchArms {
                then_block,
                elif_blocks: elif_branch_blocks,
                else_block,
                join,
            },
        );

        self.cur = join;
    }

    fn build_while(&mut self, stmt: TirRef) {
        let view = self.tir.while_loop_view(stmt);
        let header = self.new_block(false);
        let body = self.new_block(false);
        let exit = self.new_block(false);

        self.set_term(
            self.cur,
            Terminator::While {
                loop_inst: stmt,
                header,
            },
            Some(stmt),
        );

        self.cur = header;
        self.emit_expr(view.cond);
        self.set_term(
            self.cur,
            Terminator::WhileTest {
                loop_inst: stmt,
                cond: view.cond,
                body,
                exit,
            },
            None,
        );

        self.loops.push(LoopFrame { header, exit });
        self.cur = body;
        self.push_scope();
        self.emit_stmts(&view.body);
        self.pop_scope();
        self.loops.pop();
        if !self.terminated(self.cur) {
            if self.has_pred[self.cur.0 as usize] {
                self.set_term(
                    self.cur,
                    Terminator::Backedge {
                        to: header,
                        inc: None,
                    },
                    None,
                );
            } else {
                // Dead join inside the body (every path returned/broke).
                self.set_term(self.cur, Terminator::Unreachable, None);
            }
        }

        self.cur = exit;
    }

    fn build_for(&mut self, stmt: TirRef) {
        let view = self.tir.for_range_view(stmt);
        let header = self.new_block(false);
        let body = self.new_block(false);
        let exit = self.new_block(false);
        let ty = self.tir.inst(stmt).ty;
        let var = self.push_place(Place::LoopVar {
            loop_inst: stmt,
            name: view.var_name,
            ty,
        });
        self.inst_meta[stmt.index()].loop_var = Some(var);

        // Bounds evaluate ONCE, in the pre-header: the backedge targets
        // the header directly, so bound steps must not live where the
        // backedge can reach them again.
        self.emit_expr(view.start);
        self.emit_expr(view.end);
        self.record_write(var, stmt);
        self.set_term(
            self.cur,
            Terminator::For {
                loop_inst: stmt,
                header,
            },
            Some(stmt),
        );

        self.cur = header;
        self.set_term(
            self.cur,
            Terminator::ForTest {
                loop_inst: stmt,
                var,
                end: view.end,
                body,
                exit,
            },
            None,
        );

        self.loops.push(LoopFrame { header, exit });
        self.cur = body;
        self.push_scope();
        self.bind(view.var_name, var);
        self.emit_stmts(&view.body);
        self.pop_scope();
        self.loops.pop();
        if !self.terminated(self.cur) {
            if self.has_pred[self.cur.0 as usize] {
                self.set_term(
                    self.cur,
                    Terminator::Backedge {
                        to: header,
                        inc: Some(var),
                    },
                    None,
                );
            } else {
                // Dead join inside the body (every path returned/broke/continued).
                self.set_term(self.cur, Terminator::Unreachable, None);
            }
        }

        self.cur = exit;
    }

    fn build_match(&mut self, stmt: TirRef) {
        let view = self.tir.match_view(stmt);
        let match_ty = view.ty;
        self.emit_expr(view.scrutinee);

        // By-move owner tokens (arm order, binding order, non-Copy),
        // named from the arm bindings — minted up front so arm scopes
        // can bind them while building.
        let tokens = self.tir.match_by_move_owner_refs(stmt, self.pool);
        let names: Vec<StringId> = view
            .arms()
            .flat_map(|arm| arm.bindings())
            .filter(|b| !self.pool.is_copy(b.ty))
            .map(|b| b.name)
            .collect();
        debug_assert_eq!(tokens.len(), names.len(), "token/binding mismatch");
        self.register_tokens(stmt, &tokens, &names);

        // Arm block ids up front (the Switch terminator references them).
        let arm_start = self.extra.len() as u32;
        let arm_blocks: Vec<BlockId> = (0..view.len()).map(|_| self.new_block(false)).collect();
        for &b in &arm_blocks {
            self.extra.push(b.0);
        }

        // Divergence per arm decides whether a join exists.
        let mut any_falls_through = false;
        for arm in view.arms() {
            let terminates = match arm.body {
                MatchBodyView::Expr(e) => self.pool.is_never(self.tir.inst(e).ty),
                MatchBodyView::Stmts(_) => {
                    let stmts: Vec<TirRef> = arm.body.stmts().collect();
                    self.tir.block_definitely_terminates(&stmts, self.pool)
                }
            };
            any_falls_through |= !terminates;
        }
        let join = any_falls_through.then(|| self.new_block(false));

        self.set_term(
            self.cur,
            Terminator::Switch {
                match_inst: stmt,
                scrutinee: view.scrutinee,
                arms: ExtraRange {
                    offset: arm_start,
                    len: self.extra.len() as u32 - arm_start,
                },
                join,
            },
            Some(stmt),
        );

        if !self.pool.is_never(match_ty) && match_ty != self.pool.void() {
            let place = self.push_place(Place::MatchResult {
                match_inst: stmt,
                ty: match_ty,
            });
            self.inst_meta[stmt.index()].match_result = Some(place);
        }

        let token_places = self.token_places_of(stmt).to_vec();
        let mut token_iter = token_places.iter().copied();
        for (i, arm) in view.arms().enumerate() {
            self.cur = arm_blocks[i];
            self.push_scope();
            for binding in arm.bindings() {
                if !self.pool.is_copy(binding.ty) {
                    let place = token_iter
                        .next()
                        .expect("token count mismatch with by-move bindings");
                    self.bind(binding.name, place);
                }
            }
            match arm.body {
                MatchBodyView::Expr(body) => {
                    self.emit_expr(body);
                }
                MatchBodyView::Stmts(_) => {
                    let stmts: Vec<TirRef> = arm.body.stmts().collect();
                    self.emit_stmts(&stmts);
                }
            }
            match join {
                Some(join) => self.close_region(self.cur, join),
                None => {
                    // The arm itself must have terminated; close any
                    // dead continuation its divergence created.
                    debug_assert!(
                        self.terminated(arm_blocks[i]),
                        "all-diverging match arm fell through"
                    );
                    if !self.terminated(self.cur) {
                        debug_assert!(
                            !self.has_pred[self.cur.0 as usize],
                            "live fall-through in an all-diverging match"
                        );
                        self.set_term(self.cur, Terminator::Unreachable, None);
                    }
                }
            }
            self.pop_scope();
        }
        debug_assert!(token_iter.next().is_none(), "unused by-move tokens");

        match join {
            Some(join) => self.cur = join,
            None => {
                // Every arm diverged: the continuation is unreachable.
                self.cur = self.new_block(true);
            }
        }
    }

    // ---------- tokens ----------

    /// Mint token places for a match/destructure instruction and record
    /// the inst → places mapping. `refs`/`names` are in arm/binding
    /// order, non-`Copy` only (the sema owner-token convention).
    fn register_tokens(&mut self, inst: TirRef, refs: &[TirRef], names: &[StringId]) {
        let places: Vec<PlaceId> = refs
            .iter()
            .zip(names)
            .map(|(&r, &name)| {
                let ty = self.tir.inst(r).ty;
                self.push_place(Place::Token { inst: r, name, ty })
            })
            .collect();
        self.token_places.insert(inst, places.into_boxed_slice());
    }

    fn token_places_of(&self, inst: TirRef) -> &[PlaceId] {
        self.token_places.get(&inst).map(|p| &p[..]).unwrap_or(&[])
    }

    /// Per-place write lineage: counting-sort the flat (place, write)
    /// pairs by place id (places are minted in emission order, so the
    /// sort is stable in write order).
    fn flatten_writes(
        place_count: &usize,
        writes_flat: &[(PlaceId, TirRef)],
    ) -> (Vec<Range<u32>>, Vec<TirRef>) {
        // Empty fast path: no writes — every place has an empty range.
        if writes_flat.is_empty() {
            return (vec![0u32..0; *place_count], Vec::new());
        }
        let mut place_write_ranges = vec![0u32..0; *place_count];
        let mut place_write_list = Vec::with_capacity(writes_flat.len());
        // One bucket vec serving as counts, then prefix offsets, then
        // the fill cursor — three roles, one allocation.
        let mut bucket = vec![0u32; *place_count];
        for &(place, _) in writes_flat {
            bucket[place.0 as usize] += 1;
        }
        let mut acc = 0u32;
        for (i, c) in bucket.iter_mut().enumerate() {
            let count = *c;
            place_write_ranges[i] = acc..acc + count;
            *c = acc;
            acc += count;
        }
        // Placeholder is never observed: the counting sort writes every
        // slot (slot count == pair count per place).
        place_write_list.resize(writes_flat.len(), TirRef::from_raw(1));
        for &(place, w) in writes_flat {
            let slot = bucket[place.0 as usize];
            bucket[place.0 as usize] += 1;
            place_write_list[slot as usize] = w;
        }
        (place_write_ranges, place_write_list)
    }

    // ---------- finish ----------

    /// The cascade-entry block → IfStmt reverse map (see
    /// [`Flow::branch_if`]). Every `IfStmt` stamps its first `Branch`'s
    /// block in `inst_meta.term_block` while lowering.
    fn branch_if_map(&self) -> HashMap<BlockId, TirRef> {
        self.branch_arms
            .keys()
            .filter_map(|&if_inst| {
                self.inst_meta[if_inst.index()]
                    .term_block
                    .map(|b| (BlockId(b), if_inst))
            })
            .collect()
    }

    fn finish(mut self, entry: BlockId) -> Flow {
        // Computed before the struct is consumed by moves below.
        let branch_if = self.branch_if_map();
        // Single-block bodies (no control flow — the common case) need
        // no reachability pass or predecessor buckets: the block has
        // no successors and is trivially reachable. Skipping the graph
        // machinery keeps the small-function case at a handful of
        // allocations (R3).
        if self.blocks.len() == 1 {
            let (place_write_ranges, place_write_list) =
                Self::flatten_writes(&self.places.len(), &self.writes_flat);
            let mut flow = Flow {
                identity: crate::artifact::ArtifactId::fresh(),
                source_body: self.tir.identity(),
                source_stamp: self.tir.representation_stamp(),
                content_stamp: 0,
                layout_context: self.pool.identity(),
                entry,
                blocks: self.blocks,
                steps: self.steps,
                terms: self
                    .terms
                    .into_iter()
                    .map(|t| t.expect("validated above"))
                    .collect(),
                places: self.places,
                extra: self.extra,
                succs: one_empty_range(),
                edges: Vec::new(),
                preds: one_empty_range(),
                redges: Vec::new(),
                inst_meta: self.inst_meta,
                token_places: self.token_places,
                branch_arms: self.branch_arms,
                branch_if: branch_if.clone(),
                term_payload: self.term_payload,
                place_write_ranges,
                place_write_list,
            };
            flow.content_stamp = flow.representation_stamp();
            flow.validate(self.tir, self.pool);
            return flow;
        }
        // CFG edges.
        let mut edges: Vec<BlockId> = Vec::new();
        let mut succs: Vec<Range<u32>> = Vec::with_capacity(self.blocks.len());
        for t in &self.terms {
            let start = edges.len() as u32;
            match t.expect("every block terminated in Flow::build") {
                Terminator::Goto { target } => edges.push(target),
                Terminator::Branch { then_, else_, .. } => {
                    edges.push(then_);
                    edges.push(else_);
                }
                Terminator::Switch { arms, .. } => {
                    // Arms are the only successors; the join is reached
                    // through falling-through arms, not the dispatch.
                    for i in arms.as_range() {
                        edges.push(BlockId(self.extra[i]));
                    }
                }
                Terminator::While { header, .. } | Terminator::For { header, .. } => {
                    edges.push(header)
                }
                Terminator::WhileTest { body, exit, .. }
                | Terminator::ForTest { body, exit, .. } => {
                    edges.push(body);
                    edges.push(exit);
                }
                Terminator::Backedge { to, .. } => edges.push(to),
                Terminator::Break { target } | Terminator::Continue { target } => {
                    edges.push(target)
                }
                // A diverging call's continuation is unreachable:
                // control never reaches `next`, so there is no edge
                // (consumers read `next` straight off the terminator).
                Terminator::Call { .. } => {}
                Terminator::Return { .. } | Terminator::ReturnVoid | Terminator::Unreachable => {}
            }
            succs.push(start..edges.len() as u32);
        }

        // Predecessors, bucketed by prefix counts.
        let mut counts = vec![0u32; self.blocks.len()];
        for (b, _) in self.terms.iter().enumerate() {
            let r = &succs[b];
            for &s in &edges[r.start as usize..r.end as usize] {
                counts[s.0 as usize] += 1;
            }
        }
        let mut fill = Vec::with_capacity(self.blocks.len());
        let mut acc = 0u32;
        for c in &counts {
            fill.push(acc);
            acc += c;
        }
        let mut redges: Vec<BlockId> = vec![BlockId::NONE; acc as usize];
        for (b, _) in self.terms.iter().enumerate() {
            let r = &succs[b];
            for &s in &edges[r.start as usize..r.end as usize] {
                let slot = fill[s.0 as usize];
                fill[s.0 as usize] += 1;
                redges[slot as usize] = BlockId(b as u32);
            }
        }
        let mut preds: Vec<Range<u32>> = Vec::with_capacity(self.blocks.len());
        let mut start = 0u32;
        for c in &counts {
            preds.push(start..start + c);
            start += c;
        }

        // Mark blocks by true reachability from the entry: a block
        // inside dead code (e.g. a loop below a return) can have
        // predecessor edges from other dead blocks and still be
        // unreachable — construction-time flags alone don't see that.
        let mut visited: Vec<bool> = vec![false; self.blocks.len()];
        let mut stack = vec![entry];
        while let Some(b) = stack.pop() {
            if std::mem::replace(&mut visited[b.0 as usize], true) {
                continue;
            }
            let r = &succs[b.0 as usize];
            for &s in &edges[r.start as usize..r.end as usize] {
                stack.push(s);
            }
        }
        for (i, blk) in self.blocks.iter_mut().enumerate() {
            blk.unreachable = BlockId(i as u32) != entry && !visited[i];
        }

        let (place_write_ranges, place_write_list) =
            Self::flatten_writes(&self.places.len(), &self.writes_flat);

        let mut flow = Flow {
            identity: crate::artifact::ArtifactId::fresh(),
            source_body: self.tir.identity(),
            source_stamp: self.tir.representation_stamp(),
            content_stamp: 0,
            layout_context: self.pool.identity(),
            entry,
            blocks: self.blocks,
            steps: self.steps,
            terms: self
                .terms
                .into_iter()
                .map(|t| t.expect("validated above"))
                .collect(),
            places: self.places,
            extra: self.extra,
            succs,
            edges,
            preds,
            redges,
            inst_meta: self.inst_meta,
            token_places: self.token_places,
            branch_arms: self.branch_arms,
            branch_if: branch_if.clone(),
            term_payload: self.term_payload,
            place_write_ranges,
            place_write_list,
        };
        flow.content_stamp = flow.representation_stamp();
        flow.validate(self.tir, self.pool);
        flow
    }
}

/// The successor-edge targets of a terminator (a diverging call's
/// continuation is deliberately excluded — control never reaches it).
fn term_targets(t: &Terminator) -> Vec<BlockId> {
    match *t {
        Terminator::Goto { target } => vec![target],
        Terminator::Branch { then_, else_, .. } => vec![then_, else_],
        // Switch arms are resolved against `extra` by `set_term`.
        Terminator::Switch { .. } => vec![],
        Terminator::While { header, .. } | Terminator::For { header, .. } => vec![header],
        Terminator::WhileTest { body, exit, .. } | Terminator::ForTest { body, exit, .. } => {
            vec![body, exit]
        }
        Terminator::Backedge { to, .. } => vec![to],
        Terminator::Break { target } | Terminator::Continue { target } => vec![target],
        Terminator::Call { .. }
        | Terminator::Return { .. }
        | Terminator::ReturnVoid
        | Terminator::Unreachable => vec![],
    }
}

/// The single-block body's one empty successor/predecessor range list.
#[allow(clippy::single_range_in_vec_init)]
fn one_empty_range() -> Vec<Range<u32>> {
    vec![0..0]
}
