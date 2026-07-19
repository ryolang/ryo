//! Shared build-script helpers for the Ryo workspace. Currently home to
//! the `ryo-runtime` static archive build, previously duplicated
//! verbatim between `ryo/build.rs` and `ryo-backend/build.rs`.

use std::path::{Path, PathBuf};

/// Resolve the `ryo-runtime` static archive for the current cargo
/// profile and target triple, building it on demand, and return its
/// path as a `String`.
///
/// - Profile: Cargo's build-script `PROFILE` contract (`debug` or
///   `release`); `release` builds pass `--release`.
/// - Target: Cargo's build-script `TARGET` triple, forwarded as
///   `--target` so cross-compilation produces an archive for the outer
///   target, not the host.
/// - Target dir: `$CARGO_TARGET_DIR/runtime-build` when
///   `CARGO_TARGET_DIR` is set, else `<root_dir>/target/runtime-build`.
///   A separate target directory avoids cargo lock deadlocks when this
///   runs inside a build script under cargo.
/// - The build always runs: cargo's own change detection no-ops when
///   fresh, so the archive can never go stale the way the old
///   `if !path.exists()` guard allowed.
///
/// Panics when the build fails or the resolved path is not UTF-8.
pub fn ensure_runtime_archive(root_dir: &Path) -> String {
    let profile = std::env::var("PROFILE").expect("cargo sets PROFILE for build scripts");
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    let custom_target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root_dir.join("target"))
        .join("runtime-build");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = std::process::Command::new(&cargo);
    cmd.arg("build")
        .arg("-p")
        .arg("ryo-runtime")
        .arg("--target")
        .arg(&target)
        .arg("--target-dir")
        .arg(&custom_target_dir);
    if profile == "release" {
        cmd.arg("--release");
    }
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn `cargo build -p ryo-runtime`: {e}"));
    if !status.success() {
        panic!("`cargo build -p ryo-runtime` failed with {status}");
    }
    // With an explicit `--target`, cargo namespaces the output under the
    // triple: <target-dir>/<triple>/<profile>/.
    let path = custom_target_dir
        .join(&target)
        .join(&profile)
        .join("libryo_runtime.a");
    if !path.exists() {
        panic!(
            "libryo_runtime.a still missing at {} after build attempt",
            path.display()
        );
    }
    // Safely check if path contains non-UTF-8 characters, providing clear instructions if so.
    match path.to_str() {
        Some(s) => s.to_string(),
        None => {
            panic!(
                "The resolved runtime library path at '{}' contains non-UTF-8 characters. \
                 Please set the RYO_RUNTIME_LIB environment variable explicitly to override it.",
                path.display()
            );
        }
    }
}
