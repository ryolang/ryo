use super::*;
use common::*;

#[test]
fn param_slice_read_defers_free_to_projection_last_use() {
    // fn f(move s: str): v = s[0:2]; print(v); print(42) — the
    // slice projects the param, so its Free defers to the
    // projection's last use (the read inside print(v)), past the
    // param's own last direct read.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let f = pool.intern_str("f");
    let s_name = pool.intern_str("s");
    let v_name = pool.intern_str("v");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        f,
        vec![TirParam {
            name: s_name,
            ty: str_ty,
            mode: ParamMode::Move,
            span,
        }],
        void,
        span,
    );
    let base = tb.var(s_name, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i2), view_ty, span);
    let vdecl = tb.var_decl(v_name, false, view_ty, sl, span);
    let vread = tb.var(v_name, view_ty, span);
    let call1 = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let n = tb.int_const(42, int_ty, span);
    let call2 = tb.call(print, &[n], &all_borrow(&[n]), void, span);
    let tir = tb.finish(&[vdecl, call1, call2]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);

    let frees: Vec<_> = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.target == TirRef::param(0))
        .collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the owned param; schedule = {:?}",
        sc.free_schedule
    );
    assert_eq!(
        frees[0].after, vread,
        "a slice of the param defers its Free to the projection's last use; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn move_and_viewasstr_borrow_of_one_owner_reports_borrow_note() {
    // `two(move s, s[0:1])` against a `(move, str)` signature — the
    // P6'-converted ViewAsStr arg borrows the view's ROOT owner, so
    // E0031 must carry the "borrowed here" note (look through the
    // conversion, like the Rule-7 partition does).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let two = pool.intern_str("two");
    let span = SimpleSpan::new((), 0..0);
    // Distinct spans for the move arg and the reborrow chain, so
    // the note can be pinned to the reborrow specifically.
    let move_span = SimpleSpan::new((), 10..11);
    let reborrow_span = SimpleSpan::new((), 20..26);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let sv1 = tb.var(s, str_ty, move_span);
    let sv2 = tb.var(s, str_ty, reborrow_span);
    let zero = tb.int_const(0, int_ty, reborrow_span);
    let one = tb.int_const(1, int_ty, reborrow_span);
    let sl = tb.slice(sv2, Some(zero), Some(one), view_ty, reborrow_span);
    let reborrow = tb.view_as_str(sl, str_ty, reborrow_span);
    let modes = vec![ParamMode::Move, ParamMode::Borrow];
    let call = tb.call(two, &[sv1, reborrow], &modes, void, span);
    let tir = tb.finish(&[decl, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    let e0031: Vec<_> = diags
        .iter()
        .filter(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall))
        .collect();
    assert_eq!(e0031.len(), 1, "expected exactly one E0031; got {diags:?}");
    let note = e0031[0]
        .notes
        .iter()
        .find(|n| n.message == "borrowed here")
        .unwrap_or_else(|| {
            panic!(
                "E0031 must carry the 'borrowed here' note through ViewAsStr; got {:?}",
                e0031[0].notes
            )
        });
    assert_eq!(
        note.span,
        Some(tir.span(reborrow)),
        "the note must attach to the reborrow, not the move arg ({move_span:?})"
    );
}

#[test]
fn view_creation_registers_projection() {
    // fn main(): s: str = "hello"; v = s[0:2]; print(v)
    // → no diags; root_owner[v] == s; s's free is anchored after
    //   print(v), not after decl (P5 deferral, final spec §3.2).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
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
    let vread = tb.var(v, view_ty, span);
    let call = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[decl, vdecl, stmt]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(sink.is_empty(), "expected no diagnostics");

    // The FreePoint targeting s's owner fires after the print(v)
    // statement (anchored on v's read inside the call), not after
    // the slice-base read — the projection keeps the buffer alive.
    assert!(
        sidecar
            .free_schedule
            .iter()
            .any(|fp| fp.target == lit && fp.after == vread && fp.branch.is_none()),
        "expected s's Free anchored after print(v)'s read of v; got: {:?}",
        sidecar.free_schedule
    );
    assert_eq!(
        sidecar
            .free_schedule
            .iter()
            .filter(|fp| fp.target == lit)
            .count(),
        1,
        "expected exactly one Free for lit"
    );
}

#[test]
fn freeze_blocks_move_while_view_live() {
    // s: str = "hello"; v = s[0:2]; consume(s)  (move-mode callee)
    // → SourceProjected at the consume call (P2, final spec §3.2).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
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
    let sread = tb.var(s, str_ty, span);
    let call = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[decl, vdecl, stmt]);

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
fn freeze_blocks_inout_while_view_live() {
    // s: str = "hello"; v = s[0:2]; str_push(&s, "!")
    // → SourceProjected (inout passing mutates the owner; P2).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
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
    let sread = tb.var(s, str_ty, span);
    let suffix = tb.str_const(bang, str_ty, span);
    let call = tb.call(
        str_push,
        &[sread, suffix],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[decl, vdecl, stmt]);

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
        diags[0].message.contains("mutate"),
        "expected the diagnostic to say `mutate`; got: {}",
        diags[0].message
    );
}

