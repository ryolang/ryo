use super::FlowBuilder;
use super::control::{self, ControlTask};
use crate::extra::ExtraList;
use crate::spine::ChainSpine;
use crate::tir::{TirData, TirRef, TirTag};

use super::{BlockId, Place, PlaceWrite, StepFacts, Terminator, WriteKind};

#[derive(Clone, Copy)]
pub(super) struct BodyCursor<'a> {
    stmts: ExtraList<'a, TirRef>,
    next: usize,
}

impl<'a> BodyCursor<'a> {
    pub(super) fn new(stmts: ExtraList<'a, TirRef>) -> Self {
        Self { stmts, next: 0 }
    }

    pub(super) fn next(&mut self) -> Option<TirRef> {
        let stmt = self.stmts.get(self.next);
        self.next += usize::from(stmt.is_some());
        stmt
    }
}

pub(super) enum Task<'a> {
    EnterExpr(TirRef),
    FinishExpr(TirRef),
    EnterStmt(TirRef),
    FinishStmt(TirRef),
    Body(BodyCursor<'a>),
    AfterBoolLhs {
        expr: TirRef,
        lhs: TirRef,
        rhs: TirRef,
        is_and: bool,
    },
    AfterBoolRhs {
        expr: TirRef,
        join: BlockId,
    },
    AfterNeverArgs(TirRef),
    Control(ControlTask),
    LegacyControl(TirRef),
}

pub(super) fn emit_expr(builder: &mut FlowBuilder<'_>, expr: TirRef) {
    debug_assert!(builder.pending.is_none());
    builder.pending = Some(Task::EnterExpr(expr));
    emit_root(builder);
}

pub(super) fn emit_stmt(builder: &mut FlowBuilder<'_>, stmt: TirRef) {
    debug_assert!(builder.pending.is_none());
    builder.pending = Some(Task::EnterStmt(stmt));
    emit_root(builder);
}

