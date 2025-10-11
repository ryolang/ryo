use crate::lexer::Token;
use crate::parser::parser;
use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::{Parser as ChumskyParser, input::Stream, prelude::*};
use clap::{Parser, Subcommand};
use logos::Logos;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
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
    let exe_filename = format!("{}{}", stem, env::consts::EXE_SUFFIX);

    (obj_filename, exe_filename)
}

// Custom error types
#[derive(Debug)]
enum CompilerError {
    IoError(std::io::Error),
    ParseError(String),
    CodegenError(String),
    LinkError(String),
    ExecutionError(String),
}

impl std::fmt::Display for CompilerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompilerError::IoError(e) => write!(f, "IO error: {}", e),
            CompilerError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            CompilerError::CodegenError(msg) => write!(f, "Codegen error: {}", msg),
            CompilerError::LinkError(msg) => write!(f, "Link error: {}", msg),
            CompilerError::ExecutionError(msg) => write!(f, "Execution error: {}", msg),
        }
    }
}

impl std::error::Error for CompilerError {}

impl From<std::io::Error> for CompilerError {
    fn from(error: std::io::Error) -> Self {
        CompilerError::IoError(error)
    }
}

mod ast;
mod codegen;
mod evaluator;
mod lexer;
mod parser;

#[derive(Parser)]
#[command(name = "ryo")]
#[command(about = "The Ryo programming language compiler")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Tokenize a Ryo source file and print the token stream
    Lex {
        /// Input file to tokenize
        file: PathBuf,
    },
    /// Compile and run a Ryo program
    Run {
        /// Input file to compile and run
        file: PathBuf,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Lex { file } => {
            lex_command(&file).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
        Commands::Run { file } => {
            run_file(&file).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
    }
}

fn lex_command(file: &Path) -> Result<(), CompilerError> {
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

fn read_source_file(file: &Path) -> Result<String, CompilerError> {
    fs::read_to_string(file).map_err(CompilerError::from)
}

fn parse_source(input: &str) -> Result<ast::Expr, CompilerError> {
    let token_iter = Token::lexer(input).spanned().map(|(tok, span)| match tok {
        Ok(tok) => (tok, span.into()),
        Err(()) => (Token::Error, span.into()),
    });

    let token_stream =
        Stream::from_iter(token_iter).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));

    match parser().parse(token_stream).into_result() {
        Ok(expr) => Ok(expr),
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

fn display_input_and_ast(input: &str, expr: &ast::Expr) {
    println!("[Input Expression]\n{}", input);
    println!("\n[AST]");
    expr.pretty_print();
}

fn compile_to_object(expr: &ast::Expr) -> Result<Vec<u8>, CompilerError> {
    println!("\n[Codegen]");

    let target_triple = Triple::host();
    println!("  Target: {}", target_triple);

    let mut codegen =
        codegen::Codegen::new(target_triple).map_err(|e| CompilerError::CodegenError(e))?;
    println!("  Initialized Codegen for target.");

    let _func_id = codegen
        .compile(expr.clone())
        .map_err(|e| CompilerError::CodegenError(e))?;
    println!("  Compiled expression to Cranelift IR.");

    let obj_bytes = codegen
        .finish()
        .map_err(|e| CompilerError::CodegenError(e))?;
    println!("  Generated object code ({} bytes).", obj_bytes.len());

    Ok(obj_bytes)
}

fn write_object_file(obj_bytes: Vec<u8>, obj_filename: &str) -> Result<(), CompilerError> {
    fs::write(obj_filename, obj_bytes)?;
    println!("  Wrote object file to '{}'.", obj_filename);
    Ok(())
}

fn link_executable(obj_filename: &str, exe_filename: &str) -> Result<(), CompilerError> {
    println!("\n[Linking]");
    let link_start = Instant::now();

    let status = try_linkers(obj_filename, exe_filename)?;

    if !status.success() {
        return Err(CompilerError::LinkError(format!(
            "Linker failed with status: {}",
            status
        )));
    }

    let link_duration = link_start.elapsed();
    println!(
        "  Linked '{}' successfully -> '{}' in {:.2}ms.",
        obj_filename,
        exe_filename,
        link_duration.as_secs_f64() * 1000.0
    );
    println!(
        "  Executable size: {} bytes",
        fs::metadata(exe_filename)
            .map_err(CompilerError::from)?
            .len()
    );

    Ok(())
}

fn try_linkers(
    obj_filename: &str,
    exe_filename: &str,
) -> Result<std::process::ExitStatus, CompilerError> {
    // Try zig cc first
    if let Ok(status) = Command::new("zig")
        .arg("cc")
        .arg(obj_filename)
        .arg("-o")
        .arg(exe_filename)
        .status()
    {
        println!("  Attempting link with 'zig cc'...");
        return Ok(status);
    }

    // Try clang
    println!("  'zig cc' not found or failed, trying 'clang'...");
    if let Ok(status) = Command::new("clang")
        .arg(obj_filename)
        .arg("-o")
        .arg(exe_filename)
        .status()
    {
        return Ok(status);
    }

    // Try cc
    println!("  'clang' not found, trying 'cc'...");
    Command::new("cc")
        .arg(obj_filename)
        .arg("-o")
        .arg(exe_filename)
        .status()
        .map_err(|e| CompilerError::LinkError(format!("Failed to run linker 'cc': {}", e)))
}

fn execute_program(exe_filename: &str) -> Result<Option<i32>, CompilerError> {
    println!("\n[Execution]");
    let run_status = Command::new(format!("./{}", exe_filename))
        .status()
        .map_err(|e| {
            CompilerError::ExecutionError(format!(
                "Failed to run executable '{}': {}",
                exe_filename, e
            ))
        })?;

    match run_status.code() {
        Some(code) => {
            println!("  '{}' exited with code: {}", exe_filename, code);
            Ok(Some(code))
        }
        None => {
            println!("  '{}' terminated by signal.", exe_filename);
            Ok(None)
        }
    }
}

fn display_result(result: Option<i32>) {
    if let Some(code) = result {
        println!("\n[Result] => {}", code);
    }
}

fn run_file(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let expr = parse_source(&input)?;

    display_input_and_ast(&input, &expr);

    let obj_bytes = compile_to_object(&expr)?;
    let (obj_filename, exe_filename) = get_output_filenames(file);
    write_object_file(obj_bytes, &obj_filename)?;
    link_executable(&obj_filename, &exe_filename)?;
    let result = execute_program(&exe_filename)?;

    display_result(result);
    Ok(())
}
