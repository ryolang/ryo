//! Value-range facts for spec §18 guard elision (Phase 1).
//!
//! A per-function map from binding name to inclusive `[lo, hi]` bounds,
//! seeded from dominating `if`/`while` comparisons against constants and
//! killed on assignment, `inout` passing, and joins whose predecessors
//! disagree. When both operands of `+`/`-`/`*` (or the operand of unary
//! `-`) have bounds whose result provably fits in `i64`, codegen emits
//! the raw Cranelift op instead of `s*_overflow` plus a panic guard.
//!
//! Soundness rule: a missing fact (`None`) always means "keep the
//! guard". Every range here must hold for *every* execution that
//! reaches the program point where it is consulted.

use std::collections::HashMap;

use ryo_core::tir::{Tir, TirData, TirRef, TirTag};
use ryo_core::types::StringId;

/// Inclusive integer range. All arithmetic goes through `i128` so a
/// blown bound is detected, never wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntRange {
    pub lo: i64,
    pub hi: i64,
}

impl IntRange {
    pub fn point(v: i64) -> Self {
        Self { lo: v, hi: v }
    }

    /// `from_i128` rejects any bound outside `i64`; the elision caller
    /// treats `None` as "keep the guard".
    #[allow(dead_code)] // Task 3 (elision) is the first caller.
    fn from_i128(lo: i128, hi: i128) -> Option<Self> {
        if lo < i64::MIN as i128 || hi > i64::MAX as i128 {
            return None;
        }
        Some(Self {
            lo: lo as i64,
            hi: hi as i64,
        })
    }

    /// Tighten with another fact that holds at the same program point.
    /// May yield an empty range (`lo > hi`) on contradictory (dead)
    /// paths — harmless: `checked_*` on it still returns `Some` only
    /// for results that fit, and dead code may be compiled arbitrarily.
    pub fn intersect(self, other: Self) -> Self {
        Self {
            lo: self.lo.max(other.lo),
            hi: self.hi.min(other.hi),
        }
    }

    /// `Some` result range of `self + rhs` iff it provably fits `i64`.
    #[allow(dead_code)] // Task 3 (elision) is the first caller.
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        Self::from_i128(
            self.lo as i128 + rhs.lo as i128,
            self.hi as i128 + rhs.hi as i128,
        )
    }

    #[allow(dead_code)] // Task 3 (elision) is the first caller.
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        Self::from_i128(
            self.lo as i128 - rhs.hi as i128,
            self.hi as i128 - rhs.lo as i128,
        )
    }

    #[allow(dead_code)] // Task 3 (elision) is the first caller.
    pub fn checked_mul(self, rhs: Self) -> Option<Self> {
        let corners = [
            self.lo as i128 * rhs.lo as i128,
            self.lo as i128 * rhs.hi as i128,
            self.hi as i128 * rhs.lo as i128,
            self.hi as i128 * rhs.hi as i128,
        ];
        let lo = *corners.iter().min().expect("four corners");
        let hi = *corners.iter().max().expect("four corners");
        Self::from_i128(lo, hi)
    }

    /// `Some` iff `-self` fits `i64` — i.e. `lo > i64::MIN`.
    #[allow(dead_code)] // Task 3 (elision) is the first caller.
    pub fn checked_neg(self) -> Option<Self> {
        Self::from_i128(-(self.hi as i128), -(self.lo as i128))
    }
}

/// Bounds for a TIR expression: exact for integer constants, the
/// recorded fact for variables, unknown (`None`) for everything else.
#[allow(dead_code)] // Task 3 (elision) is the first caller.
pub(crate) fn int_range_of(
    tir: &Tir,
    facts: &HashMap<StringId, IntRange>,
    r: TirRef,
) -> Option<IntRange> {
    let inst = tir.inst(r);
    match (inst.tag, inst.data) {
        (TirTag::IntConst, TirData::Int(v)) => Some(IntRange::point(v)),
        (TirTag::Var, TirData::Var(name)) => facts.get(&name).copied(),
        _ => None,
    }
}

