use super::super::*;
use super::common::*;

#[test]
fn conditional_dead_reassign_schedules_fallthrough_drop() {
    // `mut s = "a"; if c: s = "b"` with s never read after.
    // The taken arm drops "a" (free_on_reassign) and the drain frees
    // "b"; the NOT-taken path must also free "a" — via an arm-gated
    // ConditionalDeadDrop for the pre-branch owner.
    use ryo_core::span::Span;
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = Span::EMPTY;
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit_b, span);
    let if_s = tb.if_stmt(cond, &[asg], &[], None, void, span);
    let tir = tb.finish(&[decl, if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    let drops: Vec<_> = block_start_drops(&sc)
        .into_iter()
        .filter(|d| d.target == lit_a)
        .collect();
    assert_eq!(
        drops.len(),
        1,
        "expected one block-start dead drop for the pre-branch owner; got {:?}",
        sc.drops
    );
    assert!(
        matches!(
            drops[0].position,
            ryo_core::flow::DropPosition::BlockStart { .. }
        ),
        "the drop must fire at an untouched arm (the fall-through)"
    );
    let _ = if_s;
}

#[test]
fn conditional_reassign_all_arms_reseated_no_drop() {
    // `if c: s = "b" else: s = "d"` — every arm reseats, so the
    // pre-branch buffer is dropped by free_on_reassign on every
    // path; no ConditionalDeadDrop may be scheduled.
    use ryo_core::span::Span;
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = Span::EMPTY;
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        block_start_drops(&sc).is_empty(),
        "all arms reseated — no untouched path, no dead drop; got {:?}",
        sc.drops
    );
}

#[test]
fn loop_dead_reassign_anchors_after_loop() {
    // `mut s = "a"; while c: s = "b"` with s never read
    // after. The dead-store Free must anchor AFTER THE LOOP (not
    // after the in-loop assign): the in-loop anchor never fires on
    // the zero-iteration path, leaking the pre-loop buffer. The
    // after-loop anchor emits the binding's current FatLocals —
    // final value on taken paths, pre-loop value on zero iterations.
    use ryo_core::span::Span;
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = Span::EMPTY;
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let asg = tb.assign(s, str_ty, lit_b, span);
    let wl = tb.while_loop(cond, &[asg], void, span);
    let tir = tb.finish(&[decl, wl]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == wl && fp.target == lit_b),
        "expected a Free anchored after the loop for the binding's value; schedule = {:?}",
        sc.drops
    );
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == asg && fp.target == lit_b),
        "the in-loop dead-store Free must move to the loop anchor (it never fires on zero iterations); schedule = {:?}",
        sc.drops
    );
}

#[test]
fn loop_dead_reassign_with_return_keeps_in_body_free() {
    // When the loop body can `return`, the after-loop anchor
    // is unreachable on the return path — the in-body Free must stay
    // alongside the after-loop one.
    use ryo_core::span::Span;
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = Span::EMPTY;
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == wl && fp.target == lit_b),
        "expected the after-loop Free; schedule = {:?}",
        sc.drops
    );
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == asg && fp.target == lit_b),
        "a returning body keeps the in-body Free (return path); schedule = {:?}",
        sc.drops
    );
}

#[test]
fn loop_local_dead_value_keeps_in_body_anchor() {
    // Guard: a value DECLARED inside the loop body is not a
    // pre-loop binding — its Free must stay anchored in the body
    // (the binding's FatLocals don't exist on the zero-iteration
    // path).
    use ryo_core::span::Span;
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let t = pool.intern_str("t");
    let span = Span::EMPTY;
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let lit_x = tb.str_const(pool.intern_str("x"), str_ty, span);
    let decl = tb.var_decl(t, false, str_ty, lit_x, span);
    let wl = tb.while_loop(cond, &[decl], void, span);
    let tir = tb.finish(&[wl]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == decl && fp.target == lit_x),
        "loop-local dead value keeps its in-body Free; schedule = {:?}",
        sc.drops
    );
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == wl),
        "loop-local value must NOT be re-anchored after the loop; schedule = {:?}",
        sc.drops
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
    use ryo_core::span::Span;
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let i = pool.intern_str("i");
    let print = pool.intern_str("print");
    let span = Span::EMPTY;
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == fr && fp.target == lit_a),
        "the last-use Free must anchor after the loop (in-body anchoring frees s between iterations — UAF); schedule = {:?}",
        sc.drops
    );
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == sv && fp.target == lit_a),
        "no Free may anchor after the in-loop read itself; schedule = {:?}",
        sc.drops
    );
}

