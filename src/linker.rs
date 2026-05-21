use crate::errors::CompilerError;
use crate::toolchain;
use std::path::Path;
use std::process::Command;

pub(crate) fn link_executable(
    obj_file: &str,
    exe_file: &str,
    runtime_lib: &Path,
) -> Result<(), CompilerError> {
    let zig_path = toolchain::ensure_zig()?;

    let mut args = vec![
        "cc",
        "-o",
        exe_file,
        obj_file,
        runtime_lib.to_str().unwrap_or("libryo_runtime.a"),
    ];

    // Rust's staticlib bundles precompiled std objects that reference
    // _Unwind_* symbols even with panic=abort (from backtrace support).
    // On macOS the system libunwind satisfies them; on Linux we must
    // explicitly link zig's bundled libunwind.
    if cfg!(target_os = "linux") {
        args.push("-lunwind");
    }

    let output = Command::new(&zig_path)
        .args(&args)
        .output()
        .map_err(|e| CompilerError::LinkError(format!("Failed to run zig cc: {e}")))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(CompilerError::LinkError(format!("zig cc failed: {stderr}")))
    }
}