/// Facts implied by a `var <cmp> const` condition taken with the given
/// polarity (`true` = the condition holds on this path, `false` = it
/// does not). Comparisons without a constant side, and the
/// non-informative polarity of `==`/`!=`, yield nothing. `ICmp*`
/// operands are always `int` (strings compare through `StrCmp*`), so
/// the `Var` side is always a scalar binding.
pub(crate) fn cond_facts(tir: &Tir, cond: TirRef, polarity: bool) -> Vec<(StringId, IntRange)> {
    let inst = tir.inst(cond);
    let TirData::BinOp { lhs, rhs } = inst.data else {
        return Vec::new();
    };
    let (l_tag, l_data) = (tir.inst(lhs).tag, tir.inst(lhs).data);
    let (r_tag, r_data) = (tir.inst(rhs).tag, tir.inst(rhs).data);
    // Normalize to `var <rel> k`; a constant on the left flips the relation.
    let (name, k, tag) = match (l_tag, l_data, r_tag, r_data) {
        (TirTag::Var, TirData::Var(name), TirTag::IntConst, TirData::Int(k)) => (name, k, inst.tag),
        (TirTag::IntConst, TirData::Int(k), TirTag::Var, TirData::Var(name)) => {
            (name, k, flip(inst.tag))
        }
        _ => return Vec::new(),
    };
    // `checked_add/sub(1)` failing means the polarity is unsatisfiable
    // (e.g. `x > i64::MAX` true) — seed nothing; the guard fallback
    // keeps the (dead) path correct.
    let range = match (tag, polarity) {
        (TirTag::ICmpLt, true) => k.checked_sub(1).map(|hi| IntRange { lo: i64::MIN, hi }),
        (TirTag::ICmpLt, false) => Some(IntRange {
            lo: k,
            hi: i64::MAX,
        }),
        (TirTag::ICmpLe, true) => Some(IntRange {
            lo: i64::MIN,
            hi: k,
        }),
        (TirTag::ICmpLe, false) => k.checked_add(1).map(|lo| IntRange { lo, hi: i64::MAX }),
        (TirTag::ICmpGt, true) => k.checked_add(1).map(|lo| IntRange { lo, hi: i64::MAX }),
        (TirTag::ICmpGt, false) => Some(IntRange {
            lo: i64::MIN,
            hi: k,
        }),
        (TirTag::ICmpGe, true) => Some(IntRange {
            lo: k,
            hi: i64::MAX,
        }),
        (TirTag::ICmpGe, false) => k.checked_sub(1).map(|hi| IntRange { lo: i64::MIN, hi }),
        (TirTag::ICmpEq, true) | (TirTag::ICmpNe, false) => Some(IntRange::point(k)),
        _ => None,
    };
    range.map(|r| vec![(name, r)]).unwrap_or_default()
}

/// The relation with its operands swapped (`a < b` ≡ `b > a`).
fn flip(tag: TirTag) -> TirTag {
    match tag {
        TirTag::ICmpLt => TirTag::ICmpGt,
        TirTag::ICmpLe => TirTag::ICmpGe,
        TirTag::ICmpGt => TirTag::ICmpLt,
        TirTag::ICmpGe => TirTag::ICmpLe,
        other => other, // ICmpEq / ICmpNe are symmetric
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_add_fits() {
        let a = IntRange::point(2);
        let b = IntRange::point(3);
        assert_eq!(a.checked_add(b), Some(IntRange { lo: 5, hi: 5 }));
    }

    #[test]
    fn add_at_i64_edge_overflows() {
        let max = IntRange::point(i64::MAX);
        let one = IntRange::point(1);
        assert_eq!(max.checked_add(one), None);
        let min = IntRange::point(i64::MIN);
        let neg = IntRange::point(-1);
        assert_eq!(min.checked_add(neg), None);
    }

    #[test]
    fn sub_range_order() {
        // [2, 10] - [1, 3] = [2 - 3, 10 - 1] = [-1, 9]
        let a = IntRange { lo: 2, hi: 10 };
        let b = IntRange { lo: 1, hi: 3 };
        assert_eq!(a.checked_sub(b), Some(IntRange { lo: -1, hi: 9 }));
    }

    #[test]
    fn sub_underflow_detected() {
        // [i64::MIN, -1] - [1, 1] underflows at lo.
        let a = IntRange {
            lo: i64::MIN,
            hi: -1,
        };
        assert_eq!(a.checked_sub(IntRange::point(1)), None);
    }

    #[test]
    fn mul_uses_extreme_corner_products() {
        // [-3, 2] * [-4, 5]: corners -3*-4=12, -3*5=-15, 2*-4=-8, 2*5=10
        let a = IntRange { lo: -3, hi: 2 };
        let b = IntRange { lo: -4, hi: 5 };
        assert_eq!(a.checked_mul(b), Some(IntRange { lo: -15, hi: 12 }));
    }

    #[test]
    fn mul_overflow_detected() {
        let big = IntRange {
            lo: 1,
            hi: i64::MAX,
        };
        assert_eq!(big.checked_mul(big), None);
    }

    #[test]
    fn neg_fails_only_at_i64_min() {
        assert_eq!(
            IntRange {
                lo: i64::MIN + 1,
                hi: 5
            }
            .checked_neg(),
            Some(IntRange {
                lo: -5,
                hi: i64::MAX
            })
        );
        assert_eq!(
            IntRange {
                lo: i64::MIN,
                hi: 0
            }
            .checked_neg(),
            None
        );
    }

    #[test]
    fn intersect_clamps() {
        let a = IntRange {
            lo: 1,
            hi: i64::MAX,
        };
        let b = IntRange {
            lo: i64::MIN,
            hi: 99,
        };
        assert_eq!(a.intersect(b), IntRange { lo: 1, hi: 99 });
    }
}
