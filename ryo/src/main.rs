use clap::{Parser, Subcommand};
use std::path::PathBuf;

use ryo_backend::toolchain;
use ryo_core::errors::CompilerError;
use ryo_driver::pipeline::{self, EmitKind};

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

#[derive(Subcommand)]
enum ToolchainAction {
    /// Download and install the Zig linker
    Install,
    /// Show toolchain installation status
    Status {
        /// Print the absolute path to the Zig binary (ensuring it is installed)
        #[arg(long)]
        path: bool,
    },
}

fn main() -> std::process::ExitCode {
    // Windows reserves 1 MiB for the main-thread stack (8 MiB on
    // macOS/Linux); the recursive front-end and JIT-executed programs
    // overflow that in debug builds. Run the CLI on a thread with an
    // explicit larger stack — a lazily-committed reserve, so it costs
    // nothing until used.
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(cli_main)
        .expect("failed to spawn the CLI thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

fn cli_main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run_command(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // Diagnostics were already rendered to stderr by the
            // pipeline; printing the error again would duplicate the
            // report. (Returning `Err` from `main` would also make the
            // std Termination handler add its own differently-formatted
            // summary line.)
            if !matches!(e, CompilerError::Diagnostics(_)) {
                eprintln!("error: {e}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn run_command(cli: Cli) -> Result<(), CompilerError> {
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
            ToolchainAction::Status { path } => {
                if path {
                    let zig_path = toolchain::ensure_zig()?;
                    print!("{}", zig_path.display());
                } else {
                    let status = if toolchain::is_installed() {
                        "installed"
                    } else {
                        "not installed"
                    };
                    println!("Zig version: {} ({status})", toolchain::pinned_version());
                }
            }
        },
    }

    Ok(())
}
