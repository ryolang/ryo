use ryo_backend::{codegen, linker, runtime_lib};
use ryo_core::errors::CompilerError;
use ryo_core::types::InternPool;
use std::fs;
use std::path::{Path, PathBuf};
use target_lexicon::Triple;

use super::diagnostics::fail_with_diags;
use super::emit::{display_ast, display_flow, display_tir, display_uir};
use super::lower_and_analyze;
use super::options::{EmitKind, EmitSet, LinkMode};
use super::{read_source_file, source_name};

// Helper function to generate output filenames. Artifacts land next
// to the source file, not in the CWD, so two same-stem sources in
// different directories built from one CWD don't clobber each other.
// Paths are built by extension replacement (never through `str`), so
// non-UTF-8 source paths survive through object writing and linking.
fn get_output_filenames(input_file: &Path) -> (PathBuf, PathBuf) {
    let obj_filename = input_file.with_extension(if cfg!(windows) { "obj" } else { "o" });
    // EXE_SUFFIX is ".exe" on Windows and "" elsewhere; with_extension
    // takes the bare extension ("" clears it, leaving the stem).
    let exe_filename =
        input_file.with_extension(std::env::consts::EXE_SUFFIX.trim_start_matches('.'));

    (obj_filename, exe_filename)
}

/// AOT-compile `file` to a standalone binary next to the source.
/// Silent on success unless `--emit` requests IR sections (same
/// rendering and pipeline order as `ryo ir`; CLIF prints after
/// codegen, before linking). `link` selects the libc zig cc links
/// against on Linux (musl default); it is a no-op on other hosts.
pub fn build_file(
    file: &Path,
    emit: &[EmitKind],
    link: LinkMode,
    show_inferred_errors: bool,
) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let name = source_name(file);
    let want = EmitSet::from_args(emit);
    let (program, parse_diags) = match ryo_frontend::parse::parse_source(&input, &mut pool) {
        Ok(v) => v,
        Err(diags) => return Err(fail_with_diags(diags, &input, &name)),
    };

    if want.ast {
        display_ast(&program, &pool);
        println!();
    }

    let (uir, analyzed) = lower_and_analyze(
        &program,
        &mut pool,
        &input,
        &name,
        file,
        parse_diags,
        show_inferred_errors,
    )?;
    analyzed
        .require_backend_transition()
        .map_err(CompilerError::CodegenError)?;
    let tirs = analyzed.tirs();
    let flows = analyzed.flows();
    let sidecar = analyzed.cleanup();

    if want.uir {
        display_uir(&uir, &pool);
        println!();
    }
    if want.tir {
        display_tir(tirs, &pool);
        println!();
    }
    if want.flow {
        display_flow(tirs, flows, sidecar, &pool)?;
    }

    let (obj_filename, exe_filename) = get_output_filenames(file);

    let target = Triple::host();
    let mut codegen = codegen::Codegen::new_aot(target).map_err(CompilerError::CodegenError)?;
    let (_main_id, clif) = codegen
        .compile(tirs, flows, &pool, sidecar, want.clif)
        .map_err(CompilerError::CodegenError)?;
    if want.clif {
        println!("[Cranelift IR]");
        print!("{clif}");
    }
    let obj_bytes = codegen.finish().map_err(CompilerError::CodegenError)?;

    fs::write(&obj_filename, obj_bytes).map_err(CompilerError::from)?;

    // Extract embedded runtime archive and keep its cache lock until zig finishes reading it.
    let (runtime_path, _runtime_lock) = runtime_lib::extract_runtime_to_cache()
        .map_err(|e| CompilerError::LinkError(format!("Failed to extract runtime: {e}")))?;

    let link_result =
        linker::link_executable(&obj_filename, &exe_filename, &runtime_path, link.into());
    drop(_runtime_lock);

    // Default: clean up the intermediate object file. Set
    // `RYO_KEEP_OBJ=1` to retain it — used by tooling that needs to
    // relink the same object with extra flags (e.g. the ASan smoke
    // tests in `tests/asan_smoke.rs` re-link with `-fsanitize=address`).
    // Runs on the link-failure path too, which previously leaked the
    // `.o` via the early `?`.
    if std::env::var_os("RYO_KEEP_OBJ").is_none() {
        let _ = fs::remove_file(&obj_filename);
    }
    link_result?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_filenames_land_next_to_source() {
        let obj_ext = if cfg!(windows) { "obj" } else { "o" };
        let exe_suffix = std::env::consts::EXE_SUFFIX;

        let (obj, exe) = get_output_filenames(Path::new("some/dir/hello.ryo"));
        assert_eq!(obj, PathBuf::from(format!("some/dir/hello.{obj_ext}")));
        assert_eq!(exe, PathBuf::from(format!("some/dir/hello{exe_suffix}")));

        // No directory component: output stays relative to the CWD,
        // exactly as before.
        let (obj, exe) = get_output_filenames(Path::new("hello.ryo"));
        assert_eq!(obj, PathBuf::from(format!("hello.{obj_ext}")));
        assert_eq!(exe, PathBuf::from(format!("hello{exe_suffix}")));
    }
}
