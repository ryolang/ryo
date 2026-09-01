mod common;
use common::*;

use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn test_lex_command_integration() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "tokens.ryo", "x = 1 + 2 * 3");

    let output =
        run_ryo_command(&["lex", "tokens.ryo"], &test_file).expect("Failed to run ryo lex command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Lex command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify token output contains expected tokens.
    // The lex driver renders ident/string-literal payloads through
    // the InternPool (Phase 2), so we see the original text;
    // integer literals are parsed at lex time so they print as
    // typed values rather than as the source slice.
    assert!(stdout.contains("Ident(\"x\")"), "Missing x identifier");
    assert!(stdout.contains("Assign"), "Missing Assign token");
    assert!(stdout.contains("IntLit(1)"), "Missing IntLit(1) token");
    assert!(stdout.contains("Add"), "Missing Add token");
    assert!(stdout.contains("IntLit(2)"), "Missing IntLit(2) token");
    assert!(stdout.contains("Mul"), "Missing Mul token");
    assert!(stdout.contains("IntLit(3)"), "Missing IntLit(3) token");

    // Verify no output files are created for lex command (lex doesn't generate files)
    assert!(
        !PathBuf::from("tokens.o").exists(),
        "Object file should not be created for lex command"
    );
}

#[test]
fn test_parse_command_simple_declaration() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "simple.ryo", "x = 42");

    let output = run_ryo_command(&["parse", "simple.ryo"], &test_file)
        .expect("Failed to run ryo parse command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Parse command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify AST output contains expected elements
    assert!(stdout.contains("[AST]"), "Missing AST section");
    assert!(stdout.contains("Program"), "Missing Program node");
    assert!(stdout.contains("VarDecl"), "Missing VarDecl node");
}

#[test]
fn test_parse_command_with_type_annotation() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "typed.ryo", "x: int = 42");

    let output = run_ryo_command(&["parse", "typed.ryo"], &test_file)
        .expect("Failed to run ryo parse command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Parse command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify AST output
    assert!(stdout.contains("VarDecl"), "Missing VarDecl node");
    assert!(stdout.contains("int"), "Missing type annotation");
}

#[test]
fn test_parse_command_multiple_statements() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "multi.ryo", "x = 1\ny = 2\nz = 3");

    let output = run_ryo_command(&["parse", "multi.ryo"], &test_file)
        .expect("Failed to run ryo parse command");

    if !output.status.success() {
        println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Parse command failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify AST output
    assert!(stdout.contains("VarDecl"), "Missing VarDecl nodes");
}

#[test]
fn test_file_not_found_error() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let nonexistent_path = temp_dir.path().join("nonexistent.ryo");

    let output = run_ryo_command(&["parse", "nonexistent.ryo"], &nonexistent_path)
        .expect("Failed to run ryo command");

    // Command should fail
    assert!(
        !output.status.success(),
        "Command should fail when file doesn't exist"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // The stable needle is our own `IO error` prefix from
    // CompilerError's Display; the OS message after it differs by
    // platform ("No such file or directory" vs "The system cannot
    // find the file specified.").
    assert!(
        stderr.contains("IO error") || stderr.contains("No such file"),
        "Should contain file not found error, got: {}",
        stderr
    );
}

// ============================================================================
// Codegen Integration Tests (ryo run command)
// ============================================================================

