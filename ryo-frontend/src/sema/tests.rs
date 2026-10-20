use super::*;
use crate::astgen;
use crate::lexer::lex;
use crate::parser::{ParseState, program_parser};
use chumsky::Parser;
use chumsky::input::Input;
use ryo_core::tir::{Tir, TirData, TirTag};

mod warning_buffer;

pub(super) fn sp() -> Span {
    Span::EMPTY
}

pub(super) type RunOk = (Vec<Tir>, InternPool);

/// Lex + parse + astgen + sema. Returns either the typed TIR
/// (alongside the pool) or the diagnostics that stopped one of
/// those stages.
pub(super) fn run(input: &str) -> Result<RunOk, Vec<Diag>> {
    let mut pool = InternPool::new();
    let mut lex_sink = DiagSink::new();
    let tokens = lex(input, &mut pool, &mut lex_sink);
    assert!(
        !lex_sink.has_errors(),
        "lex errors: {:?}",
        lex_sink.into_diags()
    );
    let token_stream = tokens[..].split_token_span((0..input.len()).into());
    let mut state = ParseState::new(pool);
    program_parser()
        .parse_with_state(token_stream, &mut state)
        .into_result()
        .expect("parse ok");
    let (ast, mut pool) = state.into_parts();

    let mut sink = DiagSink::new();
    let uir = astgen::generate(&ast, &mut pool, &mut sink);
    if sink.has_errors() {
        return Err(sink.into_diags());
    }
    let tirs = analyze(&uir, &mut pool, &mut sink, input, Path::new("<test>"));
    if sink.has_errors() {
        return Err(sink.into_diags());
    }
    Ok((tirs, pool))
}

/// Variant that returns TIR even when sema reported errors —
/// used to assert the "Unreachable + diag" invariant from §4.5.
pub(super) fn run_with_errors(input: &str) -> (Vec<Tir>, Vec<Diag>, InternPool) {
    let mut pool = InternPool::new();
    let mut lex_sink = DiagSink::new();
    let tokens = lex(input, &mut pool, &mut lex_sink);
    assert!(
        !lex_sink.has_errors(),
        "lex errors: {:?}",
        lex_sink.into_diags()
    );
    let token_stream = tokens[..].split_token_span((0..input.len()).into());
    let mut state = ParseState::new(pool);
    program_parser()
        .parse_with_state(token_stream, &mut state)
        .into_result()
        .expect("parse ok");
    let (ast, mut pool) = state.into_parts();

    let mut sink = DiagSink::new();
    let uir = astgen::generate(&ast, &mut pool, &mut sink);
    let tirs = analyze(&uir, &mut pool, &mut sink, input, Path::new("<test>"));
    (tirs, sink.into_diags(), pool)
}

pub(super) fn first_msg(diags: &[Diag]) -> &str {
    &diags[0].message
}

pub(super) fn any_code(diags: &[Diag], code: DiagCode) -> bool {
    diags.iter().any(|d| d.code == code)
}

pub(super) fn count_code(diags: &[Diag], code: DiagCode) -> usize {
    diags.iter().filter(|d| d.code == code).count()
}

pub(super) fn tir_named<'a>(tirs: &'a [Tir], pool: &InternPool, name: &str) -> &'a Tir {
    let id = pool.find_str(name).expect("name should be interned");
    tirs.iter()
        .find(|t| t.name == id)
        .unwrap_or_else(|| panic!("no function named {:?}", name))
}

pub(super) fn stmt_at(tir: &Tir, i: usize) -> TirRef {
    tir.body_stmts().at(i)
}

#[test]
fn fills_types_on_flat_integer_var() {
    let (tirs, pool) = run("x = 42").unwrap();
    let main = tir_named(&tirs, &pool, "main");
    // Implicit-main is now void; the var-decl itself is still int.
    assert_eq!(main.return_type, pool.void());
    let var_decl = stmt_at(main, 0);
    assert_eq!(main.inst(var_decl).ty, pool.int());
}

#[test]
fn infers_string_literal_type() {
    let (tirs, pool) = run("x = \"hello\"").unwrap();
    let main = &tirs[0];
    let v = main.var_decl_view(stmt_at(main, 0));
    assert_eq!(main.inst(v.initializer).ty, pool.str_());
}

#[test]
fn typed_variable_annotation_honored() {
    let (tirs, pool) = run("x: int = 42").unwrap();
    let main = &tirs[0];
    assert_eq!(main.inst(stmt_at(main, 0)).ty, pool.int());
}

#[test]
fn bool_annotation_resolves() {
    let (tirs, pool) = run("x: bool = true").unwrap();
    let main = &tirs[0];
    assert_eq!(main.inst(stmt_at(main, 0)).ty, pool.bool_());
}

#[test]
fn variable_reference_type_resolved() {
    let (tirs, pool) = run("x = 42\ny = x").unwrap();
    let main = &tirs[0];
    let stmts = main.body_stmts();
    let v = main.var_decl_view(stmts.at(1));
    assert_eq!(main.inst(stmts.at(1)).ty, pool.int());
    assert_eq!(main.inst(v.initializer).ty, pool.int());
    assert!(matches!(main.inst(v.initializer).tag, TirTag::Var));
}

#[test]
fn undefined_variable_rejected() {
    let diags = run("x = y").unwrap_err();
    assert!(any_code(&diags, DiagCode::UndefinedVariable));
}

#[test]
fn undefined_function_rejected() {
    let diags = run("x = not_a_fn()").unwrap_err();
    assert!(any_code(&diags, DiagCode::UndefinedFunction));
}

#[test]
fn sema_continues_past_first_error_and_collects_multiple() {
    let diags = run("a = x\nb = y\n").unwrap_err();
    let undefs = diags
        .iter()
        .filter(|d| d.code == DiagCode::UndefinedVariable)
        .count();
    assert_eq!(undefs, 2, "got: {:#?}", diags);
}

