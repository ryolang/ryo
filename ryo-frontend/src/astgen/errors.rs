use super::Span;
use ryo_core::ast;
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::types::{InternPool, StringId, TypeId};
use ryo_core::uir::UirErrorFamilyDecl;
use std::collections::{HashMap, HashSet};

pub(super) struct ErrorRegistry {
    pub types: HashMap<StringId, TypeId>,
    pub entries: HashMap<StringId, (TypeId, ast::ErrorDef, Span)>,
    pub order: Vec<StringId>,
    pub families: HashMap<StringId, Vec<TypeId>>,
    pub family_decls: Vec<UirErrorFamilyDecl>,
}

impl ErrorRegistry {
    pub fn new() -> Self {
        Self {
            types: HashMap::new(),
            entries: HashMap::new(),
            order: Vec::new(),
            families: HashMap::new(),
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
