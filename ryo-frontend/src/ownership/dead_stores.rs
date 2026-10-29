//! Flow-sensitive dead-store analysis over lexical binding homes and store generations.

#[cfg(test)]
use super::collect_loop_nesting;
use super::{
    DeadStoreEntry, LoopNesting, Owner, RegionStop, UNTRACKED_PLACE, collect_region,
    flow_owner_place, loop_test_block, needs_tracking,
};
use ryo_core::flow::{BlockId, Flow, Place, PlaceId, Terminator};
use ryo_core::tir::{ParamMode, Tir, TirRef, TirTag};
use ryo_core::types::InternPool;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    fn new(bits: usize) -> Self {
        Self {
            words: vec![0; bits.div_ceil(64)],
        }
    }

    fn insert(&mut self, bit: u32) {
        let word = bit as usize / 64;
        if self.words.len() <= word {
            self.words.resize(word + 1, 0);
        }
        self.words[word] |= 1_u64 << (bit % 64);
    }

    fn remove(&mut self, bit: u32) {
        if let Some(word) = self.words.get_mut(bit as usize / 64) {
            *word &= !(1_u64 << (bit % 64));
        }
    }

    fn contains(&self, bit: u32) -> bool {
        self.words
            .get(bit as usize / 64)
            .is_some_and(|word| word & (1_u64 << (bit % 64)) != 0)
    }

    fn union_with(&mut self, other: &Self) {
        self.words
            .resize(self.words.len().max(other.words.len()), 0);
        for (word, rhs) in self.words.iter_mut().zip(&other.words) {
            *word |= rhs;
        }
    }

    fn difference_with(&mut self, other: &Self) {
        for (word, rhs) in self.words.iter_mut().zip(&other.words) {
            *word &= !rhs;
        }
    }

    fn retain(&mut self, mut keep: impl FnMut(u32) -> bool) {
        for (word_index, word) in self.words.iter_mut().enumerate() {
            let mut bits = *word;
            while bits != 0 {
                let bit = bits.trailing_zeros();
                let id = (word_index * 64) as u32 + bit;
                if !keep(id) {
                    *word &= !(1_u64 << bit);
                }
                bits &= bits - 1;
            }
        }
    }

    fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.words.iter().enumerate().flat_map(|(word, bits)| {
            (0..64).filter_map(move |bit| {
                (bits & (1_u64 << bit) != 0).then_some((word * 64 + bit) as u32)
            })
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct StoreGeneration {
    owner: Owner,
    entry: DeadStoreEntry,
    def_block: Option<BlockId>,
    arm: Option<(TirRef, usize)>,
}

#[derive(Clone, Debug, Default)]
struct BlockTransfer {
    seed: BitSet,
    kill: BitSet,
    generated: BitSet,
}

#[derive(Default)]
pub(crate) struct DeadStoreAnalysis {
    pending: HashMap<Owner, DeadStoreEntry>,
    arm_dead: HashMap<PlaceId, DeadStoreEntry>,
    arm_dead_order: Vec<PlaceId>,
}

impl DeadStoreAnalysis {
    #[cfg(test)]
    pub(crate) fn analyze(tir: &Tir, flow: &Flow, pool: &InternPool) -> Self {
        let nesting = collect_loop_nesting(tir, flow);
        Self::analyze_with_nesting(tir, flow, pool, &nesting)
    }

    pub(crate) fn analyze_with_nesting(
        tir: &Tir,
        flow: &Flow,
        pool: &InternPool,
        nesting: &LoopNesting,
    ) -> Self {
        let (stores, by_home, initial) = collect_stores(tir, flow, pool, nesting);
        let generations = index_generations(tir, &stores);
        let read_orders = last_read_orders(flow);
        if stores
            .iter()
            .all(|store| read_after_store(flow, &read_orders, *store))
        {
            return Self::default();
        }
        let transfers = build_transfers(tir, flow, pool, &stores, &by_home, &generations, nesting);
        let dominators = compute_dominators(flow);
        let outs = propagate(flow, &stores, &transfers, &initial, &dominators);
        let pending = collect_pending(flow, &stores, &outs, &dominators, &read_orders);
        let (arm_dead, arm_dead_order) =
            collect_arm_dead(tir, flow, pool, &stores, &generations, &outs, &read_orders);
        Self {
            pending,
            arm_dead,
            arm_dead_order,
        }
    }

    pub(crate) fn pending(&self) -> impl Iterator<Item = (&Owner, &DeadStoreEntry)> {
        self.pending.iter()
    }

    pub(crate) fn is_pending(&self, owner: &Owner) -> bool {
        self.pending.contains_key(owner)
    }

    pub(crate) fn arm_dead_store(&self, home: PlaceId) -> Option<DeadStoreEntry> {
        self.arm_dead.get(&home).copied()
    }

    pub(crate) fn arm_dead_stores(&self) -> impl Iterator<Item = DeadStoreEntry> + '_ {
        self.arm_dead_order.iter().map(|home| self.arm_dead[home])
    }
}

fn add_store(
    stores: &mut Vec<StoreGeneration>,
    by_home: &mut [Vec<u32>],
    owner: Owner,
    entry: DeadStoreEntry,
    def_block: Option<BlockId>,
    arm: Option<(TirRef, usize)>,
) {
    let id = u32::try_from(stores.len()).expect("store generation index fits u32");
    let home = entry.3;
    stores.push(StoreGeneration {
        owner,
        entry,
        def_block,
        arm,
    });
    by_home[home.0 as usize].push(id);
}

fn kill_home(home: PlaceId, by_home: &[Vec<u32>], transfer: &mut BlockTransfer) {
    for id in &by_home[home.0 as usize] {
        transfer.kill.insert(*id);
        transfer.generated.remove(*id);
    }
}

fn generate_store(id: u32, home: PlaceId, by_home: &[Vec<u32>], transfer: &mut BlockTransfer) {
    for old in &by_home[home.0 as usize] {
        transfer.generated.remove(*old);
    }
    transfer.generated.insert(id);
}

fn collect_stores(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    nesting: &LoopNesting,
) -> (Vec<StoreGeneration>, Vec<Vec<u32>>, BitSet) {
    let mut stores = Vec::new();
    let mut by_home = vec![Vec::new(); flow.places.len()];
    let mut initial = BitSet::new(tir.params.len());

    for (index, param) in tir.params.iter().enumerate() {
        if param.mode == ParamMode::Inout {
            continue;
        }
        let home = flow
            .places
            .iter()
            .enumerate()
            .find_map(|(id, place)| match place {
                Place::Param { index: i, .. } if *i as usize == index => Some(PlaceId(id as u32)),
                _ => None,
            })
            .expect("parameter place");
        let id = stores.len() as u32;
        stores.push(StoreGeneration {
            owner: Owner(home),
            entry: (param.name, param.span, TirRef::param(index), home),
            def_block: None,
            arm: None,
        });
        by_home[home.0 as usize].push(id);
        initial.insert(id);
    }

    for block in flow.forward_order() {
        if let Terminator::Switch {
            match_inst, arms, ..
        } = *flow.term(block)
        {
            let view = tir.match_view(match_inst);
            let token_places = flow.token_places(match_inst);
            let mut token_cursor = 0;
            for (arm_index, arm) in view.arms().enumerate() {
                let arm_block = BlockId(flow.extra[arms.as_range().start + arm_index]);
                for binding in arm.bindings() {
                    if pool.is_copy(binding.ty) {
                        continue;
                    }
                    let home = token_places[token_cursor];
                    token_cursor += 1;
                    add_store(
                        &mut stores,
                        &mut by_home,
                        Owner(home),
                        (binding.name, tir.span(match_inst), match_inst, home),
                        Some(arm_block),
                        Some((match_inst, arm_index)),
                    );
                }
            }
        }

        for step in flow.block_steps(block) {
            let r = step.inst;
            if tir.inst(r).tag == TirTag::Destructure {
                let view = tir.destructure_view(r);
                let token_places = flow.token_places(r);
                let mut token_cursor = 0;
                for field in &view.fields {
                    let Some(name) = field.bind else {
                        continue;
                    };
                    let home = token_places[token_cursor];
                    token_cursor += 1;
                    if needs_tracking(field.ty, pool) {
                        add_store(
                            &mut stores,
                            &mut by_home,
                            Owner(home),
                            (name, tir.span(r), r, home),
                            Some(block),
                            None,
                        );
                    }
                }
            }
            let value = match tir.inst(r).tag {
                TirTag::VarDecl => Some(tir.var_decl_view(r).initializer),
                TirTag::Assign => Some(tir.assign_view(r).value),
                _ => None,
            };
            let (Some(value), Some(write)) = (value, step.facts.write) else {
                continue;
            };
            let home = write.place;
            let inout = matches!(flow.place(home), Place::Param { index, .. } if tir.params[*index as usize].mode == ParamMode::Inout);
            if inout {
                continue;
            }
            let tracked = needs_tracking(tir.inst(value).ty, pool);
            if tracked
                && tir.inst(value).tag == TirTag::Var
                && flow
                    .step_of(value)
                    .is_some_and(|step| step.facts.read == Some(home))
            {
                continue;
            }
            let owner = if tracked {
                let loop_carried = tir.inst(value).tag == TirTag::Match
                    && match flow.place(home) {
                        Place::Local { decl, .. } => nesting
                            .ancestors_innermost_first(r)
                            .any(|loop_inst| flow.order_of(*decl) < flow.order_of(loop_inst)),
                        _ => false,
                    };
                if loop_carried {
                    Owner(home)
                } else {
                    Owner(
                        flow.match_result(value)
                            .or_else(|| flow_owner_place(flow, value))
                            .unwrap_or(UNTRACKED_PLACE),
                    )
                }
            } else {
                Owner(home)
            };
            let name = flow.place(home).name();
            add_store(
                &mut stores,
                &mut by_home,
                owner,
                (name, tir.span(r), r, home),
                Some(block),
                None,
            );
        }
    }
    initial.words.resize(stores.len().div_ceil(64), 0);
    (stores, by_home, initial)
}

fn build_transfers(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    stores: &[StoreGeneration],
    by_home: &[Vec<u32>],
    generations: &[Vec<(PlaceId, u32)>],
    nesting: &LoopNesting,
) -> Vec<BlockTransfer> {
    let mut condition_loops = vec![None; flow.blocks.len()];
    let mut scratch = Vec::new();
    for block in flow.forward_order() {
        let Terminator::While { loop_inst, header } = *flow.term(block) else {
            continue;
        };
        let test = loop_test_block(flow, loop_inst, header);
        let (region, _) = collect_region(flow, &mut scratch, header, |b| {
            RegionStop::Through(test).kind(b)
        });
        for condition_block in region {
            condition_loops[condition_block.0 as usize] = Some(loop_inst);
        }
    }
    let mut transfers = vec![BlockTransfer::default(); flow.blocks.len()];
    for (id, store) in stores.iter().enumerate() {
        if store.arm.is_some() {
            let block = store.def_block.expect("arm store block");
            transfers[block.0 as usize].seed.insert(id as u32);
        }
    }
    for block in flow.forward_order() {
        let transfer = &mut transfers[block.0 as usize];
        for step in flow.block_steps(block) {
            let r = step.inst;
            if let Some(home) = step.facts.read {
                kill_home(home, by_home, transfer);
                if let Some(loop_inst) = condition_loops[block.0 as usize] {
                    for id in &by_home[home.0 as usize] {
                        let store = stores[*id as usize];
                        if nesting
                            .ancestors_innermost_first(store.entry.2)
                            .any(|enclosing| enclosing == loop_inst)
                        {
                            transfer.kill.remove(*id);
                        }
                    }
                }
            }
            if tir.inst(r).tag == TirTag::Destructure {
                let view = tir.destructure_view(r);
                let token_places = flow.token_places(r);
                let mut token_cursor = 0;
                for field in &view.fields {
                    if field.bind.is_none() {
                        continue;
                    }
                    let home = token_places[token_cursor];
                    token_cursor += 1;
                    if let Some(id) = generation_at(generations, r, home) {
                        generate_store(id, home, by_home, transfer);
                    }
                }
            }
            let home = match tir.inst(r).tag {
                TirTag::VarDecl | TirTag::Assign => step.facts.write.map(|write| write.place),
                _ => None,
            };
            let Some(home) = home else {
                continue;
            };
            kill_home(home, by_home, transfer);
            let inout = matches!(flow.place(home), Place::Param { index, .. } if tir.params[*index as usize].mode == ParamMode::Inout);
            let self_assignment = tir.inst(r).tag == TirTag::Assign
                && needs_tracking(tir.inst(tir.assign_view(r).value).ty, pool)
                && flow
                    .step_of(tir.assign_view(r).value)
                    .is_some_and(|value_step| value_step.facts.read == Some(home));
            if !inout && !self_assignment {
                let id =
                    generation_at(generations, r, home).expect("collected assignment generation");
                generate_store(id, home, by_home, transfer);
            }
        }
    }
    let words = stores.len().div_ceil(64);
    for transfer in &mut transfers {
        transfer.seed.words.resize(words, 0);
        transfer.kill.words.resize(words, 0);
        transfer.generated.words.resize(words, 0);
    }
    transfers
}

fn index_generations(tir: &Tir, stores: &[StoreGeneration]) -> Vec<Vec<(PlaceId, u32)>> {
    let mut table = vec![Vec::new(); tir.instructions.len()];
    for (id, store) in stores.iter().enumerate() {
        if !store.entry.2.is_param() {
            table[store.entry.2.index()].push((store.entry.3, id as u32));
        }
    }
    table
}

fn generation_at(generations: &[Vec<(PlaceId, u32)>], inst: TirRef, home: PlaceId) -> Option<u32> {
    generations[inst.index()]
        .iter()
        .find_map(|(candidate, id)| (*candidate == home).then_some(*id))
}

fn compute_dominators(flow: &Flow) -> Vec<Option<BlockId>> {
    let count = flow.blocks.len();
    let mut idom = vec![None; count];
    for block in flow.forward_order() {
        if block == flow.entry
            || (!flow.blocks[block.0 as usize].unreachable && flow.predecessors(block).is_empty())
        {
            idom[block.0 as usize] = Some(block);
        }
    }
    let mut changed = true;
    let mut rounds = 0;
    while changed {
        changed = false;
        rounds += 1;
        assert!(rounds <= count + 1, "dominator computation converges");
        for block in flow.forward_order() {
            if idom[block.0 as usize] == Some(block) {
                continue;
            }
            let mut new = None;
            for pred in flow.predecessors(block) {
                if flow.blocks[pred.0 as usize].unreachable || idom[pred.0 as usize].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => *pred,
                    Some(current) => intersect_dominators(&idom, current, *pred),
                });
            }
            if new != idom[block.0 as usize] {
                idom[block.0 as usize] = new;
                changed = true;
            }
        }
    }
    idom
}

