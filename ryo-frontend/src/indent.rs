//! Indentation pre-processor.
//!
//! Operates on the lexer's borrowed `RawToken<'a>` (the form that
//! still has a slice into source for `Newline`'s leading
//! whitespace). After this pass, `Newline` no longer carries any
//! payload of interest — the public `Token` strips it during
//! interning — and `Indent` / `Dedent` markers are inserted at
//! level transitions.

use chumsky::span::{SimpleSpan, Span};

use crate::lexer::RawToken;
use ryo_core::diag::DiagCode;

type Spanned<'a> = (RawToken<'a>, SimpleSpan);

const AST_DEPTH_LIMIT: usize = 256;
const INDENT_DEPTH_LIMIT: usize = AST_DEPTH_LIMIT + 1;

/// Failure of the indentation pre-processor.
///
/// Carries the span of the offending `Newline` token — that token's
/// text is the `\n` plus the following indentation whitespace, so the
/// diagnostic squiggle lands on the indentation itself rather than on
/// some unrelated earlier token.
#[derive(Debug, Clone)]
pub(crate) struct IndentError {
    pub span: SimpleSpan,
    pub code: DiagCode,
    pub message: String,
}

pub(crate) fn process<'a>(tokens: Vec<Spanned<'a>>) -> Result<Vec<Spanned<'a>>, IndentError> {
    // The output preserves every input token and only *adds*
    // Indent/Dedent markers, so `tokens.len()` is a tight lower bound.
    // Growing from zero here meant repeatedly reallocating and copying
    // the whole buffer (a large share of the lex-stage cost); reserve
    // the token count plus a heuristic headroom for the inserted
    // markers. The headroom is not a bound — a deep dedent emits one
    // Dedent per popped level, which can exceed it — it just keeps the
    // common case in already-allocated space.
    let mut result: Vec<Spanned<'a>> = Vec::with_capacity(tokens.len() + tokens.len() / 8 + 8);
    let mut indent_stack: Vec<usize> = vec![0];
    let mut i = 0;

    while i < tokens.len() {
        let (tok, span) = &tokens[i];

        if let RawToken::Newline(s) = tok {
            // Skip the newline itself: `\n`, or `\r\n` for CRLF
            // sources (the lexer's Newline regex matches `\r?\n`).
            let whitespace = match s.strip_prefix('\r') {
                Some(rest) => &rest[1..],
                None => &s[1..],
            };

            // Validate indentation for non-empty lines.
            if i + 1 < tokens.len() && !matches!(&tokens[i + 1].0, RawToken::Newline(_)) {
                if let Err(message) = validate_indentation(whitespace) {
                    return Err(IndentError {
                        span: *span,
                        code: DiagCode::ParseError,
                        message,
                    });
                }
                let new_level = whitespace.chars().filter(|c| *c == '\t').count();
                let current_level = *indent_stack
                    .last()
                    .expect("indent stack is seeded with base level 0 and never fully popped");

                if new_level > current_level {
                    if indent_stack.len() > INDENT_DEPTH_LIMIT {
                        return Err(IndentError {
                            span: *span,
                            code: DiagCode::NestingLimitExceeded,
                            message: format!(
                                "AST nesting exceeds the maximum depth of {AST_DEPTH_LIMIT}"
                            ),
                        });
                    }
                    indent_stack.push(new_level);
                    result.push((RawToken::Indent, *span));
                } else if new_level < current_level {
                    while *indent_stack
                        .last()
                        .expect("base level 0 stays on the stack: pops stop at `> new_level` and new_level >= 0")
                        > new_level
                    {
                        indent_stack.pop();
                        result.push((RawToken::Dedent, *span));
                    }
                    if *indent_stack
                        .last()
                        .expect("dedent loop above cannot pop the base level 0")
                        != new_level
                    {
                        return Err(IndentError {
                            span: *span,
                            code: DiagCode::ParseError,
                            message: format!(
                                "Indentation error: dedent to level {} does not match any outer indentation level",
                                new_level
                            ),
                        });
                    }
                }
            }
            // Always preserve the newline so the parser can use it
            // as a statement terminator.
            result.push((RawToken::Newline(s), *span));
        } else {
            result.push((tok.clone(), *span));
        }

        i += 1;
    }

    // Emit a Dedent for every remaining level above 0 at EOF.
    let eof_span = tokens
        .last()
        .map(|(_, s)| *s)
        .unwrap_or(SimpleSpan::new((), 0..0));
    while indent_stack.len() > 1 {
        indent_stack.pop();
        result.push((RawToken::Dedent, eof_span));
    }

    Ok(result)
}

