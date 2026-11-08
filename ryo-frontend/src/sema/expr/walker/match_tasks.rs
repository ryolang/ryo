use super::super::super::check_reserved_name;
use super::super::super::match_::{
    MatchArmPreparation, MatchPlan, TypedArm, TypedArmBody, analyze_scrutinee, finish_match_arm,
    finish_typed_match, prepare_match, prepare_match_arm,
};
use super::super::super::stmt::{
    analyze_stmt_with_context, check_condition_bool, check_field_assignment_root, check_range_args,
    finish_for_range, finish_if_stmt, finish_while_loop,
};
use super::{
    FuncCtx, NeverMode, Scope, ScopeArena, ScopeId, Sema, SemaExprContext, StructState, Task,
};
use ryo_core::spine::ChainSpine;
use ryo_core::tir::TirRef;
use ryo_core::types::TypeId;
use ryo_core::uir::{InstData, InstRef, InstTag, UirArmBodyView, UirMatchArmView};
use std::collections::HashMap;

struct PendingArm {
    scope_id: ScopeId,
    owners: Vec<(u32, TypeId)>,
    binding_error: bool,
}

pub(super) struct BlockState {
    statements: Vec<InstRef>,
    next_statement: usize,
    tirs: Vec<TirRef>,
    scope_id: ScopeId,
    parent_scope_id: Option<ScopeId>,
}

impl BlockState {
    pub(super) fn arm(statements: Vec<InstRef>, scope_id: ScopeId) -> Self {
        Self {
            statements,
            next_statement: 0,
            tirs: Vec::new(),
            scope_id,
            parent_scope_id: None,
        }
    }

    fn child(statements: Vec<InstRef>, scope_id: ScopeId, parent_scope_id: ScopeId) -> Self {
        Self {
            statements,
            next_statement: 0,
            tirs: Vec::new(),
            scope_id,
            parent_scope_id: Some(parent_scope_id),
        }
    }
}

pub(super) struct StmtState {
    statement: InstRef,
    scope_id: ScopeId,
    block: BlockState,
    condition: Option<TirRef>,
    value: Option<TirRef>,
    then_tirs: Option<Vec<TirRef>>,
    elif_tirs: Vec<(TirRef, Vec<TirRef>)>,
    next_elif: usize,
}

impl StmtState {
    fn new(statement: InstRef, block: BlockState) -> Self {
        let scope_id = block.scope_id;
        Self {
            statement,
            scope_id,
            block,
            condition: None,
            value: None,
            then_tirs: None,
            elif_tirs: Vec::new(),
            next_elif: 0,
        }
    }
}

pub(super) struct MatchState<'a> {
    pub(super) scopes: ScopeArena<'a>,
    plans: HashMap<InstRef, MatchPlan>,
    typed_arms: HashMap<InstRef, Vec<Option<TypedArm>>>,
    owner_tokens: HashMap<InstRef, Vec<(u32, TypeId)>>,
    arm_preparations: HashMap<(InstRef, usize), PendingArm>,
}

impl<'a> MatchState<'a> {
    pub(super) fn new(scope: &'a Scope<'a>) -> Self {
        Self {
            scopes: ScopeArena::new(scope),
            plans: HashMap::new(),
            typed_arms: HashMap::new(),
            owner_tokens: HashMap::new(),
            arm_preparations: HashMap::new(),
        }
    }

    pub(super) fn after_scrutinee(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        node: InstRef,
        scope_id: ScopeId,
    ) {
        let scope = self.scopes.get(scope_id);
        let catch = sema.uir.inst(node).tag == InstTag::Catch;
        let scrutinee_ref = match_scrutinee(sema, node);
        let (scrutinee, borrowed) =
            analyze_scrutinee(sema, fcx, scope, scrutinee_ref, SemaExprContext::root());
        let plan = prepare_match(sema, fcx, node, catch, scrutinee, borrowed);
        let arm_count = plan.analysis.arms.len();
        self.typed_arms
            .insert(node, (0..arm_count).map(|_| None).collect::<Vec<_>>());
        self.owner_tokens.insert(node, Vec::new());
        self.plans.insert(node, plan);
        for index in (0..arm_count).rev() {
            tasks.push(Task::PrepareArm(node, index, scope_id));
        }
    }

