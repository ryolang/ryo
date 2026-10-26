use ryo_backend::codegen;
use ryo_core::ast;
use ryo_core::diag::DiagSink;
use ryo_core::errors::CompilerError;
use ryo_core::tir::{self, Tir};
use ryo_core::types::InternPool;
use ryo_core::uir::Uir;
use ryo_frontend::lexer::{self, Token};
use std::path::Path;
use target_lexicon::Triple;

use super::diagnostics::finalize_diags;
use super::source_name;

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

    println!("Token stream for '{}':", file.display());
    println!();

    // Render identifier and string-literal payloads through the
    // pool so the user sees the actual text rather than an opaque
    // handle id. Other variants format normally via Debug.
    for (tok, span) in &tokens {
        match tok {
            Token::Ident(id) => {
                println!("Ident({:?}) @ {}..{}", pool.str(*id), span.start, span.end)
            }
            Token::StrLit(id) => {
                println!("StrLit({:?}) @ {}..{}", pool.str(*id), span.start, span.end)
            }
            other => println!("{:?} @ {}..{}", other, span.start, span.end),
        }
    }
    Ok(())
}

pub(super) fn display_ast(program: &ast::Ast, pool: &InternPool) {
    println!("[AST]");
    print!("{}", ryo_core::ast_pretty::render_program(program, pool));
}

pub(super) fn display_uir(uir: &Uir, pool: &InternPool) {
    println!("[UIR]");
    print!("{}", uir.dump(pool));
}

pub(super) fn display_tir(tirs: &[Tir], pool: &InternPool) {
    println!("[TIR]");
    print!("{}", tir::dump(tirs, pool));
}

pub(super) fn display_flow(
    tirs: &[Tir],
    flows: &[ryo_core::flow::Flow],
    sidecar: &ryo_core::ownership::OwnershipSidecar,
    pool: &InternPool,
) -> Result<(), CompilerError> {
    println!("[Flow]");
    for ((tir, flow), cleanup) in tirs.iter().zip(flows).zip(&sidecar.functions) {
        print!(
            "{}",
            flow.dump(tir, cleanup, pool)
                .map_err(CompilerError::CodegenError)?
        );
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

    println!("[Cranelift IR]");
    print!("{}", ir);

    Ok(())
}
