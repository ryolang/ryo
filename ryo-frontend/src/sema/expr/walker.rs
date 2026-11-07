use super::super::ScopeParent;
use super::super::call::{finish_call, prepare_call};
use super::{
    CHAIN_RECURSION_LIMIT, ConstInt, FuncCtx, Scope, Sema, analyze_binop_link,
    analyze_expr_allow_never_with, analyze_expr_with, analyze_unary_link, const_eval_int,
    is_chain_binop_tag, operators, reject_never,
};
use ryo_core::spine::ChainSpine;
use ryo_core::tir::TirRef;
use ryo_core::types::{TypeId, TypeKind, VariantKind, ViewKind};
use ryo_core::uir::{InstData, InstRef, InstTag};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub(crate) struct SemaExprContext {
    pub(crate) depth: u32,
    pub(crate) limit: u32,
}

impl SemaExprContext {
    pub(crate) fn root() -> Self {
        #[cfg(test)]
        if let Some(limit) = TEST_EXPR_LIMIT.with(|limit| limit.get()) {
            return Self { depth: 0, limit };
        }
        Self {
            depth: 0,
            limit: CHAIN_RECURSION_LIMIT,
        }
    }

    pub(crate) fn child(self) -> Self {
        Self {
            depth: self.depth.saturating_add(1),
            limit: self.limit,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NeverMode {
    Reject,
    Allow,
}

#[cfg(test)]
std::thread_local! {
    static TEST_EXPR_LIMIT: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    static TEST_MACHINE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_machine_call_count() {
    TEST_MACHINE_CALLS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn machine_call_count() -> usize {
    TEST_MACHINE_CALLS.with(|count| count.get())
}

#[cfg(test)]
pub(crate) fn with_expr_limit<T>(limit: u32, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<u32>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_EXPR_LIMIT.with(|current| current.set(self.0));
        }
    }

    let previous = TEST_EXPR_LIMIT.with(|current| current.replace(Some(limit)));
    let _reset = Reset(previous);
    f()
}

pub(crate) fn is_generic_expr_tag(tag: InstTag) -> bool {
    matches!(
        tag,
        InstTag::Neg
            | InstTag::Not
            | InstTag::Call
            | InstTag::MethodCall
            | InstTag::Slice
            | InstTag::Index
            | InstTag::Borrow
            | InstTag::StructLit
            | InstTag::EnumLit
            | InstTag::ErrorConstruct
            | InstTag::FieldAccess
            | InstTag::Match
            | InstTag::Catch
            | InstTag::Try
    ) || is_chain_binop_tag(tag)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ScopeId(usize);

struct ScopeArena<'a> {
    root: &'a Scope<'a>,
    owned: Vec<Option<Scope<'a>>>,
}

impl<'a> ScopeArena<'a> {
    fn new(root: &'a Scope<'a>) -> Self {
        Self {
            root,
            owned: Vec::new(),
        }
    }

    fn root_id(&self) -> ScopeId {
        ScopeId(0)
    }

    fn get(&self, id: ScopeId) -> &Scope<'a> {
        if id.0 == 0 {
            return self.root;
        }
        self.owned[id.0 - 1].as_ref().expect("active scope")
    }

    fn get_mut(&mut self, id: ScopeId) -> &mut Scope<'a> {
        id.0.checked_sub(1)
            .and_then(|index| self.owned.get_mut(index))
            .and_then(Option::as_mut)
            .expect("owned scope")
    }

    fn take_parent(&mut self, id: ScopeId) -> ScopeParent<'a> {
        if id.0 == 0 {
            return ScopeParent::Borrowed(self.root);
        }
        ScopeParent::Owned(Box::new(
            self.owned[id.0 - 1].take().expect("active parent scope"),
        ))
    }

    fn restore(&mut self, id: ScopeId, parent: ScopeParent<'a>) {
        if let ScopeParent::Owned(scope) = parent {
            self.owned[id.0 - 1] = Some(*scope);
        }
    }

    fn push(&mut self, scope: Scope<'a>) -> ScopeId {
        self.owned.push(Some(scope));
        ScopeId(self.owned.len())
    }

