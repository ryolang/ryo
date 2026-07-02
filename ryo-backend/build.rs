use sha2::{Digest, Sha256};
use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let root_dir = PathBuf::from(&manifest_dir).parent().unwrap().to_path_buf();

    // Runtime archive path. Honor RYO_RUNTIME_LIB if set (used by downstream
    // packagers). Otherwise build it on demand using the current cargo profile
    // in a separate target directory to avoid cargo lock deadlocks.
    let runtime_path = env::var("RYO_RUNTIME_LIB").unwrap_or_else(|_| {
        let target_dir =
            env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| {
                root_dir.join("target").to_string_lossy().to_string()
            });
        let raw_profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
        let profile = match raw_profile.as_str() {
            "release" | "production" | "prod" => "release",
            "debug" | "dev" => "debug",
            _ => {
                let opt_level = env::var("OPT_LEVEL").unwrap_or_else(|_| "0".to_string());
                if opt_level != "0" { "release" } else { "debug" }
            }
        };
        let mut path = PathBuf::from(&target_dir)
            .join(profile)
            .join("libryo_runtime.a");
        if !path.exists() {
            // Build the runtime archive in-process in a separate target directory to avoid deadlocks.
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

    let runtime_src = root_dir.join("runtime/src");
    println!("cargo:rerun-if-changed={}", runtime_src.display());
}
