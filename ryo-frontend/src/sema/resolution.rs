use super::Sema;
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::types::{EnumVariantDef, StringId, StructFlags, TypeId, TypeKind};
use ryo_core::uir::{Span, TypeExprId, UirTypeExprKind};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefState {
    Pending,
    InProgress,
    Defined,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NominalEntry {
    Struct(usize),
    Enum(usize),
    Error(usize),
    Family(usize),
}

struct NominalResolver<'a, 'b> {
    sema: &'a mut Sema<'b>,
    entries: HashMap<StringId, NominalEntry>,
    states: HashMap<StringId, DefState>,
    occupied_names: HashSet<StringId>,
    family_names: HashSet<StringId>,
    family_accepted: Vec<bool>,
    family_seen: Vec<HashSet<StringId>>,
}

pub(super) fn declare_and_define(sema: &mut Sema<'_>) {
    utf8_error(sema);
    let mut resolver = NominalResolver::new(sema);
    resolver.declare_all();
    resolver.define_all();
}

pub(super) fn utf8_error(sema: &mut Sema<'_>) -> TypeId {
    let name = sema.type_names.utf8_error;
    if let Some(&ty) = sema.error_types.get(&name) {
        return ty;
    }
    let ty = sema.pool.declare_error(name);
    let field = sema.pool.intern_str("0");
    let str_ty = sema.pool.str_();
    sema.pool.define_error(
        ty,
        EnumVariantDef {
            name,
            kind: ryo_core::types::VariantKind::Tuple,
            fields: vec![(field, str_ty)],
        },
    );
    sema.error_types.insert(name, ty);
    sema.defined_layouts.insert(ty);
    ty
}

impl<'a, 'b> NominalResolver<'a, 'b> {
    fn new(sema: &'a mut Sema<'b>) -> Self {
        let utf8_name = sema.type_names.utf8_error;
        let family_count = sema.uir.error_families.len();
        Self {
            sema,
            entries: HashMap::new(),
            states: HashMap::from([(utf8_name, DefState::Defined)]),
            occupied_names: HashSet::from([utf8_name]),
            family_names: HashSet::new(),
            family_accepted: vec![false; family_count],
            family_seen: vec![HashSet::new(); family_count],
        }
    }

    fn declare_all(&mut self) {
        let mut declarations = Vec::new();
        for (index, decl) in self.sema.uir.struct_decls.iter().enumerate() {
            declarations.push((decl.source_index, NominalEntry::Struct(index)));
        }
        for (index, decl) in self.sema.uir.enum_decls.iter().enumerate() {
            declarations.push((decl.source_index, NominalEntry::Enum(index)));
        }
        for (index, decl) in self.sema.uir.error_families.iter().enumerate() {
            declarations.push((decl.source_index, NominalEntry::Family(index)));
        }
        for (index, decl) in self.sema.uir.error_decls.iter().enumerate() {
            declarations.push((decl.source_index, NominalEntry::Error(index)));
        }
        declarations.sort_by_key(|&(source_index, _)| source_index);
        for (_, entry) in declarations {
            match entry {
                NominalEntry::Struct(index) => self.declare_struct(index),
                NominalEntry::Enum(index) => self.declare_enum(index),
                NominalEntry::Error(index) => self.declare_error(index),
                NominalEntry::Family(index) => self.declare_family(index),
            }
        }
        for index in 0..self.sema.uir.error_families.len() {
            if !self.family_accepted[index] {
                continue;
            }
            let name = self.sema.uir.error_families[index].name;
            let members = self.accepted_family_member_names(index);
            let member_types = members
                .iter()
                .map(|member| self.sema.error_types[member])
                .collect();
            self.sema.error_families.insert(name, member_types);
            self.sema.error_family_member_decls.insert(name, members);
        }
    }

    fn declare_struct(&mut self, index: usize) {
        let decl = self.sema.uir.struct_decls[index].clone();
        let name = decl.name;
        if self.reject_utf8_name(name, decl.name_span) {
            return;
        }
        if matches!(self.entries.get(&name), Some(NominalEntry::Struct(_))) {
            self.sema.sink.emit(Diag::error(
                decl.name_span,
                DiagCode::DuplicateDeclaration,
                format!(
                    "duplicate struct declaration: '{}'",
                    self.sema.pool.str(name)
                ),
            ));
            return;
        }
        if self.reject_cross_kind(name, decl.name_span) {
            return;
        }
        let ty = self.sema.pool.declare_struct(name);
        self.sema.struct_types.insert(name, ty);
        self.entries.insert(name, NominalEntry::Struct(index));
        self.states.insert(name, DefState::Pending);
        self.occupied_names.insert(name);
    }

    fn declare_enum(&mut self, index: usize) {
        let decl = self.sema.uir.enum_decls[index].clone();
        let name = decl.name;
        if self.reject_utf8_name(name, decl.name_span) {
            return;
        }
        if matches!(self.entries.get(&name), Some(NominalEntry::Enum(_))) {
            self.sema.sink.emit(Diag::error(
                decl.name_span,
                DiagCode::DuplicateDeclaration,
                format!("duplicate enum declaration: '{}'", self.sema.pool.str(name)),
            ));
            return;
        }
        if self.reject_cross_kind(name, decl.name_span) {
            return;
        }
        let ty = self.sema.pool.declare_enum(name);
        self.sema.enum_types.insert(name, ty);
        self.entries.insert(name, NominalEntry::Enum(index));
        self.states.insert(name, DefState::Pending);
        self.occupied_names.insert(name);
    }

    fn declare_family(&mut self, index: usize) {
        let decl = self.sema.uir.error_families[index].clone();
        let name = decl.name;
        if self.reject_utf8_name(name, decl.name_span) {
            return;
        }
        if decl.members.is_empty() {
            return;
        }
        if self.family_names.contains(&name) {
            self.sema.sink.emit(Diag::error(
                decl.name_span,
                DiagCode::DuplicateDeclaration,
                format!(
                    "error family '{}' cannot be extended or redeclared",
                    self.sema.pool.str(name)
                ),
            ));
            return;
        }
        if self.reject_cross_kind(name, decl.name_span) {
            return;
        }

        self.family_accepted[index] = true;
        self.family_names.insert(name);
        self.occupied_names.insert(name);
        self.entries.insert(name, NominalEntry::Family(index));
        self.states.insert(name, DefState::Pending);
        self.sema.error_families.insert(name, Vec::new());
        self.sema
            .error_family_members
            .insert(name, decl.member_names.clone());
    }

    fn declare_error(&mut self, index: usize) {
        let decl = self.sema.uir.error_decls[index].clone();
        if let Some(root) = decl.family {
            self.declare_family_member(index, root);
            return;
        }
        let name = decl.name;
        if self.reject_utf8_name(name, decl.payload.span) {
            return;
        }
        if matches!(self.entries.get(&name), Some(NominalEntry::Error(_))) {
            self.sema.sink.emit(Diag::error(
                decl.payload.span,
                DiagCode::DuplicateDeclaration,
                format!(
                    "duplicate error declaration: '{}'",
                    self.sema.pool.str(name)
                ),
            ));
            return;
        }
        if self.reject_cross_kind(name, decl.payload.span) {
            return;
        }
        self.insert_error(index, name);
    }

    fn declare_family_member(&mut self, index: usize, root: StringId) {
        let decl = self.sema.uir.error_decls[index].clone();
        let Some(family_index) = self
            .sema
            .uir
            .error_families
            .iter()
            .position(|family| family.name == root && family.source_index == decl.source_index)
        else {
            return;
        };
        if !self.family_accepted[family_index] {
            return;
        }
        let family = self.sema.uir.error_families[family_index].clone();
        let prefix = format!("{}.", self.sema.pool.str(root));
        let short_name = self.sema.pool.str(decl.name);
        let short_name = short_name.strip_prefix(&prefix).unwrap_or(short_name);
        let short_name = family
            .member_names
            .iter()
            .copied()
            .find(|&name| self.sema.pool.str(name) == short_name)
            .unwrap_or(decl.name);
        if !self.family_seen[family_index].insert(short_name) {
            self.sema.sink.emit(Diag::error(
                decl.payload.span,
                DiagCode::DuplicateVariant,
                format!(
                    "duplicate member '{}' in error family '{}'",
                    self.sema.pool.str(short_name),
                    self.sema.pool.str(root)
                ),
            ));
            return;
        }
        let name = decl.name;
        if self.reject_utf8_name(name, decl.payload.span)
            || self.reject_cross_kind(name, decl.payload.span)
        {
            return;
        }
        self.insert_error(index, name);
    }

    fn accepted_family_member_names(&self, index: usize) -> Vec<StringId> {
        let family = &self.sema.uir.error_families[index];
        let has_linked_decls = self.sema.uir.error_decls.iter().any(|decl| {
            decl.family == Some(family.name) && decl.source_index == family.source_index
        });
        let mut seen = HashSet::new();
        family
            .members
            .iter()
            .copied()
            .filter(|member| seen.insert(*member))
            .filter(|member| {
                let Some(NominalEntry::Error(error_index)) = self.entries.get(member) else {
                    return false;
                };
                let decl = &self.sema.uir.error_decls[*error_index];
                if has_linked_decls {
                    decl.family == Some(family.name) && decl.source_index == family.source_index
                } else {
                    decl.family.is_none()
                }
            })
            .collect()
    }

    fn insert_error(&mut self, index: usize, name: StringId) -> TypeId {
        let ty = self.sema.pool.declare_error(name);
        self.sema.error_types.insert(name, ty);
        self.entries.insert(name, NominalEntry::Error(index));
        self.states.insert(name, DefState::Pending);
        self.occupied_names.insert(name);
        ty
    }

    fn reject_utf8_name(&mut self, name: StringId, span: Span) -> bool {
        if name != self.sema.type_names.utf8_error {
            return false;
        }
        self.sema.sink.emit(Diag::error(
            span,
            DiagCode::DuplicateDeclaration,
            "duplicate declaration of compiler-known type 'Utf8Error'",
        ));
        true
    }

    fn reject_cross_kind(&mut self, name: StringId, span: Span) -> bool {
        if !self.occupied_names.contains(&name) {
            return false;
        }
        self.sema.sink.emit(Diag::error(
            span,
            DiagCode::DuplicateDeclaration,
            format!(
                "type '{}' is declared with more than one kind",
                self.sema.pool.str(name)
            ),
        ));
        true
    }

    fn define_all(&mut self) {
        let mut order = Vec::new();
        for (index, decl) in self.sema.uir.struct_decls.iter().enumerate() {
            if self.entries.get(&decl.name) == Some(&NominalEntry::Struct(index)) {
                order.push(decl.name);
            }
        }
        for (index, decl) in self.sema.uir.enum_decls.iter().enumerate() {
            if self.entries.get(&decl.name) == Some(&NominalEntry::Enum(index)) {
                order.push(decl.name);
            }
        }
        for (index, decl) in self.sema.uir.error_families.iter().enumerate() {
            if self.entries.get(&decl.name) == Some(&NominalEntry::Family(index)) {
                order.push(decl.name);
            }
        }
        for (index, decl) in self.sema.uir.error_decls.iter().enumerate() {
            if self.entries.get(&decl.name) == Some(&NominalEntry::Error(index)) {
                order.push(decl.name);
            }
        }
        for name in order {
            self.define(name);
        }

        let error = self.sema.pool.error_type();
        for (&name, &state) in &self.states {
            if state != DefState::Failed {
                continue;
            }
            match self.entries[&name] {
                NominalEntry::Struct(_) => {
                    self.sema.struct_types.insert(name, error);
                }
                NominalEntry::Enum(_) => {
                    self.sema.enum_types.insert(name, error);
                }
                NominalEntry::Error(_) => {
                    self.sema.error_types.insert(name, error);
                }
                NominalEntry::Family(_) => {}
            }
        }
        self.sema.type_expr_cache.fill([None, None]);
    }

    fn define(&mut self, name: StringId) {
        if self.states.get(&name) != Some(&DefState::Pending) {
            return;
        }
        self.states.insert(name, DefState::InProgress);
        match self.entries[&name] {
            NominalEntry::Struct(index) => self.define_struct(index),
            NominalEntry::Enum(index) => self.define_enum(index),
            NominalEntry::Error(index) => self.define_error(index),
            NominalEntry::Family(index) => self.define_error_family(index),
        }
    }

    fn order_ref(&mut self, name: StringId, span: Span, field: StringId) -> bool {
        let noun = if self.sema.struct_types.contains_key(&name) {
            "struct"
        } else if self.sema.enum_types.contains_key(&name) {
            "enum"
        } else if self.sema.error_types.contains_key(&name) {
            "error"
        } else if self.sema.error_families.contains_key(&name) {
            "error set"
        } else {
            return false;
        };
        match self.states[&name] {
            DefState::InProgress => {
                self.sema.sink.emit(Diag::error(
                    span,
                    DiagCode::InfiniteSize,
                    format!(
                        "{noun} '{}' cannot contain itself by value: field '{}' has type '{}', which would make its size infinite",
                        self.sema.pool.str(name),
                        self.sema.pool.str(field),
                        self.sema.pool.str(name),
                    ),
                ));
                true
            }
            DefState::Pending => {
                self.define(name);
                self.states[&name] != DefState::Defined
            }
            DefState::Defined => false,
            DefState::Failed => true,
        }
    }

    fn resolve_fields(
        &mut self,
        fields: &[(StringId, TypeExprId, Span)],
    ) -> (Vec<(StringId, TypeId, Span)>, bool) {
        let mut resolved = Vec::with_capacity(fields.len());
        let mut failed = false;
        for &(field_name, type_expr, span) in fields {
            let mut refs = Vec::new();
            named_refs_in(self.sema, type_expr, &mut refs);
            for &(name, is_view, ref_span) in &refs {
                if is_view || self.sema.is_primitive(name) {
                    continue;
                }
                if self.order_ref(name, ref_span, field_name) {
                    failed = true;
                }
            }
            let ty = self.sema.resolve_type_expr(type_expr, false);
            if self.sema.pool.is_error(ty) {
                failed = true;
            }
            resolved.push((field_name, ty, span));
        }
        (resolved, failed)
    }

    fn check_derive_eq(
        &mut self,
        name: StringId,
        fields: impl Iterator<Item = (StringId, TypeId, Span)>,
    ) {
        for (field, ty, span) in fields {
            if self.sema.pool.is_eq_capable(ty) {
                continue;
            }
            self.sema.sink.emit(Diag::error(
                span,
                DiagCode::DeriveFieldNotEq,
                format!(
                    "cannot derive 'Eq' for '{}': field '{}' of type '{}' is not Eq-capable",
                    self.sema.pool.str(name),
                    self.sema.pool.str(field),
                    self.sema.pool.display(ty),
                ),
            ));
        }
    }

    fn define_struct(&mut self, index: usize) {
        let decl = self.sema.uir.struct_decls[index].clone();
        let name = decl.name;
        let source_fields = decl
            .fields
            .iter()
            .map(|field| (field.name, field.type_expr, field.span))
            .collect::<Vec<_>>();
        let (fields, failed) = self.resolve_fields(&source_fields);
        if failed {
            self.states.insert(name, DefState::Failed);
            return;
        }
        if decl.attrs.derive_eq {
            self.check_derive_eq(name, fields.iter().copied());
        }
        let field_types = fields
            .iter()
            .map(|&(field, ty, _)| (field, ty))
            .collect::<Vec<_>>();
        let ty = self.sema.struct_types[&name];
        self.sema.pool.define_struct(
            ty,
            name,
            &field_types,
            StructFlags {
                derive_eq: decl.attrs.derive_eq,
                repr_c: decl.attrs.repr_c,
            },
        );
        self.states.insert(name, DefState::Defined);
        for (_, ty, span) in fields {
            if self.sema.pool.is_view(ty) {
                self.sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ViewFieldType,
                    format!(
                        "struct fields must be owned values; '{}' is a projection (Rule 6)",
                        self.sema.pool.display(ty)
                    ),
                ));
            }
        }
    }

    fn define_enum(&mut self, index: usize) {
        let decl = self.sema.uir.enum_decls[index].clone();
        let name = decl.name;
        if decl.variants.is_empty() {
            self.sema.sink.emit(Diag::error(
                decl.name_span,
                DiagCode::EmptyEnum,
                format!(
                    "enum '{}' must declare at least one variant",
                    self.sema.pool.str(name)
                ),
            ));
            self.states.insert(name, DefState::Failed);
            return;
        }
        let mut variants = Vec::with_capacity(decl.variants.len());
        let mut fields = Vec::new();
        let mut seen = Vec::with_capacity(decl.variants.len());
        let mut failed = false;
        for variant in &decl.variants {
            if seen.contains(&variant.name) {
                self.sema.sink.emit(Diag::error(
                    variant.name_span,
                    DiagCode::DuplicateVariant,
                    format!(
                        "duplicate variant '{}' in enum '{}'",
                        self.sema.pool.str(variant.name),
                        self.sema.pool.str(name),
                    ),
                ));
                failed = true;
            } else {
                seen.push(variant.name);
            }
            let source_fields = variant
                .fields
                .iter()
                .map(|field| (field.name, field.type_expr, field.span))
                .collect::<Vec<_>>();
            let (resolved, payload_failed) = self.resolve_fields(&source_fields);
            failed |= payload_failed;
            fields.extend(resolved.iter().copied());
            variants.push(EnumVariantDef {
                name: variant.name,
                kind: variant.kind,
                fields: resolved
                    .into_iter()
                    .map(|(field, ty, _)| (field, ty))
                    .collect(),
            });
        }
        if failed {
            self.states.insert(name, DefState::Failed);
            return;
        }
        if decl.attrs.derive_eq {
            self.check_derive_eq(name, fields.iter().copied());
        }
        let ty = self.sema.enum_types[&name];
        self.sema.pool.define_enum(
            ty,
            name,
            &variants,
            StructFlags {
                derive_eq: decl.attrs.derive_eq,
                repr_c: false,
            },
        );
        self.states.insert(name, DefState::Defined);
        self.sema.defined_layouts.insert(ty);
        for (_, field_ty, span) in fields {
            if self.sema.pool.is_view(field_ty) {
                self.sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ViewFieldType,
                    format!(
                        "enum payload fields must be owned values; '{}' is a projection (Rule 6)",
                        self.sema.pool.display(field_ty)
                    ),
                ));
            }
        }
    }

    fn define_error(&mut self, index: usize) {
        let decl = self.sema.uir.error_decls[index].clone();
        let name = decl.name;
        let mut seen = Vec::with_capacity(decl.payload.fields.len());
        let mut failed = false;
        for field in &decl.payload.fields {
            if seen.contains(&field.name) {
                self.sema.sink.emit(Diag::error(
                    field.span,
                    DiagCode::DuplicateStructField,
                    format!(
                        "field '{}' is specified more than once in error '{}'",
                        self.sema.pool.str(field.name),
                        self.sema.pool.str(name),
                    ),
                ));
                failed = true;
            } else {
                seen.push(field.name);
            }
        }
        let source_fields = decl
            .payload
            .fields
            .iter()
            .map(|field| (field.name, field.type_expr, field.span))
            .collect::<Vec<_>>();
        let (fields, payload_failed) = self.resolve_fields(&source_fields);
        failed |= payload_failed;
        if failed {
            self.states.insert(name, DefState::Failed);
            return;
        }
        let ty = self.sema.error_types[&name];
        self.sema.pool.define_error(
            ty,
            EnumVariantDef {
                name,
                kind: decl.payload.kind,
                fields: fields.iter().map(|&(field, ty, _)| (field, ty)).collect(),
            },
        );
        self.states.insert(name, DefState::Defined);
        self.sema.defined_layouts.insert(ty);
        for (_, field_ty, span) in fields {
            if self.sema.pool.is_view(field_ty) {
                self.sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ViewFieldType,
                    format!(
                        "error payload fields must be owned values; '{}' is a projection (Rule 6)",
                        self.sema.pool.display(field_ty)
                    ),
                ));
            }
        }
    }

    fn define_error_family(&mut self, index: usize) {
        let name = self.sema.uir.error_families[index].name;
        let mut defined = true;
        for member in self.accepted_family_member_names(index) {
            self.define(member);
            defined &= self.states[&member] == DefState::Defined;
        }
        self.states.insert(
            name,
            if defined {
                DefState::Defined
            } else {
                DefState::Failed
            },
        );
    }
}

