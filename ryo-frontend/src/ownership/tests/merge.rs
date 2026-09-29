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
    // `tests/integration_ownership.rs`.
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
fn merge_branches_leaves_branch_allocator_untouched() {
    // Direct regression for Bug 4 in M8.1c. The loop merge starts
    // from the pre-loop entry; the BranchId allocator lives on the
    // single `Ownership` walked in place across all arms and loop
    // passes, so a merge can never roll it backward. BranchState
    // doesn't even carry next_branch_id — pin that structurally:
    // merging arms must leave the allocator untouched.
    let mut entry = Ownership {
        next_branch_id: 7,
        ..Ownership::default()
    };

    let arm = entry.snapshot_branch();
    entry.merge_branches(vec![arm.clone(), arm], &[true, true]);

    assert_eq!(
        entry.next_branch_id, 7,
        "merge_branches must not touch the BranchId allocator"
    );
}

/// Every real instruction of `tag` in `tir`, in arena order.
fn find_tags(tir: &ryo_core::tir::Tir, tag: ryo_core::tir::TirTag) -> Vec<TirRef> {
    (1..tir.instructions.len())
        .map(|i| TirRef::from_raw(i as u32))
        .filter(|&r| tir.inst(r).tag == tag)
        .collect()
}

/// Initializer of the n-th `VarDecl` in `tir` — the owner TirRef a
/// source-level `mut x = ...` binding starts on.
fn var_decl_init(tir: &ryo_core::tir::Tir, n: usize) -> TirRef {
    let decls = find_tags(tir, ryo_core::tir::TirTag::VarDecl);
    tir.var_decl_view(decls[n]).initializer
}

#[test]
fn return_arm_move_does_not_poison_join() {
    // Semantics change (owner-approved): an if arm that ends in
    // `return` contributes no Moved state to the join merge — the
    // move is path-local to the return exit, which the return
    // epilogue owns. Before this change the merge's any-Moved-wins
    // rule poisoned `out` for every post-`if` use and both the
    // str_push and the print tripped E0020.
    //   fn pick(c: bool) -> str:
    //       mut out = "["
    //       if c: return out
    //       str_push(&out, "x")
    //       print(out)
    //       return "done"
    let (diags, mut sidecar, tirs, _pool) = check_src_full(
        "fn pick(c: bool) -> str:\n\tmut out = \"[\"\n\tif c:\n\t\treturn out\n\tstr_push(&out, \"x\")\n\tprint(out)\n\treturn \"done\"\n",
    );
    assert!(
        diags.is_empty(),
        "conditional return of a value must not trip E0020; got: {diags:?}"
    );
    // Free scheduling: on the fall-through path nothing moved `out`,
    // so the last-use pass owns its single Free, anchored after the
    // print (the in-if return path MOVED `out` out to the caller —
    // the epilogue must NOT free it there).
    let sc = take_function_sidecar(&mut sidecar, 0);
    let tir = &tirs[0];
    let out_owner = var_decl_init(tir, 0);
    let returns = find_tags(tir, ryo_core::tir::TirTag::Return);
    assert_eq!(returns.len(), 2, "the in-if return + the final return");
    let out_frees: Vec<_> = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.target == out_owner)
        .collect();
    assert_eq!(
        out_frees.len(),
        1,
        "exactly one Free for `out` (last-use, no branch-gated double); schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        out_frees[0].after.index() > returns[0].index(),
        "the `out` Free anchors after the if (its last read is post-if); schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        out_frees[0].branch.is_none(),
        "no branch-gated Free may exist for `out` — the moved arm exits"
    );
}

