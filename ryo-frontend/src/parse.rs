//! Source text → AST + diagnostics: lex, `parse_with_state`, and the `Rich` → `Diag` conversion. The driver consumes this through `parse_source` and owns rendering.

use chumsky::error::{Rich, RichPattern, RichReason};
use chumsky::{Parser, prelude::*};
use ryo_core::ast;
use ryo_core::diag::{Diag, DiagCode, DiagSink, ParseDiag};
use ryo_core::types::InternPool;

use crate::lexer::{self, Token};
use crate::parser::{ParseState, program_parser};
use crate::spans::cspan;

pub fn parse_source(
    input: &str,
    pool: &mut InternPool,
) -> Result<(ast::Ast, Vec<Diag>), Vec<Diag>> {
    // `lexer::lex` runs logos + indent processing + string and
    // integer interning in a single pass and never fails hard: it
    // emits structured `Diag`s into the sink and recovers so the
    // parser still sees a well-formed stream. Lex and parse
    // diagnostics accumulate in the same sink so both can surface in
    // one run, rendered through the same Ariadne pipeline as the
    // middle-end diagnostics.
    let mut sink = DiagSink::new();
    let tokens = lexer::lex(input, pool, &mut sink);

    // Indentation failure: the lexer returned an empty stream because
    // without Indent/Dedent markers parsing is meaningless. Skip the
    // parser and report the lex diagnostics as-is.
    if tokens.is_empty() && sink.has_errors() {
        return Err(sink.into_diags());
    }

    // chumsky 0.12 added `Input::split_token_span`, which collapses the previous
    // `Stream::from_iter(...).map(eoi, |(t, s)| (t, s))` boilerplate that we used to
    // pull `(Token, Span)` slices into a parser-friendly shape.
    let token_stream = tokens[..].split_token_span((0..input.len()).into());

    // R10: the parser recovers at statement boundaries, so a syntax
    // error yields a diagnostic AND a partial program (with `Error`
    // placeholder nodes). Callers thread the diagnostics into the
    // middle-end sink and keep analyzing, so one bad statement never
    // suppresses semantic diagnostics elsewhere in the file (R9).
    // The parser builds directly into the AST arenas, threaded as
    // chumsky parser state (the state also carries the intern pool
    // so positional field keys can mint their canonical names).
    let mut state = ParseState::new(std::mem::take(pool));
    let (out, errs) = program_parser()
        .parse_with_state(token_stream, &mut state)
        .into_output_errors();
    let (ast, parsed_pool) = state.into_parts();
    *pool = parsed_pool;
    for e in &errs {
        let span = cspan(*e.span());
        match e.reason() {
            // Parser-emitted custom errors carry their own `DiagCode`.
            // Most are self-contained; `UnknownAttribute` carries
            // interned ids, so resolve its spelling through the pool
            // here (same pattern as `rich_error_message` below), and
            // `MisplacedAttribute` rides its mandated explanation as a
            // structured note on the headline message.
            RichReason::Custom(pd) => {
                let diag = Diag::error(span, pd.code(), pd.message(pool));
                let diag = match pd {
                    ParseDiag::MisplacedAttribute => diag.with_note(
                        None,
                        "attributes are only supported on struct and enum definitions",
                    ),
                    _ => diag,
                };
                sink.emit(diag);
            }
            RichReason::ExpectedFound { .. } => sink.emit(Diag::error(
                span,
                DiagCode::ParseError,
                rich_error_message(e, pool),
            )),
        }
    }
    match out {
        Some(()) => Ok((ast, sink.into_diags())),
        // Unrecoverable parse (no output even after recovery — e.g.
        // trailing garbage that swallows the end-of-input marker).
        None => Err(sink.into_diags()),
    }
}

/// Render a token with the intern pool available: identifiers and
/// string literals show their actual text, everything else falls back
/// to the token's pool-free `Display` (which renders those payloads
/// as opaque `<id#N>` / `<str#N>` handles).
fn render_token_with_pool(tok: &Token, pool: &InternPool) -> String {
    match tok {
        Token::Ident(id) => pool.str(*id).to_string(),
        Token::StrLit(id) => format!("\"{}\"", pool.str(*id)),
        other => other.to_string(),
    }
}

/// Rebuild chumsky's `Rich` error message with identifier and
/// string-literal payloads resolved through the pool. Mirrors
/// chumsky's own phrasing ("found 'X' expected A, B, or C") so the
/// text stays familiar, but a parse error on `x = foo(` shows `foo`
/// instead of the opaque `<id#0>` handle. Only called for
/// `ExpectedFound` errors — `Custom` payloads carry their own code
/// and self-contained message, converted by the caller.
fn rich_error_message(e: &Rich<'_, Token, SimpleSpan, ParseDiag>, pool: &InternPool) -> String {
    match e.reason() {
        RichReason::Custom(_) => {
            unreachable!("custom parse errors are converted by the caller")
        }
        RichReason::ExpectedFound { .. } => {
            // Unclosed argument list: the parser stopped at a line break
            // while `)` was still a valid continuation. A token dump
            // ("found '<newline>' expected '.', '[', '*', …") sends the
            // user to the glossary instead of the paren — name the
            // problem and the two tokens that would continue the call.
            let at_line_break = matches!(e.found(), Some(Token::Newline | Token::Dedent));
            if at_line_break
                && e.expected().any(|p| match p {
                    RichPattern::Token(tok) => **tok == Token::RParen,
                    _ => false,
                })
            {
                return "unclosed argument list: expected ',' or ')' before the end of the line"
                    .to_string();
            }
            let found = match e.found() {
                Some(tok) => format!("found '{}'", render_token_with_pool(tok, pool)),
                None => "found end of input".to_string(),
            };
            let expected: Vec<String> = e
                .expected()
                .map(|p| match p {
                    RichPattern::Token(tok) => {
                        format!("'{}'", render_token_with_pool(tok, pool))
                    }
                    other => other.to_string(),
                })
                .collect();
            let expected_part = match expected.len() {
                0 => "something else".to_string(),
                1 => expected[0].clone(),
                _ => format!(
                    "{}, or {}",
                    expected[..expected.len() - 1].join(", "),
                    expected
                        .last()
                        .expect("this arm only runs when expected.len() >= 2")
                ),
            };
            format!("{} expected {}", found, expected_part)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_source;
    use ryo_core::types::InternPool;

    #[test]
    fn parse_error_renders_identifiers_through_pool() {
        // The token's pool-free `Display` renders identifiers as
        // opaque `<id#N>` handles; the driver's parse-error path
        // must re-render them through the pool so the user sees the
        // actual identifier text.
        let mut pool = InternPool::new();
        let (_program, diags) =
            parse_source("x foo = 1", &mut pool).expect("recovery should yield a partial program");
        assert!(!diags.is_empty());
        let msg = &diags[0].message;
        assert!(
            msg.contains("foo"),
            "message should name the identifier text: {msg}"
        );
        assert!(
            !msg.contains("<id#"),
            "message must not leak opaque handle ids: {msg}"
        );
    }
}
