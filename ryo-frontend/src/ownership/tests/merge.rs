use super::super::*;
use super::common::*;

#[test]
fn branch_ids_unique_across_post_loop_if() {
    use ryo_core::span::Span;
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let print_name = pool.intern_str("print");
    let lit_a = pool.intern_str("a");
    let lit_b = pool.intern_str("b");
    let span = Span::EMPTY;

    // fn main() -> void:
    //     while false:
    //         if true:
    //             print("a")
    //     if true:
    //         print("b")
    //
    // The loop fixed-point re-walks the body; the speculative pass's
    // sidecar writes must not leak into the real schedule. The old
    // BranchId-based test passed vacuously when no gated frees were
    // scheduled; the structural equivalent pins the actual hazard:
    // at most one drop per (anchor, target) pair in the final
    // schedule. (The strong regression for the loop-re-walk shape
    // lives in `diverged_loop_writes_branch_gated_free_exactly_once`.)
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    let mut seen = HashSet::new();
    for d in &sidecar.drops {
        assert!(
            seen.insert((drop_anchor(d), d.target)),
            "duplicate drop for one (anchor, target) pair — a speculative \
             walk leaked into the real schedule: {d:?}"
        );
    }
}

#[test]
fn merge_branches_leaves_monotone_fields_untouched() {
    // Direct regression for Bug 4 in M8.1c. The loop merge starts
    // from the pre-loop entry; the monotone accumulators live on the
    // single `Ownership` walked in place across all arms and loop
    // passes, so a merge can never roll them backward. `BranchState`
    // doesn't carry them — pin that structurally: merging arms must
    // leave every monotone field untouched.
    let mut entry = Ownership::default();
    entry
        .owner_hazards
        // A dummy owner (place ids are opaque to the merge).
        .push((
            Owner(ryo_core::flow::PlaceId(0)),
            TirRef::from_raw(1),
            HazardKind::Move,
        ));
    let hazard_len = entry.owner_hazards.len();

    let arm = entry.snapshot_branch();
    entry.merge_branches(vec![arm.clone(), arm], &[true, true]);

    assert_eq!(
        entry.owner_hazards.len(),
        hazard_len,
        "merge_branches must not touch the monotone accumulators"
    );
}

/// Owner identities for the membership pins below — place ids are
/// opaque to the differ, any distinct values work.
fn owner(n: u32) -> Owner {
    Owner(ryo_core::flow::PlaceId(n))
}

#[test]
fn live_projection_membership_differ_extra_view() {
    // G3/R4-A: `{v1}` vs `{v1, v2}` are DIFFERENT states even though
    // both are non-empty — the old emptiness comparison claimed a fixed
    // point here. The merged set is what the next loop iteration
    // consumes, so an extra live view must force a re-walk.
    let mut own = Ownership::default();
    let root = owner(0);
    let v1 = owner(1);
    own.live_projections.insert(root, vec![v1]);
    let prev = own.snapshot_branch();
    own.live_projections.get_mut(&root).unwrap().push(owner(2));
    assert!(
        branch_states_differ(&prev, &own),
        "one extra live view on an already-live root must differ"
    );
}

#[test]
fn live_projection_membership_equal_reordered_duplicated() {
    // Order- and duplicate-insensitivity: the same semantic set in a
    // different list order (with a duplicate entry) is NOT a difference
    // — spurious re-walks would burn the height budget for nothing.
    let mut own = Ownership::default();
    let root = owner(0);
    own.live_projections.insert(root, vec![owner(1), owner(2)]);
    let prev = own.snapshot_branch();
    *own.live_projections.get_mut(&root).unwrap() = vec![owner(2), owner(1), owner(1)];
    assert!(
        !branch_states_differ(&prev, &own),
        "reordered/duplicated membership of the same set must compare equal"
    );
}

