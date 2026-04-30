//! Lexer for Ryo.
//!
//! Logos drives the raw scan over `&str`, producing borrowed
//! `RawToken<'a>` slices into the source. That borrow form is private
//! to this module: callers receive a single `Token` enum which is
//! `Copy`, has no lifetime, and carries `StringId` / `i64` payloads
//! interned through `InternPool`.
//!
//! The `lex` entry point also runs the indentation pre-processor and
//! parses integer/string literals into their final form, so callers
//! get a stream the parser can consume directly.

use crate::types::{InternPool, StringId};
use chumsky::span::{SimpleSpan, Span as _};
use logos::Logos;
use std::fmt;

pub type Span = SimpleSpan;

// ============================================================================
// Public, interned token type
// ============================================================================

/// The token type seen by every consumer downstream of the lexer.
///
/// `Copy` and lifetime-free — the borrowed `&'a str` form lives only
/// inside this module. Identifiers and string literals reference an
/// `InternPool` `StringId`; integer literals are parsed eagerly so
/// the parser doesn't need to redo the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Error,

    // Literals (already parsed / interned).
    IntLit(i64),
    /// IEEE-754 `f64` literal stored as its bit pattern so `Token`
    /// can keep `Eq + Hash`. Decoded with `f64::from_bits` by
    /// downstream consumers (parser/astgen).
    FloatLit(u64),
    StrLit(StringId),

    // Keywords.
    Fn,
    If,
    Else,
    Return,
    Mut,
    Struct,
    Enum,
    Match,
    True,
    False,
    And,
    Or,
    Not,

    // Identifiers.
    Ident(StringId),

    // Operators.
    Add,
    Arrow,
    Sub,
    Mul,
    Div,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    Assign,
    Colon,

    // Punctuation.
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,

    // Newline + indentation tokens (post-processed by `indent`).
    Newline,
    Indent,
    Dedent,
}

impl fmt::Display for Token {
    /// Pool-free display fallback used by chumsky's `Rich` error
    /// formatting. Identifier and string-literal payloads render as
    /// opaque handle ids; the driver re-renders diagnostics with the
    /// pool available, so users see the actual text in error
    /// reports.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Error => write!(f, "<error>"),
            Self::IntLit(n) => write!(f, "{}", n),
            Self::FloatLit(bits) => write!(f, "{}", f64::from_bits(*bits)),
            Self::StrLit(id) => write!(f, "<str#{}>", id.raw()),
            Self::Fn => write!(f, "fn"),
            Self::If => write!(f, "if"),
            Self::Else => write!(f, "else"),
            Self::Return => write!(f, "return"),
            Self::Mut => write!(f, "mut"),
            Self::Struct => write!(f, "struct"),
            Self::Enum => write!(f, "enum"),
            Self::Match => write!(f, "match"),
            Self::True => write!(f, "true"),
            Self::False => write!(f, "false"),
            Self::And => write!(f, "and"),
            Self::Or => write!(f, "or"),
            Self::Not => write!(f, "not"),
            Self::Ident(id) => write!(f, "<id#{}>", id.raw()),
            Self::Add => write!(f, "+"),
            Self::Arrow => write!(f, "->"),
            Self::Sub => write!(f, "-"),
            Self::Mul => write!(f, "*"),
            Self::Div => write!(f, "/"),
            Self::Percent => write!(f, "%"),
            Self::EqEq => write!(f, "=="),
            Self::NotEq => write!(f, "!="),
            Self::Lt => write!(f, "<"),
            Self::Gt => write!(f, ">"),
            Self::LtEq => write!(f, "<="),
            Self::GtEq => write!(f, ">="),
            Self::Assign => write!(f, "="),
            Self::Colon => write!(f, ":"),
            Self::LParen => write!(f, "("),
            Self::RParen => write!(f, ")"),
            Self::LBrace => write!(f, "{{"),
            Self::RBrace => write!(f, "}}"),
            Self::Comma => write!(f, ","),
            Self::Newline => write!(f, "<newline>"),
            Self::Indent => write!(f, "<indent>"),
            Self::Dedent => write!(f, "<dedent>"),
        }
    }
}

// ============================================================================
// Internal raw token (logos output, borrowed into source)
// ============================================================================

