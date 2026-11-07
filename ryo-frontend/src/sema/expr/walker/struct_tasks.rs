use super::super::{analyze_struct_lit_with_skips, field_list};
use super::{FuncCtx, NeverMode, Scope, ScopeId, Sema, Task};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::spine::ChainSpine;
use ryo_core::tir::TirRef;
use ryo_core::types::{TypeId, TypeKind};
use ryo_core::uir::{InstRef, InstTag};
use std::collections::{HashMap, HashSet};

pub(super) struct StructState {
    targets: HashMap<InstRef, TypeId>,
    diagnosed: HashMap<InstRef, HashSet<usize>>,
}

impl StructState {
    pub(super) fn new(root: InstRef, target: Option<TypeId>) -> Self {
        let mut targets = HashMap::new();
        if let Some(target) = target {
            targets.insert(root, target);
        }
        Self {
            targets,
            diagnosed: HashMap::new(),
        }
    }

    pub(super) fn expect(&mut self, node: InstRef, target: TypeId) {
        self.targets.insert(node, target);
    }

    fn target(&self, sema: &Sema<'_>, node: InstRef) -> Option<TypeId> {
        let target = match sema.uir.struct_lit_view(node).name {
            Some(name) => sema.struct_types.get(&name).copied(),
            None => self.targets.get(&node).copied(),
        }?;
        matches!(
            sema.pool.kind(target),
            TypeKind::Struct | TypeKind::AnonStruct
        )
        .then_some(target)
    }

    fn field_target(&self, sema: &Sema<'_>, node: InstRef, index: usize) -> Option<TypeId> {
        let target = self.target(sema, node)?;
        let view = sema.uir.struct_lit_view(node);
        let (name, _) = view.fields().nth(index)?;
        match sema.pool.kind(target) {
            TypeKind::Struct => sema.pool.struct_field(target, name).map(|field| field.ty),
            TypeKind::AnonStruct => {
                let expected = sema.pool.struct_view(target);
                let shape_matches = expected.fields.len() == view.fields().len()
                    && expected
                        .fields
                        .iter()
                        .zip(view.fields())
                        .all(|(field, (name, _))| field.name == name);
                shape_matches.then_some(expected.fields[index].ty)
            }
            _ => None,
        }
    }

    pub(super) fn enter(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &Sema<'_>,
        node: InstRef,
        scope_id: ScopeId,
        mode: NeverMode,
    ) -> bool {
        if sema.uir.inst(node).tag != InstTag::StructLit {
            return false;
        }
        let view = sema.uir.struct_lit_view(node);
        if let Some(name) = view.name
            && sema
                .struct_types
                .get(&name)
                .is_none_or(|&sty| sema.pool.is_error(sty))
        {
            return false;
        }
        tasks.push(Task::Exit(node, scope_id, mode));
        tasks.push(Task::StructField(node, 0, scope_id));
        true
    }

    pub(super) fn before_field(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        node: InstRef,
        index: usize,
        scope_id: ScopeId,
    ) {
        let view = sema.uir.struct_lit_view(node);
        let Some((name, child)) = view.fields().nth(index) else {
            return;
        };
        let target = self.target(sema, node);
        let field = target.and_then(|target| sema.pool.struct_field(target, name));
        let duplicate = view.fields().take(index).any(|(prior, _)| prior == name);
        let span = sema.uir.span(child);
        if view.name.is_some() && field.is_none() {
            let sty = target.expect("named struct target");
            let sview = sema.pool.struct_view(sty);
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UnknownField,
                format!(
                    "'{}' has no field '{}' (fields: {})",
                    sema.pool.str(sview.name),
                    sema.pool.str(name),
                    field_list(sema.pool, &sview.fields),
                ),
            ));
        } else if duplicate {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::DuplicateStructField,
                format!(
                    "field '{}' is specified more than once",
                    sema.pool.str(name)
                ),
            ));
        }
        if sema.uir.inst(child).tag == InstTag::StructLit
            && let Some(target) = self.field_target(sema, node, index)
        {
            self.targets.insert(child, target);
        }
        self.diagnosed.entry(node).or_default().insert(index);
        tasks.push(Task::StructField(node, index + 1, scope_id));
        tasks.push(Task::AfterStructField(node, index, scope_id));
        tasks.push(Task::Enter(child, scope_id, NeverMode::Reject));
    }

    pub(super) fn after_field(
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        node: InstRef,
        index: usize,
    ) {
        let view = sema.uir.struct_lit_view(node);
        let Some((name, child)) = view.fields().nth(index) else {
            return;
        };
        let target = self.target(sema, node);
        let field = target.and_then(|target| sema.pool.struct_field(target, name));
        if view.name.is_some()
            && (field.is_none() || view.fields().take(index).any(|(n, _)| n == name))
        {
            return;
        }
        let value = analyzed[&child];
        let value_ty = fcx.builder.ty_of(value);
        if view.name.is_some()
            && let Some(field) = field
            && !super::super::super::errors::is_error_union(sema, field.ty)
            && !sema.pool.compatible(value_ty, field.ty)
        {
            sema.sink.emit(Diag::error(
                sema.uir.span(child),
                DiagCode::TypeMismatch,
                format!(
                    "field '{}': expected '{}', found '{}'",
                    sema.pool.str(name),
                    sema.pool.display(field.ty),
                    sema.pool.display(value_ty),
                ),
            ));
        }
        if view.name.is_none()
            && !view.fields().take(index).any(|(prior, _)| prior == name)
            && sema.pool.is_view(value_ty)
        {
            sema.sink.emit(Diag::error(
                sema.uir.span(child),
                DiagCode::ViewFieldType,
                format!(
                    "struct fields must be owned values; '{}' is a projection (Rule 6)",
                    sema.pool.display(value_ty)
                ),
            ));
        }
    }

    pub(super) fn finish(
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        scope: &Scope,
        node: InstRef,
        context: super::SemaExprContext,
    ) -> TirRef {
        analyze_struct_lit_with_skips(
            sema,
            fcx,
            scope,
            node,
            sema.uir.span(node),
            self.targets.remove(&node),
            context,
            self.diagnosed.remove(&node),
        )
    }
}
