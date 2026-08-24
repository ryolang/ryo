mod common;
use common::*;

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

// =============================================================================
// AOT Build + Run Verification Tests
// =============================================================================

#[test]
fn assert_true_aot_run_succeeds() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(true, \"aot ok\")\n\tprint(\"built\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "assert_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert!(
        run_output.status.success(),
        "compiled binary should exit 0. stderr: {}",
        String::from_utf8_lossy(&run_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert!(
        stdout.contains("built"),
        "expected 'built' in stdout, got: {}",
        stdout
    );
}

#[test]
fn print_aot_run_stdout_exact() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tprint(\"Hello, World!\\n\")\n";
    let test_file = create_test_file(temp_dir.path(), "print_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "print_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert!(run_output.status.success(), "compiled binary should exit 0");
    let stdout = String::from_utf8_lossy(&run_output.stdout);
    assert_eq!(
        stdout, "Hello, World!\n",
        "binary stdout must be exactly the printed bytes"
    );
}

#[test]
fn assert_false_aot_run_exits_101() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tassert(false, \"boom\")\n";
    let test_file = create_test_file(temp_dir.path(), "assert_false_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "assert_false_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert_eq!(
        run_output.status.code(),
        Some(101),
        "binary should exit 101 on assert failure"
    );
    let stderr = String::from_utf8_lossy(&run_output.stderr);
    assert!(
        stderr.contains("assertion failed")
            && stderr.contains("in main()")
            && stderr.contains("boom"),
        "stderr should contain formatted message with location, got: {}",
        stderr
    );
}

#[test]
fn panic_with_emojis_aot_run_exits_101() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpanic(\"🔥 boom 💥\")\n";
    let test_file = create_test_file(temp_dir.path(), "panic_emoji_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "panic_emoji_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert_eq!(
        run_output.status.code(),
        Some(101),
        "binary should exit 101 on panic"
    );
    let stderr = String::from_utf8_lossy(&run_output.stderr);
    assert!(
        stderr.contains("panicked")
            && stderr.contains("in main()")
            && stderr.contains("🔥 boom 💥"),
        "stderr should contain panic message with emojis, got: {}",
        stderr
    );
}

#[test]
fn panic_aot_run_exits_101() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tpanic(\"explicit\")\n";
    let test_file = create_test_file(temp_dir.path(), "panic_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "panic_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to execute compiled binary");

    assert_eq!(
        run_output.status.code(),
        Some(101),
        "binary should exit 101 on panic"
    );
    let stderr = String::from_utf8_lossy(&run_output.stderr);
    assert!(
        stderr.contains("panicked") && stderr.contains("in main()") && stderr.contains("explicit"),
        "stderr should contain panic message with location, got: {}",
        stderr
    );
}

#[test]
fn while_loop_aot_build_and_run() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let code = "fn main():\n\tmut i = 10\n\twhile i > 0:\n\t\ti -= 1\n\tassert(i == 0, \"should count down to 0\")\n";
    let test_file = create_test_file(temp_dir.path(), "while_aot.ryo", code);

    let build_output = run_ryo_build(&test_file, temp_dir.path());
    assert!(
        build_output.status.success(),
        "build should succeed. STDERR: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    let binary_path = exe_path(temp_dir.path(), "while_aot");
    let run_output = Command::new(&binary_path)
        .output()
        .expect("Failed to run compiled binary");

    assert!(run_output.status.success(), "compiled binary should exit 0");
}

#[test]
fn test_benchmark_files_aot_compile_and_run() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");

    // Locate the benchmark files relative to the workspace root.
    // CARGO_MANIFEST_DIR is ryo/ package directory, so its parent is the workspace root.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().expect("workspace root");

    let fib_file = workspace_root.join("benchmarks/fibonacci/fib.ryo");
    let eager_file = workspace_root.join("benchmarks/eager_destruction/eager_destruction.ryo");

    assert!(fib_file.exists(), "fib.ryo not found at {:?}", fib_file);
    assert!(
        eager_file.exists(),
        "eager_destruction.ryo not found at {:?}",
        eager_file
    );

    // Copy the sources into the temp dir: `ryo build` writes the
    // binary next to the source, and building in-place would
    // overwrite the committed benchmark binaries in the repo.
    let fib_file = temp_dir.path().join("fib.ryo");
    let eager_file = temp_dir.path().join("eager_destruction.ryo");
    std::fs::copy(
        workspace_root.join("benchmarks/fibonacci/fib.ryo"),
        &fib_file,
    )
    .expect("copy fib.ryo");
    std::fs::copy(
        workspace_root.join("benchmarks/eager_destruction/eager_destruction.ryo"),
        &eager_file,
    )
    .expect("copy eager_destruction.ryo");

    // 1. Compile and run fibonacci
    let fib_build = run_ryo_build(&fib_file, temp_dir.path());
    assert!(
        fib_build.status.success(),
        "fib.ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&fib_build.stderr)
    );
    let fib_exe = exe_path(temp_dir.path(), "fib");
    let fib_run = Command::new(&fib_exe)
        .output()
        .expect("Failed to run compiled fib");
    assert!(
        fib_run.status.success(),
        "compiled fib run failed. STDERR: {}",
        String::from_utf8_lossy(&fib_run.stderr)
    );

    // 2. Compile and run eager_destruction
    let eager_build = run_ryo_build(&eager_file, temp_dir.path());
    assert!(
        eager_build.status.success(),
        "eager_destruction.ryo build failed. STDERR: {}",
        String::from_utf8_lossy(&eager_build.stderr)
    );
    let eager_exe = exe_path(temp_dir.path(), "eager_destruction");
    let eager_run = Command::new(&eager_exe)
        .output()
        .expect("Failed to run compiled eager_destruction");
    assert!(
        eager_run.status.success(),
        "compiled eager_destruction run failed. STDERR: {}",
        String::from_utf8_lossy(&eager_run.stderr)
    );
}

#[test]
fn test_examples_parse() {
    // Sweep: every top-level examples/*.ryo must parse. Local
    // complement to the upstream Examples CI workflow — no CI
    // dependency, and it guards examples/string_slices.ryo (Task 11).
    // `read_dir` is non-recursive, so examples/future/ (not yet
    // supported syntax) is excluded naturally.
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("examples");
    let mut count = 0;
    for entry in std::fs::read_dir(&examples).expect("examples dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("ryo") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let output = run_ryo_command(&["parse", &name], &path).expect("run ryo parse");
        assert!(
            output.status.success(),
            "examples/{} failed to parse:\n{}",
            name,
            String::from_utf8_lossy(&output.stderr)
        );
        count += 1;
    }
    assert!(count > 10, "expected examples to exist, found {count}");
}
