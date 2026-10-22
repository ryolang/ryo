use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn runtime_lib_path() -> PathBuf {
    PathBuf::from(env!("RYO_RUNTIME_LIB"))
}

fn zig_path() -> PathBuf {
    // Queried by every zig-linked fixture; one `ryo toolchain status --path`
    // spawn per suite process instead of one per fixture.
    static ZIG_PATH: OnceLock<PathBuf> = OnceLock::new();
    ZIG_PATH
        .get_or_init(|| {
            let output = Command::new(env!("CARGO_BIN_EXE_ryo"))
                .args(["toolchain", "status", "--path"])
                .output()
                .expect("failed to execute ryo toolchain status --path");
            assert!(
                output.status.success(),
                "failed to get zig path from ryo: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string())
        })
        .clone()
}

/// Whether a smoke suite may run in this environment: under CI the
/// dedicated `asan`/`valgrind` jobs are the sole runners of these suites
/// and set `RYO_SMOKE_LANE=1`; the general `cargo test --workspace` lane —
/// which would duplicate that work per matrix leg — skips them. Local
/// runs (`CI` unset) and the Docker Linux script always run the suites,
/// preserving today's local behavior. The exact-value rule mirrors
/// `require_valgrind`'s opt-out, so `RYO_SMOKE_LANE=0`/`""` still skip.
/// Note the skip reason goes through libtest's captured stderr: it is
/// only visible with `--nocapture` (same as `require_valgrind`'s
/// messages), which the dedicated CI lanes pass.
pub fn smoke_lane_active(name: &str) -> bool {
    if std::env::var("RYO_SMOKE_LANE").as_deref() == Ok("1") || std::env::var_os("CI").is_none() {
        return true;
    }
    eprintln!(
        "skipping {name}: smoke suite runs in the dedicated CI lane \
         (set RYO_SMOKE_LANE=1 to run it here)"
    );
    false
}

/// Which compiler performs the fixture re-link.
///
/// `Zig` is the default (matches the AOT pipeline's managed toolchain).
/// `HostCc` is the host C compiler (`cc`): required for sanitizer
/// re-links, because zig cc accepts `-fsanitize=address` without
/// complaint but links no ASan runtime on any platform (verified on
/// macOS and linux-x86_64/aarch64 — the symbols assertion in
/// asan_smoke.rs exists because of this), while gcc/clang on a glibc
/// host ship a working ASan.
pub enum TestLinker {
    Zig,
    HostCc,
}

/// Compiles a Ryo program and re-links the object file.
///
/// Returns the temporary directory (which must be kept alive by the caller)
/// and the path to the compiled executable.
pub fn build_and_link(
    source: &str,
    name: &str,
    extra_link_args: &[&str],
) -> (tempfile::TempDir, PathBuf) {
    build_and_link_with(source, name, extra_link_args, TestLinker::Zig)
}

/// Same as [`build_and_link`] but re-links with the host C compiler —
/// see [`TestLinker::HostCc`]. Used by the sanitizer smoke suites.
pub fn build_and_link_host_cc(
    source: &str,
    name: &str,
    extra_link_args: &[&str],
) -> (tempfile::TempDir, PathBuf) {
    build_and_link_with(source, name, extra_link_args, TestLinker::HostCc)
}

fn build_and_link_with(
    source: &str,
    name: &str,
    extra_link_args: &[&str],
    linker: TestLinker,
) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src_path = tmp.path().join(format!("{name}.ryo"));
    std::fs::write(&src_path, source).expect("write source");

    // Step 1: ryo build (keep obj)
    let status = Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("build")
        .arg(&src_path)
        .env("RYO_KEEP_OBJ", "1")
        .current_dir(tmp.path())
        .status()
        .expect("ryo build");
    assert!(status.success(), "ryo build failed for {name}");

    // Step 2: relink (object extension matches the AOT pipeline:
    // `.obj` on Windows, `.o` elsewhere — see pipeline.rs
    // get_output_filenames)
    let obj = tmp.path().join(format!(
        "{name}.{}",
        if cfg!(windows) { "obj" } else { "o" }
    ));
    let exe = tmp.path().join(format!("{name}_test_binary"));

    let runtime_lib = runtime_lib_path();
    assert!(
        runtime_lib.exists(),
        "runtime archive missing at {} — it is built by ryo-backend's build.rs; run `cargo build` first",
        runtime_lib.display()
    );

    let (compiler, compiler_name): (PathBuf, &str) = match linker {
        TestLinker::Zig => (zig_path(), "zig cc"),
        TestLinker::HostCc => (PathBuf::from("cc"), "host cc"),
    };
    let mut cmd = Command::new(&compiler);
    if matches!(linker, TestLinker::Zig) {
        cmd.arg("cc");
    }
    cmd.args(extra_link_args);
    cmd.arg("-o");
    cmd.arg(&exe);
    cmd.arg(&obj);
    cmd.arg(&runtime_lib);
    let out = cmd.output().expect("relink fixture");
    assert!(
        out.status.success(),
        "{compiler_name} failed with args {:?}:\nstdout: {}\nstderr: {}",
        extra_link_args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    (tmp, exe)
}

// Helper function to run ryo compiler and capture output
pub fn run_ryo_command(
    args: &[&str],
    file_path: &Path,
) -> Result<std::process::Output, std::io::Error> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ryo"));
    cmd.args(&args[..args.len() - 1]) // All args except the filename
        .arg(file_path); // Use absolute path for the file
    cmd.output()
}

// Helper function to create a temporary test file
pub fn create_test_file(dir: &Path, filename: &str, content: &str) -> std::path::PathBuf {
    let file_path = dir.join(filename);
    std::fs::write(&file_path, content).expect("Failed to write test file");
    file_path
}

/// Path to an AOT-built binary, with the platform `.exe` suffix.
pub fn exe_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX))
}

/// Run `ryo build` and return the path to the compiled binary.
///
/// The AOT pipeline writes the binary next to the source file. Tests
/// place (or copy) the source into a dedicated output directory so the
/// artifact lands somewhere predictable and is cleaned up with the
/// `TempDir`.
pub fn run_ryo_build(source_file: &Path, out_dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ryo"))
        .arg("build")
        .arg(source_file)
        .current_dir(out_dir)
        .output()
        .expect("Failed to run ryo build command")
}
