use super::Sema;
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::types::{EnumVariantDef, StringId, StructFlags, TypeId, TypeKind};
use ryo_core::uir::{Span, TypeExprId, UirTypeExprKind};
use std::collections::{HashMap, HashSet};

#[cfg(test)]
thread_local! {
    static DEFINE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MAX_DEFINE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static STRUCT_DEFINE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) struct DefineDepth;

#[cfg(test)]
impl DefineDepth {
    fn enter() -> Self {
        DEFINE_DEPTH.with(|depth| {
            let next = depth.get() + 1;
            depth.set(next);
            MAX_DEFINE_DEPTH.with(|max| max.set(max.get().max(next)));
        });
        Self
    }

    pub(super) fn reset_max() {
        MAX_DEFINE_DEPTH.with(|max| max.set(0));
    }

    pub(super) fn max() -> usize {
        MAX_DEFINE_DEPTH.with(|max| max.get())
    }

    fn note_struct_define() {
        STRUCT_DEFINE_CALLS.with(|calls| calls.set(calls.get() + 1));
    }

    pub(super) fn reset_struct_define_calls() {
        STRUCT_DEFINE_CALLS.with(|calls| calls.set(0));
    }

    pub(super) fn struct_define_calls() -> usize {
        STRUCT_DEFINE_CALLS.with(|calls| calls.get())
    }
}

