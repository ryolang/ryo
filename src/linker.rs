use crate::errors::CompilerError;
use std::process::Command;

pub(crate) fn link_executable(obj_file: &str, exe_file: &str) -> Result<(), CompilerError> {
    let linkers = vec!["zig cc", "clang", "cc"];
    let mut last_error = String::new();

    for linker in linkers {
        let parts: Vec<&str> = linker.split_whitespace().collect();
        let output = if parts.len() > 1 {
            Command::new(parts[0])
                .arg(parts[1])
                .arg("-o")
                .arg(exe_file)
                .arg(obj_file)
                .output()
        } else {
            Command::new(linker)
                .arg("-o")
                .arg(exe_file)
                .arg(obj_file)
                .output()
        };

        match output {
            Ok(output) if output.status.success() => {
                println!("Linked with {}: {}", linker, exe_file);
                return Ok(());
            }
            Ok(output) => {
                last_error = String::from_utf8_lossy(&output.stderr).to_string();
            }
            Err(e) => {
                last_error = e.to_string();
            }
        }
    }

    Err(CompilerError::LinkError(format!(
        "Failed to link with any available linker. Last error: {}",
        last_error
    )))
}
