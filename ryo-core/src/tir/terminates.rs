//! Shared definite-termination analysis (return/jump flow) — split
//! from `tir.rs` to keep that file under the 2000-line CI gate.
//!
//! One implementation over the raw `(instructions, extra)` arenas both
//! [`Tir`][super::Tir] and [`TirBuilder`][super::TirBuilder] hold, so
//! mid-sema queries (match arm typing) and post-build queries (the
//! E0036 missing-return check) can never drift. `jumps` selects the
//! sense: `false` — only `return`/`never` count (E0036); `true` —
//! `break`/`continue` also count (the M12 match-arm sense: a statement
//! arm ending in a jump out of the match is `never` at the join, never
//! an E0135 fallthrough).
//!
//! Statement rules:
//! - `Return` / `ReturnVoid`: returns by construction. `Unreachable`:
//!   error-recovery sentinel — treated as terminating so a body that
//!   already errored doesn't cascade a spurious diagnostic.
//! - `Break` / `Continue`: terminate only in the `jumps` sense.
//! - `ExprStmt` whose operand is `never`-typed (e.g. a `panic` call):
//!   diverges, nothing past it executes.
//! - `VarDecl` / `Assign` / `CompoundAssign` whose evaluated
//!   initializer/value is `never`-typed: same divergence, the binding
//!   never completes.
//! - `IfStmt`: the then-block, every elif body, and the else block must
//!   all terminate; without an else the not-taken path falls through.
//! - Everything else — including loops, whose bodies can run zero times
//!   — falls through.

use super::{
    TirData, TirRef, TirTag, TypedInst, assign_extra, compound_assign_extra, if_stmt_extra,
    var_decl_extra,
};
use crate::types::InternPool;

pub(super) fn block_terminates(
    insts: &[TypedInst],
    extra: &[u32],
    stmts: impl IntoIterator<Item = TirRef>,
    pool: &InternPool,
    jumps: bool,
) -> bool {
    stmts
        .into_iter()
        .any(|s| stmt_terminates(insts, extra, s, pool, jumps))
}

fn stmt_terminates(
    insts: &[TypedInst],
    extra: &[u32],
    r: TirRef,
    pool: &InternPool,
    jumps: bool,
) -> bool {
    let inst = &insts[r.index()];
    match inst.tag {
        TirTag::Return | TirTag::ReturnVoid | TirTag::Unreachable => true,
        TirTag::Break | TirTag::Continue => jumps,
        TirTag::ExprStmt => {
            let TirData::UnOp(operand) = inst.data else {
                unreachable!("ExprStmt must carry TirData::UnOp");
            };
            pool.is_never(insts[operand.index()].ty)
        }
        TirTag::VarDecl => {
            let view = var_decl_extra::decode(extra_slice(extra, inst));
            pool.is_never(insts[view.initializer.index()].ty)
        }
        TirTag::Assign => {
            let view = assign_extra::decode(extra_slice(extra, inst));
            pool.is_never(insts[view.value.index()].ty)
        }
        TirTag::CompoundAssign => {
            let view = compound_assign_extra::decode(extra_slice(extra, inst));
            pool.is_never(insts[view.value.index()].ty)
        }
        TirTag::IfStmt => {
            // Only the bodies matter here; the fused decoder reads the
            // same wire `Tir::if_stmt_view` documents.
            let view = if_stmt_extra::decode(extra_slice(extra, inst));
            let Some(else_stmts) = view.else_stmts else {
                return false;
            };
            if !block_terminates(insts, extra, view.then_stmts, pool, jumps) {
                return false;
            }
            for elif in &view.elif_branches {
                if !block_terminates(insts, extra, elif.body, pool, jumps) {
                    return false;
                }
            }
            block_terminates(insts, extra, else_stmts, pool, jumps)
        }
        _ => false,
    }
}

/// The `extra` payload slice of an instruction that must carry
/// `TirData::Extra`.
fn extra_slice<'a>(extra: &'a [u32], inst: &TypedInst) -> &'a [u32] {
    let TirData::Extra(rng) = inst.data else {
        unreachable!("instruction must carry TirData::Extra");
    };
    &extra[rng.as_range()]
}