#[cfg(test)]
impl Drop for DefineDepth {
    fn drop(&mut self) {
        DEFINE_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

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

type ResolvedFields = (Vec<(StringId, TypeId, Span)>, bool);

struct NominalResolver<'a, 'b> {
    sema: &'a mut Sema<'b>,
    entries: HashMap<StringId, NominalEntry>,
    states: HashMap<StringId, DefState>,
    occupied_names: HashSet<StringId>,
    family_names: HashSet<StringId>,
    family_accepted: Vec<bool>,
    family_seen: Vec<HashSet<StringId>>,
    accepted_family_members: Vec<Vec<StringId>>,
    family_has_linked_decls: Vec<bool>,
    validated_enum_variants: HashSet<(StringId, usize)>,
}

pub(super) fn declare_and_define(sema: &mut Sema<'_>) {
    if sema.uir.struct_decls.is_empty()
        && sema.uir.enum_decls.is_empty()
        && sema.uir.error_decls.is_empty()
        && sema.uir.error_families.is_empty()
    {
        return;
    }
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

pub(super) fn error_type(sema: &mut Sema<'_>, name: StringId) -> Option<TypeId> {
    if name == sema.type_names.utf8_error {
        Some(utf8_error(sema))
    } else {
        sema.error_types.get(&name).copied()
    }
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
            accepted_family_members: vec![Vec::new(); family_count],
            family_has_linked_decls: vec![false; family_count],
            validated_enum_variants: HashSet::new(),
        }
    }

    fn declare_all(&mut self) {
        let mut declarations = Vec::new();
        let mut family_error_decls = vec![Vec::new(); self.sema.uir.error_families.len()];
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
            let Some(link) = decl.family else {
                declarations.push((decl.source_index, NominalEntry::Error(index)));
                continue;
            };
            let Ok(family_index) = usize::try_from(link.family) else {
                continue;
            };
            if let Some(members) = family_error_decls.get_mut(family_index) {
                members.push(index);
            }
        }
        declarations.sort_by_key(|&(source_index, _)| source_index);
        for (_, entry) in declarations {
            match entry {
                NominalEntry::Struct(index) => self.declare_struct(index),
                NominalEntry::Enum(index) => self.declare_enum(index),
                NominalEntry::Error(index) => self.declare_error(index),
                NominalEntry::Family(index) => {
                    self.declare_family(index);
                    for &error_index in &family_error_decls[index] {
                        let link = self.sema.uir.error_decls[error_index]
                            .family
                            .expect("indexed family member must retain its link");
                        self.declare_family_member(error_index, link);
                    }
                }
            }
        }
        for index in 0..self.sema.uir.error_families.len() {
            if !self.family_accepted[index] {
                continue;
            }
            if self.accepted_family_members[index].is_empty()
                && !self.family_has_linked_decls[index]
            {
                let family = &self.sema.uir.error_families[index];
                let mut seen = HashSet::new();
                let members = family
                    .members
                    .iter()
                    .copied()
                    .filter(|member| seen.insert(*member))
                    .filter(|member| {
                        let Some(NominalEntry::Error(error_index)) = self.entries.get(member)
                        else {
                            return false;
                        };
                        self.sema.uir.error_decls[*error_index].family.is_none()
                    })
                    .collect();
                self.accepted_family_members[index] = members;
            }
            let name = self.sema.uir.error_families[index].name;
            let member_types = self.accepted_family_members[index]
                .iter()
                .map(|member| self.sema.error_types[member])
                .collect();
            self.sema.error_families.insert(name, member_types);
            let members = self.accepted_family_members[index].clone();
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
        let decl = &self.sema.uir.error_families[index];
        let name = decl.name;
        let name_span = decl.name_span;
        let has_members = !decl.members.is_empty();
        if self.reject_utf8_name(name, name_span) {
            return;
        }
        if !has_members {
            return;
        }
        if self.family_names.contains(&name) {
            self.sema.sink.emit(Diag::error(
                name_span,
                DiagCode::DuplicateDeclaration,
                format!(
                    "error family '{}' cannot be extended or redeclared",
                    self.sema.pool.str(name)
                ),
            ));
            return;
        }
        if self.reject_cross_kind(name, name_span) {
            return;
        }

        self.family_accepted[index] = true;
        self.family_names.insert(name);
        self.occupied_names.insert(name);
        self.entries.insert(name, NominalEntry::Family(index));
        self.states.insert(name, DefState::Pending);
        self.sema.error_families.insert(name, Vec::new());
        let member_names = self.sema.uir.error_families[index].member_names.clone();
        self.sema
            .error_family_members
            .insert(name, member_names.clone());
        for member in member_names {
            let owners = self.sema.error_family_owners.entry(member).or_default();
            if !owners.contains(&name) {
                owners.push(name);
                let pool = &self.sema.pool;
                owners.sort_by(|a, b| pool.str(*a).cmp(pool.str(*b)));
            }
        }
    }

    fn declare_error(&mut self, index: usize) {
        if let Some(link) = self.sema.uir.error_decls[index].family {
            self.declare_family_member(index, link);
            return;
        }
        let decl = &self.sema.uir.error_decls[index];
        let name = decl.name;
        let name_span = decl.payload.span;
        if self.reject_utf8_name(name, name_span) {
            return;
        }
        if matches!(self.entries.get(&name), Some(NominalEntry::Error(_))) {
            self.sema.sink.emit(Diag::error(
                name_span,
                DiagCode::DuplicateDeclaration,
                format!(
                    "duplicate error declaration: '{}'",
                    self.sema.pool.str(name)
                ),
            ));
            return;
        }
        if self.reject_cross_kind(name, name_span) {
            return;
        }
        self.insert_error(index, name);
    }

    fn declare_family_member(&mut self, index: usize, link: ryo_core::uir::UirErrorMemberLink) {
        let family_index = usize::try_from(link.family).expect("family index fits usize");
        let member_index = usize::try_from(link.member).expect("member index fits usize");
        let Some(family) = self.sema.uir.error_families.get(family_index) else {
            return;
        };
        let Some((&short_name, &qualified)) = family
            .member_names
            .get(member_index)
            .zip(family.members.get(member_index))
        else {
            return;
        };
        let name = self.sema.uir.error_decls[index].name;
        if qualified != name {
            return;
        }
        let root = family.name;
        self.family_has_linked_decls[family_index] = true;
        if !self.family_accepted[family_index] {
            return;
        }
        let name_span = self.sema.uir.error_decls[index].payload.span;
        if !self.family_seen[family_index].insert(short_name) {
            self.sema.sink.emit(Diag::error(
                name_span,
                DiagCode::DuplicateVariant,
                format!(
                    "duplicate member '{}' in error family '{}'",
                    self.sema.pool.str(short_name),
                    self.sema.pool.str(root)
                ),
            ));
            return;
        }
        if self.reject_utf8_name(name, name_span) || self.reject_cross_kind(name, name_span) {
            return;
        }
        self.insert_error(index, name);
        self.accepted_family_members[family_index].push(name);
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
        for name in self.dependency_order(&order) {
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
        if let Some(cache) = &mut self.sema.type_expr_cache {
            cache.fill([None, None]);
        }
    }

    fn dependency_order(&self, source_order: &[StringId]) -> Vec<StringId> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum VisitState {
            InProgress,
            Done,
        }

        if source_order
            .iter()
            .any(|&name| self.has_duplicate_nominal_fields(name))
        {
            return source_order.to_vec();
        }

        let mut states = HashMap::new();
        let mut ordered = Vec::with_capacity(source_order.len());
        let mut has_cycle = false;
        for &root in source_order {
            if states.get(&root) == Some(&VisitState::Done) {
                continue;
            }
            let mut work = vec![(root, false)];
            while let Some((name, finish)) = work.pop() {
                if finish {
                    if states.get(&name) == Some(&VisitState::InProgress) {
                        states.insert(name, VisitState::Done);
                        ordered.push(name);
                    }
                    continue;
                }
                match states.get(&name) {
                    Some(VisitState::InProgress) => {
                        has_cycle = true;
                        continue;
                    }
                    Some(VisitState::Done) => continue,
                    None => {}
                }
                states.insert(name, VisitState::InProgress);
                work.push((name, true));
                for dependency in self.nominal_dependencies(name).into_iter().rev() {
                    if states.get(&dependency) != Some(&VisitState::Done) {
                        work.push((dependency, false));
                    }
                }
            }
        }
        if has_cycle {
            source_order.to_vec()
        } else {
            ordered
        }
    }

    fn has_duplicate_nominal_fields(&self, name: StringId) -> bool {
        match self.entries[&name] {
            NominalEntry::Struct(index) => {
                let fields = &self.sema.uir.struct_decls[index].fields;
                let mut seen = HashSet::with_capacity(fields.len());
                fields.iter().any(|field| !seen.insert(field.name))
            }
            NominalEntry::Enum(index) => {
                self.sema.uir.enum_decls[index]
                    .variants
                    .iter()
                    .any(|variant| {
                        let mut seen = HashSet::with_capacity(variant.fields.len());
                        variant.fields.iter().any(|field| !seen.insert(field.name))
                    })
            }
            NominalEntry::Error(index) => {
                let fields = &self.sema.uir.error_decls[index].payload.fields;
                let mut seen = HashSet::with_capacity(fields.len());
                fields.iter().any(|field| !seen.insert(field.name))
            }
            NominalEntry::Family(_) => false,
        }
    }

    fn nominal_dependencies(&self, name: StringId) -> Vec<StringId> {
        let mut refs = Vec::new();
        match self.entries[&name] {
            NominalEntry::Struct(index) => {
                for field in &self.sema.uir.struct_decls[index].fields {
                    named_refs_in(self.sema, field.type_expr, &mut refs);
                }
            }
            NominalEntry::Enum(index) => {
                for variant in &self.sema.uir.enum_decls[index].variants {
                    for field in &variant.fields {
                        named_refs_in(self.sema, field.type_expr, &mut refs);
                    }
                }
            }
            NominalEntry::Error(index) => {
                for field in &self.sema.uir.error_decls[index].payload.fields {
                    named_refs_in(self.sema, field.type_expr, &mut refs);
                }
            }
            NominalEntry::Family(index) => {
                refs.extend(
                    self.accepted_family_members[index]
                        .iter()
                        .map(|&member| (member, false, Span::EMPTY)),
                );
            }
        }
        refs.into_iter()
            .filter(|&(name, is_view, _)| {
                !is_view && !self.sema.is_primitive(name) && self.entries.contains_key(&name)
            })
            .map(|(name, _, _)| name)
            .collect()
    }

    fn define(&mut self, name: StringId) {
        if self.states.get(&name) != Some(&DefState::Pending) {
            return;
        }
        #[cfg(test)]
        let _depth = DefineDepth::enter();
        let mut work = vec![name];
        while let Some(current) = work.last().copied() {
            match self.states[&current] {
                DefState::Pending => {
                    self.states.insert(current, DefState::InProgress);
                }
                DefState::InProgress => {}
                DefState::Defined | DefState::Failed => {
                    work.pop();
                    continue;
                }
            }
            let completed = match self.entries[&current] {
                NominalEntry::Struct(index) => self.define_struct(index, &mut work),
                NominalEntry::Enum(index) => self.define_enum(index, &mut work),
                NominalEntry::Error(index) => self.define_error(index, &mut work),
                NominalEntry::Family(index) => self.define_error_family(index, &mut work),
            };
            if completed.is_some() {
                work.pop();
            }
        }
    }

    fn order_ref(
        &mut self,
        name: StringId,
        span: Span,
        field: StringId,
        work: &mut Vec<StringId>,
    ) -> Option<bool> {
        let noun = if self.sema.struct_types.contains_key(&name) {
            "struct"
        } else if self.sema.enum_types.contains_key(&name) {
            "enum"
        } else if self.sema.error_types.contains_key(&name) {
            "error"
        } else if self.sema.error_families.contains_key(&name) {
            "error set"
        } else {
            return Some(false);
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
                Some(true)
            }
            DefState::Pending => {
                work.push(name);
                None
            }
            DefState::Defined => Some(false),
            DefState::Failed => Some(true),
        }
    }

