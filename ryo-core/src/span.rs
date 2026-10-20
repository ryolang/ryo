//! Source spans as renderer-agnostic plain data.
//!
//! Spans stay renderer-agnostic plain data so future non-human/JSON
//! diagnostic consumers don't inherit a parser crate.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const EMPTY: Span = Span { start: 0, end: 0 };

    pub const fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    pub fn range(self) -> std::ops::Range<usize> {
        self.start..self.end
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
}
