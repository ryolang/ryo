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
