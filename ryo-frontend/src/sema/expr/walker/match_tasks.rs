use super::super::super::match_::{
    MatchArmPreparation, MatchPlan, TypedArm, TypedArmBody, analyze_scrutinee, finish_match_arm,
    finish_typed_match, prepare_match, prepare_match_arm,
};
use super::super::super::stmt::analyze_stmt;
use super::{
    FuncCtx, NeverMode, Scope, ScopeArena, ScopeId, Sema, SemaExprContext, StructState, Task,
};
use ryo_core::spine::ChainSpine;
use ryo_core::tir::TirRef;
use ryo_core::types::TypeId;
use ryo_core::uir::{InstRef, InstTag, UirArmBodyView, UirMatchArmView};
use std::collections::{BTreeMap, HashMap};

struct PendingArm {
    scope_id: ScopeId,
    owners: Vec<(u32, TypeId)>,
    binding_error: bool,
}

pub(super) struct ArmStatement {
    pub(super) node: InstRef,
    pub(super) arm_index: usize,
    pub(super) stmt_index: usize,
}

pub(super) struct MatchState<'a> {
    pub(super) scopes: ScopeArena<'a>,
    plans: HashMap<InstRef, MatchPlan>,
    typed_arms: HashMap<InstRef, Vec<Option<TypedArm>>>,
    owner_tokens: HashMap<InstRef, Vec<(u32, TypeId)>>,
    arm_preparations: HashMap<(InstRef, usize), PendingArm>,
    statement_tirs: HashMap<(InstRef, usize), BTreeMap<usize, TirRef>>,
}

impl<'a> MatchState<'a> {
    pub(super) fn new(scope: &'a Scope<'a>) -> Self {
        Self {
            scopes: ScopeArena::new(scope),
            plans: HashMap::new(),
            typed_arms: HashMap::new(),
            owner_tokens: HashMap::new(),
            arm_preparations: HashMap::new(),
            statement_tirs: HashMap::new(),
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
            for stmt_index in (0..stmts.len()).rev() {
                tasks.push(Task::BlockStmt(node, index, stmt_index, child_scope));
            }
        }
    }

    pub(super) fn block_stmt(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        struct_state: &mut StructState,
        statement: ArmStatement,
        scope_id: ScopeId,
    ) {
        let ArmStatement {
            node,
            arm_index,
            stmt_index,
        } = statement;
        let uir_arms = match_arms(sema, node);
        let stmt = uir_arms[arm_index]
            .body
            .stmts()
            .nth(stmt_index)
            .expect("stmt");
        let expression = match sema.uir.inst(stmt).tag {
            InstTag::ExprStmt => match sema.uir.inst(stmt).data {
                ryo_core::uir::InstData::UnOp(operand) => Some(operand),
                _ => unreachable!(),
            },
            InstTag::Return => match sema.uir.inst(stmt).data {
                ryo_core::uir::InstData::UnOp(operand) => Some(operand),
                _ => None,
            },
            InstTag::VarDecl => {
                let view = sema.uir.var_decl_view(stmt);
                (view.ty.is_none() || sema.uir.inst(view.initializer).tag != InstTag::StructLit)
                    .then_some(view.initializer)
            }
            _ => None,
        };
        if let Some(expression) = expression {
            if sema.uir.inst(stmt).tag == InstTag::Return
                && sema.uir.inst(expression).tag == InstTag::StructLit
            {
                struct_state.expect(expression, fcx.return_type);
            }
            tasks.push(Task::FinishStmt(node, arm_index, stmt_index, scope_id));
            tasks.push(Task::Enter(expression, scope_id, NeverMode::Allow));
        } else {
            let value = analyze_stmt(sema, fcx, self.scopes.get_mut(scope_id), stmt);
            self.statement_tirs
                .entry((node, arm_index))
                .or_default()
                .insert(stmt_index, value);
        }
    }

    pub(super) fn finish_stmt(
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        node: InstRef,
        arm_index: usize,
        stmt_index: usize,
        scope_id: ScopeId,
    ) {
        let uir_arms = match_arms(sema, node);
        let stmt = uir_arms[arm_index]
            .body
            .stmts()
            .nth(stmt_index)
            .expect("stmt");
        let value = analyze_stmt(sema, fcx, self.scopes.get_mut(scope_id), stmt);
        self.statement_tirs
            .entry((node, arm_index))
            .or_default()
            .insert(stmt_index, value);
    }

    pub(super) fn finish_arm(
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
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
            UirArmBodyView::Stmts(_) => TypedArmBody::Stmts(
                self.statement_tirs
                    .remove(&(node, index))
                    .unwrap_or_default()
                    .into_values()
                    .collect(),
            ),
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