    fn resolve_fields(
        &mut self,
        fields: &[(StringId, TypeExprId, Span)],
        work: &mut Vec<StringId>,
    ) -> Option<ResolvedFields> {
        let mut resolved = Vec::with_capacity(fields.len());
        let mut failed = false;
        let mut refs = Vec::new();
        for &(field_name, type_expr, span) in fields {
            refs.clear();
            named_refs_in(self.sema, type_expr, &mut refs);
            for &(name, is_view, ref_span) in &refs {
                if is_view || self.sema.is_primitive(name) {
                    continue;
                }
                if self.order_ref(name, ref_span, field_name, work)? {
                    failed = true;
                }
            }
            let ty = self.sema.resolve_type_expr(type_expr, false);
            if self.sema.pool.is_error(ty) {
                failed = true;
            }
            resolved.push((field_name, ty, span));
        }
        Some((resolved, failed))
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

    fn reject_duplicate_fields(
        &mut self,
        name: StringId,
        kind: &str,
        fields: &[(StringId, TypeExprId, Span)],
        emit_diags: bool,
    ) -> bool {
        let mut seen = Vec::with_capacity(fields.len());
        let mut failed = false;
        for &(field, _, span) in fields {
            if seen.contains(&field) {
                if emit_diags {
                    self.sema.sink.emit(Diag::error(
                        span,
                        DiagCode::DuplicateStructField,
                        format!(
                            "field '{}' is specified more than once in {kind} '{}'",
                            self.sema.pool.str(field),
                            self.sema.pool.str(name),
                        ),
                    ));
                }
                failed = true;
            } else {
                seen.push(field);
            }
        }
        failed
    }

    fn define_struct(&mut self, index: usize, work: &mut Vec<StringId>) -> Option<()> {
        #[cfg(test)]
        DefineDepth::note_struct_define();
        let decl = self.sema.uir.struct_decls[index].clone();
        let name = decl.name;
        let source_fields = decl
            .fields
            .iter()
            .map(|field| (field.name, field.type_expr, field.span))
            .collect::<Vec<_>>();
        if self.reject_duplicate_fields(name, "struct", &source_fields, true) {
            self.states.insert(name, DefState::Failed);
            return Some(());
        }
        let (fields, failed) = self.resolve_fields(&source_fields, work)?;
        if failed {
            self.states.insert(name, DefState::Failed);
            return Some(());
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
        Some(())
    }

    fn define_enum(&mut self, index: usize, work: &mut Vec<StringId>) -> Option<()> {
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
            return Some(());
        }
        let mut variants = Vec::with_capacity(decl.variants.len());
        let mut fields = Vec::new();
        let mut seen = Vec::with_capacity(decl.variants.len());
        let mut failed = false;
        for (variant_index, variant) in decl.variants.iter().enumerate() {
            let validated = self
                .validated_enum_variants
                .contains(&(name, variant_index));
            if seen.contains(&variant.name) {
                if !validated {
                    self.sema.sink.emit(Diag::error(
                        variant.name_span,
                        DiagCode::DuplicateVariant,
                        format!(
                            "duplicate variant '{}' in enum '{}'",
                            self.sema.pool.str(variant.name),
                            self.sema.pool.str(name),
                        ),
                    ));
                }
                failed = true;
            } else {
                seen.push(variant.name);
            }
            let source_fields = variant
                .fields
                .iter()
                .map(|field| (field.name, field.type_expr, field.span))
                .collect::<Vec<_>>();
            let duplicate = self.reject_duplicate_fields(name, "enum", &source_fields, !validated);
            self.validated_enum_variants.insert((name, variant_index));
            let (resolved, payload_failed) = if duplicate {
                (Vec::new(), false)
            } else {
                self.resolve_fields(&source_fields, work)?
            };
            failed |= duplicate | payload_failed;
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
            return Some(());
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
        Some(())
    }

    fn define_error(&mut self, index: usize, work: &mut Vec<StringId>) -> Option<()> {
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
        if failed {
            self.states.insert(name, DefState::Failed);
            return Some(());
        }
        let (fields, failed) = self.resolve_fields(&source_fields, work)?;
        if failed {
            self.states.insert(name, DefState::Failed);
            return Some(());
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
        Some(())
    }

    fn define_error_family(&mut self, index: usize, work: &mut Vec<StringId>) -> Option<()> {
        let name = self.sema.uir.error_families[index].name;
        let mut defined = true;
        for member_index in 0..self.accepted_family_members[index].len() {
            let member = self.accepted_family_members[index][member_index];
            match self.states[&member] {
                DefState::Pending => {
                    work.push(member);
                    return None;
                }
                DefState::InProgress => {
                    self.sema.sink.emit(Diag::error(
                        self.sema.uir.error_families[index].name_span,
                        DiagCode::InfiniteSize,
                        format!(
                            "error set '{}' cannot contain itself by value: member '{}' has type '{}', which would make its size infinite",
                            self.sema.pool.str(name),
                            self.sema.pool.str(member),
                            self.sema.pool.str(member),
                        ),
                    ));
                    self.states.insert(name, DefState::Failed);
                    return Some(());
                }
                DefState::Defined => {}
                DefState::Failed => defined = false,
            }
        }
        self.states.insert(
            name,
            if defined {
                DefState::Defined
            } else {
                DefState::Failed
            },
        );
        Some(())
    }
}

fn named_refs_in(sema: &Sema<'_>, id: TypeExprId, out: &mut Vec<(StringId, bool, Span)>) {
    let mut pending = vec![id];
    while let Some(id) = pending.pop() {
        let expr = sema.uir.type_expr(id);
        match expr.kind {
            UirTypeExprKind::Name { name, is_view } => out.push((name, is_view, expr.span)),
            UirTypeExprKind::Anon { fields } => {
                for &(_, field) in sema.uir.type_field_list(fields).iter().rev() {
                    pending.push(field);
                }
            }
            UirTypeExprKind::Positional(elements) => {
                for &element in sema.uir.type_expr_list(elements).iter().rev() {
                    pending.push(element);
                }
            }
            UirTypeExprKind::ErrorUnion {
                error_set, success, ..
            } => {
                pending.push(success);
                for &member in sema.uir.type_expr_list(error_set).iter().rev() {
                    pending.push(member);
                }
            }
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

    fn cached_type_expr(&self, id: TypeExprId, cache_slot: usize) -> Option<TypeId> {
        self.type_expr_cache
            .as_ref()?
            .get(id.index())?
            .get(cache_slot)
            .copied()
            .flatten()
    }

    fn cache_type_expr(&mut self, id: TypeExprId, cache_slot: usize, resolved: TypeId) {
        if let Some(cache) = &mut self.type_expr_cache {
            cache[id.index()][cache_slot] = Some(resolved);
        }
    }

    fn ensure_type_expr_cache(&mut self) {
        if self.type_expr_cache.is_none() {
            self.type_expr_cache = Some(vec![[None, None]; self.uir.type_exprs.len()]);
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn resolve_type_expr(&mut self, id: TypeExprId, allow_inferred: bool) -> TypeId {
        let cache_slot = usize::from(allow_inferred);
        if let Some(resolved) = self.cached_type_expr(id, cache_slot) {
            return resolved;
        }
        let expr = self.uir.type_expr(id);
        if let UirTypeExprKind::Name { name, is_view } = expr.kind {
            let resolved = if allow_inferred && name == self.type_names.void {
                self.pool.void()
            } else if allow_inferred && name == self.type_names.never {
                self.pool.never()
            } else {
                self.resolve_type_name(name, is_view, expr.span)
            };
            self.cache_type_expr(id, cache_slot, resolved);
            return resolved;
        }
        self.ensure_type_expr_cache();

        enum Task {
            Enter(TypeExprId, bool),
            EnterSuccess(TypeExprId),
            CheckField(TypeExprId),
            CheckErrorMember(TypeExprId),
            FinishSuccess(TypeExprId),
            Finish(TypeExprId, bool),
        }

        let mut tasks = vec![Task::Enter(id, allow_inferred)];
        let mut results = Vec::new();
        while let Some(task) = tasks.pop() {
            match task {
                Task::Enter(id, allow_inferred) => {
                    let cache_slot = usize::from(allow_inferred);
                    if let Some(resolved) = self.cached_type_expr(id, cache_slot) {
                        results.push(resolved);
                        continue;
                    }
                    let expr = self.uir.type_expr(id);
                    match expr.kind {
                        UirTypeExprKind::Name { name, is_view } => {
                            let resolved = if allow_inferred && name == self.type_names.void {
                                self.pool.void()
                            } else if allow_inferred && name == self.type_names.never {
                                self.pool.never()
                            } else {
                                self.resolve_type_name(name, is_view, expr.span)
                            };
                            self.cache_type_expr(id, cache_slot, resolved);
                            results.push(resolved);
                        }
                        UirTypeExprKind::Anon { fields } => {
                            tasks.push(Task::Finish(id, allow_inferred));
                            for &(_, field) in self.uir.type_field_list(fields).iter().rev() {
                                tasks.push(Task::CheckField(field));
                                tasks.push(Task::Enter(field, false));
                            }
                        }
                        UirTypeExprKind::Positional(elements) => {
                            tasks.push(Task::Finish(id, allow_inferred));
                            for &element in self.uir.type_expr_list(elements).iter().rev() {
                                tasks.push(Task::CheckField(element));
                                tasks.push(Task::Enter(element, false));
                            }
                        }
                        UirTypeExprKind::ErrorUnion {
                            error_set,
                            success,
                            inferred,
                        } => {
                            if inferred && !allow_inferred {
                                self.emit_inferred_type(expr.span);
                                let resolved = self.pool.error_type();
                                self.cache_type_expr(id, cache_slot, resolved);
                                results.push(resolved);
                                continue;
                            }
                            if !inferred && self.uir.type_expr_list(error_set).is_empty() {
                                self.emit_empty_union(expr.span);
                                let resolved = self.pool.error_type();
                                self.cache_type_expr(id, cache_slot, resolved);
                                results.push(resolved);
                                continue;
                            }
                            tasks.push(Task::Finish(id, allow_inferred));
                            tasks.push(Task::EnterSuccess(success));
                            if !inferred {
                                for &member in self.uir.type_expr_list(error_set).iter().rev() {
                                    tasks.push(Task::CheckErrorMember(member));
                                    tasks.push(Task::Enter(member, false));
                                }
                            }
                        }
                    }
                }
                Task::EnterSuccess(id) => {
                    let expr = self.uir.type_expr(id);
                    match expr.kind {
                        UirTypeExprKind::Name {
                            name,
                            is_view: false,
                        } if name == self.type_names.void => results.push(self.pool.void()),
                        UirTypeExprKind::Name {
                            name,
                            is_view: false,
                        } if name == self.type_names.never => {
                            self.emit_never_success(expr.span);
                            results.push(self.pool.error_type());
                        }
                        _ => {
                            tasks.push(Task::FinishSuccess(id));
                            tasks.push(Task::Enter(id, false));
                        }
                    }
                }
                Task::CheckField(id) => {
                    let ty = *results.last().expect("field type resolved before checking");
                    if self.pool.is_view(ty) {
                        self.emit_view_field(self.uir.type_expr(id).span, ty);
                    }
                }
                Task::CheckErrorMember(id) => {
                    let ty = *results
                        .last()
                        .expect("error member resolved before checking");
                    if !self.pool.is_error(ty)
                        && !matches!(
                            self.pool.kind(ty),
                            TypeKind::DeclaredError | TypeKind::ErrorSet
                        )
                    {
                        self.emit_error_member(self.uir.type_expr(id).span);
                    }
                }
                Task::FinishSuccess(id) => {
                    let success = results
                        .pop()
                        .expect("success type resolved before checking");
                    let resolved = self.validate_error_union_success(id, success);
                    results.push(resolved);
                }
                Task::Finish(id, allow_inferred) => {
                    let cache_slot = usize::from(allow_inferred);
                    let expr = self.uir.type_expr(id);
                    let resolved = match expr.kind {
                        UirTypeExprKind::Anon { fields } => {
                            let values = results
                                .split_off(results.len() - self.uir.type_field_list(fields).len());
                            let fields = self
                                .uir
                                .type_field_list(fields)
                                .iter()
                                .zip(values)
                                .map(|(&(name, id), ty)| (name, id, ty))
                                .collect::<Vec<_>>();
                            self.resolve_anon_fields(&fields)
                        }
                        UirTypeExprKind::Positional(elements) => {
                            let values = results
                                .split_off(results.len() - self.uir.type_expr_list(elements).len());
                            let fields = self
                                .uir
                                .type_expr_list(elements)
                                .iter()
                                .copied()
                                .zip(values)
                                .enumerate()
                                .map(|(index, (id, ty))| {
                                    (self.pool.intern_str(&index.to_string()), id, ty)
                                })
                                .collect::<Vec<_>>();
                            self.resolve_anon_fields(&fields)
                        }
                        UirTypeExprKind::ErrorUnion {
                            error_set,
                            inferred,
                            ..
                        } => {
                            let success = results.pop().expect("union success resolved last");
                            let members = if inferred {
                                Vec::new()
                            } else {
                                results.split_off(
                                    results.len() - self.uir.type_expr_list(error_set).len(),
                                )
                            };
                            let error_set = self.uir.type_expr_list(error_set).to_vec();
                            self.resolve_error_union(&error_set, members, success, inferred)
                        }
                        UirTypeExprKind::Name { .. } => unreachable!("names finish during entry"),
                    };
                    self.cache_type_expr(id, cache_slot, resolved);
                    results.push(resolved);
                }
            }
        }
        results.pop().expect("root type expression must resolve")
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
        } else if let Some(ty) = error_type(self, name) {
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

    fn emit_inferred_type(&mut self, span: Span) {
        self.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            "inferred error unions are only valid in function-return position",
        ));
    }

    fn emit_empty_union(&mut self, span: Span) {
        self.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            "an explicit error union must contain at least one error",
        ));
    }

    fn emit_never_success(&mut self, span: Span) {
        self.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            "error union success type cannot be 'never'",
        ));
    }

    fn emit_view_field(&mut self, span: Span, ty: TypeId) {
        self.sink.emit(Diag::error(
            span,
            DiagCode::ViewFieldType,
            format!(
                "struct fields must be owned values; '{}' is a projection (Rule 6)",
                self.pool.display(ty)
            ),
        ));
    }

    fn emit_error_member(&mut self, span: Span) {
        self.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            "error union members must be declared errors or error sets",
        ));
    }

