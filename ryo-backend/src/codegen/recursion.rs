//! Call-graph recursion analysis for the I-177 stack-limit check.
//!
//! Unbounded stack growth needs an unbounded chain of live frames, and
//! with direct calls only (Ryo has no function values) every such chain
//! must traverse a call-graph cycle again and again. So the stack-limit
//! check only needs to guard *cycle edges*: calls whose callee sits in
//! the caller's own strongly connected component (self-recursion
//! included). Acyclic call chains are bounded by the program text and
//! their frames fit within the 32 KiB margin the runtime leaves below
//! the recorded limit, the same margin runtime calls already rely on.
//!
//! Guarding cycle edges rather than every function entry keeps the
//! check off non-recursive functions entirely (no extra IR to compile,
//! no per-call cost) and off the non-calling paths of recursive ones
//! (e.g. the base case of `fibonacci`).

use ryo_core::tir::{Tir, TirRef, TirTag};
use ryo_core::types::StringId;
use std::collections::HashMap;

/// Map each function that lies on a call-graph cycle to the id of its
/// strongly connected component. Functions absent from the map are not
/// recursive (directly or mutually). A call from `f` to `g` is a cycle
/// edge — and needs the stack-limit check — iff both are present and
/// share an id. Calls to non-user functions (builtins, runtime) are
/// ignored. Over-approximating (e.g. counting a call in dead code) only
/// adds checks, never removes a needed one.
pub(crate) fn scan_recursive_sccs(tirs: &[Tir]) -> HashMap<StringId, u32> {
    let index_of: HashMap<StringId, usize> = tirs
        .iter()
        .enumerate()
        .map(|(i, tir)| (tir.name, i))
        .collect();

    // Adjacency list (deduplicated per caller) plus self-loop flags.
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); tirs.len()];
    let mut self_loop = vec![false; tirs.len()];
    for (caller, tir) in tirs.iter().enumerate() {
        // Slot 0 is the reserved arena sentinel, never an instruction.
        for idx in 1..tir.instructions.len() {
            if tir.instructions[idx].tag != TirTag::Call {
                continue;
            }
            let r = TirRef::from_raw(idx as u32);
            let Some(&callee) = index_of.get(&tir.call_view(r).name) else {
                continue;
            };
            if callee == caller {
                self_loop[caller] = true;
            } else if !succs[caller].contains(&callee) {
                succs[caller].push(callee);
            }
        }
    }

    // Iterative Tarjan: no recursion, so pathological call graphs
    // cannot overflow the compiler's own stack.
    const UNVISITED: usize = usize::MAX;
    let n = tirs.len();
    let mut index = vec![UNVISITED; n];
    let mut lowlink = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut next_scc = 0u32;
    let mut result = HashMap::new();
    // DFS frames: (node, position in its successor list).
    let mut work: Vec<(usize, usize)> = Vec::new();

    for root in 0..n {
        if index[root] != UNVISITED {
            continue;
        }
        work.push((root, 0));
        while let Some(&(v, pos)) = work.last() {
            if index[v] == UNVISITED {
                index[v] = next_index;
                lowlink[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = succs[v].get(pos) {
                if let Some(top) = work.last_mut() {
                    top.1 += 1;
                }
                if index[w] == UNVISITED {
                    work.push((w, 0));
                } else if on_stack[w] {
                    lowlink[v] = lowlink[v].min(index[w]);
                }
                continue;
            }
            // All successors done: close v, propagate lowlink upward.
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                lowlink[parent] = lowlink[parent].min(lowlink[v]);
            }
            if lowlink[v] == index[v] {
                let mut members = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    members.push(w);
                    if w == v {
                        break;
                    }
                }
                if members.len() > 1 || self_loop[v] {
                    for m in members {
                        result.insert(tirs[m].name, next_scc);
                    }
                    next_scc += 1;
                }
            }
        }
    }
    result
}
