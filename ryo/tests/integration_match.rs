mod common;
use common::*;

use tempfile::TempDir;

// =============================================================================
// M12 Pattern matching (Task 9 codegen) — JIT end-to-end tests. Enum tag
// dispatch with wildcard routing, per-arm payload bindings (the shared
// `bind_field_move` move logic), anon-scrutinee int/bool guard chains,
// scalar/void merges, and the arm-start drop of an unbound needs-drop
// payload field on a consuming arm. Aggregate-result matches and str
// literal guards land in later tasks; leaks on the heap-backed shapes are
// pinned under valgrind by the Task 12 smoke tests.
// =============================================================================

#[test]
fn match_unwrap_or_spec_example_both_variants_jit() {
    // The spec example: statement arms that both `return`, so the match
    // is never-typed and the merge block is unreachable (filled with a
    // trap; codegen continues in a dead block).
    assert_ryo_output(
        "match_unwrap_or",
        "enum Option:\n\tSome(value: int)\n\tNone\n\nfn unwrap_or(opt: Option, default: int) -> int:\n\tmatch opt:\n\t\tOption.Some(v):\n\t\t\treturn v\n\t\tOption.None:\n\t\t\treturn default\n\nfn main():\n\tprint(unwrap_or(Option.Some{value=5}, 0))\n\tprint(\"\\n\")\n\tprint(unwrap_or(Option.None, 9))\n\tprint(\"\\n\")\n",
        "5\n9\n",
    );
}

#[test]
fn match_value_producing_expr_arms_scalar_merge_jit() {
    // A value-producing match: expression arms evaluate and jump to the
    // merge block, which carries one scalar block param per predecessor.
    assert_ryo_output(
        "match_scalar_merge",
        "enum Option:\n\tSome(value: int)\n\tNone\n\nfn main():\n\ta = Option.Some{value=41}\n\tx = match a:\n\t\tOption.Some(v): v + 1\n\t\tOption.None: 0\n\tprint(x)\n\tprint(\"\\n\")\n\tb = Option.None\n\ty = match b:\n\t\tOption.Some(v): v + 1\n\t\tOption.None: -1\n\tprint(y)\n\tprint(\"\\n\")\n",
        "42\n-1\n",
    );
}

#[test]
fn match_all_three_payload_kinds_bind_and_print_jit() {
    // Unit variants dispatch by tag alone; tuple and named payloads bind
    // by position / by name (with `_` skips) and the bound locals print.
    assert_ryo_output(
        "match_payload_kinds",
        "enum Color:\n\tRed\n\tGreen\n\nenum Shape:\n\tCircle(float)\n\nenum Coords:\n\tOrigin\n\tPoint(x: int, y: int)\n\nfn main():\n\tc = Color.Green\n\tmatch c:\n\t\tColor.Red:\n\t\t\tprint(\"red\\n\")\n\t\tColor.Green:\n\t\t\tprint(\"green\\n\")\n\ts = Shape.Circle(2.5)\n\tmatch s:\n\t\tShape.Circle(r):\n\t\t\tprint(r)\n\t\t\tprint(\"\\n\")\n\tp = Coords.Point{x=3, y=4}\n\tmatch p:\n\t\tCoords.Point{x=px, y=_}:\n\t\t\tprint(px)\n\t\t\tprint(\"\\n\")\n\t\tCoords.Origin:\n\t\t\tprint(\"origin\\n\")\n\tq = Coords.Point{x=5, y=6}\n\tmatch q:\n\t\tCoords.Point{x=ax, y=ay}:\n\t\t\tprint(ax)\n\t\t\tprint(\":\")\n\t\t\tprint(ay)\n\t\t\tprint(\"\\n\")\n\t\tCoords.Origin:\n\t\t\tprint(\"origin\\n\")\n",
        "green\n2.5\n3\n5:6\n",
    );
}

#[test]
fn match_scrutinee_temporary_call_result_jit() {
    // The scrutinee is a call result: evaluated into its sret enum slot
    // before the tag dispatch; the slot address is the payload base.
    assert_ryo_output(
        "match_scrutinee_temp",
        "enum Option:\n\tSome(value: int)\n\tNone\n\nfn make_opt(n: int) -> Option:\n\treturn Option.Some{value=n}\n\nfn main():\n\tx = match make_opt(5):\n\t\tOption.Some(v): v * 2\n\t\tOption.None: 0\n\tprint(x)\n\tprint(\"\\n\")\n",
        "10\n",
    );
}