#[derive(Logos, Debug, PartialEq, Eq, Hash, Clone)]
pub(crate) enum RawToken<'a> {
    Error,

    // Float regex is declared *before* the int regex purely for
    // readability; logos picks the longest match regardless.
    #[regex(r"[0-9]+\.[0-9]+")]
    Float(&'a str),
    #[regex(r"[0-9]+")]
    Int(&'a str),
    #[regex(r#""([^"\\]|\\.)*""#)]
    Str(&'a str),

    #[token("fn")]
    Fn,
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("return")]
    Return,
    #[token("mut")]
    Mut,
    #[token("struct")]
    Struct,
    #[token("enum")]
    Enum,
    #[token("match")]
    Match,
    #[token("true")]
    True,
    #[token("false")]
    False,
    #[token("and")]
    And,
    #[token("or")]
    Or,
    #[token("not")]
    Not,

    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Ident(&'a str),

    #[token("+")]
    Add,
    #[token("->")]
    Arrow,
    #[token("-")]
    Sub,
    #[token("*")]
    Mul,
    #[token("/")]
    Div,
    #[token("%")]
    Percent,
    #[token("==")]
    EqEq,
    #[token("!=")]
    NotEq,
    #[token("<=")]
    LtEq,
    #[token(">=")]
    GtEq,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("=")]
    Assign,
    #[token(":")]
    Colon,

    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token(",")]
    Comma,

    #[regex(r"\n[ \t]*")]
    Newline(&'a str),

    Indent,
    Dedent,

    #[regex(r"#[^\n]*", logos::skip, allow_greedy = true)]
    Comment,

    #[regex(r"[ \t\f]+", logos::skip)]
    Whitespace,
}

// ============================================================================
// Pipeline entry point
// ============================================================================

/// Errors from the lex stage (indent processing, integer literal
/// parsing). One `Result` instead of `DiagSink` because Phase 1's
/// structured-diagnostics work lands on a parallel branch; this
/// stage will route through a `DiagSink` once the two phases
/// converge on `main`.
#[derive(Debug, Clone)]
pub struct LexError {
    /// Source span of the offending byte range. Currently consumed
    /// only by the parser-test glue, but the field is part of the
    /// public surface so the full-pipeline driver can route it
    /// through Ariadne once Phase 1's diag module lands here.
    #[allow(dead_code)]
    pub span: Span,
    pub message: String,
}

/// Run logos, indentation processing, and string/int interning in
/// one pass. Returns the spanned token stream the parser consumes,
/// or the first lex-time error encountered.
pub fn lex(input: &str, pool: &mut InternPool) -> Result<Vec<(Token, Span)>, LexError> {
    let raw_tokens: Vec<(RawToken<'_>, Span)> = RawToken::lexer(input)
        .spanned()
        .map(|(tok, span)| match tok {
            Ok(t) => (t, span.into()),
            Err(()) => (RawToken::Error, span.into()),
        })
        .collect();

    // TODO: have `indent::process` return a span/offset for the
    // offending newline so we can point Ariadne at the exact line.
    // For now, anchor on the last raw token's span (which is at
    // least *near* where the indent went wrong) and fall back to
    // 0..0 only for the empty-input case.
    let fallback_span = raw_tokens
        .last()
        .map(|(_, s)| *s)
        .unwrap_or_else(|| SimpleSpan::new((), 0..0));
    let processed = crate::indent::process(raw_tokens).map_err(|e| LexError {
        span: fallback_span,
        message: e,
    })?;

    let mut out = Vec::with_capacity(processed.len());
    for (raw, span) in processed {
        let tok = intern_token(raw, span, pool)?;
        out.push((tok, span));
    }
    Ok(out)
}

/// Decode standard escape sequences in a string-literal body.
///
/// TODO(phase-3): unknown escape sequences are silently preserved
/// (e.g. `\q` becomes the two characters `\` and `q`). Once the
/// pipeline-alignment Phase 1 work integrates with the lexer, emit
/// a structured `Diag` through a `DiagSink` here instead so the
/// user sees `unknown escape sequence '\q'` with a span pointing
/// at the offending byte. Matches the same gap noted at the
/// `LexError` definition.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('0') => out.push('\0'),
                Some(c) => {
                    // Unknown escape: preserve the backslash and
                    // the following character verbatim. See TODO
                    // above — this should become a diagnostic.
                    out.push('\\');
                    out.push(c);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn intern_token(raw: RawToken<'_>, span: Span, pool: &mut InternPool) -> Result<Token, LexError> {
    Ok(match raw {
        RawToken::Error => Token::Error,
        // Integer literals are parsed as `i64` here. This is a
        // documented limitation: the smallest `i64` value
        // (`-9_223_372_036_854_775_808`) is unrepresentable as a
        // positive integer literal because we resolve sign at
        // parse time via the unary `-` operator on top of a
        // positive token. Phase 3+ should switch to `u64`-with-
        // late-negation (or add an `IntLitMin` token variant) so
        // the literal can be spelled. Tracked alongside the
        // numeric-tower work in the roadmap.
        RawToken::Float(s) => match s.parse::<f64>() {
            Ok(n) => Token::FloatLit(n.to_bits()),
            Err(_) => {
                return Err(LexError {
                    span,
                    message: format!("invalid float literal: '{}'", s),
                });
            }
        },
        RawToken::Int(s) => match s.parse::<i64>() {
            Ok(n) => Token::IntLit(n),
            Err(_) => {
                return Err(LexError {
                    span,
                    message: format!("invalid integer literal: '{}'", s),
                });
            }
        },
        RawToken::Str(s) => {
            // Strip the surrounding quotes (regex guarantees they
            // balance) and decode standard escape sequences here so
            // the parser sees a single `StrLit(StringId)` token
            // pointing at the user-visible bytes.
            let inner = &s[1..s.len() - 1];
            let decoded = unescape(inner);
            Token::StrLit(pool.intern_str(&decoded))
        }
        RawToken::Ident(s) => Token::Ident(pool.intern_str(s)),

        RawToken::Fn => Token::Fn,
        RawToken::If => Token::If,
        RawToken::Else => Token::Else,
        RawToken::Return => Token::Return,
        RawToken::Mut => Token::Mut,
        RawToken::Struct => Token::Struct,
        RawToken::Enum => Token::Enum,
        RawToken::Match => Token::Match,
        RawToken::True => Token::True,
        RawToken::False => Token::False,
        RawToken::And => Token::And,
        RawToken::Or => Token::Or,
        RawToken::Not => Token::Not,

        RawToken::Add => Token::Add,
        RawToken::Arrow => Token::Arrow,
        RawToken::Sub => Token::Sub,
        RawToken::Mul => Token::Mul,
        RawToken::Div => Token::Div,
        RawToken::Percent => Token::Percent,
        RawToken::EqEq => Token::EqEq,
        RawToken::NotEq => Token::NotEq,
        RawToken::Lt => Token::Lt,
        RawToken::Gt => Token::Gt,
        RawToken::LtEq => Token::LtEq,
        RawToken::GtEq => Token::GtEq,
        RawToken::Assign => Token::Assign,
        RawToken::Colon => Token::Colon,

        RawToken::LParen => Token::LParen,
        RawToken::RParen => Token::RParen,
        RawToken::LBrace => Token::LBrace,
        RawToken::RBrace => Token::RBrace,
        RawToken::Comma => Token::Comma,

        RawToken::Newline(_) => Token::Newline,
        RawToken::Indent => Token::Indent,
        RawToken::Dedent => Token::Dedent,

        RawToken::Comment | RawToken::Whitespace => {
            // These variants are tagged `logos::skip` on `RawToken`
            // and never appear in the iterator output. If logos is
            // ever reconfigured to surface them, fail loudly so we
            // notice rather than silently producing `Token::Error`.
            unreachable!("logos::skip variants never reach intern_token")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_strings(input: &str) -> (Vec<Token>, InternPool) {
        let mut pool = InternPool::new();
        let toks = lex(input, &mut pool).expect("lex ok");
        let cleaned: Vec<Token> = toks
            .into_iter()
            .map(|(t, _)| t)
            .filter(|t| !matches!(t, Token::Newline | Token::Indent | Token::Dedent))
            .collect();
        (cleaned, pool)
    }

    fn ident(toks: &[Token], idx: usize, pool: &InternPool, expected: &str) {
        match toks[idx] {
            Token::Ident(id) => assert_eq!(pool.str(id), expected),
            ref t => panic!("expected ident at {}, got {:?}", idx, t),
        }
    }

    #[test]
    fn lex_keywords() {
        let (toks, _) = lex_strings("fn if else return mut struct enum match");
        assert_eq!(toks.len(), 8);
        assert_eq!(toks[0], Token::Fn);
        assert_eq!(toks[1], Token::If);
        assert_eq!(toks[2], Token::Else);
        assert_eq!(toks[3], Token::Return);
        assert_eq!(toks[4], Token::Mut);
        assert_eq!(toks[5], Token::Struct);
        assert_eq!(toks[6], Token::Enum);
        assert_eq!(toks[7], Token::Match);
    }

    #[test]
    fn lex_simple_identifier() {
        let (toks, pool) = lex_strings("foo");
        assert_eq!(toks.len(), 1);
        ident(&toks, 0, &pool, "foo");
    }

    #[test]
    fn lex_identifier_with_underscores_and_digits() {
        let (toks, pool) = lex_strings("my_var _private __dunder var1 test42");
        assert_eq!(toks.len(), 5);
        ident(&toks, 0, &pool, "my_var");
        ident(&toks, 1, &pool, "_private");
        ident(&toks, 2, &pool, "__dunder");
        ident(&toks, 3, &pool, "var1");
        ident(&toks, 4, &pool, "test42");
    }

    #[test]
    fn lex_assignment() {
        let (toks, pool) = lex_strings("x = 5");
        assert_eq!(toks.len(), 3);
        ident(&toks, 0, &pool, "x");
        assert_eq!(toks[1], Token::Assign);
        assert_eq!(toks[2], Token::IntLit(5));
    }

    #[test]
    fn lex_string_literal_strips_quotes_and_dedups() {
        let (toks, pool) = lex_strings("\"hi\" \"hi\" \"bye\"");
        assert_eq!(toks.len(), 3);
        let id_a = match toks[0] {
            Token::StrLit(id) => id,
            _ => panic!(),
        };
        let id_b = match toks[1] {
            Token::StrLit(id) => id,
            _ => panic!(),
        };
        let id_c = match toks[2] {
            Token::StrLit(id) => id,
            _ => panic!(),
        };
        assert_eq!(id_a, id_b, "duplicate strings dedup");
        assert_ne!(id_a, id_c);
        assert_eq!(pool.str(id_a), "hi");
        assert_eq!(pool.str(id_c), "bye");
    }

    #[test]
    fn lex_comment_skipped() {
        let (toks, _) = lex_strings("x = 5 # this is a comment");
        // Trailing comment is filtered; the synthesized newline post
        // the comment may have been collapsed by indent.
        assert!(toks.len() >= 3);
        match toks[0] {
            Token::Ident(_) => {}
            _ => panic!(),
        }
    }

    #[test]
    fn lex_int_overflow_emits_error() {
        let mut pool = InternPool::new();
        let res = lex("99999999999999999999", &mut pool);
        assert!(res.is_err());
    }

    #[test]
    fn lex_float_literal() {
        let (toks, _) = lex_strings("2.5");
        assert_eq!(toks.len(), 1);
        match toks[0] {
            Token::FloatLit(bits) => {
                assert!((f64::from_bits(bits) - 2.5).abs() < 1e-12);
            }
            ref t => panic!("expected FloatLit, got {:?}", t),
        }
    }

    #[test]
    fn lex_float_does_not_swallow_int() {
        let (toks, _) = lex_strings("3 14");
        assert_eq!(toks, vec![Token::IntLit(3), Token::IntLit(14)]);
    }

    #[test]
    fn lex_ordering_tokens() {
        let (toks, _) = lex_strings("< > <= >=");
        assert_eq!(toks, vec![Token::Lt, Token::Gt, Token::LtEq, Token::GtEq]);
    }

    #[test]
    fn lex_modulo_token() {
        let (toks, _) = lex_strings("a % b");
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[1], Token::Percent);
    }

    #[test]
    fn lex_lt_vs_lteq() {
        let (toks, _) = lex_strings("a <= b < c");
        assert_eq!(toks.len(), 5);
        assert_eq!(toks[1], Token::LtEq);
        assert_eq!(toks[3], Token::Lt);
    }

    #[test]
    fn lex_curly_braces_and_arrow() {
        let (toks, _) = lex_strings("{ } ->");
        assert_eq!(toks, vec![Token::LBrace, Token::RBrace, Token::Arrow]);
    }
}
