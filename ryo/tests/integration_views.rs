mod common;
use common::*;

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// M8.4: string slices — compile-and-run, runtime panics, and diagnostics.
// This group is the black-box net for the `TypeKind::View(ViewKind)`
// refactor: assertions target exit codes, stderr text, and E-codes —
// never compiler internals.
// ---------------------------------------------------------------------------

#[test]
fn test_slice_shorthands_and_reslice() {
    assert_ryo_runs(
        "slice_forms.ryo",
        "fn main():\n\ts: str = \"hello world\"\n\tprint(s[0:5])\n\tprint(s[6:])\n\tprint(s[:5])\n\tprint(s[:])\n\tv = s[0:5]\n\tprint(v[1:3])\n",
    );
}

#[test]
fn test_slice_multibyte_utf8() {
    assert_ryo_runs(
        "slice_utf8.ryo",
        "fn main():\n\ts: str = \"héllo wörld\"\n\tprint(s[0:6])\n\tprint(s[7:13])\n",
    );
}

#[test]
fn test_view_param_with_owned_and_view_args() {
    // A `strview` parameter accepts both an owned `str` (implicit
    // owner → view conversion, §3.4) and an existing view. The
    // function prints rather than returns the slice — E1 forbids
    // view returns (see test_view_return_diag).
    assert_ryo_runs(
        "view_param.ryo",
        "fn print_first_word(text: strview):\n\tmut i: int = 0\n\twhile i < text.len():\n\t\tif text[i:i+1] == \" \":\n\t\t\tprint(text[0:i])\n\t\t\treturn\n\t\ti += 1\n\tprint(text)\n\nfn main():\n\ts: str = \"hello world\"\n\tprint_first_word(s)\n\tprint_first_word(s[6:])\n",
    );
}

#[test]
fn test_slice_of_borrowed_param_ok() {
    assert_ryo_runs(
        "slice_param_ok.ryo",
        "fn head(s: str):\n\tprint(s[0:1])\n\nfn main():\n\tx: str = \"hi\"\n\thead(x)\n",
    );
}

#[test]
fn test_slice_empty() {
    assert_ryo_runs(
        "slice_empty.ryo",
        "fn main():\n\ts: str = \"abc\"\n\tprint(s[3:])\n\tprint(s[0:0])\n",
    );
}

#[test]
fn test_slice_out_of_range_panics() {
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "slice_oob.ryo",
        "fn main():\n\ts: str = \"abc\"\n\tprint(s[0:9])\n",
    );
    let output = run_ryo_command(&["run", "slice_oob.ryo"], &test_file).expect("run ryo");
    assert!(!output.status.success());
    // `__ryo_slice` panics with the `__ryo_panic` convention:
    // stderr message + exit 101 (matches the existing panic tests at
    // :1526-1530).
    assert_eq!(
        output.status.code(),
        Some(101),
        "expected panic exit code 101: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("slice index out of range"),
        "missing panic message: {stderr}"
    );
}

#[test]
fn test_slice_non_boundary_panics() {
    // `é` is 2 bytes (bytes 1-2 of "héllo"): byte 1 STARTS the character
    // (a valid boundary) and byte 2 falls INSIDE it, so `s[1:2]` panics on
    // its end bound, not its start.
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "slice_boundary.ryo",
        "fn main():\n\ts: str = \"héllo\"\n\tprint(s[1:2])\n",
    );
    let output = run_ryo_command(&["run", "slice_boundary.ryo"], &test_file).expect("run ryo");
    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(101),
        "expected panic exit code 101: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("UTF-8 char boundary"),
        "missing panic message: {stderr}"
    );
}

#[test]
fn test_slice_reversed_range_panics() {
    // Reversed ranges (start > end) are invalid and panic at creation
    // (final spec §3.1).
    let temp_dir = TempDir::new().expect("temp dir");
    let test_file = create_test_file(
        temp_dir.path(),
        "slice_reversed.ryo",
        "fn main():\n\ts: str = \"abc\"\n\tprint(s[3:1])\n",
    );
    let output = run_ryo_command(&["run", "slice_reversed.ryo"], &test_file).expect("run ryo");
    assert!(!output.status.success());
    assert_eq!(
        output.status.code(),
        Some(101),
        "expected panic exit code 101: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("slice index out of range"),
        "missing panic message: {stderr}"
    );
}

#[test]
fn test_view_return_diag() {
    // E1 (final spec §3.3): views cannot escape via return. Sema
    // rejects the `-> strview` signature with E0022 (ownership's E0034
    // backstop also fires on the `return s[0:1]` body).
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn bad(s: str) -> strview:\n\treturn s[0:1]\n";
    let test_file = create_test_file(temp_dir.path(), "view_return.ryo", code);
    let output = run_ryo_command(&["run", "view_return.ryo"], &test_file).expect("run");
    assert!(!output.status.success(), "expected compile error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0022"), "expected E0022: {}", stderr);
    assert!(
        stderr.contains("cannot return views"),
        "expected view-return message: {}",
        stderr
    );
}

#[test]
fn test_slice_of_borrowed_param() {
    // `head` declares `-> strview`: view returns are E1-rejected (final
    // spec §3.3), so this program must not compile. The runnable
    // borrowed-param case is test_slice_of_borrowed_param_ok above.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn head(s: str) -> strview:\n\tprint(s[0:1])\nfn main():\n\tbad()\nfn bad():\n\tx: str = \"hi\"\n\thead(x)\n";
    let test_file = create_test_file(temp_dir.path(), "slice_param.ryo", code);
    let output = run_ryo_command(&["run", "slice_param.ryo"], &test_file).expect("run");
    assert!(!output.status.success(), "expected compile error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0022"), "expected E0022: {}", stderr);
}