#[test]
fn return_accumulator_in_loop_no_e0020() {
    // The bug-report shape: `return out` inside a loop used to stamp
    // Moved into the shared lattice at the if-join, the loop merge
    // then propagated it across the back-edge, and Phase 2 re-walk
    // flagged every subsequent use (str_push, the in-loop return, the
    // final return) with E0020. The if arm exits, so its move must
    // not reach the join or the back-edge merge at all.
    //   fn build(n: int) -> str:
    //       mut out = "["
    //       mut i = 0
    //       mut tag = "T"
    //       while i < n:
    //           str_push(&out, "x")
    //           if i == 2: return out
    //           i += 1
    //       print(out)
    //       print(tag)
    //       return "done"
    // `tag` is never moved, so the return epilogue must still destroy
    // it on the in-loop return path; `out` stays owned on the
    // fall-through path, so the last-use pass must free it after the
    // loop (its last read is the post-loop print).
    let (diags, mut sidecar, tirs, _pool) = check_src_full(
        "fn build(n: int) -> str:\n\tmut out = \"[\"\n\tmut i = 0\n\tmut tag = \"T\"\n\twhile i < n:\n\t\tstr_push(&out, \"x\")\n\t\tif i == 2:\n\t\t\treturn out\n\t\ti += 1\n\tprint(out)\n\tprint(tag)\n\treturn \"done\"\n",
    );
    assert!(
        diags.is_empty(),
        "returning an accumulator from inside a loop must not trip E0020; got: {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    let tir = &tirs[0];
    let out_owner = var_decl_init(tir, 0);
    let tag_owner = var_decl_init(tir, 2);
    let returns = find_tags(tir, ryo_core::tir::TirTag::Return);
    let loops = find_tags(tir, ryo_core::tir::TirTag::WhileLoop);
    assert_eq!(returns.len(), 2, "one in-loop return + the final return");
    assert_eq!(loops.len(), 1);
    // The return path is owned by the epilogue: `tag` (still owned at
    // the in-loop return) is freed there, while the returned `out`
    // moves out to the caller and must NOT be freed on that path.
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.after == returns[0] && fp.target == tag_owner),
        "the epilogue must free the still-owned `tag` on the in-loop return path; schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        !sc.free_schedule
            .iter()
            .any(|fp| fp.after == returns[0] && fp.target == out_owner),
        "the returned `out` moved out — no Free for it on the return path; schedule = {:?}",
        sc.free_schedule
    );
    // The loop-carried binding is freed after the loop: `out`'s only
    // Free anchors at its post-loop last use.
    let out_frees: Vec<_> = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.target == out_owner)
        .collect();
    assert_eq!(
        out_frees.len(),
        1,
        "exactly one Free for `out` (last-use, no branch-gated double); schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        out_frees[0].after.index() > loops[0].index(),
        "the `out` Free must anchor after the loop (its last read is post-loop); schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn trailing_all_return_if_loop_body_does_not_poison_backedge() {
    // Loop whose LAST statement is an if whose arms ALL return: the
    // body end never flows to the back-edge or the post-loop join, so
    // the moves in those arms must not enter the (entry ⊔ post-body)
    // merge. Pre-fix `after_flows_off_end` only recognized a literal
    // trailing Return/ReturnVoid and the post-loop `out = out + "z"`
    // use tripped a bogus E0020.
    //   fn f(c: bool) -> str:
    //       mut out = "a"
    //       while c:
    //           out = out + "b"
    //           if c:
    //               return out
    //           else:
    //               return out + "!"
    //       out = out + "z"
    //       return out
    let (diags, mut _sidecar, _tirs, _pool) = check_src_full(
        "fn f(c: bool) -> str:\n\tmut out = \"a\"\n\twhile c:\n\t\tout = out + \"b\"\n\t\tif c:\n\t\t\treturn out\n\t\telse:\n\t\t\treturn out + \"!\"\n\tout = out + \"z\"\n\treturn out\n",
    );
    assert!(
        diags.is_empty(),
        "a loop body ending in an all-return if must not poison the back-edge merge; got: {diags:?}"
    );
}

#[test]
fn move_on_fallthrough_arm_still_e0020() {
    // Negative guard for the semantics change: the exemption applies
    // ONLY to arms that exit. A value moved on an arm that falls
    // through still poisons the join — a post-`if` use must trip
    // E0020 exactly once.
    let diags = check_src(
        "fn consume(move s: str):\n\tprint(s)\n\nfn main():\n\tflag: bool = true\n\tname: str = \"Alice\"\n\tif flag:\n\t\tconsume(name)\n\tprint(name)\n",
    );
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::UseAfterMove),
        "expected UseAfterMove; got: {:?}",
        diags[0].code
    );
}

#[test]
fn nested_conditional_return_with_fallthrough_move_still_e0020() {
    // The join exemption must be MUST-terminate, not may-exit. The
    // then-arm below CONTAINS a return (nested under `if c`) but also
    // falls through with `x` moved, so it reaches the merge on the
    // c-is-false path and its Moved state must contribute: the
    // post-`if` `print(x)` is a use of a conditionally-moved value
    // and must trip E0020. A may-style predicate (any return at any
    // depth disqualifies the arm) silently drops the Moved state from
    // the join and the use-after-move compiles — the unsoundness this
    // test pins.
    //   fn f(move x: str, d: bool, c: bool) -> str:
    //       if d:
    //           if c:
    //               return x
    //           consume(x)
    //       print(x)
    //       return "done"
    let diags = check_src(
        "fn consume(move s: str):\n\tm = s + \"!\"\n\tprint(m)\n\nfn f(move x: str, d: bool, c: bool) -> str:\n\tif d:\n\t\tif c:\n\t\t\treturn x\n\t\tconsume(x)\n\tprint(x)\n\treturn \"done\"\n",
    );
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::UseAfterMove),
        "expected UseAfterMove; got: {:?}",
        diags[0].code
    );
}

#[test]
fn all_terminating_nested_if_arm_does_not_poison_join() {
    // Positive guard pairing with the test above: when the then-arm's
    // last statement is an if whose arms ALL terminate (both return),
    // the arm never reaches the merge, so the moves inside it are
    // path-local to the returns and a post-`if` use must NOT trip
    // E0020. Pins the recursive must-terminate predicate.
    //   fn g(move x: str, d: bool, c: bool) -> str:
    //       if d:
    //           if c:
    //               return x
    //           else:
    //               return x + "!"
    //       print(x)
    //       return "done"
    let diags = check_src(
        "fn g(move x: str, d: bool, c: bool) -> str:\n\tif d:\n\t\tif c:\n\t\t\treturn x\n\t\telse:\n\t\t\treturn x + \"!\"\n\tprint(x)\n\treturn \"done\"\n",
    );
    assert!(
        diags.is_empty(),
        "an all-terminating arm contributes no Moved state to the join; got: {diags:?}"
    );
}
