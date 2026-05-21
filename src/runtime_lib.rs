use std::fs;
use std::io;
use std::path::PathBuf;

const RYO_RUNTIME_LIB: &[u8] = include_bytes!(env!("RYO_RUNTIME_LIB"));

pub fn extract_runtime_to_temp() -> Result<PathBuf, io::Error> {
    let dir = std::env::temp_dir().join(format!("ryo-runtime-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let path = dir.join("libryo_runtime.a");
    fs::write(&path, RYO_RUNTIME_LIB)?;
    Ok(path)
}

pub fn cleanup_runtime_temp(path: &std::path::Path) {
    let _ = fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
}
