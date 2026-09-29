use crate::toolchain;
use ryo_core::errors::CompilerError;
use std::path::Path;
use std::process::Command;

/// Which libc `zig cc` links produced Linux binaries against. On
/// non-Linux hosts the choice is accepted but has no effect: zig cc
/// already links natively there, so both modes collapse to the same
/// command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkMode {
    /// Static musl (the default): pass `-target <arch>-linux-musl` so
    /// produced binaries run on any Linux regardless of host glibc
    /// version. zig cc bundles musl for every target, so this needs no
    /// host libc development files. See the std.net DNS caveat in
    /// implementation_roadmap.md (musl's resolver skips NSS plugins).
    #[default]
    Musl,
    /// Omit `-target` and link natively against the host glibc (the
    /// pre-musl behavior). Binaries are only as portable as the build
    /// host's glibc, but they get the host's full NSS/getaddrinfo
    /// stack.
    Glibc,
}

impl LinkMode {
    /// Append this mode's zig cc flags to `cmd`. Factored out of
    /// `link_executable` so the flag-vs-no-flag behavior is unit
    /// testable without running the linker.
    pub fn apply_to(self, cmd: &mut Command) {
        #[cfg(target_os = "linux")]
        {
            if let Self::Musl = self {
                cmd.arg("-target")
                    .arg(format!("{}-linux-musl", std::env::consts::ARCH));
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (self, cmd);
    }
}

pub fn link_executable(
    obj_file: &Path,
    exe_file: &Path,
    runtime_lib: &Path,
    link_mode: LinkMode,
) -> Result<(), CompilerError> {
    let zig_path = toolchain::ensure_zig()?;

    let mut cmd = Command::new(&zig_path);
    cmd.arg("cc").arg("-o").arg(exe_file).arg(obj_file);
    cmd.arg(runtime_lib.as_os_str());
    link_mode.apply_to(&mut cmd);

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

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(mode: LinkMode) -> Vec<String> {
        let mut cmd = Command::new("zig");
        cmd.arg("cc");
        mode.apply_to(&mut cmd);
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn musl_passes_musl_target_glibc_omits_it() {
        let musl = args_of(LinkMode::Musl);
        let target_pos = musl
            .iter()
            .position(|a| a == "-target")
            .expect("musl mode must pass -target");
        assert_eq!(
            musl[target_pos + 1],
            format!("{}-linux-musl", std::env::consts::ARCH)
        );

        let glibc = args_of(LinkMode::Glibc);
        assert!(
            !glibc.iter().any(|a| a == "-target"),
            "glibc mode must omit -target so zig cc links host-native: {glibc:?}"
        );
    }

    // Off Linux, `-target <arch>-linux-musl` would make zig cc emit a
    // Linux binary from a macOS/Windows host, so both modes must be a
    // no-op there — the flag is accepted but ignored.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn link_mode_is_a_noop_off_linux() {
        assert_eq!(args_of(LinkMode::Musl), args_of(LinkMode::Glibc));
        assert!(!args_of(LinkMode::Musl).iter().any(|a| a == "-target"));
    }
}
