//! Span conversion at the chumsky boundary: parser and lexer spans carry
//! their source identity as chumsky context; everything stored in `ryo_core`
//! IRs/diagnostics goes through `cspan`.

use ryo_core::source::SourceId;

#[inline]
pub fn cspan(s: chumsky::span::SimpleSpan<usize, SourceId>) -> ryo_core::diag::Span {
    ryo_core::span::Span::in_source(s.context, s.start, s.end)
}
