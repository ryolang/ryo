use crate::ast;
use crate::ast_lower;
use crate::codegen;
use crate::errors::CompilerError;
use crate::hir;
use crate::lexer::{self, Token};
use crate::linker;
use crate::parser::program_parser;
use crate::sema;
use crate::types::InternPool;
use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::{Parser, input::Stream, prelude::*};
use std::fs;
use std::path::Path;
use target_lexicon::Triple;

// Constants for magic strings
const SOURCE_ID: &str = "cmdline";

// Helper function to generate output filenames
fn get_output_filenames(input_file: &Path) -> (String, String) {
    let stem = input_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");

    let obj_filename = format!("{}.{}", stem, if cfg!(windows) { "obj" } else { "o" });
    let exe_filename = format!("{}{}", stem, std::env::consts::EXE_SUFFIX);

    (obj_filename, exe_filename)
}

pub(crate) fn lex_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    display_tokens(&input, file);
    Ok(())
}

fn display_tokens(input: &str, file: &Path) {
    let mut pool = InternPool::new();
    let tokens = match lexer::lex(input, &mut pool) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Lex error: {}", e.message);
            return;
        }
    };

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
}

pub(crate) fn parse_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let program = parse_source(&input, &mut pool)?;
    display_ast(&program, &pool);
    Ok(())
}

fn read_source_file(file: &Path) -> Result<String, CompilerError> {
    fs::read_to_string(file).map_err(CompilerError::from)
}

fn parse_source(input: &str, pool: &mut InternPool) -> Result<ast::Program, CompilerError> {
    let tokens = lexer::lex(input, pool)
        .map_err(|e| CompilerError::ParseError(format!("lex error: {}", e.message)))?;

    let token_stream =
        Stream::from_iter(tokens).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));

    match program_parser().parse(token_stream).into_result() {
        Ok(program) => Ok(program),
        Err(errs) => {
            display_parse_errors(&errs, input);
            Err(CompilerError::ParseError(
                "Parse errors occurred".to_string(),
            ))
        }
    }
}

fn display_parse_errors(errs: &[Rich<'_, Token>], input: &str) {
    let source = Source::from(input);
    for err in errs {
        Report::build(
            ReportKind::Error,
            (SOURCE_ID, err.span().start..err.span().end),
        )
        .with_code(3)
        .with_message(err.to_string())
        .with_label(
            Label::new((SOURCE_ID, err.span().into_range()))
                .with_message(err.reason().to_string())
                .with_color(Color::Red),
        )
        .finish()
        .eprint((SOURCE_ID, &source))
        .unwrap();
    }
}

fn display_ast(program: &ast::Program, pool: &InternPool) {
    println!("[AST]");
    program.pretty_print(pool);
}

pub(crate) fn ir_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let program = parse_source(&input, &mut pool)?;

    display_ast(&program, &pool);
    println!();

    let hir = lower_and_analyze(&program, &mut pool)?;
    generate_and_display_ir(&hir, &pool)?;

    Ok(())
}

/// Run the front-end (ast_lower + sema) and return a fully typed HIR
/// alongside its `InternPool`. Centralized so the three driver
/// commands (`ir`, `run`, `build`) stay in lockstep when future
/// pre-codegen passes are added.
fn lower_and_analyze(
    program: &ast::Program,
    pool: &mut InternPool,
) -> Result<hir::HirProgram, CompilerError> {
    let mut hir = ast_lower::lower(program, pool).map_err(CompilerError::LowerError)?;
    sema::analyze(&mut hir, pool).map_err(CompilerError::SemaError)?;
    Ok(hir)
}

fn generate_and_display_ir(hir: &hir::HirProgram, pool: &InternPool) -> Result<(), CompilerError> {
    let target = Triple::host();
    let mut codegen = codegen::Codegen::new_aot(target).map_err(CompilerError::CodegenError)?;
    let ir = codegen
        .compile_and_dump_ir(hir, pool)
        .map_err(CompilerError::CodegenError)?;

    println!("[Cranelift IR]");
    print!("{}", ir);

    Ok(())
}

pub(crate) fn run_file(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let program = parse_source(&input, &mut pool)?;

    println!("[Input Source]");
    println!("{}", input);
    println!();
    display_ast(&program, &pool);
    println!();

    let hir = lower_and_analyze(&program, &mut pool)?;

    println!("[Codegen]");
    let mut codegen = codegen::Codegen::new_jit().map_err(CompilerError::CodegenError)?;
    let main_id = codegen
        .compile(&hir, &pool)
        .map_err(CompilerError::CodegenError)?;
    let result = codegen
        .execute(main_id)
        .map_err(CompilerError::ExecutionError)?;

    display_result(result);

    Ok(())
}

pub(crate) fn build_file(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let mut pool = InternPool::new();
    let program = parse_source(&input, &mut pool)?;
    let hir = lower_and_analyze(&program, &mut pool)?;

    let (obj_filename, exe_filename) = get_output_filenames(file);

    println!("[Codegen]");
    let target = Triple::host();
    let mut codegen = codegen::Codegen::new_aot(target).map_err(CompilerError::CodegenError)?;
    codegen
        .compile(&hir, &pool)
        .map_err(CompilerError::CodegenError)?;
    let obj_bytes = codegen.finish().map_err(CompilerError::CodegenError)?;

    fs::write(&obj_filename, obj_bytes).map_err(CompilerError::from)?;
    println!("Generated object file: {}", obj_filename);

    linker::link_executable(&obj_filename, &exe_filename)?;
    let _ = fs::remove_file(&obj_filename);

    println!("Built: {}", exe_filename);
    Ok(())
}

fn display_result(result: i32) {
    println!("[Result] => {}", result);
}
