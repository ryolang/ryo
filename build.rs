use sha2::{Digest, Sha256};
use std::env;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Some(git_ref) = resolve_git_ref() {
        println!("cargo:rerun-if-changed={git_ref}");
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
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
        let target_dir =
            env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| format!("{manifest_dir}/target"));
        let raw_profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
        // Mapping rules for Cargo profile resolution:
        // - Known "release", "production", and "prod" profiles map to "release".
        // - Known "debug" and "dev" profiles map to "debug".
        // - For unrecognized/custom profiles, we consult OPT_LEVEL and treat any
        //   non-"0" optimization level as "release" (since custom optimized profiles
        //   typically build under optimized target layouts).
        // NOTE: Custom profiles with debug = true but OPT_LEVEL > 0 (e.g., opt-level = 1, 2, 3)
        // will be classified as "release", avoiding build directory mismatch surprises.
        let profile = match raw_profile.as_str() {
            "release" | "production" | "prod" => "release",
            "debug" | "dev" => "debug",
            _ => {
                let opt_level = env::var("OPT_LEVEL").unwrap_or_else(|_| "0".to_string());
                if opt_level != "0" { "release" } else { "debug" }
            }
        };
        let mut path = std::path::PathBuf::from(&target_dir)
            .join(profile)
            .join("libryo_runtime.a");
        if !path.exists() {
            // Build the runtime archive in-process in a separate target directory to avoid deadlocks.
            let custom_target_dir =
                std::path::PathBuf::from(&manifest_dir).join("target/runtime-build");
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
            if status.success() {
                path = custom_target_dir.join(profile).join("libryo_runtime.a");
            }
        }
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

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let runtime_src = std::path::PathBuf::from(&manifest_dir).join("runtime/src");
    println!("cargo:rerun-if-changed={}", runtime_src.display());
}

fn resolve_git_ref() -> Option<String> {
    let head = std::fs::read_to_string(".git/HEAD").ok()?;
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
