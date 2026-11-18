use super::{DeclId, DeclState, FuncCtx, Sema};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{TirRef, TirTag};
use ryo_core::types::{TypeId, TypeKind};
use ryo_core::uir::{InstRef, InstTag, Span};

#[derive(Default)]
pub(super) struct InferenceState {
    pub members: Vec<TypeId>,
    pub dependencies: Vec<DeclId>,
    pub final_union: Option<TypeId>,
    pub deferred: Vec<TirRef>,
}

pub(super) fn is_inferred(sema: &Sema<'_>, decl: DeclId) -> bool {
    sema.pool
        .kind(sema.uir.func_bodies[sema.body_index(decl)].return_type)
        == TypeKind::InferredErrorUnion
}

pub(super) fn record_members(sema: &mut Sema<'_>, decl: DeclId, members: &[TypeId]) {
    let index = sema.body_index(decl);
    for &member in members {
        let state = &mut sema.inference[index];
        if !state.members.contains(&member) {
            state.members.push(member);
        }
    }
}

pub(super) fn record_dependency(sema: &mut Sema<'_>, caller: DeclId, callee: DeclId) {
    if !is_inferred(sema, caller) || !is_inferred(sema, callee) {
        return;
    }
    let caller_index = sema.body_index(caller);
    let state = &mut sema.inference[caller_index];
    if !state.dependencies.contains(&callee) {
        state.dependencies.push(callee);
    }
}

pub(super) fn note_dependency(sema: &mut Sema<'_>, fcx: &FuncCtx, operand: TirRef) {
    if let Some(&callee) = fcx.inferred_call_dependencies.get(&operand) {
        record_dependency(sema, fcx.decl, callee);
    }
}

pub(super) fn defer_coercion(sema: &mut Sema<'_>, decl: DeclId, coercion: TirRef) {
    let index = sema.body_index(decl);
    sema.inference[index].deferred.push(coercion);
}

pub(super) fn resolve_inferred_callee(sema: &mut Sema<'_>, callee: DeclId, span: Span) -> bool {
    if !is_inferred(sema, callee) {
        return true;
    }
    match sema.decl_state[sema.body_index(callee)] {
        DeclState::Resolved => {
            return sema.inference[sema.body_index(callee)]
                .final_union
                .is_some();
        }
        DeclState::Failed => return false,
        DeclState::InProgress => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::CycleInResolution,
                format!(
                    "recursive inference of '{}'; declare an explicit error union for every inferred function in the cycle",
                    sema.pool.str(sema.uir.func_bodies[sema.body_index(callee)].name)
                ),
            ));
            return false;
        }
        DeclState::Unresolved => {}
    }
    sema.resolve_decl(callee);
    sema.inference[sema.body_index(callee)]
        .final_union
        .is_some()
}

pub(super) fn finalize_inferred_function(sema: &mut Sema<'_>, decl: DeclId) -> TypeId {
    let index = sema.body_index(decl);
    let dependencies = sema.inference[index].dependencies.clone();
    for dependency in dependencies {
        let dependency_index = sema.body_index(dependency);
        if sema.inference[dependency_index].final_union.is_none()
            && sema.decl_state[dependency_index] == DeclState::Unresolved
        {
            sema.resolve_decl(dependency);
        }
        let members = sema.inference[dependency_index].members.clone();
        record_members(sema, decl, &members);
    }

    let body = &sema.uir.func_bodies[index];
    let success = sema.pool.error_union_view(body.return_type).success;
    let members = sema.inference[index].members.clone();
    let error_set = sema.pool.error_set(&members);
    let final_union = sema.pool.error_union(error_set, success);
    sema.inference[index].final_union = Some(final_union);

    let deferred = sema.inference[index].deferred.clone();
    let tir = sema.results[index]
        .as_mut()
        .expect("inferred body must be emitted before finalization");
    tir.return_type = final_union;
    for coercion in deferred {
        debug_assert!(tir.instructions[coercion.index()].tag == TirTag::ErrorUnionCoerce);
        tir.instructions[coercion.index()].ty = final_union;
    }

    if sema.name_to_decl.get(&body.name) == Some(&decl)
        && let Some(signature) = sema.signatures.get_mut(&decl)
    {
        signature.return_type = final_union;
    }
    final_union
}

