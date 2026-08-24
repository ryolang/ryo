use super::*;

#[test]
fn viewofstr_arg_counts_as_borrow_rule7() {
    // T5/T7 carry-forward: fn two(inout a: str, b: strview) called as
    // two(&s, s) — sema wraps the second arg in ViewOfStr; the
    // Rule-7 borrow partition must look through the conversion and
    // diagnose the aliasing (E0032).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let void = pool.void();
    let main = pool.intern_str("main");
    let two = pool.intern_str("two");
    let s = pool.intern_str("s");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let inout_arg = tb.var(s, str_ty, span);
    let view_base = tb.var(s, str_ty, span);
    let view_arg = tb.view_of_str(view_base, view_ty, span);
    let call = tb.call(
        two,
        &[inout_arg, view_arg],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[decl, stmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::MutableAliasingViolation),
        "expected E0032 MutableAliasingViolation; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn viewofstr_read_counts_as_use_dead_store() {
    // T5/T7 carry-forward: an owned `str` used ONLY via a ViewOfStr
    // conversion must count as used — no W0001. Here s's sole read
    // is the owned side of the mixed `str == strview` comparison:
    //   s: str = "hi"; other: str = "yo"; if s == other[0:1]: print("x")
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
    let other = pool.intern_str("other");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_hi = tb.str_const(pool.intern_str("hi"), str_ty, span);
    let decl_s = tb.var_decl(s, false, str_ty, lit_hi, span);
    let lit_yo = tb.str_const(pool.intern_str("yo"), str_ty, span);
    let decl_o = tb.var_decl(other, false, str_ty, lit_yo, span);
    let obase = tb.var(other, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i1 = tb.int_const(1, int_ty, span);
    let sl = tb.slice(obase, Some(i0), Some(i1), view_ty, span);
    let sread = tb.var(s, str_ty, span);
    let vos = tb.view_of_str(sread, view_ty, span);
    let eq = tb.binary(TirTag::StrCmpEq, bool_ty, vos, sl, span);
    let xlit = tb.str_const(pool.intern_str("x"), str_ty, span);
    let pcall = tb.call(print, &[xlit], &all_borrow(&[xlit]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let ifs = tb.if_stmt(eq, &[pstmt], &[], None, void, span);
    let tir = tb.finish(&[decl_s, decl_o, ifs]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "expected no diagnostics — the ViewOfStr read must count as a use of s"
    );
}

#[test]
fn viewasstr_arg_counts_as_borrow_rule7() {
    // P6' carry-forward: fn two(inout a: str, b: str) called as
    // two(&s, s[0:1]) — sema wraps the second arg in ViewAsStr; the
    // Rule-7 borrow partition must look through the conversion and
    // diagnose the aliasing (E0032), same as the ViewOfStr case.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let two = pool.intern_str("two");
    let s = pool.intern_str("s");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let inout_arg = tb.var(s, str_ty, span);
    let slice_base = tb.var(s, str_ty, span);
    let zero = tb.int_const(0, int_ty, span);
    let one = tb.int_const(1, int_ty, span);
    let slice = tb.slice(slice_base, Some(zero), Some(one), view_ty, span);
    let reborrow = tb.view_as_str(slice, str_ty, span);
    let call = tb.call(
        two,
        &[inout_arg, reborrow],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[decl, stmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::MutableAliasingViolation),
        "expected E0032 MutableAliasingViolation; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn viewasstr_reborrow_is_call_scoped() {
    // P6': the re-borrow lives only for the call's duration — the
    // root owner can still be moved afterwards (no freeze, no
    // aliasing), unlike a bound slice projection.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let show = pool.intern_str("show");
    let eat = pool.intern_str("eat");
    let s = pool.intern_str("s");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let slice_base = tb.var(s, str_ty, span);
    let zero = tb.int_const(0, int_ty, span);
    let one = tb.int_const(1, int_ty, span);
    let slice = tb.slice(slice_base, Some(zero), Some(one), view_ty, span);
    let reborrow = tb.view_as_str(slice, str_ty, span);
    let show_call = tb.call(show, &[reborrow], &[ParamMode::Borrow], void, span);
    let show_stmt = tb.unary(TirTag::ExprStmt, void, show_call, span);
    // `s` moved later in the caller — fine: the re-borrow ended
    // with the `show` call.
    let moved = tb.var(s, str_ty, span);
    let eat_call = tb.call(eat, &[moved], &[ParamMode::Move], void, span);
    let eat_stmt = tb.unary(TirTag::ExprStmt, void, eat_call, span);
    let tir = tb.finish(&[decl, show_stmt, eat_stmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "expected no diagnostics — the re-borrow is call-scoped; got: {:?}",
        sink.into_diags()
    );
}

#[test]
fn str_materialize_result_is_fresh_owner() {
    // M8.4.1.2: `fn show(text: strview) -> str: return str(text)` —
    // the materialized copy is a fresh owner by construction (the
    // str-returning-Call seeding), so returning it is the sanctioned
    // escape fix: no diagnostics, no ownership special-casing.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirData, TirParam, TirTag};

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let show = pool.intern_str("show");
    let text = pool.intern_str("text");
    let materialize = pool.intern_str("__ryo_str_from_view");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(
        show,
        vec![TirParam {
            name: text,
            ty: view_ty,
            mode: ParamMode::Borrow,
            span,
        }],
        str_ty,
        span,
    );
    let arg = tb.var(text, view_ty, span);
    let copy = tb.call(materialize, &[arg], &[ParamMode::Borrow], str_ty, span);
    let ret = tb.push_typed(TirTag::Return, TirData::UnOp(copy), str_ty, span);
    let tir = tb.finish(&[ret]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "materialize-and-return must be clean — the copy is a fresh owner; got: {:?}",
        sink.into_diags()
    );
}

#[test]
fn str_materialize_arg_counts_as_borrow_rule7() {
    // M8.4.1.2: fn two(inout a: str, b: str) called as
    // two(&s, str(s[0:1])) — the materialization READS the view's
    // buffer at call time, so it counts as an immutable borrow of
    // `s` for the call's duration (E4): the Rule-7 partition must
    // look through `__ryo_str_from_view` to the view's root, exactly
    // like the ViewAsStr case (viewasstr_arg_counts_as_borrow_rule7).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let two = pool.intern_str("two");
    let s = pool.intern_str("s");
    let hello = pool.intern_str("hello");
    let materialize = pool.intern_str("__ryo_str_from_view");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let inout_arg = tb.var(s, str_ty, span);
    let slice_base = tb.var(s, str_ty, span);
    let zero = tb.int_const(0, int_ty, span);
    let one = tb.int_const(1, int_ty, span);
    let slice = tb.slice(slice_base, Some(zero), Some(one), view_ty, span);
    let copy = tb.call(materialize, &[slice], &[ParamMode::Borrow], str_ty, span);
    let call = tb.call(
        two,
        &[inout_arg, copy],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[decl, stmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic; got: {diags:?}"
    );
    assert!(
        matches!(diags[0].code, DiagCode::MutableAliasingViolation),
        "expected E0032 MutableAliasingViolation; got: {:?}",
        diags[0].code
    );
    assert!(
        diags[0].message.contains("`s`"),
        "expected the diagnostic to name `s`; got: {}",
        diags[0].message
    );
}

#[test]
fn loop_view_created_after_owner_mutation_dies_within_iteration() {
    // A view VarDecl AFTER an inout-consume of the owner inside the
    // SAME while body:
    //   s: str = "hello"
    //   while true:
    //       str_push(&s, "!")   # mutates the owner
    //       v = s[0:2]          # projection created after the consume
    //       print(v)            # v's only read, same iteration
    // → NO diagnostic. The view is created and read inside the same
    // loop body, so it is NOT loop-deferred (deferral requires the
    // last read in a loop the creation is outside of — see
    // collect_view_liveness); it dies at `print(v)` within pass 1,
    // the back-edge state tuple is unchanged, and no re-walk fires.
    // That is the sound outcome: every iteration's `str_push` runs
    // before that iteration's fresh slice is taken, so no stale
    // view pointer is ever read. (The unsound sibling — view
    // created OUTSIDE the loop, mutated inside — is covered by
    // `loop_deferred_view_survives_if_join_prune`.)
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
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let wl = tb.while_loop(cond, &[push_stmt, vdecl, pstmt], void, span);
    let tir = tb.finish(&[decl, wl]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "expected no diagnostics — the view dies within each iteration, \
         before the next iteration's str_push; got: {:?}",
        sink.into_diags()
    );
}

#[test]
fn loop_view_live_at_back_edge_flags_earlier_mutation_on_rewalk() {
    // Pins the full-tuple convergence check's re-walk DISCOVERY
    // path (the sibling acceptance test above pins the converge
    // side): a body-created view that is STILL LIVE at the
    // back-edge forces pass 2, and only pass 2
    // sees the projection at the earlier owner-consume.
    //   s: str = "hello"
    //   suffix: str = "!"
    //   while true:
    //       str_push(&s, suffix)  # pass 1: no projection exists yet
    //       v = s[0:2]            # registers the projection
    // v is never read: an unread view has no last use, so its
    // projection lives to scope end (P4) and is non-empty at the
    // back-edge → the live-projection leg of the state tuple
    // differs → re-walk → pass 2 walks `str_push` with the
    // projection live → exactly one SourceProjected (E0035) naming
    // `s`. The mutation IS unsound here: iteration 2's push
    // reallocs while v (never killed by a read) points into the
    // old buffer.
    //
    // The suffix is deliberately bound OUTSIDE the loop: a StrConst
    // inside the body would enter `states` as a fresh Valid temp
    // and flip the owner-state leg of the tuple, forcing the
    // re-walk even without the projection-emptiness comparison.
    // With the body kept allocation-free, the projection leg is the
    // ONLY re-walk trigger — verified by mutation: skipping the
    // live-projection comparison in `states_differ_snapshot` makes
    // this test fail with 0 diagnostics. A refactor re-narrowing
    // the convergence comparison (e.g. back to Moved-ness only)
    // drops the re-walk and MUST fail this test.
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
    let suffix = pool.intern_str("suffix");
    let v = pool.intern_str("v");
    let str_push = pool.intern_str("str_push");
    let hello = pool.intern_str("hello");
    let bang = pool.intern_str("!");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let bang_lit = tb.str_const(bang, str_ty, span);
    let suffix_decl = tb.var_decl(suffix, false, str_ty, bang_lit, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sread = tb.var(s, str_ty, span);
    let suffix_read = tb.var(suffix, str_ty, span);
    let push = tb.call(
        str_push,
        &[sread, suffix_read],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let push_stmt = tb.unary(TirTag::ExprStmt, void, push, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let wl = tb.while_loop(cond, &[push_stmt, vdecl], void, span);
    let tir = tb.finish(&[decl, suffix_decl, wl]);

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
fn view_and_move_args_same_owner_rejected() {
    // fn two(a: strview, move b: str) called as two(s[0:1], s) —
    // both args share root owner `s`. The view arg borrows the root
    // for the whole call (E4), so the move in the same call is a
    // P2 freeze violation: exactly one SourceProjected (E0035).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let two = pool.intern_str("two");
    let s = pool.intern_str("s");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i1 = tb.int_const(1, int_ty, span);
    let view_arg = tb.slice(base, Some(i0), Some(i1), view_ty, span);
    let move_arg = tb.var(s, str_ty, span);
    let call = tb.call(
        two,
        &[view_arg, move_arg],
        &[ParamMode::Borrow, ParamMode::Move],
        void,
        span,
    );
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[decl, stmt]);

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
    // Diagnostic-quality gap: the message names "value", not `s` —
    // the view/move-overlap path resolves the name via
    // `owner_name_for_diag`, which inspects the owner's initializer
    // (a StrConst) and falls back to "value", unlike the Rule-7
    // E0032 path that scans the call args for the `Var` read
    // (`rule7_owner_name`). Pinned as-is; a future fix should name
    // the binding.
    assert!(
        diags[0].message.contains("value"),
        "expected the current message wording; got: {}",
        diags[0].message
    );
}

#[test]
fn p5_view_read_inside_if_arm_anchors_free_at_if_exit() {
    // P5 deferral ACROSS a branch — distinct from the plain last-use
    // lift in `last_use_inside_if_anchors_after_if`: the owner's own
    // last read is the slice creation OUTSIDE the if; only the
    // projection's last read is inside the then-arm. P5 defers the
    // owner's destruction to that read (final spec §3.2), and the
    // conditional re-anchor must lift the FreePoint to the IfStmt
    // exit — anchoring inside the arm would leak on the not-taken
    // path.
    //   s: str = "hello"
    //   v = s[0:2]
    //   if true: print(v)
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
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let ifs = tb.if_stmt(cond, &[pstmt], &[], None, void, span);
    let tir = tb.finish(&[decl, vdecl, ifs]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sink.is_empty(),
        "expected no diagnostics; got: {:?}",
        sink.into_diags()
    );
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.target == lit && fp.after == ifs && fp.branch.is_none()),
        "expected s's Free anchored at the IfStmt exit (P5 deferral across the branch); schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        !sc.free_schedule
            .iter()
            .any(|fp| fp.target == lit && fp.after != ifs),
        "no Free for s may anchor inside the arm (leaks on the not-taken path); schedule = {:?}",
        sc.free_schedule
    );
}
