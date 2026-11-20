//! Structural variant-candidate lowering. Declaration selection, field
//! identities, method fallback, and semantic diagnostics belong to Sema.

use super::*;
use ryo_core::uir::UirVariantForm;

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_variant_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    c: ast::VariantConstruct,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
    depth: u32,
) -> InstRef {
    let mut step = prepare_variant_construct(b, ast, c, types, defined_enums, pool, sink, span);
    loop {
        match step {
            VariantStep::Complete(result) => return result,
            VariantStep::Child(state, arg) => {
                let value = super::expr::gen_expr_d(
                    b,
                    ast,
                    arg,
                    types,
                    defined_enums,
                    pool,
                    sink,
                    depth + 1,
                );
                step = resume_variant_construct(*state, value, b, ast, types, pool, sink);
            }
        }
    }
}

pub(super) enum VariantStep {
    Child(Box<VariantState>, ast::ExprId),
    Complete(InstRef),
}

pub(super) struct VariantState {
    c: ast::VariantConstruct,
    args: ast::VariantArgs,
    span: Span,
    next: usize,
    arg_refs: Vec<(u32, InstRef)>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_variant_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    c: ast::VariantConstruct,
    _types: &TypeResolver,
    _defined_enums: &HashSet<TypeId>,
    _pool: &mut InternPool,
    _sink: &mut DiagSink,
    span: Span,
) -> VariantStep {
    let args = c.args.expect("parser always passes Some");
    next_variant_step(
        VariantState {
            c,
            args,
            span,
            next: 0,
            arg_refs: Vec::new(),
        },
        b,
        ast,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resume_variant_construct(
    mut state: VariantState,
    value: InstRef,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    _types: &TypeResolver,
    _pool: &mut InternPool,
    _sink: &mut DiagSink,
) -> VariantStep {
    let selector = if state.args.positional.is_some() {
        state.next as u32
    } else {
        let inits = state
            .args
            .named
            .expect("braced variant carries named fields");
        ast.struct_field_inits(inits)[state.next].0.raw()
    };
    state.arg_refs.push((selector, value));
    state.next += 1;
    next_variant_step(state, b, ast)
}

fn next_variant_step(state: VariantState, b: &mut UirBuilder, ast: &ast::Ast) -> VariantStep {
    let child = if let Some(list) = state.args.positional {
        ast.expr_list(list).get(state.next).copied()
    } else if let Some(inits) = state.args.named {
        ast.struct_field_inits(inits)
            .get(state.next)
            .map(|&(_, arg)| arg)
    } else {
        None
    };
    if let Some(child) = child {
        return VariantStep::Child(Box::new(state), child);
    }

    let form = UirVariantForm {
        positional: state.args.positional.is_some() || state.args.named_parens,
        named_parens: state.args.named_parens,
        mixed_fields: state.args.mixed_fields,
    };
    VariantStep::Complete(b.enum_lit(
        state.c.enum_name.name,
        state.c.variant.name,
        state.c.enum_name.span,
        state.c.variant.span,
        form,
        &state.arg_refs,
        state.span,
    ))
}
