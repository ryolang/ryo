mod common;
mod frees;
mod inout;
mod loops;
mod merge;
mod views_basics;
mod views_branches;
mod views_calls;

use super::*;
use common::*;

#[test]
fn copy_types_classified() {
    let pool = InternPool::new();
    assert!(pool.is_copy(pool.int()));
    assert!(pool.is_copy(pool.float()));
    assert!(pool.is_copy(pool.bool_()));
    assert!(!pool.is_copy(pool.str_()));
}

#[test]
fn move_types_classified() {
    let pool = InternPool::new();
    assert!(is_move_type(pool.str_(), &pool));
    assert!(!is_move_type(pool.int(), &pool));
    assert!(!is_move_type(pool.bool_(), &pool));
}

#[test]
fn needs_tracking_matches_move() {
    let pool = InternPool::new();
    assert!(needs_tracking(pool.str_(), &pool));
    assert!(!needs_tracking(pool.int(), &pool));
}

#[test]
fn str_const_walk_no_panic() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void: <str_const "hello"> as expr_stmt
    let mut b = TirBuilder::new(main, vec![], void, span);
    let s = b.str_const(hello, str_ty, span);
    let stmt = b.unary(TirTag::ExprStmt, void, s, span);
    let tir = b.finish(&[stmt]);

    let mut sink = DiagSink::new();
    check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(sink.is_empty());
}

#[test]
fn borrowed_param_resolves_under_owner_param_as_borrowed() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let read = pool.intern_str("read"); // fn read(s: str) -> void  (borrowed param)
    let s_name = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        read,
        vec![TirParam {
            name: s_name,
            ty: str_ty,
            mode: ParamMode::Borrow,
            span,
        }],
        void,
        span,
    );
    let v = tb.var(s_name, str_ty, span);
    let tir = tb.finish(&[v]);
    let mut sink = DiagSink::new();
    // check() initialises the param lattice under Owner::Param(s_name) as
    // Borrowed. Assert that reading the borrowed param does NOT trip E0020
    // — the Owner::Param + Borrowed init is load-bearing for the enum migration.
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        sink.into_diags()
            .iter()
            .all(|d| !matches!(d.code, DiagCode::UseAfterMove)),
        "borrowed param read must not trip UAM; the Owner::Param+Borrowed init is load-bearing"
    );
}

