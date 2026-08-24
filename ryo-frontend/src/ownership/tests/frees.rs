use super::super::*;
use super::common::*;
use crate::builtins::is_borrowed_scalar_param;

#[test]
fn dead_store_schedules_free_after_decl() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s_name = pool.intern_str("s");
    let hello = pool.intern_str("hello");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void: s: str = "hello"   # never read
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s_name, false, str_ty, lit, span);
    let tir = tb.finish(&[decl]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);

    // W0001 fires.
    let diags = sink.into_diags();
    assert!(
        diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
        "expected DeadStore warning"
    );

    // Free anchored after the VarDecl, target = the literal's TirRef.
    assert!(
        sidecar
            .free_schedule
            .iter()
            .any(|fp| fp.after == decl && fp.target == lit && fp.branch.is_none()),
        "expected dead-store Free anchored at decl with target=lit; got: {:?}",
        sidecar.free_schedule
    );

    // Exactly one Free for `lit` — guards against Task 3/4 ever
    // double-counting (anonymous-temp pass + dead-store pass both
    // emitting for the same owner).
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
fn ryo_panic_str_arg_excluded_via_abi_registry() {
    // Regression test for the borrowed-scalar exclusion. The
    // StrConst arg of `__ryo_panic` uses the borrowed-scalar ABI in
    // codegen — codegen passes the raw .rodata pointer with cap=0
    // and never owns the buffer. The
    // ownership pass excludes it from `temp_owners` by consulting the
    // `builtins` ABI registry (`is_borrowed_scalar_param`) rather than
    // a `pool.str(name) == "__ryo_panic"` name-match, so the
    // anonymous-temp Free pass does not schedule a Free for it.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let panic_name = pool.intern_str("__ryo_panic");
    let msg = pool.intern_str("boom");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void: __ryo_panic("boom", 4)
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let str_arg = tb.str_const(msg, str_ty, span);
    let len_arg = tb.int_const(4, int_ty, span);
    let call = tb.call(
        panic_name,
        &[str_arg, len_arg],
        &all_borrow(&[str_arg, len_arg]),
        void,
        span,
    );
    let tir = tb.finish(&[call]);

    let mut sink = DiagSink::new();
    let mut sidecar_map = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar_map, 0);

    // No scheduled Free should target __ryo_panic's StrConst arg —
    // codegen's borrowed-scalar ABI never frees it.
    assert!(
        sidecar.free_schedule.iter().all(|fp| fp.target != str_arg),
        "expected no scheduled Free for __ryo_panic's StrConst arg, got: {:?}",
        sidecar.free_schedule
    );

    // The exclusion is driven by the ABI registry: `__ryo_panic`
    // passes param 0 (the message) via the borrowed-scalar ABI, but not
    // param 1 (the length) or any out-of-range index.
    assert!(
        is_borrowed_scalar_param(panic_name, &pool, 0),
        "__ryo_panic param 0 must be flagged borrowed-scalar by the registry"
    );
    assert!(
        !is_borrowed_scalar_param(panic_name, &pool, 1),
        "__ryo_panic param 1 must not be flagged borrowed-scalar"
    );
}

#[test]
fn last_use_scheduled_for_unmoved_local() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let hello = pool.intern_str("hello");
    let s_name = pool.intern_str("s");
    let print_name = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    // fn main() -> void:
    //     s: str = "hello"
    //     print(s)
    let mut b = TirBuilder::new(main, vec![], void, span);
    let lit = b.str_const(hello, str_ty, span);
    let decl = b.var_decl(s_name, false, str_ty, lit, span);
    let var_read = b.var(s_name, str_ty, span);
    let call = b.call(
        print_name,
        &[var_read],
        &all_borrow(&[var_read]),
        void,
        span,
    );
    let stmt = b.unary(TirTag::ExprStmt, void, call, span);
    let tir = b.finish(&[decl, stmt]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(sink.is_empty(), "expected no diagnostics");
    assert_eq!(sidecar.free_schedule.len(), 1);
    assert_eq!(sidecar.free_schedule[0].target, lit);
    assert_eq!(sidecar.free_schedule[0].after, var_read);
    assert!(sidecar.free_schedule[0].branch.is_none());
}