#[test]
fn freeze_lifts_after_last_view_use() {
    // s: str = "hello"; v = s[0:2]; print(v); consume(s)
    // → no diags; s moves legally after v's last use (P4).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
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
    let vread = tb.var(v, view_ty, span);
    let pcall = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pstmt = tb.unary(TirTag::ExprStmt, void, pcall, span);
    let sread = tb.var(s, str_ty, span);
    let ccall = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let cstmt = tb.unary(TirTag::ExprStmt, void, ccall, span);
    let tir = tb.finish(&[decl, vdecl, pstmt, cstmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.is_empty(),
        "expected no diagnostics (freeze lifted at v's last use)"
    );
}

#[test]
fn reslice_projects_root_owner() {
    // s: str = "hello"; v = s[0:3]; w = v[1:2]; consume(s) while w live
    // → SourceProjected naming s; root_owner[w] == s (P3).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let v = pool.intern_str("v");
    let w = pool.intern_str("w");
    let consume = pool.intern_str("consume");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let base1 = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i3 = tb.int_const(3, int_ty, span);
    let sl1 = tb.slice(base1, Some(i0), Some(i3), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl1, span);
    let base2 = tb.var(v, view_ty, span);
    let i1 = tb.int_const(1, int_ty, span);
    let i2 = tb.int_const(2, int_ty, span);
    let sl2 = tb.slice(base2, Some(i1), Some(i2), view_ty, span);
    let wdecl = tb.var_decl(w, false, view_ty, sl2, span);
    let sread = tb.var(s, str_ty, span);
    let call = tb.call(consume, &[sread], &[ParamMode::Move], void, span);
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[decl, vdecl, wdecl, stmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        diags
            .iter()
            .any(|d| matches!(d.code, DiagCode::SourceProjected) && d.message.contains("`s`")),
        "expected SourceProjected naming `s` (the re-slice projects the root); got: {diags:?}"
    );
}

#[test]
fn view_return_is_escape() {
    // fn bad(s: str) -> strview: return s[0:1]
    // → sema rejects at signature level (Task 5); this is the
    //   ownership backstop: hand-built TIR with a view return
    //   must produce ViewEscape (E1, final spec §3.3).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{TirBuilder, TirParam};

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bad = pool.intern_str("bad");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);

    let params = vec![TirParam {
        name: s,
        ty: str_ty,
        mode: ParamMode::Borrow,
        span,
    }];
    let mut tb = TirBuilder::new(bad, params, view_ty, span);
    let base = tb.var(s, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i1 = tb.int_const(1, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i1), view_ty, span);
    let ret = tb.unary(TirTag::Return, view_ty, sl, span);
    let tir = tb.finish(&[ret]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        diags.iter().any(|d| matches!(d.code, DiagCode::ViewEscape)),
        "expected ViewEscape on the view return; got: {diags:?}"
    );
}

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
    // but through the ForRange path (analyze_for_range →
    // remove_loop_deferred_views at :3017 instead of the while-loop
    // call at :2994):
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

// ---------- W0003 RedundantMaterialize, case B (M8.4.1.2) ----------
