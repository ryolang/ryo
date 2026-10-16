use fs2::FileExt;
use ryo_core::errors::CompilerError;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use xz2::read::XzDecoder;

const ZIG_VERSION: &str = "0.16.0";

pub fn ensure_zig() -> Result<PathBuf, CompilerError> {
    let dir = toolchain_dir()?;
    fs::create_dir_all(&dir).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to create toolchain directory: {e}"))
    })?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(install_lock_path(&dir))
        .map_err(|e| {
            CompilerError::ToolchainError(format!("Failed to open Zig install lock: {e}"))
        })?;
    lock.lock_exclusive().map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to lock Zig installation: {e}"))
    })?;

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

fn install_lock_path(toolchain_dir: &Path) -> PathBuf {
    toolchain_dir.join(format!(".zig-{ZIG_VERSION}.install.lock"))
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

fn archive_sha256(target: &str) -> Option<&'static str> {
    // Pinned from Zig's official release index:
    // https://ziglang.org/download/index.json
    match target {
        "aarch64-macos" => Some("b23d70deaa879b5c2d486ed3316f7eaa53e84acf6fc9cc747de152450d401489"),
        "x86_64-linux" => Some("70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00"),
        "aarch64-linux" => Some("ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17"),
        "x86_64-windows" => {
            Some("68659eb5f1e4eb1437a722f1dd889c5a322c9954607f5edcf337bc3684a75a7e")
        }
        _ => None,
    }
}

fn sha256_file(path: &Path) -> Result<String, CompilerError> {
    let mut file = fs::File::open(path).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to open downloaded Zig archive: {e}"))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| {
            CompilerError::ToolchainError(format!("Failed to read downloaded Zig archive: {e}"))
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        use std::fmt::Write as _;
        write!(&mut digest, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(digest)
}

fn verify_archive(path: &Path, target: &str) -> Result<(), CompilerError> {
    let expected = archive_sha256(target).ok_or_else(|| {
        CompilerError::ToolchainError(format!("No trusted Zig checksum for target {target}"))
    })?;
    let actual = sha256_file(path)?;
    verify_digest(&actual, expected).map_err(|_| {
        CompilerError::ToolchainError(format!(
            "Zig archive checksum mismatch for {target}: expected {expected}, got {actual}"
        ))
    })
}

fn verify_digest(actual: &str, expected: &str) -> Result<(), CompilerError> {
    if actual != expected {
        return Err(CompilerError::ToolchainError(format!(
            "SHA-256 mismatch: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

fn install_staged_toolchain(source: &Path, desired_path: &Path) -> Result<(), CompilerError> {
    if desired_path.exists() {
        return Err(CompilerError::ToolchainError(format!(
            "Zig installation directory already exists: {}",
            desired_path.display()
        )));
    }
    fs::rename(source, desired_path)
        .map_err(|e| CompilerError::ToolchainError(format!("Failed to install Zig: {e}")))
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

    fs::create_dir_all(&dest).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to create toolchain directory: {e}"))
    })?;

    if desired_path.exists() {
        return Err(CompilerError::ToolchainError(format!(
            "Zig installation directory exists without a usable binary: {}",
            desired_path.display()
        )));
    }

    // A leftover staging directory can only come from an interrupted install;
    // the process-wide lock ensures no other installer is using it now.
    fs::remove_dir_all(&temp_path).ok();
    fs::create_dir_all(&temp_path).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to create temp directory: {e}"))
    })?;

    eprintln!("Zig toolchain not found. Downloading zig {ZIG_VERSION} for {target}...");

    let response = ureq::get(&url)
        .call()
        .map_err(|e| CompilerError::ToolchainError(format!("Failed to download Zig: {e}")))?;

    let archive_path = temp_path.join(format!("zig-download.{ext}"));
    let mut reader = response.into_body().into_reader();
    let mut archive_file = fs::File::create(&archive_path)
        .map_err(|e| CompilerError::ToolchainError(format!("Failed to stage Zig archive: {e}")))?;
    std::io::copy(&mut reader, &mut archive_file)
        .map_err(|e| CompilerError::ToolchainError(format!("Failed to download Zig: {e}")))?;
    archive_file.flush().map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to flush downloaded Zig archive: {e}"))
    })?;
    drop(archive_file);
    verify_archive(&archive_path, target)?;

    eprintln!("Extracting...");
    if ext == "zip" {
        extract_zip(&archive_path, &temp_path)?;
    } else {
        let file = fs::File::open(&archive_path).map_err(|e| {
            CompilerError::ToolchainError(format!("Failed to open Zig archive: {e}"))
        })?;
        let decompressor = XzDecoder::new(file);
        let mut archive = tar::Archive::new(decompressor);
        archive.unpack(&temp_path).map_err(|e| {
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

    fs::remove_file(&archive_path).map_err(|e| {
        CompilerError::ToolchainError(format!("Failed to remove staged Zig archive: {e}"))
    })?;

    install_staged_toolchain(&source, &desired_path)?;

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
            eprintln!(
                "warning: skipping non-enclosed zip entry '{}'",
                entry.name()
            );
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
    fn test_official_archive_checksums_cover_supported_targets() {
        assert_eq!(
            archive_sha256("aarch64-macos"),
            Some("b23d70deaa879b5c2d486ed3316f7eaa53e84acf6fc9cc747de152450d401489")
        );
        assert_eq!(
            archive_sha256("x86_64-linux"),
            Some("70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00")
        );
        assert_eq!(
            archive_sha256("aarch64-linux"),
            Some("ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17")
        );
        assert_eq!(
            archive_sha256("x86_64-windows"),
            Some("68659eb5f1e4eb1437a722f1dd889c5a322c9954607f5edcf337bc3684a75a7e")
        );
        assert_eq!(archive_sha256("unsupported-target"), None);
    }

    #[test]
    fn test_verify_sha256_accepts_matching_archive_and_rejects_mismatch() {
        let dir = tempfile::TempDir::new().unwrap();
        let archive = dir.path().join("archive");
        fs::write(&archive, b"abc").unwrap();

        assert!(
            verify_digest(
                &sha256_file(&archive).unwrap(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            )
            .is_ok()
        );
        assert!(verify_digest(&sha256_file(&archive).unwrap(), "wrong checksum").is_err());
    }

    #[test]
    fn test_install_staged_toolchain_preserves_existing_install() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("staged");
        let desired = dir.path().join("zig-0.16.0");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("zig"), b"new binary").unwrap();
        fs::create_dir_all(&desired).unwrap();
        fs::write(desired.join("zig"), b"active binary").unwrap();

        assert!(install_staged_toolchain(&source, &desired).is_err());
        assert_eq!(fs::read(desired.join("zig")).unwrap(), b"active binary");
        assert!(source.exists());
    }

    #[test]
    fn test_install_staged_toolchain_publishes_new_install() {
        let dir = tempfile::TempDir::new().unwrap();
        let source = dir.path().join("staged");
        let desired = dir.path().join("zig-0.16.0");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("zig"), b"new binary").unwrap();

        install_staged_toolchain(&source, &desired).unwrap();
        assert_eq!(fs::read(desired.join("zig")).unwrap(), b"new binary");
        assert!(!source.exists());
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