pub(super) fn emit_root(builder: &mut FlowBuilder<'_>) {
    let root = builder
        .pending
        .take()
        .unwrap_or_else(|| Task::Body(BodyCursor::new(builder.tir.body_stmts())));
    let mut tasks = ChainSpine::new();
    tasks.push(root);
    let mut children = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Body(mut cursor) => {
                if let Some(stmt) = cursor.next() {
                    tasks.push(Task::Body(cursor));
                    tasks.push(Task::EnterStmt(stmt));
                }
            }
            Task::EnterExpr(expr) => {
                builder.tir.walk_operands(expr, &mut |_p, child, kind| {
                    if kind == crate::tir::ChildKind::Operand {
                        builder.inst_meta[child.index()].parent = Some(expr.raw());
                    }
                });
                let inst = *builder.tir.inst(expr);
                match inst.tag {
                    TirTag::Match | TirTag::IfStmt => {
                        control::schedule_control(builder, expr, &mut tasks)
                    }
                    TirTag::WhileLoop | TirTag::ForRange => {
                        tasks.push(Task::LegacyControl(expr));
                    }
                    TirTag::BoolAnd | TirTag::BoolOr => {
                        let is_and = inst.tag == TirTag::BoolAnd;
                        let (lhs, rhs) = match inst.data {
                            TirData::BinOp { lhs, rhs } => (lhs, rhs),
                            _ => unreachable!("BoolAnd/BoolOr must carry TirData::BinOp"),
                        };
                        tasks.push(Task::AfterBoolLhs {
                            expr,
                            lhs,
                            rhs,
                            is_and,
                        });
                        tasks.push(Task::EnterExpr(lhs));
                    }
                    TirTag::Call if builder.pool.is_never(inst.ty) => {
                        let view = builder.tir.call_view(expr);
                        tasks.push(Task::AfterNeverArgs(expr));
                        for arg in view.args.iter().rev() {
                            tasks.push(Task::EnterExpr(arg));
                        }
                    }
                    _ => match inst.data {
                        TirData::BinOp { lhs, rhs } => {
                            tasks.push(Task::FinishExpr(expr));
                            tasks.push(Task::EnterExpr(rhs));
                            tasks.push(Task::EnterExpr(lhs));
                        }
                        TirData::UnOp(operand) => {
                            tasks.push(Task::FinishExpr(expr));
                            tasks.push(Task::EnterExpr(operand));
                        }
                        _ => {
                            children.clear();
                            builder.tir.walk_operands(expr, &mut |_p, child, kind| {
                                debug_assert!(
                                    kind == crate::tir::ChildKind::Operand,
                                    "expression insts announce operands only"
                                );
                                children.push(child);
                            });
                            if children.is_empty() {
                                let facts = builder.facts_for(expr);
                                builder.push_step(expr, facts);
                            } else {
                                tasks.push(Task::FinishExpr(expr));
                                for &child in children.iter().rev() {
                                    tasks.push(Task::EnterExpr(child));
                                }
                            }
                        }
                    },
                }
            }
            Task::FinishExpr(expr) => {
                let facts = builder.facts_for(expr);
                builder.push_step(expr, facts);
            }
            Task::EnterStmt(stmt) => {
                builder.tir.walk_operands(stmt, &mut |_p, child, kind| {
                    if kind == crate::tir::ChildKind::Operand {
                        builder.inst_meta[child.index()].parent = Some(stmt.raw());
                    }
                });
                let inst = *builder.tir.inst(stmt);
                match inst.tag {
                    TirTag::VarDecl => {
                        let view = builder.tir.var_decl_view(stmt);
                        tasks.push(Task::FinishStmt(stmt));
                        tasks.push(Task::EnterExpr(view.initializer));
                    }
                    TirTag::Assign => {
                        let view = builder.tir.assign_view(stmt);
                        tasks.push(Task::FinishStmt(stmt));
                        tasks.push(Task::EnterExpr(view.value));
                    }
                    TirTag::CompoundAssign => {
                        let view = builder.tir.compound_assign_view(stmt);
                        tasks.push(Task::FinishStmt(stmt));
                        tasks.push(Task::EnterExpr(view.value));
                    }
                    TirTag::FieldAssign | TirTag::CompoundFieldAssign => {
                        let (target, value) = match inst.tag {
                            TirTag::FieldAssign => {
                                let view = builder.tir.field_assign_view(stmt);
                                (view.target, view.value)
                            }
                            _ => {
                                let view = builder.tir.compound_field_assign_view(stmt);
                                (view.target, view.value)
                            }
                        };
                        tasks.push(Task::FinishStmt(stmt));
                        tasks.push(Task::EnterExpr(value));
                        tasks.push(Task::EnterExpr(target));
                    }
                    TirTag::Match | TirTag::IfStmt => {
                        control::schedule_control(builder, stmt, &mut tasks)
                    }
                    TirTag::WhileLoop | TirTag::ForRange => {
                        tasks.push(Task::LegacyControl(stmt));
                    }
                    TirTag::Destructure => {
                        let view = builder.tir.destructure_view(stmt);
                        tasks.push(Task::FinishStmt(stmt));
                        tasks.push(Task::EnterExpr(view.rhs));
                    }
                    TirTag::Return => match inst.data {
                        TirData::UnOp(value) => {
                            tasks.push(Task::FinishStmt(stmt));
                            tasks.push(Task::EnterExpr(value));
                        }
                        _ => unreachable!("Return carries UnOp"),
                    },
                    TirTag::ReturnVoid | TirTag::Break | TirTag::Continue | TirTag::Unreachable => {
                        tasks.push(Task::FinishStmt(stmt));
                    }
                    TirTag::ExprStmt => match inst.data {
                        TirData::UnOp(expr) => {
                            tasks.push(Task::FinishStmt(stmt));
                            tasks.push(Task::EnterExpr(expr));
                        }
                        _ => unreachable!("ExprStmt carries UnOp"),
                    },
                    _ => {
                        tasks.push(Task::FinishStmt(stmt));
                        tasks.push(Task::EnterExpr(stmt));
                    }
                }
            }
            Task::FinishStmt(stmt) => {
                let inst = *builder.tir.inst(stmt);
                match inst.tag {
                    TirTag::VarDecl => {
                        let view = builder.tir.var_decl_view(stmt);
                        let place = builder.push_place(Place::Local {
                            decl: stmt,
                            name: view.name,
                            ty: inst.ty,
                            mutable: view.mutable,
                        });
                        builder.inst_meta[stmt.index()].local = Some(place);
                        builder.record_write(place, stmt);
                        builder.push_step(
                            stmt,
                            StepFacts {
                                write: Some(PlaceWrite {
                                    place,
                                    kind: WriteKind::Init,
                                }),
                                ..StepFacts::default()
                            },
                        );
                        builder.bind(view.name, place);
                    }
                    TirTag::Assign => {
                        let view = builder.tir.assign_view(stmt);
                        let place = builder
                            .resolve(view.name)
                            .expect("assign to unresolved binding (post-sema TIR)");
                        builder.record_write(place, stmt);
                        builder.push_step(
                            stmt,
                            StepFacts {
                                write: Some(PlaceWrite {
                                    place,
                                    kind: WriteKind::Replace,
                                }),
                                ..StepFacts::default()
                            },
                        );
                    }
                    TirTag::CompoundAssign => {
                        let view = builder.tir.compound_assign_view(stmt);
                        let place = builder
                            .resolve(view.name)
                            .expect("compound-assign to unresolved binding (post-sema TIR)");
                        builder.push_step(
                            stmt,
                            StepFacts {
                                read: Some(place),
                                write: Some(PlaceWrite {
                                    place,
                                    kind: WriteKind::Mutate,
                                }),
                                ..StepFacts::default()
                            },
                        );
                    }
                    TirTag::FieldAssign | TirTag::CompoundFieldAssign => {
                        let (target, is_compound) = match inst.tag {
                            TirTag::FieldAssign => {
                                (builder.tir.field_assign_view(stmt).target, false)
                            }
                            _ => (builder.tir.compound_field_assign_view(stmt).target, true),
                        };
                        let (base, field) = builder
                            .resolve_field_target(target)
                            .expect("field-assign target is not a place projection");
                        builder.push_step(
                            stmt,
                            StepFacts {
                                read: is_compound.then_some(base),
                                write: Some(PlaceWrite {
                                    place: base,
                                    kind: WriteKind::FieldReplace { field },
                                }),
                                ..StepFacts::default()
                            },
                        );
                    }
                    TirTag::Destructure => {
                        let view = builder.tir.destructure_view(stmt);
                        let refs = builder.tir.destructure_bound_owner_refs(stmt);
                        let names: Vec<_> = view.fields.iter().filter_map(|f| f.bind).collect();
                        debug_assert_eq!(refs.len(), names.len(), "token/binding mismatch");
                        builder.register_tokens(stmt, &refs, &names);
                        let places = builder.token_places_of(stmt).to_vec();
                        for place in places {
                            if let Place::Token { name, .. } = builder.places[place.0 as usize] {
                                builder.bind(name, place);
                            }
                        }
                        builder.push_step(stmt, StepFacts::default());
                    }
                    TirTag::Return => match inst.data {
                        TirData::UnOp(value) => {
                            builder.set_term(builder.cur, Terminator::Return { value }, Some(stmt));
                        }
                        _ => unreachable!("Return carries UnOp"),
                    },
                    TirTag::ReturnVoid => {
                        builder.set_term(builder.cur, Terminator::ReturnVoid, Some(stmt));
                    }
                    TirTag::Break | TirTag::Continue => {
                        let term = match (inst.tag, builder.loops.last()) {
                            (TirTag::Break, Some(frame)) => {
                                Terminator::Break { target: frame.exit }
                            }
                            (TirTag::Continue, Some(frame)) => Terminator::Continue {
                                target: frame.header,
                            },
                            _ => Terminator::Unreachable,
                        };
                        builder.set_term(builder.cur, term, Some(stmt));
                    }
                    TirTag::Unreachable => {
                        builder.set_term(builder.cur, Terminator::Unreachable, Some(stmt));
                    }
                    TirTag::ExprStmt => {
                        builder.push_step(stmt, StepFacts::default());
                    }
                    _ => {
                        if builder.inst_meta[stmt.index()].step.is_none() {
                            builder.push_step(stmt, StepFacts::default());
                        }
                    }
                }
            }
            Task::AfterBoolLhs {
                expr,
                lhs,
                rhs,
                is_and,
            } => {
                let rhs_block = builder.new_block(false);
                let join = builder.new_block(false);
                let (then_, else_) = if is_and {
                    (rhs_block, join)
                } else {
                    (join, rhs_block)
                };
                builder.set_term(
                    builder.cur,
                    Terminator::Branch {
                        cond: lhs,
                        then_,
                        else_,
                    },
                    None,
                );
                builder.cur = rhs_block;
                tasks.push(Task::AfterBoolRhs { expr, join });
                tasks.push(Task::EnterExpr(rhs));
            }
            Task::AfterBoolRhs { expr, join } => {
                builder.close_region(builder.cur, join);
                builder.cur = join;
                builder.push_step(expr, StepFacts::default());
            }
            Task::AfterNeverArgs(expr) => {
                let next = builder.new_block(true);
                builder.set_term(
                    builder.cur,
                    Terminator::Call { call: expr, next },
                    Some(expr),
                );
                builder.cur = next;
            }
            Task::Control(control) => control::resume_control(builder, control, &mut tasks),
            Task::LegacyControl(stmt) => match builder.tir.inst(stmt).tag {
                TirTag::WhileLoop => builder.build_while(stmt),
                TirTag::ForRange => builder.build_for(stmt),
                _ => unreachable!("legacy control dispatch for non-control instruction"),
            },
        }
    }
}
