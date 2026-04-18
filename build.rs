use std::env;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    let short_hash = get_git_short_hash().unwrap_or_else(|| "unknown".to_string());
    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let version = if short_hash == "unknown" {
        pkg_version
    } else {
        format!("{} +g{}", pkg_version, short_hash)
    };
    println!("cargo:rustc-env=RYO_VERSION={}", version);
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