#[test]
fn test_run_simple_integer_exit_code() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_simple.ryo", "x = 42");

    let output = run_ryo_command(&["run", "exit_simple.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    // Verify compilation succeeded
    assert!(
        output.status.success(),
        "ryo run should succeed. STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify output shows successful compilation
    // All programs exit with 0 (success) in Milestone 3
    assert!(
        stdout.contains("[Result] => 0"),
        "Output should show exit code 0, got: {}",
        stdout
    );

    // Verify intermediate outputs are present
    assert!(stdout.contains("[Input Source]"), "Missing input source");
    assert!(stdout.contains("[AST]"), "Missing AST output");
    assert!(stdout.contains("[Codegen]"), "Missing codegen output");
}

#[test]
fn test_run_zero_exit_code() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_zero.ryo", "x = 0");

    let output = run_ryo_command(&["run", "exit_zero.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Output should show exit code 0"
    );
}

#[test]
fn test_run_arithmetic_expression_exit_code() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_arithmetic.ryo", "result = 2 + 3 * 4");

    let output = run_ryo_command(&["run", "exit_arithmetic.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // 2 + 3 * 4 = 2 + 12 = 14 (correct precedence), but exit code is 0
    assert!(
        stdout.contains("[Result] => 0"),
        "Should exit with code 0, got: {}",
        stdout
    );
}

#[test]
fn test_run_multiple_statements_last_value() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "x = 10\ny = 20\nz = 30";
    let test_file = create_test_file(temp_dir.path(), "exit_multi.ryo", code);

    let output = run_ryo_command(&["run", "exit_multi.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // All programs exit with 0 (success)
    assert!(
        stdout.contains("[Result] => 0"),
        "Multiple statements should exit with 0"
    );
}

#[test]
fn test_run_division_by_constant() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_div.ryo", "result = 100 / 2");

    let output = run_ryo_command(&["run", "exit_div.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_run_subtraction() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_sub.ryo", "result = 100 - 30");

    let output = run_ryo_command(&["run", "exit_sub.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_run_parenthesized_expression() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_paren.ryo", "result = (10 + 5) * 2");

    let output = run_ryo_command(&["run", "exit_paren.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // (10 + 5) * 2 = 15 * 2 = 30 (computed), but exit code is 0
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

#[test]
fn test_run_with_type_annotation() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_typed.ryo", "x: int = 99");

    let output = run_ryo_command(&["run", "exit_typed.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should correctly compile typed variable and exit with 0"
    );
}

#[test]
fn test_run_mutable_variable() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_mut.ryo", "mut x = 55");

    let output = run_ryo_command(&["run", "exit_mut.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[Result] => 0"),
        "Should correctly compile mutable variable and exit with 0"
    );
}

#[test]
fn test_run_negation_operator() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "exit_neg.ryo", "x = -42");

    let output = run_ryo_command(&["run", "exit_neg.ryo"], &test_file)
        .expect("Failed to run ryo run command");

    assert!(output.status.success(), "ryo run should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // All programs exit with 0 (success)
    assert!(stdout.contains("[Result] => 0"), "Should exit with code 0");
}

// ---------- ryo ir --emit=... ----------
//
// Exit-criteria coverage for pipeline_alignment.md §3.6 / §4.5:
// `Uir::dump` and `Tir::dump` reachable from the CLI, distinct
// listings, deterministic ordering.

#[test]
fn ir_emit_uir_dumps_flat_listing() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "uir.ryo", "x = 1 + 2\n");

    let output = run_ryo_command(&["ir", "--emit=uir", "uir.ryo"], &test_file)
        .expect("Failed to run ryo ir --emit=uir");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("[UIR]"), "missing [UIR] banner: {}", stdout);
    assert!(
        stdout.contains("fn main() -> void"),
        "missing fn header: {}",
        stdout
    );
    assert!(
        stdout.contains("= int 1"),
        "missing int literal listing: {}",
        stdout
    );
    assert!(
        stdout.contains("= add %"),
        "missing add listing: {}",
        stdout
    );
    // UIR must not include typed listings.
    assert!(
        !stdout.contains("[TIR]"),
        "TIR leaked into UIR-only run: {}",
        stdout
    );
    assert!(
        !stdout.contains("iadd"),
        "TIR-spelled op leaked: {}",
        stdout
    );
}

#[test]
fn ir_emit_tir_dumps_typed_listing() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "tir.ryo", "x = 1 + 2\n");

    let output = run_ryo_command(&["ir", "--emit=tir", "tir.ryo"], &test_file)
        .expect("Failed to run ryo ir --emit=tir");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("[TIR]"), "missing [TIR] banner: {}", stdout);
    assert!(
        stdout.contains(": int ="),
        "missing typed slot rendering: {}",
        stdout
    );
    assert!(stdout.contains("iadd %"), "missing typed add: {}", stdout);
    // TIR-only run must not print UIR's untyped spelling.
    assert!(!stdout.contains("[UIR]"), "UIR banner leaked: {}", stdout);
}

#[test]
fn ir_emit_default_is_ast_and_clif() {
    // Bare `ryo ir <file>` preserves the pre-Phase-5 default of
    // AST + Cranelift IR so existing scripts keep working.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "default.ryo", "x = 42\n");

    let output = run_ryo_command(&["ir", "default.ryo"], &test_file).expect("Failed to run ryo ir");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("[AST]"), "missing [AST]: {}", stdout);
    assert!(
        stdout.contains("[Cranelift IR]"),
        "missing CLIF: {}",
        stdout
    );
    assert!(
        !stdout.contains("[UIR]"),
        "UIR leaked into default: {}",
        stdout
    );
    assert!(
        !stdout.contains("[TIR]"),
        "TIR leaked into default: {}",
        stdout
    );
}

