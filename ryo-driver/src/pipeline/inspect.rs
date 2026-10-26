use ryo_core::diag::DiagSink;
use ryo_core::errors::CompilerError;
use ryo_core::types::InternPool;
use ryo_frontend::{astgen, sema};
use std::path::Path;

use super::diagnostics::{fail_with_diags, finalize_diags};
use super::emit::{
    display_ast, display_flow, display_tir, display_tokens, display_uir, generate_and_display_ir,
};
use super::options::{EmitKind, EmitSet};
use super::{read_source_file, source_name};

pub fn lex_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    display_tokens(&input, file)
}

pub fn parse_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let name = source_name(file);
    let (program, diags) = match ryo_frontend::parse::parse_source(&input, &mut pool) {
        Ok(v) => v,
        Err(diags) => return Err(fail_with_diags(diags, &input, &name)),
    };
    display_ast(&program, &pool);
    finalize_diags(diags, &input, &name)?;
    Ok(())
}

/// Drive `ryo ir` with the requested set of IR sections.
///
/// `emit` is the user-supplied `--emit=<kind>[,<kind>...]` list.
/// Empty prints all five sections: AST, UIR, TIR, Flow, and CLIF.
///
/// Sections are normalized into pipeline order before printing
/// (AST → UIR → TIR → Flow → CLIF) so flag order is irrelevant. Stages
/// run only as far as the deepest requested section requires; an
/// `--emit=uir` invocation never reaches sema.
pub fn ir_command(file: &Path, emit: &[EmitKind]) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let name = source_name(file);
    let mut pool = InternPool::new();

    let want = if emit.is_empty() {
        // Default: all five sections, in pipeline order. (Until
        // 2026-09 this was AST + CLIF only, a leftover from when
        // those were the only two dumps.)
        EmitSet {
            ast: true,
            uir: true,
            tir: true,
            flow: true,
            clif: true,
        }
    } else {
        EmitSet::from_args(emit)
    };
    let (program, parse_diags) = match ryo_frontend::parse::parse_source(&input, &mut pool) {
        Ok(v) => v,
        Err(diags) => return Err(fail_with_diags(diags, &input, &name)),
    };

    if want.ast {
        display_ast(&program, &pool);
        println!();
    }

    // Lex/parse diagnostics accumulate with the middle-end's so a
    // recovered syntax error never hides UIR/TIR-stage problems.
    let mut sink = DiagSink::new();
    for d in parse_diags {
        sink.emit(d);
    }

    // UIR / TIR / CLIF gating. We always *run* astgen if any of
    // those is asked for; sema only if TIR or CLIF; codegen only
    // if CLIF. Each stage's print is independent.
    let need_uir = want.uir || want.tir || want.flow || want.clif;
    if !need_uir {
        return finalize_diags(sink.into_diags(), &input, &name);
    }

    let uir = astgen::generate(&program, &mut pool, &mut sink);

    if want.uir {
        display_uir(&uir, &pool);
        println!();
    }

    if !(want.tir || want.flow || want.clif) {
        // UIR-only run. Surface astgen diagnostics now, with a
        // non-zero exit if anything fired.
        return finalize_diags(sink.into_diags(), &input, &name);
    }

    // For TIR / CLIF we also run sema. Per the §4.5 design, sema
    // returns a well-formed TIR even with errors (Unreachable
    // slots), and `--emit=tir` deliberately prints that partial
    // TIR — the whole point of the flag is debugging sema.
    let (tirs, sema_warnings) = sema::analyze_buffered(&uir, &mut pool, &mut sink, &input, file);
    let analyzed = ryo_frontend::pipeline::BuiltProgram::build(tirs, &pool)
        .analyze(&pool, &mut sink)
        .map_err(CompilerError::CodegenError)?;
    let tirs = analyzed.tirs();
    let flows = analyzed.flows();
    let sidecar = analyzed.cleanup();
    // Sema warnings flush only after ownership has run: an ownership
    // error must suppress them exactly like a sema error would.
    if !sink.has_errors() {
        for d in sema_warnings {
            sink.emit(d);
        }
    }

    if want.tir {
        display_tir(tirs, &pool);
        println!();
    }
    if want.flow {
        display_flow(tirs, flows, sidecar, &pool)?;
    }

    if want.clif {
        // Codegen asserts no Unreachable instructions. If sema
        // failed, surface the diagnostics and abort — we cannot
        // produce a meaningful CLIF dump from a broken TIR.
        if sink.has_errors() {
            return finalize_diags(sink.into_diags(), &input, &name);
        }
        generate_and_display_ir(tirs, flows, &pool, sidecar)?;
    }

    // Tail block: drains the sink whether sema/ownership were
    // clean or only emitted warnings. Without this `ryo ir` would
    // silently swallow W0001/W0002 on success.
    finalize_diags(sink.into_diags(), &input, &name)
}
