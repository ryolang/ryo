use super::*;

#[test]
fn view_across_branches() {
    // s: str = "hello"; v = s[0:2]; if flag: print(v) else: print("x"); consume(s)
    // → legal: v's last use is inside the branch; freeze lifted at
    //   the join; P5 keeps s alive through the branch.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let consume = pool.intern_str("consume");
    let hello = pool.intern_str("hello");
    let x = pool.intern_str("x");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let xlit = tb.str_const(x, str_ty, span);
    let xcall = tb.call(print, &[xlit], &all_borrow(&[xlit]), void, span);
    let xstmt = tb.unary(TirTag::ExprStmt, void, xcall, span);
    let ifs = tb.if_stmt(cond, &[pstmt], &[], Some(&[xstmt]), void, span);
    let sread = tb.var(s, str_ty, span);
    let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
    let tir = tb.finish(&[decl, vdecl, ifs, cstmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "expected no diagnostics (freeze lifted at the join)"
    );
}

#[test]
fn freeze_is_per_arm_precise_across_if_arms() {
    // Per-arm view_last_use (author decision): during an arm walk a
    // view's last use for freeze purposes is its last read on the
    // path THROUGH that arm, not the global max over all arms.
    //   s: str = "hello"; v = s[0:2]
    //   if cond: print(v); consume(s)   # v dead before the move, this path
    //   else: print(v)
    // → legal: on the then-path v's last read completes before the
    //   consume; the else-arm read is not on that path.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let consume = pool.intern_str("consume");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let vread_t = tb.var(v, view_ty, span);
    let pcall_t = tb.call(print, &[vread_t], &all_borrow(&[vread_t]), void, span);
    let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
    let sread = tb.var(s, str_ty, span);
    let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
    let vread_e = tb.var(v, view_ty, span);
    let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
    let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
    let ifs = tb.if_stmt(cond, &[pstmt_t, cstmt], &[], Some(&[pstmt_e]), void, span);
    let tir = tb.finish(&[decl, vdecl, ifs]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "expected no diagnostics (v is dead on the then-path before the move); got: {:?}",
        sink.into_diags()
    );
}

#[test]
fn per_arm_override_respects_loop_deferral() {
    // Regression (review Critical on 2cbaa06): the per-arm override
    // must not install an arm-local last read that sits inside a
    // loop the creation is outside of — the walk-constant
    // `view_defer_loop` is computed from the GLOBAL max read only,
    // so the death site would drain the projection mid-loop and
    // un-freeze a later owner mutation in the same body.
    //   s: str = "hello"; v = s[0:2]
    //   if true:
    //       while true:
    //           print(v)          # arm-local last read, inside loop
    //           str_push(&s, "xxxx")  # realloc; iteration 2 re-reads v
    //   else:
    //       print(v)              # global max read — outside any loop
    // → exactly one SourceProjected naming `s` (P4 deferral holds
    //   per arm: the in-loop read keeps v live through the loop).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let str_push = pool.intern_str("str_push");
    let hello = pool.intern_str("hello");
    let x4 = pool.intern_str("xxxx");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let if_cond = tb.bool_const(true, bool_ty, span);
    let wcond = tb.bool_const(true, bool_ty, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let sread = tb.var(s, str_ty, span);
    let suffix = tb.str_const(x4, str_ty, span);
    let push = tb.call(
        str_push,
        &[sread, suffix],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
    let wl = tb.while_loop(wcond, &[pstmt, push_stmt], void, span);
    let vread_e = tb.var(v, view_ty, span);
    let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
    let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
    let ifs = tb.if_stmt(if_cond, &[wl], &[], Some(&[pstmt_e]), void, span);
    let tir = tb.finish(&[decl, vdecl, ifs]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::SourceProjected),
        "expected SourceProjected; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn per_arm_kill_respects_loop_deferral() {
    // Kill-side companion to `per_arm_override_respects_loop_deferral`:
    // the arm-kill fires only when the arm has NO reads of the view
    // in its subtree, so there is no deeper arm-local read to
    // strand — and a deeper read in a SIBLING arm is itself the
    // global max read, which the pre-pass deferral already covers.
    //   s: str = "hello"; v = s[0:2]
    //   if true:
    //       consume(s)        # kill candidate: no reads of v here
    //   else:
    //       while true:
    //           print(v)      # global max read, deeper than creation
    //                         #   → loop-deferred → kill/override skipped
    // → exactly one SourceProjected naming `s` (v is live in the
    //   then-arm through the deferral exemption).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let consume = pool.intern_str("consume");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let if_cond = tb.bool_const(true, bool_ty, span);
    let sread = tb.var(s, str_ty, span);
    let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
    let wcond = tb.bool_const(true, bool_ty, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let wl = tb.while_loop(wcond, &[pstmt], void, span);
    let ifs = tb.if_stmt(if_cond, &[cstmt], &[], Some(&[wl]), void, span);
    let tir = tb.finish(&[decl, vdecl, ifs]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::SourceProjected),
        "expected SourceProjected; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn per_arm_kill_without_deferral_stays_scoped() {
    // Non-deferred companion to `per_arm_kill_respects_loop_deferral`:
    // the kill needs no loop guard because it fires only in an arm
    // with NO reads of the view — a deeper read in a SIBLING arm is
    // the global max read and lies on a different path, so the view
    // is genuinely dead on the no-read arm's path and moving the
    // owner there is sound. Soundness rests on the per-arm
    // `live_projections` snapshot scoping the kill to that arm's
    // walk: the else arm below must still see the view live.
    //   s: str = "hello"; v = s[0:2]
    //   if c1: print(v)               # sibling read of v
    //   elif c2: consume(s)           # no reads of v → kill → LEGAL
    //   else: consume(s); print(v)    # v live until its read → E0035
    // → exactly one SourceProjected naming `s` (from the else arm):
    //   the elif move is accepted (the kill fired without deferral)
    //   and the else move is rejected (the kill did not leak across
    //   arms — had the elif walk left the projection drained, the
    //   else-arm move would be silently accepted too).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let consume = pool.intern_str("consume");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond1 = tb.bool_const(true, bool_ty, span);
    let cond2 = tb.bool_const(true, bool_ty, span);
    let vread_t = tb.var(v, view_ty, span);
    let pcall_t = tb.call(print, &[vread_t], &all_borrow(&[vread_t]), void, span);
    let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
    let sread_elif = tb.var(s, str_ty, span);
    let ccall_elif = tb.call(consume, &[sread_elif], &[ParamMode::Move], void, span);
    let cstmt_elif = tb.unary(TirTag::ExprStmt, void, ccall_elif, span);
    let sread_e = tb.var(s, str_ty, span);
    let ccall_e = tb.call(consume, &[sread_e], &[ParamMode::Move], void, span);
    let cstmt_e = tb.unary(TirTag::ExprStmt, void, ccall_e, span);
    let vread_e = tb.var(v, view_ty, span);
    let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
    let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
    let ifs = tb.if_stmt(
        cond1,
        &[pstmt_t],
        &[(cond2, vec![cstmt_elif])],
        Some(&[cstmt_e, pstmt_e]),
        void,
        span,
    );
    let tir = tb.finish(&[decl, vdecl, ifs]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::SourceProjected),
        "expected SourceProjected; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn per_arm_override_applies_to_elif_arms() {
    // Elif arms go through `refine_view_liveness_for_arm` like the
    // then/else arms (arm index 1 + elif_index, with the pre-pass's
    // `arm_reads` laid out then/elif…/else): a view read in the elif
    // arm whose global last use lies in a LATER arm dies at its
    // elif-local last read, un-freezing the owner for the rest of
    // the elif arm. Were elif arms skipped, the view would stay live
    // to the else-arm read and the move below would be spuriously
    // diagnosed — this is the only per-arm shape that distinguishes
    // a working elif path from a missing one.
    //   s: str = "hello"; v = s[0:2]
    //   if c1: print("t")             # no reads of v (kill — inert)
    //   elif c2: print(v); consume(s) # move AFTER v's elif-local
    //                                 #   last read → LEGAL
    //   else: print(v)                # global max read (later arm)
    // → no diagnostics.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let consume = pool.intern_str("consume");
    let hello = pool.intern_str("hello");
    let tee = pool.intern_str("t");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond1 = tb.bool_const(true, bool_ty, span);
    let cond2 = tb.bool_const(true, bool_ty, span);
    let tlit = tb.str_const(tee, str_ty, span);
    let pcall_t = tb.call(print, &[tlit], &all_borrow(&[tlit]), void, span);
    let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
    let vread_elif = tb.var(v, view_ty, span);
    let pcall_elif = tb.call(print, &[vread_elif], &all_borrow(&[vread_elif]), void, span);
    let pstmt_elif = tb.unary(TirTag::ExprStmt, void, pcall_elif, span);
    let sread = tb.var(s, str_ty, span);
    let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
    let vread_e = tb.var(v, view_ty, span);
    let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
    let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
    let ifs = tb.if_stmt(
        cond1,
        &[pstmt_t],
        &[(cond2, vec![pstmt_elif, cstmt])],
        Some(&[pstmt_e]),
        void,
        span,
    );
    let tir = tb.finish(&[decl, vdecl, ifs]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "expected no diagnostics (v dies at its elif-local last read); got: {:?}",
        sink.into_diags()
    );
}

#[test]
fn view_created_in_loop_body_per_arm_kill_applies() {
    // Loop deferral (`view_defer_loop`) covers only views created
    // OUTSIDE the loop their last read sits in (`created_in <
    // read_in`). A view created INSIDE the loop body is re-sliced
    // from the current buffer every iteration, so the back-edge
    // cannot strand a stale read: the deferral does not apply and
    // the per-arm kill fires — on the arm with no reads of v the
    // owner mutation is sound and must NOT be diagnosed.
    //   s: str = "hello"
    //   while true:
    //       v = s[0:2]
    //       if c: str_push(&s, "!")   # no reads of v → kill → LEGAL
    //       else: print(v)
    // → no diagnostics (no spurious SourceProjected).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let str_push = pool.intern_str("str_push");
    let hello = pool.intern_str("hello");
    let bang = pool.intern_str("!");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let wcond = tb.bool_const(true, bool_ty, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sread = tb.var(s, str_ty, span);
    let suffix = tb.str_const(bang, str_ty, span);
    let push = tb.call(
        str_push,
        &[sread, suffix],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let ifs = tb.if_stmt(cond, &[push_stmt], &[], Some(&[pstmt]), void, span);
    let wl = tb.while_loop(wcond, &[vdecl, ifs], void, span);
    let tir = tb.finish(&[decl, wl]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "expected no diagnostics (v is re-sliced each iteration and dead on the push arm); got: {:?}",
        sink.into_diags()
    );
}

#[test]
fn view_created_in_loop_body_freeze_holds_before_read() {
    // Flip side of `view_created_in_loop_body_per_arm_kill_applies`:
    // in-loop creation must not disable the freeze on the arm that
    // DOES read the view — the mutation precedes v's read on the
    // same path, so v is live at the mutation site. This is the
    // no-false-UAF-acceptance direction: the realloc in str_push
    // would leave v pointing at freed memory when the read runs.
    //   s: str = "hello"
    //   while true:
    //       v = s[0:2]
    //       if c: str_push(&s, "!"); print(v)   # mutation while v live
    // → exactly one SourceProjected naming `s`.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let str_push = pool.intern_str("str_push");
    let hello = pool.intern_str("hello");
    let bang = pool.intern_str("!");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let wcond = tb.bool_const(true, bool_ty, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sread = tb.var(s, str_ty, span);
    let suffix = tb.str_const(bang, str_ty, span);
    let push = tb.call(
        str_push,
        &[sread, suffix],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let ifs = tb.if_stmt(cond, &[push_stmt, pstmt], &[], None, void, span);
    let wl = tb.while_loop(wcond, &[vdecl, ifs], void, span);
    let tir = tb.finish(&[decl, wl]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::SourceProjected),
        "expected SourceProjected; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn freeze_holds_before_arm_local_last_read() {
    // Contract: a move of the owner BEFORE the view's arm-local last
    // read stays rejected — per-arm precision does not weaken the
    // intra-arm freeze.
    //   s: str = "hello"; v = s[0:2]
    //   if cond: consume(s); print(v)   # move precedes v's last read
    //   else: print(v)
    // → exactly one SourceProjected naming `s`.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let consume = pool.intern_str("consume");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sread = tb.var(s, str_ty, span);
    let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
    let vread_t = tb.var(v, view_ty, span);
    let pcall_t = tb.call(print, &[vread_t], &all_borrow(&[vread_t]), void, span);
    let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
    let vread_e = tb.var(v, view_ty, span);
    let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
    let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
    let ifs = tb.if_stmt(cond, &[cstmt, pstmt_t], &[], Some(&[pstmt_e]), void, span);
    let tir = tb.finish(&[decl, vdecl, ifs]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::SourceProjected),
        "expected SourceProjected; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn freeze_holds_in_arm_when_view_read_after_join() {
    // Contract: a view read AFTER the join keeps the owner frozen in
    // every arm — the post-join read lies on every path, so no arm
    // may refine it away.
    //   s: str = "hello"; v = s[0:2]
    //   if cond: print(v)
    //   else: consume(s)      # v still live (read after the if)
    //   print(v)
    // → exactly one SourceProjected naming `s`.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let consume = pool.intern_str("consume");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let vread_t = tb.var(v, view_ty, span);
    let pcall_t = tb.call(print, &[vread_t], &all_borrow(&[vread_t]), void, span);
    let pstmt_t = tb.unary(TirTag::ExprStmt, void, pcall_t, span);
    let sread = tb.var(s, str_ty, span);
    let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
    let ifs = tb.if_stmt(cond, &[pstmt_t], &[], Some(&[cstmt]), void, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let tir = tb.finish(&[decl, vdecl, ifs, pstmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::SourceProjected),
        "expected SourceProjected; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn move_in_arm_where_view_is_dead_ok_but_post_join_use_is_uam() {
    // Contract: on a path with no remaining view reads the owner may
    // move — but the conditional-move machinery still guards the
    // join: using the owner after the if is use-after-move.
    //   s: str = "hello"; v = s[0:2]
    //   if cond: consume(s)   # v is never read on this path → legal
    //   else: print(v)
    //   print(s)              # moved on the then-path → E0020
    // → exactly one UseAfterMove (and NO SourceProjected).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let consume = pool.intern_str("consume");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sread = tb.var(s, str_ty, span);
    let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
    let vread_e = tb.var(v, view_ty, span);
    let pcall_e = tb.call(print, &[vread_e], &all_borrow(&[vread_e]), void, span);
    let pstmt_e = tb.unary(TirTag::ExprStmt, void, pcall_e, span);
    let ifs = tb.if_stmt(cond, &[cstmt], &[], Some(&[pstmt_e]), void, span);
    let sread2 = tb.var(s, str_ty, span);
    let pcall = tb.call(print, &[sread2], &all_borrow(&[sread2]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let tir = tb.finish(&[decl, vdecl, ifs, pstmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
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
fn loop_deferred_view_stays_frozen_across_arm_without_read() {
    // Contract: per-arm refinement must NOT apply to loop-deferred
    // views — a later iteration re-reads the view through the
    // back-edge, so it is live on every arm's path inside the loop.
    //   s: str = "hello"; v = s[0:2]
    //   while true:
    //       if cond: str_push(&s, "!")   # v unread on this arm's path
    //       else: print(v)               # …but re-read next iteration
    // → exactly one SourceProjected naming `s`.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let str_push = pool.intern_str("str_push");
    let hello = pool.intern_str("hello");
    let bang = pool.intern_str("!");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let wcond = tb.bool_const(true, bool_ty, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sread = tb.var(s, str_ty, span);
    let suffix = tb.str_const(bang, str_ty, span);
    let push = tb.call(
        str_push,
        &[sread, suffix],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let ifs = tb.if_stmt(cond, &[push_stmt], &[], Some(&[pstmt]), void, span);
    let wl = tb.while_loop(wcond, &[ifs], void, span);
    let tir = tb.finish(&[decl, vdecl, wl]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::SourceProjected),
        "expected SourceProjected; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn for_range_deferred_view_stays_frozen_across_arm_without_read() {
    // Same contract as loop_deferred_view_stays_frozen_across_arm_without_read
    // but through the ForRange path (loops::analyze_for_range →
    // views::remove_loop_deferred_views instead of the while-loop call in
    // loops::analyze_while_loop):
    //   s: str = "hello"; v = s[0:2]
    //   for i in range(0, 3):
    //       if cond: str_push(&s, "!")   # v unread on this arm's path
    //       else: print(v)               # …but re-read next iteration
    // → exactly one SourceProjected naming `s`.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let i = pool.intern_str("i");
    let print = pool.intern_str("print");
    let str_push = pool.intern_str("str_push");
    let hello = pool.intern_str("hello");
    let bang = pool.intern_str("!");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sread = tb.var(s, str_ty, span);
    let suffix = tb.str_const(bang, str_ty, span);
    let push = tb.call(
        str_push,
        &[sread, suffix],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let ifs = tb.if_stmt(cond, &[push_stmt], &[], Some(&[pstmt]), void, span);
    let i0b = tb.int_const(0, int_ty, span);
    let i3 = tb.int_const(3, int_ty, span);
    let fr = tb.for_range(i, i0b, i3, &[ifs], void, span);
    let tir = tb.finish(&[decl, vdecl, fr]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::SourceProjected),
        "expected SourceProjected; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn view_in_loop_body_converges() {
    // s: str = "hello"; while true: v = s[0:2]; print(v)
    // → converges (full-tuple comparison); no spurious
    //   SourceProjected on the second iteration; s freed after the
    //   loop.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let wl = tb.while_loop(cond, &[vdecl, pstmt], void, span);
    let tir = tb.finish(&[decl, wl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sink.is_empty(),
        "expected no diagnostics (no spurious SourceProjected)"
    );
    // s's Free is anchored after the loop (its last use — through
    // the projection — is inside the loop body; the conditional
    // re-anchor moves it to the loop exit).
    assert!(
        sidecar
            .free_schedule
            .iter()
            .any(|fp| fp.target == lit && fp.after == wl && fp.branch.is_none()),
        "expected s's Free anchored after the loop; got: {:?}",
        sidecar.free_schedule
    );
}

#[test]
fn loop_deferred_view_survives_if_join_prune() {
    // Regression test (review Critical): prune_branch_dead_projections
    // must not kill a loop-deferred view at the if join.
    //   s: str = "hello"; v = s[0:2]
    //   while true:
    //       if true: print(v)     # v's last read → loop-deferred (P4)
    //       str_push(&s, "!")     # mutates the owner while v is live
    // → exactly one SourceProjected naming `s` (P2). Before the fix
    //   the if-join prune emptied live_projections[s] mid-loop-body,
    //   silently accepting a mutation whose realloc later iterations
    //   would read through v's stale pointer — the UAF class P2
    //   exists to reject.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let str_push = pool.intern_str("str_push");
    let hello = pool.intern_str("hello");
    let bang = pool.intern_str("!");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let while_cond = tb.bool_const(true, bool_ty, span);
    let if_cond = tb.bool_const(true, bool_ty, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let ifs = tb.if_stmt(if_cond, &[pstmt], &[], None, void, span);
    let sread = tb.var(s, str_ty, span);
    let suffix = tb.str_const(bang, str_ty, span);
    let push = tb.call(
        str_push,
        &[sread, suffix],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
    let wl = tb.while_loop(while_cond, &[ifs, push_stmt], void, span);
    let tir = tb.finish(&[decl, vdecl, wl]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::SourceProjected),
        "expected SourceProjected; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}
