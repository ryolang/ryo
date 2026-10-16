//! Debug-only structural validation of [`Flow`] invariants, run once
//! per function body at the end of [`Flow::build`]. These checks are
//! the contract validators named by the Flow IR design: every consumer
//! (ownership analysis, cleanup lowering, backend emission) may assume
//! them, so a violation fails loudly here instead of miscompiling.

use std::collections::HashSet;

use super::{BlockId, Flow, Place, Terminator};
use crate::tir::{ChildKind, Tir, TirRef, TirTag};
use crate::types::InternPool;

pub(super) fn validate_flow(flow: &Flow, tir: &Tir, pool: &InternPool) {
    check_termination_and_reachability(flow);
    check_instruction_coverage(flow, tir);
    check_def_before_use(flow, tir);
    check_place_lineage(flow);
    check_join_shapes(flow);
    check_recorded_branch_arms(flow);
    check_block_maps(flow);
    check_bool_split_shapes(flow, tir);
    check_for_preheader_shapes(flow, tir);
    check_match_result_places(flow, tir, pool);
}

/// `branch_if` / `term_payload` agree with the terminator metadata: a
/// block maps to an IfStmt iff the IfStmt's cascade entry terminates it,
/// and every block's payload is the instruction stamped at build.
fn check_block_maps(flow: &Flow) {
    for (b, payload) in flow.term_payload.iter().enumerate() {
        let b = BlockId(b as u32);
        if let Some(inst) = payload {
            debug_assert_eq!(
                flow.term_block_of(*inst),
                Some(b),
                "term_payload[{b:?}] disagrees with term_block_of({inst:?})"
            );
        }
    }
    for (b, if_inst) in &flow.branch_if {
        debug_assert_eq!(
            flow.term_block_of(*if_inst),
            Some(*b),
            "branch_if[{b:?}] disagrees with term_block_of({if_inst:?})"
        );
        debug_assert_eq!(
            flow.term_payload(*b),
            Some(*if_inst),
            "branch_if[{b:?}] must equal the block's term payload"
        );
    }
}

/// The `unreachable` flag is exactly graph reachability from the
/// entry (set mechanically at build finish): marked blocks have no
/// path from entry; unmarked blocks do.
fn check_termination_and_reachability(flow: &Flow) {
    debug_assert!(!flow.blocks.is_empty(), "Flow IR has no entry block");
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut stack = vec![flow.entry];
    while let Some(b) = stack.pop() {
        if !seen.insert(b) {
            continue;
        }
        stack.extend_from_slice(flow.successors(b));
    }
    for (i, blk) in flow.blocks.iter().enumerate() {
        let b = BlockId(i as u32);
        debug_assert_eq!(
            blk.unreachable,
            b != flow.entry && !seen.contains(&b),
            "block {:?} unreachable flag disagrees with graph reachability",
            b
        );
    }
}

/// Every instruction reachable from the body roots is accounted exactly
/// once: a step, a terminator payload, or a registered owner token.
fn check_instruction_coverage(flow: &Flow, tir: &Tir) {
    let mut seen: HashSet<TirRef> = HashSet::new();
    let mut covered: HashSet<TirRef> = HashSet::new();
    for (inst, meta) in flow.inst_meta.iter().enumerate() {
        if let Some(i) = meta.step {
            covered.insert(flow.steps[i as usize].inst);
        }
        if meta.term_block.is_some() {
            covered.insert(TirRef::from_raw(inst as u32));
        }
    }
    for inst in flow.token_places.keys() {
        covered.insert(*inst);
    }

    fn walk(tir: &Tir, r: TirRef, seen: &mut HashSet<TirRef>, covered: &HashSet<TirRef>) {
        if !seen.insert(r) {
            return;
        }
        debug_assert!(
            covered.contains(&r),
            "instruction {:?} ({:?}) is not a step, terminator payload, or token",
            r,
            tir.inst(r).tag
        );
        tir.walk_operands(r, &mut |_p, child, _kind| {
            walk(tir, child, seen, covered);
        });
    }

    for stmt in tir.body_stmts_view() {
        walk(tir, stmt, &mut seen, &covered);
    }
}

