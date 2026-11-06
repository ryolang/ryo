use super::{
    CHAIN_RECURSION_LIMIT, FuncCtx, Scope, Sema, analyze_binop_link, analyze_expr_with,
    analyze_unary_link, const_eval_int, is_chain_binop_tag,
};
use ryo_core::spine::ChainSpine;
use ryo_core::tir::TirRef;
use ryo_core::uir::{InstData, InstRef, InstTag};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub(super) struct SemaExprContext {
    pub(super) depth: u32,
    pub(super) limit: u32,
}

impl SemaExprContext {
    pub(super) fn root() -> Self {
        #[cfg(test)]
        if let Some(limit) = TEST_EXPR_LIMIT.with(|limit| limit.get()) {
            return Self { depth: 0, limit };
        }
        Self {
            depth: 0,
            limit: CHAIN_RECURSION_LIMIT,
        }
    }

    pub(super) fn child(self) -> Self {
        Self {
            depth: self.depth.saturating_add(1),
            limit: self.limit,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NeverMode {
    Reject,
    Allow,
}

#[cfg(test)]
std::thread_local! {
    static TEST_EXPR_LIMIT: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn with_expr_limit<T>(limit: u32, f: impl FnOnce() -> T) -> T {
    let previous = TEST_EXPR_LIMIT.with(|current| current.replace(Some(limit)));
    let result = f();
    TEST_EXPR_LIMIT.with(|current| current.set(previous));
    result
}

#[cold]
#[inline(never)]
pub(super) fn analyze_expr_machine(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    root: InstRef,
    context: SemaExprContext,
) -> TirRef {
    enum Task {
        Enter(InstRef),
        AfterLhs(InstRef),
        Exit(InstRef),
    }

    fn is_chain(sema: &Sema<'_>, node: InstRef) -> bool {
        matches!(sema.uir.inst(node).tag, InstTag::Neg | InstTag::Not)
            || is_chain_binop_tag(sema.uir.inst(node).tag)
    }

    let mut tasks = ChainSpine::new();
    let mut early_errors = Vec::new();
    let mut consts = HashMap::new();
    let mut analyzed = HashMap::new();
    tasks.push(Task::Enter(root));
    while let Some(task) = tasks.pop() {
        match task {
            Task::Enter(node) => {
                if fcx.inst_map[node.index()].is_some() {
                    let value = analyze_expr_with(
                        sema,
                        fcx,
                        scope,
                        node,
                        super::NeverMode::Reject,
                        context.child(),
                    );
                    analyzed.insert(node, value);
                    consts.insert(node, const_eval_int(sema.uir, node));
                    continue;
                }
                let inst = sema.uir.inst(node);
                match inst.data {
                    InstData::BinOp { lhs, .. } if is_chain_binop_tag(inst.tag) => {
                        tasks.push(Task::Exit(node));
                        tasks.push(Task::AfterLhs(node));
                        if is_chain(sema, lhs) && fcx.inst_map[lhs.index()].is_none() {
                            tasks.push(Task::Enter(lhs));
                        } else {
                            analyzed.insert(
                                lhs,
                                analyze_expr_with(
                                    sema,
                                    fcx,
                                    scope,
                                    lhs,
                                    super::NeverMode::Reject,
                                    context.child(),
                                ),
                            );
                            consts.insert(lhs, const_eval_int(sema.uir, lhs));
                        }
                    }
                    InstData::UnOp(operand) if matches!(inst.tag, InstTag::Neg | InstTag::Not) => {
                        tasks.push(Task::Exit(node));
                        if is_chain(sema, operand) && fcx.inst_map[operand.index()].is_none() {
                            tasks.push(Task::Enter(operand));
                        } else {
                            analyzed.insert(
                                operand,
                                analyze_expr_with(
                                    sema,
                                    fcx,
                                    scope,
                                    operand,
                                    super::NeverMode::Reject,
                                    context.child(),
                                ),
                            );
                            consts.insert(operand, const_eval_int(sema.uir, operand));
                        }
                    }
                    _ => {
                        let value = analyze_expr_with(
                            sema,
                            fcx,
                            scope,
                            node,
                            super::NeverMode::Reject,
                            context.child(),
                        );
                        analyzed.insert(node, value);
                        consts.insert(node, const_eval_int(sema.uir, node));
                    }
                }
            }
            Task::AfterLhs(node) => {
                let InstData::BinOp { rhs, .. } = sema.uir.inst(node).data else {
                    unreachable!("chain tag must carry InstData::BinOp");
                };
                if is_chain(sema, rhs) && fcx.inst_map[rhs.index()].is_none() {
                    tasks.push(Task::Enter(rhs));
                } else {
                    analyzed.insert(
                        rhs,
                        analyze_expr_with(
                            sema,
                            fcx,
                            scope,
                            rhs,
                            super::NeverMode::Reject,
                            context.child(),
                        ),
                    );
                    consts.insert(rhs, const_eval_int(sema.uir, rhs));
                }
            }
            Task::Exit(node) => {
                let inst = sema.uir.inst(node);
                match inst.data {
                    InstData::BinOp { lhs, rhs } if is_chain_binop_tag(inst.tag) => {
                        let acc = analyzed.get(&lhs).copied().expect("lhs analyzed first");
                        let r2 = analyzed.get(&rhs).copied().expect("rhs analyzed second");
                        let (next, node_const, memo) = analyze_binop_link(
                            sema,
                            fcx,
                            inst.tag,
                            acc,
                            consts[&lhs],
                            r2,
                            consts[&rhs],
                            sema.uir.span(node),
                        );
                        analyzed.insert(node, next);
                        consts.insert(node, node_const);
                        fcx.inst_map[node.index()] = Some(next);
                        if !memo {
                            early_errors.push(node);
                        }
                    }
                    InstData::UnOp(operand) if matches!(inst.tag, InstTag::Neg | InstTag::Not) => {
                        let (next, node_const) = analyze_unary_link(
                            sema,
                            fcx,
                            scope,
                            inst.tag,
                            operand,
                            consts[&operand],
                            sema.uir.span(node),
                        );
                        analyzed.insert(node, next);
                        consts.insert(node, node_const);
                        fcx.inst_map[node.index()] = Some(next);
                    }
                    _ => unreachable!("only unary and binary nodes have exit tasks"),
                }
            }
        }
    }
    let result = analyzed[&root];
    for node in early_errors {
        fcx.inst_map[node.index()] = None;
    }
    result
}
