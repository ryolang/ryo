use crate::toolchain;
use ryo_core::errors::CompilerError;
use std::path::Path;
use std::process::Command;

pub fn link_executable(
    obj_file: &Path,
    exe_file: &Path,
    runtime_lib: &Path,
) -> Result<(), CompilerError> {
    let zig_path = toolchain::ensure_zig()?;

    let mut cmd = Command::new(&zig_path);
    cmd.arg("cc").arg("-o").arg(exe_file).arg(obj_file);
    cmd.arg(runtime_lib.as_os_str());

    // Static musl on Linux: produced binaries run on any Linux regardless
    // of host glibc version. zig cc bundles musl for every target, so this
    // needs no host libc development files. See the std.net DNS caveat in
    // implementation_roadmap.md (musl's resolver skips NSS plugins).
    #[cfg(target_os = "linux")]
    cmd.arg("-target")
        .arg(format!("{}-linux-musl", std::env::consts::ARCH));

    let output = cmd
        .output()
        .map_err(|e| CompilerError::LinkError(format!("Failed to run zig cc: {e}")))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(CompilerError::LinkError(format!("zig cc failed: {stderr}")))
    }
}
