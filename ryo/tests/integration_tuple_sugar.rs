//! M10 tuple sugar — JIT end-to-end tests.
//!
//! `(a, b)` literals (and the single-element `(a,)`), `(T1, T2)`
//! positional type sugar, and paren Debug rendering for
//! all-positional anonymous shapes. `(17, "alice")` is sugar over
//! the anonymous struct literal `{0=17, 1="alice"}` — one
//! structural type, one Debug path.

mod common;
use common::*;

use tempfile::TempDir;

#[test]
fn tuple_sugar_roundtrip() {
    // `(int, str)` in the return type and `{0: int, 1: str}` are one
    // structural type: annotating `zero` with the paren spelling
    // compiles only if the shapes dedup (a mismatch fails
    // compilation). The `==` line lands with Task 8 (equality).
    assert_ryo_output(
        "tuple_sugar_roundtrip",
        "fn pair() -> (int, str):\n\treturn (17, \"alice\")\n\nfn main():\n\tp = pair()\n\tzero: (int, str) = {0=17, 1=\"alice\"}\n\tprint(p.0)\n\tprint(\"\\n\")\n\tprint(zero.1)\n\tprint(\"\\n\")\n\t# assert p == zero -- enabled in task 8 (equality)\n",
        "17\nalice\n",
    );
}

#[test]
fn tuple_debug_paren() {
    // All-positional shapes render in paren form — `(v0, v1)`, and
    // `(v,)` for a single field. str fields quote (the M10 ruling:
    // the anon Debug repr matches named structs and Python container
    // repr), so a `str` element renders with quotes inside parens.
    assert_ryo_output(
        "tuple_debug_paren",
        "fn pair() -> (int, str):\n\treturn (17, \"alice\")\n\nfn main():\n\tprint(pair())\n\tprint(\"\\n\")\n\tprint((7,))\n\tprint(\"\\n\")\n",
        "(17, \"alice\")\n(7,)\n",
    );
}

#[test]
fn mixed_shape_braces() {
    // A mixed positional/named shape never renders parens — braces
    // with field names, in written order.
    assert_ryo_output(
        "mixed_shape_braces",
        "fn main():\n\tprint({0=1, x=2})\n\tprint(\"\\n\")\n",
        "{0=1, x=2}\n",
    );
}

#[test]
fn positional_access_on_call_result() {
    // `.0` applies to any postfix-chain head, including a call
    // result — the parse property f-string interpolation will need
    // (`f"{pair().0}"`, plan decision 3) once f-strings land.
    assert_ryo_output(
        "positional_access_on_call_result",
        "fn pair() -> (int, str):\n\treturn (17, \"alice\")\n\nfn main():\n\tprint(pair().0)\n\tprint(\"\\n\")\n",
        "17\n",
    );
}

#[test]
fn empty_parens_rejected_with_unit_message() {
    // `()` is the unit: E0111 names `void` as the unit type and
    // suggests `none` / `(x,)`. One diagnostic total — the recovery
    // is the empty anonymous literal, which sema tolerates silently
    // (same contract as `{}`'s E0109).
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tp = ()\n";
    let test_file = create_test_file(temp_dir.path(), "unit_paren.ryo", code);

    let output =
        run_ryo_command(&["run", "unit_paren.ryo"], &test_file).expect("Failed to run ryo command");

    assert!(!output.status.success(), "empty parens must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("[E0111]").count(),
        1,
        "expected exactly the E0111 diagnostic, got: {}",
        stderr
    );
    assert!(
        stderr.contains("void"),
        "error should name `void` as the unit type, got: {}",
        stderr
    );
    assert!(
        stderr.contains("none"),
        "error should suggest `none`, got: {}",
        stderr
    );
    assert!(
        stderr.contains("(x,)"),
        "error should suggest `(x,)`, got: {}",
        stderr
    );
}
