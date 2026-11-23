use super::*;
use ryo_core::uir::UirVariantForm;

pub(super) fn qualified_error_name(
    pool: &mut InternPool,
    family: StringId,
    member: StringId,
) -> StringId {
    let qualified = format!("{}.{}", pool.str(family), pool.str(member));
    pool.intern_str(&qualified)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_error_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    c: ast::ErrorConstruct,
    names: &Names,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
    depth: u32,
) -> InstRef {
    let step = prepare_error_construct(b, ast, c, span);
    drive_error_construct(step, b, ast, names, pool, sink, depth)
}

#[allow(clippy::too_many_arguments)]
fn drive_error_construct(
    mut step: ErrorStep,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    names: &Names,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    depth: u32,
) -> InstRef {
    loop {
        match step {
            ErrorStep::Complete(result) => return result,
            ErrorStep::Child(state, arg) => {
                let value = super::expr::gen_expr_d(b, ast, arg, names, pool, sink, depth + 1);
                step = resume_error_construct(state, value, b, ast);
            }
        }
    }
}

pub(super) enum ErrorStep {
    Child(ErrorState, ast::ExprId),
    Complete(InstRef),
}

pub(super) struct ErrorState {
    name: StringId,
    args: ast::VariantArgs,
    span: Span,
    next: usize,
    arg_refs: Vec<(u32, InstRef)>,
}

pub(super) fn prepare_error_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    c: ast::ErrorConstruct,
    span: Span,
) -> ErrorStep {
    let name = c.name.name;
    let Some(args) = c.args else {
        return ErrorStep::Complete(b.error_construct(
            name,
            None,
            None,
            UirVariantForm::default(),
            &[],
            span,
        ));
    };
    next_error_step(
        ErrorState {
            name,
            args,
            span,
            next: 0,
            arg_refs: Vec::new(),
        },
        b,
        ast,
    )
}

pub(super) fn resume_error_construct(
    mut state: ErrorState,
    value: InstRef,
    b: &mut UirBuilder,
    ast: &ast::Ast,
) -> ErrorStep {
    let selector = if state.args.positional.is_some() {
        state.next as u32
    } else {
        let inits = state
            .args
            .named
            .expect("error constructor has one arg form");
        ast.struct_field_inits(inits)[state.next].0.raw()
    };
    state.arg_refs.push((selector, value));
    state.next += 1;
    next_error_step(state, b, ast)
}

fn next_error_step(state: ErrorState, b: &mut UirBuilder, ast: &ast::Ast) -> ErrorStep {
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
        return ErrorStep::Child(state, child);
    }

    let form = UirVariantForm {
        positional: state.args.positional.is_some(),
        named_parens: state.args.named_parens,
        mixed_fields: state.args.mixed_fields,
    };
    ErrorStep::Complete(b.error_construct(
        state.name,
        None,
        None,
        form,
        &state.arg_refs,
        state.span,
    ))
}
