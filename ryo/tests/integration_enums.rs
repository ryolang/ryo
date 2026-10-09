mod common;
use common::*;

use tempfile::TempDir;

// =============================================================================
// M11 Enums — JIT end-to-end tests. An enum value is one whole-value owner
// (like a struct): construction, move, drop, Debug print, and derived
// equality all run through the same ownership pass rules. These tests pin
// the observable behavior; the Valgrind suite (Task 11) runs the
// heap-backed cases under leak detection.
// =============================================================================

#[test]
fn enum_all_three_shapes_construct_and_print_jit() {
    // Unit, tuple, and named variants construct and print through the
    // Debug repr: `EnumName.Variant` for unit, `(v, v)` for tuple
    // payloads, `{f=v, f=v}` for named payloads. str payloads render
    // quoted.
    assert_ryo_output(
        "enum_shapes_print",
        "enum Color:\n\tRed\n\tGreen\n\nenum Result:\n\tSuccess(int)\n\tError(message: str)\n\nenum Shape:\n\tCircle(float)\n\tRectangle(width: float, height: float)\n\nfn main():\n\tprint(Color.Red)\n\tprint(\"\\n\")\n\tprint(Result.Success(5))\n\tprint(\"\\n\")\n\tprint(Result.Error{message=\"boom\"})\n\tprint(\"\\n\")\n\tprint(Shape.Rectangle{width=1.0, height=2.0})\n\tprint(\"\\n\")\n\tprint(Shape.Circle(5.0))\n\tprint(\"\\n\")\n",
        "Color.Red\nResult.Success(5)\nResult.Error{message=\"boom\"}\nShape.Rectangle{width=1.0, height=2.0}\nShape.Circle(5.0)\n",
    );
}

// =============================================================================
// (b) #[derive(Eq)] — `==`/`!=` lower to a tag compare plus a tag-switch
// field-wise payload compare. `==` borrows both operands.
// =============================================================================

#[test]
fn enum_eq_equal_unequal_across_variants_and_payloads_jit() {
    assert_ryo_output(
        "enum_eq",
        "#[derive(Eq)]\nenum Color:\n\tRed\n\tGreen\n\n#[derive(Eq)]\nenum Result:\n\tSuccess(int)\n\tError(message: str)\n\nfn main():\n\ta = Result.Success(1)\n\tb = Result.Success(1)\n\tc = Result.Success(2)\n\td = Result.Error{message=\"x\"}\n\te = Result.Error{message=\"x\"}\n\tf = Result.Error{message=\"y\"}\n\tprint(a == b)\n\tprint(\"\\n\")\n\tprint(a == c)\n\tprint(\"\\n\")\n\tprint(a != c)\n\tprint(\"\\n\")\n\tprint(d == e)\n\tprint(\"\\n\")\n\tprint(d == f)\n\tprint(\"\\n\")\n\tprint(a == d)\n\tprint(\"\\n\")\n\tprint(Color.Red == Color.Red)\n\tprint(\"\\n\")\n\tprint(Color.Red == Color.Green)\n\tprint(\"\\n\")\n\tprint(Color.Red != Color.Green)\n\tprint(\"\\n\")\n\tprint(a)\n\tprint(\"\\n\")\n\tprint(d)\n\tprint(\"\\n\")\n",
        "true\nfalse\ntrue\ntrue\nfalse\nfalse\ntrue\nfalse\ntrue\nResult.Success(1)\nResult.Error{message=\"x\"}\n",
    );
}

// =============================================================================
// Review Focus 1 — str-payload drop paths through reassignment in a loop
// with an early return. Both the inline (SSO, no-op free) and the
// spilled (heap-backed) message shapes must run clean; Task 11 re-runs
// the heap shape under Valgrind. The Success path carries no heap
// payload: destroying it frees nothing observable.
// =============================================================================

#[test]
fn enum_inline_str_payload_reassigned_in_loop_early_return_jit() {
    // `message="boom"` is 4 bytes — the inline SSO path, where
    // ryo_str_free no-ops. Each loop reassign drops the old Error's
    // payload before the overwrite; the early return moves the live
    // value out of the loop.
    assert_ryo_output(
        "enum_inline_str_payload",
        "enum Result:\n\tSuccess(int)\n\tError(message: str)\n\nfn probe() -> Result:\n\tmut r = Result.Error{message=\"boom\"}\n\tmut i = 0\n\twhile i < 3:\n\t\tr = Result.Error{message=\"boom\"}\n\t\tif i == 1:\n\t\t\treturn r\n\t\ti += 1\n\treturn Result.Success(5)\n\nfn main():\n\tr = probe()\n\tprint(r)\n\tprint(\"\\n\")\n",
        "Result.Error{message=\"boom\"}\n",
    );
}

