use std::fs;
use std::io;
use std::path::PathBuf;

const RYO_RUNTIME_LIB: &[u8] = include_bytes!(env!("RYO_RUNTIME_LIB"));

fn cache_dir() -> Result<PathBuf, io::Error> {
    dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot determine home directory"))
        .map(|h| h.join(".ryo").join("cache"))
}

fn content_hash() -> String {
    env!("RYO_RUNTIME_HASH")[..16].to_string()
}

/// Cache filename for the extracted archive: the embedded bytes are a
/// COFF archive on Windows (`.lib`) and an ELF/Mach-O archive
/// elsewhere (`.a`). Host-native only — no cross-compile naming yet.
fn extracted_name(hash: &str) -> String {
    if cfg!(windows) {
        format!("ryo_runtime-{hash}.lib")
    } else {
        format!("libryo_runtime-{hash}.a")
    }
}

pub fn extract_runtime_to_temp() -> Result<PathBuf, io::Error> {
    let dir = cache_dir()?;
    let hash = content_hash();
    let path = dir.join(extracted_name(&hash));

    if path.exists() {
        return Ok(path);
    }

    fs::create_dir_all(&dir)?;
    // Write to a temp name and rename for atomicity
    let tmp_path = dir.join(format!(
        "{}.tmp.{}",
        extracted_name(&hash),
        std::process::id()
    ));
    fs::write(&tmp_path, RYO_RUNTIME_LIB)?;
    fs::rename(&tmp_path, &path)?;
    Ok(path)
}

pub fn cleanup_runtime_temp(_path: &std::path::Path) {
    // Cached — no cleanup needed. The file persists for future builds.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracted_name_matches_host() {
        let name = extracted_name("0123456789abcdef");
        if cfg!(windows) {
            assert_eq!(name, "ryo_runtime-0123456789abcdef.lib");
        } else {
            assert_eq!(name, "libryo_runtime-0123456789abcdef.a");
        }
    }
}
