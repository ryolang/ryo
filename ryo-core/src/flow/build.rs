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
use crate::tir::{Tir, TirRef, TirTag};

mod control;
mod emission;
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

        emission::emit_root(&mut self);

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
            // `StrConst`/`BytesConst`/`DebugRepr`/`ErrorMessage` join the
            // producer set (G2): the ownership pass keys owner state by place,
            // so every tracked producer needs a temp place — a
            // literal's place is its storage identity even though
            // codegen materializes it inline.
            TirTag::StrConst
            | TirTag::BytesConst
            | TirTag::DebugRepr
            | TirTag::ErrorMessage
            | TirTag::StrConcat
            | TirTag::BytesConcat
            | TirTag::Call
            | TirTag::StructLit
            | TirTag::EnumLit
            | TirTag::ErrorUnionCoerce
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
        let mut current = r;
        loop {
            let inst = self.tir.inst(current);
            match inst.data {
                crate::tir::TirData::Var(name) => return self.resolve(name),
                crate::tir::TirData::FieldAccess { object, .. } => current = object,
                crate::tir::TirData::UnOp(operand)
                    if matches!(inst.tag, TirTag::ViewAsOwner | TirTag::ToView) =>
                {
                    current = operand;
                }
                crate::tir::TirData::Slice { base, .. } => current = base,
                _ => {
                    return self.inst_meta.get(current.index()).and_then(|m| m.temp);
                }
            }
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
