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
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    assert!(
        sidecar.free_schedule.iter().any(|fp| fp.target == lit),
        "expected inside-loop StrConst to be scheduled for Free; got: {:?}",
        sidecar.free_schedule
    );
    assert_eq!(
        sidecar
            .free_schedule
            .iter()
            .filter(|fp| fp.target == lit)
            .count(),
        1,
        "expected exactly one Free for the inside-loop StrConst; got: {:?}",
        sidecar.free_schedule
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
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    let frees: Vec<_> = sidecar
        .free_schedule
        .iter()
        .filter(|fp| fp.target == alloc)
        .collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the pre-loop owner; got: {:?}",
        sidecar.free_schedule
    );
    assert_eq!(
        frees[0].after, wl,
        "a pre-loop owner whose last use is in the loop is freed at the loop exit (covers break, normal-exit, and zero-iteration paths); got: {:?}",
        sidecar.free_schedule
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
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    // The only Free for `alloc` must be anchored on `print_call`,
    // and we must NOT have any Free anchored on `cont`.
    let frees_for_alloc: Vec<_> = sidecar
        .free_schedule
        .iter()
        .filter(|fp| fp.target == alloc)
        .collect();
    assert_eq!(
        frees_for_alloc.len(),
        1,
        "expected exactly one Free for `alloc`; got: {:?}",
        sidecar.free_schedule
    );
    assert_ne!(
        frees_for_alloc[0].after, cont,
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
    let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sc, 0);
    // No Free for `lit` anchored on the continue jump:
    assert!(
        !sc.free_schedule
            .iter()
            .any(|fp| fp.target == lit && fp.after == cont),
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
    let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sc, 0);
    // Under the buggy compiler, `lit1` is defensively freed on `cont`:
    let freed_on_cont = sc
        .free_schedule
        .iter()
        .any(|fp| fp.target == lit1 && fp.after == cont);
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
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    assert!(
        sidecar
            .free_schedule
            .iter()
            .any(|fp| fp.after == brk && fp.target == alloc),
        "expected a Free for the inside-loop owner anchored on break; got: {:?}",
        sidecar.free_schedule
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
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    // Exactly one Free for `alloc`, anchored on `print_call` (the
    // last-use), not on `brk`.
    let frees_for_alloc: Vec<_> = sidecar
        .free_schedule
        .iter()
        .filter(|fp| fp.target == alloc)
        .collect();
    assert_eq!(
        frees_for_alloc.len(),
        1,
        "expected exactly one Free for `alloc`; got: {:?}",
        sidecar.free_schedule
    );
    assert_ne!(
        frees_for_alloc[0].after, brk,
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
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    // Two Frees expected: one anchored on s_var (the Var read in
    // the then-arm — collect_last_uses anchors on Var reads, not
    // their wrapping Call), one anchored on brk (cross-branch
    // leak fix).
    let frees_for_alloc: Vec<_> = sidecar
        .free_schedule
        .iter()
        .filter(|fp| fp.target == alloc)
        .collect();
    assert!(
        frees_for_alloc.iter().any(|fp| fp.after == s_var),
        "expected then-arm last-use Free anchored on s_var (the Var read); got: {:?}",
        sidecar.free_schedule
    );
    assert!(
        frees_for_alloc.iter().any(|fp| fp.after == brk),
        "expected cross-branch jump-anchored Free on break; got: {:?}",
        sidecar.free_schedule
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
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    assert!(
        sidecar
            .free_schedule
            .iter()
            .any(|fp| fp.after == cont && fp.target == alloc),
        "expected a Free for the inside-loop owner anchored on continue; got: {:?}",
        sidecar.free_schedule
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
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    let diags = sink.into_diags();
    assert!(
        !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
        "did not expect DeadStore for `s` (it is read inside the loop): {:?}",
        diags
    );

    let count = sidecar
        .free_schedule
        .iter()
        .filter(|fp| fp.target == lit)
        .count();
    assert_eq!(
        count, 1,
        "expected exactly one Free for pre-loop owner read only inside loop; got {} schedule={:?}",
        count, sidecar.free_schedule
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
    // tuple differs and the diverged path runs. The if schedules a
    // branch-gated Free for x on the else arm. Before the fix the
    // scratch walk wrote that FreePoint into the REAL sidecar and
    // the re-walk wrote it again under freshly minted BranchIds —
    // two entries for one arm, the first gated on a BranchId that
    // `if_branches` no longer records and codegen never activates.
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
    let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
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

    // Exactly ONE branch-gated Free for the if's else arm,
    // gated on the BranchId `if_branches` actually records.
    assert_eq!(
        sc.if_branches.len(),
        1,
        "one if → one entry set; got {:?}",
        sc.if_branches
    );
    let ids = sc
        .if_branches
        .get(&if_inst)
        .expect("if_branches entry for the loop-body if");
    let gated: Vec<_> = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.branch.is_some())
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "diverged loop must schedule the else-arm Free exactly once; got {gated:?}"
    );
    assert_eq!(
        gated[0].branch, ids.else_branch,
        "the gated Free must reference the live else BranchId; if_branches = {ids:?}"
    );
}
