use ryo_backend::codegen;
use ryo_core::errors::CompilerError;
use ryo_core::types::InternPool;
use std::path::Path;

use super::diagnostics::fail_with_diags;
use super::emit::{display_ast, display_flow, display_tir, display_uir};
use super::lower_and_analyze;
use super::options::{EmitKind, EmitSet};
use super::{read_source_file, source_name};

/// JIT-compile and run `file`. The default output is exactly what
/// the compiled program writes to stdout/stderr — no compiler
/// banners — and the return value is the program's own exit code.
/// `--emit` additionally prints IR sections in pipeline order (AST
/// after parse; UIR/TIR after lowering; CLIF after codegen, before
/// execution), rendered identically to `ryo ir`. `program_args` are
/// forwarded to the program and published to the runtime's argv storage
/// by the entry shim before `main`'s first instruction; a synthetic
/// argv[0] — the source file path — is prepended so argc/argv match the
/// AOT binary's C-runtime table (argv[0] = invocation path, included in
/// the count). Callers with no program arguments pass `&[]`.
pub fn run_file(
    file: &Path,
    emit: &[EmitKind],
    program_args: &[String],
) -> Result<i32, CompilerError> {
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

    let (uir, analyzed) = lower_and_analyze(&program, &mut pool, &input, &name, file, parse_diags)?;
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

    let mut codegen = codegen::Codegen::new_jit().map_err(CompilerError::CodegenError)?;
    let (main_id, clif) = codegen
        .compile(tirs, flows, &pool, sidecar, want.clif)
        .map_err(CompilerError::CodegenError)?;
    if want.clif {
        println!("[Cranelift IR]");
        print!("{clif}");
    }

    let mut argv = Vec::with_capacity(program_args.len() + 1);
    argv.push(file.to_string_lossy().into_owned());
    argv.extend_from_slice(program_args);
    codegen
        .execute(main_id, &argv)
        .map_err(CompilerError::ExecutionError)
}