fn intersect_dominators(idom: &[Option<BlockId>], mut a: BlockId, mut b: BlockId) -> BlockId {
    while a != b {
        while a.0 > b.0 {
            a = idom[a.0 as usize].expect("dominator parent");
        }
        while b.0 > a.0 {
            b = idom[b.0 as usize].expect("dominator parent");
        }
    }
    a
}

fn dominates(idom: &[Option<BlockId>], dom: BlockId, block: BlockId) -> bool {
    let mut current = Some(block);
    for _ in 0..idom.len() {
        let Some(block) = current else {
            return false;
        };
        if block == dom {
            return true;
        }
        current = idom[block.0 as usize];
        if current == Some(block) {
            return block == dom;
        }
    }
    false
}

fn store_dominates(store: StoreGeneration, block: BlockId, idom: &[Option<BlockId>]) -> bool {
    store
        .def_block
        .is_none_or(|def| dominates(idom, def, block))
}

fn propagate(
    flow: &Flow,
    stores: &[StoreGeneration],
    transfers: &[BlockTransfer],
    initial: &BitSet,
    dominators: &[Option<BlockId>],
) -> Vec<BitSet> {
    let mut top = BitSet::new(stores.len());
    for id in 0..stores.len() as u32 {
        top.insert(id);
    }
    let mut outs = vec![top; flow.blocks.len()];
    let mut output = BitSet::new(stores.len());
    let cap = 4 * (flow.blocks.len() + stores.len() + 1);
    for _ in 0..cap {
        let mut changed = false;
        for block in flow.forward_order() {
            if block == flow.entry {
                output.clone_from(initial);
            } else {
                join_predecessors(flow, block, stores, &outs, dominators, &mut output);
            }
            let transfer = &transfers[block.0 as usize];
            output.union_with(&transfer.seed);
            output.difference_with(&transfer.kill);
            output.union_with(&transfer.generated);
            if output != outs[block.0 as usize] {
                outs[block.0 as usize].clone_from(&output);
                changed = true;
            }
        }
        if !changed {
            return outs;
        }
    }
    unreachable!("dead-store fixed point did not converge");
}

