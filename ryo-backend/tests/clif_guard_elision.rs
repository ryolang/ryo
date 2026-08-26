//! CLIF-level pinning tests for value-range guard elision (Phase 1).
//! Each test compiles a snippet with `compile_and_dump_ir` and asserts
//! on the presence/absence of `s*_overflow` opcodes in the emitted
//! CLIF. Assertions are made over the whole dump, so every test source
//! keeps `main` free of integer arithmetic to stay unambiguous.

use chumsky::Parser;
use chumsky::input::Input;
use ryo_backend::codegen::Codegen;
use ryo_core::ownership::OwnershipSidecar;
use ryo_core::tir::Tir;
use ryo_core::types::InternPool;
use ryo_frontend::lexer;
use ryo_frontend::parser::program_parser;

fn analyze(src: &str) -> (Vec<Tir>, InternPool, OwnershipSidecar) {
    let mut pool = InternPool::new();
    let mut sink = ryo_core::diag::DiagSink::new();
    let tokens = lexer::lex(src, &mut pool, &mut sink);
    assert!(!sink.has_errors(), "lex should succeed");
    let token_stream = tokens[..].split_token_span((0..src.len()).into());
    let mut ast = ryo_core::ast::Ast::new();
    program_parser()
        .parse_with_state(token_stream, &mut ast)
        .into_result()
        .expect("parse should succeed");
    let mut astgen_sink = ryo_core::diag::DiagSink::new();
    let uir = ryo_frontend::astgen::generate(&ast, &mut pool, &mut astgen_sink);
    let mut sema_sink = ryo_core::diag::DiagSink::new();
    let tirs = ryo_frontend::sema::analyze(
        &uir,
        &mut pool,
        &mut sema_sink,
        src,
        std::path::Path::new("test.ryo"),
    );
    assert!(!sema_sink.has_errors(), "sema should succeed");
    let mut ownership_sink = ryo_core::diag::DiagSink::new();
    let sidecar = ryo_frontend::ownership::check(&tirs, &pool, &mut ownership_sink);
    assert!(!ownership_sink.has_errors(), "ownership should succeed");
    (tirs, pool, sidecar)
}

fn clif_of(src: &str) -> String {
    let (tirs, pool, sidecar) = analyze(src);
    let mut codegen = Codegen::new_jit().expect("JIT codegen should initialize");
    codegen
        .compile_and_dump_ir(&tirs, &pool, &sidecar)
        .expect("codegen should succeed")
}

#[test]
fn fib_shape_elides_sub_guards() {
    // `if n <= 1: return n` proves n >= 2 on the fall-through path, so
    // both `n - 1` and `n - 2` are unoverflowable. The final `+` of two
    // call results has no bounds and keeps its guard.
    let clif = clif_of(
        "fn fib(n: int) -> int:\n\
         \tif n <= 1:\n\
         \t\treturn n\n\
         \treturn fib(n - 1) + fib(n - 2)\n\
         \n\
         fn main():\n\
         \tassert(fib(10) == 55, \"fib check\")\n",
    );
    assert!(
        !clif.contains("ssub_overflow"),
        "both n-1 and n-2 should be unguarded:\n{clif}"
    );
    assert!(clif.contains("isub"), "raw isub should be emitted:\n{clif}");
    assert_eq!(
        clif.matches("sadd_overflow").count(),
        1,
        "only the unbounded final add keeps its guard:\n{clif}"
    );
}

#[test]
fn unbounded_side_keeps_guard() {
    // x >= 0 bounds only the low side; x + 1 can still overflow at MAX.
    let clif = clif_of(
        "fn add_one(x: int) -> int:\n\
         \tif x >= 0:\n\
         \t\treturn x + 1\n\
         \treturn 0\n\
         \n\
         fn main():\n\
         \tassert(add_one(4) == 5, \"check\")\n",
    );
    assert!(clif.contains("sadd_overflow"), "guard must stay:\n{clif}");
}

#[test]
fn while_body_seeds_cond_fact() {
    // `i = i - 1` under `while i > 0` is exact (i >= 1). `total + i`
    // has no bound on total and keeps its guard.
    let clif = clif_of(
        "fn sum_down(n: int) -> int:\n\
         \tmut total = 0\n\
         \tmut i = n\n\
         \twhile i > 0:\n\
         \t\ttotal = total + i\n\
         \t\ti = i - 1\n\
         \treturn total\n\
         \n\
         fn main():\n\
         \tassert(sum_down(10) == 55, \"sum\")\n",
    );
    assert!(
        !clif.contains("ssub_overflow"),
        "i - 1 should be unguarded:\n{clif}"
    );
    assert!(
        clif.contains("sadd_overflow"),
        "total + i must stay checked:\n{clif}"
    );
}