#[test]
fn test_move_view_diag() {
    // E2 (final spec §3.3): a slice cannot be passed to a `move`
    // parameter — the view would escape its scope. Sema also reports
    // the E0012 type mismatch on the same call; E0034 is the
    // ownership-pass diagnostic this test pins.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn eat(move s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\teat(s[0:1])\n";
    let test_file = create_test_file(temp_dir.path(), "move_view.ryo", code);
    let output = run_ryo_command(&["run", "move_view.ryo"], &test_file).expect("run");
    assert!(!output.status.success(), "expected compile error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0034"), "expected E0034: {}", stderr);
    assert!(
        stderr.contains("cannot pass a slice to a `move` parameter"),
        "expected move-view message: {}",
        stderr
    );
}

#[test]
fn test_freeze_move_diag() {
    // P2 freeze (final spec §3.2): `v`'s last use is after the move,
    // so the projection is live at `eat(s)`.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn eat(move s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\tv = s[0:2]\n\teat(s)\n\tprint(v)\n";
    let test_file = create_test_file(temp_dir.path(), "freeze_move.ryo", code);
    let output = run_ryo_command(&["run", "freeze_move.ryo"], &test_file).expect("run");
    assert!(!output.status.success(), "expected compile error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0035"), "expected E0035: {}", stderr);
    assert!(
        stderr.contains("cannot move `s` while a slice of it is live"),
        "expected freeze message: {}",
        stderr
    );
}

#[test]
fn test_freeze_inout_diag() {
    // P2 freeze (final spec §3.2): passing `&s` inout mutates the
    // owner while `v` is live (used after the call).
    let temp_dir = TempDir::new().expect("temp");
    let code =
        "fn main():\n\tmut s: str = \"hi\"\n\tv = s[0:2]\n\tstr_push(&s, \"!\")\n\tprint(v)\n";
    let test_file = create_test_file(temp_dir.path(), "freeze_inout.ryo", code);
    let output = run_ryo_command(&["run", "freeze_inout.ryo"], &test_file).expect("run");
    assert!(!output.status.success(), "expected compile error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0035"), "expected E0035: {}", stderr);
    assert!(
        stderr.contains("cannot mutate `s` while a slice of it is live"),
        "expected freeze message: {}",
        stderr
    );
}

#[test]
fn test_view_to_str_param_end_to_end() {
    // P6' (M8.4.1): a view passed to an owned `str` parameter
    // re-borrows — codegen materializes a cap=0 triple (no allocation,
    // call-scoped), so both slice args compile and run. (`assert`'s
    // message stays string-literal-only: the panic text is formatted
    // at compile time, so a runtime view cannot flow through it.)
    assert_ryo_runs(
        "view_str_param.ryo",
        "fn show(s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hello\"\n\tshow(s[0:3])\n\tshow(s[0:2])\n",
    );
}

#[test]
fn test_str_materialize_escape_and_independence() {
    // M8.4.1.2: `str(view)` materializes an owned copy. Deep-copy proof:
    // materialize, then mutate the original — the copy is unaffected;
    // and `return str(text)` is the sanctioned fix for the E1 view
    // escape (a `strview` cannot be returned, an owned copy can).
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn first(text: strview) -> str:\n\treturn str(text)\n\nfn main():\n\tmut s: str = \"hello\"\n\tx: str = str(s[0:2])\n\tstr_push(&s, \"!\")\n\tprint(x)\n\tprint(first(s[0:3]))\n";
    let test_file = create_test_file(temp_dir.path(), "str_materialize.ryo", code);
    let output = run_ryo_command(&["run", "str_materialize.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The program's output sits between the "[Codegen]" dump and
    // "[Result]". `print` is a raw write (no trailing newline), so the
    // two prints concatenate: "he" (the copy, unaffected by the `!`
    // push) then "hel" (the escape-fixed return).
    let after_codegen = stdout.split("[Codegen]").nth(1).unwrap();
    let program_out = after_codegen.split("[Result]").next().unwrap().trim();
    assert_eq!(
        program_out, "hehel",
        "materialized copies must survive mutation of the original"
    );
}

#[test]
fn test_redundant_materialize_warning() {
    // M8.4.1.2 W0003: a bound `str(view)` copy that never escapes and
    // whose source is never mutated is a redundant allocation. It is a
    // warning, not an error — the run still succeeds.
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\ts: str = \"hi\"\n\tx: str = str(s[0:1])\n\tprint(x)\n";
    let test_file = create_test_file(temp_dir.path(), "redundant_materialize.ryo", code);
    let output = run_ryo_command(&["run", "redundant_materialize.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("W0003").count(),
        1,
        "expected exactly one W0003 warning: {}",
        stderr
    );
    assert!(
        stderr.contains("copy never escapes"),
        "expected the case-B message in stderr: {}",
        stderr
    );
}

#[test]
fn test_str_materialize_rejects_owned_str() {
    // M8.4.1.2: the `str(...)` materialize call form is strview-only —
    // an owned `str` argument would be a same-type copy, which is the
    // future `Clone` trait's job. Sema rejects it with E0012; this pins
    // the rejection end-to-end (previously covered only by the sema
    // unit test `str_materialize_rejects_non_view_arg`).
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn main():\n\ts: str = \"hi\"\n\tx: str = str(s)\n";
    let test_file = create_test_file(temp_dir.path(), "str_owned.ryo", code);
    let output = run_ryo_command(&["run", "str_owned.ryo"], &test_file).expect("run");
    assert!(!output.status.success(), "expected compile error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("E0012"), "expected E0012: {}", stderr);
    assert!(
        stderr.contains("str() argument must be strview"),
        "expected strview-only message: {}",
        stderr
    );
}
