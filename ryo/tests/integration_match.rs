mod common;
use common::*;

use tempfile::TempDir;

// =============================================================================
// M12 Pattern matching — JIT end-to-end tests, grouped by lowering area.
// Enum tag dispatch with wildcard routing, per-arm payload bindings
// (the shared `bind_field_move` move logic), anon-scrutinee int/bool guard
// chains, scalar/void merges, and the arm-start drop of an unbound
// needs-drop payload field on a consuming arm. Literal-arm chains
// on int/bool/str scrutinees (ordered `icmp_imm eq`/`ryo_str_eq` + brif,
// wildcard fallthrough, bool full coverage without a wildcard), str literal
// guards on anon arms, and the W0005/E0129 surfaces. Aggregate
// results (str/enum/struct) merge through a pre-allocated result slot each
// value arm writes via the canonical per-type aggregate writes; mixed
// never/value matches; all-never termination; discarded non-void matches;
// and the post-merge drop of an owned needs-drop scrutinee temporary.
// The negative sweep (E0131-E0136 each surface on stderr for a
// reproducing source; E0129 and W0005 are pinned by the literal-chain
// tests above) — leaks/double-frees on the heap-backed shapes are pinned
// under ASan/Valgrind by the asan_match_* / valgrind_match_* smoke tests.
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
    // registers a block-start DropOp for it, fired at the arm's start. The
    // payloads are runtime-built past the SSO inline cap so the drop is a
    // real ryo_str_free (leak pinning lands in the valgrind
    // suite); this test pins the JIT output.
    assert_ryo_output(
        "match_arm_start_drop",
        "enum Duo:\n\tTwo(str, str)\n\tNone\n\nfn main():\n\tpad = \"0123456789012345678901234567890123456789\"\n\td = Duo.Two(\"first-\" + pad, \"second-\" + pad)\n\tmatch d:\n\t\tTwo(a, _):\n\t\t\tprint(a)\n\t\t\tprint(\"\\n\")\n\t\tNone:\n\t\t\tprint(\"none\\n\")\n",
        "first-0123456789012345678901234567890123456789\n",
    );
}

// --- Literal scrutinee chains (int/bool/str) ---

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
    // to the merge block with the scalar result, exactly the enum-merge
    // shape.
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

// --- Aggregate results, termination, discarded values ---

#[test]
fn match_str_result_merge_both_ways_jit() {
    // A str-valued match merges through the pre-allocated result slot:
    // each value arm stores its fat triple into the slot (the canonical
    // fat-return write), the merge loads the triple back, and the bound
    // local prints. Both arms exercised.
    assert_ryo_output(
        "match_str_result",
        "fn pick(b: bool) -> str:\n\ts = match b:\n\t\ttrue: \"yes\"\n\t\tfalse: \"no\"\n\treturn s\n\nfn main():\n\tprint(pick(true))\n\tprint(\"\\n\")\n\tprint(pick(false))\n\tprint(\"\\n\")\n",
        "yes\nno\n",
    );
}

#[test]
fn match_enum_result_merge_debug_print_all_variants_jit() {
    // An enum-valued match: each arm constructs a fresh variant value in
    // its own slot, copies it tag + active payload into the result slot
    // (the `emit_enum_return` convention), and the merge yields the
    // result slot's address for the Debug print.
    assert_ryo_output(
        "match_enum_result",
        "enum Color:\n\tRed\n\tGreen\n\tBlue\n\nfn next(c: Color) -> Color:\n\tr = match c:\n\t\tColor.Red: Color.Green\n\t\tColor.Green: Color.Blue\n\t\tColor.Blue: Color.Red\n\treturn r\n\nfn main():\n\tprint(next(Color.Red))\n\tprint(\"\\n\")\n\tprint(next(Color.Green))\n\tprint(\"\\n\")\n\tprint(next(Color.Blue))\n\tprint(\"\\n\")\n",
        "Color.Green\nColor.Blue\nColor.Red\n",
    );
}

#[test]
fn match_anon_struct_result_merge_field_prints_jit() {
    // An anonymous-struct-valued match: field-wise copies into the result
    // slot (the `emit_struct_return` convention); the merge yields the
    // slot address and field reads print through it. Both arms exercised.
    assert_ryo_output(
        "match_struct_result",
        "fn point(b: bool) -> {x: int, y: int}:\n\treturn match b:\n\t\ttrue: {x=1, y=2}\n\t\tfalse: {x=3, y=4}\n\nfn main():\n\tp = point(true)\n\tprint(p.x)\n\tprint(\":\")\n\tprint(p.y)\n\tprint(\"\\n\")\n\tq = point(false)\n\tprint(q.x)\n\tprint(\":\")\n\tprint(q.y)\n\tprint(\"\\n\")\n",
        "1:2\n3:4\n",
    );
}

#[test]
fn match_mixed_never_value_scalar_result_used_jit() {
    // The dominance pin: a value arm jumps to the merge with the scalar
    // block param while the `return` arm leaves no merge edge. The merge
    // value must be the arm's value (not garbage from the diverging arm)
    // on the value path, and the function must return early on the
    // diverging path. Both paths asserted.
    assert_ryo_output(
        "match_mixed_never_value",
        "enum Option:\n\tSome(value: int)\n\tNone\n\nfn get(opt: Option) -> int:\n\tx = match opt:\n\t\tOption.Some(v): v\n\t\tOption.None: return -1\n\treturn x * 10\n\nfn main():\n\tprint(get(Option.Some{value=7}))\n\tprint(\"\\n\")\n\tprint(get(Option.None))\n\tprint(\"\\n\")\n",
        "70\n-1\n",
    );
}