#[test]
fn last_use_inside_if_anchors_after_if() {
    // Same family through an if: `mut s = "a"; if d: print(s)` —
    // anchoring after the in-arm read leaks `s` on the not-taken
    // path; the merge point is where the value is dead on all paths.
    use ryo_core::span::Span;
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let print = pool.intern_str("print");
    let span = Span::EMPTY;
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let sv = tb.var(s, str_ty, span);
    let pr = tb.call(print, &[sv], &[ParamMode::Borrow], void, span);
    let if_s = tb.if_stmt(cond, &[pr], &[], None, void, span);
    let tir = tb.finish(&[decl, if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == if_s && fp.target == lit_a),
        "the last-use Free must anchor after the if (in-arm anchoring leaks on the not-taken path); schedule = {:?}",
        sc.drops
    );
}

#[test]
fn return_epilogue_frees_live_local() {
    // Return-epilogue: `mut s = "a"; if d: print(s) else: return`.
    // On the else path, `s` is still live at the return and nothing
    // freed it — the last-use Free anchors in the sibling then-arm,
    // which the else path never reaches. An early return must
    // destroy the function's still-owned locals on ITS path.
    use ryo_core::span::Span;
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let print = pool.intern_str("print");
    let span = Span::EMPTY;
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == ret && fp.target == lit_a),
        "the still-live local must be freed on the early-return path; schedule = {:?}",
        sc.drops
    );
}

#[test]
fn return_epilogue_skips_consumed_return_value() {
    // `fn f() -> str: s = "a"; return s` — the returned value moved
    // out; an epilogue Free for it would be a use-after-free in the
    // caller.
    use ryo_core::span::Span;
    use ryo_core::tir::{TirBuilder, TirData, TirTag};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let f = pool.intern_str("f");
    let s = pool.intern_str("s");
    let span = Span::EMPTY;
    let mut tb = TirBuilder::new(f, vec![], str_ty, span);
    let lit = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit, span);
    let sv = tb.var(s, str_ty, span);
    let ret = tb.push_typed(TirTag::Return, TirData::UnOp(sv), str_ty, span);
    let tir = tb.finish(&[decl, ret]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.target == lit && fp.position.anchor_inst().unwrap() == ret),
        "the returned value moved out — no epilogue Free for it; schedule = {:?}",
        sc.drops
    );
}

#[test]
fn return_epilogue_skips_dead_store_owned() {
    // `mut s = "a"; if d: return` with s never read: the dead-store
    // drain already frees `s` right after its declaration (covering
    // every path), so no epilogue Free may be added for it.
    use ryo_core::span::Span;
    use ryo_core::tir::TirBuilder;
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let span = Span::EMPTY;
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl = tb.var_decl(s, false, str_ty, lit_a, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let ret = tb.return_void(void, span);
    let if_s = tb.if_stmt(cond, &[ret], &[], None, void, span);
    let tir = tb.finish(&[decl, if_s]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.target == lit_a && fp.position.anchor_inst().unwrap() == ret),
        "dead-store-owned value is already freed at its decl — no epilogue Free; schedule = {:?}",
        sc.drops
    );
}

#[test]
fn return_epilogue_covers_move_param() {
    // `fn f(move s: str): if d: return` — the owned param is still
    // Valid at the early return; the callee must destroy it there
    // (the never-read param's Free anchors after the last body
    // stmt, which the early-return path never reaches).
    use ryo_core::span::Span;
    use ryo_core::tir::{ParamMode, TirBuilder, TirParam};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let f = pool.intern_str("f");
    let s = pool.intern_str("s");
    let span = Span::EMPTY;
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == ret && fp.target == TirRef::param(0)),
        "an owned param must be destroyed on the early-return path; schedule = {:?}",
        sc.drops
    );
}

