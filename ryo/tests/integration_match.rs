mod common;
use common::*;

use tempfile::TempDir;

// =============================================================================
// M12 Pattern matching (Task 9 + Task 10 codegen) — JIT end-to-end tests.
// Task 9: enum tag dispatch with wildcard routing, per-arm payload bindings
// (the shared `bind_field_move` move logic), anon-scrutinee int/bool guard
// chains, scalar/void merges, and the arm-start drop of an unbound
// needs-drop payload field on a consuming arm. Task 10: literal-arm chains
// on int/bool/str scrutinees (ordered `icmp_imm eq`/`ryo_str_eq` + brif,
// wildcard fallthrough, bool full coverage without a wildcard), str literal
// guards on anon arms, and the W0005/E0129 surfaces. Aggregate-result
// matches land in later tasks; leaks on the heap-backed shapes are pinned
// under valgrind by the Task 12 smoke tests.
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

// --- Task 10: literal scrutinee chains (int/bool/str) ---

#[test]
fn match_int_literal_arms_all_paths_jit() {
    // Ordered literal-arm chain over an int scrutinee: `icmp_imm eq` per
    // arm in source order, the wildcard the final fallthrough. The `42`
    // arm's body prints a str containing spaces and escaped quotes
    // EXACTLY (review pin); `-1` is pre-folded by the parser.
    assert_ryo_output(
        "match_int_literals",
        "fn describe(n: int):\n\tmatch n:\n\t\t0:\n\t\t\tprint(\"zero\\n\")\n\t\t42:\n\t\t\tprint(\"the \\\"answer\\\" is 42!\\n\")\n\t\t-1:\n\t\t\tprint(\"neg\\n\")\n\t\t_:\n\t\t\tprint(\"other\\n\")\n\nfn main():\n\tdescribe(0)\n\tdescribe(42)\n\tdescribe(-1)\n\tdescribe(7)\n",
        "zero\nthe \"answer\" is 42!\nneg\nother\n",
    );
}

#[test]
fn match_int_literal_arms_value_producing_scalar_merge_jit() {
    // Literal-arm chain in value-producing position: expression arms jump
    // to the merge block with the scalar result, exactly the Task 9 merge.
    assert_ryo_output(
        "match_int_literals_value",
        "fn code(n: int) -> int:\n\tx = match n:\n\t\t0: 100\n\t\t42: 200\n\t\t-1: 300\n\t\t_: -1\n\treturn x\n\nfn main():\n\tprint(code(0))\n\tprint(\"\\n\")\n\tprint(code(42))\n\tprint(\"\\n\")\n\tprint(code(-1))\n\tprint(\"\\n\")\n\tprint(code(7))\n\tprint(\"\\n\")\n",
        "100\n200\n300\n-1\n",
    );
}

#[test]
fn match_bool_full_coverage_without_wildcard_jit() {
    // A bool scrutinee covered by `true` + `false` alone is exhaustive
    // (sema E0129 passes without a wildcard); the chain tail after both
    // compares fails is unreachable and traps like the enum full-coverage
    // `otherwise` — never executed here.
    assert_ryo_output(
        "match_bool_no_wildcard",
        "fn b2i(b: bool) -> int:\n\tx = match b:\n\t\ttrue: 1\n\t\tfalse: 0\n\treturn x\n\nfn main():\n\tprint(b2i(true))\n\tprint(\"\\n\")\n\tprint(b2i(false))\n\tprint(\"\\n\")\n",
        "1\n0\n",
    );
}

#[test]
fn match_str_literal_arms_bytewise_compare_jit() {
    // Str scrutinee: each literal arm compares the scrutinee's extracted
    // (ptr, len) content pair against the literal's .rodata pair through
    // the runtime's bytewise `ryo_str_eq` (the M9.1 emit_field_eq
    // convention). The `"hi there\n"` arm pins bytewise correctness for a
    // literal containing a space AND an escape sequence; the near-miss
    // scrutinees (no trailing newline, longer literal) must NOT match.
    // `run("qu" + "it")` exercises the SSO-inline extraction path (the
    // concat result's bytes live in the slot, not behind the pointer).
    assert_ryo_output(
        "match_str_literals",
        "fn run(command: str):\n\tmatch command:\n\t\t\"quit\":\n\t\t\tprint(\"quitting\\n\")\n\t\t\"help\":\n\t\t\tprint(\"helping\\n\")\n\t\t\"hi there\\n\":\n\t\t\tprint(\"greeted\\n\")\n\t\t_:\n\t\t\tprint(\"unknown\\n\")\n\nfn main():\n\trun(\"quit\")\n\trun(\"help\")\n\trun(\"hi there\\n\")\n\trun(\"hi there\")\n\trun(\"hi there now\")\n\trun(\"qu\" + \"it\")\n",
        "quitting\nhelping\ngreeted\nunknown\nunknown\nquitting\n",
    );
}