#[test]
fn reassignment_records_free_on_old_owner() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let hello = pool.intern_str("hello");
    let world = pool.intern_str("world");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    // fn main():
    //     mut s: str = "hello"
    //     s = "world"
    //     print(s)
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let l1 = tb.str_const(hello, str_ty, span);
    let decl = tb.var_decl(s, /* mutable = */ true, str_ty, l1, span);
    let l2 = tb.str_const(world, str_ty, span);
    let assign = tb.assign(s, str_ty, l2, span);
    let var_read = tb.var(s, str_ty, span);
    let call = tb.call(print, &[var_read], &all_borrow(&[var_read]), void, span);
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[decl, assign, stmt]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(sink.is_empty(), "expected no diagnostics");

    // Reassign frees l1 (old owner) keyed on the Assign inst.
    assert_eq!(
        sidecar.free_on_reassign.get(&assign),
        Some(&l1),
        "expected free_on_reassign[assign] = l1"
    );

    // Last-use frees l2 (new owner reaches function exit via print(s)).
    assert!(
        sidecar
            .free_schedule
            .iter()
            .any(|fp| fp.target == l2 && fp.after == var_read && fp.branch.is_none()),
        "expected last-use Free for l2; got: {:?}",
        sidecar.free_schedule
    );

    // No dead-store Free for l1 — it's covered by free_on_reassign.
    assert!(
        !sidecar.free_schedule.iter().any(|fp| fp.target == l1),
        "l1 must not be in free_schedule (it's in free_on_reassign): {:?}",
        sidecar.free_schedule
    );
}

#[test]
fn concat_intermediate_freed_after_consumer() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    use std::collections::HashSet;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let print = pool.intern_str("print");
    let a = pool.intern_str("a");
    let b = pool.intern_str("b");
    let span = SimpleSpan::new((), 0..0);

    // print("a" + "b")
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let la = tb.str_const(a, str_ty, span);
    let lb = tb.str_const(b, str_ty, span);
    let cat = tb.binary(TirTag::StrConcat, str_ty, la, lb, span);
    let call = tb.call(print, &[cat], &all_borrow(&[cat]), void, span);
    let stmt = tb.unary(TirTag::ExprStmt, void, call, span);
    let tir = tb.finish(&[stmt]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(sink.is_empty());

    // Three Frees: la, lb, cat. Anchored after consumers (la/lb on
    // cat, cat on call). Order-independent.
    let targets: HashSet<TirRef> = sidecar.free_schedule.iter().map(|fp| fp.target).collect();
    assert!(targets.contains(&la), "expected la in free_schedule");
    assert!(targets.contains(&lb), "expected lb in free_schedule");
    assert!(targets.contains(&cat), "expected cat in free_schedule");
    assert_eq!(sidecar.free_schedule.len(), 3);
}

#[test]
fn last_use_uses_pre_rebind_owner_not_post() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let n = pool.intern_str("n");
    let alice = pool.intern_str("Alice");
    let bob = pool.intern_str("Bob");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    // fn main():
    //     mut n: str = "Alice"
    //     print(n)        # last-use of "Alice"
    //     n = "Bob"
    //     print(n)        # last-use of "Bob"
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let alice_lit = tb.str_const(alice, str_ty, span);
    let decl = tb.var_decl(n, true, str_ty, alice_lit, span);
    let read1 = tb.var(n, str_ty, span);
    let call1 = tb.call(print, &[read1], &all_borrow(&[read1]), void, span);
    let stmt1 = tb.unary(TirTag::ExprStmt, void, call1, span);
    let bob_lit = tb.str_const(bob, str_ty, span);
    let assign = tb.assign(n, str_ty, bob_lit, span);
    let read2 = tb.var(n, str_ty, span);
    let call2 = tb.call(print, &[read2], &all_borrow(&[read2]), void, span);
    let stmt2 = tb.unary(TirTag::ExprStmt, void, call2, span);
    let tir = tb.finish(&[decl, stmt1, assign, stmt2]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(sink.is_empty(), "expected no diagnostics");

    // The Free for "Alice" must come from free_on_reassign[assign],
    // NOT from last-use scheduling. Last-use should target "Bob"
    // (anchored after read2), not "Alice".
    assert_eq!(
        sidecar.free_on_reassign.get(&assign),
        Some(&alice_lit),
        "expected free_on_reassign[assign] = alice_lit"
    );
    // free_schedule must not contain a FreePoint with target=alice_lit
    // anchored at read1 (the bug's signature was wrong-target via post-rebind current_owner).
    assert!(
        !sidecar
            .free_schedule
            .iter()
            .any(|fp| fp.after == read1 && fp.target == alice_lit),
        "expected no last-use Free for Alice anchored at read1 (Alice freed via free_on_reassign): {:?}",
        sidecar.free_schedule
    );
    // Last-use Free for Bob must exist anchored at read2.
    assert!(
        sidecar
            .free_schedule
            .iter()
            .any(|fp| fp.after == read2 && fp.target == bob_lit && fp.branch.is_none()),
        "expected last-use Free for Bob anchored at read2; got: {:?}",
        sidecar.free_schedule
    );
}

