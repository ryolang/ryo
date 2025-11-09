use crate::lexer::Token;
use crate::parser::program_parser;
use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::{Parser as ChumskyParser, input::Stream, prelude::*};
use clap::{Parser, Subcommand};
use logos::Logos;
use std::fs;
use std::path::{Path, PathBuf};

mod ast;
mod codegen;
mod evaluator;
mod lexer;
mod parser;

// Constants for magic strings
const SOURCE_ID: &str = "cmdline";

// Helper function to generate output filenames
fn get_output_filenames(input_file: &Path) -> (String, String) {
    let stem = input_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");

    let obj_filename = format!(
        "{}.{}",
        stem,
        if cfg!(windows) { "obj" } else { "o" }
    );
    let exe_filename = format!("{}{}", stem, std::env::consts::EXE_SUFFIX);

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
    /// Parse a Ryo source file and print the AST
    Parse {
        /// Input file to parse
        file: PathBuf,
    },
    // NOTE: Run command temporarily disabled until codegen is updated for new AST
    // /// Compile and run a Ryo program
    // Run {
    //     /// Input file to compile and run
    //     file: PathBuf,
    // },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Lex { file } => {
            lex_command(&file).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
        Commands::Parse { file } => {
            parse_command(&file).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
        // Commands::Run { file } => {
        //     run_file(&file).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        // }
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

fn parse_command(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;
    let program = parse_source(&input)?;
    display_ast(&program);
    Ok(())
}

fn read_source_file(file: &Path) -> Result<String, CompilerError> {
    fs::read_to_string(file).map_err(CompilerError::from)
}

fn parse_source(input: &str) -> Result<ast::Program, CompilerError> {
    let token_iter = Token::lexer(input).spanned().map(|(tok, span)| match tok {
        Ok(tok) => (tok, span.into()),
        Err(()) => (Token::Error, span.into()),
    });

    let token_stream =
        Stream::from_iter(token_iter).map((0..input.len()).into(), |(t, s): (_, _)| (t, s));

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