    pub(super) fn prepare_arm(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        node: InstRef,
        index: usize,
        scope_id: ScopeId,
    ) {
        if self.plans[&node].analysis.arms[index].had_error {
            return;
        }
        let parent = self.scopes.take_parent(scope_id);
        let uir_arms = match_arms(sema, node);
        let (expr_body, stmt_bodies) = match uir_arms[index].body {
            UirArmBodyView::Expr(body) => (Some(body), None),
            UirArmBodyView::Stmts(stmts) => (
                None,
                Some(
                    stmts
                        .iter()
                        .map(|raw| InstRef::from_raw(*raw))
                        .collect::<Vec<_>>(),
                ),
            ),
        };
        let direct_catch_body = matches!(
            uir_arms[index].body,
            UirArmBodyView::Expr(body) if sema.uir.is_direct_catch_match(body)
        );
        drop(uir_arms);
        let plan = &self.plans[&node];
        let arm = &plan.analysis.arms[index];
        let Some(preparation) = prepare_match_arm(
            sema,
            fcx,
            parent,
            arm,
            direct_catch_body,
            plan.analysis.scrutinee_borrowed,
            plan.mode,
        ) else {
            return;
        };
        let child_scope = self.scopes.push(preparation.scope);
        self.arm_preparations.insert(
            (node, index),
            PendingArm {
                scope_id: child_scope,
                owners: preparation.owners,
                binding_error: preparation.binding_error,
            },
        );
        tasks.push(Task::FinishArm(node, index, scope_id));
        if let Some(body) = expr_body {
            tasks.push(Task::Enter(body, child_scope, NeverMode::Allow));
        }
        if let Some(stmts) = stmt_bodies {
            tasks.push(Task::BlockEnter(BlockState::arm(stmts, child_scope)));
        }
    }

    pub(super) fn advance_block(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        block_results: &mut ChainSpine<Vec<TirRef>>,
        sema: &mut Sema<'_>,
        mut block: BlockState,
    ) {
        if let Some(&statement) = block.statements.get(block.next_statement) {
            block.next_statement += 1;
            tasks.push(Task::StmtEnter(StmtState::new(statement, block)));
            return;
        }
        if let Some(parent_scope_id) = block.parent_scope_id {
            let mut scope = self.scopes.take(block.scope_id);
            scope.drain_dead_stores(sema);
            let parent = scope.take_parent().expect("child block has a parent");
            self.scopes.restore(parent_scope_id, parent);
        }
        block_results.push(block.tirs);
    }

    fn child_block(
        &mut self,
        parent_scope_id: ScopeId,
        statements: Vec<InstRef>,
        seed: impl FnOnce(&mut Scope),
    ) -> BlockState {
        let parent = self.scopes.take_parent(parent_scope_id);
        let mut scope = Scope {
            parent: Some(parent),
            bindings: HashMap::new(),
        };
        seed(&mut scope);
        let scope_id = self.scopes.push(scope);
        BlockState::child(statements, scope_id, parent_scope_id)
    }

