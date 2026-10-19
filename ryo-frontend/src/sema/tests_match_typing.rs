//! Match arm typing (E0133-E0135) and the typed TIR `Match` emission:
//! per-arm binding scopes, per-arm result-type joins (never/void
//! rules), the E0133 borrow-binding rule, and owner-token ordering.
//! Split out of `tests_match.rs` to stay under the file-length limit;
//! the match-view helpers come from `tests_match.rs` (pub(super)).

use super::tests::*;
use super::tests_match::{match_in_var_init, match_stmt_operand};
use super::*;
use chumsky::span::Span as _;
use ryo_core::tir::{Discriminant, MatchBinding, MatchBodyView, TirRef, TirTag};

// ---------- Arm typing (E0133-E0135) + typed TIR Match ----------

#[test]
fn value_join_emits_typed_match_with_arm_bindings() {
    // `match opt: Opt.Some(v): v / Opt.None: 0` — the match types as
    // `int` (the joined arm type) and arm 0 carries its payload binding
    // on the wire: (field 0, `v`, int) in declaration-order index form.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tx = match o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: 0\n";
    let (tirs, pool) = run(src).expect("typed match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert!(matches!(main.inst(m).tag, TirTag::Match));
    assert_eq!(main.inst(m).ty, pool.int());
    let view = main.match_view(m);
    let scrutinee = view.scrutinee;
    assert!(
        matches!(main.inst(scrutinee).tag, TirTag::Var),
        "the scrutinee is the `o` binding's read, got {:?}",
        main.inst(scrutinee).tag
    );
    assert_eq!(view.ty, pool.int());
    assert_eq!(view.len(), 2);
    assert_eq!(view.arm(0).discriminant, Discriminant::Variant(0));
    assert_eq!(view.arm(1).discriminant, Discriminant::Variant(1));
    let v = pool.find_str("v").expect("`v` interned");
    assert_eq!(
        view.arm(0).bindings().collect::<Vec<_>>(),
        [MatchBinding {
            field_index: 0,
            name: v,
            ty: pool.int(),
        }],
        "arm 0 binds field 0 as `v: int`"
    );
    assert_eq!(view.arm(1).binding_count(), 0);
    let MatchBodyView::Expr(arm0_body) = view.arm(0).body else {
        panic!("arm 0 is an expr body, got {:?}", view.arm(0).body);
    };
    assert_eq!(main.inst(arm0_body).ty, pool.int());
}

#[test]
fn never_joins_freely_and_statement_arm_bodies_wire_as_stmts() {
    // A `return` arm contributes `never`: the match still types as int.
    // The statement-arm body rides the wire as a stmt list (the Return
    // inst), not an expr operand.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tx = match o:\n\t\tOpt.Some(v): return v\n\t\tOpt.None: 0\n\treturn x\n";
    let (tirs, pool) = run(src).expect("never-arm join compiles");
    let f = tir_named(&tirs, &pool, "f");
    let m = match_in_var_init(f, 0);
    assert_eq!(f.inst(m).ty, pool.int());
    let view = f.match_view(m);
    let arm0 = view.arm(0);
    let stmts: Vec<TirRef> = arm0.body.stmts().collect();
    assert_eq!(stmts.len(), 1, "single-line `return` arm is one stmt");
    assert!(
        matches!(f.inst(stmts[0]).tag, TirTag::Return),
        "the stmt body is the Return inst, got {:?}",
        f.inst(stmts[0]).tag
    );
    let MatchBodyView::Expr(none_body) = view.arm(1).body else {
        panic!("arm 1 is an expr body, got {:?}", view.arm(1).body);
    };
    assert_eq!(f.inst(none_body).ty, pool.int());
}

#[test]
fn all_never_match_types_as_never() {
    // Every arm returns: the match expression itself is `never`, and a
    // void function whose body is that match satisfies return-flow
    // analysis through the ExprStmt-never rule.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tmatch o:\n\t\tOpt.Some(v):\n\t\t\treturn v\n\t\tOpt.None:\n\t\t\treturn 0\n";
    let (tirs, pool) = run(src).expect("all-never match compiles");
    let f = tir_named(&tirs, &pool, "f");
    let m = match_stmt_operand(f, 0);
    assert_eq!(f.inst(m).ty, pool.never(), "all-never match is never");
}

#[test]
fn panic_expr_arm_joins_as_diverging_no_e0017() {
    // Reviewer case: an inline `panic(...)` expression arm is analyzed through the
    // never-tolerant entry point, so it joins as DIVERGING (the match
    // still types as int from the value arm) — not the E0017
    // value-position rejection a never-typed expression usually gets.
    let src = "fn main():\n\tx = match true:\n\t\ttrue: 1\n\t\tfalse: panic(\"no\")\n\tprint(x)\n";
    let (tirs, diags, pool) = run_with_errors(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != DiagCode::VoidValueInExpression),
        "no E0017 for the panic arm: {diags:?}"
    );
    assert_eq!(diags.len(), 0, "the mixed value/diverging match is clean");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 0);
    assert_eq!(
        main.inst(m).ty,
        pool.int(),
        "the value arm sets the join type"
    );
    let view = main.match_view(m);
    let MatchBodyView::Expr(diverging) = view.arm(1).body else {
        panic!("arm 1 is an expr body, got {:?}", view.arm(1).body);
    };
    assert_eq!(
        main.inst(diverging).ty,
        pool.never(),
        "the panic arm body stays never-typed on the wire"
    );
}

