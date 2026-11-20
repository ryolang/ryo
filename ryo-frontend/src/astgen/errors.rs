use super::*;
use ryo_core::uir::{UirErrorFamilyDecl, UirVariantForm};

pub(super) struct ErrorRegistry {
    pub types: HashMap<StringId, TypeId>,
    pub entries: HashMap<StringId, (TypeId, ast::ErrorDef, Span)>,
    pub order: Vec<StringId>,
    pub families: HashMap<StringId, Vec<TypeId>>,
    pub family_members: HashMap<StringId, Vec<StringId>>,
    pub family_decls: Vec<UirErrorFamilyDecl>,
}

impl ErrorRegistry {
    pub fn new() -> Self {
        Self {
            types: HashMap::new(),
            entries: HashMap::new(),
            order: Vec::new(),
            families: HashMap::new(),
            family_members: HashMap::new(),
            family_decls: Vec::new(),
        }
    }

    pub fn declare_family(
        &mut self,
        program: &ast::Ast,
        def: &ast::ErrorFamilyDef,
        span: Span,
        occupied_names: &mut HashSet<StringId>,
        pool: &mut InternPool,
        sink: &mut DiagSink,
    ) {
        let family = def.name.name;
        let members = program.enum_variants(def.members);
        if members.is_empty() {
            return;
        }
        if self.families.contains_key(&family) {
            sink.emit(Diag::error(
                def.name.span,
                DiagCode::DuplicateDeclaration,
                format!(
                    "error family '{}' cannot be extended or redeclared",
                    pool.str(family)
                ),
            ));
            return;
        }
        if occupied_names.contains(&family) {
            sink.emit(Diag::error(
                def.name.span,
                DiagCode::DuplicateDeclaration,
                format!(
                    "type '{}' is declared with more than one kind",
                    pool.str(family)
                ),
            ));
            return;
        }

        let mut seen = HashSet::new();
        let mut qualified_members = Vec::with_capacity(members.len());
        for member in members {
            if !seen.insert(member.name.name) {
                sink.emit(Diag::error(
                    member.name.span,
                    DiagCode::DuplicateVariant,
                    format!(
                        "duplicate member '{}' in error family '{}'",
                        pool.str(member.name.name),
                        pool.str(family)
                    ),
                ));
                continue;
            }
            let qualified = qualified_error_name(pool, family, member.name.name);
            if occupied_names.contains(&qualified) {
                sink.emit(Diag::error(
                    member.name.span,
                    DiagCode::DuplicateDeclaration,
                    format!(
                        "type '{}' is declared with more than one kind",
                        pool.str(qualified)
                    ),
                ));
                continue;
            }
            qualified_members.push((qualified, *member));
        }

        occupied_names.insert(family);
        let mut member_types = Vec::with_capacity(qualified_members.len());
        for (qualified, member) in qualified_members {
            let ty = pool.declare_error(qualified);
            let error_def = ast::ErrorDef {
                name: ast::Ident::new(qualified, member.name.span),
                payload: member.payload,
            };
            self.types.insert(qualified, ty);
            self.entries.insert(qualified, (ty, error_def, member.span));
            self.order.push(qualified);
            occupied_names.insert(qualified);
            member_types.push(ty);
        }
        self.families.insert(family, member_types.clone());
        self.family_members.insert(
            family,
            members.iter().map(|member| member.name.name).collect(),
        );
        self.family_decls.push(UirErrorFamilyDecl {
            name: family,
            member_names: members.iter().map(|member| member.name.name).collect(),
            members: member_types,
            visibility: def.visibility,
            span,
        });
    }
}

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
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
    depth: u32,
) -> InstRef {
    let step = prepare_error_construct(b, ast, c, types, pool, sink, span);
    drive_error_construct(step, b, ast, types, defined_layouts, pool, sink, depth)
}

#[allow(clippy::too_many_arguments)]
fn drive_error_construct(
    mut step: ErrorStep,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    depth: u32,
) -> InstRef {
    loop {
        match step {
            ErrorStep::Complete(result) => return result,
            ErrorStep::Child(state, arg) => {
                let value = super::expr::gen_expr_d(
                    b,
                    ast,
                    arg,
                    types,
                    defined_layouts,
                    pool,
                    sink,
                    depth + 1,
                );
                step = resume_error_construct(state, value, b, ast, pool);
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

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_error_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    c: ast::ErrorConstruct,
    _types: &TypeResolver,
    _pool: &mut InternPool,
    _sink: &mut DiagSink,
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
    _pool: &mut InternPool,
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