#[test]
fn enum_heap_str_payload_reassigned_in_loop_early_return_jit() {
    // The spilled shape: `int_to_str(i) + <100+ byte literal>` builds a
    // fresh heap buffer per iteration (a plain literal would be rodata
    // and free nothing). Reassignment must drop the superseded buffer
    // exactly once per iteration — the leak path Valgrind pins in
    // Task 11. The early return at i == 1 moves the "1..." value out.
    let pad = "a".repeat(100);
    let code = format!(
        "enum Result:\n\tSuccess(int)\n\tError(message: str)\n\nfn probe() -> Result:\n\tmut r = Result.Error{{message=int_to_str(42) + \"{pad}\"}}\n\tmut i = 0\n\twhile i < 3:\n\t\tr = Result.Error{{message=int_to_str(i) + \"{pad}\"}}\n\t\tif i == 1:\n\t\t\treturn r\n\t\ti += 1\n\treturn Result.Success(5)\n\nfn main():\n\tr = probe()\n\tprint(r)\n\tprint(\"\\n\")\n"
    );
    assert_ryo_output(
        "enum_heap_str_payload",
        &code,
        &format!("Result.Error{{message=\"1{pad}\"}}\n"),
    );
}

#[test]
fn enum_reassign_from_heap_error_to_success_jit() {
    // The Result.Success(5) path: reassigning FROM Error TO Success
    // drops the Error payload (free-on-reassign on a needs-drop enum).
    // The payload is runtime-built past the 23-byte inline cap so the
    // variant-switch free is a real ryo_str_free (Valgrind-pinned in
    // Task 11, not an SSO no-op). The returned Success carries only an
    // int, so its own destruction frees nothing observable.
    let pad = "a".repeat(30);
    let code = format!(
        "enum Result:\n\tSuccess(int)\n\tError(message: str)\n\nfn probe() -> Result:\n\tmut r = Result.Error{{message=int_to_str(42) + \"{pad}\"}}\n\tr = Result.Success(5)\n\treturn r\n\nfn main():\n\tr = probe()\n\tprint(r)\n\tprint(\"\\n\")\n"
    );
    assert_ryo_output("enum_success_path", &code, "Result.Success(5)\n");
}

// =============================================================================
// Review Focus 2 — Copy enum: `Small` is `is_copy` (every payload field
// is a Copy type), so assignment copies instead of moving. The copy
// happens while the narrow `A(int)` variant is active and the largest
// variant `B(f32 x4)` is not — a full-object read would touch the
// inactive payload bytes (Valgrind flags the uninit read in Task 11).
// =============================================================================

#[test]
fn enum_copy_variant_copied_while_narrow_variant_active_jit() {
    assert_ryo_output(
        "enum_copy_small",
        "enum Small:\n\tA(int)\n\tB(float, float, float, float)\n\nfn main():\n\ta = Small.A(7)\n\tb = a\n\tc = a\n\tprint(a)\n\tprint(\"\\n\")\n\tprint(b)\n\tprint(\"\\n\")\n\tprint(c)\n\tprint(\"\\n\")\n\twide = Small.B(1.0, 2.0, 3.0, 4.0)\n\tnarrow = wide\n\tprint(wide)\n\tprint(\"\\n\")\n\tprint(narrow)\n\tprint(\"\\n\")\n",
        "Small.A(7)\nSmall.A(7)\nSmall.A(7)\nSmall.B(1.0, 2.0, 3.0, 4.0)\nSmall.B(1.0, 2.0, 3.0, 4.0)\n",
    );
}

// =============================================================================
// Review Focus 3 — enum values across the parameter ABI: borrow (default),
// `move`, and `inout`, plus sret returns. Payload "mutation" is whole-
// value reassignment to a different variant. Caller and callee each free
// exactly once (Valgrind pins the heap payloads in Task 11).
// =============================================================================