#[test]
fn live_projection_membership_partial_removal_still_differs() {
    // Freeze preservation: removing one view while another remains is
    // still a membership change. Both sides are non-empty, so the old
    // emptiness comparison missed BOTH directions of this shape.
    let mut own = Ownership::default();
    let root = owner(0);
    own.live_projections.insert(root, vec![owner(1), owner(2)]);
    let prev = own.snapshot_branch();
    // Shrink to a different single member.
    *own.live_projections.get_mut(&root).unwrap() = vec![owner(2)];
    assert!(
        branch_states_differ(&prev, &own),
        "a two-view set vs a different one-view subset must differ — a member was removed"
    );
    // And from the other side: a non-empty set vs the empty set.
    let prev = own.snapshot_branch();
    own.live_projections.get_mut(&root).unwrap().clear();
    assert!(
        branch_states_differ(&prev, &own),
        "a two-view set vs its one-view subset must differ — the root lost a live view"
    );
    // A missing key and a present-but-empty entry are the same empty
    // set: no difference.
    own.live_projections.remove(&root);
    let prev = own.snapshot_branch();
    own.live_projections.insert(root, Vec::new());
    assert!(
        !branch_states_differ(&prev, &own),
        "missing key vs empty entry are both the empty set — equal"
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
        .drops
        .iter()
        .filter(|fp| fp.target == out_owner)
        .collect();
    assert_eq!(
        out_frees.len(),
        1,
        "exactly one Free for `out` (last-use, no arm-conditional double); schedule = {:?}",
        sc.drops
    );
    assert!(
        out_frees[0].position.anchor_inst().unwrap().index() > returns[0].index(),
        "the `out` Free anchors after the if (its last read is post-if); schedule = {:?}",
        sc.drops
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
        sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == returns[0] && fp.target == tag_owner),
        "the epilogue must free the still-owned `tag` on the in-loop return path; schedule = {:?}",
        sc.drops
    );
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == returns[0] && fp.target == out_owner),
        "the returned `out` moved out — no Free for it on the return path; schedule = {:?}",
        sc.drops
    );
    // The loop-carried binding is freed after the loop: `out`'s only
    // Free anchors at its post-loop last use.
    let out_frees: Vec<_> = sc
        .drops
        .iter()
        .filter(|fp| fp.target == out_owner)
        .collect();
    assert_eq!(
        out_frees.len(),
        1,
        "exactly one Free for `out` (last-use, no arm-conditional double); schedule = {:?}",
        sc.drops
    );
    assert!(
        out_frees[0].position.anchor_inst().unwrap().index() > loops[0].index(),
        "the `out` Free must anchor after the loop (its last read is post-loop); schedule = {:?}",
        sc.drops
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

#[test]
fn elif_reseat_every_arm_suppresses_redundant_exit_frees() {
    // `mut s` declared before an if/elif, reseated with a heap
    // value in EVERY arm, read again after the chain. Each arm value's
    // last use is inside its arm, so the raw last-use anchors re-anchor
    // to the branch exit — but every such Free lowers through the
    // binding's home-slot redirect, and the slot holds exactly one
    // buffer per path. The pre-branch owner's own last-use Free
    // (anchored after the post-branch read, also slot-redirected)
    // covers all paths, so the per-arm exit Frees must be suppressed.
    // Before the suppression pass, both arm values kept exit Frees: a
    // double-free of the slot, and — because the non-last-write target
    // fails codegen's stale-target redirect filter — stale arm-local
    // cached values in the merge block (Cranelift verifier error).
    let src = "\
fn make(tag: int) -> str:
\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"

fn main():
\tmut s = make(0)
\tif false:
\t\ts = make(1)
\t\tprint(s)
\telif true:
\t\ts = make(2)
\t\tprint(s)
\tprint(s)
";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected: {diags:?}"
    );
    let idx = tirs
        .iter()
        .position(|t| pool.str(t.name) == "main")
        .unwrap();
    let sc = take_function_sidecar(&mut sidecar, idx);
    let tir = &tirs[idx];

    // Identify the producers: make(0) (pre-branch), make(1) and make(2)
    // (the arm reseats). Filter by callee name — the program also calls
    // print, which must not be mistaken for a make.
    let make_name = pool.intern_str("make");
    let calls: Vec<TirRef> = (1..tir.instructions.len())
        .map(|i| TirRef::from_raw(i as u32))
        .filter(|&r| {
            tir.inst(r).tag == ryo_core::tir::TirTag::Call && tir.call_view(r).name == make_name
        })
        .collect();
    let [make0, make1, make2] = calls.as_slice() else {
        panic!("expected exactly 3 make calls, got {calls:?}");
    };

    // Both reassign Frees survive (they release the displaced pre-branch
    // buffer inside each arm).
    assert_eq!(
        sc.free_on_reassign.iter().flatten().count(),
        2,
        "both arm reassigns must keep their displacement Free"
    );

    // The arm reseated values must have NO Free of their own — the
    // merge-seated pre-branch owner's Free releases the slot on every
    // path.
    for reseat in [*make1, *make2] {
        assert!(
            sc.drops.iter().all(|fp| fp.target != reseat),
            "reseated value %{:?} must not keep its own Free; schedule: {:?}",
            reseat,
            sc.drops
        );
    }
    // Exactly one Free for the pre-branch owner, anchored after the
    // post-branch read (a Var), so codegen's binding-path redirect
    // frees the path-correct slot content.
    let owner_frees: Vec<_> = sc.drops.iter().filter(|fp| fp.target == *make0).collect();
    assert_eq!(
        owner_frees.len(),
        1,
        "exactly one Free for the pre-branch owner; schedule: {:?}",
        sc.drops
    );
    assert_eq!(
        tir.inst(owner_frees[0].position.anchor_inst().unwrap()).tag,
        ryo_core::tir::TirTag::Var,
        "owner Free anchors after the post-branch read"
    );
}