#[test]
fn clif_string_ops_use_packed_return_no_stack_slots() {
    // Phase 0 runtime ABI: string-producing runtime calls return
    // {ptr, len} packed in one u128 — no per-call-site stack slots,
    // no out-pointer, no reload (spec 2026-08-25 §2 amendment).
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "clif_str.ryo",
        "fn main():\n\ts: str = \"a\" + \"b\"\n\tt: str = int_to_str(42)\n\tprint(s + t)\n",
    );

    let output = run_ryo_command(&["ir", "--emit=clif", "clif_str.ryo"], &test_file)
        .expect("Failed to run ryo ir --emit=clif");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("-> i128"),
        "runtime string calls must return the packed u128 pair: {}",
        stdout
    );
    assert!(
        !stdout.contains("explicit_slot"),
        "string call paths must not allocate stack slots: {}",
        stdout
    );
    assert!(
        !stdout.contains("stack_addr"),
        "string call paths must not take stack-slot addresses: {}",
        stdout
    );
}

#[test]
fn clif_bytes_ops_use_packed_return_no_stack_slots() {
    // M8.4.2 rides the Phase 0 runtime ABI: bytes-producing runtime
    // calls return {ptr, len} packed in one u128 — no per-call-site
    // stack slots, no out-pointer, no reload (same pin as the str twin
    // above; the bytes_push slot ABI is not exercised by this program).
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "clif_bytes.ryo",
        "fn main():\n\tb: bytes = b\"\\x01\" + b\"\\x02\"\n\tc: bytes = bytes(b[0:1])\n\tprint(int_to_str(b.len() + c.len()))\n",
    );

    let output = run_ryo_command(&["ir", "--emit=clif", "clif_bytes.ryo"], &test_file)
        .expect("Failed to run ryo ir --emit=clif");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("-> i128"),
        "runtime bytes calls must return the packed u128 pair: {}",
        stdout
    );
    assert!(
        !stdout.contains("explicit_slot"),
        "bytes call paths must not allocate stack slots: {}",
        stdout
    );
    assert!(
        !stdout.contains("stack_addr"),
        "bytes call paths must not take stack-slot addresses: {}",
        stdout
    );
}

#[test]
fn clif_user_str_return_keeps_sret() {
    // Copy-elision boundary (docs/dev/copy_elision.md G1/G2): user
    // functions returning `str` keep the hidden sret destination-slot
    // convention — the Phase 0 ABI change touches the *runtime* ABI only.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "clif_sret.ryo",
        "fn make() -> str:\n\treturn \"x\"\n\nfn main():\n\tprint(make())\n",
    );

    let output = run_ryo_command(&["ir", "--emit=clif", "clif_sret.ryo"], &test_file)
        .expect("Failed to run ryo ir --emit=clif");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("(i64 sret)"),
        "user str-returning function must keep the sret convention: {}",
        stdout
    );
}

/// The `fnN` names in a single-function CLIF dump whose signature
/// text satisfies `sig_pred`. Runtime names are opaque in the text
/// format (`fn0 = u0:1 sig0`), so the signature shape parsed from the
/// preamble is the only handle a test has. Only valid for dumps of a
/// single Cranelift function — `fn`/`sig` numbering restarts per
/// function.
fn clif_fns_matching_sig(clif: &str, sig_pred: impl Fn(&str) -> bool) -> Vec<String> {
    let mut matching_sigs: Vec<String> = Vec::new();
    let mut matching_fns: Vec<String> = Vec::new();
    for line in clif.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("sig") {
            if let Some((id, sig)) = rest.split_once(" = ")
                && sig_pred(sig)
            {
                matching_sigs.push(format!("sig{}", id.trim()));
            }
        } else if let Some(rest) = line.strip_prefix("fn") {
            // rhs is "<extern ref> <sig>", e.g. "u0:1 sig0".
            if let Some((id, rhs)) = rest.split_once(" = ")
                && let Some(sig) = rhs.split_whitespace().nth(1)
                && matching_sigs.iter().any(|s| s == sig)
            {
                matching_fns.push(format!("fn{}", id.trim()));
            }
        }
    }
    matching_fns
}

/// Count call sites to any of `fns` within a CLIF text region.
fn count_calls_to(region: &str, fns: &[String]) -> usize {
    region
        .lines()
        .filter(|line| fns.iter().any(|f| line.contains(&format!("call {}(", f))))
        .count()
}

/// The CLIF text of the entry block (`block0:` up to the next block
/// header) of a single-function dump.
fn clif_entry_block(clif: &str) -> &str {
    let start = clif.find("block0:").expect("dump has an entry block");
    let rest = &clif[start..];
    match rest[1..].find("\nblock") {
        Some(end) => &rest[..end + 1],
        None => rest,
    }
}

