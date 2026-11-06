use ryo_backend::codegen;
use ryo_core::ast;
use ryo_core::diag::DiagSink;
use ryo_core::errors::CompilerError;
use ryo_core::tir::{self, Tir};
use ryo_core::types::InternPool;
use ryo_core::uir::Uir;
use ryo_frontend::lexer::{self, Token};
use std::io::{self, Write as _};
use std::path::Path;
use target_lexicon::Triple;

use super::diagnostics::finalize_diags;
use super::source_name;
use crate::output::{finish_output, write_stdout};

pub(super) fn display_tokens(input: &str, file: &Path) -> Result<(), CompilerError> {
    let mut pool = InternPool::new();
    let name = source_name(file);
    let mut sink = DiagSink::new();
    let tokens = lexer::lex(input, &mut pool, &mut sink);
    if !sink.is_empty() {
        // Route lex diagnostics through the same Diag pipeline as
        // parse / sema errors so `ryo lex` matches the rest of the
        // CLI's exit-code and rendering behaviour. Previously this
        // path silently `eprintln!`d and returned `Ok(())`, which
        // hid lex errors from CI.
        return finalize_diags(sink.into_diags(), input, &name);
    }

    write_stdout(format_args!("Token stream for '{}':\n", file.display()))?;
    write_stdout(format_args!("\n"))?;

    // Render identifier and string-literal payloads through the
    // pool so the user sees the actual text rather than an opaque
    // handle id. Other variants format normally via Debug.
    for (tok, span) in &tokens {
        match tok {
            Token::Ident(id) => write_stdout(format_args!(
                "Ident({:?}) @ {}..{}\n",
                pool.str(*id),
                span.start,
                span.end
            ))?,
            Token::StrLit(id) => write_stdout(format_args!(
                "StrLit({:?}) @ {}..{}\n",
                pool.str(*id),
                span.start,
                span.end
            ))?,
            other => write_stdout(format_args!("{:?} @ {}..{}\n", other, span.start, span.end))?,
        }
    }
    Ok(())
}

pub(super) fn display_ast(program: &ast::Ast, pool: &InternPool) -> Result<(), CompilerError> {
    write_stdout(format_args!("[AST]\n"))?;
    // Stream instead of buffering: a deeply nested program's tree dump
    // is quadratic in depth, far larger than any reasonable buffer.
    struct IoWriter<'a, W: std::io::Write> {
        out: &'a mut W,
        error: Option<io::Error>,
    }
    impl<W: std::io::Write> std::fmt::Write for IoWriter<'_, W> {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            match self.out.write_all(s.as_bytes()) {
                Ok(()) => Ok(()),
                Err(error) => {
                    self.error = Some(error);
                    Err(std::fmt::Error)
                }
            }
        }
    }
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut writer = IoWriter {
        out: &mut out,
        error: None,
    };
    // Writer I/O failures are captured in `writer.error` and reported below.
    ryo_core::ast_pretty::write_program(&mut writer, program, pool).ok();
    let error = writer.error.take();
    drop(writer);
    finish_output(match error {
        Some(error) => Err(error),
        None => out.flush(),
    })
}

pub(super) fn display_uir(uir: &Uir, pool: &InternPool) -> Result<(), CompilerError> {
    write_stdout(format_args!("[UIR]\n"))?;
    write_stdout(format_args!("{}", uir.dump(pool)))
}

pub(super) fn display_tir(tirs: &[Tir], pool: &InternPool) -> Result<(), CompilerError> {
    write_stdout(format_args!("[TIR]\n"))?;
    write_stdout(format_args!("{}", tir::dump(tirs, pool)))
}

pub(super) fn display_flow(
    tirs: &[Tir],
    flows: &[ryo_core::flow::Flow],
    sidecar: &ryo_core::ownership::OwnershipSidecar,
    pool: &InternPool,
) -> Result<(), CompilerError> {
    write_stdout(format_args!("[Flow]\n"))?;
    for ((tir, flow), cleanup) in tirs.iter().zip(flows).zip(&sidecar.functions) {
        write_stdout(format_args!(
            "{}",
            flow.dump(tir, cleanup, pool)
                .map_err(CompilerError::CodegenError)?
        ))?;
    }
    Ok(())
}

pub(super) fn generate_and_display_ir(
    tirs: &[Tir],
    flows: &[ryo_core::flow::Flow],
    pool: &InternPool,
    sidecar: &ryo_core::ownership::OwnershipSidecar,
) -> Result<(), CompilerError> {
    let target = Triple::host();
    let mut codegen = codegen::Codegen::new_aot(target).map_err(CompilerError::CodegenError)?;
    let ir = codegen
        .compile_and_dump_ir(tirs, flows, pool, sidecar)
        .map_err(CompilerError::CodegenError)?;

    write_stdout(format_args!("[Cranelift IR]\n"))?;
    write_stdout(format_args!("{}", ir))
}