#[test]
fn match_duplicate_literal_arm_first_wins_w0005_jit() {
    // A duplicate literal arm is W0005 at compile time (warning on
    // stderr, compilation succeeds); at runtime the FIRST arm claiming
    // the value wins and the dead duplicate's body never runs.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tn = 1\n\tmatch n:\n\t\t1:\n\t\t\tprint(\"first\\n\")\n\t\t1:\n\t\t\tprint(\"second\\n\")\n\t\t_:\n\t\t\tprint(\"other\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "match_dup_literal.ryo", code);
    let output = run_ryo_command(&["run", "match_dup_literal.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "a duplicate arm is a warning, not an error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "first\n", "stdout mismatch");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("W0005"), "stderr: {}", stderr);
}

#[test]
fn match_anon_scrutinee_str_literal_guards_jit() {
    // Anon-arm str literal guards: the field's fat triple loads from the
    // scrutinee slot (SSO-inline fields extract against the field's own
    // address) and compares bytewise against the literal through
    // `ryo_str_eq` — the M9.1 emit_field_eq convention. Covers an
    // inline (short) field, a heap (past-SSO-cap) field, and a
    // non-matching fallthrough.
    assert_ryo_output(
        "match_anon_str_guards",
        "fn main():\n\tmsg = {text=\"quit\", n=1}\n\tmatch msg:\n\t\t{text=\"quit\", n=_}:\n\t\t\tprint(\"quitting\\n\")\n\t\t{text=\"0123456789012345678901234567890123456789\", n=_}:\n\t\t\tprint(\"long\\n\")\n\t\t_:\n\t\t\tprint(\"other\\n\")\n\theap = {text=\"0123456789012345678901234567890123456789\", n=2}\n\tmatch heap:\n\t\t{text=\"quit\", n=_}:\n\t\t\tprint(\"quitting\\n\")\n\t\t{text=\"0123456789012345678901234567890123456789\", n=_}:\n\t\t\tprint(\"long\\n\")\n\t\t_:\n\t\t\tprint(\"other\\n\")\n\tnope = {text=\"bye\", n=3}\n\tmatch nope:\n\t\t{text=\"quit\", n=_}:\n\t\t\tprint(\"quitting\\n\")\n\t\t{text=\"0123456789012345678901234567890123456789\", n=_}:\n\t\t\tprint(\"long\\n\")\n\t\t_:\n\t\t\tprint(\"other\\n\")\n",
        "quitting\nlong\nother\n",
    );
}

#[test]
fn match_anon_guarded_arm_after_wildcard_is_well_formed_jit() {
    // A guarded arm AFTER the wildcard is W0005-dead, but its guard chain
    // still lowers (unreachable, well-formed blocks — the dead chain's
    // tail trap must receive the last compare's edge before being
    // sealed). The program compiles with a warning and the wildcard wins.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tp = {x=1, y=2}\n\tmatch p:\n\t\t{x=1, y=2}:\n\t\t\tprint(\"both\\n\")\n\t\t_:\n\t\t\tprint(\"wild\\n\")\n\t\t{x=3, y=_}:\n\t\t\tprint(\"dead\\n\")\n\tq = {x=9, y=9}\n\tmatch q:\n\t\t{x=1, y=2}:\n\t\t\tprint(\"both\\n\")\n\t\t_:\n\t\t\tprint(\"wild\\n\")\n\t\t{x=3, y=_}:\n\t\t\tprint(\"dead\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "match_anon_dead_guard.ryo", code);
    let output = run_ryo_command(&["run", "match_anon_dead_guard.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "a W0005-dead guarded arm must not abort codegen: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "both\nwild\n", "stdout mismatch");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("W0005"), "stderr: {}", stderr);
}

#[test]
fn match_str_non_exhaustive_errors_e0129_jit() {
    // All-literal str arms without a wildcard are E0129 (sema); the
    // driver surfaces it on stderr and never reaches codegen.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\ts = \"quit\"\n\tmatch s:\n\t\t\"quit\":\n\t\t\tprint(\"q\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "match_str_non_exhaustive.ryo", code);
    let output =
        run_ryo_command(&["run", "match_str_non_exhaustive.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "a non-exhaustive str match must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0129"), "stderr: {}", stderr);
}

#[test]
fn match_int_non_exhaustive_errors_e0129_jit() {
    // Literal int arms without a wildcard cannot enumerate int — E0129.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tn = 5\n\tmatch n:\n\t\t5:\n\t\t\tprint(\"five\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "match_int_non_exhaustive.ryo", code);
    let output =
        run_ryo_command(&["run", "match_int_non_exhaustive.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "a non-exhaustive int match must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0129"), "stderr: {}", stderr);
}
