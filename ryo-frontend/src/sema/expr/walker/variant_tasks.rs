use super::super::super::errors::{
    analyze_error_construct_with_skips, is_rejected_error_declaration,
};
use super::super::{analyze_variant_construct_with_skips, field_list, ordinal};
use super::{FuncCtx, NeverMode, Scope, ScopeId, Sema, Task};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::extra::enum_lit_extra;
use ryo_core::spine::ChainSpine;
use ryo_core::tir::TirRef;
use ryo_core::types::{StringId, StructField, TypeKind, VariantKind};
use ryo_core::uir::{InstRef, InstTag};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct VariantState {
    diagnosed: HashMap<InstRef, HashSet<usize>>,
}

impl VariantState {
    pub(super) fn enter(
        &mut self,
        tasks: &mut ChainSpine<Task>,
        sema: &Sema<'_>,
        node: InstRef,
        scope_id: ScopeId,
        mode: NeverMode,
    ) -> bool {
        let tag = sema.uir.inst(node).tag;
        let eligible = match tag {
            InstTag::EnumLit => !sema.pool.is_error(sema.uir.enum_lit_view(node).ty),
            InstTag::ErrorConstruct => {
                let (name, _, _, view) = sema.uir.error_construct_view(node);
                sema.pool.kind(view.ty) == TypeKind::DeclaredError
                    && !is_rejected_error_declaration(sema, name)
            }
            _ => false,
        };
        if !eligible {
            return false;
        }
        tasks.push(Task::Exit(node, scope_id, mode));
        tasks.push(Task::VariantField(node, 0, scope_id));
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
        let child = match sema.uir.inst(node).tag {
            InstTag::EnumLit => {
                let view = sema.uir.enum_lit_view(node);
                let Some((fidx, child)) = view.fields().nth(index) else {
                    return;
                };
                let Some((ename, vname, kind, fields)) = enum_parts(sema, node) else {
                    return;
                };
                if fidx != enum_lit_extra::ORPHANED_FIELD {
                    let span = sema.uir.span(child);
                    let Some(field) = fields.get(fidx as usize) else {
                        let message = if kind == VariantKind::Tuple {
                            format!(
                                "found a {} positional argument — '{}' takes {}",
                                ordinal(fidx as usize + 1),
                                sema.pool.str(vname),
                                fields.len(),
                            )
                        } else {
                            format!(
                                "variant '{}' of enum '{}' has no field '{}' (fields: {})",
                                sema.pool.str(vname),
                                sema.pool.str(ename),
                                fidx,
                                field_list(sema.pool, &fields),
                            )
                        };
                        sema.sink
                            .emit(Diag::error(span, DiagCode::UnknownVariantField, message));
                        self.schedule(tasks, node, index, child, scope_id);
                        return;
                    };
                    if view
                        .fields()
                        .take(index)
                        .any(|(prior, _)| prior == fidx && prior != enum_lit_extra::ORPHANED_FIELD)
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
                child
            }
            InstTag::ErrorConstruct => {
                let view = sema.uir.error_construct_view(node).3;
                let Some((_, child)) = view.fields().nth(index) else {
                    return;
                };
                child
            }
            _ => return,
        };
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
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        node: InstRef,
        index: usize,
    ) {
        match sema.uir.inst(node).tag {
            InstTag::EnumLit => self.after_enum(sema, fcx, analyzed, node, index),
            InstTag::ErrorConstruct => self.after_error(sema, fcx, analyzed, node, index),
            _ => unreachable!(),
        }
    }

    fn after_enum(
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        node: InstRef,
        index: usize,
    ) {
        let view = sema.uir.enum_lit_view(node);
        let Some((fidx, child)) = view.fields().nth(index) else {
            return;
        };
        let Some((ename, vname, kind, fields)) = enum_parts(sema, node) else {
            return;
        };
        let Some(field) = fields.get(fidx as usize) else {
            return;
        };
        if fidx == enum_lit_extra::ORPHANED_FIELD
            || view.fields().take(index).any(|(prior, _)| prior == fidx)
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
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &FuncCtx,
        analyzed: &HashMap<InstRef, TirRef>,
        node: InstRef,
        index: usize,
    ) {
        let (_, _, _, view) = sema.uir.error_construct_view(node);
        let Some((selector, child)) = view.fields().nth(index) else {
            return;
        };
        let error_name = sema.pool.enum_view(view.ty).name();
        let fields = sema
            .pool
            .enum_view(view.ty)
            .variants()
            .next()
            .expect("declared error variant")
            .fields
            .to_vec();
        let field = error_field(&fields, selector, view.positional);
        let Some(field) = field else {
            let message = if view.positional {
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
        if view.fields().take(index).any(|(prior, _)| {
            error_field(&fields, prior, view.positional)
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

    pub(super) fn finish(
        &mut self,
        sema: &mut Sema<'_>,
        fcx: &mut FuncCtx,
        scope: &Scope,
        node: InstRef,
        context: super::SemaExprContext,
    ) -> TirRef {
        let diagnosed = self.diagnosed.remove(&node);
        let span = sema.uir.span(node);
        match sema.uir.inst(node).tag {
            InstTag::EnumLit => analyze_variant_construct_with_skips(
                sema, fcx, scope, node, span, context, diagnosed,
            ),
            InstTag::ErrorConstruct => {
                analyze_error_construct_with_skips(sema, fcx, scope, node, span, context, diagnosed)
            }
            _ => unreachable!(),
        }
    }
}

fn enum_parts(
    sema: &Sema<'_>,
    node: InstRef,
) -> Option<(StringId, StringId, VariantKind, Vec<StructField>)> {
    let view = sema.uir.enum_lit_view(node);
    let eview = sema.pool.enum_view(view.ty);
    let variant = eview.variants().nth(view.variant_index as usize)?;
    Some((
        eview.name(),
        variant.name,
        variant.kind,
        variant.fields.to_vec(),
    ))
}

fn error_field(fields: &[StructField], selector: u32, positional: bool) -> Option<StructField> {
    if positional {
        fields.get(selector as usize).copied()
    } else {
        let name = StringId::from_raw(selector);
        fields.iter().find(|field| field.name == name).copied()
    }
}