    pub(super) fn block_stmt(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        struct_state: &mut StructState,
        state: StmtState,
        context: SemaExprContext,
    ) {
        let stmt = state.statement;
        let scope_id = state.scope_id;
        let tag = sema.uir.inst(stmt).tag;
        let expression = match tag {
            InstTag::ExprStmt | InstTag::Return => match sema.uir.inst(stmt).data {
                InstData::UnOp(operand) => Some(operand),
                _ => unreachable!(),
            },
            InstTag::VarDecl => Some(sema.uir.var_decl_view(stmt).initializer),
            InstTag::AssignOrDecl => Some(sema.uir.assign_or_decl_view(stmt).value),
            InstTag::CompoundAssign => Some(sema.uir.compound_assign_view(stmt).value),
            InstTag::Destructure => Some(sema.uir.destructure_view(stmt).value),
            InstTag::IfStmt => Some(sema.uir.if_stmt_view(stmt).cond),
            InstTag::WhileLoop => Some(sema.uir.while_loop_view(stmt).cond),
            _ => None,
        };
        if let Some(expression) = expression {
            let mode = if matches!(
                tag,
                InstTag::ExprStmt
                    | InstTag::VarDecl
                    | InstTag::AssignOrDecl
                    | InstTag::CompoundAssign
                    | InstTag::Destructure
            ) {
                NeverMode::Allow
            } else {
                NeverMode::Reject
            };
            if sema.uir.inst(expression).tag == InstTag::StructLit {
                let target = match tag {
                    InstTag::Return => Some(fcx.return_type),
                    InstTag::VarDecl => sema.uir.var_decl_view(stmt).ty,
                    InstTag::AssignOrDecl => {
                        let name = sema.uir.assign_or_decl_view(stmt).name;
                        match self.scopes.get(scope_id).lookup_full(name) {
                            Some((existing, true)) => Some(existing),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(target) = target {
                    struct_state.expect(expression, target);
                }
            }
            tasks.push(Task::StmtValue(state));
            tasks.push(Task::Enter(expression, scope_id, mode));
            return;
        }
        if tag == InstTag::ForRange {
            let start = sema.uir.for_range_view(stmt).start;
            tasks.push(Task::ForStart(state));
            tasks.push(Task::Enter(start, scope_id, NeverMode::Reject));
            return;
        }
        if matches!(tag, InstTag::FieldAssign | InstTag::CompoundFieldAssign) {
            let target = if tag == InstTag::FieldAssign {
                sema.uir.field_assign_view(stmt).target
            } else {
                sema.uir.compound_field_assign_view(stmt).target
            };
            if !check_field_assignment_root(sema, self.scopes.get(scope_id), target) {
                let value = fcx
                    .builder
                    .unreachable(sema.pool.error_type(), sema.uir.span(stmt));
                statement_done(tasks, state, value);
                return;
            }
            tasks.push(Task::StmtTarget(state));
            tasks.push(Task::Enter(target, scope_id, NeverMode::Reject));
            return;
        }
        let value =
            analyze_stmt_with_context(sema, fcx, self.scopes.get_mut(scope_id), stmt, context);
        statement_done(tasks, state, value);
    }

    pub(super) fn finish_stmt(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        mut state: StmtState,
        context: SemaExprContext,
    ) {
        let stmt = state.statement;
        let scope_id = state.scope_id;
        match sema.uir.inst(stmt).tag {
            InstTag::IfStmt => {
                let view = sema.uir.if_stmt_view(stmt);
                let condition = analyzed[&view.cond];
                check_condition_bool(sema, fcx, condition, view.cond);
                state.condition = Some(condition);
                let body = view.then_stmts.iter().collect::<Vec<_>>();
                let block = self.child_block(scope_id, body, |_| {});
                tasks.push(Task::StmtBody(state));
                tasks.push(Task::BlockEnter(block));
            }
            InstTag::WhileLoop => {
                let view = sema.uir.while_loop_view(stmt);
                let condition = analyzed[&view.cond];
                check_condition_bool(sema, fcx, condition, view.cond);
                state.condition = Some(condition);
                let body = view.body.iter().collect::<Vec<_>>();
                fcx.loop_depth += 1;
                let block = self.child_block(scope_id, body, |_| {});
                tasks.push(Task::StmtBody(state));
                tasks.push(Task::BlockEnter(block));
            }
            InstTag::ForRange => {
                let view = sema.uir.for_range_view(stmt);
                let start_tir = state.condition.take().expect("range start analyzed");
                let end_tir = analyzed[&view.end];
                check_range_args(sema, fcx, view.start, start_tir, view.end, end_tir);
                state.condition = Some(start_tir);
                state.value = Some(end_tir);
                let var_name = view.var_name;
                let body = view.body.iter().collect::<Vec<_>>();
                let span = sema.uir.span(stmt);
                let is_reserved = check_reserved_name(
                    sema,
                    var_name,
                    span,
                    "is a reserved builtin and cannot be redefined",
                );
                let int_ty = sema.pool.int();
                let error_ty = sema.pool.error_type();
                fcx.loop_depth += 1;
                let block = self.child_block(scope_id, body, |scope| {
                    if is_reserved {
                        scope.insert_binding(var_name, error_ty, false, false);
                    } else {
                        scope.insert_dead_store_binding(var_name, int_ty, false, false, span);
                    }
                });
                tasks.push(Task::StmtBody(state));
                tasks.push(Task::BlockEnter(block));
            }
            _ => {
                let value = analyze_stmt_with_context(
                    sema,
                    fcx,
                    self.scopes.get_mut(scope_id),
                    stmt,
                    context,
                );
                statement_done(tasks, state, value);
            }
        }
    }

    pub(super) fn finish_target(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &FuncCtx,
        struct_state: &mut StructState,
        analyzed: &HashMap<InstRef, TirRef>,
        state: StmtState,
    ) {
        let (target, value) = match sema.uir.inst(state.statement).tag {
            InstTag::FieldAssign => {
                let view = sema.uir.field_assign_view(state.statement);
                (view.target, view.value)
            }
            InstTag::CompoundFieldAssign => {
                let view = sema.uir.compound_field_assign_view(state.statement);
                (view.target, view.value)
            }
            _ => unreachable!("target continuation for non-field assignment"),
        };
        if sema.uir.inst(state.statement).tag == InstTag::FieldAssign
            && sema.uir.inst(value).tag == InstTag::StructLit
        {
            struct_state.expect(value, fcx.builder.ty_of(analyzed[&target]));
        }
        let scope_id = state.scope_id;
        tasks.push(Task::StmtValue(state));
        tasks.push(Task::Enter(value, scope_id, NeverMode::Allow));
    }

    pub(super) fn for_start(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        analyzed: &HashMap<InstRef, TirRef>,
        mut state: StmtState,
    ) {
        let view = sema.uir.for_range_view(state.statement);
        state.condition = Some(analyzed[&view.start]);
        let end = view.end;
        let scope_id = state.scope_id;
        tasks.push(Task::StmtValue(state));
        tasks.push(Task::Enter(end, scope_id, NeverMode::Reject));
    }

    pub(super) fn finish_body(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        block_results: &mut ChainSpine<Vec<TirRef>>,
        mut state: StmtState,
    ) {
        let body = block_results.pop().expect("statement body analyzed");
        match sema.uir.inst(state.statement).tag {
            InstTag::IfStmt => {
                state.then_tirs = Some(body);
                self.schedule_if_branches(tasks, sema, fcx, state);
            }
            InstTag::WhileLoop => {
                fcx.loop_depth -= 1;
                let value = finish_while_loop(
                    sema,
                    fcx,
                    state.statement,
                    state.condition.take().expect("while condition analyzed"),
                    &body,
                );
                statement_done(tasks, state, value);
            }
            InstTag::ForRange => {
                fcx.loop_depth -= 1;
                let var_name = sema.uir.for_range_view(state.statement).var_name;
                let value = finish_for_range(
                    sema,
                    fcx,
                    state.statement,
                    var_name,
                    state.condition.take().expect("range start analyzed"),
                    state.value.take().expect("range end analyzed"),
                    &body,
                );
                statement_done(tasks, state, value);
            }
            _ => unreachable!("body continuation for statement without a body"),
        }
    }

    fn schedule_if_branches(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        mut state: StmtState,
    ) {
        let mut view = sema.uir.if_stmt_view(state.statement);
        let scope_id = state.scope_id;
        if let Some(elif) = view.elif_branches.nth(state.next_elif) {
            tasks.push(Task::ElifCond(state));
            tasks.push(Task::Enter(elif.cond, scope_id, NeverMode::Reject));
        } else if let Some(else_stmts) = view.else_stmts {
            let body = else_stmts.iter().collect::<Vec<_>>();
            let block = self.child_block(state.scope_id, body, |_| {});
            tasks.push(Task::IfElse(state));
            tasks.push(Task::BlockEnter(block));
        } else {
            let value = finish_if_stmt(
                sema,
                fcx,
                state.statement,
                state.condition.take().expect("if condition analyzed"),
                state.then_tirs.as_deref().expect("if then block analyzed"),
                &state.elif_tirs,
                None,
            );
            statement_done(tasks, state, value);
        }
    }

    pub(super) fn finish_elif_condition(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        mut state: StmtState,
    ) {
        let mut view = sema.uir.if_stmt_view(state.statement);
        let elif = view
            .elif_branches
            .nth(state.next_elif)
            .expect("elif condition scheduled");
        let condition = analyzed[&elif.cond];
        check_condition_bool(sema, fcx, condition, elif.cond);
        state.value = Some(condition);
        let body = elif.body.iter().collect::<Vec<_>>();
        let block = self.child_block(state.scope_id, body, |_| {});
        tasks.push(Task::ElifBody(state));
        tasks.push(Task::BlockEnter(block));
    }

    pub(super) fn finish_elif_body(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        block_results: &mut ChainSpine<Vec<TirRef>>,
        mut state: StmtState,
    ) {
        let body = block_results.pop().expect("elif body analyzed");
        let condition = state.value.take().expect("elif condition analyzed");
        state.elif_tirs.push((condition, body));
        state.next_elif += 1;
        self.schedule_if_branches(tasks, sema, fcx, state);
    }

    pub(super) fn finish_else(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        block_results: &mut ChainSpine<Vec<TirRef>>,
        mut state: StmtState,
    ) {
        let body = block_results.pop().expect("else body analyzed");
        let value = finish_if_stmt(
            sema,
            fcx,
            state.statement,
            state.condition.take().expect("if condition analyzed"),
            state.then_tirs.as_deref().expect("if then block analyzed"),
            &state.elif_tirs,
            Some(&body),
        );
        statement_done(tasks, state, value);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_arm(
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        block_results: &mut ChainSpine<Vec<TirRef>>,
        node: InstRef,
        index: usize,
        parent_scope_id: ScopeId,
    ) {
        let uir_arms = match_arms(sema, node);
        let PendingArm {
            scope_id,
            owners,
            binding_error,
        } = self
            .arm_preparations
            .remove(&(node, index))
            .expect("arm prepared first");
        let body = match uir_arms[index].body {
            UirArmBodyView::Expr(body) => TypedArmBody::Expr(analyzed[&body]),
            UirArmBodyView::Stmts(_) => {
                TypedArmBody::Stmts(block_results.pop().expect("arm statements analyzed"))
            }
        };
        let mut scope = self.scopes.take(scope_id);
        let parent = scope.take_parent().expect("arm has a parent scope");
        let preparation = MatchArmPreparation {
            scope,
            owners,
            binding_error,
        };
        let (typed, owners, binding_error) = {
            let plan = &self.plans[&node];
            finish_match_arm(sema, fcx, preparation, &plan.analysis.arms[index], body)
        };
        self.scopes.restore(parent_scope_id, parent);
        let plan = self.plans.get_mut(&node).expect("match prepared first");
        plan.analysis.had_error |= binding_error;
        self.owner_tokens
            .get_mut(&node)
            .expect("owner list")
            .extend(owners);
        self.typed_arms.get_mut(&node).expect("arm list")[index] = Some(typed);
    }

    pub(super) fn finish_match(
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        node: InstRef,
    ) -> TirRef {
        let plan = self.plans.remove(&node).expect("match prepared first");
        let arms = self
            .typed_arms
            .remove(&node)
            .expect("arm list")
            .into_iter()
            .flatten()
            .collect();
        let owners = self.owner_tokens.remove(&node).expect("owner list");
        let next = finish_typed_match(sema, fcx, plan.analysis, arms, owners, plan.span, plan.mode);
        if sema.uir.is_direct_catch_match(node) {
            fcx.builder.mark_direct_catch_match(next);
        }
        next
    }
}

fn statement_done(tasks: &mut ChainSpine<Task>, mut state: StmtState, value: TirRef) {
    state.block.tirs.push(value);
    tasks.push(Task::BlockNext(state.block));
}

pub(super) fn match_scrutinee(sema: &Sema<'_>, node: InstRef) -> InstRef {
    if sema.uir.inst(node).tag == InstTag::Catch {
        sema.uir.catch_view(node).scrutinee
    } else {
        sema.uir.match_view(node).scrutinee
    }
}

fn match_arms<'a>(sema: &'a Sema<'a>, node: InstRef) -> Vec<UirMatchArmView<'a>> {
    if sema.uir.inst(node).tag == InstTag::Catch {
        sema.uir.catch_view(node).arms
    } else {
        sema.uir.match_view(node).arms
    }
}