#[test]
fn double_consume_reports_uam_once_via_consume_authority() {
    // x = "v"; take(x); take(x)  -- the second take consumes an
    // already-moved binding. The Var arm used to emit E0020 for it;
    // now the consume site (consume_underlying) is the authority
    // and must emit exactly once.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let x = pool.intern_str("x");
    let take = pool.intern_str("take");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
    let decl = tb.var_decl(x, false, str_ty, lit, span);
    let xv1 = tb.var(x, str_ty, span);
    let take1 = tb.call(take, &[xv1], &[ParamMode::Move], void, span);
    let xv2 = tb.var(x, str_ty, span);
    let take2 = tb.call(take, &[xv2], &[ParamMode::Move], void, span);
    let tir = tb.finish(&[decl, take1, take2]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let uam = sink
        .into_diags()
        .iter()
        .filter(|d| matches!(d.code, DiagCode::UseAfterMove))
        .count();
    assert_eq!(uam, 1, "double-consume reports UAM exactly once; got {uam}");
}

#[test]
fn borrow_of_moved_value_still_reports_uam() {
    // print(moved_x) — borrow of a moved value, no consume. After the
    // Var arm is demoted, the borrow-arg check_use_moved must still fire.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let x = pool.intern_str("x");
    let take = pool.intern_str("take");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
    let decl = tb.var_decl(x, false, str_ty, lit, span);
    let xv = tb.var(x, str_ty, span);
    let take_call = tb.call(take, &[xv], &[ParamMode::Move], void, span); // moves x
    let xv2 = tb.var(x, str_ty, span);
    let print_call = tb.call(print, &[xv2], &[ParamMode::Borrow], void, span); // borrow of moved
    let tir = tb.finish(&[decl, take_call, print_call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        diags
            .iter()
            .any(|d| matches!(d.code, DiagCode::UseAfterMove)),
        "borrow of moved value must still report E0020 via borrow-arg path; got {diags:?}"
    );
}

#[test]
fn three_arm_if_with_conditional_move_and_loop_rebind() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    use std::collections::HashSet;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let x = pool.intern_str("x");
    let s = pool.intern_str("s");
    let take = pool.intern_str("take");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_v = tb.str_const(pool.intern_str("v"), str_ty, span);
    let decl_x = tb.var_decl(x, false, str_ty, lit_v, span);

    let lit_mut = tb.str_const(pool.intern_str("initial"), str_ty, span);
    let decl_mut = tb.var_decl(s, true, str_ty, lit_mut, span);

    let cond_then = tb.bool_const(true, bool_ty, span);
    let read_then = tb.var(x, str_ty, span);
    let call_then = tb.call(print, &[read_then], &[ParamMode::Borrow], void, span);

    let cond_elif = tb.bool_const(false, bool_ty, span);
    let read_elif = tb.var(x, str_ty, span);
    let call_elif = tb.call(take, &[read_elif], &[ParamMode::Move], void, span);

    let read_else = tb.var(x, str_ty, span);
    let call_else = tb.call(print, &[read_else], &[ParamMode::Borrow], void, span);

    let if_stmt = tb.if_stmt(
        cond_then,
        &[call_then],
        &[(cond_elif, vec![call_elif])],
        Some(&[call_else]),
        void,
        span,
    );

    let cond_w = tb.bool_const(false, bool_ty, span);
    let body_lit = tb.str_const(pool.intern_str("new_val"), str_ty, span);
    let body_assign = tb.assign(s, str_ty, body_lit, span);
    let wl = tb.while_loop(cond_w, &[body_assign], void, span);

    let read_post = tb.var(x, str_ty, span);
    let call_post = tb.call(print, &[read_post], &[ParamMode::Borrow], void, span);

    let tir = tb.finish(&[decl_x, decl_mut, if_stmt, wl, call_post]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    // (a) Assert no next_branch_id collision in sidecar.if_branches
    let mut ids = HashSet::new();
    for ib in sidecar.if_branches.iter().flatten() {
        assert!(
            ids.insert(ib.then_branch.0),
            "duplicate branch id {}",
            ib.then_branch.0
        );
        for b in &ib.elif_branches {
            assert!(ids.insert(b.0), "duplicate branch id {}", b.0);
        }
        if let Some(b) = ib.else_branch {
            assert!(ids.insert(b.0), "duplicate branch id {}", b.0);
        }
    }

    // (b) Assert a post-if read of the conditionally-moved binding emits exactly one DiagCode::UseAfterMove
    let uam = sink
        .into_diags()
        .iter()
        .filter(|d| matches!(d.code, DiagCode::UseAfterMove))
        .count();
    assert_eq!(
        uam, 1,
        "post-if read of conditionally-moved binding must report UAM exactly once; got {uam}"
    );
}

#[test]
fn move_and_borrow_of_same_owner_in_one_call_e0023_both_orderings() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    use ryo_core::types::TypeId;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let x = pool.intern_str("x");
    let f = pool.intern_str("f");
    let span = SimpleSpan::new((), 0..0);

    fn build(
        tir: &mut TirBuilder,
        x: StringId,
        str_ty: TypeId,
        void: TypeId,
        f: StringId,
        first_move: bool,
        span: Span,
    ) -> (TirRef, TirRef, TirRef) {
        let xv1 = tir.var(x, str_ty, span);
        let xv2 = tir.var(x, str_ty, span);
        let modes = if first_move {
            vec![ParamMode::Move, ParamMode::Borrow]
        } else {
            vec![ParamMode::Borrow, ParamMode::Move]
        };
        let call = tir.call(f, &[xv1, xv2], &modes, void, span);
        (xv1, xv2, call)
    }

    // Ordering 1: Move first, then Borrow
    let mut tb1 = TirBuilder::new(main, vec![], void, span);
    let lit1 = tb1.str_const(pool.intern_str("v"), str_ty, span);
    let decl1 = tb1.var_decl(x, false, str_ty, lit1, span);
    let (_xv1, _xv2, call1) = build(&mut tb1, x, str_ty, void, f, true, span);
    let tir1 = tb1.finish(&[decl1, call1]);

    let mut sink1 = DiagSink::new();
    let _sc1 = check(std::slice::from_ref(&tir1), &pool, &mut sink1);
    let diags1 = sink1.into_diags();
    let e0023_count1 = diags1
        .iter()
        .filter(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall))
        .count();
    assert_eq!(
        e0023_count1, 1,
        "Expected exactly one MoveWhileBorrowedInCall (E0031) in first-move ordering; got {diags1:?}"
    );

    // Ordering 2: Borrow first, then Move
    let mut tb2 = TirBuilder::new(main, vec![], void, span);
    let lit2 = tb2.str_const(pool.intern_str("v"), str_ty, span);
    let decl2 = tb2.var_decl(x, false, str_ty, lit2, span);
    let (_xv3, _xv4, call2) = build(&mut tb2, x, str_ty, void, f, false, span);
    let tir2 = tb2.finish(&[decl2, call2]);

    let mut sink2 = DiagSink::new();
    let _sc2 = check(std::slice::from_ref(&tir2), &pool, &mut sink2);
    let diags2 = sink2.into_diags();
    let e0023_count2 = diags2
        .iter()
        .filter(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall))
        .count();
    assert_eq!(
        e0023_count2, 1,
        "Expected exactly one MoveWhileBorrowedInCall (E0031) in borrow-first ordering; got {diags2:?}"
    );
}

