use super::super::*;
use super::common::*;
use ryo_core::declaration::DeclId;

// ---------- M8.4: projection tracking (final spec §3.2/§3.3) ----------

#[test]
fn free_schedule_is_deterministic() {
    // Build a TIR with several owners + views; run `check` twice;
    // assert identical free_schedule.
    use ryo_core::span::Span;
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
    let span = Span::EMPTY;

    let mut tb = TirBuilder::new(main, vec![], void, span);
    // s1 = "a"; s2 = "b"; v = s1[0:1]; print(v); print(s2);
    // s3 = "c" (dead store); if true: print(s1) else: print("x");
    // print("tmp")
    let lit_a = tb.str_const(pool.intern_str("a"), str_ty, span);
    let decl1 = test_var_decl(&mut tb, s1, false, str_ty, lit_a, span);
    let lit_b = tb.str_const(pool.intern_str("b"), str_ty, span);
    let decl2 = test_var_decl(&mut tb, s2, false, str_ty, lit_b, span);
    let base = test_var(&mut tb, s1, str_ty, span);
    let i0 = tb.int_const(0, int_ty, span);
    let i1 = tb.int_const(1, int_ty, span);
    let sl = tb.slice(base, Some(i0), Some(i1), view_ty, span);
    let vdecl = test_var_decl(&mut tb, v, false, view_ty, sl, span);
    let vread = test_var(&mut tb, v, view_ty, span);
    let pv = tb.call(
        DeclId::NONE,
        print,
        &[vread],
        &all_borrow(&[vread]),
        void,
        span,
    );
    let pv_stmt = tb.unary(TirTag::ExprStmt, void, pv, span);
    let r2 = test_var(&mut tb, s2, str_ty, span);
    let ps2 = tb.call(DeclId::NONE, print, &[r2], &all_borrow(&[r2]), void, span);
    let ps2_stmt = tb.unary(TirTag::ExprStmt, void, ps2, span);
    let lit_c = tb.str_const(pool.intern_str("c"), str_ty, span);
    let decl3 = test_var_decl(&mut tb, s3, false, str_ty, lit_c, span);
    let cond = tb.bool_const(true, bool_ty, span);
    let r1 = test_var(&mut tb, s1, str_ty, span);
    let ps1 = tb.call(DeclId::NONE, print, &[r1], &all_borrow(&[r1]), void, span);
    let ps1_stmt = tb.unary(TirTag::ExprStmt, void, ps1, span);
    let xlit = tb.str_const(pool.intern_str("x"), str_ty, span);
    let px = tb.call(
        DeclId::NONE,
        print,
        &[xlit],
        &all_borrow(&[xlit]),
        void,
        span,
    );
    let px_stmt = tb.unary(TirTag::ExprStmt, void, px, span);
    let ifs = tb.if_stmt(cond, &[ps1_stmt], &[], Some(&[px_stmt]), void, span);
    let tlit = tb.str_const(pool.intern_str("tmp"), str_ty, span);
    let pt = tb.call(
        DeclId::NONE,
        print,
        &[tlit],
        &all_borrow(&[tlit]),
        void,
        span,
    );
    let pt_stmt = tb.unary(TirTag::ExprStmt, void, pt, span);
    let tir = tb.finish(&[decl1, decl2, vdecl, pv_stmt, ps2_stmt, decl3, ifs, pt_stmt]);

    let run = |tir: &Tir| {
        let mut sink = DiagSink::new();
        let mut sidecar = check(
            std::slice::from_ref(tir),
            &[ryo_core::flow::Flow::build(tir, &pool)],
            &pool,
            &mut sink,
        );
        let sc = take_function_sidecar(&mut sidecar, 0);
        sc.drops
            .iter()
            .map(|fp| (fp.position.anchor_inst().unwrap().raw(), fp.target.raw()))
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
