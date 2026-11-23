use super::super::super::call::{CallPlan, finish_call, prepare_call};
use super::super::super::errors::{
    analyze_error_candidate_with_skips, is_rejected_error_declaration,
};
use super::super::candidates::{Candidate, ResolvedEnumLit, resolve_error, resolve_variant};
use super::super::{SemaExprContext, analyze_variant_candidate_with_skips, field_list, ordinal};
use super::{FuncCtx, NeverMode, Scope, ScopeId, Sema, Task};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::extra::enum_lit_extra;
use ryo_core::spine::ChainSpine;
use ryo_core::tir::TirRef;
use ryo_core::types::{StringId, StructField, TypeKind, VariantKind};
use ryo_core::uir::{CallView, ExtraList, InstRef, InstTag};
use std::collections::{HashMap, HashSet};

pub(crate) struct VariantState {
    diagnosed: HashMap<InstRef, HashSet<usize>>,
    candidates: Vec<Option<Candidate>>,
    method_recoveries: Vec<Option<TirRef>>,
    method_receivers: Vec<Option<TirRef>>,
    used_slots: Vec<usize>,
    visited_slots: Vec<bool>,
}

impl VariantState {
    pub(crate) fn new(instruction_count: usize) -> Self {
        Self {
            diagnosed: HashMap::new(),
            candidates: vec![None; instruction_count],
            method_recoveries: vec![None; instruction_count],
            method_receivers: vec![None; instruction_count],
            used_slots: Vec::new(),
            visited_slots: vec![false; instruction_count],
        }
    }

    pub(crate) fn reset(&mut self) {
        for slot in self.used_slots.drain(..) {
            self.candidates[slot] = None;
            self.method_recoveries[slot] = None;
            self.method_receivers[slot] = None;
            self.visited_slots[slot] = false;
        }
        self.diagnosed.clear();
    }

