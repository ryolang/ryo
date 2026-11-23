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

use crate::output::write_stdout;
use ryo_core::ast;
use ryo_core::compilation::CompilationContext;
use ryo_core::diag::{Diag, DiagSink};
use ryo_core::errors::CompilerError;
use ryo_core::source::SourceId;
use ryo_core::types::InternPool;
use ryo_core::uir::Uir;
use ryo_frontend::{astgen, sema};
use std::fs;
use std::path::Path;

use diagnostics::{fail_with_diags, finalize_diags, ownership_error_diag};

#[cfg(test)]
mod source_identity_tests;
#[cfg(test)]
mod tests;

pub(super) fn read_source_file(file: &Path) -> Result<String, CompilerError> {
    fs::read_to_string(file).map_err(CompilerError::from)
}

fn compilation_from_source(
    file: &Path,
    input: String,
) -> Result<(CompilationContext, SourceId), CompilerError> {
    let mut context = CompilationContext::new();
    let id = context.register_source(file.to_path_buf(), input);
    context.sources().require_current(id)?;
    Ok((context, id))
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
    context: &mut CompilationContext,
    source_id: SourceId,
    file_path: &Path,
    parse_diags: Vec<Diag>,
    show_inferred_errors: bool,
) -> Result<(Uir, ryo_frontend::pipeline::CleanupProgram), CompilerError> {
    let source = context.sources().require_current(source_id)?;
    let source_text = source.text().to_owned();
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
    let report =
        sema::analyze_buffered_report(&uir, pool, &mut sink, &source_text, file_path, context);
    let analyzed = match ryo_frontend::pipeline::BuiltProgram::build(report.tirs, pool)
        .analyze(pool, &mut sink)
    {
        Ok(analyzed) => analyzed,
        Err(error) => {
            sink.emit(ownership_error_diag(error));
            return Err(fail_with_diags(sink.into_diags(), context.sources()));
        }
    };
    if show_inferred_errors && !sink.has_errors() {
        for signature in &report.inferred {
            let error_set = pool.error_union_view(signature.return_type).error_set;
            let suffix = if pool.error_set_view(error_set).members.is_empty() {
                " (no errors)"
            } else {
                ""
            };
            write_stdout(format_args!(
                "{}() -> {}{}\n",
                pool.str(signature.name),
                pool.display(signature.return_type),
                suffix
            ))?;
        }
    }
    if !sink.has_errors() {
        for d in report.warnings {
            sink.emit(d);
        }
    }
    // Single tail block: render-if-non-empty, Err iff any errors.
    // Same shape as `ir_command` so warnings (`W0001` DeadStore,
    // `W0002` RedundantMove, …) surface on the success path
    // without a separate render block that could drift from the
    // error path.
    finalize_diags(sink.into_diags(), context.sources())?;
    Ok((uir, analyzed))
}