#[test]
fn match_mixed_never_value_str_result_jit() {
    // Aggregate variant of the mixed shape: the diverging arm never
    // touches the result slot (non-value arms write nothing); the merge
    // reads the slot only on the paths that wrote it. Pins slot
    // reachability for the aggregate path specifically.
    assert_ryo_output(
        "match_mixed_never_str",
        "fn pick(b: bool) -> str:\n\ts = match b:\n\t\ttrue: \"yes\"\n\t\tfalse: return \"early\"\n\treturn s\n\nfn main():\n\tprint(pick(true))\n\tprint(\"\\n\")\n\tprint(pick(false))\n\tprint(\"\\n\")\n",
        "yes\nearly\n",
    );
}

#[test]
fn match_all_never_arms_merge_unreachable_code_after_compiles_jit() {
    // Every arm returns: the merge block is unreachable and trap-filled
    // (all-never matches lower the same way), and the dead statement
    // after the match must still
    // compile (codegen continues in a fresh dead block). The function
    // returns normally on every path and main proceeds.
    assert_ryo_output(
        "match_all_never",
        "enum Color:\n\tRed\n\tGreen\n\nfn die(c: Color):\n\tmatch c:\n\t\tColor.Red:\n\t\t\treturn\n\t\tColor.Green:\n\t\t\treturn\n\tprint(\"dead\\n\")\n\nfn main():\n\tdie(Color.Red)\n\tdie(Color.Green)\n\tprint(\"after\\n\")\n",
        "after\n",
    );
}

#[test]
fn match_discarded_nonvoid_str_result_no_w0001_jit() {
    // A non-void match used as a bare statement: it compiles and runs,
    // the result slot is simply never read, and no W0001 fires (W0001
    // binds only Move-typed BINDINGS today — I-191; a match binds
    // nothing). The arm literals are static (cap == 0), so no free
    // traffic is scheduled either.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\tb = true\n\tmatch b:\n\t\ttrue: \"yes\"\n\t\t_: \"no\"\n\tprint(\"done\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "match_discard_str.ryo", code);
    let output = run_ryo_command(&["run", "match_discard_str.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "a discarded non-void match must compile and run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "done\n", "stdout mismatch");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("W0001"),
        "a discarded match binds nothing — no W0001: {stderr}"
    );
}

#[test]
fn match_scrutinee_temp_enum_payload_drop_both_paths_jit() {
    // An OWNED TEMPORARY scrutinee with a heap str payload, no consuming
    // arm: the ownership pass schedules the whole-value drop anchored
    // after the match (the anon-temp pass; the Match inst is the
    // scrutinee's single consumer), and codegen's end-of-statement sweep
    // lands it post-merge — after the merge block, on every path that
    // reaches it. The >SSO-cap payload makes the free a real
    // ryo_str_free; the exactly-once pin under valgrind lands in the
    // sanitizer smoke suite.
    assert_ryo_output(
        "match_scrutinee_temp_drop",
        "enum Msg:\n\tText(body: str)\n\tQuit\n\nfn make_msg(n: int) -> Msg:\n\tif n > 0:\n\t\treturn Msg.Text{body=\"01234567890123456789012345678901234567890123456789\"}\n\treturn Msg.Quit\n\nfn main():\n\tmatch make_msg(1):\n\t\tMsg.Text(_):\n\t\t\tprint(\"text\\n\")\n\t\tMsg.Quit:\n\t\t\tprint(\"quit\\n\")\n\tmatch make_msg(0):\n\t\tMsg.Text(_):\n\t\t\tprint(\"text\\n\")\n\t\tMsg.Quit:\n\t\t\tprint(\"quit\\n\")\n",
        "text\nquit\n",
    );
}

#[test]
fn match_scrutinee_temp_str_heap_drop_jit() {
    // The literal-chain analogue: an owned str temporary from a call,
    // matched by content. The scrutinee's free fires post-merge through
    // the fat temp path (a real ryo_str_free for the heap arm).
    assert_ryo_output(
        "match_scrutinee_temp_str",
        "fn make_str(n: int) -> str:\n\tif n > 0:\n\t\treturn \"01234567890123456789012345678901234567890123456789\"\n\treturn \"x\"\n\nfn main():\n\tmatch make_str(1):\n\t\t\"01234567890123456789012345678901234567890123456789\":\n\t\t\tprint(\"long\\n\")\n\t\t_:\n\t\t\tprint(\"short\\n\")\n\tmatch make_str(0):\n\t\t\"01234567890123456789012345678901234567890123456789\":\n\t\t\tprint(\"long\\n\")\n\t\t_:\n\t\t\tprint(\"short\\n\")\n",
        "long\nshort\n",
    );
}

#[test]
fn match_return_string_payload_from_diverging_arm_jit() {
    // All arms return, so the match has no join. The returned string
    // must leave through its arm. Test inline and heap-backed strings.
    assert_ryo_output(
        "match_return_string_payload",
        "enum Message:\n\tText(str)\n\tEmpty\n\nfn take_short() -> str:\n\tm = Message.Text(\"short\")\n\tmatch m:\n\t\tMessage.Text(value):\n\t\t\treturn value\n\t\tMessage.Empty:\n\t\t\treturn \"empty\"\n\nfn take_heap() -> str:\n\tm = Message.Text(int_to_str(1) + \"-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz\")\n\tmatch m:\n\t\tMessage.Text(value):\n\t\t\treturn value\n\t\tMessage.Empty:\n\t\t\treturn \"empty\"\n\nfn length_short() -> int:\n\tm = Message.Text(\"short\")\n\treturn match m:\n\t\tMessage.Text(value): value.len()\n\t\tMessage.Empty: 0\n\nfn length_heap() -> int:\n\tm = Message.Text(int_to_str(1) + \"-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz\")\n\treturn match m:\n\t\tMessage.Text(value): value.len()\n\t\tMessage.Empty: 0\n\nfn main():\n\tprint(take_short())\n\tprint(\"\\n\")\n\tprint(take_heap())\n\tprint(\"\\n\")\n\tprint(length_short())\n\tprint(\"\\n\")\n\tprint(length_heap())\n\tprint(\"\\n\")\n",
        "short\n1-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnopqrstuvwxyz\n5\n55\n",
    );
}

