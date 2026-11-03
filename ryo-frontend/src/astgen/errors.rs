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
            sink.emit(Diag::error(
                def.name.span,
                DiagCode::EmptyEnum,
                format!(
                    "error family '{}' must declare at least one member",
                    pool.str(family)
                ),
            ));
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
        let mut failed = false;
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
                failed = true;
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
                failed = true;
            }
            qualified_members.push((qualified, *member));
        }
        if failed {
            return;
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
    let name = c.name.name;
    if let Some(members) = types.error_family_members.get(&name) {
        let example = format!("{}.{}", pool.str(name), pool.str(members[0]));
        sink.emit(Diag::error(
            c.name.span,
            DiagCode::TypeMismatch,
            format!(
                "error family '{}' is an error-set alias, not a value or constructor; construct a member as {example}",
                pool.str(name),
            ),
        ));
        return b.error_construct(name, Some(name), None, pool.error_type(), false, &[], span);
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
            let example = format!("{}.{}", pool.str(family), pool.str(name));
            let code = if c.args.is_none() {
                DiagCode::UndefinedVariable
            } else {
                DiagCode::UndefinedFunction
            };
            sink.emit(Diag::error(
                c.name.span,
                code,
                format!(
                    "error family member '{}' must be qualified; construct it as {example}",
                    pool.str(name),
                ),
            ));
            let qualified = qualified_error_name(pool, family, name);
            return b.error_construct(
                qualified,
                Some(family),
                None,
                pool.error_type(),
                false,
                &[],
                span,
            );
        }
    }

    let ty = types
        .error_types
        .get(&name)
        .copied()
        .unwrap_or_else(|| pool.error_type());
    lower_resolved_error_construct(
        b,
        ast,
        name,
        None,
        None,
        ty,
        c.args,
        types,
        defined_layouts,
        pool,
        sink,
        span,
    )
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
    let members = &types.error_family_members[&family.name];
    if !members.contains(&member.name) {
        let name = qualified_error_name(pool, family.name, member.name);
        return b.error_construct(
            name,
            Some(family.name),
            Some(member.name),
            pool.error_type(),
            false,
            &[],
            span,
        );
    }

    let name = qualified_error_name(pool, family.name, member.name);
    let ty = types.error_types[&name];
    lower_resolved_error_construct(
        b,
        ast,
        name,
        Some(family.name),
        Some(member.name),
        ty,
        args,
        types,
        defined_layouts,
        pool,
        sink,
        span,
    )
}

#[allow(clippy::too_many_arguments)]
fn lower_resolved_error_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    name: StringId,
    family_root: Option<StringId>,
    family_member: Option<StringId>,
    ty: TypeId,
    args: Option<ast::VariantArgs>,
    types: &TypeResolver,
    defined_layouts: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
) -> InstRef {
    if family_root.is_some() && pool.is_error(ty) {
        return b.error_construct(name, family_root, family_member, ty, false, &[], span);
    }
    let Some(args) = args else {
        if pool.is_error(ty) {
            return b.var_ref(name, span);
        }
        return b.error_construct(name, family_root, family_member, ty, false, &[], span);
    };

    let positional = args.positional.is_some();
    let mut arg_refs = Vec::new();
    if let Some(list) = args.positional {
        for (index, &arg) in ast.expr_list(list).iter().enumerate() {
            let value = gen_expr(b, ast, arg, types, defined_layouts, pool, sink);
            arg_refs.push((index as u32, value));
        }
    } else if let Some(inits) = args.named {
        for &(field, arg) in ast.struct_field_inits(inits) {
            let value = gen_expr(b, ast, arg, types, defined_layouts, pool, sink);
            arg_refs.push((field.raw(), value));
        }
    }

    if positional && pool.is_error(ty) {
        let values: Vec<_> = arg_refs.iter().map(|(_, value)| *value).collect();
        b.call(name, &values, span)
    } else {
        b.error_construct(
            name,
            family_root,
            family_member,
            ty,
            positional,
            &arg_refs,
            span,
        )
    }
}