    fn take(&mut self, id: ScopeId) -> Scope<'a> {
        self.owned[id.0 - 1].take().expect("unfinished arm scope")
    }
}

mod match_tasks;
mod struct_tasks;
mod variant_tasks;
use match_tasks::{BlockState, MatchState, StmtState, match_scrutinee};
use struct_tasks::StructState;
use variant_tasks::VariantState;

enum Task {
    Enter(InstRef, ScopeId, NeverMode),
    CallArg(InstRef, usize, ScopeId),
    StructField(InstRef, usize, ScopeId),
    AfterStructField(InstRef, usize, ScopeId),
    VariantField(InstRef, usize, ScopeId),
    AfterVariantField(InstRef, usize, ScopeId),
    AfterBase(InstRef, ScopeId),
    AfterLhs(InstRef, ScopeId),
    AfterScrutinee(InstRef, ScopeId),
    PrepareArm(InstRef, usize, ScopeId),
    BlockEnter(BlockState),
    BlockNext(BlockState),
    StmtEnter(StmtState),
    StmtValue(StmtState),
    StmtTarget(StmtState),
    StmtBody(StmtState),
    ElifCond(StmtState),
    ElifBody(StmtState),
    IfElse(StmtState),
    ForStart(StmtState),
    FinishArm(InstRef, usize, ScopeId),
    Exit(InstRef, ScopeId, NeverMode),
}

fn is_chain(sema: &Sema<'_>, node: InstRef) -> bool {
    is_generic_expr_tag(sema.uir.inst(node).tag)
}

pub(crate) fn analyze_expr_machine(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    root: InstRef,
    context: SemaExprContext,
) -> TirRef {
    analyze_expr_machine_inner(sema, fcx, scope, root, None, context)
}

pub(crate) fn analyze_struct_lit_machine(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    root: InstRef,
    target: TypeId,
    context: SemaExprContext,
) -> TirRef {
    analyze_expr_machine_inner(sema, fcx, scope, root, Some(target), context)
}