#[test]
fn returning_arm_reseat_epilogue_skips_superseded_owner() {
    // Companion to the every-arm suppression above: arms that reseat a pre-branch
    // binding and then `return`. The return epilogue must free the
    // RESEATED value at the return, but not the pre-reassign owner —
    // the arm's reassign already released it via `free_on_reassign`,
    // so an epilogue Free for it double-frees (runtime trap). Only the
    // fall-through path still owns the pre-branch buffer; its Free is
    // the last-use Free after the post-branch read.
    let src = "\
fn make(tag: int) -> str:
\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"

fn f(c1: bool, c2: bool):
\tmut s = make(0)
\tif c1:
\t\ts = make(1)
\t\tprint(s)
\t\treturn
\telif c2:
\t\ts = make(2)
\t\tprint(s)
\t\treturn
\tprint(s)

fn main():
\tf(true, false)
";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected: {diags:?}"
    );
    let idx = tirs.iter().position(|t| pool.str(t.name) == "f").unwrap();
    let sc = take_function_sidecar(&mut sidecar, idx);
    let tir = &tirs[idx];

    let make_name = pool.intern_str("make");
    let calls: Vec<TirRef> = (1..tir.instructions.len())
        .map(|i| TirRef::from_raw(i as u32))
        .filter(|&r| {
            tir.inst(r).tag == ryo_core::tir::TirTag::Call && tir.call_view(r).name == make_name
        })
        .collect();
    let [make0, make1, make2] = calls.as_slice() else {
        panic!("expected 3 make calls, got {calls:?}");
    };
    let returns: Vec<TirRef> = (1..tir.instructions.len())
        .map(|i| TirRef::from_raw(i as u32))
        .filter(|&r| {
            matches!(
                tir.inst(r).tag,
                ryo_core::tir::TirTag::Return | ryo_core::tir::TirTag::ReturnVoid
            )
        })
        .collect();
    assert_eq!(returns.len(), 2, "expected 2 returns, got {returns:?}");

    // The pre-reassign owner keeps NO Free anchored at either return —
    // the arm reassigns released it. (Its only Free is the last-use
    // Free after the post-branch read.)
    for ret in &returns {
        assert!(
            sc.drops
                .iter()
                .all(|fp| fp.position.anchor_inst().unwrap() != *ret || fp.target != *make0),
            "pre-reassign owner must not be freed at a return whose path reseated it"
        );
    }
    // Each reseated value IS freed at its own return.
    for (reseat, ret) in [(*make1, returns[0]), (*make2, returns[1])] {
        assert!(
            sc.drops
                .iter()
                .any(|fp| fp.position.anchor_inst().unwrap() == ret && fp.target == reseat),
            "reseated value %{reseat:?} must be freed at its return {ret:?}"
        );
    }
    // The fall-through path still frees the pre-branch owner after the
    // post-branch read.
    let owner_frees: Vec<_> = sc.drops.iter().filter(|fp| fp.target == *make0).collect();
    assert_eq!(
        owner_frees.len(),
        1,
        "exactly one Free for the pre-branch owner; schedule: {:?}",
        sc.drops
    );
    assert_eq!(
        tir.inst(owner_frees[0].position.anchor_inst().unwrap()).tag,
        ryo_core::tir::TirTag::Var,
        "owner Free anchors after the post-branch read"
    );
}

