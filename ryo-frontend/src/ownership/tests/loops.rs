use super::super::*;
use super::common::*;

#[test]
fn inside_loop_temp_is_freed() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let print_name = pool.intern_str("print");
    let inside = pool.intern_str("inside");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     while true:
    //         print("inside")     # StrConst is the temp under test
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit = tb.str_const(inside, str_ty, span);
    let print_call = tb.call(print_name, &[lit], &all_borrow(&[lit]), void, span);
    let wl = tb.while_loop(cond, &[print_call], void, span);
    let tir = tb.finish(&[wl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    assert!(
        sidecar.drops.iter().any(|fp| fp.target == lit),
        "expected inside-loop StrConst to be scheduled for Free; got: {:?}",
        sidecar.drops
    );
    assert_eq!(
        sidecar.drops.iter().filter(|fp| fp.target == lit).count(),
        1,
        "expected exactly one Free for the inside-loop StrConst; got: {:?}",
        sidecar.drops
    );
}

#[test]
fn pre_loop_owner_last_use_in_loop_freed_at_loop_exit() {
    // A pre-loop owner whose last-use is inside a loop must be
    // freed on a `break` path that bypasses that last-use, as we
    // are exiting the loop and will never reach the last-use again.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s_name = pool.intern_str("s");
    let int_to_str = pool.intern_str("int_to_str");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     s: str = int_to_str(0)
    //     while true:
    //         if true:
    //             break
    //         print(s)
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let zero = tb.int_const(0, int_ty, span);
    let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
    let decl = tb.var_decl(s_name, false, str_ty, alloc, span);

    let cond_w = tb.bool_const(true, bool_ty, span);
    let cond_i = tb.bool_const(true, bool_ty, span);
    let brk = tb.break_stmt(void, span);
    let if_inside = tb.if_stmt(cond_i, &[brk], &[], None, void, span);
    let s_var = tb.var(s_name, str_ty, span);
    let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
    let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
    let wl = tb.while_loop(cond_w, &[if_inside, print_stmt], void, span);
    let tir = tb.finish(&[decl, wl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    let frees: Vec<_> = sidecar
        .drops
        .iter()
        .filter(|fp| fp.target == alloc)
        .collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the pre-loop owner; got: {:?}",
        sidecar.drops
    );
    assert_eq!(
        frees[0].position.anchor_inst().unwrap(),
        wl,
        "a pre-loop owner whose last use is in the loop is freed at the loop exit (covers break, normal-exit, and zero-iteration paths); got: {:?}",
        sidecar.drops
    );
}

#[test]
fn pre_loop_owner_continue_before_last_use_does_not_free_on_continue() {
    // A pre-loop owner whose last-use is inside a loop must NOT be
    // freed on a `continue` path, as we will loop back and might
    // read it in the next iteration (causing use-after-free).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s_name = pool.intern_str("s");
    let int_to_str = pool.intern_str("int_to_str");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     s: str = int_to_str(0)
    //     while true:
    //         if true:
    //             continue
    //         print(s)
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let zero = tb.int_const(0, int_ty, span);
    let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
    let decl = tb.var_decl(s_name, false, str_ty, alloc, span);

    let cond_w = tb.bool_const(true, bool_ty, span);
    let cond_i = tb.bool_const(true, bool_ty, span);
    let cont = tb.continue_stmt(void, span);
    let if_inside = tb.if_stmt(cond_i, &[cont], &[], None, void, span);
    let s_var = tb.var(s_name, str_ty, span);
    let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
    let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
    let wl = tb.while_loop(cond_w, &[if_inside, print_stmt], void, span);
    let tir = tb.finish(&[decl, wl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    // The only Free for `alloc` must be anchored on `print_call`,
    // and we must NOT have any Free anchored on `cont`.
    let frees_for_alloc: Vec<_> = sidecar
        .drops
        .iter()
        .filter(|fp| fp.target == alloc)
        .collect();
    assert_eq!(
        frees_for_alloc.len(),
        1,
        "expected exactly one Free for `alloc`; got: {:?}",
        sidecar.drops
    );
    assert_ne!(
        frees_for_alloc[0].position.anchor_inst().unwrap(),
        cont,
        "Free for pre-loop owner must not be anchored on continue; got: {:?}",
        frees_for_alloc[0]
    );
}

#[test]
fn continue_jump_does_not_free_pre_loop_owner_uaf_guard() {
    // Pre-loop owner read only inside the loop, with a `continue` before
    // its last use. The defensive emit must NOT fire on continue (would
    // free the buffer the next iteration reads -> UAF). break still frees.
    // Construct: s = "x"; while c: if d: continue; print(s)
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let bool_ty = pool.bool_();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let c = pool.intern_str("c");
    let d = pool.intern_str("d");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(pool.intern_str("x"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let cond = tb.var(c, bool_ty, span);
    let cont = tb.continue_stmt(void, span);
    let sv = tb.var(s, str_ty, span);
    let pcall = tb.call(
        print,
        &[sv],
        &[ryo_core::tir::ParamMode::Borrow],
        void,
        span,
    );
    let ifcond = tb.var(d, bool_ty, span);
    let ifstmt = tb.if_stmt(ifcond, &[cont], &[], Some(&[pcall]), void, span);
    let lp = tb.while_loop(cond, &[ifstmt], void, span);
    let tir = tb.finish(&[decl, lp]);
    let mut sink = DiagSink::new();
    let mut sc = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sc, 0);
    // No Free for `lit` anchored on the continue jump:
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.target == lit && fp.position.anchor_inst().unwrap() == cont),
        "continue must not free the pre-loop owner (UAF); got {sc:?}"
    );
}

#[test]
fn continue_jump_does_not_free_pre_loop_owner_uaf_guard_reassigned() {
    // Construct: s = "x"; while c: if d: continue; s = "y"
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let bool_ty = pool.bool_();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let c = pool.intern_str("c");
    let d = pool.intern_str("d");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit1 = tb.str_const(pool.intern_str("x"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit1, span);
    let cond = tb.var(c, bool_ty, span);
    let cont = tb.continue_stmt(void, span);
    let ifcond = tb.var(d, bool_ty, span);
    let ifstmt = tb.if_stmt(ifcond, &[cont], &[], None, void, span);
    let lit2 = tb.str_const(pool.intern_str("y"), str_ty, span);
    let assign = tb.assign(s, str_ty, lit2, span);
    let lp = tb.while_loop(cond, &[ifstmt, assign], void, span);
    let tir = tb.finish(&[decl, lp]);
    let mut sink = DiagSink::new();
    let mut sc = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sc, 0);
    // Under the buggy compiler, `lit1` is defensively freed on `cont`:
    let freed_on_cont = sc
        .drops
        .iter()
        .any(|fp| fp.target == lit1 && fp.position.anchor_inst().unwrap() == cont);
    assert!(
        !freed_on_cont,
        "continue must not free the pre-loop owner (UAF); got {sc:?}"
    );
}

#[test]
fn break_before_last_use_schedules_jump_free() {
    // Regression for the break-path leak. A `break` taken before the
    // `print(s)` last-use must trigger a Free anchored on the break
    // instr — otherwise the inside-loop allocation leaks on the break
    // path.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s_name = pool.intern_str("s");
    let int_to_str = pool.intern_str("int_to_str");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     while true:
    //         s: str = int_to_str(0)
    //         if true:
    //             break
    //         print(s)
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let cond_w = tb.bool_const(true, bool_ty, span);
    let zero = tb.int_const(0, int_ty, span);
    let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
    let decl = tb.var_decl(s_name, false, str_ty, alloc, span);
    let cond_i = tb.bool_const(true, bool_ty, span);
    let brk = tb.break_stmt(void, span);
    let if_inside = tb.if_stmt(cond_i, &[brk], &[], None, void, span);
    let s_var = tb.var(s_name, str_ty, span);
    let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
    let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
    let wl = tb.while_loop(cond_w, &[decl, if_inside, print_stmt], void, span);
    let tir = tb.finish(&[wl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    assert!(
        sidecar
            .drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == brk && fp.target == alloc),
        "expected a Free for the inside-loop owner anchored on break; got: {:?}",
        sidecar.drops
    );
}

#[test]
fn break_after_last_use_does_not_double_schedule() {
    // The `break_inside_loop_owner` shape: print(s) is before
    // break, so the natural last-use Free fires before the jump
    // on any path that reaches it. The break/continue scheduler
    // must NOT add a redundant Free anchored on break, or codegen
    // would double-free.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s_name = pool.intern_str("s");
    let int_to_str = pool.intern_str("int_to_str");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     while true:
    //         s: str = int_to_str(0)
    //         print(s)
    //         if true:
    //             break
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let cond_w = tb.bool_const(true, bool_ty, span);
    let zero = tb.int_const(0, int_ty, span);
    let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
    let decl = tb.var_decl(s_name, false, str_ty, alloc, span);
    let s_var = tb.var(s_name, str_ty, span);
    let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
    let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
    let cond_i = tb.bool_const(true, bool_ty, span);
    let brk = tb.break_stmt(void, span);
    let if_inside = tb.if_stmt(cond_i, &[brk], &[], None, void, span);
    let wl = tb.while_loop(cond_w, &[decl, print_stmt, if_inside], void, span);
    let tir = tb.finish(&[wl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    // Exactly one Free for `alloc`, anchored on `print_call` (the
    // last-use), not on `brk`.
    let frees_for_alloc: Vec<_> = sidecar
        .drops
        .iter()
        .filter(|fp| fp.target == alloc)
        .collect();
    assert_eq!(
        frees_for_alloc.len(),
        1,
        "expected exactly one Free for `alloc`; got: {:?}",
        sidecar.drops
    );
    assert_ne!(
        frees_for_alloc[0].position.anchor_inst().unwrap(),
        brk,
        "Free for `alloc` must not be anchored on break; got: {:?}",
        frees_for_alloc[0]
    );
}

#[test]
fn break_in_else_arm_sibling_print_schedules_jump_free() {
    // Cross-branch regression for the break-path leak. The natural
    // last-use Free for `alloc` anchors on `print(s)` inside the
    // THEN arm; the break
    // sits in the ELSE arm. Lexical raw() ordering would put the
    // print's anchor before the break, but on the break path the
    // print never ran — so the buffer leaks unless we schedule a
    // jump-anchored Free here.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s_name = pool.intern_str("s");
    let int_to_str = pool.intern_str("int_to_str");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     while true:
    //         s: str = int_to_str(0)
    //         if true:
    //             print(s)        # natural last-use, then-arm
    //         else:
    //             break           # cross-branch leak site
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let cond_w = tb.bool_const(true, bool_ty, span);
    let zero = tb.int_const(0, int_ty, span);
    let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
    let decl = tb.var_decl(s_name, false, str_ty, alloc, span);
    let cond_i = tb.bool_const(true, bool_ty, span);
    let s_var = tb.var(s_name, str_ty, span);
    let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
    let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
    let brk = tb.break_stmt(void, span);
    let if_inside = tb.if_stmt(cond_i, &[print_stmt], &[], Some(&[brk]), void, span);
    let wl = tb.while_loop(cond_w, &[decl, if_inside], void, span);
    let tir = tb.finish(&[wl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    // Two Frees expected: one anchored on s_var (the Var read in
    // the then-arm — collect_last_uses anchors on Var reads, not
    // their wrapping Call), one anchored on brk (cross-branch
    // leak fix).
    let frees_for_alloc: Vec<_> = sidecar
        .drops
        .iter()
        .filter(|fp| fp.target == alloc)
        .collect();
    assert!(
        frees_for_alloc
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == s_var),
        "expected then-arm last-use Free anchored on s_var (the Var read); got: {:?}",
        sidecar.drops
    );
    assert!(
        frees_for_alloc
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == brk),
        "expected cross-branch jump-anchored Free on break; got: {:?}",
        sidecar.drops
    );
}

#[test]
fn continue_before_last_use_schedules_jump_free() {
    // Symmetric regression for `continue` instead of `break`.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s_name = pool.intern_str("s");
    let int_to_str = pool.intern_str("int_to_str");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     while true:
    //         s: str = int_to_str(0)
    //         if true:
    //             continue
    //         print(s)
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let cond_w = tb.bool_const(true, bool_ty, span);
    let zero = tb.int_const(0, int_ty, span);
    let alloc = tb.call(int_to_str, &[zero], &all_borrow(&[zero]), str_ty, span);
    let decl = tb.var_decl(s_name, false, str_ty, alloc, span);
    let cond_i = tb.bool_const(true, bool_ty, span);
    let cont = tb.continue_stmt(void, span);
    let if_inside = tb.if_stmt(cond_i, &[cont], &[], None, void, span);
    let s_var = tb.var(s_name, str_ty, span);
    let print_call = tb.call(print, &[s_var], &all_borrow(&[s_var]), void, span);
    let print_stmt = tb.unary(TirTag::ExprStmt, void, print_call, span);
    let wl = tb.while_loop(cond_w, &[decl, if_inside, print_stmt], void, span);
    let tir = tb.finish(&[wl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    assert!(
        sidecar
            .drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == cont && fp.target == alloc),
        "expected a Free for the inside-loop owner anchored on continue; got: {:?}",
        sidecar.drops
    );
}

#[test]
fn pre_loop_owner_read_only_in_loop_is_freed() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s_name = pool.intern_str("s");
    let print_name = pool.intern_str("print");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     s: str = "hello"
    //     while false:
    //         print(s)            # only read of `s`, inside the loop
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s_name, false, str_ty, lit, span);
    let cond = tb.bool_const(false, bool_ty, span);
    let s_var = tb.var(s_name, str_ty, span);
    let print_call = tb.call(print_name, &[s_var], &all_borrow(&[s_var]), void, span);
    let wl = tb.while_loop(cond, &[print_call], void, span);
    let tir = tb.finish(&[decl, wl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    let diags = sink.into_diags();
    assert!(
        !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
        "did not expect DeadStore for `s` (it is read inside the loop): {:?}",
        diags
    );

    let count = sidecar.drops.iter().filter(|fp| fp.target == lit).count();
    assert_eq!(
        count, 1,
        "expected exactly one Free for pre-loop owner read only inside loop; got {} schedule={:?}",
        count, sidecar.drops
    );
}

#[test]
fn diverged_loop_writes_branch_gated_free_exactly_once() {
    // Regression: speculative sidecar writes from the loop
    // fixed-point's first walk must not leak into the real sidecar.
    //   m: str = "m"
    //   while true:
    //       x: str = "x"     # fresh owner every iteration
    //       if true:
    //           consume(x)   # then arm moves x
    //       else:
    //           print(x)     # else arm keeps x Valid
    //       consume(m)       # moved without rebinding → divergence
    //
    // m flips Valid → Moved across the back-edge, so the state
    // tuple differs and the diverged path runs. The if schedules an
    // arm-conditional Free for x anchored inside the else arm. Before
    // the fix the scratch walk wrote that DropOp into the REAL sidecar
    // and the re-walk wrote it again — two entries for one arm, the
    // first anchored on a path the merge discarded.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let m = pool.intern_str("m");
    let x = pool.intern_str("x");
    let consume = pool.intern_str("consume");
    let print = pool.intern_str("print");
    let m_lit = pool.intern_str("m");
    let x_lit = pool.intern_str("x");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_m = tb.str_const(m_lit, str_ty, span);
    let decl_m = tb.var_decl(m, false, str_ty, lit_m, span);

    let cond_w = tb.bool_const(true, bool_ty, span);
    let lit_x = tb.str_const(x_lit, str_ty, span);
    let decl_x = tb.var_decl(x, false, str_ty, lit_x, span);
    let cond_i = tb.bool_const(true, bool_ty, span);
    let x_then = tb.var(x, str_ty, span);
    let consume_then = tb.call(consume, &[x_then], &[ParamMode::Move], void, span);
    let then_stmt = tb.unary(TirTag::ExprStmt, void, consume_then, span);
    let x_else = tb.var(x, str_ty, span);
    let print_else = tb.call(print, &[x_else], &all_borrow(&[x_else]), void, span);
    let else_stmt = tb.unary(TirTag::ExprStmt, void, print_else, span);
    let if_inst = tb.if_stmt(cond_i, &[then_stmt], &[], Some(&[else_stmt]), void, span);
    let m_read = tb.var(m, str_ty, span);
    let consume_m = tb.call(consume, &[m_read], &[ParamMode::Move], void, span);
    let consume_m_stmt = tb.unary(TirTag::ExprStmt, void, consume_m, span);
    let wl = tb.while_loop(cond_w, &[decl_x, if_inst, consume_m_stmt], void, span);
    let tir = tb.finish(&[decl_m, wl]);

    let mut sink = DiagSink::new();
    let mut sc = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sc, 0);

    // Precondition: the loop actually diverged — the re-walk sees
    // `consume(m)` with m already Moved and reports UAM once. If
    // this ever fails, the fixed-point no longer takes the diverged
    // path and the assertions below pass vacuously.
    let uam = sink
        .into_diags()
        .iter()
        .filter(|d| matches!(d.code, DiagCode::UseAfterMove))
        .count();
    assert_eq!(
        uam, 1,
        "expected exactly one UAM from the diverged re-walk; got {uam}"
    );

    // Exactly ONE arm-conditional Free for the if's else arm — its
    // anchor inside the else's Flow IR block (the structural gate).
    let flow = build_flow(&tir, &pool);
    let arm_blocks = flow.branch_arm_blocks(if_inst);
    let gated: Vec<_> = sc
        .drops
        .iter()
        .filter(|d| flow.step_block_of(drop_anchor(d)) == Some(arm_blocks.else_block))
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "diverged loop must schedule the else-arm Free exactly once; got {gated:?}"
    );
    // And no other in-arm anchors anywhere in the schedule (the
    // discarded scratch-walk duplicate would show up here).
    let in_arm: Vec<_> = sc
        .drops
        .iter()
        .filter(|d| flow.step_block_of(drop_anchor(d)) == Some(arm_blocks.then_block))
        .collect();
    assert!(
        in_arm.is_empty(),
        "no drops anchored in the then arm; got {in_arm:?}"
    );
}

#[test]
fn loop_nesting_chains_depth_three() {
    // Depth-3 nest (the rest of the suite never exceeds depth 2):
    //   for i in range(0, 10):
    //       while true:
    //           while true:
    //               print("deep")
    // Pins the parent-pointer tables directly: per-instruction depth,
    // the innermost-first ancestor order, and `ancestor_at_depth`'s
    // correspondence to the old outermost-first stack indexing
    // (position `d` of the stack == the ancestor whose own depth is
    // `d`).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let i = pool.intern_str("i");
    let print = pool.intern_str("print");
    let deep = pool.intern_str("deep");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let start = tb.int_const(0, int_ty, span);
    let end = tb.int_const(10, int_ty, span);
    let cond_w1 = tb.bool_const(true, bool_ty, span);
    let cond_w2 = tb.bool_const(true, bool_ty, span);
    let lit = tb.str_const(deep, str_ty, span);
    let print_call = tb.call(print, &[lit], &all_borrow(&[lit]), void, span);
    let wl_inner = tb.while_loop(cond_w2, &[print_call], void, span);
    let wl_outer = tb.while_loop(cond_w1, &[wl_inner], void, span);
    let fr = tb.for_range(i, start, end, &[wl_outer], void, span);
    let tir = tb.finish(&[fr]);

    let flow = ryo_core::flow::Flow::build(&tir, &pool);
    let nesting = collect_loop_nesting(&tir, &flow);

    // Depths: the loop instructions themselves sit at the ENCLOSING
    // depth; their subtrees (bounds, conditions, bodies) sit one
    // level deeper.
    assert_eq!(nesting.depth_of(fr), 0);
    assert_eq!(nesting.depth_of(start), 1, "for bounds are inside the for");
    assert_eq!(nesting.depth_of(wl_outer), 1);
    assert_eq!(nesting.depth_of(cond_w1), 2);
    assert_eq!(nesting.depth_of(wl_inner), 2);
    assert_eq!(nesting.depth_of(print_call), 3);
    assert_eq!(nesting.depth_of(lit), 3);

    // Ancestors by parent hops, innermost first.
    let ancestors: Vec<_> = nesting.ancestors_innermost_first(print_call).collect();
    assert_eq!(ancestors, vec![wl_inner, wl_outer, fr]);
    assert!(nesting.ancestors_innermost_first(fr).next().is_none());

    // `ancestor_at_depth(r, d)` == position `d` of the old
    // outermost-first stack: the first loop in r's chain beyond a
    // creation nested `d` deep.
    assert_eq!(nesting.ancestor_at_depth(print_call, 0), fr);
    assert_eq!(nesting.ancestor_at_depth(print_call, 1), wl_outer);
    assert_eq!(nesting.ancestor_at_depth(print_call, 2), wl_inner);
}

#[test]
fn loop_nesting_cond_match_arms_stay_at_loop_depth() {
    // M12 value-position regression: a match in a `while` CONDITION with
    // a statement arm — the arm body re-executes per iteration, so it
    // nests at the loop's depth + 1. The generic loop pass records the
    // loop's ENTIRE subtree (condition included) at depth + 1; the
    // operand-match descent must NOT re-walk the cond match's arms at
    // the enclosing frame (it runs with the frame's depth, overwriting
    // the correct depth + 1 entries with shallower ones — consumers
    // like the view-liveness depth compare read this table).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{Discriminant, TirBuilder, TypedMatchArm, TypedMatchBody};

    let mut pool = InternPool::new();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let x = pool.intern_str("x");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let x_init = tb.int_const(0, int_ty, span);
    let x_decl = tb.var_decl(x, false, int_ty, x_init, span);
    let scrut = tb.int_const(0, int_ty, span);
    let arm_value = tb.int_const(1, int_ty, span);
    let arm_assign = tb.assign(x, int_ty, arm_value, span);
    let arm_expr = tb.bool_const(true, bool_ty, span);
    let m = tb.match_expr(
        scrut,
        bool_ty,
        &[
            TypedMatchArm {
                span,
                discriminant: Discriminant::Int(0),
                bindings: &[],
                guards: &[],
                body: TypedMatchBody::Stmts(&[arm_assign]),
            },
            TypedMatchArm {
                span,
                discriminant: Discriminant::Wildcard,
                bindings: &[],
                guards: &[],
                body: TypedMatchBody::Expr(arm_expr),
            },
        ],
        span,
    );
    let wl = tb.while_loop(m, &[], void, span);
    let tir = tb.finish(&[x_decl, wl]);

    let flow = ryo_core::flow::Flow::build(&tir, &pool);
    let nesting = collect_loop_nesting(&tir, &flow);

    // The loop statement sits at the enclosing depth; its whole subtree
    // — cond match and both arm bodies included — sits one level deeper.
    assert_eq!(nesting.depth_of(wl), 0);
    assert_eq!(nesting.depth_of(m), 1, "the cond match is inside the loop");
    assert_eq!(
        nesting.depth_of(arm_assign),
        1,
        "the statement-arm body stays at the loop's depth + 1"
    );
    assert_eq!(nesting.depth_of(arm_value), 1);
    assert_eq!(
        nesting.depth_of(arm_expr),
        1,
        "the expression-arm body stays at the loop's depth + 1"
    );
}

#[test]
fn loop_carried_concat_break_gets_no_jump_free() {
    // Regression for the loop-carried double free: a `mut str`
    // concatenated and reassigned inside a `while` loop is the
    // binding's CURRENT owner at the `break` — it survives the jump
    // and is read after the loop. A break-anchored Free would free
    // the binding's current home buffer (codegen's binding-path
    // redirect) while the last-use pass independently frees the same
    // buffer after the final read. The jump scheduler must skip
    // loop-carried owners; the legitimate Free anchors after the
    // post-loop read instead.
    let src = "fn main():\n\tmut total = \"aaaaaaaaaaaaaaaaaaaaaaa\"\n\twhile true:\n\t\ttotal = total + \"b\"\n\t\tif total.len() > 5000:\n\t\t\tbreak\n\tprint(total)\n";
    let (diags, mut sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "diags: {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    let tir = &tirs[0];
    let jumps: Vec<TirRef> = (1..tir.instructions.len() as u32)
        .map(TirRef::from_raw)
        .filter(|&r| matches!(tir.inst(r).tag, TirTag::Break | TirTag::Continue))
        .collect();
    assert!(!jumps.is_empty(), "expected a break in the loop");
    let concats: Vec<TirRef> = (1..tir.instructions.len() as u32)
        .map(TirRef::from_raw)
        .filter(|&r| matches!(tir.inst(r).tag, TirTag::StrConcat))
        .collect();
    assert_eq!(concats.len(), 1, "expected one concat; got {concats:?}");
    let concat = concats[0];
    assert!(
        sc.drops
            .iter()
            .all(|fp| !(jumps.contains(&fp.position.anchor_inst().unwrap()) && fp.target == concat)),
        "no Free anchored on a jump may target the loop-carried concat owner; got: {:?}",
        sc.drops
    );
    // The binding's buffer must still be freed — after the post-loop
    // read, not at the loop exit (which would free it before the read).
    let post_loop_read_free = sc
        .drops
        .iter()
        .find(|fp| fp.target == concat)
        .expect("concat owner must have a last-use Free");
    assert!(
        !jumps.contains(&post_loop_read_free.position.anchor_inst().unwrap()),
        "concat Free must not anchor on a jump; got: {:?}",
        post_loop_read_free
    );
}

#[test]
fn loop_carried_concat_continue_gets_no_jump_free() {
    // The `continue` shape of the loop-carried double free: same
    // skip rule as `break`.
    let src = "fn main():\n\tmut total = \"aaaaaaaaaaaaaaaaaaaaaaa\"\n\tmut n = 0\n\twhile n < 100:\n\t\ttotal = total + \"b\"\n\t\tn = n + 1\n\t\tif n < 50:\n\t\t\tcontinue\n\tprint(total)\n";
    let (diags, mut sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "diags: {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    let tir = &tirs[0];
    let jumps: Vec<TirRef> = (1..tir.instructions.len() as u32)
        .map(TirRef::from_raw)
        .filter(|&r| matches!(tir.inst(r).tag, TirTag::Break | TirTag::Continue))
        .collect();
    assert!(
        jumps
            .iter()
            .any(|&r| matches!(tir.inst(r).tag, TirTag::Continue)),
        "expected a continue in the loop"
    );
    let concat = (1..tir.instructions.len() as u32)
        .map(TirRef::from_raw)
        .find(|&r| matches!(tir.inst(r).tag, TirTag::StrConcat))
        .expect("expected a concat");
    assert!(
        sc.drops
            .iter()
            .all(|fp| !(jumps.contains(&fp.position.anchor_inst().unwrap()) && fp.target == concat)),
        "no Free anchored on a jump may target the loop-carried concat owner; got: {:?}",
        sc.drops
    );
}

#[test]
fn break_before_in_loop_owner_def_schedules_no_jump_free() {
    // `while true: if c: break; child = make()` — the break lexically
    // PRECEDES the inside-loop owner definition. The jump-anchored
    // free pass must not attribute the owner to the break: the jump's
    // path never materializes the value (pre-fix: compile abort
    // "ownership pass scheduled Free for struct %N but no struct local
    // or ValueRepr" — the while-true + break rewrite of the same
    // recursive-descent parser loop that SIGABRT'd in its do-while
    // form). The owner's release is its own in-body last-use Free.
    let src = r#"
struct P:
	text: str
	next: int

fn make(x: int) -> P:
	return P{text=int_to_str(x) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", next=x}

fn cond(i: int) -> bool:
	return i > 0

fn main():
	mut i = 0
	while true:
		if cond(i):
			break
		child = make(i)
		print(child.text)
		i += 1
	print("done")
"#;
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "expected no errors: {diags:?}"
    );
    let idx = tirs
        .iter()
        .position(|t| pool.str(t.name) == "main")
        .unwrap();
    let tir = &tirs[idx];
    let sc = take_function_sidecar(&mut sidecar, idx);

    let mut break_ref = None;
    let mut post_break_owner = None;
    let child = pool.intern_str("child");
    for r in 1..tir.instructions.len() {
        let rr = TirRef::from_raw(r as u32);
        match tir.instructions[r].tag {
            TirTag::Break => break_ref = Some(rr),
            TirTag::VarDecl => {
                let v = tir.var_decl_view(rr);
                if v.name == child {
                    post_break_owner = Some(v.initializer);
                }
            }
            _ => {}
        }
    }
    let break_ref = break_ref.expect("break found");
    let post_break_owner = post_break_owner.expect("child decl found");
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == break_ref
                && fp.target == post_break_owner),
        "a Free anchored at the break must not target the not-yet-defined owner: {:?}",
        sc.drops
    );
    // The owner's release is its in-body last-use Free.
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.target == post_break_owner
                && fp.position.anchor_inst().unwrap() != break_ref),
        "owner must be freed by its in-body last-use Free: {:?}",
        sc.drops
    );
}

#[test]
fn continue_before_in_loop_owner_def_schedules_no_jump_free() {
    // `while true: i += 1; if i == 1: continue; child = make(i)` — the
    // continue lexically PRECEDES the inside-loop owner definition.
    // The same jump-path guard as break applies: pre-fix the scheduler
    // emitted a continue-anchored Free for the not-yet-defined owner
    // (latent garbage/double free — it lowered through the binding's
    // home slot, whose content at the jump is a stale or uninitialized
    // buffer). The owner's release is its in-body last-use Free.
    let src = r#"
struct P:
	text: str
	next: int

fn make(x: int) -> P:
	return P{text=int_to_str(x) + "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", next=x}

fn main():
	mut i = 0
	while true:
		i += 1
		if i == 1:
			continue
		if i > 3:
			break
		child = make(i)
		print(child.text)
	print("done")
"#;
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "expected no errors: {diags:?}"
    );
    let idx = tirs
        .iter()
        .position(|t| pool.str(t.name) == "main")
        .unwrap();
    let tir = &tirs[idx];
    let sc = take_function_sidecar(&mut sidecar, idx);

    let mut continue_ref = None;
    let mut post_continue_owner = None;
    let child = pool.intern_str("child");
    for r in 1..tir.instructions.len() {
        let rr = TirRef::from_raw(r as u32);
        match tir.instructions[r].tag {
            TirTag::Continue => continue_ref = Some(rr),
            TirTag::VarDecl => {
                let v = tir.var_decl_view(rr);
                if v.name == child {
                    post_continue_owner = Some(v.initializer);
                }
            }
            _ => {}
        }
    }
    let continue_ref = continue_ref.expect("continue found");
    let post_continue_owner = post_continue_owner.expect("child decl found");
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == continue_ref
                && fp.target == post_continue_owner),
        "a Free anchored at the continue must not target the not-yet-defined owner: {:?}",
        sc.drops
    );
    assert!(
        sc.drops.iter().any(|fp| fp.target == post_continue_owner
            && fp.position.anchor_inst().unwrap() != continue_ref),
        "owner must be freed by its in-body last-use Free: {:?}",
        sc.drops
    );
}

