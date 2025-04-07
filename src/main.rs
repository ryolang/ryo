use crate::lexer::Token;
use crate::parser::parser;
use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::{
    input::{Stream, ValueInput},
    prelude::*,
};
use logos::Logos;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use target_lexicon::Triple;

mod ast;
mod codegen;
mod evaluator;
mod lexer;
mod parser;

fn main() -> Result<(), String> {
    //reads the input expression from the command line
    let input = env::args()
        .nth(1)
        .expect("Expected expression argument (e.g. `1 + 7 * (3 - 4) / 5`)");

    // Create a logos lexer over the source code
    let token_iter = Token::lexer(&input)
        .spanned()
        // Convert logos errors into tokens. We want parsing to be recoverable and not fail at the lexing stage, so
        // we have a dedicated `Token::Error` variant that represents a token error that was previously encountered
        .map(|(tok, span)| match tok {
            // Turn the `Range<usize>` spans logos gives us into chumsky's `SimpleSpan` via `Into`, because it's easier
            // to work with
            Ok(tok) => (tok, span.into()),
            Err(()) => (Token::Error, span.into()),
        });

    // Turn the token iterator into a stream that chumsky can use for things like backtracking
    let token_stream = Stream::from_iter(token_iter)
        // Tell chumsky to split the (Token, SimpleSpan) stream into its parts so that it can handle the spans for us
        // This involves giving chumsky an 'end of input' span: we just use a zero-width span at the end of the string
        .map((0..input.len()).into(), |(t, s): (_, _)| (t, s));

    // Parse the token stream with our chumsky parser
    match parser().parse(token_stream).into_result() {
        // If parsing was successful, attempt to evaluate the s-expression
        Ok(expr) => {
            println!(
                "[Input Expression]
{}",
                input
            );
            println!(
                "
[AST]
{:#?}",
                expr
            );

            // --- Codegen Steps ---
            println!(
                "
[Codegen]"
            );

            // 1. Get target triple
            let target_triple = Triple::host();
            println!("  Target: {}", target_triple);

            // 2. Create Codegen instance (passing the Triple again)
            let mut codegen = codegen::Codegen::new(target_triple.clone())?;
            println!("  Initialized Codegen for target.");

            // 3. Compile the expression
            let _func_id = codegen.compile(expr)?; // We don't need the FuncId for now
            println!("  Compiled expression to Cranelift IR.");

            // 4. Finish compilation and get object bytes
            let obj_bytes = codegen.finish()?;
            println!("  Generated object code ({} bytes).", obj_bytes.len());

            // 5. Write bytes to an object file
            let obj_filename = "output.o";
            fs::write(obj_filename, obj_bytes)
                .map_err(|e| format!("Failed to write object file '{}': {}", obj_filename, e))?;
            println!("  Wrote object file to '{}'.", obj_filename);

            // --- Linking Step ---
            println!(
                "
[Linking]"
            );
            let exe_filename = "output_executable";
            // Try zig cc first, then clang, then cc as fallbacks
            let linker_status = Command::new("zig")
                .arg("cc") // Use zig's C compiler frontend for linking
                .arg(obj_filename)
                .arg("-o")
                .arg(exe_filename)
                .status();

            let status = match linker_status {
                Ok(status) => {
                    println!("  Attempting link with 'zig cc'...");
                    status
                }
                Err(_) => {
                    println!("  'zig cc' not found or failed, trying 'clang'...");
                    match Command::new("clang")
                        .arg(obj_filename)
                        .arg("-o")
                        .arg(exe_filename)
                        .status()
                    {
                        Ok(status) => status,
                        Err(_) => {
                            println!("  'clang' not found, trying 'cc'...");
                            Command::new("cc")
                                .arg(obj_filename)
                                .arg("-o")
                                .arg(exe_filename)
                                .status()
                                .map_err(|e| format!("Failed to run linker 'cc': {}", e))?
                        }
                    }
                }
            };

            if !status.success() {
                return Err(format!("Linker failed with status: {}", status));
            }
            println!(
                "  Linked '{}' successfully -> '{}'.",
                obj_filename, exe_filename
            );

            // --- Execution Step ---
            println!(
                "
[Execution]"
            );
            let run_status = Command::new(format!("./{}", exe_filename))
                .status()
                .map_err(|e| format!("Failed to run executable '{}': {}", exe_filename, e))?;

            // On Unix-like systems, the exit code is the result.
            // Handle cases where the process might be terminated by a signal.
            match run_status.code() {
                Some(code) => {
                    println!("  '{}' exited with code: {}", exe_filename, code);
                    println!(
                        "
[Result] => {}",
                        code
                    ); // The exit code is our result
                }
                None => println!("  '{}' terminated by signal.", exe_filename),
            }

            // Optional: Clean up intermediate files
            // fs::remove_file(obj_filename).ok();
            // fs::remove_file(exe_filename).ok();

            Ok(()) // Indicate success
        }
        // If parsing was unsuccessful, generate a nice user-friendly diagnostic with ariadne. You could also use
        // codespan, or whatever other diagnostic library you care about. You could even just display-print the errors
        // with Rust's built-in `Display` trait, but it's a little crude
        Err(errs) => {
            let source_id = "cmdline";
            for err in errs {
                Report::build(
                    ReportKind::Error,
                    (source_id, err.span().start..err.span().end),
                )
                .with_code(3)
                .with_message(err.to_string())
                .with_label(
                    Label::new((source_id, err.span().into_range()))
                        .with_message(err.reason().to_string())
                        .with_color(Color::Red),
                )
                .finish()
                .eprint((source_id, Source::from(&input)))
                .unwrap();
            }
            Ok(()) // Return Ok(()) if parsing failed but errors were printed
        }
    }
}
