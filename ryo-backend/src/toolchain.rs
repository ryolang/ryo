use ryo_core::errors::CompilerError;
use std::fs;
use std::path::{Path, PathBuf};
use xz2::read::XzDecoder;

const ZIG_VERSION: &str = "0.16.0";

pub fn ensure_zig() -> Result<PathBuf, CompilerError> {
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

pub fn is_installed() -> bool {
    zig_binary_path().is_ok_and(|p| p.exists())
}

pub fn pinned_version() -> &'static str {
    ZIG_VERSION
}

fn zig_binary_path() -> Result<PathBuf, CompilerError> {
    let base = toolchain_dir()?;
    Ok(base
        .join(format!("zig-{ZIG_VERSION}"))
        .join(format!("zig{}", std::env::consts::EXE_SUFFIX)))
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
        ("windows", "x86_64") => Ok("x86_64-windows"),
        (os, arch) => Err(CompilerError::ToolchainError(format!(
            "Unsupported platform: {os}-{arch}"
        ))),
    }
}

/// Archive format zig publishes for the given zig target: Windows
/// builds ship as `.zip`, everything else as `.tar.xz`.
fn archive_ext(target: &str) -> &'static str {
    if target.contains("windows") {
        "zip"
    } else {
        "tar.xz"
    }
}

fn download_zig() -> Result<(), CompilerError> {
    let target = zig_target()?;
    let ext = archive_ext(target);
    let url =
        format!("https://ziglang.org/download/{ZIG_VERSION}/zig-{target}-{ZIG_VERSION}.{ext}");
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

    // tar.xz streams response → XZ → tar (no large buffers). The zip
    // path stages the download to a temp file: `ZipArchive` needs
    // `Seek`, and the file is removed with the temp dir afterwards.
    fs::create_dir_all(&temp_path).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to create temp directory: {e}"))
    })?;

    if ext == "zip" {
        let zip_path = temp_path.join("zig-download.zip");
        let mut reader = response.into_body().into_reader();
        let mut file = fs::File::create(&zip_path).map_err(|e| {
            fs::remove_dir_all(&temp_path).ok();
            CompilerError::ToolchainError(format!("Failed to stage zip download: {e}"))
        })?;
        std::io::copy(&mut reader, &mut file).map_err(|e| {
            fs::remove_dir_all(&temp_path).ok();
            CompilerError::ToolchainError(format!("Failed to download Zig: {e}"))
        })?;
        drop(file);
        extract_zip(&zip_path, &temp_path).inspect_err(|_| {
            fs::remove_dir_all(&temp_path).ok();
        })?;
    } else {
        let decompressor = XzDecoder::new(response.into_body().into_reader());
        let mut archive = tar::Archive::new(decompressor);
        archive.unpack(&temp_path).map_err(|e| {
            fs::remove_dir_all(&temp_path).ok();
            CompilerError::ToolchainError(format!("Failed to extract Zig archive: {e}"))
        })?;
    }

    // The archive extracts to zig-{target}-{version}/ inside temp dir
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

/// Extract a zip archive into `dest`, skipping entries whose names
/// would escape it (zip slip). The download is staged on disk because
/// `ZipArchive` needs `Seek`, unlike the streaming tar.xz path.
fn extract_zip(zip_path: &Path, dest: &Path) -> Result<(), CompilerError> {
    let file = fs::File::open(zip_path).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to open downloaded zip: {e}"))
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to read Zig zip archive: {e}"))
    })?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| {
            CompilerError::ToolchainError(format!("Failed to read zip entry {i}: {e}"))
        })?;
        let Some(enclosed) = entry.enclosed_name() else {
            // Surface dropped entries: a trusted upstream archive should
            // never trip the zip-slip guard, so a skip means an unexpected
            // layout that would otherwise fail later with a confusing
            // "Zig binary not found after download".
            eprintln!("warning: skipping non-enclosed zip entry '{}'", entry.name());
            continue;
        };
        let outpath = dest.join(enclosed);
        if entry.is_dir() {
            fs::create_dir_all(&outpath).map_err(|e| {
                CompilerError::ToolchainError(format!("Failed to create directory: {e}"))
            })?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    CompilerError::ToolchainError(format!("Failed to create directory: {e}"))
                })?;
            }
            let mut outfile = fs::File::create(&outpath).map_err(|e| {
                CompilerError::ToolchainError(format!("Failed to create file: {e}"))
            })?;
            std::io::copy(&mut entry, &mut outfile).map_err(|e| {
                CompilerError::ToolchainError(format!("Failed to write zip entry: {e}"))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zig_target_valid() {
        let target = zig_target().unwrap();
        assert!(
            [
                "aarch64-macos",
                "x86_64-linux",
                "aarch64-linux",
                "x86_64-windows"
            ]
            .contains(&target)
        );
    }

    #[test]
    fn test_archive_ext_per_target() {
        assert_eq!(archive_ext("x86_64-windows"), "zip");
        assert_eq!(archive_ext("aarch64-macos"), "tar.xz");
        assert_eq!(archive_ext("x86_64-linux"), "tar.xz");
        assert_eq!(archive_ext("aarch64-linux"), "tar.xz");
    }

    #[test]
    fn test_zig_binary_path_contains_version() {
        let path = zig_binary_path().unwrap();
        let path_str = path.to_string_lossy();
        assert!(path_str.contains(&format!("zig-{ZIG_VERSION}")));
        assert!(path_str.ends_with(&format!("zig{}", std::env::consts::EXE_SUFFIX)));
    }

    #[test]
    fn test_extract_zip_roundtrip() {
        use std::io::Write;
        // TempDir cleans up on drop even when an assertion fails mid-test.
        let dir = tempfile::TempDir::new().unwrap();
        let zip_path = dir.path().join("test.zip");
        let dest = dir.path().join("out");
        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            writer
                .start_file("zig-x86_64-windows-0.16.0/zig.exe", options)
                .unwrap();
            writer.write_all(b"fake").unwrap();
            writer.finish().unwrap();
        }
        extract_zip(&zip_path, &dest).unwrap();
        assert_eq!(
            fs::read(dest.join("zig-x86_64-windows-0.16.0/zig.exe")).unwrap(),
            b"fake"
        );
    }

    #[test]
    fn test_toolchain_dir_under_home() {
        let dir = toolchain_dir().unwrap();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains(".ryo"));
        assert!(dir_str.ends_with("toolchain"));
    }
}