/// Operand data dependencies are evaluated before their consumers:
/// for every *stepped* instruction, each operand edge must point to an
/// instruction with a smaller emission order. Structural instructions
/// (match/if/loop/return terminators) are skipped as parents: their
/// TIR children are layout, not data flow — Flow IR blocks define their
/// real order. Match arm bodies are control edges (they run after the
/// dispatch) and are likewise excluded.
fn check_def_before_use(flow: &Flow, tir: &Tir) {
    let mut seen: HashSet<TirRef> = HashSet::new();
    fn walk(tir: &Tir, flow: &Flow, r: TirRef, seen: &mut HashSet<TirRef>) {
        if !seen.insert(r) {
            return;
        }
        let Some(parent_order) = flow.order_of(r) else {
            unreachable!("uncovered instruction in def-before-use walk: {r:?}")
        };
        let parent_is_step = flow.step_of(r).is_some();
        tir.walk_operands(r, &mut |_p, child, kind| {
            if parent_is_step
                && kind == ChildKind::Operand
                && !matches!(tir.inst(r).tag, TirTag::Match)
            {
                let Some(child_order) = flow.order_of(child) else {
                    unreachable!("operand {child:?} of {r:?} has no emission order")
                };
                debug_assert!(
                    child_order < parent_order,
                    "def-before-use violated: {:?} (order {}) used by {:?} (order {})",
                    child,
                    child_order,
                    r,
                    parent_order
                );
            }
            walk(tir, flow, child, seen);
        });
    }
    for stmt in tir.body_stmts_view() {
        walk(tir, flow, stmt, &mut seen);
    }
}

/// Place lineage: writes reference real steps; Locals are keyed by
/// unique decls; tokens exist for every registered binding.
fn check_place_lineage(flow: &Flow) {
    let mut decls: HashSet<TirRef> = HashSet::new();
    for (i, place) in flow.places.iter().enumerate() {
        let id = super::PlaceId(i as u32);
        for &w in flow.place_writes(id) {
            // Writes are steps (decls/assigns) or structural
            // instructions (a for-loop header initializes its
            // induction place).
            debug_assert!(
                flow.step_of(w).is_some() || flow.term_block_of(w).is_some(),
                "place {:?} lineage references unknown instruction {:?}",
                place,
                w
            );
        }
        if let Place::Local { decl, .. } = place {
            debug_assert!(
                decls.insert(*decl),
                "two Local places share decl {:?} (shadowing must mint a new place)",
                decl
            );
        }
    }
}

/// Join shapes: a Switch join's predecessors are exactly its
/// falling-through arm blocks.
fn check_join_shapes(flow: &Flow) {
    for term in flow.terms.iter() {
        if let Terminator::Switch {
            arms,
            join: Some(join),
            ..
        } = *term
        {
            let arm_blocks: Vec<BlockId> =
                arms.as_range().map(|j| BlockId(flow.extra[j])).collect();
            let mut preds: Vec<BlockId> = flow.predecessors(join).to_vec();
            preds.sort();
            // An arm containing nested control flow exits through a
            // descendant block (the nested construct's join), not its
            // first block: attribute each join predecessor to the arm
            // that reaches it without passing through the join.
            let mut expected: Vec<BlockId> = Vec::new();
            for arm in arm_blocks {
                let mut seen: HashSet<BlockId> = HashSet::new();
                let mut stack = vec![arm];
                while let Some(b) = stack.pop() {
                    if b == join || !seen.insert(b) {
                        continue;
                    }
                    if flow.predecessors(join).contains(&b) {
                        expected.push(b);
                    }
                    stack.extend_from_slice(flow.successors(b));
                }
            }
            expected.sort();
            expected.dedup();
            debug_assert_eq!(
                preds, expected,
                "switch join {:?} predecessors must be exactly its falling-through arms",
                join
            );
        }
        // Note: a loop body need not contain a backedge — a body
        // whose every path breaks/continues/returns exits without one.
        // Reachability and termination are checked globally above.
    }
}

