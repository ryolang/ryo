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

type Spanned<'a> = (RawToken<'a>, SimpleSpan);

pub(crate) fn process<'a>(tokens: Vec<Spanned<'a>>) -> Result<Vec<Spanned<'a>>, String> {
    let mut result: Vec<Spanned<'a>> = Vec::new();
    let mut indent_stack: Vec<usize> = vec![0];
    let mut i = 0;

    while i < tokens.len() {
        let (tok, span) = &tokens[i];

        if let RawToken::Newline(s) = tok {
            let whitespace = &s[1..]; // skip the '\n' character

            // Validate indentation for non-empty lines.
            if i + 1 < tokens.len() && !matches!(&tokens[i + 1].0, RawToken::Newline(_)) {
                validate_indentation(whitespace)?;
                let new_level = whitespace.chars().filter(|c| *c == '\t').count();
                let current_level = *indent_stack.last().unwrap();

                if new_level > current_level {
                    indent_stack.push(new_level);
                    result.push((RawToken::Indent, *span));
                } else if new_level < current_level {
                    while *indent_stack.last().unwrap() > new_level {
                        indent_stack.pop();
                        result.push((RawToken::Dedent, *span));
                    }
                    if *indent_stack.last().unwrap() != new_level {
                        return Err(format!(
                            "Indentation error: dedent to level {} does not match any outer indentation level",
                            new_level
                        ));
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
    use logos::Logos;

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
        assert!(result.unwrap_err().contains("spaces"));
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
}