#[test]
fn all_diverging_panic_and_return_arms_type_never() {
    // Reviewer-round pair: `panic(...)` (expr arm) and `return` (statement arm)
    // in one match — both diverge, the join is `never`, no E0017.
    let src = "fn f(b: bool) -> int:\n\tmatch b:\n\t\ttrue: panic(\"a\")\n\t\tfalse: return 7\n\nfn main():\n\tprint(f(false))\n";
    let (tirs, diags, pool) = run_with_errors(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != DiagCode::VoidValueInExpression),
        "no E0017 for the panic arm: {diags:?}"
    );
    assert_eq!(diags.len(), 0, "the all-diverging match is clean");
    let f = tir_named(&tirs, &pool, "f");
    let m = match_stmt_operand(f, 0);
    assert_eq!(f.inst(m).ty, pool.never(), "all-diverging match is never");
}

#[test]
fn view_result_is_rejected_with_e0137() {
    // P2c: every value arm agreeing on a view type passes the E0134 join
    // but no merge shape exists for a view result — E0137 names the view
    // type and the deferral, and the match recovers error-typed (so
    // codegen's eval_inst_view can never see a Match).
    let src = "fn main():\n\ts = \"hello\"\n\tv = match true:\n\t\ttrue: s[0:2]\n\t\tfalse: s[2:4]\n\tprint(v)\n";
    let (tirs, diags, pool) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MatchViewResult)
        .expect("E0137 must fire");
    for needle in ["'strview'", "not yet supported"] {
        assert!(
            diag.message.contains(needle),
            "message must contain {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert!(
        matches!(main.inst(m).tag, TirTag::Unreachable),
        "errored match recovers with Unreachable, got {:?}",
        main.inst(m).tag
    );
}

#[test]
fn view_result_with_e0134_offender_reports_only_e0134() {
    // A view candidate against a disagreeing value arm is a plain E0134
    // — piling the E0137 deferral on top would double-report.
    let src = "fn main():\n\ts = \"hello\"\n\tv = match true:\n\t\ttrue: s[0:2]\n\t\tfalse: 1\n\tprint(v)\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(
        diags.iter().any(|d| d.code == DiagCode::ArmTypeMismatch),
        "E0134 must fire: {diags:?}"
    );
    assert!(
        diags.iter().all(|d| d.code != DiagCode::MatchViewResult),
        "no E0137 pile-on when a plain mismatch exists: {diags:?}"
    );
}

