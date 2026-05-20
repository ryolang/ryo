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

    // Runtime archive path — set by `just build` or default location.
    let runtime_path = env::var("RYO_RUNTIME_LIB").unwrap_or_else(|_| {
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
        let target_dir =
            env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| format!("{manifest_dir}/target"));
        let path = std::path::PathBuf::from(&target_dir)
            .join("release")
            .join("libryo_runtime.a");
        if !path.exists() {
            panic!(
                "\n\nlibryo_runtime.a not found at {}\n\
                 Run `just build` or `cargo build -p ryo-runtime --release` first.\n\n",
                path.display()
            );
        }
        path.to_str().unwrap().to_string()
    });

    println!("cargo:rustc-env=RYO_RUNTIME_LIB={runtime_path}");
    println!("cargo:rerun-if-env-changed=RYO_RUNTIME_LIB");
    println!("cargo:rerun-if-changed={runtime_path}");

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
