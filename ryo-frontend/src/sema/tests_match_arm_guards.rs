//! Typed-arm guard and termination coverage: anon-arm literal guards
//! ride the arm guard list to the typed wire, non-anon arms carry
//! empty guard lists, and break/continue arms terminate the arm body
//! (a fallthrough assignment arm inside a loop stays E0135). Split out
//! of `tests_match.rs` to stay under the file-length limit.

use super::tests::*;
use super::tests_match::match_in_var_init;
use super::*;
use ryo_core::ast::LitPattern;
use ryo_core::tir::{Discriminant, MatchBinding, TirRef, TirTag};

// ---------- R12/R13 fix coverage: anon-arm guards, jump-terminated arms ----------

#[test]
fn anon_arm_literal_guards_reach_the_typed_wire() {
    // R12: `{x=0, y=py}:` — the literal constraint rides the arm's
    // guard list; the `y` bind rides the bindings. Both decode from the
    // typed Match view.
    let src = "fn main():\n\tp = {x=1, y=2}\n\tr = match p:\n\t\t{x=0, y=py}: py\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("guarded anon arm compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    assert_eq!(main.inst(m).ty, pool.int());
    let view = main.match_view(m);
    let arm0 = view.arm(0);
    assert_eq!(
        arm0.discriminant,
        Discriminant::Wildcard,
        "coverage placeholder kept orthogonal to the guards"
    );
    assert_eq!(
        arm0.guards().collect::<Vec<_>>(),
        [(0, LitPattern::Int(0))],
        "the literal field constraint reaches the wire"
    );
    let py = pool.find_str("py").expect("`py` interned");
    assert_eq!(
        arm0.bindings().collect::<Vec<_>>(),
        [MatchBinding {
            field_index: 1,
            name: py,
            ty: pool.int(),
        }],
        "the `y` bind coexists with the guard"
    );
    // The wildcard fallback arm carries no guards.
    assert_eq!(view.arm(1).guard_count(), 0);
    assert!(view.arm(1).guards().next().is_none());
}

#[test]
fn positional_anon_arm_guards_reach_the_typed_wire() {
    // `(0, _):` — tuple sugar; element 0 is the literal constraint.
    let src = "fn main():\n\tp = {x=1, y=2}\n\tr = match p:\n\t\t(0, _): 1\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("positional guarded arm compiles");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    let arm0 = view.arm(0);
    assert_eq!(arm0.guards().collect::<Vec<_>>(), [(0, LitPattern::Int(0))]);
    assert_eq!(arm0.binding_count(), 0);
}

#[test]
fn non_anon_arms_carry_empty_guard_lists() {
    // Variant and literal arms encode their matching entirely in the
    // discriminant — their guard lists are empty.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tr = match o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: 0\n";
    let (tirs, pool) = run(src).expect("variant arms compile");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    assert_eq!(view.arm(0).guard_count(), 0);
    assert_eq!(view.arm(1).guard_count(), 0);

    let src = "fn main():\n\tn = 5\n\tr = match n:\n\t\t0: 1\n\t\t_: 0\n";
    let (tirs, pool) = run(src).expect("literal arms compile");
    let main = tir_named(&tirs, &pool, "main");
    let m = match_in_var_init(main, 1);
    let view = main.match_view(m);
    assert_eq!(view.arm(0).guard_count(), 0);
    assert_eq!(view.arm(1).guard_count(), 0);
}

#[test]
fn break_arm_terminates_the_value_match_arm() {
    // R13: inside a loop, `Some(v): break` is a terminating arm — the
    // match types as int (never-compatible at the join), no E0135.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tmut x = 0\n\twhile true:\n\t\tx = match o:\n\t\t\tOpt.Some(v): break\n\t\t\tOpt.None: 0\n\t\tx += 1\n";
    let (tirs, pool) = run(src).expect("break arm is terminating");
    let main = tir_named(&tirs, &pool, "main");
    let while_stmt = stmt_at(main, 2);
    let while_body = main.while_loop_view(while_stmt).body;
    let assign = while_body.at(0);
    let m = main.assign_view(assign).value;
    assert_eq!(main.inst(m).ty, pool.int(), "break arm joins freely");
    let view = main.match_view(m);
    let arm0_stmts: Vec<TirRef> = view.arm(0).body.stmts().collect();
    assert!(
        matches!(main.inst(arm0_stmts[0]).tag, TirTag::Break),
        "the break arm body is the Break stmt, got {:?}",
        main.inst(arm0_stmts[0]).tag
    );
}

#[test]
fn continue_arm_terminates_the_value_match_arm() {
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tmut x = 0\n\twhile true:\n\t\tx = match o:\n\t\t\tOpt.Some(v): continue\n\t\t\tOpt.None: 0\n\t\tx += 1\n";
    let (tirs, pool) = run(src).expect("continue arm is terminating");
    let main = tir_named(&tirs, &pool, "main");
    let while_stmt = stmt_at(main, 2);
    let while_body = main.while_loop_view(while_stmt).body;
    let m = main.assign_view(while_body.at(0)).value;
    assert_eq!(main.inst(m).ty, pool.int());
}

#[test]
fn fallthrough_assignment_arm_in_loop_is_still_e0135() {
    // R13's flip side: a statement arm that can REALLY fall through is
    // still E0135 in a value match, loop or not.
    let src = "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tmut x = 0\n\twhile true:\n\t\tx = match o:\n\t\t\tOpt.Some(v): x = 5\n\t\t\tOpt.None: 0\n\t\tx += 1\n";
    let (_t, diags, _p) = run_with_errors(src);
    let diag = diags
        .iter()
        .find(|d| d.code == DiagCode::ArmNoValue)
        .expect("E0135 must still fire for the falling arm");
    assert!(
        diag.message.contains("'int'"),
        "message names the candidate type, got {:?}",
        diag.message
    );
    assert_eq!(
        count_code(&diags, DiagCode::ArmNoValue),
        1,
        "exactly one E0135: {diags:?}"
    );
}
