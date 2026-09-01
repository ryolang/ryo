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
//! get a stream the parser can consume directly. Problems found along
//! the way (invalid characters, bad literals, unknown escapes, indent
//! errors) are emitted as structured `Diag`s through a caller-supplied
//! `DiagSink`; lexing recovers and continues so several problems can
//! surface in a single run.

use chumsky::span::{SimpleSpan, Span as _};
use logos::Logos;
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::types::{InternPool, StringId};
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
    /// The literal `9223372036854775808` (i.e. `i64::MAX + 1`), whose
    /// positive form overflows `i64`. Only grammatical as the direct
    /// operand of unary `-`, where the parser folds it to
    /// `Literal::Int(i64::MIN)` — anywhere else it is a parse error.
    /// This is how `-9_223_372_036_854_775_808` stays spellable
    /// while sign resolution remains a unary operator.
    IntLitMin,
    /// IEEE-754 `f64` literal stored as its bit pattern so `Token`
    /// can keep `Eq + Hash`. Decoded with `f64::from_bits` by
    /// downstream consumers (parser/astgen).
    FloatLit(u64),
    StrLit(StringId),
    /// `b"..."` byte-string literal (M8.4.2). Payload is the DECODED
    /// byte content, interned via `InternPool::intern_bytes` — not
    /// necessarily valid UTF-8, so read it back with `pool.bytes_payload`.
    BytesLit(StringId),

    // Keywords.
    Fn,
    If,
    Elif,
    Else,
    Return,
    Mut,
    Move,
    Struct,
    Enum,
    Match,
    True,
    False,
    And,
    Or,
    Not,
    While,

    Inout,
    Amp,
    Break,
    Continue,
    For,
    In,

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
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    Colon,

    // Punctuation.
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Dot,

    // Newline + indentation tokens (post-processed by `indent`).
    Newline,
    Indent,
    Dedent,
}

impl fmt::Display for Token {
    /// Pool-free display fallback used by chumsky's `Rich` error
    /// formatting. Identifier and string-literal payloads render as
    /// opaque handle ids; the driver re-renders parse diagnostics
    /// through the pool (see `parse_source` in `ryo-driver`), so
    /// users see the actual text in error reports and this fallback
    /// only shows up in non-driver contexts (unit tests, Debug
    /// dumps).
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Error => write!(f, "<error>"),
            Self::IntLit(n) => write!(f, "{}", n),
            Self::IntLitMin => write!(f, "9223372036854775808"),
            Self::FloatLit(bits) => write!(f, "{}", f64::from_bits(*bits)),
            Self::StrLit(id) => write!(f, "<str#{}>", id.raw()),
            Self::BytesLit(id) => write!(f, "<bytes#{}>", id.raw()),
            Self::Fn => write!(f, "fn"),
            Self::If => write!(f, "if"),
            Self::Elif => write!(f, "elif"),
            Self::Else => write!(f, "else"),
            Self::Return => write!(f, "return"),
            Self::Mut => write!(f, "mut"),
            Self::Move => write!(f, "move"),
            Self::Struct => write!(f, "struct"),
            Self::Enum => write!(f, "enum"),
            Self::Match => write!(f, "match"),
            Self::True => write!(f, "true"),
            Self::False => write!(f, "false"),
            Self::And => write!(f, "and"),
            Self::Or => write!(f, "or"),
            Self::Not => write!(f, "not"),
            Self::While => write!(f, "while"),
            Self::Break => write!(f, "break"),
            Self::Continue => write!(f, "continue"),
            Self::For => write!(f, "for"),
            Self::In => write!(f, "in"),
            Self::Inout => write!(f, "inout"),
            Self::Amp => write!(f, "&"),
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
            Self::PlusAssign => write!(f, "+="),
            Self::MinusAssign => write!(f, "-="),
            Self::StarAssign => write!(f, "*="),
            Self::SlashAssign => write!(f, "/="),
            Self::PercentAssign => write!(f, "%="),
            Self::Colon => write!(f, ":"),
            Self::LParen => write!(f, "("),
            Self::RParen => write!(f, ")"),
            Self::LBrace => write!(f, "{{"),
            Self::RBrace => write!(f, "}}"),
            Self::LBracket => write!(f, "["),
            Self::RBracket => write!(f, "]"),
            Self::Comma => write!(f, ","),
            Self::Dot => write!(f, "."),
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
    // `b"..."` beats `Ident` by longest match.
    #[regex(r#"b"([^"\\]|\\.)*""#)]
    Bytes(&'a str),

    #[token("fn")]
    Fn,
    #[token("if")]
    If,
    #[token("elif")]
    Elif,
    #[token("else")]
    Else,
    #[token("return")]
    Return,
    #[token("mut")]
    Mut,
    #[token("move")]
    Move,
    #[token("inout")]
    Inout,
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
    #[token("while")]
    While,
    #[token("break")]
    Break,
    #[token("continue")]
    Continue,
    #[token("for")]
    For,
    #[token("in")]
    In,

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
    #[token("&")]
    Amp,
    #[token("+=")]
    PlusAssign,
    #[token("-=")]
    MinusAssign,
    #[token("*=")]
    StarAssign,
    #[token("/=")]
    SlashAssign,
    #[token("%=")]
    PercentAssign,
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
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(",")]
    Comma,
    #[token(".")]
    Dot,