#[test]
fn unconsumed_move_param_schedules_free_at_function_end() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let consume = pool.intern_str("consume"); // fn consume(move s: str) -> void
    let s_name = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        consume,
        vec![TirParam {
            name: s_name,
            ty: str_ty,
            mode: ParamMode::Move,
            span,
        }],
        void,
        span,
    );
    // Just return, do not consume `s`.
    let ret = tb.return_void(void, span);
    let tir = tb.finish(&[ret]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);

    let virtual_ref = TirRef::param(0);
    let fp = sc
        .free_schedule
        .iter()
        .find(|fp| fp.target == virtual_ref)
        .expect("free scheduled");
    assert_eq!(fp.after, ret);
}

#[test]
fn read_move_param_frees_after_last_read_not_last_stmt() {
    // fn f(move s: str): print(s); print(42) — the param's Free
    // anchors after its last read (the `Var` inside print(s)),
    // not after the later statement that never touches it.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let void = pool.void();
    let f = pool.intern_str("f");
    let s_name = pool.intern_str("s");
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
    let s_read = tb.var(s_name, str_ty, span);
    let call1 = tb.call(print, &[s_read], &all_borrow(&[s_read]), void, span);
    let n = tb.int_const(42, int_ty, span);
    let call2 = tb.call(print, &[n], &all_borrow(&[n]), void, span);
    let tir = tb.finish(&[call1, call2]);
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
        frees[0].after, s_read,
        "the Free must anchor after the param's last read; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn param_last_read_inside_loop_frees_after_loop() {
    // fn f(move s: str, cond: bool): while cond: print(s) — the
    // last read sits inside the loop; anchoring after it would fire
    // the Free per iteration (UAF on the next iteration's read), so
    // the Free moves to after the loop statement.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let f = pool.intern_str("f");
    let s_name = pool.intern_str("s");
    let cond_name = pool.intern_str("cond");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        f,
        vec![
            TirParam {
                name: s_name,
                ty: str_ty,
                mode: ParamMode::Move,
                span,
            },
            TirParam {
                name: cond_name,
                ty: bool_ty,
                mode: ParamMode::Borrow,
                span,
            },
        ],
        void,
        span,
    );
    let cond = tb.var(cond_name, bool_ty, span);
    let s_read = tb.var(s_name, str_ty, span);
    let call = tb.call(print, &[s_read], &all_borrow(&[s_read]), void, span);
    let while_stmt = tb.while_loop(cond, &[call], void, span);
    let tir = tb.finish(&[while_stmt]);
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
        frees[0].after, while_stmt,
        "a param last read inside a loop must be freed after the loop, not inside it; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn never_read_move_param_still_freed_once_after_last_stmt() {
    // fn f(move s: str): print(42) — a never-read owned param must
    // still be freed exactly once, anchored after the last body
    // statement (there is no last read to anchor on).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let void = pool.void();
    let f = pool.intern_str("f");
    let s_name = pool.intern_str("s");
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
    let n = tb.int_const(42, int_ty, span);
    let call = tb.call(print, &[n], &all_borrow(&[n]), void, span);
    let tir = tb.finish(&[call]);
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
        frees[0].after, call,
        "a never-read param keeps the last-body-statement anchor; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn conditional_move_param_schedules_branch_gated_free() {
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let consume_cond = pool.intern_str("consume_cond");
    let s_name = pool.intern_str("s");
    let cond_name = pool.intern_str("cond");
    let take = pool.intern_str("take");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(
        consume_cond,
        vec![
            TirParam {
                name: s_name,
                ty: str_ty,
                mode: ParamMode::Move,
                span,
            },
            TirParam {
                name: cond_name,
                ty: bool_ty,
                mode: ParamMode::Borrow,
                span,
            },
        ],
        void,
        span,
    );

    let cond_val = tb.var(cond_name, bool_ty, span);
    let s_val_then = tb.var(s_name, str_ty, span);
    let call_then = tb.call(take, &[s_val_then], &[ParamMode::Move], void, span);

    let s_val_else = tb.var(s_name, str_ty, span);
    let call_else = tb.call(take, &[s_val_else], &[ParamMode::Borrow], void, span);

    let if_stmt = tb.if_stmt(cond_val, &[call_then], &[], Some(&[call_else]), void, span);

    let tir = tb.finish(&[if_stmt]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);

    let virtual_ref = TirRef::param(0);
    // We expect a branch-gated free for the parameter in the else branch!
    let fp = sc
        .free_schedule
        .iter()
        .find(|fp| fp.target == virtual_ref && fp.branch.is_some())
        .expect("branch-gated free scheduled");
    assert_eq!(fp.after, call_else);
}

