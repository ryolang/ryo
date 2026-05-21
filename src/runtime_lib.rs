use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::PathBuf;

const RYO_RUNTIME_LIB: &[u8] = include_bytes!(env!("RYO_RUNTIME_LIB"));

fn cache_dir() -> PathBuf {
    dirs::home_dir()
        .expect("cannot determine home directory")
        .join(".ryo")
        .join("cache")
}

fn content_hash() -> String {
    let hash = Sha256::digest(RYO_RUNTIME_LIB);
    format!("{:x}", hash)[..16].to_string()
}

pub fn extract_runtime_to_temp() -> Result<PathBuf, io::Error> {
    let dir = cache_dir();
    let hash = content_hash();
    let path = dir.join(format!("libryo_runtime-{}.a", hash));

    if path.exists() {
        return Ok(path);
    }

    fs::create_dir_all(&dir)?;
    // Write to a temp name and rename for atomicity
    let tmp_path = dir.join(format!("libryo_runtime-{}.a.tmp.{}", hash, std::process::id()));
    fs::write(&tmp_path, RYO_RUNTIME_LIB)?;
    fs::rename(&tmp_path, &path)?;
    Ok(path)
}

pub fn cleanup_runtime_temp(_path: &std::path::Path) {
    // Cached — no cleanup needed. The file persists for future builds.
}
