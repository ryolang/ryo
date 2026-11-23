use super::{Scope, Sema};
use ryo_core::diag::{Diag, DiagCode, ParseDiag};
use ryo_core::extra::enum_lit_extra;
use ryo_core::types::{StringId, TypeId, VariantKind};
use ryo_core::uir::{InstRef, Span, UirVariantForm};

#[derive(Debug, Clone)]
pub(crate) struct ResolvedEnumLit {
    pub ty: TypeId,
    pub variant_index: u32,
    pub fields: Vec<(u32, InstRef)>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedErrorConstruct {
    pub name: StringId,
    pub family_root: Option<StringId>,
    pub family_member: Option<StringId>,
    pub ty: TypeId,
    pub form: UirVariantForm,
    pub fields: Vec<(u32, InstRef)>,
    pub shadowed: Option<(StringId, TypeId)>,
}

impl ResolvedErrorConstruct {
    pub(crate) fn fields(&self) -> impl Iterator<Item = (u32, InstRef)> + '_ {
        self.fields.iter().copied()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Candidate {
    Enum(ResolvedEnumLit),
    Error(ResolvedErrorConstruct),
    Method,
    Call,
    Variable(StringId),
    Recovery { analyze_args: bool },
}

pub(crate) fn resolve_variant(sema: &mut Sema<'_>, scope: &Scope, node: InstRef) -> Candidate {
    let view = sema.uir.enum_lit_view(node);
    if let Some(&ty) = sema.enum_types.get(&view.enum_name) {
        if view.form.positional && scope.lookup_full(view.enum_name).is_some() {
            if view.form.named_parens {
                sema.sink.emit(Diag::error(
                    view.variant_span,
                    DiagCode::NamedArgInParens,
                    ParseDiag::NamedArgInParens {
                        enum_name: None,
                        variant: None,
                    }
                    .message(sema.pool),
                ));
            }
            return Candidate::Method;
        }
        if sema.pool.is_error(ty) {
            return Candidate::Recovery { analyze_args: true };
        }
        if view.form.named_parens {
            sema.sink.emit(Diag::error(
                view.variant_span,
                DiagCode::NamedArgInParens,
                ParseDiag::NamedArgInParens {
                    enum_name: Some(view.enum_name),
                    variant: Some(view.variant),
                }
                .message(sema.pool),
            ));
        }
        let (variant_index, kind, fields) = {
            let eview = sema.pool.enum_view(ty);
            let Some((variant_index, variant)) = eview
                .variants()
                .enumerate()
                .find(|(_, variant)| variant.name == view.variant)
            else {
                sema.sink.emit(Diag::error(
                    view.variant_span,
                    DiagCode::UnknownVariant,
                    format!(
                        "enum '{}' has no variant '{}'",
                        sema.pool.str(view.enum_name),
                        sema.pool.str(view.variant),
                    ),
                ));
                return Candidate::Recovery { analyze_args: true };
            };
            (variant_index as u32, variant.kind, variant.fields.to_vec())
        };
        if view.form.mixed_fields {
            return Candidate::Recovery { analyze_args: true };
        }
        let mut resolved = Vec::with_capacity(view.fields().count());
        for (argument_index, (selector, value)) in view.fields().enumerate() {
            if view.form.positional {
                resolved.push((argument_index as u32, value));
                continue;
            }
            let field_name = StringId::from_raw(selector);
            let Some(field_index) = fields.iter().position(|field| field.name == field_name) else {
                let hint = if kind == VariantKind::Tuple {
                    format!(
                        "; '{}' is a tuple variant: {}.{}(...)",
                        sema.pool.str(view.variant),
                        sema.pool.str(view.enum_name),
                        sema.pool.str(view.variant),
                    )
                } else {
                    String::new()
                };
                sema.sink.emit(Diag::error(
                    sema.uir.span(value),
                    DiagCode::UnknownField,
                    format!(
                        "variant '{}' of enum '{}' has no field '{}'{}",
                        sema.pool.str(view.variant),
                        sema.pool.str(view.enum_name),
                        sema.pool.str(field_name),
                        hint,
                    ),
                ));
                resolved.push((enum_lit_extra::ORPHANED_FIELD, value));
                continue;
            };
            resolved.push((field_index as u32, value));
        }
        return Candidate::Enum(ResolvedEnumLit {
            ty,
            variant_index,
            fields: resolved,
        });
    }

    if error_family_member_names(sema, view.enum_name).is_some() {
        if view.form.mixed_fields {
            return Candidate::Recovery { analyze_args: true };
        }
        let form = UirVariantForm {
            positional: view.form.positional && !view.form.named_parens,
            ..view.form
        };
        return resolve_qualified_error(
            sema,
            scope,
            view.enum_name,
            view.variant,
            form,
            view.fields().collect(),
            true,
            sema.uir.span(node),
        );
    }

    if view.form.positional {
        if view.form.named_parens {
            sema.sink.emit(Diag::error(
                view.variant_span,
                DiagCode::NamedArgInParens,
                ParseDiag::NamedArgInParens {
                    enum_name: None,
                    variant: None,
                }
                .message(sema.pool),
            ));
        }
        return Candidate::Method;
    }

    sema.sink.emit(Diag::error(
        view.enum_span,
        DiagCode::UnknownType,
        format!("unknown type: '{}'", sema.pool.str(view.enum_name)),
    ));
    Candidate::Recovery { analyze_args: true }
}

pub(crate) fn resolve_error(sema: &mut Sema<'_>, scope: &Scope, node: InstRef) -> Candidate {
    let (name, family_root, family_member, view) = sema.uir.error_construct_view(node);
    if view.form.mixed_fields {
        return Candidate::Recovery { analyze_args: true };
    }

    if family_root.is_none() && family_member.is_none() {
        if error_family_member_names(sema, name).is_some() {
            return resolved_error(
                sema,
                scope,
                name,
                Some(name),
                None,
                sema.pool.error_type(),
                view.form,
                view.fields().collect(),
            );
        }
        if !sema.error_types.contains_key(&name) {
            let family = sema
                .error_family_owners
                .get(&name)
                .and_then(|owners| owners.first())
                .copied();
            if let Some(family) = family {
                return resolved_error(
                    sema,
                    scope,
                    name,
                    Some(family),
                    None,
                    sema.pool.error_type(),
                    view.form,
                    view.fields().collect(),
                );
            }
        }
    }

    if let Some(&ty) = sema.error_types.get(&name) {
        if family_root.is_none()
            && !view.form.positional
            && !view.form.named_parens
            && view.fields().next().is_none()
            && sema.pool.is_error(ty)
        {
            return Candidate::Variable(name);
        }
        return resolved_error(
            sema,
            scope,
            name,
            family_root,
            family_member,
            ty,
            view.form,
            view.fields().collect(),
        );
    }

    if family_root.is_none() && view.form.positional {
        for (_, value) in view.fields() {
            sema.call_arg_refs[value.index()] = true;
        }
        return Candidate::Call;
    }
    if family_root.is_none() && view.fields().next().is_none() && !view.form.named_parens {
        return Candidate::Variable(name);
    }
    resolved_error(
        sema,
        scope,
        name,
        family_root,
        family_member,
        sema.pool.error_type(),
        view.form,
        view.fields().collect(),
    )
}

pub(crate) fn resolve_bare_qualified_error(
    sema: &mut Sema<'_>,
    scope: &Scope,
    root: StringId,
    member: StringId,
    span: Span,
) -> Option<Candidate> {
    if error_family_member_names(sema, root).is_some() {
        Some(resolve_qualified_error(
            sema,
            scope,
            root,
            member,
            UirVariantForm::default(),
            Vec::new(),
            false,
            span,
        ))
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_qualified_error(
    sema: &mut Sema<'_>,
    scope: &Scope,
    root: StringId,
    member: StringId,
    form: UirVariantForm,
    fields: Vec<(u32, InstRef)>,
    has_arg_list: bool,
    span: Span,
) -> Candidate {
    let name = qualified_error_name(sema.pool, root, member);
    let ty = sema
        .error_types
        .get(&name)
        .copied()
        .unwrap_or_else(|| sema.pool.error_type());
    if has_arg_list
        && !form.positional
        && !form.named_parens
        && sema.error_types.contains_key(&name)
    {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            format!(
                "error family member '{}' cannot use braced construction; use the bare member or parenthesized payload syntax",
                sema.pool.str(name),
            ),
        ));
        return Candidate::Recovery { analyze_args: true };
    }
    resolved_error(
        sema,
        scope,
        name,
        Some(root),
        Some(member),
        ty,
        form,
        fields,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolved_error(
    _sema: &Sema<'_>,
    scope: &Scope,
    name: StringId,
    family_root: Option<StringId>,
    family_member: Option<StringId>,
    ty: TypeId,
    form: UirVariantForm,
    fields: Vec<(u32, InstRef)>,
) -> Candidate {
    let shadowed = match (family_root, family_member) {
        (Some(root), Some(_)) => scope.lookup(root).map(|bound| (root, bound)),
        _ => scope.lookup(name).map(|bound| (name, bound)),
    };
    Candidate::Error(ResolvedErrorConstruct {
        name,
        family_root,
        family_member,
        ty,
        form,
        fields,
        shadowed,
    })
}

pub(crate) fn error_family_member_names<'a>(
    sema: &'a Sema<'a>,
    root: StringId,
) -> Option<&'a [StringId]> {
    sema.error_family_members.get(&root).map(Vec::as_slice)
}

pub(crate) fn qualified_error_name(
    pool: &mut ryo_core::types::InternPool,
    root: StringId,
    member: StringId,
) -> StringId {
    let qualified = format!("{}.{}", pool.str(root), pool.str(member));
    pool.intern_str(&qualified)
}

pub(crate) fn resolve_optional_match_error_path(
    sema: &mut Sema<'_>,
    root: Option<StringId>,
    member: StringId,
    span: Span,
) -> Option<Result<StringId, ()>> {
    let root = root?;
    let (known, available) = {
        let members = error_family_member_names(sema, root)?;
        let known = members.contains(&member);
        let available = (!known).then(|| {
            members
                .iter()
                .map(|member| format!("'{}'", sema.pool.str(*member)))
                .collect::<Vec<_>>()
                .join(", ")
        });
        (known, available)
    };
    if known {
        return Some(Ok(qualified_error_name(sema.pool, root, member)));
    }
    sema.sink.emit(Diag::error(
        span,
        DiagCode::UnknownMatchVariant,
        format!(
            "error family '{}' has no member '{}' (members: {})",
            sema.pool.str(root),
            sema.pool.str(member),
            available.expect("unknown member diagnostic members")
        ),
    ));
    Some(Err(()))
}

pub(crate) fn unqualified_call_candidate_name(sema: &Sema<'_>, node: InstRef) -> Option<StringId> {
    let (name, family_root, family_member, view) = sema.uir.error_construct_view(node);
    if family_root.is_some()
        || family_member.is_some()
        || !view.form.positional
        || view.form.mixed_fields
        || sema.error_types.contains_key(&name)
        || error_family_member_names(sema, name).is_some()
        || sema.error_family_owners.contains_key(&name)
    {
        return None;
    }
    Some(name)
}
