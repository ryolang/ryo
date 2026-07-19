use sha2::{Digest, Sha256};
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let root_dir = PathBuf::from(&manifest_dir).parent().unwrap().to_path_buf();

    let git_head_path = root_dir.join(".git/HEAD");
    println!("cargo:rerun-if-changed={}", git_head_path.display());
    if let Some(git_ref) = resolve_git_ref(&root_dir) {
        println!(
            "cargo:rerun-if-changed={}",
            root_dir.join(git_ref).display()
        );
    }
    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let short_hash = get_git_short_hash();
    let commit_date = get_git_commit_date();
    let version = match (short_hash, commit_date) {
        (Some(hash), Some(date)) => format!("{pkg_version}-dev.{date}+{hash}"),
        (Some(hash), None) => format!("{pkg_version}-dev+{hash}"),
        _ => pkg_version,
    };
    println!("cargo:rustc-env=RYO_VERSION={version}");

    // Runtime archive path. Honor RYO_RUNTIME_LIB if set (used by downstream
    // packagers). Otherwise build it on demand using the current cargo profile
    // in a separate target directory to avoid cargo lock deadlocks.
    let runtime_path = env::var("RYO_RUNTIME_LIB").unwrap_or_else(|_| {
        let raw_profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
        let profile = match raw_profile.as_str() {
            "release" | "production" | "prod" => "release",
            "debug" | "dev" => "debug",
            _ => {
                let opt_level = env::var("OPT_LEVEL").unwrap_or_else(|_| "0".to_string());
                if opt_level != "0" { "release" } else { "debug" }
            }
        };
        // Always rebuild via cargo: its own change detection no-ops when
        // fresh, so the archive can never go stale the way the old
        // `if !path.exists()` guard allowed (I-113). A separate target
        // directory avoids cargo lock deadlocks.
        let custom_target_dir = root_dir.join("target/runtime-build");
        let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let mut cmd = std::process::Command::new(&cargo);
        cmd.arg("build")
            .arg("-p")
            .arg("ryo-runtime")
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
        let path = custom_target_dir.join(profile).join("libryo_runtime.a");
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
    });

    println!("cargo:rustc-env=RYO_RUNTIME_LIB={runtime_path}");
    println!("cargo:rerun-if-env-changed=RYO_RUNTIME_LIB");
    println!("cargo:rerun-if-changed={runtime_path}");

    let runtime_bytes = std::fs::read(&runtime_path).unwrap_or_else(|e| {
        panic!("failed to read runtime lib at {}: {}", runtime_path, e);
    });
    let mut hasher = Sha256::new();
    hasher.update(&runtime_bytes);
    let hash_result = hasher.finalize();
    let hash_string = format!("{:x}", hash_result);
    println!("cargo:rustc-env=RYO_RUNTIME_HASH={hash_string}");

    let runtime_src = root_dir.join("runtime/src");
    println!("cargo:rerun-if-changed={}", runtime_src.display());
}

fn resolve_git_ref(root_dir: &std::path::Path) -> Option<String> {
    let head = std::fs::read_to_string(root_dir.join(".git/HEAD")).ok()?;
    let head = head.trim();
    head.strip_prefix("ref: ")
        .map(|refpath| format!(".git/{refpath}"))
}

fn get_git_short_hash() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()?;
    if output.status.success() {
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !hash.is_empty() {
            return Some(hash);
        }
    }
    None
}

fn get_git_commit_date() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["log", "-1", "--format=%cd", "--date=format:%Y%m%d"])
        .output()
        .ok()?;
    if output.status.success() {
        let date = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !date.is_empty() {
            return Some(date);
        }
    }
    None
}
