use super::super::*;
use super::common::*;

#[test]
fn inout_same_owner_twice_rejected() {
    // swap(&c, &c) — two mutable borrows of one int owner in the same
    // call (Rule 7 case 1). The int args never enter the lattice, so
    // this exercises the name-based inout owner resolution.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let c = pool.intern_str("c");
    let swap = pool.intern_str("swap");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let ic = tb.int_const(1, int_ty, span);
    let decl = tb.var_decl(c, false, int_ty, ic, span);
    let cv1 = tb.var(c, int_ty, span);
    let cv2 = tb.var(c, int_ty, span);
    let call = tb.call(
        swap,
        &[cv1, cv2],
        &[ParamMode::Inout, ParamMode::Inout],
        void,
        span,
    );
    let tir = tb.finish(&[decl, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    let e0032_count = diags
        .iter()
        .filter(|d| matches!(d.code, DiagCode::MutableAliasingViolation))
        .count();
    assert_eq!(
        e0032_count, 1,
        "Expected exactly one MutableAliasingViolation (E0032) for swap(&c, &c); got {diags:?}"
    );
}

#[test]
fn inout_and_borrow_same_owner_rejected() {
    // f(&c, c) — mutable borrow plus immutable borrow of one int owner
    // in the same call (Rule 7 case 2).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let c = pool.intern_str("c");
    let f = pool.intern_str("f");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let ic = tb.int_const(1, int_ty, span);
    let decl = tb.var_decl(c, false, int_ty, ic, span);
    let cv1 = tb.var(c, int_ty, span);
    let cv2 = tb.var(c, int_ty, span);
    let call = tb.call(
        f,
        &[cv1, cv2],
        &[ParamMode::Inout, ParamMode::Borrow],
        void,
        span,
    );
    let tir = tb.finish(&[decl, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    let e0032_count = diags
        .iter()
        .filter(|d| matches!(d.code, DiagCode::MutableAliasingViolation))
        .count();
    assert_eq!(
        e0032_count, 1,
        "Expected exactly one MutableAliasingViolation (E0032) for f(&c, c); got {diags:?}"
    );
}

#[test]
fn inout_and_move_same_owner_rejected() {
    // f(&x, move x) — mutable borrow plus move of one str owner in the
    // same call (Rule 7 case 3). The tracked str args exercise the
    // lattice-backed path of the overlap check.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let x = pool.intern_str("x");
    let f = pool.intern_str("f");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
    let decl = tb.var_decl(x, false, str_ty, lit, span);
    let xv1 = tb.var(x, str_ty, span);
    let xv2 = tb.var(x, str_ty, span);
    let call = tb.call(
        f,
        &[xv1, xv2],
        &[ParamMode::Inout, ParamMode::Move],
        void,
        span,
    );
    let tir = tb.finish(&[decl, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    let e0032_count = diags
        .iter()
        .filter(|d| matches!(d.code, DiagCode::MutableAliasingViolation))
        .count();
    assert_eq!(
        e0032_count, 1,
        "Expected exactly one MutableAliasingViolation (E0032) for f(&x, move x); got {diags:?}"
    );
}

#[test]
fn e0032_names_local_binding_not_value() {
    // `swap(&c, &c)` must name `c` in the message — the spec's
    // rendered example shows the backticked binding name, not the
    // generic "value" that `owner_name_for_diag` falls back to for
    // locals (it inspects the initializer, not the read).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let c = pool.intern_str("c");
    let swap = pool.intern_str("swap");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let ic = tb.int_const(1, int_ty, span);
    let decl = tb.var_decl(c, false, int_ty, ic, span);
    let cv1 = tb.var(c, int_ty, span);
    let cv2 = tb.var(c, int_ty, span);
    let call = tb.call(
        swap,
        &[cv1, cv2],
        &[ParamMode::Inout, ParamMode::Inout],
        void,
        span,
    );
    let tir = tb.finish(&[decl, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    let msg = &diags
        .iter()
        .find(|d| matches!(d.code, DiagCode::MutableAliasingViolation))
        .expect("E0032 must fire")
        .message;
    assert!(
        msg.contains("`c`"),
        "E0032 must name the binding `c`; got: {msg}"
    );
    assert!(
        !msg.contains("value"),
        "E0032 must not fall back to 'value'; got: {msg}"
    );
}

#[test]
fn inout_distinct_owners_ok() {
    // swap(&a, &b) — mutable borrows of DISTINCT int owners: no E0032.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let a = pool.intern_str("a");
    let b = pool.intern_str("b");
    let swap = pool.intern_str("swap");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let ia = tb.int_const(1, int_ty, span);
    let ib = tb.int_const(2, int_ty, span);
    let da = tb.var_decl(a, false, int_ty, ia, span);
    let db = tb.var_decl(b, false, int_ty, ib, span);
    let av = tb.var(a, int_ty, span);
    let bv = tb.var(b, int_ty, span);
    let call = tb.call(
        swap,
        &[av, bv],
        &[ParamMode::Inout, ParamMode::Inout],
        void,
        span,
    );
    let tir = tb.finish(&[da, db, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        !diags
            .iter()
            .any(|d| matches!(d.code, DiagCode::MutableAliasingViolation)),
        "mutable borrows of DISTINCT owners are fine (Rule 7); got {diags:?}"
    );
}

#[test]
fn inout_str_param_reassign_escapes_no_dead_store_no_free() {
    // Callee side: `fn set(inout s: str): s = "new"`. The
    // replacement value escapes through the write-back pointer, so the
    // pass must NOT emit W0001 or free the new value; the OLD pointee
    // (the incoming buffer) must be dropped at the reassign.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let set = pool.intern_str("set");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        set,
        vec![TirParam {
            name: s,
            ty: str_ty,
            mode: ParamMode::Inout,
            span,
        }],
        void,
        span,
    );
    let lit = tb.str_const(pool.intern_str("new"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit, span);
    let tir = tb.finish(&[asg]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
        "inout param reassignment must not warn dead-store — the value escapes via write-back; got {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert_eq!(
        sc.free_on_reassign[asg.index()],
        Some(TirRef::param(0)),
        "reassigning an inout str param must drop the incoming buffer at the reassign; free_on_reassign = {:?}",
        sc.free_on_reassign
    );
    assert!(
        !sc.free_schedule.iter().any(|fp| fp.target == lit),
        "the reassigned value escapes to the caller — no Free may target it; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn inout_str_param_reassign_inside_if_escapes() {
    // `fn g(inout s: str): if c: s = "b"`. The rebind is
    // branch-divergent — the merge keeps `Param(s)` as the binding's
    // owner and can stamp it Valid — but the bound value still
    // escapes via the write-back: no W0001, no Free for the rebound
    // value or the param, while the taken arm still drops the
    // incoming buffer.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let g = pool.intern_str("g");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        g,
        vec![TirParam {
            name: s,
            ty: str_ty,
            mode: ParamMode::Inout,
            span,
        }],
        void,
        span,
    );
    let cond = tb.bool_const(true, bool_ty, span);
    let lit = tb.str_const(pool.intern_str("b"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit, span);
    let if_s = tb.if_stmt(cond, &[asg], &[], None, void, span);
    let tir = tb.finish(&[if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
        "inout param reassignment escapes — no dead-store warning, even branch-divergent; got {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_on_reassign[asg.index()].is_some(),
        "the taken arm must drop the incoming buffer; free_on_reassign = {:?}",
        sc.free_on_reassign
    );
    assert!(
        !sc.free_schedule.iter().any(|fp| fp.target == lit),
        "the rebound value escapes to the caller — no Free may target it; schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        !sc.free_schedule
            .iter()
            .any(|fp| fp.target == TirRef::param(0)),
        "the inout param's value escapes — no callee Free for the param owner; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn inout_str_param_move_out_after_reassign_rejected() {
    // The value bound to an inout str param escapes via the
    // write-back, so moving it out (even after a reassign made it a
    // fresh, Valid owner) must still be an error.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let f = pool.intern_str("f");
    let g = pool.intern_str("g");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        f,
        vec![TirParam {
            name: s,
            ty: str_ty,
            mode: ParamMode::Inout,
            span,
        }],
        void,
        span,
    );
    let lit = tb.str_const(pool.intern_str("new"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit, span);
    let sv = tb.var(s, str_ty, span);
    let call = tb.call(g, &[sv], &[ParamMode::Move], void, span);
    let tir = tb.finish(&[asg, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        diags
            .iter()
            .any(|d| matches!(d.code, DiagCode::MoveOutOfBorrowedParam)),
        "moving out of an inout param after reassign must be E0021; got {diags:?}"
    );
}

#[test]
fn inout_str_param_return_after_reassign_rejected() {
    // Returning the value currently bound to an inout str param
    // double-owns it (it also escapes via the write-back) — E0022.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirData, TirParam, TirTag};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let f = pool.intern_str("f");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        f,
        vec![TirParam {
            name: s,
            ty: str_ty,
            mode: ParamMode::Inout,
            span,
        }],
        str_ty,
        span,
    );
    let lit = tb.str_const(pool.intern_str("new"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit, span);
    let sv = tb.var(s, str_ty, span);
    let ret = tb.push_typed(TirTag::Return, TirData::UnOp(sv), str_ty, span);
    let _ = void;
    let tir = tb.finish(&[asg, ret]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        diags
            .iter()
            .any(|d| matches!(d.code, DiagCode::ReturnBorrowedValue)),
        "returning an inout param's bound value must be E0022; got {diags:?}"
    );
}

#[test]
fn inout_call_keeps_owner_and_frees_current_buffer() {
    // Caller side: `mut s = "hi"; set(&s); print(s)`. An inout
    // call is a pure borrow of the slot: the binding KEEPS its
    // pre-call owner (no reseat, no Moved, no dead-store churn), and
    // the freshness of the freed buffer is codegen's job — it emits
    // the Free from the binding's current `StrLocals` (which hold the
    // write-back triple after the reload), never the stale pre-call
    // repr. Assert the lattice invariants: owner unchanged, exactly
    // one Free for it, no diagnostics.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let set = pool.intern_str("set");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(pool.intern_str("hi"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let sv1 = tb.var(s, str_ty, span);
    let call = tb.call(set, &[sv1], &[ParamMode::Inout], void, span);
    let sv2 = tb.var(s, str_ty, span);
    let pr = tb.call(print, &[sv2], &[ParamMode::Borrow], void, span);
    let tir = tb.finish(&[decl, call, pr]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        diags.is_empty(),
        "inout call on a str local must produce no diagnostics; got {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    let frees: Vec<_> = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.target == lit)
        .collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the binding's owner; codegen emits it from the current StrLocals; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn inout_call_inside_if_no_false_dead_store() {
    // `mut s = "a"; if c: set(&s); print(s)` — the
    // reseat happens inside the if-arm; the post-if read must clear
    // the dead-store entry across the branch merge.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let set = pool.intern_str("set");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sv1 = tb.var(s, str_ty, span);
    let call = tb.call(set, &[sv1], &[ParamMode::Inout], void, span);
    let if_s = tb.if_stmt(cond, &[call], &[], None, void, span);
    let sv2 = tb.var(s, str_ty, span);
    let pr = tb.call(print, &[sv2], &[ParamMode::Borrow], void, span);
    let tir = tb.finish(&[decl, if_s, pr]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
        "s is read after the if — no dead-store warning may survive the merge; got {diags:?}"
    );
}

#[test]
fn inout_call_inside_loop_no_false_dead_store() {
    // `mut s = ""; while i < 3: str_push(&s, "x") ...
    // print(s)` — same merge concern through the loop fixed point.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let set = pool.intern_str("set");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(pool.intern_str(""), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sv1 = tb.var(s, str_ty, span);
    let call = tb.call(set, &[sv1], &[ParamMode::Inout], void, span);
    let wl = tb.while_loop(cond, &[call], void, span);
    let sv2 = tb.var(s, str_ty, span);
    let pr = tb.call(print, &[sv2], &[ParamMode::Borrow], void, span);
    let tir = tb.finish(&[decl, wl, pr]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
        "s is read after the loop — no dead-store warning may survive the loop merge; got {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert_eq!(
        sc.free_schedule
            .iter()
            .filter(|fp| fp.target == lit)
            .count(),
        1,
        "exactly one Free for the binding's owner after the loop; schedule = {:?}",
        sc.free_schedule
    );
}
