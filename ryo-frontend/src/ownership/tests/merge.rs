use super::super::*;
use super::common::*;

#[test]
fn branch_ids_unique_across_post_loop_if() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let print_name = pool.intern_str("print");
    let lit_a = pool.intern_str("a");
    let lit_b = pool.intern_str("b");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     while false:
    //         if true:
    //             print("a")
    //     if true:
    //         print("b")
    //
    // The post-loop `if` must not reuse BranchIds that the
    // inside-loop `if` already minted. Today this test may pass
    // vacuously because M8.1's print-of-StrConst doesn't produce
    // branch-gated Frees, so `free_schedule` may have no
    // `Some(BranchId)` entries to inspect. The strong regression
    // for Bug 4 lives in
    // `branch_ids_do_not_collide_after_loop` in
    // `tests/integration_tests.rs`.
    let mut tb = TirBuilder::new(main, vec![], void, span);

    let cond_w = tb.bool_const(false, bool_ty, span);
    let cond_i1 = tb.bool_const(true, bool_ty, span);
    let s_a = tb.str_const(lit_a, str_ty, span);
    let print_a = tb.call(print_name, &[s_a], &all_borrow(&[s_a]), void, span);
    let if_inside = tb.if_stmt(cond_i1, &[print_a], &[], None, void, span);
    let wl = tb.while_loop(cond_w, &[if_inside], void, span);

    let cond_i2 = tb.bool_const(true, bool_ty, span);
    let s_b = tb.str_const(lit_b, str_ty, span);
    let print_b = tb.call(print_name, &[s_b], &all_borrow(&[s_b]), void, span);
    let if_post = tb.if_stmt(cond_i2, &[print_b], &[], None, void, span);

    let tir = tb.finish(&[wl, if_post]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    let max = sidecar
        .free_schedule
        .iter()
        .filter_map(|fp| fp.branch.map(|b| b.0))
        .max();
    if let Some(m) = max {
        assert!(
            m >= 2,
            "post-loop branch reused an inside-loop BranchId; max id = {m}, schedule = {:?}",
            sidecar.free_schedule
        );
    }
    // If no branch-gated frees were scheduled, the test passes
    // vacuously — the integration test
    // `branch_ids_do_not_collide_after_loop` is the stronger
    // guarantee.
}

#[test]
fn merge_branches_takes_max_next_branch_id() {
    // Direct regression for Bug 4 in M8.1c. The loop merge starts
    // from the pre-loop entry; merge_branches must monotonically
    // advance next_branch_id so that BranchIds minted inside a
    // loop body are not reused by post-loop ifs.
    let mut entry = Ownership {
        next_branch_id: 0,
        ..Ownership::default()
    };

    // inside-loop minted ids 0..=4
    let after_body = Ownership {
        next_branch_id: 5,
        ..Ownership::default()
    };

    entry.merge_branches(&[&entry.clone(), &after_body]);

    assert_eq!(
        entry.next_branch_id, 5,
        "merge_branches must take the max of branch next_branch_id values"
    );
}

#[test]
fn merge_branches_keeps_self_when_self_is_higher() {
    // Symmetry check: max() also wins when self already has the
    // higher value (a branch that didn't allocate any BranchIds
    // shouldn't roll the allocator backward).
    let mut entry = Ownership {
        next_branch_id: 7,
        ..Ownership::default()
    };

    let other = Ownership::default(); // next_branch_id = 0

    entry.merge_branches(&[&other, &other]);

    assert_eq!(
        entry.next_branch_id, 7,
        "merge_branches must not roll next_branch_id backward"
    );
}