#[cold]
#[inline(never)]
#[allow(clippy::too_many_lines)]
fn analyze_expr_machine_inner(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    root: InstRef,
    root_target: Option<TypeId>,
    context: SemaExprContext,
) -> TirRef {
    #[cfg(test)]
    TEST_MACHINE_CALLS.with(|count| count.set(count.get() + 1));
    let mut match_state = MatchState::new(scope);
    let root_scope = match_state.scopes.root_id();
    let mut tasks = ChainSpine::new();
    let mut block_results = ChainSpine::new();
    let mut early_errors = Vec::new();
    let mut consts = HashMap::new();
    let mut analyzed = HashMap::new();
    let mut calls = HashMap::new();
    let mut method_recoveries = HashMap::new();
    let mut struct_state = StructState::new(root, root_target);
    let mut variant_state = VariantState::default();
    tasks.push(Task::Enter(root, root_scope, NeverMode::Reject));
    while let Some(task) = tasks.pop() {
        match task {
            Task::Enter(node, scope_id, mode) => {
                let scope = match_state.scopes.get(scope_id);
                if fcx.inst_map[node.index()].is_some() {
                    let value = analyze_expr_with(sema, fcx, scope, node, mode, context.child());
                    analyzed.insert(node, value);
                    consts.insert(node, const_eval_int(sema.uir, node));
                    continue;
                }
                let inst = sema.uir.inst(node);
                if inst.tag == InstTag::Call {
                    let view = sema.uir.call_view(node);
                    let plan = prepare_call(sema, &view, sema.uir.span(node));
                    tasks.push(Task::Exit(node, scope_id, mode));
                    for index in (0..view.args.len()).rev() {
                        tasks.push(Task::CallArg(node, index, scope_id));
                    }
                    calls.insert(node, plan);
                    continue;
                }
                if inst.tag == InstTag::MethodCall {
                    let view = sema.uir.method_call_view(node);
                    if let Some(recovery) = super::methods::lowercase_enum_method_receiver_recovery(
                        sema,
                        fcx,
                        scope,
                        view.receiver,
                        view.name,
                        sema.uir.span(node),
                    ) {
                        tasks.push(Task::Exit(node, scope_id, mode));
                        for arg in view.args.iter().rev() {
                            tasks.push(Task::Enter(arg, scope_id, NeverMode::Reject));
                        }
                        method_recoveries.insert(node, recovery);
                        continue;
                    }
                }
                if matches!(inst.tag, InstTag::Match | InstTag::Catch) {
                    tasks.push(Task::Exit(node, scope_id, mode));
                    tasks.push(Task::AfterScrutinee(node, scope_id));
                    let scrutinee = match_scrutinee(sema, node);
                    let entered = match sema.uir.inst(scrutinee).data {
                        InstData::Borrow(inner) => inner,
                        _ => scrutinee,
                    };
                    tasks.push(Task::Enter(entered, scope_id, NeverMode::Reject));
                    continue;
                }
                if matches!(inst.tag, InstTag::Slice | InstTag::Index) {
                    let base = match inst.data {
                        InstData::Slice { base, .. } | InstData::BinOp { lhs: base, .. } => base,
                        _ => unreachable!(),
                    };
                    tasks.push(Task::Exit(node, scope_id, mode));
                    tasks.push(Task::AfterBase(node, scope_id));
                    tasks.push(Task::Enter(base, scope_id, NeverMode::Reject));
                    continue;
                }
                if struct_state.enter(&mut tasks, sema, node, scope_id, mode) {
                    continue;
                }
                if !composite_has_leaf_recovery(sema, scope, node)
                    && variant_state.enter(&mut tasks, sema, node, scope_id, mode)
                {
                    continue;
                }
                if is_composite_tag(inst.tag) && !composite_has_leaf_recovery(sema, scope, node) {
                    let children = composite_children(sema, node);
                    tasks.push(Task::Exit(node, scope_id, mode));
                    for child in children.into_iter().rev() {
                        tasks.push(Task::Enter(child, scope_id, NeverMode::Reject));
                    }
                    continue;
                }
                match inst.data {
                    InstData::BinOp { lhs, .. } if is_chain_binop_tag(inst.tag) => {
                        tasks.push(Task::Exit(node, scope_id, mode));
                        tasks.push(Task::AfterLhs(node, scope_id));
                        if is_chain(sema, lhs) && fcx.inst_map[lhs.index()].is_none() {
                            tasks.push(Task::Enter(lhs, scope_id, NeverMode::Reject));
                        } else {
                            analyzed.insert(
                                lhs,
                                analyze_expr_with(
                                    sema,
                                    fcx,
                                    scope,
                                    lhs,
                                    NeverMode::Reject,
                                    context.child(),
                                ),
                            );
                            consts.insert(lhs, const_eval_int(sema.uir, lhs));
                        }
                    }
                    InstData::UnOp(operand) if matches!(inst.tag, InstTag::Neg | InstTag::Not) => {
                        tasks.push(Task::Exit(node, scope_id, mode));
                        if is_chain(sema, operand) && fcx.inst_map[operand.index()].is_none() {
                            tasks.push(Task::Enter(operand, scope_id, NeverMode::Reject));
                        } else {
                            analyzed.insert(
                                operand,
                                analyze_expr_with(
                                    sema,
                                    fcx,
                                    scope,
                                    operand,
                                    NeverMode::Reject,
                                    context.child(),
                                ),
                            );
                            consts.insert(operand, const_eval_int(sema.uir, operand));
                        }
                    }
                    _ if is_composite_tag(inst.tag) => {
                        let value = analyze_expr_allow_never_with(
                            sema,
                            fcx,
                            scope,
                            node,
                            SemaExprContext {
                                depth: 0,
                                limit: u32::MAX,
                            },
                        );
                        analyzed.insert(node, value);
                        consts.insert(node, const_eval_int(sema.uir, node));
                    }
                    _ => {
                        let value =
                            analyze_expr_with(sema, fcx, scope, node, mode, context.child());
                        analyzed.insert(node, value);
                        consts.insert(node, const_eval_int(sema.uir, node));
                    }
                }
            }
            Task::StructField(node, index, scope_id) => {
                struct_state.before_field(&mut tasks, sema, node, index, scope_id);
            }
            Task::AfterStructField(node, index, _scope_id) => {
                struct_state.after_field(sema, fcx, &analyzed, node, index);
            }
            Task::VariantField(node, index, scope_id) => {
                variant_state.before_field(&mut tasks, sema, node, index, scope_id);
            }
            Task::AfterVariantField(node, index, _scope_id) => {
                variant_state.after_field(sema, fcx, &analyzed, node, index);
            }
            Task::CallArg(node, index, scope_id) => {
                let scope = match_state.scopes.get(scope_id);
                let view = sema.uir.call_view(node);
                let arg = view.args.at(index);
                let target = calls[&node]
                    .expected()
                    .and_then(|types| types.get(index))
                    .copied();
                if let Some(target) = target
                    && sema.uir.inst(arg).tag == InstTag::StructLit
                {
                    struct_state.expect(arg, target);
                    tasks.push(Task::Enter(arg, scope_id, NeverMode::Reject));
                } else if is_chain(sema, arg) && fcx.inst_map[arg.index()].is_none() {
                    tasks.push(Task::Enter(arg, scope_id, NeverMode::Reject));
                } else {
                    let value = analyze_expr_with(
                        sema,
                        fcx,
                        scope,
                        arg,
                        NeverMode::Reject,
                        context.child(),
                    );
                    analyzed.insert(arg, value);
                    consts.insert(arg, const_eval_int(sema.uir, arg));
                }
            }
            Task::AfterBase(node, scope_id) => {
                let inst = sema.uir.inst(node);
                let base = match inst.data {
                    InstData::Slice { base, .. } | InstData::BinOp { lhs: base, .. } => base,
                    _ => unreachable!(),
                };
                let base_ty = fcx.builder.ty_of(analyzed[&base]);
                let base_kind = sema.pool.kind(base_ty);
                match inst.data {
                    InstData::Slice { start, end, .. }
                        if matches!(
                            base_kind,
                            TypeKind::Str
                                | TypeKind::Bytes
                                | TypeKind::View(ViewKind::Str | ViewKind::Bytes)
                        ) =>
                    {
                        for child in [end, start].into_iter().flatten() {
                            tasks.push(Task::Enter(child, scope_id, NeverMode::Reject));
                        }
                    }
                    InstData::BinOp { rhs, .. }
                        if matches!(
                            base_kind,
                            TypeKind::Bytes | TypeKind::View(ViewKind::Bytes)
                        ) =>
                    {
                        tasks.push(Task::Enter(rhs, scope_id, NeverMode::Reject));
                    }
                    _ => {}
                }
            }
            Task::AfterLhs(node, scope_id) => {
                let scope = match_state.scopes.get(scope_id);
                let InstData::BinOp { rhs, .. } = sema.uir.inst(node).data else {
                    unreachable!("chain tag must carry InstData::BinOp");
                };
                if is_chain(sema, rhs) && fcx.inst_map[rhs.index()].is_none() {
                    tasks.push(Task::Enter(rhs, scope_id, NeverMode::Reject));
                } else {
                    analyzed.insert(
                        rhs,
                        analyze_expr_with(
                            sema,
                            fcx,
                            scope,
                            rhs,
                            NeverMode::Reject,
                            context.child(),
                        ),
                    );
                    consts.insert(rhs, const_eval_int(sema.uir, rhs));
                }
            }
            Task::AfterScrutinee(node, scope_id) => {
                match_state.after_scrutinee(&mut tasks, sema, fcx, node, scope_id);
            }
            Task::PrepareArm(node, index, scope_id) => {
                match_state.prepare_arm(&mut tasks, sema, fcx, node, index, scope_id);
            }
            Task::BlockEnter(block) | Task::BlockNext(block) => {
                match_state.advance_block(&mut tasks, &mut block_results, sema, block);
            }
            Task::StmtEnter(state) => {
                match_state.block_stmt(&mut tasks, sema, fcx, &mut struct_state, state, context);
            }
            Task::StmtValue(state) => {
                match_state.finish_stmt(&mut tasks, sema, fcx, &analyzed, state, context);
            }
            Task::StmtTarget(state) => {
                match_state.finish_target(
                    &mut tasks,
                    sema,
                    fcx,
                    &mut struct_state,
                    &analyzed,
                    state,
                );
            }
            Task::ForStart(state) => {
                match_state.for_start(&mut tasks, sema, &analyzed, state);
            }
            Task::StmtBody(state) => {
                match_state.finish_body(&mut tasks, sema, fcx, &mut block_results, state);
            }
            Task::ElifCond(state) => {
                match_state.finish_elif_condition(&mut tasks, sema, fcx, &analyzed, state);
            }
            Task::ElifBody(state) => {
                match_state.finish_elif_body(&mut tasks, sema, fcx, &mut block_results, state);
            }
            Task::IfElse(state) => {
                match_state.finish_else(&mut tasks, sema, fcx, &mut block_results, state);
            }
            Task::FinishArm(node, index, parent_scope_id) => {
                match_state.finish_arm(
                    sema,
                    fcx,
                    &analyzed,
                    &mut block_results,
                    node,
                    index,
                    parent_scope_id,
                );
            }
            Task::Exit(node, scope_id, mode) => {
                let scope = match_state.scopes.get(scope_id);
                let inst = sema.uir.inst(node);
                if let Some(recovery) = method_recoveries.remove(&node) {
                    analyzed.insert(node, recovery);
                    consts.insert(node, ConstInt::NotConst);
                    fcx.inst_map[node.index()] = Some(recovery);
                    continue;
                }
                if matches!(inst.tag, InstTag::Match | InstTag::Catch) {
                    let next = match_state.finish_match(sema, fcx, node);
                    let next = if node == root || mode == NeverMode::Allow {
                        next
                    } else {
                        reject_never(sema, fcx, node, next)
                    };
                    analyzed.insert(node, next);
                    consts.insert(node, ConstInt::NotConst);
                    fcx.inst_map[node.index()] = Some(next);
                    continue;
                }
                if inst.tag == InstTag::Call {
                    let view = sema.uir.call_view(node);
                    let arg_tirs = view
                        .args
                        .iter()
                        .map(|arg| analyzed[&arg])
                        .collect::<Vec<_>>();
                    let plan = calls.remove(&node).expect("call prepared first");
                    let next =
                        finish_call(sema, fcx, scope, &view, sema.uir.span(node), plan, arg_tirs);
                    let next = if node == root || mode == NeverMode::Allow {
                        next
                    } else {
                        reject_never(sema, fcx, node, next)
                    };
                    analyzed.insert(node, next);
                    consts.insert(node, ConstInt::NotConst);
                    fcx.inst_map[node.index()] = Some(next);
                    continue;
                }
                match inst.data {
                    InstData::BinOp { lhs, rhs } if is_chain_binop_tag(inst.tag) => {
                        let (acc, r2) = (analyzed[&lhs], analyzed[&rhs]);
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
                            operators::UnaryOp {
                                tag: inst.tag,
                                operand,
                                operand_const: consts[&operand],
                                span: sema.uir.span(node),
                                context,
                            },
                        );
                        analyzed.insert(node, next);
                        consts.insert(node, node_const);
                        fcx.inst_map[node.index()] = Some(next);
                    }
                    _ if is_composite_tag(inst.tag) => {
                        let leaf_context = SemaExprContext {
                            depth: 0,
                            limit: u32::MAX,
                        };
                        let next = if inst.tag == InstTag::StructLit {
                            struct_state.finish(sema, fcx, scope, node, leaf_context)
                        } else if matches!(inst.tag, InstTag::EnumLit | InstTag::ErrorConstruct) {
                            variant_state.finish(sema, fcx, scope, node, leaf_context)
                        } else {
                            analyze_expr_allow_never_with(sema, fcx, scope, node, leaf_context)
                        };
                        let next = if node == root || mode == NeverMode::Allow {
                            next
                        } else {
                            reject_never(sema, fcx, node, next)
                        };
                        analyzed.insert(node, next);
                        consts.insert(node, const_eval_int(sema.uir, node));
                        fcx.inst_map[node.index()] = Some(next);
                    }
                    _ => unreachable!("only scheduled nodes have exit tasks"),
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

fn is_composite_tag(tag: InstTag) -> bool {
    matches!(
        tag,
        InstTag::MethodCall
            | InstTag::Slice
            | InstTag::Index
            | InstTag::Borrow
            | InstTag::StructLit
            | InstTag::EnumLit
            | InstTag::ErrorConstruct
            | InstTag::FieldAccess
            | InstTag::Try
    )
}

fn composite_children(sema: &Sema<'_>, node: InstRef) -> Vec<InstRef> {
    let inst = sema.uir.inst(node);
    match inst.tag {
        InstTag::MethodCall => {
            let view = sema.uir.method_call_view(node);
            std::iter::once(view.receiver)
                .chain(view.args.iter())
                .collect()
        }
        InstTag::Slice => {
            let InstData::Slice { base, start, end } = inst.data else {
                unreachable!()
            };
            std::iter::once(base).chain(start).chain(end).collect()
        }
        InstTag::Index => {
            let InstData::BinOp { lhs, rhs } = inst.data else {
                unreachable!()
            };
            vec![lhs, rhs]
        }
        InstTag::Borrow => {
            let InstData::Borrow(inner) = inst.data else {
                unreachable!()
            };
            vec![inner]
        }
        InstTag::StructLit => sema
            .uir
            .struct_lit_view(node)
            .fields()
            .map(|(_, value)| value)
            .collect(),
        InstTag::EnumLit => sema
            .uir
            .enum_lit_view(node)
            .fields()
            .map(|(_, value)| value)
            .collect(),
        InstTag::ErrorConstruct => sema
            .uir
            .error_construct_view(node)
            .3
            .fields()
            .map(|(_, value)| value)
            .collect(),
        InstTag::FieldAccess => {
            let InstData::FieldAccess { object, .. } = inst.data else {
                unreachable!()
            };
            vec![object]
        }
        InstTag::Try => {
            let InstData::UnOp(operand) = inst.data else {
                unreachable!()
            };
            vec![operand]
        }
        _ => unreachable!("not a composite expression"),
    }
}

fn field_access_has_type_recovery(sema: &Sema<'_>, scope: &Scope, node: InstRef) -> bool {
    let InstData::FieldAccess { object, .. } = sema.uir.inst(node).data else {
        return false;
    };
    let InstData::Var(name) = sema.uir.inst(object).data else {
        return false;
    };
    scope.lookup(name).is_none()
        && (sema.enum_types.contains_key(&name) || sema.struct_types.contains_key(&name))
}

fn composite_has_leaf_recovery(sema: &Sema<'_>, scope: &Scope, node: InstRef) -> bool {
    let tag = sema.uir.inst(node).tag;
    if tag == InstTag::FieldAccess && field_access_has_type_recovery(sema, scope, node) {
        return true;
    }
    if tag == InstTag::StructLit {
        let view = sema.uir.struct_lit_view(node);
        if let Some(name) = view.name {
            return sema
                .struct_types
                .get(&name)
                .is_none_or(|&sty| sema.pool.is_error(sty));
        }
    }
    if tag == InstTag::ErrorConstruct {
        let (name, family_root, family_member, view) = sema.uir.error_construct_view(node);
        if family_root.is_some()
            && family_member.is_some()
            && sema.pool.is_error(view.ty)
            && !sema.error_types.contains_key(&name)
        {
            return true;
        }
        if sema.pool.kind(view.ty) == TypeKind::DeclaredError {
            let kind = sema
                .pool
                .enum_view(view.ty)
                .variants()
                .next()
                .expect("declared errors have one variant")
                .kind;
            return kind == VariantKind::Unit
                && (view.positional || view.fields().next().is_some());
        }
    }
    if tag != InstTag::EnumLit {
        return false;
    }
    let view = sema.uir.enum_lit_view(node);
    if sema.pool.is_error(view.ty) {
        return true;
    }
    let Some(variant) = sema
        .pool
        .enum_view(view.ty)
        .variants()
        .nth(view.variant_index as usize)
    else {
        return true;
    };
    view.positional && matches!(variant.kind, VariantKind::Named | VariantKind::Unit)
}