#[test]
fn rebind_then_reassign_does_not_double_free() {
    // s = "a"   (temp_a bound to s)
    // s = "b"   (reassign: free_on_reassign covers temp_a; s -> temp_b)
    // At exit, temp_a is Valid (resurrected by rebind) and NOT in
    // current_owner.values() (s moved to temp_b). Without `named_inits`
    // containing temp_a, the anon-temp pass would schedule a second
    // Free for it -> double-free. It must be classified.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s_name = pool.intern_str("s");
    let a = pool.intern_str("a");
    let b = pool.intern_str("b");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(a, str_ty, span);
    let decl = tb.var_decl(s_name, true, str_ty, lit_a, span);
    let lit_b = tb.str_const(b, str_ty, span);
    let assign = tb.assign(s_name, str_ty, lit_b, span);
    let tir = tb.finish(&[decl, assign]);

    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);

    // temp_a (lit_a) is released via `free_on_reassign` (codegen lowers
    // its destructor at the Assign), so it must NOT also appear in
    // `free_schedule` — that would be a double-free. The anon-temp pass
    // skips it because it is a named init (the VarDecl initializer);
    // if that filter were missing the anon-temp pass would schedule a
    // second Free here. See the sibling invariant in
    // `reassignment_records_free_on_old_owner`.
    let a_frees = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.target == lit_a)
        .count();
    assert_eq!(
        a_frees, 0,
        "temp_a must not be in free_schedule (it's in free_on_reassign); got {sc:?}"
    );
    // Positive half: temp_a must actually be recorded in
    // free_on_reassign (mirrors `reassignment_records_free_on_old_owner`).
    // Without this the test would pass on a leak (temp_a never freed).
    assert_eq!(
        sc.free_on_reassign.get(&assign),
        Some(&lit_a),
        "temp_a must be freed once via free_on_reassign (not leaked); got {sc:?}",
    );
    // lit_b is never read, so it is a dead store and is freed exactly
    // once via the dead-store pass (anchored at the Assign).
    let b_frees = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.target == lit_b)
        .count();
    assert_eq!(
        b_frees, 1,
        "temp_b freed exactly once via dead-store; got {sc:?}"
    );
    let _ = assign;
}

#[test]
fn rebind_then_read_no_double_free() {
    // s = "a"; print(s)  -> temp_a backed by s, last-use frees it once.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let a = pool.intern_str("a");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(a, str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let v = tb.var(s, str_ty, span);
    let call = tb.call(print, &[v], &all_borrow(&[v]), void, span);
    let tir = tb.finish(&[decl, call]);
    let mut sink = DiagSink::new();
    let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sc, 0);
    let lit_frees = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.target == lit)
        .count();
    assert_eq!(
        lit_frees, 1,
        "backed temp freed exactly once via last-use; got {sc:?}"
    );
}

#[test]
fn rebind_in_loop_converges_no_spurious_free() {
    // s = "a"; while c: s = "a"   (rebind each iteration)
    //
    // The loop-body temp `body_lit` is a named init (the Assign's
    // value), so the anon-temp pass must SKIP it (static classifier)
    // and let the dead-store pass own its single Free.
    // The rejected dynamic `current_owner.values()` classifier would
    // NOT skip it: at the loop merge the entry-state owner (lit) wins
    // via first-write-wins, so body_lit drops out of
    // current_owner.values() and the anon-temp pass schedules a
    // spurious second Free -> double-free. Assert body_lit is
    // scheduled exactly once (static) rather than twice (dynamic).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let void = pool.void();
    let bool_ty = pool.bool_();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let a = pool.intern_str("a");
    let c = pool.intern_str("c");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit = tb.str_const(a, str_ty, span);
    let decl = tb.var_decl(s, true, str_ty, lit, span);
    let cond = tb.var(c, bool_ty, span);
    let body_lit = tb.str_const(a, str_ty, span);
    let body_assign = tb.assign(s, str_ty, body_lit, span);
    let lp = tb.while_loop(cond, &[body_assign], void, span);
    let tir = tb.finish(&[decl, lp]);
    let mut sink = DiagSink::new();
    let mut sc = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sc, 0);
    // body_lit must be scheduled exactly once (dead-store pass owns
    // it; anon-temp skips it as a named init). The dynamic
    // current_owner.values() classifier would schedule it twice
    // (anon-temp + dead-store) -> the double-free this test guards.
    let body_lit_frees = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.target == body_lit)
        .count();
    assert_eq!(
        body_lit_frees, 1,
        "loop-rebound temp must be freed exactly once (static classifier skips it in anon-temp); got {sc:?}"
    );
    // lit (the decl init) is freed via free_on_reassign at the
    // loop-body Assign, so it must NOT also appear in free_schedule.
    let lit_frees = sc
        .free_schedule
        .iter()
        .filter(|fp| fp.target == lit)
        .count();
    assert_eq!(
        lit_frees, 0,
        "decl-init temp must not be in free_schedule (it's in free_on_reassign); got {sc:?}"
    );
    let _ = (body_assign, lp);
}

