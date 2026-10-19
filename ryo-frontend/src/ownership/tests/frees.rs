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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
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
            .drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == decl && fp.target == lit),
        "expected dead-store Free anchored at decl with target=lit; got: {:?}",
        sidecar.drops
    );

    // Exactly one Free for `lit` — guards against Task 3/4 ever
    // double-counting (anonymous-temp pass + dead-store pass both
    // emitting for the same owner).
    assert_eq!(
        sidecar.drops.iter().filter(|fp| fp.target == lit).count(),
        1,
        "expected exactly one Free for lit"
    );
}

#[test]
fn match_payload_last_use_across_if_paths_frees_at_if_join() {
    let src = "enum Message:\n\tText(str)\n\tEmpty\n\nfn main():\n\tmessage = Message.Text(\"first-\" + \"abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz\")\n\tmatch message:\n\t\tMessage.Text(payload):\n\t\t\tif payload.len() > 5:\n\t\t\t\tprint(payload)\n\t\t\telse:\n\t\t\t\tprint(payload)\n\t\tMessage.Empty:\n\t\t\tprint(\"empty\")\n";
    let (diags, mut sidecar, tirs, pool) = check_src_full(src);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != ryo_core::diag::Severity::Error),
        "source must pass sema and ownership: {diags:?}"
    );

    let tir = &tirs[0];
    let match_inst = (1..tir.instructions.len())
        .map(|i| TirRef::from_raw(i as u32))
        .find(|&r| tir.inst(r).tag == TirTag::Match)
        .expect("match instruction");
    let payload_token = tir.match_by_move_owner_refs(match_inst, &pool)[0];
    let if_inst = (1..tir.instructions.len())
        .map(|i| TirRef::from_raw(i as u32))
        .find(|&r| tir.inst(r).tag == TirTag::IfStmt)
        .expect("nested if instruction");
    let function = take_function_sidecar(&mut sidecar, 0);
    let frees: Vec<_> = function
        .drops
        .iter()
        .filter(|drop| drop.target == payload_token)
        .collect();

    assert_eq!(frees.len(), 1, "one payload Free: {frees:?}");
    assert_eq!(
        frees[0].position.anchor_inst(),
        Some(if_inst),
        "the payload is live across both if paths, so free it at their join"
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
    let mut sidecar_map = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar_map, 0);

    // No scheduled Free should target __ryo_panic's StrConst arg —
    // codegen's borrowed-scalar ABI never frees it.
    assert!(
        sidecar.drops.iter().all(|fp| fp.target != str_arg),
        "expected no scheduled Free for __ryo_panic's StrConst arg, got: {:?}",
        sidecar.drops
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(sink.is_empty(), "expected no diagnostics");
    assert_eq!(sidecar.drops.len(), 1);
    assert_eq!(sidecar.drops[0].target, lit);
    assert_eq!(sidecar.drops[0].position.anchor_inst().unwrap(), var_read);
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(sink.is_empty(), "expected no diagnostics");

    // Reassign frees l1 (old owner) keyed on the Assign inst.
    assert_eq!(
        sidecar.free_on_reassign[assign.index()],
        Some(l1),
        "expected free_on_reassign[assign] = l1"
    );

    // Last-use frees l2 (new owner reaches function exit via print(s)).
    assert!(
        sidecar
            .drops
            .iter()
            .any(|fp| fp.target == l2 && fp.position.anchor_inst().unwrap() == var_read),
        "expected last-use Free for l2; got: {:?}",
        sidecar.drops
    );

    // No dead-store Free for l1 — it's covered by free_on_reassign.
    assert!(
        !sidecar.drops.iter().any(|fp| fp.target == l1),
        "l1 must not be in the drops schedule (it's in free_on_reassign): {:?}",
        sidecar.drops
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(sink.is_empty());

    // Three Frees: la, lb, cat. Anchored after consumers (la/lb on
    // cat, cat on call). Order-independent.
    let targets: HashSet<TirRef> = sidecar.drops.iter().map(|fp| fp.target).collect();
    assert!(targets.contains(&la), "expected la in the drops schedule");
    assert!(targets.contains(&lb), "expected lb in the drops schedule");
    assert!(targets.contains(&cat), "expected cat in the drops schedule");
    assert_eq!(sidecar.drops.len(), 3);
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sidecar = take_function_sidecar(&mut sidecar, 0);
    assert!(sink.is_empty(), "expected no diagnostics");

    // The Free for "Alice" must come from free_on_reassign[assign],
    // NOT from last-use scheduling. Last-use should target "Bob"
    // (anchored after read2), not "Alice".
    assert_eq!(
        sidecar.free_on_reassign[assign.index()],
        Some(alice_lit),
        "expected free_on_reassign[assign] = alice_lit"
    );
    // the drops schedule must not contain a DropOp with target=alice_lit
    // anchored at read1 (the bug's signature was wrong-target via post-rebind current_owner).
    assert!(
        !sidecar
            .drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == read1 && fp.target == alice_lit),
        "expected no last-use Free for Alice anchored at read1 (Alice freed via free_on_reassign): {:?}",
        sidecar.drops
    );
    // Last-use Free for Bob must exist anchored at read2.
    assert!(
        sidecar
            .drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == read2 && fp.target == bob_lit),
        "expected last-use Free for Bob anchored at read2; got: {:?}",
        sidecar.drops
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);

    let virtual_ref = TirRef::param(0);
    let fp = sc
        .drops
        .iter()
        .find(|fp| fp.target == virtual_ref)
        .expect("free scheduled");
    assert_eq!(fp.position.anchor_inst().unwrap(), ret);
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);

    let frees: Vec<_> = sc
        .drops
        .iter()
        .filter(|fp| fp.target == TirRef::param(0))
        .collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the owned param; schedule = {:?}",
        sc.drops
    );
    assert_eq!(
        frees[0].position.anchor_inst().unwrap(),
        s_read,
        "the Free must anchor after the param's last read; schedule = {:?}",
        sc.drops
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);

    let frees: Vec<_> = sc
        .drops
        .iter()
        .filter(|fp| fp.target == TirRef::param(0))
        .collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the owned param; schedule = {:?}",
        sc.drops
    );
    assert_eq!(
        frees[0].position.anchor_inst().unwrap(),
        while_stmt,
        "a param last read inside a loop must be freed after the loop, not inside it; schedule = {:?}",
        sc.drops
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);

    let frees: Vec<_> = sc
        .drops
        .iter()
        .filter(|fp| fp.target == TirRef::param(0))
        .collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the owned param; schedule = {:?}",
        sc.drops
    );
    assert_eq!(
        frees[0].position.anchor_inst().unwrap(),
        call,
        "a never-read param keeps the last-body-statement anchor; schedule = {:?}",
        sc.drops
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);

    let virtual_ref = TirRef::param(0);
    // We expect an arm-conditional free for the parameter in the else branch!
    let fp = sc
        .drops
        .iter()
        .find(|fp| fp.target == virtual_ref)
        .expect("arm-conditional free scheduled");
    assert_eq!(fp.position.anchor_inst().unwrap(), call_else);
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);

    // temp_a (lit_a) is released via `free_on_reassign` (codegen lowers
    // its destructor at the Assign), so it must NOT also appear in
    // `free_schedule` — that would be a double-free. The anon-temp pass
    // skips it because it is a named init (the VarDecl initializer);
    // if that filter were missing the anon-temp pass would schedule a
    // second Free here. See the sibling invariant in
    // `reassignment_records_free_on_old_owner`.
    let a_frees = sc.drops.iter().filter(|fp| fp.target == lit_a).count();
    assert_eq!(
        a_frees, 0,
        "temp_a must not be in free_schedule (it's in free_on_reassign); got {sc:?}"
    );
    // Positive half: temp_a must actually be recorded in
    // free_on_reassign (mirrors `reassignment_records_free_on_old_owner`).
    // Without this the test would pass on a leak (temp_a never freed).
    assert_eq!(
        sc.free_on_reassign[assign.index()],
        Some(lit_a),
        "temp_a must be freed once via free_on_reassign (not leaked); got {sc:?}",
    );
    // lit_b is never read, so it is a dead store and is freed exactly
    // once via the dead-store pass (anchored at the Assign).
    let b_frees = sc.drops.iter().filter(|fp| fp.target == lit_b).count();
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
    let mut sc = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sc, 0);
    let lit_frees = sc.drops.iter().filter(|fp| fp.target == lit).count();
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
    let mut sc = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sc, 0);
    // body_lit must be scheduled exactly once (dead-store pass owns
    // it; anon-temp skips it as a named init). The dynamic
    // current_owner.values() classifier would schedule it twice
    // (anon-temp + dead-store) -> the double-free this test guards.
    let body_lit_frees = sc.drops.iter().filter(|fp| fp.target == body_lit).count();
    assert_eq!(
        body_lit_frees, 1,
        "loop-rebound temp must be freed exactly once (static classifier skips it in anon-temp); got {sc:?}"
    );
    // lit (the decl init) is freed via free_on_reassign at the
    // loop-body Assign, so it must NOT also appear in free_schedule.
    let lit_frees = sc.drops.iter().filter(|fp| fp.target == lit).count();
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
    // Free from the binding's current FatLocals (path-correct buffer).
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let diags = sink.into_diags();
    assert!(
        !diags.iter().any(|d| matches!(d.code, DiagCode::DeadStore)),
        "s is read after the if — no dead-store warning; got {diags:?}"
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.free_on_reassign[asg.index()].is_some(),
        "the taken arm must drop the pre-reassign buffer; free_on_reassign = {:?}",
        sc.free_on_reassign
    );
    assert_eq!(
        sc.drops.iter().filter(|fp| fp.target == lit_a).count(),
        1,
        "the binding's owner must still get its last-use Free (covers the not-taken path); schedule = {:?}",
        sc.drops
    );
    assert!(
        !sc.drops.iter().any(|fp| fp.target == lit_b),
        "lit_b's buffer is freed through the binding's current FatLocals (same buffer as lit_a's Free on the taken path) — a second Free would double-free; schedule = {:?}",
        sc.drops
    );
}