#[test]
fn enum_borrow_move_inout_params_and_sret_return_jit() {
    // `reseat` writes a runtime-built message past the 23-byte inline
    // cap, so heap traffic crosses every ABI edge here: the inout
    // write-back escapes the callee's fresh buffer (which the callee
    // must not free), `consume` moves the heap-backed value through a
    // move param and returns it by sret, and `bump`'s variant switch
    // frees that payload caller-side. Caller and callee each free
    // exactly once (Valgrind pins the heap payloads in Task 11).
    let pad = "a".repeat(30);
    let code = format!(
        "enum Result:\n\tSuccess(int)\n\tError(message: str)\n\nfn show(r: Result):\n\tprint(r)\n\tprint(\"\\n\")\n\nfn reseat(inout r: Result):\n\tr = Result.Error{{message=int_to_str(7) + \"{pad}\"}}\n\nfn bump(inout r: Result):\n\tr = Result.Success(99)\n\nfn consume(move r: Result) -> Result:\n\treturn r\n\nfn main():\n\tmut r = Result.Success(1)\n\tshow(r)\n\treseat(&r)\n\tshow(r)\n\tmut out = consume(r)\n\tshow(out)\n\tbump(&out)\n\tshow(out)\n"
    );
    let expected = format!(
        "Result.Success(1)\nResult.Error{{message=\"7{pad}\"}}\nResult.Error{{message=\"7{pad}\"}}\nResult.Success(99)\n"
    );
    assert_ryo_output("enum_param_abi", &code, &expected);
}

// =============================================================================
// Review Focus 4 — an enum holding a struct holding a heap str, itself
// nested in a struct: construct, move, drop. The heap-built str (> SSO)
// makes drop completeness observable at every level under Valgrind in
// Task 11 (each payload freed exactly once; destruction order is not
// asserted here).
// =============================================================================

#[test]
fn enum_nested_in_struct_construct_move_drop_jit() {
    let pad = "b".repeat(40);
    let code = format!(
        "struct Msg:\n\ttext: str\n\nenum Note:\n\tInfo(Msg)\n\tBlank\n\nstruct Wrapper:\n\tinner: Note\n\nfn main():\n\tm = Msg{{text=int_to_str(7) + \"{pad}\"}}\n\tw = Wrapper{{inner=Note.Info(m)}}\n\tv = w\n\tprint(v)\n\tprint(\"\\n\")\n"
    );
    assert_ryo_output(
        "enum_nested_wrapper",
        &code,
        &format!("Wrapper{{inner=Note.Info(Msg{{text=\"7{pad}\"}})}}\n"),
    );
}

// =============================================================================
// (g) Move semantics: `Small`-style Copy enums copy (still usable after
// assignment — see enum_copy_small above). A needs-drop Move enum moves:
// any read after the move is E0020, mirroring the struct/str move-error
// assertions in integration_ownership.rs.
// =============================================================================

#[test]
fn enum_use_after_move_errors_e0020_jit() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "enum Result:\n\tSuccess(int)\n\tError(message: str)\n\nfn main():\n\toutcome = Result.Error{message=\"boom\"}\n\ts = outcome\n\tprint(outcome)\n\tprint(\"\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "enum_use_after_move.ryo", code);
    let output = run_ryo_command(&["run", "enum_use_after_move.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "using a moved Move-enum must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0020"), "stderr: {}", stderr);
    assert!(
        stderr.contains("outcome"),
        "expected binding name in message: {}",
        stderr
    );
    assert!(
        stderr.contains("moved here") || stderr.contains("moved into"),
        "expected move-site note: {}",
        stderr
    );
}

#[test]
fn enum_move_into_function_consumes_value_jit() {
    let temp_dir = TempDir::new().expect("temp");
    let code = "enum Result:\n\tSuccess(int)\n\tError(message: str)\n\nfn consume(move r: Result):\n\tprint(r)\n\tprint(\"\\n\")\n\nfn main():\n\tr = Result.Success(1)\n\tconsume(r)\n\tconsume(r)\n";
    let test_file = create_test_file(temp_dir.path(), "enum_move_into_fn.ryo", code);
    let output = run_ryo_command(&["run", "enum_move_into_fn.ryo"], &test_file).expect("run");
    assert!(
        !output.status.success(),
        "passing a moved enum by move a second time must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0020"), "stderr: {}", stderr);
}

// =============================================================================
// (h) Forward and cross-kind references: every decl below names a type
// declared LATER (struct -> enum -> enum -> struct). The pre-scan +
// define DFS must order them; construction and print prove resolution.
// =============================================================================

#[test]
fn enum_forward_and_cross_kind_references_jit() {
    assert_ryo_output(
        "enum_forward_refs",
        "struct Holder:\n\titem: Envelope\n\nenum Envelope:\n\tSealed(body: Letter)\n\tEmpty\n\nenum Letter:\n\tWritten(page: Page)\n\tIlliterate\n\nstruct Page:\n\twords: str\n\nfn main():\n\tpg = Page{words=\"hello\"}\n\tl = Letter.Written(pg)\n\te = Envelope.Sealed(l)\n\th = Holder{item=e}\n\tprint(h)\n\tprint(\"\\n\")\n",
        "Holder{item=Envelope.Sealed{body=Letter.Written{page=Page{words=\"hello\"}}}}\n",
    );
}
