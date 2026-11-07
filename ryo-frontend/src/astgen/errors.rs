use super::*;
use ryo_core::uir::UirErrorFamilyDecl;

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
) -> InstRef {
    let step = prepare_error_construct(b, ast, c, types, pool, sink, span);
    drive_error_construct(step, b, ast, types, defined_layouts, pool, sink)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_qualified_error_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    family: ast::Ident,
    member: ast::Ident,
    args: Option<ast::VariantArgs>,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
) -> InstRef {
    let step =
        prepare_qualified_error_construct(b, ast, family, member, args, types, pool, sink, span);
    drive_error_construct(step, b, ast, types, defined_layouts, pool, sink)
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
) -> InstRef {
    loop {
        match step {
            ErrorStep::Complete(result) => return result,
            ErrorStep::Child(state, arg) => {
                let value = gen_expr(b, ast, arg, types, defined_layouts, pool, sink);
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
    family_root: Option<StringId>,
    family_member: Option<StringId>,
    ty: TypeId,
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
    types: &TypeResolver,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
) -> ErrorStep {
    let name = c.name.name;
    if types.error_family_members.contains_key(&name) {
        return prepare_resolved_error_construct(
            b,
            ast,
            name,
            Some(name),
            None,
            pool.error_type(),
            c.args,
            types,
            pool,
            sink,
            span,
        );
    }

    if !types.error_types.contains_key(&name) {
        let mut owners: Vec<_> = types
            .error_family_members
            .iter()
            .filter(|(_, members)| members.contains(&name))
            .map(|(&family, _)| family)
            .collect();
        owners.sort_by(|&a, &b| pool.str(a).cmp(pool.str(b)));
        if let Some(&family) = owners.first() {
            return prepare_resolved_error_construct(
                b,
                ast,
                name,
                Some(family),
                None,
                pool.error_type(),
                c.args,
                types,
                pool,
                sink,
                span,
            );
        }
    }

    let ty = types
        .error_types
        .get(&name)
        .copied()
        .unwrap_or_else(|| pool.error_type());
    prepare_resolved_error_construct(
        b, ast, name, None, None, ty, c.args, types, pool, sink, span,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_qualified_error_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    family: ast::Ident,
    member: ast::Ident,
    args: Option<ast::VariantArgs>,
    types: &TypeResolver,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
) -> ErrorStep {
    let members = &types.error_family_members[&family.name];
    if !members.contains(&member.name) {
        let name = qualified_error_name(pool, family.name, member.name);
        return prepare_resolved_error_construct(
            b,
            ast,
            name,
            Some(family.name),
            Some(member.name),
            pool.error_type(),
            args,
            types,
            pool,
            sink,
            span,
        );
    }

    let name = qualified_error_name(pool, family.name, member.name);
    let ty = types.error_types[&name];
    prepare_resolved_error_construct(
        b,
        ast,
        name,
        Some(family.name),
        Some(member.name),
        ty,
        args,
        types,
        pool,
        sink,
        span,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_resolved_error_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    name: StringId,
    family_root: Option<StringId>,
    family_member: Option<StringId>,
    ty: TypeId,
    args: Option<ast::VariantArgs>,
    types: &TypeResolver,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
) -> ErrorStep {
    let Some(args) = args else {
        if pool.is_error(ty) && family_root.is_none() {
            return ErrorStep::Complete(b.var_ref(name, span));
        }
        return ErrorStep::Complete(b.error_construct(
            name,
            family_root,
            family_member,
            ty,
            false,
            &[],
            span,
        ));
    };

    if family_root.is_some()
        && types.error_types.contains_key(&name)
        && args.named.is_some()
        && !args.named_parens
    {
        sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            format!(
                "error family member '{}' cannot use braced construction; use the bare member or parenthesized payload syntax",
                pool.str(name)
            ),
        ));
        return ErrorStep::Complete(b.enum_lit(pool.error_type(), 0, false, &[], span));
    }

    next_error_step(
        ErrorState {
            name,
            family_root,
            family_member,
            ty,
            args,
            span,
            next: 0,
            arg_refs: Vec::new(),
        },
        b,
        ast,
        pool,
    )
}

pub(super) fn resume_error_construct(
    mut state: ErrorState,
    value: InstRef,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    pool: &mut InternPool,
) -> ErrorStep {
    let field = if state.args.positional.is_some() {
        state.next as u32
    } else {
        let inits = state
            .args
            .named
            .expect("error constructor has one arg form");
        ast.struct_field_inits(inits)[state.next].0.raw()
    };
    state.arg_refs.push((field, value));
    state.next += 1;
    next_error_step(state, b, ast, pool)
}

fn next_error_step(
    state: ErrorState,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    pool: &mut InternPool,
) -> ErrorStep {
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

    let positional = state.args.positional.is_some();
    let result = if state.args.mixed_fields {
        if state.family_root.is_none() {
            b.enum_lit(
                pool.error_type(),
                0,
                positional,
                &state.arg_refs,
                state.span,
            )
        } else {
            b.error_construct(
                state.name,
                state.family_root,
                state.family_member,
                pool.error_type(),
                positional,
                &state.arg_refs,
                state.span,
            )
        }
    } else if state.family_root.is_none() && positional && pool.is_error(state.ty) {
        let values: Vec<_> = state.arg_refs.iter().map(|(_, value)| *value).collect();
        b.call(state.name, &values, state.span)
    } else {
        b.error_construct(
            state.name,
            state.family_root,
            state.family_member,
            state.ty,
            positional,
            &state.arg_refs,
            state.span,
        )
    };
    ErrorStep::Complete(result)
}
