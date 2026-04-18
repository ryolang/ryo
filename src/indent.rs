use chumsky::span::{SimpleSpan, Span};

use crate::lexer::Token;

type Spanned<'a> = (Token<'a>, SimpleSpan);

pub fn process<'a>(tokens: Vec<Spanned<'a>>) -> Result<Vec<Spanned<'a>>, String> {
    let mut result: Vec<Spanned<'a>> = Vec::new();
    let mut indent_stack: Vec<usize> = vec![0];
    let mut i = 0;

    while i < tokens.len() {
        let (tok, span) = &tokens[i];

        if let Token::Newline(s) = tok {
            let whitespace = &s[1..]; // skip the '\n' character

            // Validate indentation for non-empty lines
            if i + 1 < tokens.len() && !matches!(&tokens[i + 1].0, Token::Newline(_)) {
                validate_indentation(whitespace)?;
                let new_level = whitespace.chars().filter(|c| *c == '\t').count();
                let current_level = *indent_stack.last().unwrap();

                if new_level > current_level {
                    indent_stack.push(new_level);
                    result.push((Token::Indent, *span));
                } else if new_level < current_level {
                    while *indent_stack.last().unwrap() > new_level {
                        indent_stack.pop();
                        result.push((Token::Dedent, *span));
                    }
                    if *indent_stack.last().unwrap() != new_level {
                        return Err(format!(
                            "Indentation error: dedent to level {} does not match any outer indentation level",
                            new_level
                        ));
                    }
                }
            }
            // Always push the newline token for the parser
            result.push((Token::Newline(*s), *span));
        } else {
            result.push((tok.clone(), *span));
        }

        i += 1;
    }

    // At EOF, emit Dedent for each remaining level above 0
    let eof_span = tokens
        .last()
        .map(|(_, s)| *s)
        .unwrap_or(SimpleSpan::new((), 0..0));
    while indent_stack.len() > 1 {
        indent_stack.pop();
        result.push((Token::Dedent, eof_span));
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
    use crate::lexer::leak_token;
    use logos::Logos;

    fn lex_raw(input: &str) -> Vec<Spanned<'static>> {
        Token::lexer(input)
            .spanned()
            .filter_map(|result| match result {
                (Ok(tok), span) => {
                    let static_tok = leak_token(tok);
                    Some((static_tok, span.into()))
                }
                _ => None,
            })
            .collect()
    }

    fn has_token(tokens: &[Spanned<'_>], predicate: impl Fn(&Token<'_>) -> bool) -> bool {
        tokens.iter().any(|(tok, _)| predicate(tok))
    }

    fn count_token(tokens: &[Spanned<'_>], predicate: impl Fn(&Token<'_>) -> bool) -> usize {
        tokens.iter().filter(|(tok, _)| predicate(tok)).count()
    }

    #[test]
    fn flat_program_is_noop() {
        let raw = lex_raw("x = 42");
        let processed = process(raw).unwrap();
        assert!(!has_token(&processed, |t| matches!(t, Token::Indent)));
        assert!(!has_token(&processed, |t| matches!(t, Token::Dedent)));
        assert!(!has_token(&processed, |t| matches!(t, Token::Newline(_))));
    }

    #[test]
    fn flat_multiline_no_indent() {
        let raw = lex_raw("x = 1\ny = 2");
        let processed = process(raw).unwrap();
        assert!(!has_token(&processed, |t| matches!(t, Token::Indent)));
        assert!(!has_token(&processed, |t| matches!(t, Token::Dedent)));
    }

    #[test]
    fn single_indent_dedent() {
        let raw = lex_raw("fn foo():\n\treturn 1");
        let processed = process(raw).unwrap();
        assert_eq!(count_token(&processed, |t| matches!(t, Token::Indent)), 1);
        assert_eq!(count_token(&processed, |t| matches!(t, Token::Dedent)), 1);
    }

    #[test]
    fn two_functions() {
        let input = "fn foo():\n\treturn 1\n\nfn bar():\n\treturn 2";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        assert_eq!(count_token(&processed, |t| matches!(t, Token::Indent)), 2);
        assert_eq!(count_token(&processed, |t| matches!(t, Token::Dedent)), 2);
    }

    #[test]
    fn blank_lines_ignored() {
        let input = "fn foo():\n\n\n\treturn 1";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        assert_eq!(count_token(&processed, |t| matches!(t, Token::Indent)), 1);
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
        // Simulates nested blocks (future: if inside function)
        let input = "fn foo():\n\tx = 1\n\t\ty = 2\n\tz = 3";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        assert_eq!(count_token(&processed, |t| matches!(t, Token::Indent)), 2);
        assert_eq!(count_token(&processed, |t| matches!(t, Token::Dedent)), 2);
    }

    #[test]
    fn eof_emits_remaining_dedents() {
        let input = "fn foo():\n\tx = 1\n\t\ty = 2";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        // Level goes 0 -> 1 -> 2, EOF should emit 2 Dedents
        assert_eq!(count_token(&processed, |t| matches!(t, Token::Dedent)), 2);
    }

    #[test]
    fn newline_tokens_preserved() {
        let input = "fn foo():\n\treturn 1\n\nfn bar():\n\treturn 2";
        let raw = lex_raw(input);
        let processed = process(raw).unwrap();
        // Newlines are now preserved in output for the parser
        assert!(has_token(&processed, |t| matches!(t, Token::Newline(_))));
    }
}