#[test]
fn two_borrows_of_one_owner_ok() {
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
    let modes = vec![ParamMode::Borrow, ParamMode::Borrow];
    let call = tb.call(f, &[xv1, xv2], &modes, void, span);
    let tir = tb.finish(&[decl, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        !diags
            .iter()
            .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
        "two borrows of one owner is fine (Rule 7 many readers); got {diags:?}"
    );
}

#[test]
fn borrow_and_move_of_distinct_owners_ok() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let a = pool.intern_str("a");
    let b = pool.intern_str("b");
    let f = pool.intern_str("f");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let la = tb.str_const(pool.intern_str("va"), str_ty, span);
    let lb = tb.str_const(pool.intern_str("vb"), str_ty, span);
    let da = tb.var_decl(a, false, str_ty, la, span);
    let db = tb.var_decl(b, false, str_ty, lb, span);
    let av = tb.var(a, str_ty, span);
    let bv = tb.var(b, str_ty, span);
    let modes = vec![ParamMode::Borrow, ParamMode::Move];
    let call = tb.call(f, &[av, bv], &modes, void, span);
    let tir = tb.finish(&[da, db, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        !sink
            .into_diags()
            .iter()
            .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
        "borrow + move of DISTINCT owners is fine"
    );
}

#[test]
fn single_move_arg_no_e0023() {
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
    let xv = tb.var(x, str_ty, span);
    let call = tb.call(f, &[xv], &[ParamMode::Move], void, span);
    let tir = tb.finish(&[decl, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        !sink
            .into_diags()
            .iter()
            .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
        "a single move arg must not trip E0031"
    );
}

#[test]
fn copy_args_untracked_no_false_positive() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let n = pool.intern_str("n");
    let x = pool.intern_str("x");
    let f = pool.intern_str("f");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let ic = tb.int_const(1, int_ty, span);
    let dn = tb.var_decl(n, false, int_ty, ic, span);
    let lit = tb.str_const(pool.intern_str("v"), str_ty, span);
    let dx = tb.var_decl(x, false, str_ty, lit, span);
    let nv = tb.var(n, int_ty, span);
    let xv = tb.var(x, str_ty, span);
    let modes = vec![ParamMode::Borrow, ParamMode::Move];
    let call = tb.call(f, &[nv, xv], &modes, void, span);
    let tir = tb.finish(&[dn, dx, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        !sink
            .into_diags()
            .iter()
            .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
        "untracked int arg must not trigger a false E0031"
    );
}

#[test]
fn borrow_and_move_in_sequential_statements_ok() {
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
    // first statement: f(borrow x)
    let xv1 = tb.var(x, str_ty, span);
    let call1 = tb.call(f, &[xv1], &[ParamMode::Borrow], void, span);
    // second statement: f(move x)
    let xv2 = tb.var(x, str_ty, span);
    let call2 = tb.call(f, &[xv2], &[ParamMode::Move], void, span);
    let tir = tb.finish(&[decl, call1, call2]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    assert!(
        !sink
            .into_diags()
            .iter()
            .any(|d| matches!(d.code, DiagCode::MoveWhileBorrowedInCall)),
        "borrow and move in sequential statements must not trigger E0031"
    );
}

#[test]
fn two_immutable_borrows_ok() {
    // f(c, c) — two immutable borrows of one int owner: no E0032
    // (Rule 7 many readers). Guards the untracked-Borrow recording
    // against false positives.
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
        &[ParamMode::Borrow, ParamMode::Borrow],
        void,
        span,
    );
    let tir = tb.finish(&[decl, call]);
    let mut sink = DiagSink::new();
    let _sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        !diags
            .iter()
            .any(|d| matches!(d.code, DiagCode::MutableAliasingViolation)),
        "two immutable borrows of one owner are fine (Rule 7 many readers); got {diags:?}"
    );
}