#[test]
fn return_epilogue_ignores_loop_local_declared_after_return() {
    // An early return lexically BEFORE a loop-local's declaration must
    // not receive an epilogue Free for that local: loop fixed-point
    // seeding leaves the owner `Valid` at the return even though its
    // producer is unreachable from it, and the epilogue covered-dedup
    // cannot see the real last-use Free (anchored later, inside the
    // loop body). Scheduling that Free made codegen abort with "no
    // ValueRepr cached" (a loop-local declared after an early-returning
    // guard).
    let src = "fn adv(inout p: int):\n\tp += 1\n\nfn f(s: str) -> str:\n\tmut p = 0\n\twhile true:\n\t\tif p >= 10:\n\t\t\treturn \"<none>\"\n\t\tk = str(s[0:2])\n\t\tadv(&p)\n\t\tif k == s:\n\t\t\treturn k\n\treturn \"<none>\"\n";
    let (diags, sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error),
        "no errors expected; got: {diags:?}"
    );
    let f_idx = tirs
        .iter()
        .position(|t| {
            t.body_stmts()
                .iter()
                .any(|s| t.inst(s).tag == ryo_core::tir::TirTag::WhileLoop)
        })
        .expect("function with a while loop");
    let tir = &tirs[f_idx];
    let loop_stmt = tir
        .body_stmts()
        .iter()
        .find(|&s| tir.inst(s).tag == ryo_core::tir::TirTag::WhileLoop)
        .expect("while loop stmt");
    let loop_body = tir.while_loop_view(loop_stmt).body;
    // The bounds-guard if's then-arm holds the early return.
    let early_return = loop_body
        .iter()
        .find_map(|s| {
            if tir.inst(s).tag != ryo_core::tir::TirTag::IfStmt {
                return None;
            }
            let v = tir.if_stmt_view(s);
            v.then_stmts
                .get(0)
                .filter(|&r| tir.inst(r).tag == ryo_core::tir::TirTag::Return)
        })
        .expect("guard if with early return");
    let k_init = loop_body
        .iter()
        .find(|&s| tir.inst(s).tag == ryo_core::tir::TirTag::VarDecl)
        .map(|s| tir.var_decl_view(s).initializer)
        .expect("var_decl for k");
    let schedule = &sidecar.functions[f_idx].drops;
    assert!(
        !schedule
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == early_return && fp.target == k_init),
        "no epilogue Free for a loop-local declared after the return; schedule = {schedule:?}"
    );
    assert!(
        schedule.iter().any(|fp| fp.target == k_init),
        "k's real last-use Free must still exist; schedule = {schedule:?}"
    );
}

#[test]
fn temp_last_use_inside_loop_not_reanchored() {
    // Guard: an anonymous temp consumed inside the loop body must
    // keep its per-iteration Free (each iteration allocates a fresh
    // value) — the re-anchor applies only to named pre-branch
    // bindings.
    use ryo_core::span::Span;
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int_ty = pool.int();
    let void = pool.void();
    let main = pool.intern_str("main");
    let i = pool.intern_str("i");
    let int_to_str = pool.intern_str("int_to_str");
    let print = pool.intern_str("print");
    let span = Span::EMPTY;
    let mut tb = TirBuilder::new(main, vec![], void, span);
    let zero = tb.int_const(0, int_ty, span);
    let three = tb.int_const(3, int_ty, span);
    let iv = tb.var(i, int_ty, span);
    let call = tb.call(int_to_str, &[iv], &[ParamMode::Borrow], str_ty, span);
    let pr = tb.call(print, &[call], &[ParamMode::Borrow], void, span);
    let fr = tb.for_range(i, zero, three, &[pr], void, span);
    let tir = tb.finish(&[fr]);
    let mut sink = DiagSink::new();
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    assert!(
        !sc.drops
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == fr),
        "a loop-local temp must keep its per-iteration Free, not move to the loop anchor; schedule = {:?}",
        sc.drops
    );
    assert!(
        sc.drops
            .iter()
            .any(|fp| fp.target == call && fp.position.anchor_inst().unwrap() == pr),
        "the temp's Free stays anchored after its consumer; schedule = {:?}",
        sc.drops
    );
}

