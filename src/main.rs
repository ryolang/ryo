use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod ast;
mod ast_lower;
mod builtins;
mod codegen;
mod diag;
mod errors;
mod hir;
mod indent;
mod lexer;
mod linker;
mod parser;
mod pipeline;
mod sema;
mod toolchain;
mod types;

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
    /// Generate and display Cranelift IR for a Ryo program
    Ir {
        /// Input file to generate IR for
        file: PathBuf,
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
        Commands::Ir { file } => pipeline::ir_command(&file)?,
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
