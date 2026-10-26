mod build;
mod diagnostics;
mod emit;
mod inspect;
mod options;
mod run;

pub use build::build_file;
pub use inspect::{ir_command, lex_command, parse_command};
pub use options::{EmitKind, LinkMode};
pub use run::run_file;

use ryo_core::ast;
use ryo_core::diag::{Diag, DiagSink};
use ryo_core::errors::CompilerError;
use ryo_core::types::InternPool;
use ryo_core::uir::Uir;
use ryo_frontend::{astgen, sema};
use std::fs;
use std::path::Path;

use diagnostics::finalize_diags;

#[cfg(test)]
mod tests;

pub(super) fn read_source_file(file: &Path) -> Result<String, CompilerError> {
    fs::read_to_string(file).map_err(CompilerError::from)
}

/// Resolve the user-facing source name for diagnostics.
pub(super) fn source_name(file: &Path) -> String {
    file.to_str()
        .map(str::to_string)
        .unwrap_or_else(|| file.display().to_string())
}

/// Run the front-end (astgen + sema), build the canonical Flow IR per body,
/// and run the ownership pass, returning the UIR, the typed TIR and Flow IR
/// per-function, and the ownership sidecar. Used by `run` and `build`
/// (which require a clean front-end before codegen); the UIR comes back
/// so `run --emit=uir` / `build --emit=uir` can dump it. `ryo ir` does
/// its own staging so it can print partial UIR / TIR after a failure.
pub(super) fn lower_and_analyze(
    program: &ast::Ast,
    pool: &mut InternPool,
    input: &str,
    source_name: &str,
    file_path: &Path,
    parse_diags: Vec<Diag>,
) -> Result<(Uir, ryo_frontend::pipeline::CleanupProgram), CompilerError> {
    let mut sink = DiagSink::new();
    // Lex/parse diagnostics come first so the final render preserves
    // pipeline order.
    for d in parse_diags {
        sink.emit(d);
    }
    let uir = astgen::generate(program, pool, &mut sink);
    // Run sema even if astgen emitted errors: the Error sentinel
    // keeps cascades in check, and surfacing every problem in one
    // run is the whole point of the structured-diagnostics phase.
    // Sema warnings are buffered and flushed after ownership so an
    // ownership error suppresses them exactly like a sema error.
    let (tirs, sema_warnings) = sema::analyze_buffered(&uir, pool, &mut sink, input, file_path);
    let analyzed = ryo_frontend::pipeline::BuiltProgram::build(tirs, pool)
        .analyze(pool, &mut sink)
        .map_err(CompilerError::CodegenError)?;
    if !sink.has_errors() {
        for d in sema_warnings {
            sink.emit(d);
        }
    }
    // Single tail block: render-if-non-empty, Err iff any errors.
    // Same shape as `ir_command` so warnings (`W0001` DeadStore,
    // `W0002` RedundantMove, …) surface on the success path
    // without a separate render block that could drift from the
    // error path.
    finalize_diags(sink.into_diags(), input, source_name)?;
    Ok((uir, analyzed))
}
