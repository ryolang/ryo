//! Value-range facts for spec §18 guard elision (Phase 1).
//!
//! A per-function dense table (indexed by `StringId::raw()`) from
//! binding name to inclusive `[lo, hi]` bounds,
//! seeded from dominating `if`/`while` comparisons against constants and
//! killed on assignment, `inout` passing, and joins whose predecessors
//! disagree. When both operands of `+`/`-`/`*` (or the operand of unary
//! `-`) have bounds whose result provably fits in `i64`, codegen emits
//! the raw Cranelift op instead of `s*_overflow` plus a panic guard.
//!
//! Soundness rule: a missing fact (`None`) always means "keep the
//! guard". Every range here must hold for *every* execution that
//! reaches the program point where it is consulted.
//!
//! This module also owns the fact-table plumbing: the dense
//! `StringId`-indexed slot-table helpers (`read_slot` / `write_slot` /
//! `restore_slots`, shared by all of codegen's scoped binding tables)
//! and the `impl Codegen` block that seeds and kills facts
//! (`seed_cond_facts`, `kill_fact`, `kill_assigned_since`,
//! `kill_loop_writes` and its write collectors).

use cranelift_module::Module;
use ryo_core::tir::{ParamMode, Tir, TirData, TirRef, TirTag};
use ryo_core::types::StringId;

use super::{Codegen, FunctionContext};

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
    /// May yield an empty range (`lo > hi`) only from contradictory
    /// conditions on a single path (e.g. `if x > 0:` nested in
    /// `if x < 0:`) — harmless: `checked_*` on it still returns `Some`
    /// only for results that fit, and such code never executes.
    pub fn intersect(self, other: Self) -> Self {
        Self {
            lo: self.lo.max(other.lo),
            hi: self.hi.min(other.hi),
        }
    }

    /// `Some` result range of `self + rhs` iff it provably fits `i64`.
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        Self::from_i128(
            self.lo as i128 + rhs.lo as i128,
            self.hi as i128 + rhs.hi as i128,
        )
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        Self::from_i128(
            self.lo as i128 - rhs.hi as i128,
            self.hi as i128 - rhs.lo as i128,
        )
    }

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
    pub fn checked_neg(self) -> Option<Self> {
        Self::from_i128(-(self.hi as i128), -(self.lo as i128))
    }
}

