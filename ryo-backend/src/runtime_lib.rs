use fs2::FileExt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const RYO_RUNTIME_LIB: &[u8] = include_bytes!(env!("RYO_RUNTIME_LIB"));
// Four versions preserve recent reuse while bounding the cache to a few archives.
const MAX_CACHED_ARCHIVES: usize = 4;

fn cache_dir() -> Result<PathBuf, io::Error> {
    dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot determine home directory"))
        .map(|h| h.join(".ryo").join("cache"))
}

fn content_hash() -> String {
    env!("RYO_RUNTIME_HASH")[..16].to_string()
}

fn archive_name_parts() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("ryo_runtime-", ".lib")
    } else {
        ("libryo_runtime-", ".a")
    }
}

/// Cache filename for the extracted archive: the embedded bytes are a
/// COFF archive on Windows (`.lib`) and an ELF/Mach-O archive
/// elsewhere (`.a`). Host-native only — no cross-compile naming yet.
fn extracted_name(hash: &str) -> String {
    let (prefix, suffix) = archive_name_parts();
    format!("{prefix}{hash}{suffix}")
}

fn is_cached_archive(name: &str) -> bool {
    let (prefix, suffix) = archive_name_parts();
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .is_some_and(|hash| !hash.is_empty())
}

fn is_runtime_temp_file(name: &str) -> bool {
    let (prefix, _) = archive_name_parts();
    name.starts_with(prefix) && name.contains(".tmp.")
}