    // CRLF line endings (the Windows-editor default) lex as the same
    // Newline token; the leading `\r` is part of the token's span so
    // byte offsets stay accurate, and `indent::process` skips it when
    // measuring indentation.
    #[regex(r"\r?\n[ \t]*")]
    Newline(&'a str),

    Indent,
    Dedent,

    // `[^\r\n]` (not `[^\n]`): in a CRLF file the comment must stop
    // before the `\r` too, so the `\r\n` stays inside the following
    // Newline token's span — same as every other line ending.
    #[regex(r"#[^\r\n]*", logos::skip, allow_greedy = true)]
    Comment,

    #[regex(r"[ \t\f]+", logos::skip)]
    Whitespace,
}

// ============================================================================
// Pipeline entry point
// ============================================================================

/// Heuristic estimate of the number of raw tokens in `input`, used to
/// pre-size the token buffers so the collect/indent passes don't
/// repeatedly reallocate. It is only a hint — correctness never
/// depends on it.
fn estimated_token_count(input: &str) -> usize {
    // Average observed token width across representative Ryo sources is
    // ~3 bytes (identifiers/keywords/literals dominate over 1-byte
    // punctuation), so len/2 avoids wild over-allocation on the common
    // case. It is NOT an upper bound: punctuation-dense input (a run of
    // 1-byte tokens like `(((((`) yields close to one token per byte —
    // more than len/2 — so the buffer may still grow there. That costs
    // a reallocation, nothing more. Always reserve at least a little.
    (input.len() / 2).max(16)
}

/// Run logos, indentation processing, and string/int interning in
/// one pass. Never fails hard: problems are emitted to `sink` and
/// lexing recovers (a `Token::Error` placeholder for invalid
/// characters, a zero literal for unparseable ints/floats) so the
/// parser still sees a well-formed stream and later stages can
/// co-surface their own diagnostics in the same run. The one
/// exception is an indentation failure: the stream is unusable
/// without Indent/Dedent markers, so this returns an empty vector
/// and the driver skips parsing.
pub fn lex(input: &str, pool: &mut InternPool, sink: &mut DiagSink) -> Vec<(Token, Span)> {
    // Pre-size the raw-token buffer instead of growing it from zero.
    // logos' `SpannedIter` reports only a trivial `(0, None)` size
    // hint, so `collect` would otherwise repeatedly reallocate and
    // memcpy the whole buffer as it grows. The estimate avoids most
    // of those reallocations on typical input — but it is NOT an
    // upper bound (see estimated_token_count): punctuation-dense
    // input may still grow the buffer, costing a reallocation,
    // nothing more.
    let mut raw_tokens: Vec<(RawToken<'_>, Span)> =
        Vec::with_capacity(estimated_token_count(input));
    raw_tokens.extend(
        RawToken::lexer(input)
            .spanned()
            .map(|(tok, span)| match tok {
                Ok(t) => (t, span.into()),
                Err(()) => {
                    // logos matched nothing at this span: the source
                    // holds a character the grammar doesn't recognize.
                    // The text is sliced from the input and escaped so
                    // control bytes (e.g. a lone `\r`) render readably.
                    let text = input[span.clone()].escape_debug().to_string();
                    sink.emit(Diag::error(
                        SimpleSpan::new((), span.clone()),
                        DiagCode::InvalidCharacter,
                        format!("invalid character '{}'", text),
                    ));
                    (RawToken::Error, span.into())
                }
            }),
    );

    let processed = match crate::indent::process(raw_tokens) {
        Ok(processed) => processed,
        Err(e) => {
            // `IndentError` carries the offending `Newline` token's
            // span — its text is the `\n` plus the following
            // whitespace, so the squiggle lands on the indentation
            // itself. Without indent markers the token stream is
            // unusable, so hand back an empty stream; the driver
            // skips parsing when it sees this together with sink
            // errors.
            sink.emit(Diag::error(e.span, DiagCode::ParseError, e.message));
            return Vec::new();
        }
    };

    let mut out = Vec::with_capacity(processed.len());
    for (raw, span) in processed {
        let tok = intern_token(raw, span, pool, sink);
        out.push((tok, span));
    }
    out
}

/// Decode standard escape sequences in a string/bytes-literal body.
///
/// Unknown escape sequences (e.g. `\q`) are preserved verbatim — the
/// backslash and the following character are kept as-is — and reported
/// through `sink` as an `UnknownEscape` error pointing at exactly the
/// bytes of the escape.
///
/// `hex_escapes` enables `\xNN` (exactly two hex digits, M8.4.2) —
/// accepted in bytes literals only; in string literals `\xNN` stays an
/// `UnknownEscape` until M8.7 (Literal Completeness).
///
/// `body_span` is the span of the unquoted body (the caller computes
/// it: 1 byte past the opening quote for `"..."`, 2 for `b"..."`).
fn unescape(inner: &str, body_span: Span, sink: &mut DiagSink, hex_escapes: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(inner.len());
    // Byte offset within `inner`; tracked manually (rather than
    // `char_indices`) because consuming an escape advances past
    // several chars at once.
    let mut i = 0;
    while i < inner.len() {
        // `i` only ever advances by a full char (`len_utf8`) or a
        // whole ASCII escape, so it is always a char boundary.
        debug_assert!(inner.is_char_boundary(i));
        let ch = inner[i..]
            .chars()
            .next()
            .expect("the `i < inner.len()` loop condition guarantees a char at i");
        if ch == '\\' {
            match inner[i + 1..].chars().next() {
                Some('n') => {
                    out.push(b'\n');
                    i += 2;
                }
                Some('t') => {
                    out.push(b'\t');
                    i += 2;
                }
                Some('r') => {
                    out.push(b'\r');
                    i += 2;
                }
                Some('\\') => {
                    out.push(b'\\');
                    i += 2;
                }
                Some('"') => {
                    out.push(b'"');
                    i += 2;
                }
                Some('0') => {
                    out.push(0);
                    i += 2;
                }
                Some('x') if hex_escapes => {
                    // `\xNN` — exactly two hex digits.
                    let digits = inner.get(i + 2..i + 4);
                    match digits.and_then(|d| u8::from_str_radix(d, 16).ok()) {
                        Some(byte) => {
                            out.push(byte);
                            i += 4;
                        }
                        None => {
                            let start = body_span.start.saturating_add(i);
                            let end = start.saturating_add(2);
                            sink.emit(Diag::error(
                                SimpleSpan::new((), start..end),
                                DiagCode::UnknownEscape,
                                "invalid '\\xNN' escape: exactly two hex digits required"
                                    .to_string(),
                            ));
                            out.push(b'\\');
                            out.push(b'x');
                            i += 2;
                        }
                    }
                }
                Some(c) => {
                    // Unknown escape: report it, then preserve the
                    // backslash and the following character verbatim.
                    // Saturating adds: the span is derived from
                    // `body_span` and in-bounds offsets, so it cannot
                    // overflow in practice, but a diagnostic span must
                    // never panic the reporter.
                    let start = body_span.start.saturating_add(i);
                    let end = start.saturating_add(1).saturating_add(c.len_utf8());
                    sink.emit(Diag::error(
                        SimpleSpan::new((), start..end),
                        DiagCode::UnknownEscape,
                        format!("unknown escape sequence '\\{}'", c),
                    ));
                    out.push(b'\\');
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    i += 1 + c.len_utf8();
                }
                None => {
                    out.push(b'\\');
                    i += 1;
                }
            }
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            i += ch.len_utf8();
        }
    }
    out
}

fn intern_token(
    raw: RawToken<'_>,
    span: Span,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> Token {
    match raw {
        RawToken::Error => Token::Error,
        // Integer literals are parsed as `i64` here; sign is applied
        // later via the unary `-` operator. The one value that
        // overflows on the positive side, `i64::MAX + 1`, gets the
        // dedicated `IntLitMin` token so `-9_223_372_036_854_775_808`
        // (i.e. `i64::MIN`) stays spellable — the parser folds
        // `- IntLitMin` to `Literal::Int(i64::MIN)` and rejects the
        // token everywhere else.
        //
        // Other parse failures (e.g. overflow) emit a diagnostic and
        // recover with a zero literal so the parser doesn't choke on
        // a placeholder token and cascade a spurious parse error on
        // top of the real problem.
        RawToken::Float(s) => match s.parse::<f64>() {
            Ok(n) => Token::FloatLit(n.to_bits()),
            Err(_) => {
                sink.emit(Diag::error(
                    span,
                    DiagCode::ParseError,
                    format!("invalid float literal: '{}'", s),
                ));
                Token::FloatLit(0f64.to_bits())
            }
        },
        RawToken::Int(s) => match s.parse::<i64>() {
            Ok(n) => Token::IntLit(n),
            Err(_) if s.parse::<u64>() == Ok(i64::MAX as u64 + 1) => Token::IntLitMin,
            Err(_) => {
                sink.emit(Diag::error(
                    span,
                    DiagCode::ParseError,
                    format!("invalid integer literal: '{}'", s),
                ));
                Token::IntLit(0)
            }
        },
        RawToken::Str(s) => {
            // Strip the surrounding quotes (regex guarantees they
            // balance) and decode standard escape sequences here so
            // the parser sees a single `StrLit(StringId)` token
            // pointing at the user-visible bytes.
            let inner = &s[1..s.len() - 1];
            let body_span =
                SimpleSpan::new((), span.start.saturating_add(1)..span.end.saturating_sub(1));
            let decoded = unescape(inner, body_span, sink, false);
            // `inner` is UTF-8 source text and every escape above
            // decodes to ASCII or re-encodes a `char`, so the decoded
            // body is always valid UTF-8.
            let decoded =
                String::from_utf8(decoded).expect("string literal body decodes to valid UTF-8");
            Token::StrLit(pool.intern_str(&decoded))
        }
        RawToken::Bytes(s) => {
            // Strip the `b"` prefix and closing quote. Raw non-ASCII
            // source bytes are rejected (M8.4.2): the text/binary
            // distinction stays visible in source — use `\xNN`.
            let inner = &s[2..s.len() - 1];
            if !inner.is_ascii() {
                sink.emit(Diag::error(
                    span,
                    DiagCode::InvalidCharacter,
                    "bytes literal must be ASCII; use \\xNN escapes for non-ASCII bytes"
                        .to_string(),
                ));
            }
            let body_span =
                SimpleSpan::new((), span.start.saturating_add(2)..span.end.saturating_sub(1));
            let decoded = unescape(inner, body_span, sink, true);
            Token::BytesLit(pool.intern_bytes(&decoded))
        }
        RawToken::Ident(s) => Token::Ident(pool.intern_str(s)),

        RawToken::Fn => Token::Fn,
        RawToken::If => Token::If,
        RawToken::Elif => Token::Elif,
        RawToken::Else => Token::Else,
        RawToken::Return => Token::Return,
        RawToken::Mut => Token::Mut,
        RawToken::Move => Token::Move,
        RawToken::Inout => Token::Inout,
        RawToken::Struct => Token::Struct,
        RawToken::Enum => Token::Enum,
        RawToken::Match => Token::Match,
        RawToken::True => Token::True,
        RawToken::False => Token::False,
        RawToken::And => Token::And,
        RawToken::Or => Token::Or,
        RawToken::Not => Token::Not,
        RawToken::While => Token::While,
        RawToken::Break => Token::Break,
        RawToken::Continue => Token::Continue,
        RawToken::For => Token::For,
        RawToken::In => Token::In,

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
        RawToken::Amp => Token::Amp,
        RawToken::PlusAssign => Token::PlusAssign,
        RawToken::MinusAssign => Token::MinusAssign,
        RawToken::StarAssign => Token::StarAssign,
        RawToken::SlashAssign => Token::SlashAssign,
        RawToken::PercentAssign => Token::PercentAssign,
        RawToken::Colon => Token::Colon,

        RawToken::LParen => Token::LParen,
        RawToken::RParen => Token::RParen,
        RawToken::LBrace => Token::LBrace,
        RawToken::RBrace => Token::RBrace,
        RawToken::LBracket => Token::LBracket,
        RawToken::RBracket => Token::RBracket,
        RawToken::Comma => Token::Comma,
        RawToken::Dot => Token::Dot,

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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_strings(input: &str) -> (Vec<Token>, InternPool) {
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let toks = lex(input, &mut pool, &mut sink);
        assert!(!sink.has_errors(), "lex errors: {:?}", sink.into_diags());
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
    fn lex_move_keyword() {
        let (toks, _) = lex_strings("move");
        assert_eq!(toks.len(), 1);
        assert!(matches!(toks[0], Token::Move));
    }

    #[test]
    fn lex_inout_keyword() {
        let (toks, _) = lex_strings("inout");
        assert_eq!(toks.len(), 1);
        assert!(matches!(toks[0], Token::Inout));
    }

    #[test]
    fn lex_ampersand() {
        let (toks, _) = lex_strings("&");
        assert_eq!(toks.len(), 1);
        assert!(matches!(toks[0], Token::Amp));
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
        // An out-of-range integer literal emits a diagnostic and
        // recovers with a zero literal so parsing can continue
        // without a spurious cascade parse error.
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let toks = lex("99999999999999999999", &mut pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagCode::ParseError);
        assert_eq!(
            diags[0].message,
            "invalid integer literal: '99999999999999999999'"
        );
        assert!(
            toks.iter().any(|(t, _)| *t == Token::IntLit(0)),
            "recovery token IntLit(0) present: {:?}",
            toks
        );
    }

    #[test]
    fn lex_invalid_character_emits_diag_and_continues() {
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let toks = lex("x = @\ny = 2", &mut pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagCode::InvalidCharacter);
        assert_eq!(diags[0].message, "invalid character '@'");
        // The `@` sits at byte 4 in "x = @\ny = 2".
        assert_eq!(diags[0].span, SimpleSpan::new((), 4..5));
        // Recovery: a Token::Error placeholder is pushed and lexing
        // continues past the bad byte — the rest of the stream is
        // intact.
        assert!(toks.iter().any(|(t, _)| matches!(t, Token::Error)));
        let y = pool.intern_str("y");
        assert!(
            toks.iter().any(|(t, _)| *t == Token::Ident(y)),
            "tokens after the invalid character still lexed: {:?}",
            toks
        );
    }

    #[test]
    fn lex_i64_min_magnitude_gets_intlitmin() {
        // `9223372036854775808` (i64::MAX + 1) overflows i64 on the
        // positive side but is negatable, so it lexes to the
        // dedicated IntLitMin token with no diagnostic; one more and
        // it is an ordinary invalid-literal error.
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let toks = lex("9223372036854775808", &mut pool, &mut sink);
        assert!(!sink.has_errors(), "no diag for IntLitMin");
        assert_eq!(toks.len(), 1);
        assert!(matches!(toks[0].0, Token::IntLitMin));

        let mut sink = DiagSink::new();
        let toks = lex("9223372036854775809", &mut pool, &mut sink);
        assert!(sink.has_errors(), "overflow past i64::MAX + 1 errors");
        assert!(toks.iter().any(|(t, _)| *t == Token::IntLit(0)));
    }

    #[test]
    fn lex_unknown_escape_emits_diag_and_preserves_verbatim() {
        // `\q` is not a recognized escape: the lexer reports it with
        // a span covering exactly those two bytes, and keeps the
        // backslash + char verbatim in the interned string.
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let toks = lex("s = \"a\\nb\\qc\"", &mut pool, &mut sink);
        let diags = sink.into_diags();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, DiagCode::UnknownEscape);
        assert_eq!(diags[0].message, "unknown escape sequence '\\q'");
        // Byte layout: 0:s 1:' ' 2:= 3:' ' 4:'"' 5:a 6:\ 7:n 8:b
        // 9:\ 10:q 11:c 12:'"' — the escape spans 9..11.
        assert_eq!(diags[0].span, SimpleSpan::new((), 9..11));
        let str_tok = toks
            .iter()
            .find_map(|(t, _)| match t {
                Token::StrLit(id) => Some(*id),
                _ => None,
            })
            .expect("string token present");
        assert_eq!(pool.str(str_tok), "a\nb\\qc");
    }

    #[test]
    fn lex_crlf_line_endings() {
        // CRLF is the Windows-editor default; a full program written
        // with `\r\n` must lex identically to its LF form.
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let crlf = lex("fn main():\r\n\tx = 1\r\n\ty = 2\r\n", &mut pool, &mut sink);
        assert!(
            !sink.has_errors(),
            "CRLF source should lex cleanly: {:?}",
            sink.into_diags()
        );
        assert!(
            crlf.iter().all(|(t, _)| !matches!(t, Token::Error)),
            "no error tokens in CRLF stream: {:?}",
            crlf
        );
        let kinds: Vec<Token> = crlf.iter().map(|(t, _)| *t).collect();
        assert_eq!(
            kinds,
            vec![
                Token::Fn,
                Token::Ident(pool.intern_str("main")),
                Token::LParen,
                Token::RParen,
                Token::Colon,
                Token::Indent,
                Token::Newline,
                Token::Ident(pool.intern_str("x")),
                Token::Assign,
                Token::IntLit(1),
                Token::Newline,
                Token::Ident(pool.intern_str("y")),
                Token::Assign,
                Token::IntLit(2),
                Token::Newline,
                Token::Dedent,
            ]
        );
    }

    #[test]
    fn lex_crlf_blank_lines_and_comments() {
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let toks = lex("x = 1\r\n\r\n# comment\r\ny = 2\r\n", &mut pool, &mut sink);
        assert!(
            !sink.has_errors(),
            "CRLF with blank lines and comments should lex cleanly: {:?}",
            sink.into_diags()
        );
        assert!(
            toks.iter().all(|(t, _)| !matches!(t, Token::Error)),
            "no error tokens: {:?}",
            toks
        );
    }

    #[test]
    fn lex_crlf_comment_leaves_cr_in_newline_span() {
        // An indented comment at end of line must stop before the
        // `\r`: the `\r\n` belongs to the Newline token's span, same
        // as lines without a comment.
        let src = "fn main():\r\n\t# c\r\n\tx = 1\r\n";
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let toks = lex(src, &mut pool, &mut sink);
        assert!(
            !sink.has_errors(),
            "should lex cleanly: {:?}",
            sink.into_diags()
        );
        let newlines: Vec<_> = toks
            .iter()
            .filter(|(t, _)| matches!(t, Token::Newline))
            .collect();
        assert!(
            newlines
                .iter()
                .any(|(_, span)| &src[span.start..span.end] == "\r\n\t"),
            "expected a Newline spanning \"\\r\\n\\t\" after the comment, got: {:?}",
            newlines
                .iter()
                .map(|(_, span)| &src[span.start..span.end])
                .collect::<Vec<_>>()
        );
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

    #[test]
    fn while_break_continue_tokens() {
        let (toks, _) = lex_strings("while break continue");
        assert_eq!(toks, vec![Token::While, Token::Break, Token::Continue]);
    }

    #[test]
    fn for_in_tokens() {
        let (toks, pool) = lex_strings("for x in range");
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[0], Token::For);
        ident(&toks, 1, &pool, "x");
        assert_eq!(toks[2], Token::In);
        ident(&toks, 3, &pool, "range");
    }

    #[test]
    fn compound_assign_tokens() {
        let (toks, _) = lex_strings("x += 1");
        assert_eq!(toks.len(), 3);
        assert_eq!(toks[1], Token::PlusAssign);

        let (toks, _) = lex_strings("x -= 2");
        assert_eq!(toks[1], Token::MinusAssign);

        let (toks, _) = lex_strings("x *= 3");
        assert_eq!(toks[1], Token::StarAssign);

        let (toks, _) = lex_strings("x /= 4");
        assert_eq!(toks[1], Token::SlashAssign);

        let (toks, _) = lex_strings("x %= 5");
        assert_eq!(toks[1], Token::PercentAssign);
    }

    #[test]
    fn lex_brackets() {
        let (toks, pool) = lex_strings("s[1:2]");
        assert_eq!(toks.len(), 6);
        assert!(matches!(toks[0], Token::Ident(_)));
        assert_eq!(toks[1], Token::LBracket);
        assert_eq!(toks[2], Token::IntLit(1));
        assert_eq!(toks[3], Token::Colon);
        assert_eq!(toks[4], Token::IntLit(2));
        assert_eq!(toks[5], Token::RBracket);
        ident(&toks, 0, &pool, "s");
    }

    #[test]
    fn lex_full_slice_shorthand() {
        let (toks, _) = lex_strings("s[:]");
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[1], Token::LBracket);
        assert_eq!(toks[2], Token::Colon);
        assert_eq!(toks[3], Token::RBracket);
    }

    #[test]
    fn bytes_literal_lexes_and_decodes() {
        // toks: [Ident(x), Assign, BytesLit]
        let (toks, pool) = lex_strings(r#"x = b"A\x00\xff""#);
        match toks[2] {
            Token::BytesLit(id) => assert_eq!(pool.bytes_payload(id), b"A\x00\xff"),
            ref t => panic!("expected BytesLit at index 2, got {:?}", t),
        }
    }

    #[test]
    fn bytes_literal_decodes_string_escape_subset() {
        let (toks, pool) = lex_strings(r#"x = b"\n\t\r\\\"\0""#);
        match toks[2] {
            Token::BytesLit(id) => assert_eq!(pool.bytes_payload(id), b"\n\t\r\\\"\0"),
            ref t => panic!("expected BytesLit at index 2, got {:?}", t),
        }
    }

    #[test]
    fn bytes_literal_rejects_raw_non_ascii() {
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let _ = lex("x = b\"é\"", &mut pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            diags.iter().any(|d| d.code == DiagCode::InvalidCharacter),
            "expected InvalidCharacter, got {:?}",
            diags
        );
    }

    #[test]
    fn bytes_literal_rejects_malformed_hex_escape() {
        // `\x` must be followed by exactly two hex digits.
        for src in [r#"x = b"\x""#, r#"x = b"\x1""#, r#"x = b"\xg1""#] {
            let mut pool = InternPool::new();
            let mut sink = DiagSink::new();
            let _ = lex(src, &mut pool, &mut sink);
            let diags = sink.into_diags();
            assert!(
                diags.iter().any(|d| d.code == DiagCode::UnknownEscape),
                "expected UnknownEscape for {src}, got {:?}",
                diags
            );
        }
    }

    #[test]
    fn string_literal_still_rejects_hex_escape() {
        // `\xNN` is bytes-literal-only at M8.4.2; string escapes grow at M8.7.
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let _ = lex(r#"x = "\x41""#, &mut pool, &mut sink);
        let diags = sink.into_diags();
        assert!(
            diags.iter().any(|d| d.code == DiagCode::UnknownEscape),
            "expected UnknownEscape, got {:?}",
            diags
        );
    }

    #[test]
    fn b_ident_still_lexes_as_identifier() {
        // Longest-match: `b` alone is an Ident, `b"..."` is a BytesLit.
        let (toks, _) = lex_strings("bx = 1");
        match toks[0] {
            Token::Ident(_) => {}
            ref t => panic!("expected Ident at index 0, got {:?}", t),
        }
    }
}