#[test]
fn consume_then_rebind_in_loop_converges() {
    // G3: the body below consumes the binding's current value and
    // then reseats the binding — the shape whose entry state the old
    // feedback merge (binding-aware override applied per pass) flipped
    // back and forth forever, existing only by grace of the
    // load-bearing two-pass cap. Under the convergent iteration the
    // plain operator reaches its fixed point and the override applies
    // once as a polish computed against the pre-loop snapshot, so the
    // loop header keeps the pre-rebind owner Valid (the reseated home
    // holds a fresh value each iteration) — no E0020, and the
    // pre-rebind buffer is released exactly once (free_on_reassign).
    // A body needing >2 plain passes is not constructible with the
    // current walk: every reachable transition fires in pass 1 (the
    // walk is state-independent), so the second pass's post-body state
    // already equals the merged entry — the height bound is the
    // guarantee, never the expectation.
    let src = "fn take(move s: str):\n\tprint(s)\n\nfn main():\n\tmut s = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" + \"!\"\n\twhile true:\n\t\ttake(s)\n\t\ts = \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\" + \"?\"\n\t\tbreak\n\tprint(s)\n\tprint(\"\\n\")\n";
    let (diags, mut sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "consume-then-rebind in a loop must compile clean: {diags:?}"
    );
    let _sc = take_function_sidecar(&mut sidecar, 1);
    // (The reseat emits no free_on_reassign: the consume transferred
    // the pre-rebind buffer to `take`'s move parameter, so there is
    // nothing left for the reassign to release. The load-bearing pin
    // is the clean compile above — the old feedback merge reached it
    // only by cap accident.)
    let _ = tirs;
}

