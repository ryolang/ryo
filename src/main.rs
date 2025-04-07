use crate::lexer::Token;
use crate::parser::parser;
use ariadne::{Color, Label, Report, ReportKind, Source};
use chumsky::{
    input::{Stream, ValueInput},
    prelude::*,
};
use logos::Logos;
use std::env;

mod ast;
mod evaluator;
mod lexer;
mod parser;

const SRC: &str = r"
    (-
        (* (+ 4 7.3) 7)
        (/ 5 3)
    )
";

fn main() {
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
            println!("[AST]\n{:#?}", expr);
            //evaluates the AST to get the result
            println!("\n[result]\n{}", expr.eval());
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
        }
    }
}
