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
use crate::extra::{ExtraList, ExtraListIter};
use crate::spine::ChainSpine;
use crate::types::InternPool;

pub(super) fn block_terminates(
    insts: &[TypedInst],
    extra: &[u32],
    stmts: impl IntoIterator<Item = TirRef>,
    pool: &InternPool,
    jumps: bool,
) -> bool {
    for stmt in stmts {
        if stmt_terminates(insts, extra, stmt, pool, jumps) {
            return true;
        }
    }
    false
}

fn stmt_terminates(
    insts: &[TypedInst],
    extra: &[u32],
    r: TirRef,
    pool: &InternPool,
    jumps: bool,
) -> bool {
    enum Task<'a> {
        Statement(TirRef),
        Body(ExtraList<'a, TirRef>),
        Result(bool),
    }

    enum Cursor<'a> {
        Body(ExtraListIter<'a, TirRef>),
        If {
            elifs: if_stmt_extra::TirElifIter<'a>,
            else_stmts: Option<ExtraList<'a, TirRef>>,
        },
    }

    let mut cursors = ChainSpine::new();
    let mut task = Task::Statement(r);
    loop {
        task = match task {
            Task::Statement(r) => {
                let inst = &insts[r.index()];
                match inst.tag {
                    TirTag::Return | TirTag::ReturnVoid | TirTag::Unreachable => Task::Result(true),
                    TirTag::Break | TirTag::Continue => Task::Result(jumps),
                    TirTag::ExprStmt => {
                        let TirData::UnOp(operand) = inst.data else {
                            unreachable!("ExprStmt must carry TirData::UnOp");
                        };
                        Task::Result(pool.is_never(insts[operand.index()].ty))
                    }
                    TirTag::VarDecl => {
                        let view = var_decl_extra::decode(extra_slice(extra, inst));
                        Task::Result(pool.is_never(insts[view.initializer.index()].ty))
                    }
                    TirTag::Assign => {
                        let view = assign_extra::decode(extra_slice(extra, inst));
                        Task::Result(pool.is_never(insts[view.value.index()].ty))
                    }
                    TirTag::CompoundAssign => {
                        let view = compound_assign_extra::decode(extra_slice(extra, inst));
                        Task::Result(pool.is_never(insts[view.value.index()].ty))
                    }
                    TirTag::IfStmt => {
                        // Only the bodies matter here; the fused decoder reads the
                        // same wire `Tir::if_stmt_view` documents.
                        let view = if_stmt_extra::decode(extra_slice(extra, inst));
                        let Some(else_stmts) = view.else_stmts else {
                            task = Task::Result(false);
                            continue;
                        };
                        cursors.push(Cursor::If {
                            elifs: view.elif_branches.iter(),
                            else_stmts: Some(else_stmts),
                        });
                        Task::Body(view.then_stmts)
                    }
                    _ => Task::Result(false),
                }
            }
            Task::Body(stmts) => {
                let mut stmts = stmts.into_iter();
                if let Some(stmt) = stmts.next() {
                    cursors.push(Cursor::Body(stmts));
                    Task::Statement(stmt)
                } else {
                    Task::Result(false)
                }
            }
            Task::Result(terminates) => {
                let Some(cursor) = cursors.pop() else {
                    return terminates;
                };
                match cursor {
                    Cursor::Body(mut stmts) => {
                        if terminates {
                            Task::Result(true)
                        } else if let Some(stmt) = stmts.next() {
                            cursors.push(Cursor::Body(stmts));
                            Task::Statement(stmt)
                        } else {
                            Task::Result(false)
                        }
                    }
                    Cursor::If {
                        mut elifs,
                        mut else_stmts,
                    } => {
                        if !terminates {
                            Task::Result(false)
                        } else if let Some(elif) = elifs.next() {
                            cursors.push(Cursor::If { elifs, else_stmts });
                            Task::Body(elif.body)
                        } else if let Some(stmts) = else_stmts.take() {
                            cursors.push(Cursor::If {
                                elifs,
                                else_stmts: None,
                            });
                            Task::Body(stmts)
                        } else {
                            Task::Result(true)
                        }
                    }
                }
            }
        };
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
