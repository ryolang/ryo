use sha2::{Digest, Sha256};
use std::env;
use std::path::PathBuf;

// The runtime-resolution logic (RYO_RUNTIME_LIB fallback, profile mapping,
// in-process `cargo build -p ryo-runtime`) lives in the `build-support`
// crate, shared with `ryo/build.rs`.

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let root_dir = PathBuf::from(&manifest_dir).parent().unwrap().to_path_buf();

    // Runtime archive path. Honor RYO_RUNTIME_LIB if set (used by downstream
    // packagers). Otherwise build it on demand via the shared build-support
    // helper (profile detection + a staleness-safe rebuild in a separate
    // target directory to avoid cargo lock deadlocks).
    let runtime_path = env::var("RYO_RUNTIME_LIB")
        .unwrap_or_else(|_| build_support::ensure_runtime_archive(&root_dir));

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