#[test]
fn match_arms_with_continue_and_break_in_loop_converge() {
    let src = "fn main():\n\tmut i = 0\n\twhile i < 5:\n\t\ti += 1\n\t\tmatch i:\n\t\t\t2:\n\t\t\t\ttext = int_to_str(i) + \" continue payload that needs heap storage\"\n\t\t\t\tcontinue\n\t\t\t4:\n\t\t\t\ttext = int_to_str(i) + \" break payload that needs heap storage\"\n\t\t\t\tbreak\n\t\t\t_:\n\t\t\t\ttext = int_to_str(i) + \" ordinary payload that needs heap storage\"\n\t\t\t\tprint(text)\n\tprint(\"done\\n\")\n";
    let diags = check_src(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "match arms with continue, break, and fallthrough must compile; got {diags:?}"
    );
}

#[test]
fn match_continue_arms_free_loop_local_before_continue() {
    // Each continuing arm must release the current iteration's string.
    let src = "fn main():\n\tmut i = 0\n\twhile i < 6:\n\t\ti += 1\n\t\ttext = int_to_str(i) + \" loop payload that needs heap storage\"\n\t\tmatch i:\n\t\t\t1:\n\t\t\t\tcontinue\n\t\t\t3:\n\t\t\t\tcontinue\n\t\t\t_:\n\t\t\t\tprint(text)\n\tprint(\"\\ndone\\n\")\n";
    let (diags, mut sidecar, tirs, mut pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "no errors expected; got {diags:?}"
    );
    let tir = &tirs[0];
    let text_name = pool.intern_str("text");
    let text_init = (1..tir.instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .find_map(|r| {
            if tir.inst(r).tag == ryo_core::tir::TirTag::VarDecl {
                let decl = tir.var_decl_view(r);
                (decl.name == text_name).then_some(decl.initializer)
            } else {
                None
            }
        })
        .expect("text's initializer");
    let continue_insts: Vec<_> = (1..tir.instructions.len())
        .map(|i| ryo_core::tir::TirRef::from_raw(i as u32))
        .filter(|&inst| tir.inst(inst).tag == ryo_core::tir::TirTag::Continue)
        .collect();
    assert_eq!(continue_insts.len(), 2, "expected both continue arms");
    let f = take_function_sidecar(&mut sidecar, 0);
    for inst in continue_insts {
        assert_eq!(
            f.drops
                .iter()
                .filter(|drop| {
                    drop.target == text_init
                        && matches!(
                            drop.position,
                            ryo_core::flow::DropPosition::AfterStep { inst: anchor }
                                if anchor == inst
                        )
                })
                .count(),
            1,
            "each continue must release text once after its instruction; got {:?}",
            f.drops
        );
    }
}

#[test]
fn loop_body_return_converges_and_does_not_poison_post_loop_use() {
    // Regression (G3 convergence test): a loop whose body's live exits
    // are Return-only (`while flag: return s`). The body moves `s` on a
    // path that reaches neither the back-edge nor the post-loop join,
    // so the entry join DISCARDS the body's Moved state every pass — a
    // genuine fixed point — while the raw post-body state keeps
    // differing from the entry forever. Comparing the convergence test
    // against the raw post-body state (entry vs `after`) therefore never
    // went false and the height cap aborted the compile. The
    // convergence test now compares the previous entry against the
    // MERGED state (the one actually fed back), which is a no-op on the
    // second pass. Two poison regressions are pinned alongside: the
    // merge's binding-aware override must not write the body's
    // path-local Moved/reseat state into the joined binding state (the
    // post-loop `return s` would trip a spurious E0020), and the
    // in-loop `return s` must still record the move for the return
    // epilogue (no leak of the returned buffer).
    let src = "fn choose(flag: bool) -> str:\n\ts = \"hello\"\n\twhile flag:\n\t\treturn s\n\treturn s\n\nfn main():\n\tprint(choose(true))\n";
    let (diags, mut sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "loop-body return must compile clean (no spurious E0020 on the post-loop use): {diags:?}"
    );
    // The returned value is consumed by the caller — it must NOT be
    // freed inside `choose` (no obligation of any kind on `s`'s owner
    // in the final schedule: the in-loop return's epilogue pass sees
    // the consume record and skips it).
    let choose_idx = tirs
        .iter()
        .position(|t| _pool.str(t.name) == "choose")
        .expect("choose's tir");
    let sc = take_function_sidecar(&mut sidecar, choose_idx);
    let s_init = (1..tirs[choose_idx].instructions.len() as u32)
        .map(TirRef::from_raw)
        .find(|&r| tirs[choose_idx].inst(r).tag == TirTag::StrConst)
        .expect("the s initializer");
    assert!(
        sc.drops.iter().all(|fp| fp.target != s_init),
        "a value returned (in-loop or post-loop) must not be freed in the callee: {:?}",
        sc.drops
    );
}

#[test]
fn loop_multiple_live_views_converges() {
    // R4-A: two loop-carried views of one root (plus a second root)
    // must reach the fixed point under the membership compare — no
    // debug_assert, no cap abort, no spurious freeze/move diagnostic.
    // Both views' last use is inside the body, so P4 pruning kills them
    // at the loop exit; while iterating, the back-edge set is {v, w}.
    let src = "fn main():\n\ts = \"abcdefghijklmnopqrstuvwxyz\"\n\tt = \"0123456789\"\n\tv = s[0:10]\n\tw = s[10:20]\n\tu = t[0:5]\n\tmut i = 0\n\twhile i < 3:\n\t\tprint(v)\n\t\tprint(w)\n\t\tprint(u)\n\t\ti += 1\n\tprint(s)\n\tprint(t)\n";
    let (diags, mut _sidecar, _tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "multiple loop-carried views must compile clean: {diags:?}"
    );
}

#[test]
fn loop_branch_dependent_view_liveness_converges() {
    // R4-A: a view live on ONE arm of an if/else inside the loop. The
    // arm join unions the projection sets (P2), so the back-edge
    // membership differs from a pass-1 entry that lacks the arm-local
    // view — the membership compare must see the growth, re-walk, and
    // converge rather than abort at the cap or accept a state that was
    // never walked.
    let src = "fn main():\n\ts = \"hello there friend\"\n\tmut i = 0\n\twhile i < 2:\n\t\tif i == 0:\n\t\t\tv = s[0:5]\n\t\t\tprint(v)\n\t\telse:\n\t\t\tprint(\"skip\")\n\t\ti += 1\n\tprint(s)\n";
    let (diags, mut _sidecar, _tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "branch-dependent view liveness in a loop must compile clean: {diags:?}"
    );
}

#[test]
fn nested_loops_with_carried_views_converges() {
    // R4-A: a pre-outer-loop view read inside an INNER loop — the inner
    // fixed point runs with the view live at its back-edge, then the
    // outer fixed point re-walks the whole inner region. Both must
    // converge under the membership compare.
    let src = "fn main():\n\ts = \"abcdefghijklmnopqrst\"\n\tv = s[0:10]\n\tmut i = 0\n\twhile i < 2:\n\t\tmut j = 0\n\t\twhile j < 2:\n\t\t\tprint(v)\n\t\t\tj += 1\n\t\ti += 1\n\tprint(s)\n";
    let (diags, mut _sidecar, _tirs, _pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "nested loops with a carried view must compile clean: {diags:?}"
    );
}
