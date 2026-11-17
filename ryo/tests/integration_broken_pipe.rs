mod common;
use common::*;

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use tempfile::TempDir;

#[derive(Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

fn generated_command(program: &Path, args: &[&OsStr]) -> Command {
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("trap '' PIPE; exec \"$@\"")
            .arg("sh")
            .arg(program)
            .args(args);
        command
    }
    #[cfg(windows)]
    {
        let mut command = Command::new(program);
        command.args(args);
        command
    }
}

fn run_with_closed_pipe(program: &Path, args: &[&OsStr], stream: OutputStream) -> ExitStatus {
    let mut command = generated_command(program, args);
    match stream {
        OutputStream::Stdout => {
            command.stdout(closed_pipe()).stderr(Stdio::null());
        }
        OutputStream::Stderr => {
            command.stdout(Stdio::null()).stderr(closed_pipe());
        }
    }
    command.status().expect("run with closed output pipe")
}

fn assert_closed_pipe_termination(status: ExitStatus) {
    #[cfg(not(windows))]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(13), "status: {status:?}");
    }
    #[cfg(windows)]
    assert_eq!(status.code(), Some(141), "status: {status:?}");
}

fn jit_status(source: &str, stream: OutputStream) -> ExitStatus {
    let temp = TempDir::new().expect("tempdir");
    let file = create_test_file(temp.path(), "jit_closed_pipe.ryo", source);
    let args = [OsStr::new("run"), file.as_os_str()];
    run_with_closed_pipe(Path::new(env!("CARGO_BIN_EXE_ryo")), &args, stream)
}

fn aot_status(source: &str, stream: OutputStream) -> ExitStatus {
    let temp = TempDir::new().expect("tempdir");
    let file = create_test_file(temp.path(), "aot_closed_pipe.ryo", source);
    let build = run_ryo_build(&file, temp.path());
    assert!(
        build.status.success(),
        "AOT build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    run_with_closed_pipe(&exe_path(temp.path(), "aot_closed_pipe"), &[], stream)
}

#[test]
fn jit_stdout_closed_terminates() {
    assert_closed_pipe_termination(jit_status(
        "fn main():\n\tprint(\"out\")\n",
        OutputStream::Stdout,
    ));
}

#[test]
fn jit_stderr_closed_terminates() {
    assert_closed_pipe_termination(jit_status(
        "fn main():\n\tio_eprint(\"err\")\n",
        OutputStream::Stderr,
    ));
}

#[test]
fn aot_stdout_closed_terminates() {
    assert_closed_pipe_termination(aot_status(
        "fn main():\n\tprint(\"out\")\n",
        OutputStream::Stdout,
    ));
}

#[test]
fn aot_stderr_closed_terminates() {
    assert_closed_pipe_termination(aot_status(
        "fn main():\n\tio_eprint(\"err\")\n",
        OutputStream::Stderr,
    ));
}