fn cache_lock(dir: &Path) -> Result<fs::File, io::Error> {
    fs::create_dir_all(dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(dir.join(".runtime-cache.lock"))?;
    lock.lock_exclusive()?;
    Ok(lock)
}

fn sweep_stale_temp_files(dir: &Path) -> Result<(), io::Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_file() && name.to_str().is_some_and(is_runtime_temp_file) {
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

fn evict_old_archives(dir: &Path, current: &Path) -> Result<(), io::Error> {
    let mut archives = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_file() && name.to_str().is_some_and(is_cached_archive) {
            let modified = entry.metadata()?.modified()?;
            archives.push((modified, entry.path()));
        }
    }
    archives.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    let mut remaining = archives.len();
    for (_, path) in archives {
        if remaining <= MAX_CACHED_ARCHIVES {
            break;
        }
        if path == current {
            continue;
        }
        match fs::remove_file(path) {
            Ok(()) => remaining -= 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => remaining -= 1,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub fn extract_runtime_to_cache() -> Result<(PathBuf, fs::File), io::Error> {
    extract_runtime_to_cache_at(&cache_dir()?, &content_hash(), RYO_RUNTIME_LIB)
}

fn extract_runtime_to_cache_at(
    dir: &Path,
    hash: &str,
    runtime_lib: &[u8],
) -> Result<(PathBuf, fs::File), io::Error> {
    // The returned lock keeps cleanup and eviction excluded until the caller finishes using the archive.
    let lock = cache_lock(dir)?;
    sweep_stale_temp_files(dir)?;
    let path = dir.join(extracted_name(hash));

    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "cached runtime archive is not a regular file: {}",
                    path.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Write to a temp name and rename for atomicity.
            let tmp_path = dir.join(format!(
                "{}.tmp.{}",
                extracted_name(hash),
                std::process::id()
            ));
            fs::write(&tmp_path, runtime_lib)?;
            fs::rename(&tmp_path, &path)?;
        }
        Err(error) => return Err(error),
    }
    evict_old_archives(dir, &path)?;
    Ok((path, lock))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, SystemTime};

    fn write_archive(dir: &Path, hash: &str, modified_secs: u64) -> PathBuf {
        let path = dir.join(extracted_name(hash));
        fs::write(&path, hash).unwrap();
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(modified_secs)),
        )
        .unwrap();
        path
    }

    #[test]
    fn extracted_name_matches_host() {
        let name = extracted_name("0123456789abcdef");
        if cfg!(windows) {
            assert_eq!(name, "ryo_runtime-0123456789abcdef.lib");
        } else {
            assert_eq!(name, "libryo_runtime-0123456789abcdef.a");
        }
    }

    #[test]
    fn eviction_keeps_newest_archives_and_current_archive() {
        let dir = tempfile::TempDir::new().unwrap();
        let current = write_archive(dir.path(), "current", 1);
        let oldest = write_archive(dir.path(), "oldest", 2);
        let second_oldest = write_archive(dir.path(), "second-oldest", 3);
        let newest = [
            write_archive(dir.path(), "newest-1", 4),
            write_archive(dir.path(), "newest-2", 5),
            write_archive(dir.path(), "newest-3", 6),
        ];

        evict_old_archives(dir.path(), &current).unwrap();

        assert!(!oldest.exists());
        assert!(!second_oldest.exists());
        assert!(current.exists());
        assert!(newest.iter().all(|path| path.exists()));
    }

    #[test]
    fn extraction_reuses_cached_archive() {
        let dir = tempfile::TempDir::new().unwrap();
        let cached = dir.path().join(extracted_name("current"));
        fs::write(&cached, b"cached").unwrap();

        let (extracted, _lock) =
            extract_runtime_to_cache_at(dir.path(), "current", b"replacement").unwrap();

        assert_eq!(extracted, cached);
        assert_eq!(fs::read(extracted).unwrap(), b"cached");
    }

    #[test]
    fn extraction_rejects_cached_archive_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let cached = dir.path().join(extracted_name("current"));
        fs::create_dir(&cached).unwrap();

        let error = extract_runtime_to_cache_at(dir.path(), "current", b"replacement").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn extraction_rejects_cached_archive_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("target");
        let cached = dir.path().join(extracted_name("current"));
        fs::write(&target, b"cached").unwrap();
        std::os::unix::fs::symlink(&target, &cached).unwrap();

        let error = extract_runtime_to_cache_at(dir.path(), "current", b"replacement").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn extraction_sweeps_only_runtime_temp_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let stale = dir
            .path()
            .join(format!("{}.tmp.12345", extracted_name("stale")));
        let unrelated = dir.path().join("notes.tmp.12345");
        let directory = dir
            .path()
            .join(format!("{}.tmp.12345", extracted_name("directory")));
        fs::write(&stale, b"partial").unwrap();
        fs::write(&unrelated, b"keep").unwrap();
        fs::create_dir(&directory).unwrap();

        extract_runtime_to_cache_at(dir.path(), "current", b"runtime").unwrap();

        assert!(!stale.exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"keep");
        assert!(directory.exists());
    }

    #[test]
    fn concurrent_extraction_cannot_sweep_active_temp_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = cache_lock(dir.path()).unwrap();
        let active = dir
            .path()
            .join(format!("{}.tmp.12345", extracted_name("active")));
        fs::write(&active, b"partial").unwrap();
        let thread_dir = dir.path().to_path_buf();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        let extraction = thread::spawn(move || {
            started_tx.send(()).unwrap();
            extract_runtime_to_cache_at(&thread_dir, "current", b"runtime").unwrap();
            done_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();

        assert_eq!(
            done_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        assert!(active.exists());

        drop(lock);
        extraction.join().unwrap();
        assert!(!active.exists());
    }

    #[test]
    fn returned_lock_excludes_concurrent_maintenance() {
        let dir = tempfile::TempDir::new().unwrap();
        let (_path, lock) = extract_runtime_to_cache_at(dir.path(), "current", b"runtime").unwrap();
        let thread_dir = dir.path().to_path_buf();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        let extraction = thread::spawn(move || {
            started_tx.send(()).unwrap();
            extract_runtime_to_cache_at(&thread_dir, "other", b"other").unwrap();
            done_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();

        assert_eq!(
            done_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );

        drop(lock);
        extraction.join().unwrap();
    }
}
