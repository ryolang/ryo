mod common;
use common::*;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// M8.4.2: bytes/bytesview — compile-and-run, runtime panics, diagnostics.
// Same black-box pattern as integration_views.rs: assertions target exit
// codes, stderr/stdout text, and E-codes — never compiler internals.
// ---------------------------------------------------------------------------

/// Compile-and-run asserting the program's exact stdout (the `print`
/// path). JIT `run` wraps program output in `[Input Source]` / `[AST]`
/// / `[Codegen]` dumps and a trailing `[Result] => 0` line, and `print`
/// is a raw write (no trailing newline), so the program output is
/// extracted between the markers — the same pattern as
/// `integration_views.rs::test_str_materialize_escape_and_independence`.
fn assert_ryo_prints(test_name: &str, code: &str, expected_stdout: &str) {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), test_name, code);
    let output =
        run_ryo_command(&["run", test_name], &test_file).expect("Failed to run ryo command");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let program_out = stdout
        .split("[Codegen]")
        .nth(1)
        .and_then(|s| s.split("[Result]").next())
        .expect("run output must carry [Codegen] and [Result] markers")
        .trim();
    assert_eq!(program_out, expected_stdout, "stdout mismatch");
}

#[test]
fn test_bytes_literal_and_len() {
    assert_ryo_runs(
        "bytes_len.ryo",
        "fn main():\n\tb = b\"\\x01\\x02\\x03\"\n\tassert(b.len() == 3, \"len\")\n\tassert(not b.is_empty(), \"empty\")\n\te = b\"\"\n\tassert(e.is_empty(), \"empty literal\")\n",
    );
}

#[test]
fn test_bytes_annotation_round_trip() {
    assert_ryo_runs(
        "bytes_ann.ryo",
        "fn takes(b: bytes, v: bytesview):\n\tassert(b.len() == 2, \"b\")\n\tassert(v.len() == 1, \"v\")\n\nfn main():\n\traw = b\"AB\"\n\ttakes(raw, raw[0:1])\n",
    );
}

#[test]
fn test_bytes_concat_and_print_repr() {
    assert_ryo_prints(
        "bytes_concat.ryo",
        "fn main():\n\tmut buf = b\"\\x00\"\n\tbytes_push(&buf, 255)\n\tbuf = buf + b\"\\x01\\x02\\x03\"\n\tprint(buf)\n",
        "b\"\\0\\xff\\x01\\x02\\x03\"",
    );
}

#[test]
fn test_bytes_print_escapes() {
    assert_ryo_prints(
        "bytes_repr.ryo",
        "fn main():\n\tprint(b\"A\\x00\\xff\\n\")\n\tprint(b\"\")\n",
        // print is a raw write: the two reprs concatenate with no
        // separating newline.
        "b\"A\\0\\xff\\n\"b\"\"",
    );
}

#[test]
fn test_bytes_equality() {
    assert_ryo_runs(
        "bytes_eq.ryo",
        "fn main():\n\tb = b\"\\x01\\x02\"\n\tv = b[0:2]\n\tassert(b == b\"\\x01\\x02\", \"eq\")\n\tassert(b != b\"\\x03\", \"ne\")\n\tassert(v == b[0:2], \"view eq\")\n\tassert(b == v, \"cross eq\")\n\tassert(v == b, \"cross eq 2\")\n",
    );
}

#[test]
fn test_bytes_slice_no_utf8_boundary_check() {
    // Slicing "héllo" mid-codepoint is an error for str but fine for
    // bytes (the one behavioral divergence from strview).
    assert_ryo_prints(
        "bytes_slice.ryo",
        "fn main():\n\tb = \"héllo\".to_bytes()\n\tprint(b[1:3])\n",
        "b\"\\xc3\\xa9\"",
    );
}

#[test]
fn test_bridging_round_trip() {
    // Non-ASCII content via \xNN escapes: bytes literals must be ASCII
    // (E0102); é is \xc3\xa9 in UTF-8.
    assert_ryo_runs(
        "bytes_bridge.ryo",
        "fn main():\n\traw = b\"h\\xc3\\xa9llo\"\n\ttext = raw.to_str()\n\traw2 = text.to_bytes()\n\tassert(raw == raw2, \"round trip\")\n\tv = raw[0:1]\n\tt2 = v.to_str()\n\tprint(t2)\n",
    );
}

#[test]
fn test_bytes_materialize() {
    assert_ryo_runs(
        "bytes_mat.ryo",
        "fn main():\n\traw = b\"\\x01\\x02\"\n\tv = raw[0:1]\n\tc = bytes(v)\n\tassert(c.len() == 1, \"len\")\n\tassert(c == v, \"contents\")\n",
    );
}