fn join_predecessors(
    flow: &Flow,
    block: BlockId,
    stores: &[StoreGeneration],
    outs: &[BitSet],
    dominators: &[Option<BlockId>],
    output: &mut BitSet,
) {
    let preds = flow.predecessors(block);
    if preds.is_empty() {
        output.words.fill(0);
        return;
    }
    output.clone_from(&outs[preds[0].0 as usize]);
    for pred in &preds[1..] {
        output.union_with(&outs[pred.0 as usize]);
    }
    if preds.len() == 1 {
        return;
    }
    output.retain(|id| {
        !store_dominates(stores[id as usize], block, dominators)
            || preds.iter().all(|pred| outs[pred.0 as usize].contains(id))
    });
}

fn collect_pending(
    flow: &Flow,
    stores: &[StoreGeneration],
    outs: &[BitSet],
    dominators: &[Option<BlockId>],
    read_orders: &[u32],
) -> HashMap<Owner, DeadStoreEntry> {
    let terminals: Vec<_> = flow
        .forward_order()
        .filter(|&block| flow.successors(block).is_empty())
        .collect();
    let live_terminals: Vec<_> = terminals
        .iter()
        .copied()
        .filter(|block| !flow.blocks[block.0 as usize].unreachable)
        .collect();
    let mut union = BitSet::new(stores.len());
    for &block in &terminals {
        union.union_with(&outs[block.0 as usize]);
    }
    let mut pending = HashMap::new();
    for id in union.iter() {
        let store = stores[id as usize];
        if store.arm.is_some() || read_after_store(flow, read_orders, store) {
            continue;
        }
        let dominates_all = !live_terminals.is_empty()
            && live_terminals
                .iter()
                .all(|&block| store_dominates(store, block, dominators));
        let survives = !dominates_all
            || live_terminals
                .iter()
                .all(|block| outs[block.0 as usize].contains(id));
        if survives {
            pending.insert(store.owner, store.entry);
        }
    }
    pending
}