/// Bounds for a TIR expression: exact for integer constants, the
/// recorded fact for variables, unknown (`None`) for everything else.
pub(crate) fn int_range_of(tir: &Tir, facts: &[Option<IntRange>], r: TirRef) -> Option<IntRange> {
    let inst = tir.inst(r);
    match (inst.tag, inst.data) {
        (TirTag::IntConst, TirData::Int(v)) => Some(IntRange::point(v)),
        (TirTag::Var, TirData::Var(name)) => facts.get(name.raw() as usize).copied().flatten(),
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

impl<M: Module> Codegen<M> {
    /// Read a dense `StringId`-indexed slot table. Out-of-range ids
    /// read as absent — codegen never interns, so every name in the
    /// TIR is in bounds, but stay tolerant like `free_binding_name`.
    pub(crate) fn read_slot<T: Copy>(table: &[Option<T>], name: StringId) -> Option<T> {
        table.get(name.raw() as usize).copied().flatten()
    }

    /// Write a dense `StringId`-indexed slot table, pushing the
    /// previous slot value onto the table's undo log so scoped
    /// emitters (`emit_scoped_body`, the if/while join handling) can
    /// restore it via `restore_slots`.
    pub(crate) fn write_slot<T: Copy>(
        table: &mut [Option<T>],
        undo: &mut Vec<(u32, Option<T>)>,
        name: StringId,
        value: Option<T>,
    ) {
        let raw = name.raw();
        debug_assert!(
            (raw as usize) < table.len(),
            "binding name out of slot-table range"
        );
        if let Some(slot) = table.get_mut(raw as usize) {
            undo.push((raw, *slot));
            *slot = value;
        }
    }

    /// Restore a slot table to `mark` (a previous `undo.len()`):
    /// replay the undo log in reverse down to the mark, writing each
    /// saved old slot value back, then truncate the log.
    pub(crate) fn restore_slots<T: Copy>(
        table: &mut [Option<T>],
        undo: &mut Vec<(u32, Option<T>)>,
        mark: usize,
    ) {
        while undo.len() > mark {
            let (raw, old) = undo.pop().expect("len > mark");
            table[raw as usize] = old;
        }
    }

    /// Record an assignment: the binding's range fact dies here, and
    /// enclosing scopes learn (via `assigned_log`) to kill it at their
    /// join. Cheap enough to call on every assignment path.
    pub(crate) fn kill_fact(ctx: &mut FunctionContext<'_, M>, name: StringId) {
        Self::write_slot(&mut ctx.range_facts, &mut ctx.range_facts_undo, name, None);
        ctx.assigned_log.push(name);
    }

    /// Kill facts for every binding assigned since `mark` (a previous
    /// `assigned_log.len()`). Called after a scoped body's restore,
    /// where in-scope kills were rolled back.
    pub(crate) fn kill_assigned_since(ctx: &mut FunctionContext<'_, M>, mark: usize) {
        for i in mark..ctx.assigned_log.len() {
            let name = ctx.assigned_log[i];
            Self::write_slot(&mut ctx.range_facts, &mut ctx.range_facts_undo, name, None);
        }
    }

    /// Seed the facts a condition implies under `polarity`,
    /// intersecting with any fact that already holds here.
    pub(crate) fn seed_cond_facts(ctx: &mut FunctionContext<'_, M>, cond: TirRef, polarity: bool) {
        for (name, range) in cond_facts(ctx.tir, cond, polarity) {
            let seeded = match Self::read_slot(&ctx.range_facts, name) {
                Some(old) => old.intersect(range),
                None => range,
            };
            Self::write_slot(
                &mut ctx.range_facts,
                &mut ctx.range_facts_undo,
                name,
                Some(seeded),
            );
        }
    }

    /// Kill facts for every binding a loop may write (see
    /// `collect_loop_writes`): anything in the body, plus anything in
    /// `cond` when one is passed. Back-edge rule: a fact consulted
    /// inside a loop must hold on EVERY iteration, not just the first —
    /// the header is a join of the entry edge and the back-edge, which
    /// disagree once the body has run. The while-condition also
    /// re-evaluates per iteration, so this must run before the
    /// condition is emitted, not just before the body — and the
    /// condition itself is scanned, since an inout call inside it
    /// writes through its pointer on every iteration too.
    pub(crate) fn kill_loop_writes(
        ctx: &mut FunctionContext<'_, M>,
        cond: Option<TirRef>,
        body: &[TirRef],
    ) {
        let mut writes = Vec::new();
        if let Some(cond) = cond {
            Self::collect_writes_in(ctx.tir, cond, &mut writes);
        }
        Self::collect_loop_writes(ctx.tir, body, &mut writes);
        for name in writes {
            Self::write_slot(&mut ctx.range_facts, &mut ctx.range_facts_undo, name, None);
        }
    }

    /// Collect every binding a statement slice may write: `Assign` /
    /// `CompoundAssign` targets and names passed as `inout` call args
    /// (the callee may write through the pointer). Recursion drives off
    /// `walk_operands`, so nested if/elif/else/while/for-range bodies
    /// are covered.
    fn collect_loop_writes(tir: &Tir, stmts: &[TirRef], out: &mut Vec<StringId>) {
        for &stmt in stmts {
            Self::collect_writes_in(tir, stmt, out);
        }
    }

    fn collect_writes_in(tir: &Tir, r: TirRef, out: &mut Vec<StringId>) {
        let inst = tir.inst(r);
        match inst.tag {
            TirTag::Assign => out.push(tir.assign_view(r).name),
            TirTag::CompoundAssign => out.push(tir.compound_assign_view(r).name),
            TirTag::Call => {
                let view = tir.call_view(r);
                for (&arg, &mode) in view.args.iter().zip(view.modes.iter()) {
                    if mode != ParamMode::Inout {
                        continue;
                    }
                    // The inout arg was sema-lowered to its inner
                    // `Var(name)` ref — the same rule `local_name_of`
                    // uses. Anything else is not a plain binding write;
                    // ignore it conservatively.
                    let arg_inst = tir.inst(arg);
                    if let (TirTag::Var, TirData::Var(name)) = (arg_inst.tag, arg_inst.data) {
                        out.push(name);
                    }
                }
            }
            _ => {}
        }
        tir.walk_operands(r, &mut |_parent, child, _kind| {
            Self::collect_writes_in(tir, child, out);
        });
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