#[test]
fn reassign_inside_if_still_frees_binding_at_last_use() {
    // Pre-existing M8.1 bug: `mut s = "a"; if c:
    // s = "b"; print(s)`. The merge keeps the pre-branch owner, so the
    // reassign-target guard must not skip its last-use Free — on the
    // not-taken path the binding still owns `lit_a`. Codegen emits the
    // Free from the binding's current StrLocals (path-correct buffer).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit_b, span);
    let if_s = tb.if_stmt(cond, &[asg], &[], None, void, span);
    let sv = tb.var(s, str_ty, span);
    let pr = tb.call(print, &[sv], &[ParamMode::Borrow], void, span);
    let tir = tb.finish(&[decl, if_s, pr]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let diags = sink.into_diags();
    assert!(
        !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
        "s is read after the if — no dead-store warning; got {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_on_reassign.contains_key(&asg),
        "the taken arm must drop the pre-reassign buffer; free_on_reassign = {:?}",
        sc.free_on_reassign
    );
    assert_eq!(
        sc.free_schedule
            .iter()
            .filter(|fp| fp.target == lit_a)
            .count(),
        1,
        "the binding's owner must still get its last-use Free (covers the not-taken path); schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        !sc.free_schedule.iter().any(|fp| fp.target == lit_b),
        "lit_b's buffer is freed through the binding's current StrLocals (same buffer as lit_a's Free on the taken path) — a second Free would double-free; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn conditional_dead_reassign_schedules_fallthrough_drop() {
    // `mut s = "a"; if c: s = "b"` with s never read after.
    // The taken arm drops "a" (free_on_reassign) and the drain frees
    // "b"; the NOT-taken path must also free "a" — via an arm-gated
    // ConditionalDeadDrop for the pre-branch owner.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit_b, span);
    let if_s = tb.if_stmt(cond, &[asg], &[], None, void, span);
    let tir = tb.finish(&[decl, if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    let drops: Vec<_> = sc
        .conditional_dead_drops
        .iter()
        .filter(|d| d.target == lit_a)
        .collect();
    assert_eq!(
        drops.len(),
        1,
        "expected one ConditionalDeadDrop for the pre-branch owner; got {:?}",
        sc.conditional_dead_drops
    );
    assert_eq!(drops[0].if_stmt, if_s, "the drop must key on the if");
    assert!(
        !drops[0].arms.is_empty(),
        "the drop must name at least one untouched arm (the fall-through)"
    );
}

#[test]
fn conditional_reassign_all_arms_reseated_no_drop() {
    // `if c: s = "b" else: s = "d"` — every arm reseats, so the
    // pre-branch buffer is dropped by free_on_reassign on every
    // path; no ConditionalDeadDrop may be scheduled.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let asg_then = tb.assign(s, str_ty, lit_b, span);
    let lit_d = tb.str_const(pool.intern_str("d"), str_ty, span);
    let asg_else = tb.assign(s, str_ty, lit_d, span);
    let if_s = tb.if_stmt(cond, &[asg_then], &[], Some(&[asg_else]), void, span);
    let tir = tb.finish(&[decl, if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.conditional_dead_drops.is_empty(),
        "all arms reseated — no untouched path, no ConditionalDeadDrop; got {:?}",
        sc.conditional_dead_drops
    );
}

#[test]
fn loop_dead_reassign_anchors_after_loop() {
    // `mut s = "a"; while c: s = "b"` with s never read
    // after. The dead-store Free must anchor AFTER THE LOOP (not
    // after the in-loop assign): the in-loop anchor never fires on
    // the zero-iteration path, leaking the pre-loop buffer. The
    // after-loop anchor emits the binding's current StrLocals —
    // final value on taken paths, pre-loop value on zero iterations.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit_b, span);
    let wl = tb.while_loop(cond, &[asg], void, span);
    let tir = tb.finish(&[decl, wl]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.after == wl && fp.target == lit_b),
        "expected a Free anchored after the loop for the binding's value; schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        !sc.free_schedule
            .iter()
            .any(|fp| fp.after == asg && fp.target == lit_b),
        "the in-loop dead-store Free must move to the loop anchor (it never fires on zero iterations); schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn loop_dead_reassign_with_return_keeps_in_body_free() {
    // When the loop body can `return`, the after-loop anchor
    // is unreachable on the return path — the in-body Free must stay
    // alongside the after-loop one.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit_b, span);
    let ret = tb.return_void(void, span);
    let wl = tb.while_loop(cond, &[asg, ret], void, span);
    let tir = tb.finish(&[decl, wl]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.after == wl && fp.target == lit_b),
        "expected the after-loop Free; schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.after == asg && fp.target == lit_b),
        "a returning body keeps the in-body Free (return path); schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn loop_local_dead_value_keeps_in_body_anchor() {
    // Guard: a value DECLARED inside the loop body is not a
    // pre-loop binding — its Free must stay anchored in the body
    // (the binding's StrLocals don't exist on the zero-iteration
    // path).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let t = pool.intern_str("t");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_x = tb.str_const(pool.intern_str("x"), str_ty, span);
    let decl = tb.var_decl(t, false, str_ty, lit_x, span);
    let wl = tb.while_loop(cond, &[decl], void, span);
    let tir = tb.finish(&[wl]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.after == decl && fp.target == lit_x),
        "loop-local dead value keeps its in-body Free; schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        !sc.free_schedule.iter().any(|fp| fp.after == wl),
        "loop-local value must NOT be re-anchored after the loop; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn last_use_inside_loop_anchors_after_loop() {
    // Conditional last use: `mut s = "a";
    // for i in range(0, 3): print(s)`. The last read of `s` is
    // inside the loop body, but freeing there is a UAF — the next
    // iteration reads the freed buffer. The value is dead on ALL
    // paths only at the loop exit, so the Free must anchor after
    // the loop statement.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let i = pool.intern_str("i");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let zero = tb.int_const(0, int_ty, span);
    let three = tb.int_const(3, int_ty, span);
    let sv = tb.var(s, str_ty, span);
    let pr = tb.call(print, &[sv], &[ParamMode::Borrow], void, span);
    let fr = tb.for_range(i, zero, three, &[pr], void, span);
    let tir = tb.finish(&[decl, fr]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.after == fr && fp.target == lit_a),
        "the last-use Free must anchor after the loop (in-body anchoring frees s between iterations — UAF); schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        !sc.free_schedule
            .iter()
            .any(|fp| fp.after == sv && fp.target == lit_a),
        "no Free may anchor after the in-loop read itself; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn last_use_inside_if_anchors_after_if() {
    // Same family through an if: `mut s = "a"; if d: print(s)` —
    // anchoring after the in-arm read leaks `s` on the not-taken
    // path; the merge point is where the value is dead on all paths.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sv = tb.var(s, str_ty, span);
    let pr = tb.call(print, &[sv], &[ParamMode::Borrow], void, span);
    let if_s = tb.if_stmt(cond, &[pr], &[], None, void, span);
    let tir = tb.finish(&[decl, if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.after == if_s && fp.target == lit_a),
        "the last-use Free must anchor after the if (in-arm anchoring leaks on the not-taken path); schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn return_epilogue_frees_live_local() {
    // Return-epilogue: `mut s = "a"; if d: print(s) else: return`.
    // On the else path, `s` is still live at the return and nothing
    // freed it — the last-use Free anchors in the sibling then-arm,
    // which the else path never reaches. An early return must
    // destroy the function's still-owned locals on ITS path.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sv = tb.var(s, str_ty, span);
    let pr = tb.call(print, &[sv], &[ParamMode::Borrow], void, span);
    let ret = tb.return_void(void, span);
    let if_s = tb.if_stmt(cond, &[pr], &[], Some(&[ret]), void, span);
    let tir = tb.finish(&[decl, if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.after == ret && fp.target == lit_a),
        "the still-live local must be freed on the early-return path; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn return_epilogue_skips_consumed_return_value() {
    // `fn f() -> str: s = "a"; return s` — the returned value moved
    // out; an epilogue Free for it would be a use-after-free in the
    // caller.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{TirBuilder, TirData, TirTag};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let f = pool.intern_str("f");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(f, vec![], str_ty, span);
    let lit = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let sv = tb.var(s, str_ty, span);
    let ret = tb.push_typed(TirTag::Return, TirData::UnOp(sv), str_ty, span);
    let tir = tb.finish(&[decl, ret]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !sc.free_schedule
            .iter()
            .any(|fp| fp.target == lit && fp.after == ret),
        "the returned value moved out — no epilogue Free for it; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn return_epilogue_skips_dead_store_owned() {
    // `mut s = "a"; if d: return` with s never read: the dead-store
    // drain already frees `s` right after its declaration (covering
    // every path), so no epilogue Free may be added for it.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let ret = tb.return_void(void, span);
    let if_s = tb.if_stmt(cond, &[ret], &[], None, void, span);
    let tir = tb.finish(&[decl, if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !sc.free_schedule
            .iter()
            .any(|fp| fp.target == lit_a && fp.after == ret),
        "dead-store-owned value is already freed at its decl — no epilogue Free; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn return_epilogue_covers_move_param() {
    // `fn f(move s: str): if d: return` — the owned param is still
    // Valid at the early return; the callee must destroy it there
    // (the never-read param's Free anchors after the last body
    // stmt, which the early-return path never reaches).
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let f = pool.intern_str("f");
    let s = pool.intern_str("s");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(
        f,
        vec![TirParam {
            name: s,
            ty: str_ty,
            mode: ParamMode::Move,
            span,
        }],
        void,
        span,
    );
    let cond = tb.bool_const(true, bool_ty, span);
    let ret = tb.return_void(void, span);
    let if_s = tb.if_stmt(cond, &[ret], &[], None, void, span);
    let tir = tb.finish(&[if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.after == ret && fp.target == TirRef::param(0)),
        "an owned param must be destroyed on the early-return path; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn temp_last_use_inside_loop_not_reanchored() {
    // Guard: an anonymous temp consumed inside the loop body must
    // keep its per-iteration Free (each iteration allocates a fresh
    // value) — the re-anchor applies only to named pre-branch
    // bindings.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let i = pool.intern_str("i");
    let int_to_str = pool.intern_str("int_to_str");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let zero = tb.int_const(0, int_ty, span);
    let three = tb.int_const(3, int_ty, span);
    let iv = tb.var(i, int_ty, span);
    let call = tb.call(int_to_str, &[iv], &[ParamMode::Borrow], str_ty, span);
    let pr = tb.call(print, &[call], &[ParamMode::Borrow], void, span);
    let fr = tb.for_range(i, zero, three, &[pr], void, span);
    let tir = tb.finish(&[fr]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !sc.free_schedule.iter().any(|fp| fp.after == fr),
        "a loop-local temp must keep its per-iteration Free, not move to the loop anchor; schedule = {:?}",
        sc.free_schedule
    );
    assert!(
        sc.free_schedule
            .iter()
            .any(|fp| fp.target == call && fp.after == pr),
        "the temp's Free stays anchored after its consumer; schedule = {:?}",
        sc.free_schedule
    );
}

#[test]
fn conditional_dead_reassign_gated_on_real_else() {
    // `if c: s = "b" else: <no reassign>` with s unread after —
    // the drop must be gated on the REAL else arm's BranchId.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit_b, span);
    let lit_x = tb.str_const(pool.intern_str("x"), str_ty, span);
    let pr = tb.call(print, &[lit_x], &[ParamMode::Borrow], void, span);
    let if_s = tb.if_stmt(cond, &[asg], &[], Some(&[pr]), void, span);
    let tir = tb.finish(&[decl, if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(std::slice::from_ref(&tir), &pool, &mut sink);
    let sc = take_function_sidecar(&mut sidecar, 0);
    let else_id = sc
        .if_branches
        .get(&if_s)
        .and_then(|ids| ids.else_branch)
        .expect("else branch id");
    let drops: Vec<_> = sc
        .conditional_dead_drops
        .iter()
        .filter(|d| d.target == lit_a)
        .collect();
    assert_eq!(
        drops.len(),
        1,
        "expected one ConditionalDeadDrop; got {:?}",
        sc.conditional_dead_drops
    );
    assert!(
        drops[0].arms.contains(&else_id),
        "the drop must be gated on the else arm {:?}; got {:?}",
        else_id,
        drops[0].arms
    );
}

// ---------- M8.4: projection tracking (final spec §3.2/§3.3) ----------

#[test]
fn free_schedule_is_deterministic() {
    // Build a TIR with several owners + views; run `check` twice;
    // assert identical free_schedule.
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::TirBuilder;

    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let view_ty = pool.str_view();
    let int_ty = pool.int();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s1 = pool.intern_str("s1");
    let s2 = pool.intern_str("s2");
    let s3 = pool.intern_str("s3");
    let v = pool.intern_str("v");
    let print = pool.intern_str("print");
    let span = SimpleSpan::new((), 0..0);

    let mut tb = TirBuilder::new(main, vec![], void, span);
    // s1 = "a"; s2 = "b"; v = s1[0:1]; print(v); print(s2);
    // s3 = "c" (dead store); if true: print(s1) else: print("x");
    // print("tmp")
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl1 = tb.var_decl(s1, false, str_ty, lit_a, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let decl2 = tb.var_decl(s2, false, str_ty, lit_b, span);
    let base = tb.var(s1, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i1 = tb.int_const(1, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i1), view_ty, span);
    let vdecl = tb.var_decl(v, false, view_ty, sl, span);
    let vread = tb.var(v, view_ty, span);
    let pv = tb.call(print, &[vread], &all_borrow(&[vread]), void, span);
    let pv_stmt = tb.unary(TirTag::ExprStmt, void, pv, span);
    let r2 = tb.var(s2, str_ty, span);
    let ps2 = tb.call(print, &[r2], &all_borrow(&[r2]), void, span);
    let ps2_stmt = tb.unary(TirTag::ExprStmt, void, ps2, span);
    let lit_c = tb.str_const(pool.intern_str("c"), str_ty, span);
    let decl3 = tb.var_decl(s3, false, str_ty, lit_c, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let r1 = tb.var(s1, str_ty, span);
    let ps1 = tb.call(print, &[r1], &all_borrow(&[r1]), void, span);
    let ps1_stmt = tb.unary(TirTag::ExprStmt, void, ps1, span);
    let xlit = tb.str_const(pool.intern_str("x"), str_ty, span);
    let px = tb.call(print, &[xlit], &all_borrow(&[xlit]), void, span);
    let px_stmt = tb.unary(TirTag::ExprStmt, void, px, span);
    let ifs = tb.if_stmt(cond, &[ps1_stmt], &[], Some(&[px_stmt]), void, span);
    let tlit = tb.str_const(pool.intern_str("tmp"), str_ty, span);
    let pt = tb.call(print, &[tlit], &all_borrow(&[tlit]), void, span);
    let pt_stmt = tb.unary(TirTag::ExprStmt, void, pt, span);
    let tir = tb.finish(&[decl1, decl2, vdecl, pv_stmt, ps2_stmt, decl3, ifs, pt_stmt]);

    let run = |tir: &Tir| {
        let mut sink = DiagSink::new();
        let mut sidecar = check(std::slice::from_ref(tir), &pool, &mut sink);
        let sc = take_function_sidecar(&mut sidecar, 0);
        sc.free_schedule
            .iter()
            .map(|fp| (fp.after.raw(), fp.target.raw(), fp.branch.map(|b| b.0)))
            .collect::<Vec<_>>()
    };
    let first = run(&tir);
    let second = run(&tir);
    assert!(!first.is_empty(), "expected a non-empty free schedule");
    assert_eq!(
        first, second,
        "free_schedule must be deterministic across runs"
    );
}

#[test]
fn w0003_bound_materialize_never_escapes_warns() {
    // W0003 case B: `x = str(view)` where `x` is only borrow-read
    // (`print(x)` — a use the view itself could have served) and the
    // slice's root owner is never touched again: the allocation is
    // redundant.
    let diags = check_src("fn main():\n\ts: str = \"hi\"\n\tx: str = str(s[0:1])\n\tprint(x)\n");
    assert_eq!(
        w0003_count(&diags),
        1,
        "expected exactly one W0003; got: {diags:?}"
    );
}

#[test]
fn w0003_defensive_copy_before_source_mutation_does_not_warn() {
    // The defensive-copy exception: the source is mutated AFTER the
    // materialize point, so the owned copy genuinely outlives its
    // view (ring-buffer reuse shape) — no warning.
    let diags = check_src(
        "fn main():\n\tmut s: str = \"hi\"\n\tx: str = str(s[0:1])\n\tstr_push(&s, \"!\")\n\tprint(x)\n",
    );
    assert_eq!(
        w0003_count(&diags),
        0,
        "defensive copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0003_materialize_returned_does_not_warn() {
    // A bound copy later consumed by `return x` escapes — the
    // `states == Moved` escape check must keep W0003 silent. (The
    // unbound `return str(text)` shape never reaches the case-B
    // analysis: return operands are not collected as materialize
    // sites.)
    let diags = check_src("fn first(text: strview) -> str:\n\tx: str = str(text)\n\treturn x\n");
    assert_eq!(
        w0003_count(&diags),
        0,
        "escaping copy must not warn; got: {diags:?}"
    );
}

#[test]
fn w0003_strview_param_root_does_not_warn() {
    // Conservative direction: the view's root owner is the caller's
    // buffer, so `projection_root` yields None for a `strview`
    // parameter and case B must stay silent — the pass cannot judge
    // mutations it cannot see. Unlike
    // `w0003_materialize_returned_does_not_warn`, the copy below only
    // borrow-escapes (`print(x)`), so the escape check does NOT fire
    // first: the unresolvable root is the only thing suppressing the
    // warning.
    let diags = check_src("fn f(text: strview):\n\tx: str = str(text)\n\tprint(x)\n");
    assert_eq!(
        w0003_count(&diags),
        0,
        "strview-parameter root must not warn; got: {diags:?}"
    );
}

#[test]
fn w0003_defensive_copy_before_inout_pass_does_not_warn() {
    // Defensive-copy exception, `inout`-pass hazard kind: the source
    // root is `inout`-passed AFTER the materialize point, so the
    // callee may mutate the buffer the view aliases — the owned copy
    // is a genuine snapshot, not a redundant allocation. (`owner_hazards`
    // records inout passes and mutations alike; the mutation kind is
    // pinned by `w0003_defensive_copy_before_source_mutation_does_not_warn`.)
    let diags = check_src(
        "fn eat(inout a: str):\n\tprint(a)\n\nfn main():\n\tmut s: str = \"hi\"\n\tx: str = str(s[0:1])\n\tprint(x)\n\teat(&s)\n",
    );
    assert_eq!(
        w0003_count(&diags),
        0,
        "defensive copy before an inout pass must not warn; got: {diags:?}"
    );
}