    #[cfg(test)]
    pub(crate) fn storage_capacities(&self) -> (usize, usize, usize) {
        (
            self.candidates.capacity(),
            self.method_recoveries.capacity(),
            self.method_receivers.capacity(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn enter(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        calls: &mut HashMap<InstRef, CallPlan>,
        scope: &Scope,
        node: InstRef,
        scope_id: ScopeId,
        mode: NeverMode,
    ) -> bool {
        let candidate = match sema.uir.inst(node).tag {
            InstTag::EnumLit => resolve_variant(sema, scope, node),
            InstTag::ErrorConstruct => resolve_error(sema, scope, node),
            _ => return false,
        };
        let slot = fcx.map_slot(sema.body_map(), node);
        debug_assert!(!self.visited_slots[slot]);
        self.visited_slots[slot] = true;
        self.used_slots.push(slot);
        if matches!(candidate, Candidate::Call) {
            let (name, _, _, view) = sema.uir.error_construct_view(node);
            let args: Vec<_> = view.fields().map(|(_, value)| value).collect();
            let words: Vec<_> = args.iter().map(|arg| arg.raw()).collect();
            let view = CallView {
                name,
                args: ExtraList::new(&words),
            };
            let plan = prepare_call(sema, &view, sema.uir.span(node));
            calls.insert(node, plan);
            self.candidates[slot] = Some(candidate);
            tasks.push(Task::Exit(node, scope_id, mode));
            for index in (0..args.len()).rev() {
                tasks.push(Task::CallArg(node, index, scope_id));
            }
            return true;
        }
        if matches!(candidate, Candidate::Method) {
            let view = sema.uir.enum_lit_view(node);
            if let Some(recovery) = super::super::methods::type_method_receiver_recovery(
                sema,
                fcx,
                scope,
                view.enum_name,
                view.variant,
                sema.uir.span(node),
            ) {
                self.method_recoveries[slot] = Some(recovery);
            } else {
                self.method_receivers[slot] = Some(super::super::resolve_var_read(
                    sema,
                    fcx,
                    scope,
                    view.enum_name,
                    view.enum_span,
                ));
            }
        }
        let schedule_args = match &candidate {
            Candidate::Enum(resolved) => !enum_has_leaf_recovery(sema, node, resolved),
            Candidate::Error(error) => {
                if error.shadowed.is_some() {
                    true
                } else if sema.pool.kind(error.ty) == TypeKind::DeclaredError
                    && !is_rejected_error_declaration(sema, error.name)
                {
                    let kind = sema
                        .pool
                        .enum_view(error.ty)
                        .variants()
                        .next()
                        .expect("declared errors have one variant")
                        .kind;
                    kind != VariantKind::Unit || (!error.form.positional && error.fields.is_empty())
                } else {
                    true
                }
            }
            Candidate::Method | Candidate::Call => true,
            Candidate::Variable(_) => false,
            Candidate::Recovery { analyze_args } => *analyze_args,
        };
        self.candidates[slot] = Some(candidate);
        tasks.push(Task::Exit(node, scope_id, mode));
        if schedule_args {
            tasks.push(Task::VariantField(node, 0, scope_id));
        }
        true
    }

    pub(super) fn before_field(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &mut Sema<'_>,
        fcx: &FuncCtx,
        node: InstRef,
        index: usize,
        scope_id: ScopeId,
    ) {
        let Some(candidate) = self.candidates[fcx.map_slot(sema.body_map(), node)].as_ref() else {
            return;
        };
        let Some((selector, child)) = (match candidate {
            Candidate::Enum(resolved) => resolved.fields.get(index).copied(),
            Candidate::Error(error) => error.fields.get(index).copied(),
            Candidate::Method | Candidate::Call | Candidate::Recovery { .. } => {
                source_field(sema, node, index)
            }
            Candidate::Variable(_) => None,
        }) else {
            return;
        };

        if let Candidate::Enum(resolved) = candidate
            && selector != enum_lit_extra::ORPHANED_FIELD
        {
            let Some((ename, vname, kind, fields)) = enum_parts(sema, resolved) else {
                return;
            };
            let span = sema.uir.span(child);
            let Some(field) = fields.get(selector as usize) else {
                let message = if kind == VariantKind::Tuple {
                    format!(
                        "found a {} positional argument — '{}' takes {}",
                        ordinal(selector as usize + 1),
                        sema.pool.str(vname),
                        fields.len(),
                    )
                } else {
                    format!(
                        "variant '{}' of enum '{}' has no field '{}' (fields: {})",
                        sema.pool.str(vname),
                        sema.pool.str(ename),
                        selector,
                        field_list(sema.pool, &fields),
                    )
                };
                sema.sink
                    .emit(Diag::error(span, DiagCode::UnknownVariantField, message));
                self.schedule(tasks, node, index, child, scope_id);
                return;
            };
            if resolved
                .fields
                .iter()
                .take(index)
                .any(|&(prior, _)| prior == selector && prior != enum_lit_extra::ORPHANED_FIELD)
            {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::DuplicateVariantField,
                    format!(
                        "field '{}' is specified more than once in variant '{}' of enum '{}'",
                        sema.pool.str(field.name),
                        sema.pool.str(vname),
                        sema.pool.str(ename),
                    ),
                ));
            }
        }
        self.schedule(tasks, node, index, child, scope_id);
    }

    fn schedule(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        node: InstRef,
        index: usize,
        child: InstRef,
        scope_id: ScopeId,
    ) {
        self.diagnosed.entry(node).or_default().insert(index);
        tasks.push(Task::VariantField(node, index + 1, scope_id));
        tasks.push(Task::AfterVariantField(node, index, scope_id));
        tasks.push(Task::Enter(child, scope_id, NeverMode::Reject));
    }

    pub(super) fn after_field(
        &self,
        sema: &mut Sema<'_>,
        fcx: &FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        node: InstRef,
        index: usize,
    ) {
        let Some(candidate) = self.candidates[fcx.map_slot(sema.body_map(), node)].as_ref() else {
            return;
        };
        match candidate {
            Candidate::Enum(resolved) => {
                self.after_enum(sema, fcx, analyzed, node, index, resolved)
            }
            Candidate::Error(error) => self.after_error(sema, fcx, analyzed, node, index, error),
            Candidate::Method
            | Candidate::Call
            | Candidate::Variable(_)
            | Candidate::Recovery { .. } => {}
        }
    }

    fn after_enum(
        &self,
        sema: &mut Sema<'_>,
        fcx: &FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        _node: InstRef,
        index: usize,
        resolved: &ResolvedEnumLit,
    ) {
        let Some((fidx, child)) = resolved.fields.get(index).copied() else {
            return;
        };
        let Some((ename, vname, kind, fields)) = enum_parts(sema, resolved) else {
            return;
        };
        let Some(field) = fields.get(fidx as usize) else {
            return;
        };
        if fidx == enum_lit_extra::ORPHANED_FIELD
            || resolved
                .fields
                .iter()
                .take(index)
                .any(|&(prior, _)| prior == fidx)
        {
            return;
        }
        let actual = fcx.builder.ty_of(analyzed[&child]);
        if sema.pool.compatible(actual, field.ty) {
            return;
        }
        let message = if kind == VariantKind::Tuple {
            format!(
                "positional argument {} of variant '{}' of enum '{}': expected '{}', found '{}'",
                fidx + 1,
                sema.pool.str(vname),
                sema.pool.str(ename),
                sema.pool.display(field.ty),
                sema.pool.display(actual),
            )
        } else {
            format!(
                "field '{}' of variant '{}' of enum '{}': expected '{}', found '{}'",
                sema.pool.str(field.name),
                sema.pool.str(vname),
                sema.pool.str(ename),
                sema.pool.display(field.ty),
                sema.pool.display(actual),
            )
        };
        sema.sink.emit(Diag::error(
            sema.uir.span(child),
            DiagCode::TypeMismatch,
            message,
        ));
    }

    fn after_error(
        &self,
        sema: &mut Sema<'_>,
        fcx: &FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        node: InstRef,
        index: usize,
        error: &super::super::candidates::ResolvedErrorConstruct,
    ) {
        if error.shadowed.is_some()
            || sema.pool.kind(error.ty) != TypeKind::DeclaredError
            || is_rejected_error_declaration(sema, error.name)
        {
            return;
        }
        let Some((selector, child)) = error.fields.get(index).copied() else {
            return;
        };
        let error_name = sema.pool.enum_view(error.ty).name();
        let fields = sema
            .pool
            .enum_view(error.ty)
            .variants()
            .next()
            .expect("declared error variant")
            .fields
            .to_vec();
        let field = error_field(&fields, selector, error.form.positional);
        let Some(field) = field else {
            let message = if error.form.positional {
                format!(
                    "'{}' takes {} positional argument{}; found an extra argument",
                    sema.pool.str(error_name),
                    fields.len(),
                    if fields.len() == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "error '{}' has no field '{}'",
                    sema.pool.str(error_name),
                    sema.pool.str(StringId::from_raw(selector))
                )
            };
            sema.sink.emit(Diag::error(
                sema.uir.span(node),
                DiagCode::UnknownVariantField,
                message,
            ));
            return;
        };
        if error.fields.iter().take(index).any(|&(prior, _)| {
            error_field(&fields, prior, error.form.positional)
                .is_some_and(|candidate| candidate.idx == field.idx)
        }) {
            sema.sink.emit(Diag::error(
                sema.uir.span(child),
                DiagCode::DuplicateVariantField,
                format!(
                    "field '{}' is specified more than once in error '{}'",
                    sema.pool.str(field.name),
                    sema.pool.str(error_name)
                ),
            ));
            return;
        }
        let actual = fcx.builder.ty_of(analyzed[&child]);
        if !sema.pool.compatible(actual, field.ty) {
            sema.sink.emit(Diag::error(
                sema.uir.span(child),
                DiagCode::TypeMismatch,
                format!(
                    "field '{}' of error '{}': expected '{}', found '{}'",
                    sema.pool.str(field.name),
                    sema.pool.str(error_name),
                    sema.pool.display(field.ty),
                    sema.pool.display(actual)
                ),
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish(
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        calls: &mut HashMap<InstRef, CallPlan>,
        scope: &Scope,
        node: InstRef,
        context: SemaExprContext,
    ) -> TirRef {
        let diagnosed = self.diagnosed.remove(&node);
        let slot = fcx.map_slot(sema.body_map(), node);
        let candidate = self.candidates[slot]
            .take()
            .expect("variant candidate resolved before scheduling");
        if matches!(candidate, Candidate::Call) {
            let (name, _, _, view) = sema.uir.error_construct_view(node);
            let args: Vec<_> = view.fields().map(|(_, value)| value).collect();
            let words: Vec<_> = args.iter().map(|arg| arg.raw()).collect();
            let view = CallView {
                name,
                args: ExtraList::new(&words),
            };
            let plan = calls
                .remove(&node)
                .expect("candidate call prepared before argument scheduling");
            let arg_tirs = args.iter().map(|arg| analyzed[arg]).collect();
            return finish_call(sema, fcx, scope, &view, sema.uir.span(node), plan, arg_tirs);
        }
        if let Some(recovery) = self.method_recoveries[slot].take() {
            return recovery;
        }
        let span = sema.uir.span(node);
        let method_receiver = self.method_receivers[slot].take();
        match sema.uir.inst(node).tag {
            InstTag::EnumLit => analyze_variant_candidate_with_skips(
                sema,
                fcx,
                scope,
                node,
                span,
                context,
                candidate,
                method_receiver,
                diagnosed,
            ),
            InstTag::ErrorConstruct => analyze_error_candidate_with_skips(
                sema, fcx, scope, node, span, context, candidate, diagnosed,
            ),
            _ => unreachable!(),
        }
    }
}

fn enum_parts(
    sema: &Sema<'_>,
    resolved: &ResolvedEnumLit,
) -> Option<(StringId, StringId, VariantKind, Vec<StructField>)> {
    let eview = sema.pool.enum_view(resolved.ty);
    let variant = eview.variants().nth(resolved.variant_index as usize)?;
    Some((
        eview.name(),
        variant.name,
        variant.kind,
        variant.fields.to_vec(),
    ))
}

fn enum_has_leaf_recovery(sema: &Sema<'_>, node: InstRef, resolved: &ResolvedEnumLit) -> bool {
    let view = sema.uir.enum_lit_view(node);
    let Some((_, _, kind, _)) = enum_parts(sema, resolved) else {
        return true;
    };
    view.form.positional && matches!(kind, VariantKind::Named | VariantKind::Unit)
}

fn source_field(sema: &Sema<'_>, node: InstRef, index: usize) -> Option<(u32, InstRef)> {
    match sema.uir.inst(node).tag {
        InstTag::EnumLit => sema.uir.enum_lit_view(node).fields().nth(index),
        InstTag::ErrorConstruct => sema.uir.error_construct_view(node).3.fields().nth(index),
        _ => None,
    }
}

fn error_field(fields: &[StructField], selector: u32, positional: bool) -> Option<StructField> {
    if positional {
        fields.get(selector as usize).copied()
    } else {
        let name = StringId::from_raw(selector);
        fields.iter().find(|field| field.name == name).copied()
    }
}