#[test]
fn unknown_type_does_not_cascade() {
    let diags = run("x: nope = 1").unwrap_err();
    assert!(any_code(&diags, DiagCode::UnknownType));
    assert!(
        !any_code(&diags, DiagCode::TypeMismatch),
        "unexpected cascade: {:#?}",
        diags
    );
}

#[test]
fn function_call_return_type_resolved() {
    let code = "fn double(x: int) -> int:\n\treturn x * 2\n\nfn main():\n\tn = double(3)\n";
    let (tirs, pool) = run(code).unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let var = stmt_at(main, 0);
    let view = main.var_decl_view(var);
    let init = main.inst(view.initializer);
    assert_eq!(init.ty, pool.int());
    assert!(matches!(init.tag, TirTag::Call));
}

#[test]
fn void_function_signature() {
    let (tirs, pool) = run("fn greet():\n\tprint(\"hi\")\n\nfn main():\n\tgreet()\n").unwrap();
    let greet = tir_named(&tirs, &pool, "greet");
    assert_eq!(greet.return_type, pool.void());
    let main = tir_named(&tirs, &pool, "main");
    assert_eq!(main.return_type, pool.void());
}

#[test]
fn print_call_has_void_type() {
    // `print(...)` as a bare expression statement is the
    // canonical M8a form. The Call instruction itself is
    // typed `void` since print returns no value.
    let (tirs, pool) = run("print(\"Hello\\n\")").unwrap();
    let main = &tirs[0];
    let stmt_ref = stmt_at(main, 0);
    let stmt = main.inst(stmt_ref);
    assert!(matches!(stmt.tag, TirTag::ExprStmt));
    let call_ref = match stmt.data {
        TirData::UnOp(o) => o,
        other => panic!("expected ExprStmt UnOp, got {:?}", other),
    };
    let call = main.inst(call_ref);
    assert!(matches!(call.tag, TirTag::Call));
    assert_eq!(call.ty, pool.void());
}

#[test]
fn binding_to_void_value_rejected() {
    // The pre-M8a `_ = print(...)` / `msg = print(...)`
    // workarounds are now compile errors: a void value can't
    // be bound to a name.
    let diags = run("msg = print(\"Hello\")").unwrap_err();
    assert!(any_code(&diags, DiagCode::VoidValueInExpression));
}

#[test]
fn int_equality_yields_bool() {
    let (tirs, pool) = run("x = 1 == 2").unwrap();
    let main = &tirs[0];
    let v = main.var_decl_view(stmt_at(main, 0));
    assert_eq!(main.inst(v.initializer).ty, pool.bool_());
    assert!(matches!(main.inst(v.initializer).tag, TirTag::ICmpEq));
}

#[test]
fn mixed_type_equality_rejected() {
    let diags = run("x = 1 == true").unwrap_err();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
    assert!(first_msg(&diags).contains("type mismatch in '=='"));
}

#[test]
fn string_equality_accepted() {
    let (tirs, _pool) = run("x = \"a\" == \"b\"").unwrap();
    // The equality produces a bool-typed StrCmpEq instruction.
    let body = &tirs[0];
    let has_str_eq = body.instructions.iter().any(|i| i.tag == TirTag::StrCmpEq);
    assert!(has_str_eq, "expected StrCmpEq in TIR");
}