#[test]
fn conditional_dead_reassign_gated_on_real_else() {
    // `if c: s = "b" else: <no reassign>` with s unread after —
    // the drop must name the REAL else arm's Flow IR block.
    use ryo_core::span::Span;
    use ryo_core::tir::{ParamMode, TirBuilder};
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let bool_ty = pool.bool_();
    let void = pool.void();
    let main = pool.intern_str("main");
    let s = pool.intern_str("s");
    let print = pool.intern_str("print");
    let span = Span::EMPTY;
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
    let mut sidecar = check(
        std::slice::from_ref(&tir),
        &[ryo_core::flow::Flow::build(&tir, &pool)],
        &pool,
        &mut sink,
    );
    let sc = take_function_sidecar(&mut sidecar, 0);
    let flow = build_flow(&tir, &pool);
    let arm_blocks = flow.branch_arm_blocks(if_s);
    let drops: Vec<_> = block_start_drops(&sc)
        .into_iter()
        .filter(|d| d.target == lit_a)
        .collect();
    assert_eq!(
        drops.len(),
        1,
        "expected one block-start dead drop; got {:?}",
        sc.drops
    );
    assert!(
        matches!(
            drops[0].position,
            ryo_core::flow::DropPosition::BlockStart { block } if block == arm_blocks.else_block
        ),
        "the drop must name the else arm's block {:?}; got {:?}",
        arm_blocks.else_block,
        drops[0].position
    );
}

#[test]
fn last_use_in_loop_condition_comparison_anchors_after_loop() {
    // A heap str scanned by a slice comparison in a loop-body `if`
    // condition: the owner's last use sits in a CONDITION, which still
    // counts as inside the loop — the Free must anchor after the whole
    // `while`, not after the in-loop read (which fires mid-loop).
    let src = "fn main():\n\tmut s: str = \"\"\n\tfor i in range(0, 8):\n\t\tstr_push(&s, \"fox \")\n\tmut i = 0\n\tmut count = 0\n\twhile i + 3 <= s.len():\n\t\tif s[i:i+3] == \"fox\":\n\t\t\tcount += 1\n\t\ti += 1\n\tassert(count == 8, \"count\")\n";
    let (diags, sidecar, tirs, _pool) = check_src_full(src);
    assert_eq!(diags.len(), 1, "got {diags:?}");
    assert_eq!(diags[0].code, DiagCode::DeadStore);
    assert_eq!(diags[0].message, "value `i` is declared but never used");
    let tir = &tirs[0];
    let body = tir.body_stmts();
    let s_init = body
        .iter()
        .find(|&s| tir.inst(s).tag == ryo_core::tir::TirTag::VarDecl)
        .map(|s| tir.var_decl_view(s).initializer)
        .expect("var_decl for s");
    let while_stmt = body
        .iter()
        .find(|&s| tir.inst(s).tag == ryo_core::tir::TirTag::WhileLoop)
        .expect("while loop stmt");
    let frees_for_s: Vec<_> = sidecar.functions[0]
        .drops
        .iter()
        .filter(|fp| fp.target == s_init)
        .collect();
    assert_eq!(
        frees_for_s.len(),
        1,
        "exactly one Free for s's owner; got: {:?}",
        sidecar.functions[0].drops
    );
    assert_eq!(
        frees_for_s[0].position.anchor_inst().unwrap(),
        while_stmt,
        "Free must anchor after the while loop, not inside it"
    );
}

#[test]
fn last_use_in_inline_assert_counts_as_use() {
    // `assert(s.len() == ...)` desugars to ExprStmt(IfStmt); the read
    // inside the desugared condition must be walked: no W0001 for s, and
    // the owner's single Free anchors after the desugared if — never a
    // dead-store Free after the build loop.
    let src = "fn main():\n\tmut s: str = \"\"\n\tfor i in range(0, 25):\n\t\ts = s + \"fox \"\n\tassert(s.len() == 100, \"len\")\n\tprint(\"ok\\n\")\n";
    let (diags, sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.code == DiagCode::DeadStore && d.message.contains("`s`")),
        "assert-condition read must count as a use; got: {diags:?}"
    );
    let tir = &tirs[0];
    let body = tir.body_stmts();
    let s_init = body
        .iter()
        .find(|&s| tir.inst(s).tag == ryo_core::tir::TirTag::VarDecl)
        .map(|s| tir.var_decl_view(s).initializer)
        .expect("var_decl for s");
    // The desugared assert: an ExprStmt whose operand is an IfStmt.
    let assert_if = body
        .iter()
        .find_map(|s| {
            if tir.inst(s).tag != ryo_core::tir::TirTag::ExprStmt {
                return None;
            }
            match tir.inst(s).data {
                ryo_core::tir::TirData::UnOp(o)
                    if tir.inst(o).tag == ryo_core::tir::TirTag::IfStmt =>
                {
                    Some(o)
                }
                _ => None,
            }
        })
        .expect("desugared assert if");
    // The owner's Free is redundant with the same-binding Free
    // also parked at the desugared if — a cached-fallback Free of the
    // (possibly already displaced) buffer could double-free; the
    // sibling's redirect frees the slot's path-correct content.
    let schedule = &sidecar.functions[0].drops;
    assert!(
        schedule.iter().all(|fp| fp.target != s_init),
        "s's owner must be covered by the sibling same-binding Free; got: {schedule:?}"
    );
    assert!(
        schedule
            .iter()
            .any(|fp| fp.position.anchor_inst().unwrap() == assert_if),
        "a same-binding Free must anchor after the desugared assert if; got: {schedule:?}"
    );
}