#[test]
fn clif_str_literal_materialized_once_per_function() {
    // A string literal is pure .rodata packing with no side effects,
    // so each distinct literal must be materialized exactly once per
    // function — hoisted into the entry block — instead of emitting a
    // fresh ryo_str_from_literal call at every use (loop bodies
    // included). `ryo_str_from_literal(ptr, len) -> i128` is the only
    // (i64, i64) -> i128 runtime call this program can emit.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "clif_lit.ryo",
        "fn main():\n\ttext: str = \"the quick brown fox\"\n\tmut count = 0\n\tfor i in range(0, 16):\n\t\tif text[i:i+3] == \"fox\":\n\t\t\tcount = count + 1\n\tprint(\"fox\")\n\tprint(int_to_str(count))\n",
    );

    let output = run_ryo_command(&["ir", "--emit=clif", "clif_lit.ryo"], &test_file)
        .expect("Failed to run ryo ir --emit=clif");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    let is_from_literal = |sig: &str| sig.starts_with("(i64, i64) -> i128");
    let from_literal_fns = clif_fns_matching_sig(&stdout, is_from_literal);
    // Two distinct literals ("the quick brown fox", "fox") — "fox"
    // appears at two source sites but must materialize only once.
    assert_eq!(
        count_calls_to(&stdout, &from_literal_fns),
        2,
        "each distinct literal must be materialized exactly once per function: {}",
        stdout
    );
    assert_eq!(
        count_calls_to(clif_entry_block(&stdout), &from_literal_fns),
        2,
        "literal materializations must be hoisted out of the loop into the entry block: {}",
        stdout
    );
}

#[test]
fn clif_static_cap_str_free_is_elided() {
    // ryo_str_free(ptr, 0) returns immediately for literal-backed
    // strings (cap == 0 is the .rodata sentinel), so codegen must not
    // emit the call when the freed value's cap is statically 0 at the
    // emission site. In this all-literal function the only remaining
    // two-word void runtime call is ryo_print.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(
        temp_dir.path(),
        "clif_free.ryo",
        "fn main():\n\ts: str = \"hello\"\n\tprint(s)\n",
    );

    let output = run_ryo_command(&["ir", "--emit=clif", "clif_free.ryo"], &test_file)
        .expect("Failed to run ryo ir --emit=clif");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    let is_two_word_void = |sig: &str| sig.starts_with("(i64, i64)") && !sig.contains("->");
    let two_word_void_fns = clif_fns_matching_sig(&stdout, is_two_word_void);
    assert_eq!(
        count_calls_to(&stdout, &two_word_void_fns),
        1,
        "only ryo_print may remain; ryo_str_free on a cap=0 literal must be elided: {}",
        stdout
    );
}

#[test]
fn ir_emit_order_is_pipeline_not_flag() {
    // Section order must be AST → UIR → TIR → CLIF regardless of
    // the order in which flags are listed. We exercise this two
    // ways:
    //   1. Two shuffled permutations of the full four-section list
    //      must produce **byte-identical** output.
    //   2. Within a single run, banners must appear in pipeline
    //      order — ast_idx < uir_idx < tir_idx < clif_idx.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "order.ryo", "x = 1\n");

    // A handful of deliberately-shuffled permutations. Not
    // exhaustive (24 total) — a representative selection that
    // covers each section appearing both first and last is enough
    // to catch a regression that respects flag order.
    let perms = [
        "ast,uir,tir,clif",
        "clif,tir,uir,ast",
        "tir,ast,clif,uir",
        "uir,clif,ast,tir",
    ];

    let outputs: Vec<_> = perms
        .iter()
        .map(|p| {
            let arg = format!("--emit={}", p);
            let out = run_ryo_command(&["ir", &arg, "order.ryo"], &test_file)
                .unwrap_or_else(|e| panic!("ryo ir --emit={}: {}", p, e));
            assert!(
                out.status.success(),
                "--emit={} failed: {}",
                p,
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        })
        .collect();

    // (1) flag order must not change output.
    for (i, perm) in perms.iter().enumerate().skip(1) {
        assert_eq!(
            outputs[0], outputs[i],
            "--emit=ast,uir,tir,clif and --emit={} produced different output",
            perm
        );
    }

    // (2) banners appear in pipeline order within a run.
    let stdout = String::from_utf8_lossy(&outputs[0]);
    let ast_idx = stdout.find("[AST]").expect("AST banner");
    let uir_idx = stdout.find("[UIR]").expect("UIR banner");
    let tir_idx = stdout.find("[TIR]").expect("TIR banner");
    let clif_idx = stdout.find("[Cranelift IR]").expect("CLIF banner");
    assert!(
        ast_idx < uir_idx && uir_idx < tir_idx && tir_idx < clif_idx,
        "sections out of pipeline order \
         (ast={}, uir={}, tir={}, clif={}):\n{}",
        ast_idx,
        uir_idx,
        tir_idx,
        clif_idx,
        stdout
    );
}

#[test]
fn ir_emit_uir_with_sema_error_still_prints_uir() {
    // A type-error fixture: `--emit=uir` should print the UIR
    // (astgen succeeded) and exit 0 — sema is never run, so its
    // diagnostics never fire.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "bad.ryo", "x = -true\n");

    let output = run_ryo_command(&["ir", "--emit=uir", "bad.ryo"], &test_file)
        .expect("ryo ir --emit=uir on bad source");
    assert!(
        output.status.success(),
        "UIR-only run should not run sema; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[UIR]"), "missing UIR: {}", stdout);
    assert!(stdout.contains("= neg %"), "missing neg op: {}", stdout);
}