#[test]
fn arm_type_mismatch_is_e0134_naming_both_types() {
    // Arms yielding `int` and `str`: one E0134, primary span on the
    // offending arm's body, and the errored match recovers with the
    // error-typed sentinel.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tx = match o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: \"s\"\n";
    let (tirs, diags, pool) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmTypeMismatch)
        .expect("E0134 must fire");
    for needle in ["'int'", "'str'"] {
        assert!(
            diag.message.contains(needle),
            "message must name both arm types ({needle}), got {:?}",
            diag.message
        );
    }
    let body = src.find("\"s\"").expect("the str arm body");
    assert_eq!(
        diag.span.start(),
        body,
        "E0134 anchors on the offending arm body, got {:?}",
        diag.span
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
    // Recovery shape: an errored match is the Unreachable sentinel with
    // the error type — never a partially-typed Match.
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert!(
        matches!(main.inst(m).tag, TirTag::Unreachable),
        "errored match recovers with Unreachable, got {:?}",
        main.inst(m).tag
    );
    assert_eq!(main.inst(m).ty, pool.error_type());
}

#[test]
fn arm_type_mismatch_notes_every_later_offender() {
    // Three value arms with three types: the primary squiggle lands on
    // the FIRST offending arm body, a NOTE carries each later one.
    let src =
        "fn main():\n\tn = 5\n\tx = match n:\n\t\t0: 1\n\t\t1: \"a\"\n\t\t2: true\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmTypeMismatch)
        .expect("E0134 must fire");
    let first = src.find("\"a\"").expect("the str arm body");
    assert_eq!(diag.span.start(), first, "primary on the first offender");
    assert_eq!(diag.notes.len(), 1, "one note for the other arm: {diag:?}");
    let later = src.find("true\n").expect("the bool arm body");
    assert_eq!(
        diag.notes[0].span.map(|s| s.start()),
        Some(later),
        "the note anchors on the later offender's body, got {:?}",
        diag.notes[0]
    );
    assert!(
        diag.notes[0].message.contains("'bool'"),
        "the note names its arm's type, got {:?}",
        diag.notes[0].message
    );
    assert_eq!(diags.len(), 1, "exactly one diagnostic: {diags:?}");
}

