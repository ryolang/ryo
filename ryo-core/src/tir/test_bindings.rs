use super::{BindingId, BindingInfo, BindingKind, ScopeId, Span, TirBuilder, TirRef};
use crate::ast::CompoundOp;
use crate::types::{StringId, TypeId};

pub(crate) fn declare(
    builder: &mut TirBuilder,
    name: StringId,
    ty: TypeId,
    mutable: bool,
) -> BindingId {
    builder.register_binding(BindingInfo::new(
        name,
        ty,
        BindingKind::Local {
            mutable,
            borrowed: false,
        },
        ScopeId::from_index(0),
        Span::EMPTY,
    ))
}

pub(crate) fn binding(builder: &mut TirBuilder, name: StringId, ty: TypeId) -> BindingId {
    if let Some((index, _)) = builder
        .bindings()
        .iter()
        .enumerate()
        .rev()
        .find(|(_, info)| info.name == name)
    {
        return BindingId::from_index(index);
    }
    declare(builder, name, ty, false)
}

pub(crate) fn var(builder: &mut TirBuilder, name: StringId, ty: TypeId, span: Span) -> TirRef {
    let id = binding(builder, name, ty);
    builder.var(id, ty, span)
}

pub(crate) fn var_decl(
    builder: &mut TirBuilder,
    name: StringId,
    mutable: bool,
    ty: TypeId,
    initializer: TirRef,
    span: Span,
) -> TirRef {
    let id = declare(builder, name, ty, mutable);
    builder.var_decl(id, mutable, ty, initializer, span)
}

pub(crate) fn assign(
    builder: &mut TirBuilder,
    name: StringId,
    ty: TypeId,
    value: TirRef,
    span: Span,
) -> TirRef {
    let id = binding(builder, name, ty);
    builder.assign(id, ty, value, span)
}

pub(crate) fn compound_assign(
    builder: &mut TirBuilder,
    name: StringId,
    op: CompoundOp,
    ty: TypeId,
    value: TirRef,
    span: Span,
) -> TirRef {
    let id = binding(builder, name, ty);
    builder.compound_assign(id, op, ty, value, span)
}

pub(crate) fn for_range(
    builder: &mut TirBuilder,
    id: BindingId,
    start: TirRef,
    end: TirRef,
    body: &[TirRef],
    ty: TypeId,
    span: Span,
) -> TirRef {
    builder.for_range(id, start, end, body, ty, span)
}

pub(crate) fn destructure(
    builder: &mut TirBuilder,
    rhs: TirRef,
    fields: &[(u32, Option<StringId>, TypeId)],
    span: Span,
) -> TirRef {
    let fields = fields
        .iter()
        .map(|&(index, name, ty)| {
            let id = name.map(|name| declare(builder, name, ty, false));
            (index, id, ty)
        })
        .collect::<Vec<_>>();
    builder.destructure(rhs, &fields, span)
}