#[test]
fn match_statement_position_void_arms_print_jit() {
    // Statement-position match with void statement arms: no merge block
    // param, fall-through arms jump to the merge with no values.
    assert_ryo_output(
        "match_void_stmt",
        "enum Option:\n\tSome(value: int)\n\tNone\n\nfn main():\n\topt = Option.Some{value=7}\n\tmatch opt:\n\t\tOption.Some(v):\n\t\t\tprint(\"some \")\n\t\t\tprint(v)\n\t\t\tprint(\"\\n\")\n\t\tOption.None:\n\t\t\tprint(\"none\\n\")\n\tmatch Option.None:\n\t\tOption.Some(v):\n\t\t\tprint(\"some \")\n\t\t\tprint(v)\n\t\t\tprint(\"\\n\")\n\t\tOption.None:\n\t\t\tprint(\"none\\n\")\n",
        "some 7\nnone\n",
    );
}

#[test]
fn match_non_exhaustive_errors_e0129_jit() {
    // Missing `Green` (and no wildcard) is E0129; the driver surfaces it
    // on stderr and never reaches codegen.
    let temp_dir = TempDir::new().expect("temp");
    let code = "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tColor.Red:\n\t\t\tprint(\"red\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "match_non_exhaustive.ryo", code);
    let output = run_ryo_command(&["run", "match_non_exhaustive.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "a non-exhaustive match must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0129"), "stderr: {}", stderr);
}

#[test]
fn match_anon_scrutinee_int_bool_guard_chains_jit() {
    // Anon-struct scrutinee: each arm is a conjunction of field-equality
    // guards; a failed guard short-circuits to the next arm's test, and
    // the final wildcard arm is the fallthrough. Mixes pure-literal
    // guards, bind+guard arms, and a bool guard.
    assert_ryo_output(
        "match_anon_guards",
        "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t{x=1, y=2}:\n\t\t\tprint(\"both\\n\")\n\t\t{x=1, y=3}:\n\t\t\tprint(\"x-only\\n\")\n\t\t_:\n\t\t\tprint(\"none\\n\")\n\tq = {x=1, y=3}\n\tmatch q:\n\t\t{x=1, y=2}:\n\t\t\tprint(\"both\\n\")\n\t\t{x=1, y=3}:\n\t\t\tprint(\"x-only\\n\")\n\t\t_:\n\t\t\tprint(\"none\\n\")\n\tr = {x=9, y=2}\n\tmatch r:\n\t\t{x=px, y=2}:\n\t\t\tprint(px)\n\t\t\tprint(\"\\n\")\n\t\t_:\n\t\t\tprint(\"other\\n\")\n\tflag = {on=true, n=1}\n\tmatch flag:\n\t\t{on=true, n=_}:\n\t\t\tprint(\"on\\n\")\n\t\t{on=false, n=_}:\n\t\t\tprint(\"off\\n\")\n\t\t_:\n\t\t\tprint(\"other\\n\")\n",
        "both\nx-only\n9\non\n",
    );
}

#[test]
fn match_arm_start_drop_wildcard_payload_field_jit() {
    // `Two(a, _)` binds the first str by move (consuming the scrutinee)
    // and leaves the second payload field unbound: the ownership pass
    // registers a MatchArmDrop for it, emitted at the arm's start. The
    // payloads are runtime-built past the SSO inline cap so the drop is a
    // real ryo_str_free (leak pinning lands in the Task 12 valgrind
    // suite); this test pins the JIT output.
    assert_ryo_output(
        "match_arm_start_drop",
        "enum Duo:\n\tTwo(str, str)\n\tNone\n\nfn main():\n\tpad = \"0123456789012345678901234567890123456789\"\n\td = Duo.Two(\"first-\" + pad, \"second-\" + pad)\n\tmatch d:\n\t\tTwo(a, _):\n\t\t\tprint(a)\n\t\t\tprint(\"\\n\")\n\t\tNone:\n\t\t\tprint(\"none\\n\")\n",
        "first-0123456789012345678901234567890123456789\n",
    );
}
