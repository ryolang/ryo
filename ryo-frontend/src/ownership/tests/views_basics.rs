use super::*;

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