#[test]
fn bool_arithmetic_rejected() {
    let diags = run("x = true + 1").unwrap_err();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn bool_arithmetic_same_type_rejected_as_unsupported_op() {
    let diags = run("x = true + false").unwrap_err();
    assert!(any_code(&diags, DiagCode::UnsupportedOperator));
    assert!(!any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn bool_literal_true_type() {
    let (tirs, pool) = run("x = true").unwrap();
    let main = &tirs[0];
    let v = main.var_decl_view(stmt_at(main, 0));
    assert_eq!(main.inst(v.initializer).ty, pool.bool_());
    assert!(matches!(main.inst(v.initializer).data, TirData::Bool(true)));
}

#[test]
fn print_arity_rejected_in_sema() {
    let diags = run("print(\"a\", \"b\")").unwrap_err();
    assert!(any_code(&diags, DiagCode::ArityMismatch));
}

#[test]
fn return_type_mismatch_rejected() {
    let diags = run("fn answer() -> int:\n\treturn \"hello\"\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn call_arity_mismatch_rejected() {
    let code = "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main():\n\tx = add(1, 2, 3)\n";
    let diags = run(code).unwrap_err();
    assert!(any_code(&diags, DiagCode::ArityMismatch));
}

#[test]
fn call_argument_type_mismatch_rejected() {
    let code = "fn f(a: int) -> int:\n\treturn a\n\nfn main():\n\tx = f(true)\n";
    let diags = run(code).unwrap_err();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn main_with_return_type_rejected() {
    let diags = run("fn main() -> int:\n\treturn 0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::MainSignature));
}

#[test]
fn main_with_params_rejected() {
    let diags = run("fn main(x: int):\n\tprint(\"hi\")\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::MainSignature));
}

#[test]
fn return_value_in_void_function_rejected() {
    let diags = run("fn greet():\n\treturn 1\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn bare_return_in_void_function_accepted() {
    let (_tirs, _pool) = run("fn greet():\n\treturn\n").unwrap();
}

#[test]
fn void_function_without_explicit_return_accepted() {
    let (_tirs, _pool) = run("fn greet():\n\tprint(\"hi\")\n").unwrap();
}

#[test]
fn nonvoid_falloff_reports_missing_return() {
    let diags = run("fn f() -> int:\n\tx = 1\n").unwrap_err();
    assert_eq!(
        count_code(&diags, DiagCode::MissingReturn),
        1,
        "expected exactly one MissingReturn; got {:?}",
        diags
    );
}

#[test]
fn exhaustive_if_else_returns_accepted() {
    let code = "fn f(x: int) -> int:\n\tif x > 0:\n\t\treturn 1\n\telse:\n\t\treturn 2\n";
    let (_tirs, _pool) = run(code).unwrap();
}

#[test]
fn exhaustive_elif_chain_returns_accepted() {
    let code = "fn f(x: int) -> int:\n\tif x > 0:\n\t\treturn 1\n\telif x < 0:\n\t\treturn 0 - 1\n\telse:\n\t\treturn 0\n";
    let (_tirs, _pool) = run(code).unwrap();
}

#[test]
fn if_without_else_still_reports_missing_return() {
    let code = "fn f(x: int) -> int:\n\tif x > 0:\n\t\treturn 1\n";
    let diags = run(code).unwrap_err();
    assert!(any_code(&diags, DiagCode::MissingReturn));
}

#[test]
fn return_inside_while_does_not_count() {
    // Conservative: a loop body can execute zero times, so a
    // return inside `while` never satisfies return-flow.
    let code = "fn f(x: int) -> int:\n\twhile x > 0:\n\t\treturn 1\n";
    let diags = run(code).unwrap_err();
    assert!(any_code(&diags, DiagCode::MissingReturn));
}

#[test]
fn trailing_return_after_if_accepted() {
    let code = "fn f(x: int) -> int:\n\tif x > 0:\n\t\treturn 1\n\treturn 0\n";
    let (_tirs, _pool) = run(code).unwrap();
}

#[test]
fn panic_tail_counts_as_diverging() {
    // `panic` returns `never` — a function ending in it cannot
    // fall through, so no MissingReturn.
    let code = "fn f() -> int:\n\tpanic(\"boom\")\n";
    let (_tirs, _pool) = run(code).unwrap();
}

#[test]
fn never_var_decl_rejected() {
    // Binding a `never` value is an error — `panic` diverges and
    // produces no value to bind. No MissingReturn cascade: the
    // user's intent was to diverge.
    let diags = run("fn f() -> int:\n\tx = panic(\"boom\")\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::VoidValueInExpression));
    assert!(
        !any_code(&diags, DiagCode::MissingReturn),
        "no cascading MissingReturn; got {:?}",
        diags
    );
}

#[test]
fn never_assign_rejected() {
    let diags = run("fn f() -> int:\n\tmut x = 1\n\tx = panic(\"boom\")\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::VoidValueInExpression));
    assert!(
        !any_code(&diags, DiagCode::MissingReturn),
        "no cascading MissingReturn; got {:?}",
        diags
    );
}

#[test]
fn never_compound_assign_rejected() {
    let diags = run("fn f() -> int:\n\tmut x = 1\n\tx += panic(\"boom\")\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::VoidValueInExpression));
    assert!(
        !any_code(&diags, DiagCode::MissingReturn),
        "no cascading MissingReturn; got {:?}",
        diags
    );
}

#[test]
fn return_panic_rejected() {
    // `return <never>` is rejected like every other value
    // position: `panic` may only appear as a bare statement.
    let diags = run("fn f() -> int:\n\treturn panic(\"boom\")\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::VoidValueInExpression));
}

#[test]
fn never_binop_operand_rejected() {
    let diags = run("fn f() -> int:\n\treturn 1 + panic(\"boom\")\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::VoidValueInExpression));
}

#[test]
fn never_call_arg_rejected() {
    let code = "fn g(x: int):\n\tprint(\"{x}\")\n\nfn main():\n\tg(panic(\"boom\"))\n";
    let diags = run(code).unwrap_err();
    assert!(any_code(&diags, DiagCode::VoidValueInExpression));
}

#[test]
fn genuine_missing_return_reported_alongside_other_errors() {
    // An unrelated error doesn't mask return-flow: this function
    // genuinely lacks a return, so both diags fire.
    let (_tirs, diags, _pool) = run_with_errors("fn f() -> int:\n\tx = missing_var + 1\n");
    assert!(any_code(&diags, DiagCode::UndefinedVariable));
    assert!(any_code(&diags, DiagCode::MissingReturn));
}

#[test]
fn stmt_error_sentinel_suppresses_missing_return() {
    // A statement that failed analysis lowers to the Unreachable
    // sentinel; its structure is unknown, so return-flow must not
    // cascade a MissingReturn on top of the real error.
    let (_tirs, diags, _pool) = run_with_errors("fn f() -> int:\n\tx += 1\n");
    assert!(any_code(&diags, DiagCode::UndefinedAssignTarget));
    assert!(
        !any_code(&diags, DiagCode::MissingReturn),
        "no cascading MissingReturn; got {:?}",
        diags
    );
}

#[test]
fn vardecl_annotation_initializer_mismatch_rejected() {
    let diags = run("x: int = \"hello\"").unwrap_err();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn neg_on_bool_rejected() {
    let diags = run("x = -true").unwrap_err();
    assert!(any_code(&diags, DiagCode::UnsupportedOperator));
}

#[test]
fn neg_on_float_accepted() {
    let (tirs, pool) =
        run("fn main():\n\tx: float = 1.5\n\ty = -x\n").expect("float neg should type-check");
    let main = &tirs[0];
    let fneg = main
        .instructions
        .iter()
        .find(|inst| inst.tag == TirTag::FNeg)
        .expect("expected an FNeg instruction");
    assert_eq!(fneg.ty, pool.float());
}

#[test]
fn neg_on_float_literal_accepted() {
    run("fn main():\n\ty = -2.5\n\tprint(float_to_str(y))\n")
        .expect("float literal neg should type-check");
}

#[test]
fn nested_expression_types_all_filled() {
    let (tirs, _) = run("x = (1 + 2) * -3").unwrap();
    for tir in &tirs {
        for idx in 1..tir.instructions.len() {
            let inst = &tir.instructions[idx];
            // Every emitted instruction has *some* TypeId. The
            // error sentinel is allowed only at Unreachable; an
            // error-free run shouldn't have any.
            assert!(!matches!(inst.tag, TirTag::Unreachable));
        }
    }
}

/// §4.5 exit criterion: a UIR with a deliberate type error
/// produces (a) TIR for the rest of the function, (b) an
/// `Unreachable` instruction at the failure point, (c) exactly
/// one diagnostic in the sink.
#[test]
fn type_error_emits_unreachable_and_keeps_going() {
    // `-true` is the sole error; the print after it should
    // still appear in the function's TIR body.
    let src = "fn main():\n\tx = -true\n\tprint(\"after\")\n";
    let (tirs, diags, _pool) = run_with_errors(src);
    assert_eq!(diags.len(), 1, "got: {:#?}", diags);
    assert_eq!(diags[0].code, DiagCode::UnsupportedOperator);

    let main = &tirs[0];
    // Function body still has both statements.
    assert_eq!(main.body_stmts().len(), 2);

    // Find the Unreachable inserted in place of the failed
    // initializer.
    let mut saw_unreachable = false;
    for idx in 1..main.instructions.len() {
        if matches!(main.instructions[idx].tag, TirTag::Unreachable) {
            saw_unreachable = true;
            break;
        }
    }
    assert!(saw_unreachable, "expected an Unreachable instruction");
}

// ---------- Phase 5: worklist driver ----------

/// §5.4 exit criterion: source order doesn't matter for type
/// resolution. A caller defined *before* its callee still
/// type-checks, because signatures are eagerly resolved before
/// any body is walked.
#[test]
fn forward_reference_resolves() {
    let code = "fn main():\n\tn = helper(2)\n\nfn helper(x: int) -> int:\n\treturn x + 1\n";
    let (tirs, pool) = run(code).unwrap();
    // Both decls produced TIR.
    assert_eq!(tirs.len(), 2);
    // Source order is preserved in the result vec.
    assert_eq!(pool.str(tirs[0].name), "main");
    assert_eq!(pool.str(tirs[1].name), "helper");
}

/// §5.4: mutual recursion with explicit return types is *not* a
/// cycle — bodies depend only on callee signatures, which
/// resolve eagerly. The previous recursive driver also handled
/// this; the worklist version mustn't regress it (and mustn't
/// stack-overflow trying).
#[test]
fn mutual_recursion_does_not_trigger_cycle() {
    let code =
        "fn a() -> int:\n\treturn b()\n\nfn b() -> int:\n\treturn a()\n\nfn main():\n\tx = a()\n";
    let (tirs, _diags, _pool) = run_with_errors(code);
    assert_eq!(tirs.len(), 3);
}

/// Direct unit test of the cycle sentinel: bypass the public
/// driver, drive `require_decl` against an `InProgress` slot
/// directly. This is the substrate the comptime / inferred-
/// return-type milestones will lean on; today the path is
/// unreachable through source code.
#[test]
fn require_decl_reports_cycle_when_in_progress() {
    // Build a minimal valid UIR with one function so Sema::new
    // is happy.
    let mut pool = InternPool::new();
    let mut sink = DiagSink::new();
    let main_id = pool.intern_str("main");

    let mut b = ryo_core::uir::UirBuilder::new();
    let zero = b.int_literal(0, sp());
    let ret = b.unary(InstTag::Return, zero, sp());
    b.add_function(main_id, vec![], pool.int(), &[ret], sp());
    let uir = b.finish();

    let mut sema = Sema::new(&uir, &mut pool, &mut sink, "test", Path::new("<test>"));
    sema.resolve_signatures();
    // Pretend we're mid-resolving `main`.
    sema.decl_state[0] = DeclState::InProgress;
    let ok = sema.require_decl(DeclId::from_index(0), sp(), main_id);
    assert!(!ok, "require_decl must reject an InProgress decl");
    let diags = sink.into_diags();
    assert!(
        diags.iter().any(|d| d.code == DiagCode::CycleInResolution),
        "got: {:#?}",
        diags
    );
}

/// Source-order stability: when sema emits errors from multiple
/// functions, the diagnostic order matches the order the decls
/// appear in source. The risk register flagged worklist
/// non-determinism; this test pins the seed-in-source-order
/// invariant.
///
/// Note on what the assertion below actually proves:
/// `run_with_errors` returns `sink.into_diags()`, which yields
/// diagnostics in **emission order** — the order in which
/// `DiagSink::emit` was called, not a sorted-by-span order.
/// Because `first` and `second` each emit exactly one
/// `UndefinedVariable` diag, the assertion
/// `undef[0].span.start < undef[1].span.start` is a proxy for
/// "`first` was resolved before `second`": the worklist popped
/// decls in source order, so the diag from the body at the
/// lower span fired first. If `DiagSink::into_diags` ever
/// starts sorting by span, this test would silently keep
/// passing while no longer testing resolution order — update
/// the assertion to inspect emission-side state if that
/// changes. (Pipeline-level rendering does sort by span via
/// `render_diags`, but that's a separate path; sema's tests
/// observe the unsorted sink directly.)
#[test]
fn diagnostics_ordered_by_decl_then_position() {
    let code = "fn first() -> int:\n\treturn missing_a\n\nfn second() -> int:\n\treturn missing_b\n\nfn main():\n\tprint(\"go\")\n";
    let (_tirs, diags, _pool) = run_with_errors(code);
    let undef: Vec<_> = diags
        .iter()
        .filter(|d| d.code == DiagCode::UndefinedVariable)
        .collect();
    assert_eq!(undef.len(), 2, "got: {:#?}", diags);
    // `first` is at a lower span start than `second`, so its
    // diagnostic must come out first.
    assert!(undef[0].span.start < undef[1].span.start);
}

// ---------- Substrate stubs for comptime / generics ----------
//
// These are `cfg(any())`-gated ("never compiled") infrastructure
// tests. They exist as named hooks so the comptime / generics
// milestones land as concrete test bodies — no follow-on
// refactor needed to make space for them.
#[cfg(any())]
mod future {
    use super::*;

    #[test]
    fn comptime_block_evaluates_to_value() {
        // `comptime { 1 + 2 }` → Sema must evaluate, not emit
        // TIR; result interned as a value, substituted at use.
        unimplemented!("comptime — requires Phase 5 evaluator on top of the worklist");
    }

    #[test]
    fn generic_call_triggers_monomorphization() {
        // `fn id[T](x: T) -> T: return x` + `id(42)` + `id(true)`
        // → two TIRs from one UIR body, keyed on (DeclId,
        // [TypeId]).
        unimplemented!("generics — requires monomorphization on top of the worklist");
    }

    #[test]
    fn comptime_cycle_reports_diagnostic() {
        // A comptime block whose evaluation requires its own
        // result must emit `DiagCode::CycleInResolution`.
        unimplemented!("comptime cycle — exercises require_decl through a body-body edge");
    }
}

// ---- M7: float / ordering / modulo ----

#[test]
fn sema_float_literal_has_float_type() {
    let (tirs, pool) = run("x = 1.5").unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let v = main.var_decl_view(stmt_at(main, 0));
    let init = main.inst(v.initializer);
    assert!(matches!(init.tag, TirTag::FloatConst));
    assert_eq!(init.ty, pool.float());
}

#[test]
fn sema_float_arithmetic_lowers_to_fadd() {
    let (tirs, pool) = run("x: float = 1.0\ny = x + 2.5").unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let v = main.var_decl_view(stmt_at(main, 1));
    let init = main.inst(v.initializer);
    assert!(matches!(init.tag, TirTag::FAdd));
    assert_eq!(init.ty, pool.float());
}

#[test]
fn sema_int_division_and_modulo_stay_int() {
    let (tirs, pool) = run("a = 10\nb = 3\nq = a / b\nr = a % b").unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let q = main.var_decl_view(stmt_at(main, 2));
    let r = main.var_decl_view(stmt_at(main, 3));
    let q_init = main.inst(q.initializer);
    let r_init = main.inst(r.initializer);
    assert!(matches!(q_init.tag, TirTag::ISDiv));
    assert!(matches!(r_init.tag, TirTag::IMod));
    assert_eq!(q_init.ty, pool.int());
    assert_eq!(r_init.ty, pool.int());
}

#[test]
fn sema_ordering_returns_bool() {
    let (tirs, pool) = run("x = 1 < 2").unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let v = main.var_decl_view(stmt_at(main, 0));
    let init = main.inst(v.initializer);
    assert!(matches!(init.tag, TirTag::ICmpLt));
    assert_eq!(init.ty, pool.bool_());
}

#[test]
fn sema_float_ordering_lowers_to_fcmp() {
    let (tirs, pool) = run("x = 1.0 <= 2.0").unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let v = main.var_decl_view(stmt_at(main, 0));
    assert!(matches!(main.inst(v.initializer).tag, TirTag::FCmpLe));
}

#[test]
fn sema_mixed_int_float_arithmetic_errors() {
    let (_tirs, diags, _pool) = run_with_errors("x = 1 + 2.0");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::TypeMismatch
            && d.message.contains("'+'")
            && d.message.contains("int")
            && d.message.contains("float")),
        "diags: {:?}",
        diags
    );
}

#[test]
fn sema_mixed_int_float_ordering_errors() {
    let (_tirs, diags, _pool) = run_with_errors("x = 1 < 2.0");
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn sema_modulo_on_float_errors() {
    let (_tirs, diags, _pool) = run_with_errors("x = 1.0 % 2.0");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UnsupportedOperator
            && d.message.contains("'%'")
            && d.message.contains("float")),
        "diags: {:?}",
        diags
    );
}

#[test]
fn sema_ordering_on_bool_errors() {
    let (_tirs, diags, _pool) = run_with_errors("x = true < false");
    assert!(
        diags.iter().any(|d| d.code == DiagCode::UnsupportedOperator
            && d.message.contains("'<'")
            && d.message.contains("bool")),
        "diags: {:?}",
        diags
    );
}

// ---- M8b: assert builtin validation ----

#[test]
fn assert_true_literal_ok() {
    run("fn main():\n\tassert(true, \"ok\")\n").unwrap();
}

#[test]
fn assert_expression_condition_ok() {
    run("fn main():\n\tassert(1 == 1, \"math works\")\n").unwrap();
}

#[test]
fn assert_wrong_arity_zero_args() {
    let err = run("fn main():\n\tassert()\n").unwrap_err();
    assert!(any_code(&err, DiagCode::ArityMismatch));
}

#[test]
fn assert_wrong_arity_one_arg() {
    let err = run("fn main():\n\tassert(true)\n").unwrap_err();
    assert!(any_code(&err, DiagCode::ArityMismatch));
}

#[test]
fn assert_wrong_arity_three_args() {
    let err = run("fn main():\n\tassert(true, \"msg\", \"extra\")\n").unwrap_err();
    assert!(any_code(&err, DiagCode::ArityMismatch));
}

#[test]
fn assert_condition_must_be_bool() {
    let err = run("fn main():\n\tassert(42, \"not bool\")\n").unwrap_err();
    assert!(any_code(&err, DiagCode::TypeMismatch));
}

#[test]
fn assert_message_must_be_string_literal() {
    let err = run("fn main():\n\tassert(true, 42)\n").unwrap_err();
    assert!(any_code(&err, DiagCode::BuiltinArgKind));
}

// ---- M8c: panic builtin validation ----

#[test]
fn panic_wrong_arity_zero_args() {
    let err = run("fn main():\n\tpanic()\n").unwrap_err();
    assert!(any_code(&err, DiagCode::ArityMismatch));
}

#[test]
fn panic_wrong_arity_two_args() {
    let err = run("fn main():\n\tpanic(\"a\", \"b\")\n").unwrap_err();
    assert!(any_code(&err, DiagCode::ArityMismatch));
}

#[test]
fn panic_message_must_be_string_literal() {
    let err = run("fn main():\n\tpanic(42)\n").unwrap_err();
    assert!(any_code(&err, DiagCode::BuiltinArgKind));
}

#[test]
fn panic_string_literal_ok() {
    run("fn main():\n\tpanic(\"oops\")\n").unwrap();
}

#[test]
fn panic_call_has_never_type() {
    let (tirs, pool) = run("fn main():\n\tpanic(\"boom\")\n").unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let call_ref = stmt_at(main, 0);
    let inner = match main.inst(call_ref).data {
        TirData::UnOp(operand) => operand,
        _ => panic!("expected ExprStmt wrapping the call"),
    };
    assert_eq!(main.inst(inner).ty, pool.never());
}

#[test]
fn panic_lowers_to_ryo_panic_call() {
    let (tirs, pool) = run("fn main():\n\tpanic(\"boom\")\n").unwrap();
    let main = tir_named(&tirs, &pool, "main");
    // Statement 0 is ExprStmt wrapping the call
    let stmt = stmt_at(main, 0);
    let call_ref = match main.inst(stmt).data {
        TirData::UnOp(operand) => operand,
        _ => panic!("expected ExprStmt"),
    };
    let call = main.inst(call_ref);
    assert!(matches!(call.tag, TirTag::Call));
    let view = main.call_view(call_ref);
    assert_eq!(pool.str(view.name), "__ryo_panic");
    // Two args: pointer (str) and length (int)
    assert_eq!(view.args.len(), 2);
}

#[test]
fn assert_desugars_to_if_with_panic() {
    let (tirs, pool) = run("fn main():\n\tassert(false, \"oops\")\n").unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let stmt = stmt_at(main, 0);
    let inner_ref = match main.inst(stmt).data {
        TirData::UnOp(operand) => operand,
        _ => panic!("expected ExprStmt"),
    };
    // assert desugars to IfStmt
    assert!(
        matches!(main.inst(inner_ref).tag, TirTag::IfStmt),
        "assert should desugar to IfStmt, got {:?}",
        main.inst(inner_ref).tag
    );
}

// ---- M8c1: mutability + assignment ----

#[test]
fn reassign_mut_variable() {
    let result = run("fn main():\n\tmut x = 1\n\tx = 2\n");
    assert!(
        result.is_ok(),
        "reassignment to mut should succeed: {:?}",
        result.err()
    );
}

#[test]
fn reassign_immutable_rejected() {
    let diags = run("fn main():\n\tx = 1\n\tx = 2\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ImmutableAssign));
}

#[test]
fn compound_assign_mut_ok() {
    let result = run("fn main():\n\tmut x = 10\n\tx += 5\n");
    assert!(
        result.is_ok(),
        "compound assign to mut should succeed: {:?}",
        result.err()
    );
}

#[test]
fn compound_assign_immutable_rejected() {
    let diags = run("fn main():\n\tx = 10\n\tx += 5\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ImmutableAssign));
}

#[test]
fn compound_assign_undeclared_rejected() {
    let diags = run("fn main():\n\ty += 5\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::UndefinedAssignTarget));
}

#[test]
fn div_by_zero_literal_rejected() {
    let diags = run("fn main():\n\tx = 1 / 0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn mod_by_zero_literal_rejected() {
    let diags = run("fn main():\n\tx = 1 % 0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn compound_div_by_zero_literal_rejected() {
    let diags = run("fn main():\n\tmut x = 10\n\tx /= 0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn compound_mod_by_zero_literal_rejected() {
    let diags = run("fn main():\n\tmut x = 10\n\tx %= 0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn div_by_neg_zero_literal_rejected() {
    // `-0` parses as unary minus over the zero literal, not as a
    // signed literal — the check must see through the Neg.
    let diags = run("fn main():\n\tx = 1 / -0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn mod_by_neg_zero_literal_rejected() {
    let diags = run("fn main():\n\tx = 1 % -0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn compound_div_by_neg_zero_literal_rejected() {
    let diags = run("fn main():\n\tmut x = 10\n\tx /= -0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn compound_mod_by_neg_zero_literal_rejected() {
    let diags = run("fn main():\n\tmut x = 10\n\tx %= -0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn div_by_const_expr_zero_rejected() {
    let diags = run("fn main():\n\tx = 1 / (2 - 2)\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn div_by_double_neg_zero_rejected() {
    let diags = run("fn main():\n\tx = 1 / -(-0)\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn compound_div_by_const_expr_zero_rejected() {
    let diags = run("fn main():\n\tmut x = 10\n\tx /= (5 - 5)\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DivisionByZero));
}

#[test]
fn const_add_overflow_rejected() {
    // i64::MAX + 1: §18 traps overflow in all build modes, so a
    // constant expression that would trap is a compile error.
    let diags = run("fn main():\n\tx = 9223372036854775807 + 1\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ConstEvalFailure));
}

#[test]
fn const_mul_overflow_rejected() {
    let diags = run("fn main():\n\tx = 9223372036854775807 * 2\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ConstEvalFailure));
}

#[test]
fn const_neg_min_overflow_rejected() {
    // `(0 - MAX) - 1` const-evals to i64::MIN; negating it overflows.
    let diags = run("fn main():\n\tx = -((0 - 9223372036854775807) - 1)\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ConstEvalFailure));
}

#[test]
fn const_eval_at_boundary_ok() {
    // i64::MIN itself is reachable without overflow.
    let result = run("fn main():\n\tx = (0 - 9223372036854775807) - 1\n");
    assert!(
        result.is_ok(),
        "i64::MIN should compile: {:?}",
        result.err()
    );
}

#[test]
fn non_constant_zero_divisor_still_compiles() {
    // `x - x` is not a constant expression; the runtime guard
    // catches it instead of a compile error.
    let result = run("fn main():\n\tx = 5\n\ty = 1 / (x - x)\n");
    assert!(
        result.is_ok(),
        "non-constant divisor should compile: {:?}",
        result.err()
    );
}

#[test]
fn float_div_by_zero_literal_allowed() {
    // IEEE 754: float division by zero yields inf, no diagnostic.
    let result = run("fn main():\n\tx = 1.0 / 0.0\n");
    assert!(
        result.is_ok(),
        "float division by zero should compile: {:?}",
        result.err()
    );
}

#[test]
fn reassign_type_mismatch_rejected() {
    let diags = run("fn main():\n\tmut x = 1\n\tx = true\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

#[test]
fn assign_or_decl_creates_immutable_binding() {
    let result = run("fn main():\n\tx = 42\n\ty = x\n");
    assert!(
        result.is_ok(),
        "bare x = 42 should declare immutable x readable later: {:?}",
        result.err()
    );
}

#[test]
fn duplicate_decl_same_scope_rejected() {
    let diags = run("fn main():\n\tx = 1\n\tmut x = 2\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DuplicateDeclaration));
}

#[test]
fn duplicate_mut_decl_same_scope_rejected() {
    let diags = run("fn main():\n\tmut x = 1\n\tmut x = 2\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DuplicateDeclaration));
}

#[test]
fn duplicate_function_definition_rejected() {
    let diags = run("fn foo():\n\tx = 1\nfn foo():\n\ty = 2\nfn main():\n\tz = 3\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DuplicateDeclaration));
}

#[test]
fn distinct_function_definitions_ok() {
    run("fn foo():\n\tx = 1\nfn bar():\n\ty = 2\nfn main():\n\tz = 3\n").unwrap();
}

#[test]
fn cross_scope_reassign_mut() {
    let result = run("fn main():\n\tmut x = 1\n\tif true:\n\t\tx = 2\n");
    assert!(
        result.is_ok(),
        "cross-scope reassign of mut should succeed: {:?}",
        result.err()
    );
}

#[test]
fn cross_scope_reassign_immutable_rejected() {
    let diags = run("fn main():\n\tx = 1\n\tif true:\n\t\tx = 2\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ImmutableAssign));
}

#[test]
fn explicit_shadow_with_mut_ok() {
    let result = run("fn main():\n\tx = 1\n\tif true:\n\t\tmut x = 2\n");
    assert!(
        result.is_ok(),
        "explicit shadow with mut should work: {:?}",
        result.err()
    );
}

#[test]
fn percent_assign_on_float_rejected() {
    let diags = run("fn main():\n\tmut x = 1.0\n\tx %= 2.0\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::FloatModulo));
}

#[test]
fn compound_assign_all_int_ops() {
    for op in ["+=", "-=", "*=", "/=", "%="] {
        let code = format!("fn main():\n\tmut x = 10\n\tx {} 3\n", op);
        let result = run(&code);
        assert!(
            result.is_ok(),
            "{} on int should succeed: {:?}",
            op,
            result.err()
        );
    }
}

#[test]
fn compound_assign_float_arithmetic_ops() {
    for op in ["+=", "-=", "*=", "/="] {
        let code = format!("fn main():\n\tmut x = 1.0\n\tx {} 0.5\n", op);
        let result = run(&code);
        assert!(
            result.is_ok(),
            "{} on float should succeed: {:?}",
            op,
            result.err()
        );
    }
}

#[test]
fn compound_assign_on_bool_rejected() {
    let diags = run("fn main():\n\tmut x = true\n\tx += 1\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::UnsupportedOperator));
}

#[test]
fn compound_assign_rhs_type_mismatch_rejected() {
    let diags = run("fn main():\n\tmut x = 10\n\tx += true\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

// ---- Recursive / self-referential calls ----

#[test]
fn direct_recursion_accepted() {
    let code = "fn countdown(n: int) -> int:\n\tif n == 0:\n\t\treturn 0\n\treturn countdown(n - 1)\n\nfn main():\n\tx = countdown(5)\n";
    let result = run(code);
    assert!(
        result.is_ok(),
        "direct recursion should type-check: {:?}",
        result.err()
    );
}

#[test]
fn direct_recursion_return_type_resolved() {
    let code = "fn fib(n: int) -> int:\n\tif n < 2:\n\t\treturn n\n\treturn fib(n - 1) + fib(n - 2)\n\nfn main():\n\tx = fib(10)\n";
    let (tirs, pool) = run(code).unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let v = main.var_decl_view(stmt_at(main, 0));
    assert_eq!(main.inst(v.initializer).ty, pool.int());
}

#[test]
fn mutual_recursion_accepted() {
    let code = "fn is_even(n: int) -> bool:\n\tif n == 0:\n\t\treturn true\n\treturn is_odd(n - 1)\n\nfn is_odd(n: int) -> bool:\n\tif n == 0:\n\t\treturn false\n\treturn is_even(n - 1)\n\nfn main():\n\tx = is_even(4)\n";
    let result = run(code);
    assert!(
        result.is_ok(),
        "mutual recursion should type-check: {:?}",
        result.err()
    );
}

#[test]
fn mutual_recursion_return_types_resolved() {
    let code = "fn is_even(n: int) -> bool:\n\tif n == 0:\n\t\treturn true\n\treturn is_odd(n - 1)\n\nfn is_odd(n: int) -> bool:\n\tif n == 0:\n\t\treturn false\n\treturn is_even(n - 1)\n\nfn main():\n\tx = is_even(4)\n\ty = is_odd(3)\n";
    let (tirs, pool) = run(code).unwrap();
    let main = tir_named(&tirs, &pool, "main");
    let vx = main.var_decl_view(stmt_at(main, 0));
    let vy = main.var_decl_view(stmt_at(main, 1));
    assert_eq!(main.inst(vx.initializer).ty, pool.bool_());
    assert_eq!(main.inst(vy.initializer).ty, pool.bool_());
}

#[test]
fn recursive_call_wrong_arity_rejected() {
    let code = "fn f(n: int) -> int:\n\treturn f(n, n)\n\nfn main():\n\tx = f(1)\n";
    let diags = run(code).unwrap_err();
    assert!(any_code(&diags, DiagCode::ArityMismatch));
}

#[test]
fn recursive_call_wrong_arg_type_rejected() {
    let code = "fn f(n: int) -> int:\n\treturn f(true)\n\nfn main():\n\tx = f(1)\n";
    let diags = run(code).unwrap_err();
    assert!(any_code(&diags, DiagCode::TypeMismatch));
}

// ---- M8c2: while loops ----

#[test]
fn while_loop_basic() {
    let result = run("fn main():\n\twhile true:\n\t\tbreak\n");
    assert!(
        result.is_ok(),
        "basic while should succeed: {:?}",
        result.err()
    );
}

#[test]
fn while_non_bool_condition_rejected() {
    let diags = run("fn main():\n\twhile 42:\n\t\tbreak\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ConditionNotBool));
}

#[test]
fn break_outside_loop_rejected() {
    let diags = run("fn main():\n\tbreak\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::BreakOutsideLoop));
}

#[test]
fn continue_outside_loop_rejected() {
    let diags = run("fn main():\n\tcontinue\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ContinueOutsideLoop));
}

#[test]
fn break_inside_while_ok() {
    let result = run("fn main():\n\twhile true:\n\t\tbreak\n");
    assert!(
        result.is_ok(),
        "break inside while should succeed: {:?}",
        result.err()
    );
}

#[test]
fn continue_inside_while_ok() {
    let result = run("fn main():\n\tmut n = 3\n\twhile n > 0:\n\t\tn -= 1\n\t\tcontinue\n");
    assert!(
        result.is_ok(),
        "continue inside while should succeed: {:?}",
        result.err()
    );
}

#[test]
fn while_body_has_own_scope() {
    let diags =
        run("fn main():\n\twhile true:\n\t\tx = 1\n\t\tbreak\n\tassert(x == 1, \"unreachable\")\n")
            .unwrap_err();
    assert!(any_code(&diags, DiagCode::UndefinedVariable));
}

#[test]
fn nested_while_break_inner_only() {
    let result =
        run("fn main():\n\tmut i = 2\n\twhile i > 0:\n\t\twhile true:\n\t\t\tbreak\n\t\ti -= 1\n");
    assert!(
        result.is_ok(),
        "nested while with inner break: {:?}",
        result.err()
    );
}

// ---- M8c3: for-range loops ----

#[test]
fn for_range_basic() {
    let result = run("fn main():\n\tfor i in range(0, 5):\n\t\tx = i\n");
    assert!(
        result.is_ok(),
        "basic for-range should succeed: {:?}",
        result.err()
    );
}

#[test]
fn for_range_non_int_start_rejected() {
    let diags = run("fn main():\n\tfor i in range(true, 5):\n\t\tx = i\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::RangeArgType));
}

#[test]
fn for_range_non_int_end_rejected() {
    let diags = run("fn main():\n\tfor i in range(0, 1.5):\n\t\tx = i\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::RangeArgType));
}

#[test]
fn for_range_loop_var_immutable() {
    let diags = run("fn main():\n\tfor i in range(0, 5):\n\t\ti = 10\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ImmutableAssign));
}

#[test]
fn for_range_loop_var_not_visible_after() {
    let diags = run("fn main():\n\tfor i in range(0, 5):\n\t\tx = i\n\ty = i\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::UndefinedVariable));
}

#[test]
fn for_range_duplicate_decl_in_body_rejected() {
    let diags = run("fn main():\n\tfor i in range(0, 5):\n\t\tmut i = 10\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::DuplicateDeclaration));
}

#[test]
fn for_range_shadow_in_nested_scope_ok() {
    let result =
        run("fn main():\n\tfor i in range(0, 5):\n\t\tif true:\n\t\t\tmut i = 10\n\t\t\tx = i\n");
    assert!(
        result.is_ok(),
        "shadow in nested scope should work: {:?}",
        result.err()
    );
}

#[test]
fn for_range_break_continue_ok() {
    let result = run(
        "fn main():\n\tfor i in range(0, 10):\n\t\tif i == 3:\n\t\t\tcontinue\n\t\tif i == 7:\n\t\t\tbreak\n",
    );
    assert!(
        result.is_ok(),
        "break/continue in for should work: {:?}",
        result.err()
    );
}

#[test]
fn range_redefinition_as_variable_rejected() {
    let diags = run("fn main():\n\trange = 42\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
}

#[test]
fn range_redefinition_as_mut_rejected() {
    let diags = run("fn main():\n\tmut range = 42\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
}

#[test]
fn range_as_function_name_rejected() {
    let diags =
        run("fn range(a: int, b: int) -> int:\n\treturn a + b\n\nfn main():\n\tprint(\"hi\")\n")
            .unwrap_err();
    assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
}

#[test]
fn range_called_outside_loop_gives_helpful_error() {
    let diags = run("fn main():\n\trange(0, 5)\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
}

#[test]
fn reserved_name_no_cascade_undefined_variable() {
    let diags = run("fn main():\n\trange = 42\n\tprint(range)\n").unwrap_err();
    assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
    assert!(
        !any_code(&diags, DiagCode::UndefinedVariable),
        "should not cascade into UndefinedVariable"
    );
}