#[test]
fn cond_temp_free_anchors_after_if() {
    // A heap temp produced in an if's main condition exists on every
    // path through the branch — its Free must anchor after the if (so
    // codegen emits it in the merge block), not after the consumer
    // inside the condition, where the end-of-statement sweep fires it
    // inside the taken arm only and leaks on every not-taken path.
    let src = "fn main():\n\tmut p: str = \"f\"\n\tp = p + \"o\"\n\tif p + \"x\" == \"fox\":\n\t\tprint(\"y\\n\")\n\telse:\n\t\tprint(\"n\\n\")\n";
    let (diags, sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.severity == ryo_core::diag::Severity::Error)
    );
    let tir = &tirs[0];
    let if_stmt = tir
        .body_stmts()
        .iter()
        .find(|&s| tir.inst(s).tag == ryo_core::tir::TirTag::IfStmt)
        .expect("if stmt");
    // The anonymous `p + "x"` concat is the comparison's left operand.
    let cond = tir.if_stmt_view(if_stmt).cond;
    let concat = match tir.inst(cond).data {
        ryo_core::tir::TirData::BinOp { lhs, .. } => lhs,
        other => panic!("expected str_eq BinOp cond, got {:?}", other),
    };
    assert_eq!(tir.inst(concat).tag, ryo_core::tir::TirTag::StrConcat);
    let frees: Vec<_> = sidecar.functions[0]
        .drops
        .iter()
        .filter(|fp| fp.target == concat)
        .collect();
    assert_eq!(
        frees.len(),
        1,
        "exactly one Free for the cond temp; got: {:?}",
        sidecar.functions[0].drops
    );
    assert_eq!(
        frees[0].position.anchor_inst().unwrap(),
        if_stmt,
        "cond temp Free must anchor after the if statement"
    );
}

#[test]
fn compound_assign_rhs_method_call_counts_as_use() {
    // `total += t.len()` reads `t` through the method-call receiver —
    // the CompoundAssign walk must visit its RHS so the read clears
    // t's dead-store entry: no W0001, and t is still freed at last use.
    let src = "fn main():\n\tmut total = 0\n\tfor i in range(0, 10):\n\t\tt = int_to_str(i) + \"!\"\n\t\ttotal += t.len()\n\tprint(\"ok\\n\")\n";
    let (diags, sidecar, tirs, _pool) = check_src_full(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.code == DiagCode::DeadStore && d.message.contains("`t`")),
        "method-call receiver read must count as a use; got: {diags:?}"
    );
    let tir = &tirs[0];
    let body = tir.body_stmts();
    let loop_stmt = body
        .iter()
        .find(|&s| tir.inst(s).tag == ryo_core::tir::TirTag::ForRange)
        .expect("for-range stmt");
    let loop_body = tir.for_range_view(loop_stmt).body;
    let t_init = loop_body
        .iter()
        .find(|&s| tir.inst(s).tag == ryo_core::tir::TirTag::VarDecl)
        .map(|s| tir.var_decl_view(s).initializer)
        .expect("var_decl for t");
    let frees_for_t: Vec<_> = sidecar.functions[0]
        .drops
        .iter()
        .filter(|fp| fp.target == t_init)
        .collect();
    assert_eq!(
        frees_for_t.len(),
        1,
        "exactly one Free for t's owner; got: {:?}",
        sidecar.functions[0].drops
    );
}