#[test]
fn test_bytes_push_range_panics() {
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "bytes_push_oob.ryo",
        "fn main():\n\tmut b = b\"\\x00\"\n\tbytes_push(&b, 256)\n",
    );
    let output = run_ryo_command(&["run", "bytes_push_oob.ryo"], &test_file).expect("run ryo");
    assert_eq!(output.status.code(), Some(101));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bytes_push value out of range (0-255)"),
        "missing panic message: {stderr}"
    );
}

#[test]
fn test_bytes_to_str_invalid_utf8_panics() {
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "bytes_to_str_bad.ryo",
        "fn main():\n\tb = b\"\\xff\"\n\tprint(b.to_str())\n",
    );
    let output = run_ryo_command(&["run", "bytes_to_str_bad.ryo"], &test_file).expect("run ryo");
    assert_eq!(output.status.code(), Some(101));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bytes are not valid UTF-8"),
        "missing panic message: {stderr}"
    );
}

#[test]
fn test_bytes_slice_out_of_range_panics() {
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "bytes_slice_oob.ryo",
        "fn main():\n\tb = b\"\\x01\\x02\\x03\"\n\tprint(b[0:9])\n",
    );
    let output = run_ryo_command(&["run", "bytes_slice_oob.ryo"], &test_file).expect("run ryo");
    assert_eq!(output.status.code(), Some(101));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("slice index out of range"),
        "missing panic message: {stderr}"
    );
}

#[test]
fn test_bytes_index_reads() {
    assert_ryo_prints(
        "bytes_index.ryo",
        "fn main():\n\traw = b\"\\x01\\x02\\x03\"\n\theader = raw[0:2]\n\tprint(int_to_str(header[0]))\n\tprint(int_to_str(raw[2]))\n",
        // print is a raw write: the two ints concatenate with no
        // separating newline.
        "13",
    );
}

#[test]
fn test_bytes_index_out_of_range_panics() {
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "bytes_index_oob.ryo",
        "fn main():\n\tb = b\"\\x01\"\n\tprint(int_to_str(b[5]))\n",
    );
    let output = run_ryo_command(&["run", "bytes_index_oob.ryo"], &test_file).expect("run ryo");
    assert_eq!(output.status.code(), Some(101));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("index out of range"),
        "missing panic message: {stderr}"
    );
}

#[test]
fn test_bytes_index_negative_panics() {
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "bytes_index_neg.ryo",
        "fn main():\n\tb = b\"\\x01\"\n\tprint(int_to_str(b[0 - 1]))\n",
    );
    let output = run_ryo_command(&["run", "bytes_index_neg.ryo"], &test_file).expect("run ryo");
    assert_eq!(output.status.code(), Some(101));
}

#[test]
fn test_bytes_aot_build_and_run() {
    // AOT leg: bytes_push growth + to_str bridging through the Zig
    // linker path. `print` is a raw write (no trailing newline), so the
    // built binary's stdout is exactly "AB".
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "bytes_aot.ryo",
        "fn main():\n\tmut b = b\"\\x41\"\n\tbytes_push(&b, 66)\n\tprint(b.to_str())\n",
    );
    let build = run_ryo_command(&["build", "bytes_aot.ryo"], &test_file).expect("ryo build");
    assert!(
        build.status.success(),
        "build stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = exe_path(temp_dir.path(), "bytes_aot");
    let run = std::process::Command::new(&exe)
        .output()
        .expect("run built binary");
    assert!(
        run.status.success(),
        "built binary failed. stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "AB");
}

#[test]
fn test_bytes_type_errors_surface() {
    // Mixed bytes + str is a compile error (E0012) — the diagnostic
    // must surface through the driver, not an ICE or silent miscompile.
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "bytes_err.ryo",
        "fn main():\n\tb = b\"\\x01\"\n\tx = b + \"s\"\n",
    );
    let output = run_ryo_command(&["run", "bytes_err.ryo"], &test_file).expect("run ryo");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("type mismatch"), "missing E0012: {stderr}");
}

#[test]
fn test_bytes_conditional_reassign_dead_drop() {
    // Conditional dead-drop for a bytes owner (M8.4.2): a bytes
    // binding reassigned on one arm and never read afterwards gets its
    // pre-if heap buffer freed on the untouched path by
    // `emit_conditional_dead_drops`, which must select `ryo_bytes_free`
    // (not `ryo_str_free`). Heap-backed via `.to_bytes()` so the free
    // is a real deallocation — a rodata bytes literal would be cap=0,
    // a runtime no-op. (The dead reassign warns W0001; warnings do not
    // fail the run.)
    assert_ryo_runs(
        "bytes_dead_drop.ryo",
        "fn main():\n\tmut b = \"AB\".to_bytes()\n\tflag = false\n\tif flag:\n\t\tb = \"CD\".to_bytes()\n",
    );
}
