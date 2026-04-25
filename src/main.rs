use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod ast;
mod astgen;
mod builtins;
mod codegen;
mod diag;
mod errors;
mod indent;
mod lexer;
mod linker;
mod parser;
mod pipeline;
mod sema;
mod tir;
mod toolchain;
mod types;
mod uir;

#[derive(Parser)]
#[command(name = "ryo")]
#[command(about = "The Ryo programming language compiler")]
#[command(version = env!("RYO_VERSION"))]
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
    /// Inspect intermediate representations of a Ryo program.
    ///
    /// `--emit` selects which IR sections to print. Sections always
    /// appear in pipeline order (AST → UIR → TIR → CLIF) regardless
    /// of the order given on the command line. With no flag the
    /// command preserves its pre-Phase-5 behaviour: AST + Cranelift
    /// IR.
    Ir {
        /// Input file to generate IR for
        file: PathBuf,
        /// Comma-separated list of IRs to dump: ast, uir, tir, clif.
        #[arg(long, value_delimiter = ',', value_enum)]
        emit: Vec<EmitKind>,
    },
    /// Compile and run a Ryo program (JIT)
    Run {
        /// Input file to compile and run
        file: PathBuf,
    },
    /// Compile a Ryo program to a standalone binary (AOT)
    Build {
        /// Input file to compile
        file: PathBuf,
    },
    /// Manage the Ryo toolchain (Zig linker)
    Toolchain {
        #[command(subcommand)]
        action: ToolchainAction,
    },
}

/// Which IR section(s) `ryo ir --emit=...` should print.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum EmitKind {
    /// Pretty-printed AST (parser output).
    Ast,
    /// Untyped IR (astgen output, Zig-style ZIR analogue).
    Uir,
    /// Typed IR (sema output, Zig-style AIR analogue).
    Tir,
    /// Cranelift IR (codegen output).
    Clif,
}

#[derive(Subcommand)]
enum ToolchainAction {
    /// Download and install the Zig linker
    Install,
    /// Show toolchain installation status
    Status,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Lex { file } => pipeline::lex_command(&file)?,
        Commands::Parse { file } => pipeline::parse_command(&file)?,
        Commands::Ir { file, emit } => pipeline::ir_command(&file, &emit)?,
        Commands::Run { file } => pipeline::run_file(&file)?,
        Commands::Build { file } => pipeline::build_file(&file)?,
        Commands::Toolchain { action } => match action {
            ToolchainAction::Install => {
                toolchain::ensure_zig()?;
                println!("Toolchain ready.");
            }
            ToolchainAction::Status => {
                let status = if toolchain::is_installed() {
                    "installed"
                } else {
                    "not installed"
                };
                println!("Zig version: {} ({status})", toolchain::pinned_version());
            }
        },
    }

    Ok(())
}