#[test]
fn reseat_fallthrough_exit_free_targets_last_write() {
    // Pre-branch `mut` binding, conditional reseat, read inside a
    // LATER branch's arm, fall-through exit. The pre-branch owner's
    // last-use Free re-anchors to the later branch's exit, but the
    // owner was ALREADY released by the reassign's displacement Free on
    // the taken paths — codegen's stale-target redirect filter then
    // rejects the redirect and the cached-value fallback double-frees
    // (valgrind: Invalid free). The exit Free must instead target the
    // binding's LAST WRITE so the redirect filter passes and the slot's
    // path-correct content is freed exactly once on every path.
    let src = "\
fn make(tag: int) -> str:
\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"

fn f(c1: bool, c2: bool) -> int:
\tmut s = make(0)
\tif c1:
\t\ts = make(1)
\tif c2:
\t\tprint(s)
\t\treturn 0
\treturn 1

fn main():
\tf(true, false)
";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected: {diags:?}"
    );
    let idx = tirs.iter().position(|t| pool.str(t.name) == "f").unwrap();
    let sc = take_function_sidecar(&mut sidecar, idx);
    let tir = &tirs[idx];

    let make_name = pool.intern_str("make");
    let calls: Vec<TirRef> = (1..tir.instructions.len())
        .map(|i| TirRef::from_raw(i as u32))
        .filter(|&r| {
            tir.inst(r).tag == ryo_core::tir::TirTag::Call && tir.call_view(r).name == make_name
        })
        .collect();
    let [make0, make1] = calls.as_slice() else {
        panic!("expected exactly 2 make calls, got {calls:?}");
    };
    let ifs: Vec<TirRef> = (1..tir.instructions.len())
        .map(|i| TirRef::from_raw(i as u32))
        .filter(|&r| tir.inst(r).tag == ryo_core::tir::TirTag::IfStmt)
        .collect();
    let [if1, if2] = ifs.as_slice() else {
        panic!("expected exactly 2 if statements, got {ifs:?}");
    };

    // The reassign's displacement Free releases the pre-branch owner.
    assert!(
        sc.free_on_reassign.iter().flatten().any(|&t| t == *make0),
        "the reassign must displace the pre-branch owner: {:?}",
        sc.free_on_reassign
    );

    // No Free may target the pre-branch owner anywhere: the taken paths
    // released it at the reassign store, the not-taken paths release it
    // through the exit Free's redirect (as the slot's current content).
    assert!(
        sc.drops.iter().all(|fp| fp.target != *make0),
        "pre-branch owner must not keep its own Free; schedule: {:?}",
        sc.drops
    );

    // Exactly one Free anchors at the later branch's exit, and it
    // targets the binding's LAST WRITE (the reseated value), so
    // codegen's redirect filter (`last == target`) passes and frees the
    // slot's current content — the reseated buffer on taken paths, the
    // pre-branch buffer otherwise.
    let exit_frees: Vec<_> = sc
        .drops
        .iter()
        .filter(|fp| fp.position.anchor_inst().unwrap() == *if2)
        .collect();
    assert_eq!(
        exit_frees.len(),
        1,
        "exactly one Free at the later branch exit; schedule: {:?}",
        sc.drops
    );
    assert_eq!(
        exit_frees[0].target, *make1,
        "exit Free must target the binding's last write so the redirect passes"
    );
    // The first branch (the reseat) keeps no exit Free of its own.
    assert!(
        sc.drops
            .iter()
            .all(|fp| fp.position.anchor_inst().unwrap() != *if1),
        "no exit Free at the reseating branch; schedule: {:?}",
        sc.drops
    );
}