pub(super) fn reject_recursive_inferred_components(sema: &mut Sema<'_>) {
    if !sema
        .uir
        .func_bodies
        .iter()
        .any(|body| sema.pool.kind(body.return_type) == TypeKind::InferredErrorUnion)
    {
        return;
    }
    let graph = direct_named_call_graph(sema);
    let mut components = strongly_connected_components(&graph);
    components.retain(|component| {
        let self_cycle = component.len() == 1 && {
            let node = u32::try_from(component[0]).expect("body index out of range");
            graph[component[0]].contains(&node)
        };
        (component.len() > 1 || self_cycle)
            && component
                .iter()
                .any(|&decl| is_inferred(sema, sema.declaration_ids[decl]))
    });
    components.sort_by_key(|component| component[0]);

    for component in components {
        let inferred = component
            .iter()
            .copied()
            .find(|&decl| is_inferred(sema, sema.declaration_ids[decl]))
            .expect("cyclic component has an inferred member");
        let names = component
            .iter()
            .map(|&decl| sema.pool.str(sema.uir.func_bodies[decl].name))
            .collect::<Vec<_>>()
            .join(" -> ");
        sema.sink.emit(Diag::error(
            sema.uir.func_bodies[inferred].span,
            DiagCode::CycleInResolution,
            format!(
                "recursive call cycle involving {names} contains an inferred error union; declare an explicit error union for every inferred function in the cycle"
            ),
        ));
        for decl in component {
            if is_inferred(sema, sema.declaration_ids[decl]) {
                sema.decl_state[decl] = DeclState::Failed;
            }
        }
    }
}

fn direct_named_call_graph(sema: &Sema<'_>) -> Vec<Vec<u32>> {
    let mut graph = vec![Vec::new(); sema.uir.func_bodies.len()];
    let mut start = 1usize;
    for (index, body) in sema.uir.func_bodies.iter().enumerate() {
        let end = sema
            .uir
            .body_stmts(body)
            .into_iter()
            .map(|stmt| stmt.index())
            .max()
            .unwrap_or(start.saturating_sub(1));
        for raw in start..=end {
            let inst = InstRef::from_raw(u32::try_from(raw).expect("UIR index out of range"));
            if sema.uir.inst(inst).tag == InstTag::Call {
                let name = sema.uir.call_view(inst).name;
                if let Some(&callee) = sema.name_to_decl.get(&name) {
                    graph[index].push(
                        u32::try_from(sema.body_index(callee)).expect("body index out of range"),
                    );
                }
            }
        }
        start = end + 1;
    }
    graph
}

fn strongly_connected_components(graph: &[Vec<u32>]) -> Vec<Vec<usize>> {
    struct Tarjan<'a> {
        graph: &'a [Vec<u32>],
        next_index: usize,
        indices: Vec<Option<usize>>,
        low: Vec<usize>,
        stack: Vec<usize>,
        on_stack: Vec<bool>,
        components: Vec<Vec<usize>>,
    }

    impl Tarjan<'_> {
        fn visit(&mut self, node: usize) {
            let index = self.next_index;
            self.next_index += 1;
            self.indices[node] = Some(index);
            self.low[node] = index;
            self.stack.push(node);
            self.on_stack[node] = true;

            for edge in 0..self.graph[node].len() {
                let next = self.graph[node][edge] as usize;
                if self.indices[next].is_none() {
                    self.visit(next);
                    self.low[node] = self.low[node].min(self.low[next]);
                } else if self.on_stack[next] {
                    self.low[node] = self.low[node].min(self.indices[next].expect("visited index"));
                }
            }

            if self.low[node] != index {
                return;
            }
            let mut component = Vec::new();
            loop {
                let member = self.stack.pop().expect("SCC root has a stack member");
                self.on_stack[member] = false;
                component.push(member);
                if member == node {
                    break;
                }
            }
            component.sort_unstable();
            self.components.push(component);
        }
    }

    let mut tarjan = Tarjan {
        graph,
        next_index: 0,
        indices: vec![None; graph.len()],
        low: vec![0; graph.len()],
        stack: Vec::new(),
        on_stack: vec![false; graph.len()],
        components: Vec::new(),
    };
    for node in 0..graph.len() {
        if tarjan.indices[node].is_none() {
            tarjan.visit(node);
        }
    }
    tarjan.components
}
