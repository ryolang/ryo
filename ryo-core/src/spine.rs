//! Spill stack for operator-chain spine walks.
//!
//! Compiler passes that walk deep left-associative operator chains
//! iteratively (a 100k-term `1+1+...` chain nests one link per term)
//! keep the spine on an explicit stack: recursion would overflow the
//! thread stack long before the input is unreasonable. Real programs
//! nest operators only a few levels deep, so the first
//! [`SPINE_INLINE_CAP`] entries live in a fixed array and the common
//! case never allocates; pathologically deep chains spill to the heap
//! and stay O(links) overall.

/// Spine links held inline before spilling to the heap.
pub const SPINE_INLINE_CAP: usize = 16;

/// A last-in-first-out buffer that stays on the stack for the first
/// [`SPINE_INLINE_CAP`] entries, then spills to the heap. Consumed by
/// [`ChainSpine::fold_rev`], which folds from the deepest link outward
/// (reverse push order) — the shape a left-fold needs.
pub struct ChainSpine<T> {
    inline: [Option<T>; SPINE_INLINE_CAP],
    len: usize,
    spilled: Vec<T>,
}

impl<T> Default for ChainSpine<T> {
    fn default() -> Self {
        Self {
            inline: std::array::from_fn(|_| None),
            len: 0,
            spilled: Vec::new(),
        }
    }
}

impl<T> ChainSpine<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: T) {
        if self.spilled.is_empty() && self.len < SPINE_INLINE_CAP {
            self.inline[self.len] = Some(item);
            self.len += 1;
        } else {
            if self.spilled.is_empty() {
                self.spilled.reserve(SPINE_INLINE_CAP * 2);
                self.spilled.extend(
                    self.inline[..self.len]
                        .iter_mut()
                        .map(|slot| slot.take().expect("inline spine slot")),
                );
                self.len = 0;
            }
            self.spilled.push(item);
        }
    }

    /// Pop the most recently pushed item, if any.
    pub fn pop(&mut self) -> Option<T> {
        if let Some(item) = self.spilled.pop() {
            return Some(item);
        }
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        self.inline[self.len].take()
    }

    pub fn is_empty(&self) -> bool {
        self.spilled.is_empty() && self.len == 0
    }

    /// Fold the spine from the deepest link outward (reverse push
    /// order) — the shape a left-fold needs. The early-exit form for
    /// fallible folds: `f` returns `false` to stop the fold.
    pub fn fold_rev_until(mut self, mut f: impl FnMut(T) -> bool) {
        for item in self.spilled.drain(..).rev() {
            if !f(item) {
                return;
            }
        }
        for slot in self.inline[..self.len].iter_mut().rev() {
            if !f(slot.take().expect("inline spine slot")) {
                return;
            }
        }
    }

    pub fn fold_rev(self, f: impl FnMut(T)) {
        let mut f = f;
        self.fold_rev_until(|item| {
            f(item);
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_in_reverse_push_order_within_inline_capacity() {
        let mut spine = ChainSpine::new();
        for i in 0..SPINE_INLINE_CAP {
            spine.push(i);
        }
        let mut seen = Vec::new();
        spine.fold_rev(|item| seen.push(item));
        assert_eq!(seen, (0..SPINE_INLINE_CAP).rev().collect::<Vec<_>>());
    }

    #[test]
    fn spills_past_inline_capacity_and_still_folds_deep_first() {
        let total = SPINE_INLINE_CAP * 3 + 1;
        let mut spine = ChainSpine::new();
        for i in 0..total {
            spine.push(i);
        }
        let mut seen = Vec::new();
        spine.fold_rev(|item| seen.push(item));
        assert_eq!(seen, (0..total).rev().collect::<Vec<_>>());
    }
}