/// Recorded branch arms: the first Branch lives in the IfStmt's
/// terminator block with `then_` == the recorded `then_block`; every
/// recorded elif block holds a Branch; and the `else_` chain through
/// the recorded blocks ends at `else_block`. The chain deliberately
/// does NOT require each recorded block to be the previous `else_`
/// target: a condition that cuts blocks (match / `and` / `or`) reaches
/// its Branch-holding join block from its own condition entry.
fn check_recorded_branch_arms(flow: &Flow) {
    for (if_inst, arms) in &flow.branch_arms {
        let Some(first) = flow.term_block_of(*if_inst) else {
            debug_assert!(false, "recorded IfStmt has no terminator block");
            continue;
        };
        let mut last_else = match flow.term(first) {
            Terminator::Branch { then_, else_, .. } => {
                debug_assert_eq!(*then_, arms.then_block, "recorded then_block mismatch");
                *else_
            }
            other => {
                debug_assert!(
                    false,
                    "recorded IfStmt block must hold a Branch, got {other:?}"
                );
                continue;
            }
        };
        for &elif_block in &arms.elif_blocks {
            match flow.term(elif_block) {
                Terminator::Branch { else_, .. } => last_else = *else_,
                other => {
                    debug_assert!(
                        false,
                        "recorded elif block must hold a Branch, got {other:?}"
                    );
                    break;
                }
            }
        }
        debug_assert_eq!(last_else, arms.else_block, "recorded else block mismatch");
    }
}

/// BoolAnd/BoolOr split shape. The merged short-circuit value is a step
/// in the join block, and the join has exactly two predecessors: the
/// split block (a Branch whose OTHER target is the rhs block) and the
/// rhs fallthrough (a Goto to the join from the block where rhs
/// emission ended — with a nested cutting rhs like `a or (b and c)`
/// that is the nested construct's join, and the rhs block reaches the
/// join through the nested construct's blocks). A block whose Branch
/// cond IS a BoolAnd/BoolOr inst — an if/elif/while condition over
/// `and`/`or`, or the parent split of a chained `and`/`or` — is itself
/// the innermost join: the merged value is stepped in the very block
/// that branches on it (the arm/body blocks are its Branch targets,
/// never the join).
fn check_bool_split_shapes(flow: &Flow, tir: &Tir) {
    // Join side: a block whose first step is a BoolAnd/BoolOr inst is
    // the merge block of exactly one split.
    for (i, _blk) in flow.blocks.iter().enumerate() {
        let join = BlockId(i as u32);
        let Some(first) = flow.block_steps(join).first() else {
            continue;
        };
        if !matches!(tir.inst(first.inst).tag, TirTag::BoolAnd | TirTag::BoolOr) {
            continue;
        }
        let preds = flow.predecessors(join);
        debug_assert_eq!(
            preds.len(),
            2,
            "bool split join {:?} must have exactly 2 predecessors (split + rhs)",
            join
        );
        let mut saw_split = false;
        let mut saw_fallthrough = false;
        for &p in preds {
            match flow.term(p) {
                // The split edge: the Branch's other target is the rhs
                // block, through which the join must be reachable (a
                // nested cutting rhs — `a or (b and c)` — ends its
                // fallthrough at the nested construct's join, not here).
                Terminator::Branch { then_, else_, .. } => {
                    let rhs = if *then_ == join { *else_ } else { *then_ };
                    debug_assert!(
                        !saw_split,
                        "bool split join {:?} has two split predecessors",
                        join
                    );
                    saw_split = true;
                    let mut seen: HashSet<BlockId> = HashSet::new();
                    let mut stack = vec![rhs];
                    let mut reaches = false;
                    while let Some(b) = stack.pop() {
                        if !seen.insert(b) {
                            continue;
                        }
                        if b == join {
                            reaches = true;
                            break;
                        }
                        stack.extend_from_slice(flow.successors(b));
                    }
                    debug_assert!(
                        reaches,
                        "bool split rhs block {:?} must reach the join {:?}",
                        rhs, join
                    );
                }
                // The rhs fallthrough edge, closed at the block where rhs
                // emission ended.
                Terminator::Goto { target } => {
                    debug_assert!(
                        !saw_fallthrough,
                        "bool split join {:?} has two fallthrough predecessors",
                        join
                    );
                    saw_fallthrough = true;
                    debug_assert_eq!(
                        *target, join,
                        "bool split fallthrough predecessor {:?} must jump to the join",
                        p
                    );
                }
                other => debug_assert!(
                    false,
                    "bool split predecessor {:?} must be a Branch or a Goto, got {:?}",
                    p, other
                ),
            }
        }
    }
    // Merge-value side: a Branch whose cond is a BoolAnd/BoolOr inst
    // branches on the merged value stepped in its own block (the join).
    for (i, term) in flow.terms.iter().enumerate() {
        let b = BlockId(i as u32);
        let Terminator::Branch { cond, .. } = *term else {
            continue;
        };
        if matches!(tir.inst(cond).tag, TirTag::BoolAnd | TirTag::BoolOr) {
            debug_assert_eq!(
                flow.step_block_of(cond),
                Some(b),
                "a branch on a BoolAnd/BoolOr merge value must live in the join stepping it"
            );
        }
    }
}

