//! Source spans as renderer-agnostic plain data.
//!
//! Spans stay renderer-agnostic plain data so future non-human/JSON
//! diagnostic consumers don't inherit a parser crate.

use crate::source::SourceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub source: SourceId,
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const EMPTY: Span = Span {
        source: SourceId::NONE,
        start: 0,
        end: 0,
    };

    pub const fn new(start: usize, end: usize) -> Span {
        Span {
            source: SourceId::NONE,
            start,
            end,
        }
    }

    pub const fn in_source(source: SourceId, start: usize, end: usize) -> Span {
        Span { source, start, end }
    }

    pub const fn with_start(self, start: usize) -> Span {
        Span { start, ..self }
    }

    pub const fn with_end(self, end: usize) -> Span {
        Span { end, ..self }
    }

    pub fn range(self) -> std::ops::Range<usize> {
        self.start..self.end
    }
}

impl Default for Span {
    fn default() -> Self {
        Self::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_new_holds_fields() {
        let span = Span::new(3, 7);
        assert_eq!(span.start, 3);
        assert_eq!(span.end, 7);
    }

    #[test]
    fn span_empty_is_zero_zero() {
        assert_eq!(Span::EMPTY.start, 0);
        assert_eq!(Span::EMPTY.end, 0);
    }

    #[test]
    fn span_range_roundtrips() {
        assert_eq!(Span::new(3, 7).range(), 3..7);
    }

    #[test]
    fn span_is_copy() {
        let a = Span::new(1, 2);
        let b = a;
        let c = a;
        assert_eq!(b, c);
    }

    #[test]
    fn empty_is_explicitly_source_less() {
        let real = Span::in_source(crate::source::SourceId::from_raw_parts(1, 0), 0, 0);
        assert_eq!(Span::EMPTY.source, crate::source::SourceId::NONE);
        assert_ne!(Span::EMPTY, real);
    }

    #[test]
    fn derived_spans_preserve_source_identity() {
        let source = crate::source::SourceId::from_raw_parts(7, 3);
        let span = Span::in_source(source, 4, 9).with_start(2).with_end(12);
        assert_eq!(span, Span::in_source(source, 2, 12));
    }
}