#[test]
fn ir_emit_tir_prints_partial_tir_with_unreachable_on_sema_error() {
    // §4.5: sema emits `Unreachable` in place of failed expressions
    // and keeps going. `--emit=tir` deliberately renders that
    // partial TIR — the whole point of the flag is debugging sema.
    // Driver still exits non-zero because the sink has errors.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let test_file = create_test_file(temp_dir.path(), "partial.ryo", "x = -true\n");

    let output = run_ryo_command(&["ir", "--emit=tir", "partial.ryo"], &test_file)
        .expect("ryo ir --emit=tir on bad source");
    assert!(
        !output.status.success(),
        "should exit non-zero on sema error"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[TIR]"), "TIR banner missing: {}", stdout);
    assert!(
        stdout.contains("unreachable"),
        "Unreachable not rendered: {}",
        stdout
    );
}

#[test]
fn test_ryo_ir_surfaces_warnings_on_success() {
    // Regression: `ryo ir` used to call sema + ownership
    // against a sink and only render on the error path, so any
    // warnings (W0001 DeadStore, W0002 RedundantMove) emitted on
    // a successful run were silently dropped. After the
    // single-tail-block refactor, `ryo ir` should surface the same
    // diagnostics on stderr that `ryo run` / `ryo build` already
    // do (cf. test_redundant_move_on_int_warns).
    let temp_dir = TempDir::new().expect("temp");
    let code = "fn f(move x: int):\n\tprint(int_to_str(x))\n\nf(42)";
    let test_file = create_test_file(temp_dir.path(), "ir_w0002.ryo", code);
    let output = run_ryo_command(&["ir", "--emit=tir", "ir_w0002.ryo"], &test_file).expect("run");
    assert!(
        output.status.success(),
        "STDERR: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("W0002"),
        "expected W0002 from ryo ir on stderr: {}",
        stderr
    );
}

#[test]
fn parse_error_recovers_and_sema_errors_co_surface() {
    // R10: one syntax error must not discard the rest of the file.
    // The parser recovers at the next statement boundary, so the
    // type error on the following line still surfaces — both in one
    // run, and the compile still fails.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tx = = 1\n\ty: int = \"hi\"\n";
    let test_file = create_test_file(temp_dir.path(), "multi_error.ryo", code);

    let output = run_ryo_command(&["run", "multi_error.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "a file with parse + type errors must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("E0100"),
        "should emit E0100 for the parse error, got: {}",
        stderr
    );
    assert!(
        stderr.contains("E0012"),
        "should emit E0012 for the type error despite the parse error, got: {}",
        stderr
    );
}

#[test]
fn broken_block_header_body_does_not_leak_into_enclosing_scope() {
    // A broken `fn` header must swallow its indented body as one error
    // region: the body's type error must NOT surface (the body is not
    // analyzed at top level), and exactly one parse diagnostic is
    // emitted — no second one for a dangling `Dedent`.
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn broken(:\n\ty: int = \"hi\"\nfn main():\n\tz = 3\n";
    let test_file = create_test_file(temp_dir.path(), "broken_header.ryo", code);

    let output = run_ryo_command(&["run", "broken_header.ryo"], &test_file)
        .expect("Failed to run ryo command");

    assert!(
        !output.status.success(),
        "a file with a broken block header must be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("E0100").count(),
        1,
        "expected exactly one parse error (no dangling-Dedent diagnostic), got: {}",
        stderr
    );
    assert!(
        !stderr.contains("E0012"),
        "the swallowed body must not be analyzed at the enclosing scope, got: {}",
        stderr
    );
}