fn named_refs_in(sema: &Sema<'_>, id: TypeExprId, out: &mut Vec<(StringId, bool, Span)>) {
    let expr = sema.uir.type_expr(id);
    match expr.kind {
        UirTypeExprKind::Name { name, is_view } => out.push((name, is_view, expr.span)),
        UirTypeExprKind::Anon { fields } => {
            for &(_, field) in sema.uir.type_field_list(fields) {
                named_refs_in(sema, field, out);
            }
        }
        UirTypeExprKind::Positional(elements) => {
            for &element in sema.uir.type_expr_list(elements) {
                named_refs_in(sema, element, out);
            }
        }
        UirTypeExprKind::ErrorUnion {
            error_set, success, ..
        } => {
            for &member in sema.uir.type_expr_list(error_set) {
                named_refs_in(sema, member, out);
            }
            named_refs_in(sema, success, out);
        }
    }
}

impl Sema<'_> {
    fn is_primitive(&self, name: StringId) -> bool {
        let names = &self.type_names;
        name == names.int
            || name == names.str_
            || name == names.strview
            || name == names.bytes
            || name == names.bytesview
            || name == names.bool_
            || name == names.float
    }

    pub(super) fn resolve_type_expr(&mut self, id: TypeExprId, allow_inferred: bool) -> TypeId {
        let cache_slot = usize::from(allow_inferred);
        if let Some(resolved) = self.type_expr_cache[id.index()][cache_slot] {
            return resolved;
        }
        let expr = self.uir.type_expr(id);
        let resolved = match expr.kind {
            UirTypeExprKind::Name { name, is_view } => {
                self.resolve_type_name(name, is_view, expr.span)
            }
            UirTypeExprKind::Anon { fields } => {
                let fields = self.uir.type_field_list(fields).to_vec();
                self.resolve_anon_fields(&fields)
            }
            UirTypeExprKind::Positional(elements) => {
                let fields = self
                    .uir
                    .type_expr_list(elements)
                    .iter()
                    .enumerate()
                    .map(|(index, &element)| (self.pool.intern_str(&index.to_string()), element))
                    .collect::<Vec<_>>();
                self.resolve_anon_fields(&fields)
            }
            UirTypeExprKind::ErrorUnion {
                error_set,
                success,
                inferred,
            } => {
                let error_set = self.uir.type_expr_list(error_set).to_vec();
                self.resolve_error_union(&error_set, success, inferred, expr.span, allow_inferred)
            }
        };
        self.type_expr_cache[id.index()][cache_slot] = Some(resolved);
        resolved
    }

    fn resolve_type_name(&mut self, name: StringId, is_view: bool, span: Span) -> TypeId {
        if is_view {
            let message = if name == self.type_names.str_ {
                "`&str` was renamed to `strview` (final spec Q5)".to_string()
            } else {
                format!(
                    "unknown view type: '&{}' (view types are named: `strview`)",
                    self.pool.str(name)
                )
            };
            self.sink
                .emit(Diag::error(span, DiagCode::UnknownType, message));
            return self.pool.error_type();
        }
        let names = &self.type_names;
        if name == names.int {
            self.pool.int()
        } else if name == names.str_ {
            self.pool.str_()
        } else if name == names.strview {
            self.pool.str_view()
        } else if name == names.bytes {
            self.pool.bytes()
        } else if name == names.bytesview {
            self.pool.bytes_view()
        } else if name == names.bool_ {
            self.pool.bool_()
        } else if name == names.float {
            self.pool.float()
        } else if let Some(&ty) = self.struct_types.get(&name) {
            ty
        } else if let Some(&ty) = self.enum_types.get(&name) {
            ty
        } else if let Some(&ty) = self.error_types.get(&name) {
            ty
        } else if let Some(members) = self.error_families.get(&name).cloned() {
            if members.iter().all(|&member| self.type_has_layout(member)) {
                self.pool.error_set(&members)
            } else {
                self.pool.error_type()
            }
        } else {
            let qualified = self.pool.str(name).to_string();
            if let Some((root, member)) = qualified.split_once('.') {
                let root_id = self.pool.intern_str(root);
                if let Some(source_members) = self.error_family_members.get(&root_id).cloned() {
                    let available = source_members
                        .iter()
                        .map(|name| format!("'{}'", self.pool.str(*name)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.sink.emit(Diag::error(
                        span,
                        DiagCode::UnknownVariant,
                        format!(
                            "error family '{root}' has no member '{member}' (members: {available})"
                        ),
                    ));
                    return self.pool.error_type();
                }
            }
            self.sink.emit(Diag::error(
                span,
                DiagCode::UnknownType,
                format!("unknown type: '{}'", self.pool.str(name)),
            ));
            self.pool.error_type()
        }
    }

    fn resolve_anon_fields(&mut self, fields: &[(StringId, TypeExprId)]) -> TypeId {
        let mut pairs = Vec::with_capacity(fields.len());
        for &(name, field) in fields {
            let ty = self.resolve_type_expr(field, false);
            if self.pool.is_view(ty) {
                self.sink.emit(Diag::error(
                    self.uir.type_expr(field).span,
                    DiagCode::ViewFieldType,
                    format!(
                        "struct fields must be owned values; '{}' is a projection (Rule 6)",
                        self.pool.display(ty)
                    ),
                ));
            }
            pairs.push((name, ty));
        }

        let mut seen = Vec::with_capacity(fields.len());
        let mut duplicate = false;
        for &(name, field) in fields {
            if seen.contains(&name) {
                self.sink.emit(Diag::error(
                    self.uir.type_expr(field).span,
                    DiagCode::DuplicateStructField,
                    format!(
                        "field '{}' is specified more than once",
                        self.pool.str(name)
                    ),
                ));
                duplicate = true;
            } else {
                seen.push(name);
            }
        }
        if duplicate || pairs.iter().any(|&(_, ty)| !self.type_has_layout(ty)) {
            return self.pool.error_type();
        }
        self.pool.anon_struct(&pairs)
    }

    fn resolve_error_union(
        &mut self,
        error_set: &[TypeExprId],
        success: TypeExprId,
        inferred: bool,
        span: Span,
        allow_inferred: bool,
    ) -> TypeId {
        if inferred && !allow_inferred {
            self.sink.emit(Diag::error(
                span,
                DiagCode::TypeMismatch,
                "inferred error unions are only valid in function-return position",
            ));
            return self.pool.error_type();
        }
        if !inferred && error_set.is_empty() {
            self.sink.emit(Diag::error(
                span,
                DiagCode::TypeMismatch,
                "an explicit error union must contain at least one error",
            ));
            return self.pool.error_type();
        }
        if inferred {
            let success = self.resolve_error_union_success(success);
            if self.pool.is_error(success) {
                return self.pool.error_type();
            }
            return self.pool.inferred_error_union(success);
        }

        let mut members = Vec::with_capacity(error_set.len());
        let mut failed = false;
        for &member in error_set {
            let ty = self.resolve_type_expr(member, false);
            if self.pool.is_error(ty) {
                failed = true;
                continue;
            }
            match self.pool.kind(ty) {
                TypeKind::DeclaredError | TypeKind::ErrorSet => {
                    let expanded = self.pool.error_set_view(ty).members;
                    if expanded.iter().any(|&member| !self.type_has_layout(member)) {
                        failed = true;
                    }
                    members.extend(expanded);
                }
                _ => {
                    self.sink.emit(Diag::error(
                        self.uir.type_expr(member).span,
                        DiagCode::TypeMismatch,
                        "error union members must be declared errors or error sets",
                    ));
                    failed = true;
                }
            }
        }
        let success = self.resolve_error_union_success(success);
        if failed || self.pool.is_error(success) {
            return self.pool.error_type();
        }
        let error_set = self.pool.error_set(&members);
        self.pool.error_union(error_set, success)
    }

    fn resolve_error_union_success(&mut self, id: TypeExprId) -> TypeId {
        let expr = self.uir.type_expr(id);
        let success = match expr.kind {
            UirTypeExprKind::Name {
                name,
                is_view: false,
            } if name == self.type_names.void => self.pool.void(),
            UirTypeExprKind::Name {
                name,
                is_view: false,
            } if name == self.type_names.never => self.pool.never(),
            _ => self.resolve_type_expr(id, false),
        };
        if self.pool.is_error(success) {
            return success;
        }
        let invalid = match self.pool.kind(success) {
            TypeKind::Never => Some("error union success type cannot be 'never'"),
            TypeKind::View(_) => Some("error union success type must be owned, not a view"),
            TypeKind::DeclaredError
            | TypeKind::ErrorSet
            | TypeKind::ErrorUnion
            | TypeKind::InferredErrorUnion => {
                Some("error types, error sets, and error unions cannot be success types")
            }
            _ => None,
        };
        if let Some(message) = invalid {
            self.sink
                .emit(Diag::error(expr.span, DiagCode::TypeMismatch, message));
            return self.pool.error_type();
        }
        if !self.type_has_layout(success) {
            return self.pool.error_type();
        }
        success
    }

    fn type_has_layout(&self, ty: TypeId) -> bool {
        match self.pool.kind(ty) {
            TypeKind::Error | TypeKind::Never | TypeKind::InferredErrorUnion => false,
            TypeKind::Struct => self.pool.is_defined_struct(ty),
            TypeKind::Enum | TypeKind::DeclaredError => self.defined_layouts.contains(&ty),
            _ => true,
        }
    }
}