    fn resolve_anon_fields(&mut self, fields: &[(StringId, TypeExprId, TypeId)]) -> TypeId {
        let mut seen = Vec::with_capacity(fields.len());
        let mut duplicate = false;
        for &(name, field, _) in fields {
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
        if duplicate || fields.iter().any(|&(_, _, ty)| !self.type_has_layout(ty)) {
            return self.pool.error_type();
        }
        let pairs = fields
            .iter()
            .map(|&(name, _, ty)| (name, ty))
            .collect::<Vec<_>>();
        self.pool.anon_struct(&pairs)
    }

    fn resolve_error_union(
        &mut self,
        error_set: &[TypeExprId],
        resolved_members: Vec<TypeId>,
        success: TypeId,
        inferred: bool,
    ) -> TypeId {
        if inferred {
            if self.pool.is_error(success) {
                return self.pool.error_type();
            }
            return self.pool.inferred_error_union(success);
        }

        let mut members = Vec::with_capacity(error_set.len());
        let mut failed = false;
        for ty in resolved_members {
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
                _ => failed = true,
            }
        }
        if failed || self.pool.is_error(success) {
            return self.pool.error_type();
        }
        let error_set = self.pool.error_set(&members);
        self.pool.error_union(error_set, success)
    }

    fn validate_error_union_success(&mut self, id: TypeExprId, success: TypeId) -> TypeId {
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
            self.sink.emit(Diag::error(
                self.uir.type_expr(id).span,
                DiagCode::TypeMismatch,
                message,
            ));
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