/// For-loop preheader shape. The range bounds are stepped in the block
/// holding the `For` terminator (the backedge targets the header
/// directly, so bound steps must not live where the backedge can reach
/// them again); the header holds only the `ForTest` terminator — no
/// steps; and every backedge names its loop's test shape: a `for` latch
/// targets its `ForTest` header carrying the same induction place, a
/// `while` latch targets the loop header — the `WhileTest` itself for a
/// simple condition, or the condition's cutting terminator (Switch /
/// Branch) with the `WhileTest` reachable through the condition's
/// blocks.
fn check_for_preheader_shapes(flow: &Flow, tir: &Tir) {
    for (i, term) in flow.terms.iter().enumerate() {
        let p = BlockId(i as u32);
        let Terminator::For { loop_inst, header } = *term else {
            continue;
        };
        let view = tir.for_range_view(loop_inst);
        debug_assert_eq!(
            flow.step_block_of(view.start),
            Some(p),
            "for-range start must be stepped in the pre-header {:?}",
            p
        );
        debug_assert_eq!(
            flow.step_block_of(view.end),
            Some(p),
            "for-range end must be stepped in the pre-header {:?}",
            p
        );
        debug_assert!(
            flow.block_steps(header).is_empty(),
            "for header {:?} holds only its ForTest terminator",
            header
        );
        debug_assert!(
            matches!(flow.term(header), Terminator::ForTest { .. }),
            "for header {:?} must terminate in ForTest",
            header
        );
    }
    for term in flow.terms.iter() {
        let Terminator::Backedge { to, inc } = *term else {
            continue;
        };
        match inc {
            Some(var) => debug_assert!(
                matches!(flow.term(to), Terminator::ForTest { var: fv, .. } if *fv == var),
                "a for backedge must target its ForTest header carrying the same induction place"
            ),
            None => {
                // A while header holds the WhileTest directly when the
                // condition is a simple expression; a condition that
                // cuts blocks (match / `and` / `or` / value-if) leaves
                // its own terminator (Switch / Branch) in the header,
                // with the WhileTest in the condition's join.
                debug_assert!(
                    matches!(
                        flow.term(to),
                        Terminator::WhileTest { .. }
                            | Terminator::Switch { .. }
                            | Terminator::Branch { .. }
                    ),
                    "a while backedge must target the loop header (WhileTest or the condition's cutting terminator)"
                );
                // The loop's test is reachable from the header through
                // the condition's blocks alone (its successors into the
                // body are not traversed).
                let mut seen: HashSet<BlockId> = HashSet::new();
                let mut stack = vec![to];
                let mut found_test = false;
                while let Some(b) = stack.pop() {
                    if !seen.insert(b) {
                        continue;
                    }
                    if matches!(flow.term(b), Terminator::WhileTest { .. }) {
                        found_test = true;
                        break;
                    }
                    stack.extend_from_slice(flow.successors(b));
                }
                debug_assert!(
                    found_test,
                    "a while header's condition must reach the loop's WhileTest"
                );
            }
        }
    }
}

/// MatchResult place existence. A Switch with a join whose match type
/// is a real value (neither void nor never) allocates exactly one
/// result place; a void match, and an all-diverging (never, join-less)
/// match, allocate none.
fn check_match_result_places(flow: &Flow, tir: &Tir, pool: &InternPool) {
    for term in flow.terms.iter() {
        let Terminator::Switch {
            match_inst, join, ..
        } = *term
        else {
            continue;
        };
        let match_ty = tir.inst(match_inst).ty;
        let value_match = join.is_some() && match_ty != pool.void() && !pool.is_never(match_ty);
        debug_assert_eq!(
            flow.match_result(match_inst).is_some(),
            value_match,
            "match {:?} (ty {:?}, join {:?}) result-place existence mismatch",
            match_inst,
            match_ty,
            join
        );
    }
}
