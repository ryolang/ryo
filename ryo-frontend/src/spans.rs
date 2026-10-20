//! Span conversion at the chumsky boundary: the parser and
//! lexer produce chumsky `SimpleSpan` values; everything stored in
//! `ryo_core` IRs/diagnostics goes through `cspan`.

#[inline]
pub fn cspan(s: chumsky::span::SimpleSpan) -> ryo_core::diag::Span {
    s
}