#[test]
fn assignment_in_arm_kills_fact_at_join() {
    // m's fact [1, MAX] from `if m > 0` must not survive the inner if:
    // one arm reassigns m, so `m - 1` at the join is unprovable.
    let clif = clif_of(
        "fn f(n: int) -> int:\n\
         \tmut m = n\n\
         \tif m > 0:\n\
         \t\tif m < 100:\n\
         \t\t\tm = n\n\
         \t\ty = m - 1\n\
         \treturn m\n\
         \n\
         fn main():\n\
         \tassert(f(4) == 4, \"check\")\n",
    );
    assert!(
        clif.contains("ssub_overflow"),
        "stale fact must not elide m - 1:\n{clif}"
    );
}

#[test]
fn inout_call_kills_fact() {
    // After `bump(&m)` the callee may have written anything; the
    // pre-call fact must not elide `m - 1`.
    let clif = clif_of(
        "fn bump(inout x: int):\n\
         \tx = x + 1\n\
         \n\
         fn f(n: int) -> int:\n\
         \tmut m = n\n\
         \tif m > 0:\n\
         \t\tbump(&m)\n\
         \t\ty = m - 1\n\
         \treturn m\n\
         \n\
         fn main():\n\
         \tassert(f(4) == 4, \"check\")\n",
    );
    assert!(
        clif.contains("ssub_overflow"),
        "inout call must invalidate the fact:\n{clif}"
    );
}

#[test]
fn neg_elided_only_when_min_excluded() {
    // `if x > 0: return -x` — x >= 1 excludes i64::MIN, so no guard.
    // `if y <= 0: return -y` — y can BE i64::MIN, guard stays.
    let clif = clif_of(
        "fn neg_pos(x: int) -> int:\n\
         \tif x > 0:\n\
         \t\treturn -x\n\
         \treturn x\n\
         \n\
         fn neg_nonpos(y: int) -> int:\n\
         \tif y <= 0:\n\
         \t\treturn -y\n\
         \treturn y\n\
         \n\
         fn main():\n\
         \tassert(neg_pos(5) == 0 - 5, \"pos\")\n\
         \tassert(neg_nonpos(3) == 3, \"nonpos\")\n",
    );
    assert_eq!(
        clif.matches("ssub_overflow").count(),
        1,
        "exactly the MIN-reachable negation keeps its guard:\n{clif}"
    );
}

#[test]
fn elif_arms_do_not_inherit_sibling_true_seeds() {
    // Regression for a Task-2 review finding: the then arm's true-polarity
    // seed must not leak into elif cond/body blocks. `n <= 1`'s true seed
    // ([MIN, 1]) polluting the `n >= 10` body would intersect to an empty
    // range, and empty ranges still elide — so `n + 1` at i64::MAX would
    // silently lose its trap. Each block re-baselines from outer_facts.
    let clif = clif_of(
        "fn f(n: int) -> int:\n\
         \tif n <= 1:\n\
         \t\treturn n\n\
         \telif n >= 10:\n\
         \t\treturn n + 1\n\
         \treturn 0\n\
         \n\
         fn main():\n\
         \tassert(f(20) == 21, \"check\")\n",
    );
    assert!(
        clif.contains("sadd_overflow"),
        "n + 1 in the elif body must stay checked (n up to i64::MAX):\n{clif}"
    );
}

#[test]
fn for_range_var_fact_does_not_leak_to_shadowed_binding() {
    // Regression for a Task-2 review finding: an if inside the loop body
    // can leave a fact on the loop variable's name (fall-through re-seed);
    // after the loop the name refers to the OUTER binding again, which is
    // a different quantity. `i + 1` on the param must stay checked.
    let clif = clif_of(
        "fn f(i: int) -> int:\n\
         \tfor i in range(0, 3):\n\
         \t\tif i > 1:\n\
         \t\t\tbreak\n\
         \treturn i + 1\n\
         \n\
         fn main():\n\
         \tassert(f(7) == 8, \"check\")\n",
    );
    assert!(
        clif.contains("sadd_overflow"),
        "the outer binding's i + 1 must stay checked:\n{clif}"
    );
}