fn last_read_orders(flow: &Flow) -> Vec<u32> {
    let mut orders = vec![0; flow.places.len()];
    for step in &flow.steps {
        if let Some(home) = step.facts.read {
            let order = flow.order_of(step.inst).unwrap_or(0);
            let slot = &mut orders[home.0 as usize];
            *slot = (*slot).max(order);
        }
    }
    orders
}

fn read_after_store(flow: &Flow, read_orders: &[u32], store: StoreGeneration) -> bool {
    read_orders[store.entry.3.0 as usize] > flow.order_of(store.entry.2).unwrap_or(0)
}

fn collect_arm_dead(
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    stores: &[StoreGeneration],
    generations: &[Vec<(PlaceId, u32)>],
    outs: &[BitSet],
    read_orders: &[u32],
) -> (HashMap<PlaceId, DeadStoreEntry>, Vec<PlaceId>) {
    let mut dead = HashMap::new();
    let mut order = Vec::new();
    let mut scratch = Vec::new();
    for block in flow.forward_order() {
        let Terminator::Switch {
            match_inst,
            arms,
            join,
            ..
        } = *flow.term(block)
        else {
            continue;
        };
        let view = tir.match_view(match_inst);
        let token_places = flow.token_places(match_inst);
        let mut token_cursor = 0;
        for (arm_index, arm) in view.arms().enumerate() {
            let arm_block = BlockId(flow.extra[arms.as_range().start + arm_index]);
            let stop = join.map_or(RegionStop::At(BlockId::NONE), RegionStop::At);
            let (region, reached_join) =
                collect_region(flow, &mut scratch, arm_block, |b| stop.kind(b));
            let arm_exit = region.iter().copied().find(|&b| {
                matches!(
                    flow.term(b),
                    Terminator::Return { .. }
                        | Terminator::ReturnVoid
                        | Terminator::Break { .. }
                        | Terminator::Continue { .. }
                )
            });
            let join_exits: Vec<_> = region
                .iter()
                .copied()
                .filter(|&b| matches!(flow.term(b), Terminator::Goto { target } if Some(*target) == join))
                .collect();
            for binding in arm.bindings() {
                if pool.is_copy(binding.ty) {
                    continue;
                }
                let home = token_places[token_cursor];
                token_cursor += 1;
                let id =
                    generation_at(generations, match_inst, home).expect("match binding generation");
                if read_after_store(flow, read_orders, stores[id as usize]) {
                    continue;
                }
                let pending = if reached_join && !join_exits.is_empty() {
                    join_exits
                        .iter()
                        .all(|block| outs[block.0 as usize].contains(id))
                } else {
                    arm_exit.is_some_and(|block| outs[block.0 as usize].contains(id))
                };
                if pending {
                    let entry = stores[id as usize].entry;
                    dead.insert(home, entry);
                    order.push(home);
                }
            }
        }
    }
    (dead, order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::tests::common::check_src_full;

    #[test]
    fn unreachable_blocks_have_no_dominators() {
        let src = concat!(
            "fn f() -> int:\n",
            "\treturn 0\n",
            "\tif true:\n",
            "\t\tdead = \"then\"\n",
            "\telse:\n",
            "\t\tdead = \"else\"\n",
        );
        let (_, _, tirs, pool) = check_src_full(src);
        let flow = Flow::build(&tirs[0], &pool);
        let idom = compute_dominators(&flow);
        for block in flow.forward_order() {
            if flow.blocks[block.0 as usize].unreachable {
                assert_eq!(idom[block.0 as usize], None, "dead block {block:?}");
            }
        }
    }
}
