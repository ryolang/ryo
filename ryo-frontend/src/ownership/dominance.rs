//! Immediate-dominator analysis over a function's Flow CFG.

use ryo_core::flow::{BlockId, Flow};

pub(crate) struct Dominators {
    idom: Vec<Option<BlockId>>,
}

impl Dominators {
    pub(crate) fn new(flow: &Flow) -> Self {
        let count = flow.blocks.len();
        let (rpo, ranks) = reverse_postorder(flow);
        let mut idom = vec![None; count];
        for block in flow.forward_order() {
            if block == flow.entry
                || (!flow.blocks[block.0 as usize].unreachable
                    && flow.predecessors(block).is_empty())
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
            for &block in &rpo {
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
                        Some(current) => intersect_dominators(&idom, &ranks, current, *pred),
                    });
                }
                if new != idom[block.0 as usize] {
                    idom[block.0 as usize] = new;
                    changed = true;
                }
            }
        }
        Self { idom }
    }

    pub(crate) fn dominates(&self, definition: BlockId, use_: BlockId) -> bool {
        let mut current = Some(use_);
        for _ in 0..self.idom.len() {
            let Some(block) = current else {
                return false;
            };
            if block == definition {
                return true;
            }
            current = self.idom[block.0 as usize];
            if current == Some(block) {
                return block == definition;
            }
        }
        false
    }

    pub(crate) fn is_reachable(&self, block: BlockId) -> bool {
        self.idom[block.0 as usize].is_some()
    }

    #[cfg(test)]
    pub(crate) fn immediate(&self, block: BlockId) -> Option<BlockId> {
        self.idom[block.0 as usize]
    }
}

fn reverse_postorder(flow: &Flow) -> (Vec<BlockId>, Vec<usize>) {
    let mut postorder = Vec::new();
    let mut visited = vec![false; flow.blocks.len()];
    for start in std::iter::once(flow.entry).chain(flow.forward_order()) {
        if visited[start.0 as usize] || flow.blocks[start.0 as usize].unreachable {
            continue;
        }
        visited[start.0 as usize] = true;
        let mut stack = vec![(start, false)];
        while let Some((block, expanded)) = stack.pop() {
            if expanded {
                postorder.push(block);
                continue;
            }
            stack.push((block, true));
            for &successor in flow.successors(block) {
                if flow.blocks[successor.0 as usize].unreachable || visited[successor.0 as usize] {
                    continue;
                }
                visited[successor.0 as usize] = true;
                stack.push((successor, false));
            }
        }
    }
    postorder.reverse();
    let mut ranks = vec![usize::MAX; flow.blocks.len()];
    for (rank, &block) in postorder.iter().enumerate() {
        ranks[block.0 as usize] = rank;
    }
    (postorder, ranks)
}

fn intersect_dominators(
    idom: &[Option<BlockId>],
    ranks: &[usize],
    mut a: BlockId,
    mut b: BlockId,
) -> BlockId {
    let mut budget = 2 * idom.len() + 2;
    while a != b {
        while ranks[a.0 as usize] > ranks[b.0 as usize] {
            budget = budget
                .checked_sub(1)
                .expect("dominator intersection did not converge");
            a = idom[a.0 as usize].expect("dominator parent");
        }
        while ranks[b.0 as usize] > ranks[a.0 as usize] {
            budget = budget
                .checked_sub(1)
                .expect("dominator intersection did not converge");
            b = idom[b.0 as usize].expect("dominator parent");
        }
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_roots_fail_instead_of_hanging() {
        let idom = vec![Some(BlockId(0)), Some(BlockId(1))];
        let ranks = vec![0, 1];
        let result = std::panic::catch_unwind(|| {
            intersect_dominators(&idom, &ranks, BlockId(1), BlockId(0))
        });
        assert!(result.is_err(), "disconnected roots must not hang");
    }

    #[test]
    fn intersection_does_not_use_block_id_as_execution_order() {
        let idom = vec![Some(BlockId(0)), Some(BlockId(2)), Some(BlockId(0))];
        let ranks = vec![0, 2, 1];
        assert_eq!(
            intersect_dominators(&idom, &ranks, BlockId(1), BlockId(2)),
            BlockId(2)
        );
    }
}
