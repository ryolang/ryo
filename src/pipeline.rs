use crate::ast;
use crate::ast_lower;
use crate::codegen;
use crate::errors::CompilerError;
use crate::hir;
use crate::indent;
use crate::lexer::Token;
use crate::linker;
use crate::parser::program_parser;
use crate::sema;
use crate::types::InternPool;
use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::{Parser, input::Stream, prelude::*};
use logos::Logos;
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
    let token_iter = Token::lexer(input).spanned();

    println!("Token stream for '{}':", file.display());
    println!();

    for (result, span) in token_iter {
        match result {
            Ok(token) => {
                println!("{:?} @ {}..{}", token, span.start, span.end);
            }
            Err(()) => {
                println!("Error @ {}..{}", span.start, span.end);
            }
        }
    }
}

pub(crate) fn parse_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let program = parse_source(&input)?;
    display_ast(&program);
    Ok(())
}

fn read_source_file(file: &Path) -> Result<String, CompilerError> {
    fs::read_to_string(file).map_err(CompilerError::from)
}

fn parse_source(input: &str) -> Result<ast::Program, CompilerError> {
    let raw_tokens: Vec<_> = Token::lexer(input)
        .spanned()
        .map(|(tok, span)| match tok {
            Ok(tok) => (tok, span.into()),
            Err(()) => (Token::Error, span.into()),
        })
        .collect();

    let tokens =
        indent::process(raw_tokens).map_err(|e| CompilerError::ParseError(e.to_string()))?;

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

fn display_parse_errors(errs: &[Rich<'_, Token<'_>>], input: &str) {
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

fn display_ast(program: &ast::Program) {
    println!("[AST]");
    program.pretty_print();
}

pub(crate) fn ir_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let program = parse_source(&input)?;

    display_ast(&program);
    println!();

    let (hir, pool) = lower_and_analyze(&program)?;
    generate_and_display_ir(&hir, &pool)?;

    Ok(())
}

/// Run the front-end (ast_lower + sema) and return a fully typed HIR
/// alongside its `InternPool`. Centralized so the three driver
/// commands (`ir`, `run`, `build`) stay in lockstep when future
/// pre-codegen passes are added.
fn lower_and_analyze(
    program: &ast::Program,
) -> Result<(hir::HirProgram, InternPool), CompilerError> {
    let mut pool = InternPool::new();
    let mut hir = ast_lower::lower(program, &mut pool).map_err(CompilerError::LowerError)?;
    sema::analyze(&mut hir, &mut pool).map_err(CompilerError::SemaError)?;
    Ok((hir, pool))
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
    let program = parse_source(&input)?;

    println!("[Input Source]");
    println!("{}", input);
    println!();
    display_ast(&program);
    println!();

    let (hir, pool) = lower_and_analyze(&program)?;

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
    let program = parse_source(&input)?;
    let (hir, pool) = lower_and_analyze(&program)?;

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