fn validate_indentation(whitespace: &str) -> Result<(), String> {
    for ch in whitespace.chars() {
        if ch == ' ' {
            return Err(
                "Indentation error: spaces are not allowed for indentation, use tabs".to_string(),
            );
        }
        if ch != '\t' {
            return Err(format!(
                "Indentation error: unexpected character '{}' in indentation",
                ch
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{Token, lex};
    use logos::Logos;
    use ryo_core::diag::DiagSink;
    use ryo_core::types::InternPool;

    fn lex_raw(input: &str) -> Vec<Spanned<'_>> {
        RawToken::lexer(input)
            .spanned()
            .filter_map(|result| match result {
                (Ok(tok), span) => Some((tok, span.into())),
                _ => None,
            })
            .collect()
    }

    fn has_token(tokens: &[Spanned<'_>], predicate: impl Fn(&RawToken<'_>) -> bool) -> bool {
        tokens.iter().any(|(tok, _)| predicate(tok))
    }

    fn count_token(tokens: &[Spanned<'_>], predicate: impl Fn(&RawToken<'_>) -> bool) -> usize {
        tokens.iter().filter(|(tok, _)| predicate(tok)).count()
    }

    #[test]
    fn lex_enforces_indentation_depth_limit_before_parsing() {
        fn source_at_depth(depth: usize) -> String {
            let mut src = String::from("fn main():\n");
            for level in 1..depth {
                src.push_str(&"\t".repeat(level));
                src.push_str("if true:\n");
            }
            src.push_str(&"\t".repeat(depth));
            src.push_str("print(1)");
            src
        }

        let accepted = source_at_depth(256);
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let tokens = lex(&accepted, &mut pool, &mut sink);
        assert!(!sink.has_errors(), "depth 256 must remain accepted");
        assert_eq!(
            tokens
                .iter()
                .filter(|(token, _)| matches!(token, Token::Indent))
                .count(),
            256
        );

        let rejected = source_at_depth(258);
        let offending_newline = rejected.rfind('\n').expect("nested source has newlines");
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let tokens = lex(&rejected, &mut pool, &mut sink);
        assert!(tokens.is_empty(), "parsing must not receive deep tokens");
        let diags = sink.into_diags();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagCode::NestingLimitExceeded);
        assert!(diags[0].message.contains("256"));
        assert_eq!(
            diags[0].span,
            ryo_core::span::Span::new(offending_newline, offending_newline + 259)
        );
    }

    #[test]
    fn flat_program_is_noop() {
        let raw = lex_raw("x = 42");
        let processed = process(raw).unwrap();
        assert!(!has_token(&processed, |t| matches!(t, RawToken::Indent)));
        assert!(!has_token(&processed, |t| matches!(t, RawToken::Dedent)));
        assert!(!has_token(&processed, |t| matches!(
            t,
            RawToken::Newline(_)
        )));
    }

    #[test]
    fn flat_multiline_no_indent() {
        let raw = lex_raw("x = 1\ny = 2");
        let processed = process(raw).unwrap();
        assert!(!has_token(&processed, |t| matches!(t, RawToken::Indent)));
        assert!(!has_token(&processed, |t| matches!(t, RawToken::Dedent)));
    }

    #[test]
    fn single_indent_dedent() {
        let raw = lex_raw("fn foo():\n\treturn 1");
        let processed = process(raw).unwrap();
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Indent)),
            1
        );
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Dedent)),
            1
        );
    }

    #[test]
    fn two_functions() {
        let input = "fn foo():\n\treturn 1\n\nfn bar():\n\treturn 2";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Indent)),
            2
        );
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Dedent)),
            2
        );
    }

    #[test]
    fn blank_lines_ignored() {
        let input = "fn foo():\n\n\n\treturn 1";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Indent)),
            1
        );
    }

    #[test]
    fn spaces_rejected() {
        let raw = lex_raw("fn foo():\n    return 1");
        let result = process(raw);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("spaces"));
    }

    #[test]
    fn error_span_points_at_offending_newline() {
        // The Newline token's text is `\n` plus the following
        // indentation whitespace, so its span is exactly where the
        // diagnostic should point.
        let raw = lex_raw("fn foo():\n    return 1");
        let newline_span = raw
            .iter()
            .find(|(t, _)| matches!(t, RawToken::Newline(_)))
            .map(|(_, s)| *s)
            .expect("input has a newline token");
        let err = process(raw).unwrap_err();
        assert_eq!(err.span, newline_span);
    }

    #[test]
    fn multi_level_indent() {
        let input = "fn foo():\n\tx = 1\n\t\ty = 2\n\tz = 3";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Indent)),
            2
        );
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Dedent)),
            2
        );
    }

    #[test]
    fn eof_emits_remaining_dedents() {
        let input = "fn foo():\n\tx = 1\n\t\ty = 2";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Dedent)),
            2
        );
    }

    #[test]
    fn newline_tokens_preserved() {
        let input = "fn foo():\n\treturn 1\n\nfn bar():\n\treturn 2";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        assert!(has_token(&processed, |t| matches!(t, RawToken::Newline(_))));
    }

    #[test]
    fn crlf_indentation() {
        // CRLF sources: the `\r` must not disturb indent measurement.
        let input = "fn foo():\r\n\tx = 1\r\n\t\ty = 2\r\n\tz = 3\r\n";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Indent)),
            2
        );
        assert_eq!(
            count_token(&processed, |t| matches!(t, RawToken::Dedent)),
            2
        );
    }
}
