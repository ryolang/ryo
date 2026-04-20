use crate::errors::CompilerError;
use std::fs;
use std::path::PathBuf;
use xz2::read::XzDecoder;

const ZIG_VERSION: &str = "0.16.0";

pub(crate) fn ensure_zig() -> Result<PathBuf, CompilerError> {
    let zig_path = zig_binary_path()?;
    if zig_path.exists() {
        return Ok(zig_path);
    }
    download_zig()?;
    if zig_path.exists() {
        Ok(zig_path)
    } else {
        Err(CompilerError::ToolchainError(
            "Zig binary not found after download".into(),
        ))
    }
}

pub(crate) fn is_installed() -> bool {
    zig_binary_path().is_ok_and(|p| p.exists())
}

pub(crate) fn pinned_version() -> &'static str {
    ZIG_VERSION
}

fn zig_binary_path() -> Result<PathBuf, CompilerError> {
    let base = toolchain_dir()?;
    Ok(base.join(format!("zig-{ZIG_VERSION}")).join("zig"))
}

fn toolchain_dir() -> Result<PathBuf, CompilerError> {
    let home = dirs::home_dir().ok_or_else(|| {
        CompilerError::ToolchainError("Could not determine home directory".into())
    })?;
    Ok(home.join(".ryo").join("toolchain"))
}

fn zig_target() -> Result<&'static str, CompilerError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-macos"),
        ("linux", "x86_64") => Ok("x86_64-linux"),
        ("linux", "aarch64") => Ok("aarch64-linux"),
        (os, arch) => Err(CompilerError::ToolchainError(format!(
            "Unsupported platform: {os}-{arch}"
        ))),
    }
}

fn download_zig() -> Result<(), CompilerError> {
    let target = zig_target()?;
    let url =
        format!("https://ziglang.org/download/{ZIG_VERSION}/zig-{target}-{ZIG_VERSION}.tar.xz");
    let dest = toolchain_dir()?;

    let extracted_name = format!("zig-{target}-{ZIG_VERSION}");
    let desired_name = format!("zig-{ZIG_VERSION}");
    let temp_name = format!(".zig-{ZIG_VERSION}-downloading");

    let temp_path = dest.join(&temp_name);
    let desired_path = dest.join(&desired_name);

    fs::remove_dir_all(&temp_path).ok();

    fs::create_dir_all(&dest).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to create toolchain directory: {e}"))
    })?;

    eprintln!("Zig toolchain not found. Downloading zig {ZIG_VERSION} for {target}...");

    let response = ureq::get(&url)
        .call()
        .map_err(|e| CompilerError::ToolchainError(format!("Failed to download Zig: {e}")))?;

    eprintln!("Extracting...");

    // Stream: HTTP response → XZ decompressor → tar extractor (no large buffers)
    fs::create_dir_all(&temp_path).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to create temp directory: {e}"))
    })?;

    let decompressor = XzDecoder::new(response.into_body().into_reader());
    let mut archive = tar::Archive::new(decompressor);
    archive.unpack(&temp_path).map_err(|e| {
        fs::remove_dir_all(&temp_path).ok();
        CompilerError::ToolchainError(format!("Failed to extract Zig archive: {e}"))
    })?;

    // The tarball extracts to zig-{target}-{version}/ inside temp dir
    let inner_path = temp_path.join(&extracted_name);
    let source = if inner_path.exists() {
        inner_path
    } else {
        temp_path.clone()
    };

    fs::remove_dir_all(&desired_path).ok();

    fs::rename(&source, &desired_path).map_err(|e| {
        fs::remove_dir_all(&temp_path).ok();
        CompilerError::ToolchainError(format!("Failed to install Zig: {e}"))
    })?;

    fs::remove_dir_all(&temp_path).ok();

    eprintln!("Zig {ZIG_VERSION} installed to {}", desired_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zig_target_valid() {
        let target = zig_target().unwrap();
        assert!(["aarch64-macos", "x86_64-linux", "aarch64-linux"].contains(&target));
    }

    #[test]
    fn test_zig_binary_path_contains_version() {
        let path = zig_binary_path().unwrap();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains(&format!("zig-{ZIG_VERSION}")));
        assert!(path_str.ends_with("zig"));
    }

    #[test]
    fn test_toolchain_dir_under_home() {
        let dir = toolchain_dir().unwrap();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains(".ryo"));
        assert!(dir_str.ends_with("toolchain"));
    }
}
