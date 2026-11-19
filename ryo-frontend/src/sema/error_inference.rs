use super::{DeclId, DeclState, FuncCtx, Sema};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{TirRef, TirTag};
use ryo_core::types::{TypeId, TypeKind};
use ryo_core::uir::{InstTag, Span};

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

pub(super) fn inferred_callee_is_ready(sema: &mut Sema<'_>, callee: DeclId, span: Span) -> bool {
    if !is_inferred(sema, callee) {
        return true;
    }
    let index = sema.body_index(callee);
    match sema.decl_state[index] {
        DeclState::Resolved => sema.inference[index].final_union.is_some(),
        DeclState::Failed => false,
        DeclState::Unresolved | DeclState::InProgress => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::InternalError,
                format!(
                    "inferred callee '{}' was not finalized before its caller",
                    sema.pool.str(sema.uir.func_bodies[index].name)
                ),
            ));
            false
        }
    }
}

pub(super) fn finalize_inferred_function(sema: &mut Sema<'_>, decl: DeclId) -> TypeId {
    let index = sema.body_index(decl);
    let dependencies = sema.inference[index].dependencies.clone();
    for dependency in dependencies {
        let dependency_index = sema.body_index(dependency);
        debug_assert!(matches!(
            sema.decl_state[dependency_index],
            DeclState::Resolved | DeclState::Failed
        ));
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

pub(super) fn reject_recursive_inferred_components(sema: &mut Sema<'_>, graph: &[Vec<u32>]) {
    let mut components = strongly_connected_components(graph);
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

pub(super) fn inferred_dependencies(sema: &Sema<'_>, graph: &[Vec<u32>]) -> Vec<Vec<u32>> {
    graph
        .iter()
        .enumerate()
        .map(|(caller, edges)| {
            let mut dependencies: Vec<u32> = edges
                .iter()
                .copied()
                .filter(|&callee| {
                    callee as usize != caller
                        && is_inferred(sema, sema.declaration_ids[callee as usize])
                })
                .collect();
            dependencies.sort_unstable();
            dependencies.dedup();
            dependencies
        })
        .collect()
}

pub(super) fn direct_named_call_graph(sema: &Sema<'_>) -> Vec<Vec<u32>> {
    let mut graph = vec![Vec::new(); sema.uir.func_bodies.len()];
    for (index, edges) in graph.iter_mut().enumerate() {
        for &inst in sema.body_map().instructions(index) {
            if sema.uir.inst(inst).tag == InstTag::Call {
                let name = sema.uir.call_view(inst).name;
                if let Some(&callee) = sema.name_to_decl.get(&name) {
                    edges.push(
                        u32::try_from(sema.body_index(callee)).expect("body index out of range"),
                    );
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use ryo_core::compilation::CompilationContext;
    use ryo_core::diag::DiagSink;
    use ryo_core::types::InternPool;
    use ryo_core::uir::UirBuilder;
    use std::path::Path;

    #[test]
    fn inferred_call_graph_uses_exact_body_membership() {
        let mut pool = InternPool::new();
        let first = pool.intern_str("first");
        let second = pool.intern_str("second");
        let mut builder = UirBuilder::new();

        let call_first = builder.call(first, &[], Span::EMPTY);
        let nested_first = builder.unary(InstTag::Neg, call_first, Span::EMPTY);
        let first_stmt = builder.unary(InstTag::ExprStmt, nested_first, Span::EMPTY);

        let call_second = builder.call(second, &[], Span::EMPTY);
        let nested_second = builder.unary(InstTag::Not, call_second, Span::EMPTY);
        let second_stmt = builder.unary(InstTag::ExprStmt, nested_second, Span::EMPTY);

        builder.add_function(first, vec![], pool.int(), &[second_stmt], Span::EMPTY);
        builder.add_function(second, vec![], pool.int(), &[first_stmt], Span::EMPTY);
        let uir = builder.finish().with_layout_context(&pool);
        let map = uir.compute_body_map().unwrap();
        assert_eq!(
            map.instructions(0),
            [second_stmt, nested_second, call_second]
        );
        assert_eq!(map.instructions(1), [first_stmt, nested_first, call_first]);

        let mut sink = DiagSink::new();
        let mut context = CompilationContext::new();
        let sema = Sema::new(
            &uir,
            &mut pool,
            &mut sink,
            "",
            Path::new("<test>"),
            &mut context,
        );
        assert_eq!(direct_named_call_graph(&sema), [vec![1], vec![0]]);
    }
}