#[test]
fn void_expr_arm_in_value_match_is_e0134() {
    // A void-producing expr arm (a bare `print` call) participates in
    // the join like any arm type: against value arms it mismatches.
    let src = "fn main():\n\tn = 5\n\tx = match n:\n\t\t0: 1\n\t\t_: print(\"x\")\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmTypeMismatch)
        .expect("E0134 must fire for the void arm");
    for needle in ["'int'", "'void'"] {
        assert!(
            diag.message.contains(needle),
            "message must name {needle}, got {:?}",
            diag.message
        );
    }
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn falling_statement_arm_in_value_match_is_e0135() {
    // `Some(v): x = 5` — a statement arm that falls through while the
    // other arm produces a value: E0135 at the falling arm, suggesting
    // the fix.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\tmut x = 0\n\to = Opt.Some{value=5}\n\tmatch o:\n\t\tOpt.Some(v): x = 5\n\t\tOpt.None: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmNoValue)
        .expect("E0135 must fire");
    let body = src.find("x = 5").expect("the falling arm body");
    assert_eq!(
        diag.span.start(),
        body,
        "E0135 anchors on the falling arm body, got {:?}",
        diag.span
    );
    assert!(
        diag.notes
            .iter()
            .any(|n| n.message.contains("value") && n.message.contains("return")),
        "the help must suggest ending with a value or `return`, got {:?}",
        diag.notes
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn all_void_match_allows_fallthrough_and_types_void() {
    // Every arm produces no value (void expr arm / falling statement
    // arm): no E0135, and the match types as void. Both arm shapes —
    // the single-line void call and a block arm that falls through.
    let src = "fn main():\n\tmatch \"a\":\n\t\t_: print(\"x\")\n";
    let (tirs, pool) = run(src).expect("void match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 0);
    assert_eq!(main.inst(m).ty, pool.void(), "void arms join to void");

    let src = "fn main():\n\tmatch \"a\":\n\t\t_:\n\t\t\tprint(\"x\")\n";
    let (tirs, pool) = run(src).expect("fallthrough void match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 0);
    assert_eq!(
        main.inst(m).ty,
        pool.void(),
        "a falling statement arm is legal when no arm produces a value"
    );
}

#[test]
fn borrowed_scrutinee_binding_non_copy_field_is_e0133() {
    // `match &o:` (M8.2 borrow) with `Some(v):` binding the str payload:
    // moving a non-Copy field out of a borrow is E0133 — names the
    // binding and its type, with the workaround help note.
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn main():\n\to = Opt.Some{value=\"hi\"}\n\tmatch &o:\n\t\tOpt.Some(v): print(v)\n\t\tOpt.None: print(\"none\")\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MoveOutOfBorrowedScrutinee)
        .expect("E0133 must fire");
    for needle in ["'v'", "'str'"] {
        assert!(
            diag.message.contains(needle),
            "message must name the binding and its type ({needle}), got {:?}",
            diag.message
        );
    }
    assert!(
        !diag.notes.is_empty(),
        "E0133 must carry the workaround help note: {diag:?}"
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn owned_scrutinee_binds_non_copy_field_and_emits_owner_token() {
    // The same match without `&` is fine: the str binding is a by-move
    // bind — exactly one FieldAccess owner token immediately before the
    // Match, keyed on the binding (never evaluated by codegen).
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn main():\n\to = Opt.Some{value=\"hi\"}\n\tmatch o:\n\t\tOpt.Some(v): print(v)\n\t\tOpt.None: print(\"none\")\n";
    let (tirs, pool) = run(src).expect("owned scrutinee binds by move");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 1);
    assert_eq!(main.inst(m).ty, pool.void());
    let owners = main.match_by_move_owner_refs(m, &pool);
    assert_eq!(owners.len(), 1, "one owner token for the str binding");
    let token = owners[0];
    assert_eq!(token.index(), m.index() - 1, "token immediately precedes");
    assert!(
        matches!(main.inst(token).tag, TirTag::FieldAccess),
        "owner token is a FieldAccess, got {:?}",
        main.inst(token).tag
    );
    assert_eq!(
        main.inst(token).ty,
        pool.str_(),
        "token carries the field type"
    );
}

#[test]
fn copy_bindings_skip_owner_tokens() {
    // Copy-typed payload binds copy out of the scrutinee — no owner
    // tokens at all, even on a borrowed scrutinee.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tx = match &o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: 0\n";
    let (tirs, pool) = run(src).expect("Copy payload on a borrow is fine");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert_eq!(main.inst(m).ty, pool.int());
    assert!(
        main.match_by_move_owner_refs(m, &pool).is_empty(),
        "Copy bindings emit no owner tokens"
    );
}

#[test]
fn wildcard_arm_on_borrow_suppresses_no_binds() {
    // `Some(_):` on a borrowed scrutinee binds nothing — no E0133, and
    // no owner tokens or bindings on the wire.
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn main():\n\to = Opt.Some{value=\"hi\"}\n\tmatch &o:\n\t\tOpt.Some(_): print(\"some\")\n\t\tOpt.None: print(\"none\")\n";
    let (tirs, pool) = run(src).expect("wildcard arm on a borrow is fine");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_stmt_operand(main, 1);
    assert_eq!(main.inst(m).ty, pool.void());
    assert_eq!(main.match_view(m).arm(0).binding_count(), 0);
}

#[test]
fn borrowed_param_scrutinee_binding_non_copy_is_e0133() {
    // The other borrowed shape: a plain (borrow-mode) parameter used as
    // the scrutinee. Binding its non-Copy payload is E0133 without any
    // `&` marker. (Void function: an errored match's error-typed
    // recovery does not satisfy return-flow analysis, so a value-
    // returning fn would cascade a spurious MissingReturn on top.)
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn f(o: Opt):\n\tmatch o:\n\t\tOpt.Some(v): print(v)\n\t\tOpt.None: print(\"\")\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MoveOutOfBorrowedScrutinee)
        .expect("E0133 must fire for a borrowed param scrutinee");
    assert!(
        diag.message.contains("'v'"),
        "message names the binding, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn borrowed_param_field_projection_scrutinee_binding_non_copy_is_e0133() {
    // `match w.inner:` — a FieldAccess chain rooted at a borrow-mode
    // parameter. The projection borrows with its root, so binding the
    // non-Copy payload (`Msg` carries a str field) is E0133 exactly
    // like a bare `w` scrutinee.
    let src = "struct Msg:\n\ttext: str\n\nenum Note:\n\tInfo(Msg)\n\tBlank\n\nstruct Wrapper:\n\tinner: Note\n\nfn f(w: Wrapper):\n\tmatch w.inner:\n\t\tNote.Info(m): print(m.text)\n\t\tNote.Blank: print(\"\")\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::MoveOutOfBorrowedScrutinee)
        .expect("E0133 must fire for a borrowed-param projection scrutinee");
    assert!(
        diag.message.contains("'m'"),
        "message names the binding, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn move_param_field_projection_scrutinee_binds_non_copy_freely() {
    // A `move` parameter is owned, so the same projection match is
    // legal — the borrowed-root walk must not over-reject.
    let src = "struct Msg:\n\ttext: str\n\nenum Note:\n\tInfo(Msg)\n\tBlank\n\nstruct Wrapper:\n\tinner: Note\n\nfn f(move w: Wrapper):\n\tmatch w.inner:\n\t\tNote.Info(m): print(m.text)\n\t\tNote.Blank: print(\"\")\n";
    run(src).expect("move param projection is an owned scrutinee");
}

#[test]
fn move_param_scrutinee_binds_non_copy_freely() {
    // A `move` parameter is owned: the same binding is legal.
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn f(move o: Opt) -> str:\n\tmatch o:\n\t\tOpt.Some(v): return v\n\t\tOpt.None: return \"\"\n";
    run(src).expect("move param is an owned scrutinee");
}

#[test]
fn e0133_fires_per_arm_not_per_match() {
    // Review-focus pin: one match where arm A binds a Copy field (ok)
    // and arm B binds a str field (E0133) — exactly one E0133, and the
    // Copy arm is untouched.
    let src = "enum Mix:\n\tA(n: int)\n\tB(s: str)\n\nfn main():\n\tm = Mix.A{n=1}\n\tmatch &m:\n\t\tMix.A(n): print(n)\n\t\tMix.B(s): print(s)\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert_eq!(
        count_code(&diags, DiagCode::MoveOutOfBorrowedScrutinee),
        1,
        "exactly one E0133 for the str-binding arm: {diags:?}"
    );
    assert_eq!(diags.len(), 1, "no other diagnostics: {diags:?}");
}

#[test]
fn arm_bindings_do_not_leak_past_the_match() {
    // `v` is visible inside its arm and unresolvable after the match —
    // the arm scope dies with the arm (E0010-style unresolved name).
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tx = match o:\n\t\tOpt.Some(v): v + 1\n\t\tOpt.None: 0\n\tprint(v)\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::UndefinedVariable)
        .expect("the post-match reference must not resolve");
    let use_site = src.rfind("print(v)").expect("the post-match use") + "print(".len();
    assert_eq!(
        diag.span.start(),
        use_site,
        "the unresolved use anchors on `v`, got {:?}",
        diag.span
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn same_binding_name_in_different_arms_is_allowed() {
    let src = "enum Two:\n\tA(v: int)\n\tB(v: int)\n\nfn main():\n\tt = Two.A{v=1}\n\tx = match t:\n\t\tTwo.A(v): v\n\t\tTwo.B(v): v * 2\n";
    let (tirs, pool) = run(src).expect("per-arm scopes never collide");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    let v = pool.find_str("v").expect("`v` interned");
    for arm in view.arms() {
        assert_eq!(
            arm.bindings().collect::<Vec<_>>(),
            [MatchBinding {
                field_index: 0,
                name: v,
                ty: pool.int(),
            }]
        );
    }
}

#[test]
fn duplicate_binding_name_within_one_arm_is_rejected() {
    // `P.Both(v, v):` — the same name twice in ONE arm's pattern hits
    // the duplicate-binding diagnostic (the scope-insertion check), and
    // the errored match recovers.
    let src = "enum P:\n\tBoth(a: int, b: int)\n\nfn main():\n\tp = P.Both{a=1, b=2}\n\tx = match p:\n\t\tP.Both(v, v): v\n\t\t_: 0\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::DuplicateDeclaration)
        .expect("the duplicate binding must fire");
    assert!(
        diag.message.contains("'v'"),
        "message names the duplicated binding, got {:?}",
        diag.message
    );
    assert_eq!(diags.len(), 1, "exactly one error: {diags:?}");
}

#[test]
fn owner_tokens_follow_arm_then_binding_order() {
    // Two by-move bindings across two arms: the tokens land immediately
    // before the Match in arm order, then binding order within each arm
    // — the exact layout `match_by_move_owner_refs` recovers.
    let src = "enum E:\n\tPair(a: str, b: str)\n\tSolo(s: str)\n\nfn main():\n\te = E.Pair{a=\"a\", b=\"b\"}\n\tx = match e:\n\t\tE.Pair(a, b): 0\n\t\tE.Solo(s): 1\n";
    let (tirs, pool) = run(src).expect("multi-bind match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let owners = main.match_by_move_owner_refs(m, &pool);
    assert_eq!(owners.len(), 3, "one token per by-move binding");
    for (i, token) in owners.iter().enumerate() {
        assert_eq!(
            token.index(),
            m.index() - 3 + i,
            "token {i} immediately precedes the Match"
        );
        assert!(
            matches!(main.inst(*token).tag, TirTag::FieldAccess),
            "token {i} is a FieldAccess, got {:?}",
            main.inst(*token).tag
        );
        assert_eq!(
            main.inst(*token).ty,
            pool.str_(),
            "token {i} carries the field type"
        );
    }
    // The wire order mirrors it: arm 0's bindings (a, b) then arm 1's (s).
    let a = pool.find_str("a").unwrap();
    let b = pool.find_str("b").unwrap();
    let s = pool.find_str("s").unwrap();
    let view = main.match_view(m);
    assert_eq!(
        view.arm(0).bindings().collect::<Vec<_>>(),
        [
            MatchBinding {
                field_index: 0,
                name: a,
                ty: pool.str_(),
            },
            MatchBinding {
                field_index: 1,
                name: b,
                ty: pool.str_(),
            },
        ]
    );
    assert_eq!(
        view.arm(1).bindings().collect::<Vec<_>>(),
        [MatchBinding {
            field_index: 0,
            name: s,
            ty: pool.str_(),
        }]
    );
}

#[test]
fn literal_discriminants_round_trip_folded_values() {
    // Negative ints arrive pre-folded; str/bool literals keep their
    // folded forms; the wildcard arm is the catch-all encoding.
    let src =
        "fn main():\n\tn = 5\n\tx = match n:\n\t\t0: \"zero\"\n\t\t-1: \"neg\"\n\t\t_: \"other\"\n";
    let (tirs, pool) = run(src).expect("literal arms type");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert_eq!(main.inst(m).ty, pool.str_());
    let view = main.match_view(m);
    assert_eq!(view.arm(0).discriminant, Discriminant::Int(0));
    assert_eq!(view.arm(1).discriminant, Discriminant::Int(-1));
    assert_eq!(view.arm(2).discriminant, Discriminant::Wildcard);

    let src = "fn main():\n\tb = true\n\tx = match b:\n\t\ttrue: 1\n\t\tfalse: 0\n";
    let (tirs, pool) = run(src).expect("bool arms type");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    assert_eq!(view.arm(0).discriminant, Discriminant::Bool(true));
    assert_eq!(view.arm(1).discriminant, Discriminant::Bool(false));

    let src = "fn main():\n\ts = \"a\"\n\tx = match s:\n\t\t\"quit\": 1\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("str arms type");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    let quit = pool.find_str("quit").expect("`quit` interned");
    assert_eq!(view.arm(0).discriminant, Discriminant::Str(quit));
}

#[test]
fn anon_arm_encodes_placeholder_wildcard_discriminant() {
    // Anon patterns carry no M12 discriminant (structural lowering is
    // M12.5): the arm binds its targets and rides the wire with the
    // Wildcard placeholder encoding, documented at the emission site.
    // A clean M12 anon match necessarily has a real `_:` arm (E0129
    // otherwise).
    let src = "fn main():\n\tp = {x=1, y=2}\n\tx = match p:\n\t\t{x=px, y=_}: px\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("anon-arm match compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert_eq!(main.inst(m).ty, pool.int());
    let view = main.match_view(m);
    let px = pool.find_str("px").expect("`px` interned");
    assert_eq!(
        view.arm(0).discriminant,
        Discriminant::Wildcard,
        "anon arms encode the Wildcard placeholder"
    );
    assert_eq!(
        view.arm(0).bindings().collect::<Vec<_>>(),
        [MatchBinding {
            field_index: 0,
            name: px,
            ty: pool.int(),
        }]
    );
    assert_eq!(view.arm(1).discriminant, Discriminant::Wildcard);
    assert_eq!(view.arm(1).binding_count(), 0);
}

#[test]
fn statement_arm_block_bodies_analyze_in_the_arm_scope() {
    // A block arm's statements see the arm bindings; declarations inside
    // the arm do not leak either.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tmatch o:\n\t\tOpt.Some(v):\n\t\t\tw = v + 1\n\t\t\treturn w\n\t\tOpt.None:\n\t\t\treturn 0\n";
    run(src).expect("block arm bodies analyze against the arm bindings");

    // `w` is arm-scoped: using it in the sibling arm is unresolved.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tmatch o:\n\t\tOpt.Some(v):\n\t\t\tw = v + 1\n\t\t\treturn w\n\t\tOpt.None:\n\t\t\treturn w\n";
    let (_t, diags, _p) = run_with_errors(src);
    assert!(
        any_code(&diags, DiagCode::UndefinedVariable),
        "the sibling arm must not see `w`: {diags:?}"
    );
    assert_eq!(
        count_code(&diags, DiagCode::UndefinedVariable),
        1,
        "exactly one unresolved use: {diags:?}"
    );
}

#[test]
fn nested_matches_keep_their_owner_tokens_separate() {
    // An inner match (with its own by-move binding) inside an outer
    // match's arm body: each Match's count-back recovery lands on its
    // own tokens — the inner emission is fully inside the outer arm's
    // body region.
    let src = "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn main():\n\ta = Opt.Some{value=\"a\"}\n\tb = Opt.Some{value=\"b\"}\n\tmatch a:\n\t\tOpt.Some(v):\n\t\t\ty = match b:\n\t\t\t\tOpt.Some(w): w\n\t\t\t\tOpt.None: \"\"\n\t\t\tprint(y)\n\t\t\tprint(v)\n\t\tOpt.None:\n\t\t\tprint(\"none\")\n";
    let (tirs, pool) = run(src).expect("nested matches compile");
    let main = tir_named(&tirs, &pool, "main");
    let outer = match_stmt_operand(main, 2);
    let view = main.match_view(outer);
    assert_eq!(view.len(), 2);
    let outer_owners = main.match_by_move_owner_refs(outer, &pool);
    assert_eq!(outer_owners.len(), 1, "outer match: one by-move bind (v)");
    assert_eq!(
        outer_owners[0].index(),
        outer.index() - 1,
        "the outer token immediately precedes the outer Match"
    );
    // Reach the inner match through the outer arm's stmt list.
    let arm0_stmts: Vec<TirRef> = view.arm(0).body.stmts().collect();
    let inner = main.var_decl_view(arm0_stmts[0]).initializer;
    assert!(
        matches!(main.inst(inner).tag, TirTag::Match),
        "the arm's first stmt binds the inner match, got {:?}",
        main.inst(inner).tag
    );
    let inner_owners = main.match_by_move_owner_refs(inner, &pool);
    assert_eq!(inner_owners.len(), 1, "inner match: one by-move bind (w)");
    assert_eq!(
        inner_owners[0].index(),
        inner.index() - 1,
        "the inner token immediately precedes the inner Match"
    );
    // The inner Match and its token sit before the outer Match in the
    // arena (emitted during the outer arm's body analysis).
    assert!(inner.index() < outer.index());
}

#[test]
fn diverging_inner_match_makes_the_statement_arm_never() {
    // A statement arm whose body ends in an all-returning inner match
    // diverges: the arm is `never`, no E0135, and an all-never outer
    // match types as never.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\tmatch o:\n\t\tOpt.Some(v):\n\t\t\tmatch v:\n\t\t\t\t0: return 0\n\t\t\t\t_: return 1\n\t\tOpt.None:\n\t\t\treturn 2\n";
    let (tirs, pool) = run(src).expect("diverging inner match compiles");
    let f = tir_named(&tirs, &pool, "f");
    let outer = match_stmt_operand(f, 0);
    assert_eq!(
        f.inst(outer).ty,
        pool.never(),
        "every arm diverges (inner all-return match included)"
    );
}