#[test]
fn match_value_arm_heap_producer_transfers_jit() {
    // Arm-result transfer, the producer shape: a value arm that
    // PRODUCES an owned aggregate (a call) transfers its temporary into
    // the match result — the ownership pass Moves the producer and the
    // match inst becomes the single owner, so exactly one destruction
    // remains (the consumer's). The result prints and the program exits
    // normally (pre-fix: rejected at compile time; before that, silent
    // miscompile + SIGTRAP).
    assert_ryo_output(
        "match_arm_heap_producer",
        "fn make_str(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn pick(b: bool) -> str:\n\ts = match b:\n\t\ttrue: make_str(1)\n\t\tfalse: \"no\"\n\treturn s\n\nfn main():\n\tprint(pick(true))\n\tprint(\"\\n\")\n",
        "1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
}

#[test]
fn match_value_arm_binding_read_transfers_jit() {
    // Arm-result transfer, the by-value-read variant: an arm body that
    // reads a tracked binding moves the binding into the match result
    // (the binding's last-use drop is gone; the consumer's is the single
    // destruction). Pre-fix the aliasing read was rejected at compile
    // time (garbage stdout + SIGTRAP before that).
    assert_ryo_output(
        "match_arm_binding_read",
        "fn make_str(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn pick(b: bool) -> str:\n\tt = make_str(1)\n\ts = match b:\n\t\ttrue: t\n\t\tfalse: \"no\"\n\treturn s\n\nfn main():\n\tprint(pick(true))\n\tprint(\"\\n\")\n",
        "1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
}

// --- Review wave fixes: borrowed aggregate results (P1), panic-arm
// divergence (P2a/b), view results (P2c) ---

#[test]
fn match_borrowed_param_result_rejected_e0138_jit() {
    // P1 (memory-safety): the user's exact repro shape — a heap-backed
    // borrowed `str` parameter (concat-built past SSO) flows as an
    // expression-arm result, and the match result becomes an owned
    // binding whose cleanup frees caller-owned storage (observed at
    // HEAD: corrupted stdout + SIGTRAP). The ownership pass rejects the
    // borrowed-source family at compile time (E0138) — the backstop the
    // old codegen guard could never provide (a borrowed owner has no
    // scheduled drop).
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn pick(s: str, b: bool):\n\tx = match b:\n\t\ttrue: s\n\t\tfalse: \"no\"\n\tprint(x)\n\nfn main():\n\ts = \"0123456789012345678901234567890123456789\" + \"!\"\n\tpick(s, true)\n\tprint(s)\n";
    let test_file = create_test_file(temp_dir.path(), "match_borrowed_result.ryo", code);
    let output = run_ryo_command(&["run", "match_borrowed_result.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "a borrowed aggregate arm result must be rejected. stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for needle in ["E0138", "borrowed from the caller"] {
        assert!(
            stderr.contains(needle),
            "stderr must contain {needle}: {stderr}"
        );
    }
}

#[test]
fn match_borrowed_param_field_projection_rejected_e0138_jit() {
    // The general borrowed-source family: a field projection of a
    // borrowed parameter (`p.name`) aliases the caller's storage exactly
    // like a direct param read, so the same rejection fires.
    let temp_dir = TempDir::new().expect("temp");
    let code = "struct Person:\n\tname: str\n\tage: int\n\nfn pick(p: Person, b: bool):\n\tx = match b:\n\t\ttrue: p.name\n\t\tfalse: \"no\"\n\tprint(x)\n\nfn main():\n\tq = Person{name=\"0123456789012345678901234567890123456789\" + \"!\", age=3}\n\tpick(q, true)\n\tprint(q.name)\n";
    let test_file = create_test_file(temp_dir.path(), "match_borrowed_field.ryo", code);
    let output = run_ryo_command(&["run", "match_borrowed_field.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "a borrowed field-projection arm result must be rejected. stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("borrowed from the caller"),
        "the diagnosis must name the borrowed family: {stderr}"
    );
}

#[test]
fn match_direct_borrowed_assign_control_e0021_jit() {
    // Control: the direct-assignment shape (`x = s` on a borrowed param)
    // keeps its E0021 — the match rejection must not change it.
    assert_match_rejected(
        "match_borrowed_assign_control.ryo",
        "fn pick(s: str):\n\tx = s\n\tprint(x)\n\nfn main():\n\tpick(\"hi\")\n",
        "E0021",
    );
}

#[test]
fn match_owned_literal_result_both_arms_jit() {
    // Positive control: an OWNED aggregate through the same match shape
    // compiles and runs correctly — both arms exercised. Since the
    // arm-result transfer landed, heap-backed owned arms transfer into
    // the result too (see the transfer pins below); static-literal
    // arms exercise the same merge path at zero allocation cost.
    assert_ryo_output(
        "match_owned_result",
        "fn main():\n\tb = true\n\tx = match b:\n\t\ttrue: \"yes-owned\"\n\t\tfalse: \"no\"\n\tprint(x)\n\tprint(\"\\n\")\n\ty = match false:\n\t\ttrue: \"yes-owned\"\n\t\tfalse: \"no\"\n\tprint(y)\n\tprint(\"\\n\")\n",
        "yes-owned\nno\n",
    );
}

#[test]
fn match_panic_expr_arm_mixed_value_diverging_jit() {
    // P2a: an inline `panic(...)` expression arm joins as DIVERGING (no
    // value, like a `return` arm) instead of hitting E0017; the value
    // arm's merge value reaches the binding untouched.
    assert_ryo_output(
        "match_panic_expr_arm",
        "fn main():\n\tx = match true:\n\t\ttrue: 1\n\t\tfalse: panic(\"no\")\n\tprint(x)\n\tprint(\"\\n\")\n",
        "1\n",
    );
}

#[test]
fn match_panic_expr_arm_taken_panics_jit() {
    // P2a runtime: when the diverging arm IS taken, the panic fires with
    // its message and location (the arm body evaluated for effect, no
    // merge edge, and the process exits 101).
    let temp_dir = TempDir::new().expect("temp");
    let code =
        "fn main():\n\tx = match false:\n\t\ttrue: 1\n\t\tfalse: panic(\"no\")\n\tprint(x)\n";
    let test_file = create_test_file(temp_dir.path(), "match_panic_taken.ryo", code);
    let output = run_ryo_command(&["run", "match_panic_taken.ryo"], &test_file).expect("run");
    assert_eq!(
        output.status.code(),
        Some(101),
        "the taken panic arm must abort. stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("panicked"),
        "stderr should contain the panic message: {stderr}"
    );
}

#[test]
fn match_all_diverging_panic_and_return_arms_jit() {
    // All-diverging arms: `panic(...)` and `return` in one match — the
    // join is `never`, the merge is unreachable, and the returning arm
    // hands control back normally.
    assert_ryo_output(
        "match_all_diverging",
        "fn f(b: bool) -> int:\n\tmatch b:\n\t\ttrue: panic(\"a\")\n\t\tfalse: return 7\n\nfn main():\n\tprint(f(false))\n\tprint(\"\\n\")\n",
        "7\n",
    );
}

#[test]
fn match_panic_block_arm_value_match_jit() {
    // P2b: the block-arm form of the diverging arm — a statement arm
    // whose trailing `panic(...)` ExprStmt diverges — behaves exactly
    // like the inline P2a form (the divergence rides the same
    // stmt_terminates sense sema used to classify the arm).
    assert_ryo_output(
        "match_panic_block_arm",
        "fn main():\n\tx = match true:\n\t\ttrue: 1\n\t\tfalse:\n\t\t\tpanic(\"no\")\n\tprint(x)\n\tprint(\"\\n\")\n",
        "1\n",
    );
}

#[test]
fn match_panic_block_arm_str_result_jit() {
    // P2b in an aggregate-result match: the diverging block arm leaves
    // the result slot untouched on its (never-taken) path; the value arm
    // writes it and the merge reads it back.
    assert_ryo_output(
        "match_panic_block_str",
        "fn pick(b: bool) -> str:\n\ts = match b:\n\t\ttrue: \"yes\"\n\t\tfalse:\n\t\t\tpanic(\"no\")\n\treturn s\n\nfn main():\n\tprint(pick(true))\n\tprint(\"\\n\")\n",
        "yes\n",
    );
}

#[test]
fn match_in_elif_condition_executes_that_arm_jit() {
    // A match in an elif condition spans condition-entry, arm, and join
    // blocks; the elif's Branch holds in the match join, so the arm
    // blocks are recorded at build time (reconstructing them from the
    // condition entry panicked with "elif block must hold a Branch, got
    // Switch"). The taken match arm selects the elif body.
    assert_ryo_output(
        "match_elif_condition",
        "fn main():\n\tif false:\n\t\tprint(\"first\")\n\telif (match true:\n\t\ttrue: true\n\t\tfalse: false\n\t):\n\t\tprint(\"second\")\n\telse:\n\t\tprint(\"third\")\n",
        "second",
    );
}

#[test]
fn match_in_elif_condition_falls_to_else_jit() {
    // The same shape with a false-taking match: the else chain continues
    // past the match-elif to the else arm.
    assert_ryo_output(
        "match_elif_condition_else",
        "fn main():\n\tif false:\n\t\tprint(\"first\")\n\telif (match false:\n\t\ttrue: true\n\t\tfalse: false\n\t):\n\t\tprint(\"second\")\n\telse:\n\t\tprint(\"third\")\n",
        "third",
    );
}

#[test]
fn match_in_elif_condition_after_simple_elif_jit() {
    // Mixed chain: a simple elif, then a match elif — the recorded arm
    // blocks must not confuse the two.
    assert_ryo_output(
        "match_elif_mixed_chain",
        "fn main():\n\tif true:\n\t\tprint(\"first\")\n\telif false:\n\t\tprint(\"skipped\")\n\telif (match true:\n\t\ttrue: true\n\t\tfalse: false\n\t):\n\t\tprint(\"second\")\n\telse:\n\t\tprint(\"third\")\n",
        "first",
    );
}

#[test]
fn match_view_result_rejected_e0137_jit() {
    // P2c: view-valued match results pass the E0134 join (both arms
    // agree on strview) but no merge shape exists for a view — sema
    // rejects them loudly instead of failing in eval_inst_view.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\ts = \"hello\"\n\tv = match true:\n\t\ttrue: s[0:2]\n\t\tfalse: s[2:4]\n\tprint(v)\n";
    let test_file = create_test_file(temp_dir.path(), "match_view_result.ryo", code);
    let output = run_ryo_command(&["run", "match_view_result.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "a view-typed match result must be rejected. stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for needle in ["E0137", "not yet supported"] {
        assert!(
            stderr.contains(needle),
            "stderr must contain {needle}: {stderr}"
        );
    }
}
// E0129 (three non-exhaustive shapes) and W0005 (duplicate-arm warning
// with successful compile) are pinned by the literal-chain tests above;
// each
// remaining code gets one focused test here with a minimal reproducing
// source (the shapes mirror ryo-frontend's sema/tests_match.rs).

/// Compile `code` and assert failure with `code_str` on stderr — the
/// negative-sweep shared shape (the E0129 tests above inline it).
fn assert_match_rejected(file_name: &str, code: &str, code_str: &str) {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), file_name, code);
    let output = run_ryo_command(&["run", file_name], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "{code_str} must be rejected. stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(code_str),
        "stderr must contain {code_str}: {stderr}"
    );
}

#[test]
fn match_literal_kind_mismatch_errors_e0131_jit() {
    // A string literal arm against an int scrutinee names both sides.
    assert_match_rejected(
        "match_e0131.ryo",
        "fn main():\n\tmatch 5:\n\t\t\"s\": 1\n\t\t_: 0\n",
        "E0131",
    );
}

#[test]
fn match_unknown_variant_errors_e0132_jit() {
    // `Orange` is not a Color variant — E0132 names the identifier and
    // the scrutinee enum.
    assert_match_rejected(
        "match_e0132.ryo",
        "enum Color:\n\tRed\n\tGreen\n\nfn main():\n\tc = Color.Red\n\tmatch c:\n\t\tOrange: 1\n\t\t_: 0\n",
        "E0132",
    );
}

#[test]
fn match_move_out_of_borrowed_scrutinee_errors_e0133_jit() {
    // `match &o:` (borrowed scrutinee) with a by-move str payload bind —
    // E0133 names the binding and its type.
    assert_match_rejected(
        "match_e0133.ryo",
        "enum Opt:\n\tSome(value: str)\n\tNone\n\nfn main():\n\to = Opt.Some{value=\"hi\"}\n\tmatch &o:\n\t\tOpt.Some(v): print(v)\n\t\tOpt.None: print(\"none\")\n",
        "E0133",
    );
}

#[test]
fn match_arm_type_mismatch_errors_e0134_jit() {
    // Arms yielding `int` and `str` in one match — E0134 names both arm
    // types, anchored on the offending body.
    assert_match_rejected(
        "match_e0134.ryo",
        "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=5}\n\tx = match o:\n\t\tOpt.Some(v): v\n\t\tOpt.None: \"s\"\n",
        "E0134",
    );
}

#[test]
fn match_falling_statement_arm_in_value_match_errors_e0135_jit() {
    // `Some(v): x = 5` falls through while the sibling arm produces a
    // value — E0135 at the falling arm.
    assert_match_rejected(
        "match_e0135.ryo",
        "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\tmut x = 0\n\to = Opt.Some{value=5}\n\tmatch o:\n\t\tOpt.Some(v): x = 5\n\t\tOpt.None: 0\n",
        "E0135",
    );
}

#[test]
fn match_void_scrutinee_errors_e0136_jit() {
    // A void-returning call produces no value to match on — E0136 names
    // the scrutinee type and lists what is matchable.
    assert_match_rejected(
        "match_e0136.ryo",
        "fn void_fn():\n\tprint(\"hi\")\n\nfn main():\n\tmatch void_fn():\n\t\t_: 0\n",
        "E0136",
    );
}

// --- Review wave 2 (post-10f1c3f findings): traversal consistency ---

#[test]
fn match_value_position_sibling_view_arm_mutation_rejected_e0035_jit() {
    // P1: a match in VALUE position (a VarDecl initializer) must get the
    // scoped per-arm liveness treatment: the flat fallback left the
    // match's arm read maps empty, so arm refinement read "no reads in
    // this arm" and removed the projection — the arm's reassignment of
    // `s` then freed the buffer `print(v)` still read (corrupted output,
    // no diagnostic). The reassignment must be rejected with E0035.
    assert_match_rejected(
        "match_value_pos_view_sibling.ryo",
        "fn main():\n\tmut s = \"0123456789012345678901234567890123456789\" + \"!\"\n\tv = s[0:3]\n\tx = match true:\n\t\ttrue:\n\t\t\ts = \"changed\"\n\t\t\tprint(v)\n\t\t\treturn\n\t\tfalse: v.len()\n\tprint(x)\n",
        "E0035",
    );
}

#[test]
fn match_value_position_arm_local_view_dead_arm_still_compiles_jit() {
    // Positive control for the same path: the sibling-arm read is the
    // view's global last use, and the arm that mutates nothing is
    // genuinely read-free — the scoped kill still fires (maps present),
    // the program compiles, and the join reads the result normally.
    assert_ryo_output(
        "match_value_pos_view_dead_arm",
        "fn main():\n\ts = \"0123456789012345678901234567890123456789\" + \"!\"\n\tv = s[0:3]\n\tx = match true:\n\t\ttrue: 1\n\t\tfalse: v.len()\n\tprint(x)\n\tprint(\"\\n\")\n",
        "1\n",
    );
}

#[test]
fn match_arm_inout_realloc_runs_clean_jit() {
    // P1: an inout call on an arm-local heap str reallocates through the
    // home slot; the arm-end free must resolve the binding's CURRENT
    // storage. Pre-fix the free-binding tables never saw the arm's
    // VarDecl, cleanup freed the stale cached triple, and the program
    // crashed after printing (double free; debug, release, and AOT).
    assert_ryo_output(
        "match_arm_inout_realloc",
        "fn replace(inout s: str):\n\ts = \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\" + \"!\"\n\nfn main():\n\tmatch true:\n\t\ttrue:\n\t\t\tmut s = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" + \"!\"\n\t\t\treplace(&s)\n\t\t\tprint(s)\n\t\t\tprint(\"\\n\")\n\t\tfalse:\n\t\t\tprint(\"no\\n\")\n",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb!\n",
    );
}

#[test]
fn match_payload_yield_transfers_jit() {
    // Arm-result transfer, the payload-yield shape: yielding a by-move
    // payload binding as an aggregate match result (`Some(v): v`)
    // transfers the payload into the match result — the token's
    // arm-anchored drop disappears and the consumer's last-use Free is
    // the single destruction. The 40+1-char heap payload prints exactly
    // once (pre-fix: E0139; before that, debug aborted on the dropped
    // free and release miscompiled inside Cranelift's egraph).
    assert_ryo_output(
        "match_payload_yield_transfer",
        "enum Opt:\n\tSome(str)\n\tNone\n\nfn main():\n\to = Opt.Some(\"0123456789012345678901234567890123456789\" + \"!\")\n\tx = match o:\n\t\tSome(v): v\n\t\tNone: \"none\"\n\tprint(x)\n",
        "0123456789012345678901234567890123456789!",
    );
}

#[test]
fn match_payload_yield_static_transfers_jit() {
    // The static-payload variant transfers the same way (the
    // destruction-once expectation holds for inline payloads too).
    assert_ryo_output(
        "match_payload_yield_static_transfer",
        "enum Opt:\n\tSome(str)\n\tNone\n\nfn main():\n\to = Opt.Some(\"abc\")\n\tx = match o:\n\t\tSome(v): v\n\t\tNone: \"none\"\n\tprint(x)\n",
        "abc",
    );
}

#[test]
fn match_payload_consumed_into_producer_transfers_jit() {
    // The producer family: a payload consumed into a fresh producer
    // (`v + "?"`) roots at that producer, which the transfer Moves into
    // the match result — exactly one destruction remains and the
    // concatenated payload prints (pre-fix: rejected at compile time by
    // the codegen guard via the producer temp's scheduled drop).
    assert_ryo_output(
        "match_payload_producer_transfer",
        "enum Opt:\n\tSome(str)\n\tNone\n\nfn main():\n\to = Opt.Some(\"0123456789012345678901234567890123456789\" + \"!\")\n\tx = match o:\n\t\tSome(v): v + \"?\"\n\t\tNone: \"none\"\n\tprint(x)\n",
        "0123456789012345678901234567890123456789!?",
    );
}

// =============================================================================
// AOT coverage for the review-wave regression shapes (the JIT pins live
// above; these assert the same behavior through `ryo build` + the
// compiled binary) and the placement/exits/shadowing/inout matrix.
// =============================================================================

use std::process::Command;

/// Build `code` with `ryo build`, assert success, run the binary, and
/// assert its stdout equals `expected`.
fn assert_match_aot_output(test_name: &str, code: &str, expected: &str) {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), test_name, code);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build.status.success(),
        "ryo build failed for {test_name}. STDERR: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(exe_path(
        temp_dir.path(),
        test_name.trim_end_matches(".ryo"),
    ))
    .output()
    .expect("run binary");
    assert!(
        run.status.success(),
        "binary {test_name} exited nonzero. stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(stdout, expected, "stdout mismatch for {test_name}");
}

/// Build `code` with `ryo build` and assert rejection with `needle` on
/// stderr — the AOT-side pin of the JIT rejection tests.
fn assert_match_aot_rejected(test_name: &str, code: &str, needle: &str) {
    let temp_dir = TempDir::new().expect("temp");
    let test_file = create_test_file(temp_dir.path(), test_name, code);
    let build = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        !build.status.success(),
        "{test_name} must be rejected by ryo build. stdout: {}",
        String::from_utf8_lossy(&build.stdout)
    );
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(
        stderr.contains(needle),
        "build stderr must contain {needle}: {stderr}"
    );
}

// Source shapes shared with the JIT regression pins above.

const VALUE_POS_VIEW_SIBLING: &str = "fn main():\n\tmut s = \"0123456789012345678901234567890123456789\" + \"!\"\n\tv = s[0:3]\n\tx = match true:\n\t\ttrue:\n\t\t\ts = \"changed\"\n\t\t\tprint(v)\n\t\t\treturn\n\t\tfalse: v.len()\n\tprint(x)\n";

const ARM_INOUT_REALLOC: &str = "fn replace(inout s: str):\n\ts = \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\" + \"!\"\n\nfn main():\n\tmatch true:\n\t\ttrue:\n\t\t\tmut s = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" + \"!\"\n\t\t\treplace(&s)\n\t\t\tprint(s)\n\t\t\tprint(\"\\n\")\n\t\tfalse:\n\t\t\tprint(\"no\\n\")\n";

const PAYLOAD_YIELD: &str = "enum Opt:\n\tSome(str)\n\tNone\n\nfn main():\n\to = Opt.Some(\"0123456789012345678901234567890123456789\" + \"!\")\n\tx = match o:\n\t\tSome(v): v\n\t\tNone: \"none\"\n\tprint(x)\n";

#[test]
fn match_value_position_sibling_view_arm_mutation_rejected_e0035_aot() {
    // AOT pin of the value-position-match E0035 regression: the build
    // must fail with the same diagnostic the JIT pin asserts.
    assert_match_aot_rejected(
        "match_value_pos_view_sibling_aot.ryo",
        VALUE_POS_VIEW_SIBLING,
        "E0035",
    );
}

#[test]
fn match_arm_inout_realloc_runs_clean_aot() {
    // AOT pin of the arm-local inout double-free regression: the
    // compiled binary must print the replacement and exit normally
    // (the cleanup decisions are consumed identically on the AOT path).
    assert_match_aot_output(
        "match_arm_inout_realloc_aot.ryo",
        ARM_INOUT_REALLOC,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb!\n",
    );
}

#[test]
fn match_payload_yield_transfers_aot() {
    // AOT pin of the arm-result transfer: `ryo build` + the compiled binary
    // print the payload exactly once and exit 0 — the cleanup schedule
    // the JIT pins is consumed identically on the AOT path.
    assert_match_aot_output(
        "match_payload_yield_aot.ryo",
        PAYLOAD_YIELD,
        "0123456789012345678901234567890123456789!",
    );
}

#[test]
fn match_value_arm_heap_producer_transfers_aot() {
    // AOT pin of the producer-arm transfer (the heap producer's
    // temporary is Moved into the match result; the binary prints the
    // produced string and exits normally).
    assert_match_aot_output(
        "match_arm_heap_producer_aot.ryo",
        "fn make_str(tag: int) -> str:\n\treturn int_to_str(tag) + \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\nfn pick(b: bool) -> str:\n\ts = match b:\n\t\ttrue: make_str(1)\n\t\tfalse: \"no\"\n\treturn s\n\nfn main():\n\tprint(pick(true))\n\tprint(\"\\n\")\n",
        "1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    );
}

// --- Placement matrix: every operand position, exits, shadowing ---

#[test]
fn match_in_call_argument_position_int_payload_jit() {
    // A value match nested in a call argument: the join feeds the
    // argument marshal directly.
    assert_ryo_output(
        "match_call_arg",
        "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\to = Opt.Some{value=41}\n\tprint(match o:\n\t\tOpt.Some(v): v + 1\n\t\tOpt.None: 0\n\t)\n\tprint(\"\\n\")\n",
        "42\n",
    );
}

#[test]
fn match_in_return_operand_int_payload_jit() {
    // A value match as the return operand: the join feeds the return
    // value, and the scrutinee temporary is destroyed on both paths.
    assert_ryo_output(
        "match_return_operand",
        "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn f(o: Opt) -> int:\n\treturn match o:\n\t\tOpt.Some(v): v * 2\n\t\tOpt.None: -1\n\nfn main():\n\tprint(f(Opt.Some{value=21}))\n\tprint(\"\\n\")\n\tprint(f(Opt.None))\n\tprint(\"\\n\")\n",
        "42\n-1\n",
    );
}

#[test]
fn match_nested_value_matches_join_jit() {
    // The scrutinee of a value match is itself a value match: two
    // joins chain, both temporaries destroyed once each.
    assert_ryo_output(
        "match_nested_value",
        "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn main():\n\tinner = Opt.Some{value=1}\n\touter = Opt.Some{value=2}\n\tx = match outer:\n\t\tOpt.Some(a): match inner:\n\t\t\tOpt.Some(b): a + b\n\t\t\tOpt.None: a\n\t\tOpt.None: 0\n\tprint(x)\n\tprint(\"\\n\")\n",
        "3\n",
    );
}

#[test]
fn match_arm_shadows_outer_binding_inout_is_arm_local_jit() {
    // An arm-local `mut s` shadows an outer `s`; the inout call must
    // reseat ONLY the arm-local place (place identity, not name), and
    // the outer binding must survive the match untouched.
    assert_ryo_output(
        "match_arm_shadow_inout",
        "fn replace(inout s: str):\n\ts = \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\" + \"!\"\n\nfn main():\n\touter = \"oooooooooooooooooooooooooooooooooooooooo\" + \"!\"\n\tmatch true:\n\t\ttrue:\n\t\t\tmut outer = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" + \"!\"\n\t\t\treplace(&outer)\n\t\t\tprint(outer)\n\t\t\tprint(\"\\n\")\n\t\tfalse:\n\t\t\tprint(\"no\\n\")\n\tprint(outer)\n\tprint(\"\\n\")\n",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb!\noooooooooooooooooooooooooooooooooooooooo!\n",
    );
}

#[test]
fn match_arm_reassign_after_view_last_use_ok_jit() {
    // Positive control for the E0035 family: the reassignment lands
    // AFTER the view's last use, so the freeze has lifted — the
    // program compiles and prints the post-arm binding on every path.
    assert_ryo_output(
        "match_arm_reassign_after_last_use",
        "fn main():\n\tmut s = \"0123456789012345678901234567890123456789\" + \"!\"\n\tmut x = 0\n\tmatch true:\n\t\ttrue:\n\t\t\tv = s[0:3]\n\t\t\tprint(v.len())\n\t\t\tprint(\"\\n\")\n\t\t\ts = \"changed\"\n\t\t\tx = 1\n\t\tfalse:\n\t\t\tx = 0\n\tprint(x)\n\tprint(\"\\n\")\n\tprint(s)\n\tprint(\"\\n\")\n",
        "3\n1\nchanged\n",
    );
}

#[test]
fn match_statement_break_in_loop_arm_owner_cleanup_jit() {
    // A statement match inside a loop with a `break` in one arm and a
    // heap str last-used in the other: the break path must not leak
    // (loop-exit free) and must not double-free (arm-end free deduped).
    assert_ryo_output(
        "match_break_loop_owner",
        "fn main():\n\tmut i = 0\n\ts = \"loopowner\" + \"!\"\n\twhile true:\n\t\tmatch i:\n\t\t\t0:\n\t\t\t\tprint(s)\n\t\t\t\tprint(\"\\n\")\n\t\t\t\ti = i + 1\n\t\t\t_:\n\t\t\t\tbreak\n\tprint(\"done\\n\")\n",
        "loopowner!\ndone\n",
    );
}

#[test]
fn match_in_while_condition_loop_until_arm_value_changes_jit() {
    // A value match as the while CONDITION: the header's test is the
    // match join (the Switch lives in the header block, the WhileTest in
    // the condition's join), and the loop runs until an arm's selected
    // value flips — here `st` changing to `State.Halt` routes the match
    // to the `false` arm and the loop exits. Pins match-in-condition
    // lowering plus the backedge-to-Switch-header shape end to end.
    assert_ryo_output(
        "match_while_condition",
        "enum State:\n\tGo\n\tHalt\n\nfn main():\n\tmut st = State.Go\n\tmut i = 0\n\twhile (match st:\n\t\tState.Go: true\n\t\tState.Halt: false\n\t):\n\t\ti += 1\n\t\tif i >= 2:\n\t\t\tst = State.Halt\n\tprint(\"done\")\n\tprint(\"\\n\")\n",
        "done\n",
    );
}

#[test]
fn match_arm_break_and_continue_exit_to_loop_jit() {
    // Both loop-control exits inside match arms nested in a loop: the
    // `continue` arm jumps to the loop header, the `break` arm to the
    // loop exit, and the falling-through arm prints. The arm blocks
    // terminate in Continue/Break (no merge edge from them), and the
    // remaining arm's merge still yields normally.
    assert_ryo_output(
        "match_arm_break_continue",
        "fn main():\n\tmut i = 0\n\twhile true:\n\t\ti += 1\n\t\tmatch i:\n\t\t\t2:\n\t\t\t\tcontinue\n\t\t\t4:\n\t\t\t\tbreak\n\t\t\t_:\n\t\t\t\tprint(i)\n\t\t\t\tprint(\"\\n\")\n\tprint(\"done\\n\")\n",
        "1\n3\ndone\n",
    );
}

#[test]
fn match_view_read_in_arm_after_sibling_payload_move_jit() {
    // View protection across arms: `v` (a projection of the heap-built
    // owner `s`) is created BEFORE the match; one arm binds the
    // scrutinee's payload by move (`Some(x)` consumes `o`), the sibling
    // arm and the post-merge code read `v`. The by-move payload bind in
    // one arm must not disturb the pre-existing projection's liveness in
    // the other arm: every read of `v` — inside the moving arm, inside
    // the sibling arm, and after the merge — sees `s`'s buffer.
    assert_ryo_output(
        "match_view_across_arms",
        "enum Opt:\n\tSome(str)\n\tNone\n\nfn main():\n\tpad = \"0123456789012345678901234567890123456789\"\n\ts = \"base-\" + pad\n\tv = s[0:5]\n\to = Opt.Some(\"pay-\" + pad)\n\tmatch o:\n\t\tSome(x):\n\t\t\tprint(x)\n\t\t\tprint(\"\\n\")\n\t\t\tprint(v)\n\t\t\tprint(\"\\n\")\n\t\tNone:\n\t\t\tprint(v)\n\t\t\tprint(\"\\n\")\n\tprint(v)\n\tprint(\"\\n\")\n",
        "pay-0123456789012345678901234567890123456789\nbase-\nbase-\n",
    );
}

#[test]
fn match_arm_reassign_outer_binding_post_merge_value_jit() {
    // Arm bodies and outer-binding places. (1) An arm-body assignment
    // with no arm-local declaration resolves through the arm scope to
    // the OUTER binding: the reassignment is visible after the merge —
    // the post-merge value must be the arm's. (2) An arm-local `mut y`
    // shadows the outer binding INSIDE the arm only: reassigning the
    // shadow leaves the outer place untouched, and the merge reads the
    // outer binding the arm assigned (place identity, never name).
    assert_ryo_output(
        "match_arm_reassign_outer",
        "fn main():\n\tmut x = 0\n\tmatch true:\n\t\ttrue:\n\t\t\tx = 5\n\t\tfalse:\n\t\t\tx = 9\n\tprint(x)\n\tprint(\"\\n\")\n\tmut y = 0\n\tmatch false:\n\t\ttrue:\n\t\t\tmut y = 100\n\t\t\ty = 200\n\t\tfalse:\n\t\t\ty = 7\n\tprint(y)\n\tprint(\"\\n\")\n",
        "5\n7\n",
    );
}

#[test]
fn match_two_matches_in_one_call_arguments_jit() {
    // Nested matches simultaneously in an initializer and an argument
    // of one call: the outer match is the initializer of `x`, and each
    // arm body is a call whose second argument is itself a match. Both
    // matches cut their own arm/join blocks; the inner merge feeds the
    // call's argument marshal from inside the outer arm.
    assert_ryo_output(
        "match_two_matches_one_call",
        "enum Opt:\n\tSome(value: int)\n\tNone\n\nfn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main():\n\tp = Opt.Some{value=1}\n\tq = Opt.None\n\tx = match p:\n\t\tOpt.Some(v): add(v, match q:\n\t\t\tOpt.Some(w): w\n\t\t\tOpt.None: 20\n\t\t)\n\t\tOpt.None: add(10, match q:\n\t\t\tOpt.Some(w): w\n\t\t\tOpt.None: 20\n\t\t)\n\tprint(x)\n\tprint(\"\\n\")\n",
        "21\n",
    );
}

// --- AOT pins for the new control-flow shapes above ---

#[test]
fn match_in_while_condition_loop_until_arm_value_changes_aot() {
    // AOT pin of the match-in-while-condition shape: the compiled binary
    // must exit the loop when the arm selection flips, exactly like the
    // JIT pin.
    assert_match_aot_output(
        "match_while_condition_aot.ryo",
        "enum State:\n\tGo\n\tHalt\n\nfn main():\n\tmut st = State.Go\n\tmut i = 0\n\twhile (match st:\n\t\tState.Go: true\n\t\tState.Halt: false\n\t):\n\t\ti += 1\n\t\tif i >= 2:\n\t\t\tst = State.Halt\n\tprint(\"done\")\n\tprint(\"\\n\")\n",
        "done\n",
    );
}

#[test]
fn match_arm_break_and_continue_exit_to_loop_aot() {
    // AOT pin of the break/continue-in-arms shape: the compiled binary
    // takes the continue arm, the falling-through arm, and the break arm
    // identically to the JIT pin.
    assert_match_aot_output(
        "match_arm_break_continue_aot.ryo",
        "fn main():\n\tmut i = 0\n\twhile true:\n\t\ti += 1\n\t\tmatch i:\n\t\t\t2:\n\t\t\t\tcontinue\n\t\t\t4:\n\t\t\t\tbreak\n\t\t\t_:\n\t\t\t\tprint(i)\n\t\t\t\tprint(\"\\n\")\n\tprint(\"done\\n\")\n",
        "1\n3\ndone\n",
    );
}
