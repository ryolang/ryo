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
    TirData, TirRef, TirTag, TypedInst, assign_extra, compound_assign_extra, read_ref_list,
    var_decl_extra,
};
use crate::types::InternPool;

pub(super) fn block_terminates(
    insts: &[TypedInst],
    extra: &[u32],
    stmts: &[TirRef],
    pool: &InternPool,
    jumps: bool,
) -> bool {
    stmts
        .iter()
        .any(|&s| stmt_terminates(insts, extra, s, pool, jumps))
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
            pool.is_never(insts[extra_ref_at(extra, inst, var_decl_extra::INIT).index()].ty)
        }
        TirTag::Assign => {
            pool.is_never(insts[extra_ref_at(extra, inst, assign_extra::VALUE).index()].ty)
        }
        TirTag::CompoundAssign => {
            pool.is_never(insts[extra_ref_at(extra, inst, compound_assign_extra::VALUE).index()].ty)
        }
        TirTag::IfStmt => {
            // Decode the arm blocks straight from the wire (same layout
            // `Tir::if_stmt_view` documents); only the bodies matter
            // here. Word 0 is the cond — the arm blocks start at 1.
            let slice = extra_slice(extra, inst);
            let mut pos = 1;
            let then_stmts = read_ref_list(slice, &mut pos);
            let elif_count = slice[pos] as usize;
            pos += 1;
            let mut elif_bodies = Vec::with_capacity(elif_count);
            for _ in 0..elif_count {
                pos += 1; // elif cond
                elif_bodies.push(read_ref_list(slice, &mut pos));
            }
            let has_else = slice[pos] != 0;
            pos += 1;
            if !has_else {
                return false;
            }
            let else_stmts = read_ref_list(slice, &mut pos);
            block_terminates(insts, extra, &then_stmts, pool, jumps)
                && elif_bodies
                    .iter()
                    .all(|body| block_terminates(insts, extra, body, pool, jumps))
                && block_terminates(insts, extra, &else_stmts, pool, jumps)
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

/// Word `offset` of an instruction's `extra` payload, read as a
/// [`TirRef`].
fn extra_ref_at(extra: &[u32], inst: &TypedInst, offset: usize) -> TirRef {
    TirRef::from_raw(extra_slice(extra, inst)[offset])
}
