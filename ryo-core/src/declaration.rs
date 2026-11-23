use crate::artifact::{ArtifactId, CompilationContextId};
use crate::source::{SourceError, SourceId};
use crate::span::Span;
use crate::types::StringId;
use crate::uir::Uir;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeclId {
    context: CompilationContextId,
    index: u32,
}

impl DeclId {
    pub const NONE: Self = Self {
        context: CompilationContextId::NONE,
        index: u32::MAX,
    };

    pub fn index(self) -> usize {
        self.index as usize
    }

    pub(crate) const fn raw_parts(self) -> (u32, u32) {
        (self.context.raw(), self.index)
    }

    pub(crate) const fn from_raw_parts(context: u32, index: u32) -> Self {
        Self {
            context: CompilationContextId::from_raw(context),
            index,
        }
    }
}

impl std::fmt::Debug for DeclId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if *self == Self::NONE {
            f.write_str("DeclId::NONE")
        } else {
            write!(f, "DeclId({})", self.index)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionDecl {
    pub id: DeclId,
    pub(crate) name: StringId,
    pub uir_index: usize,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationError {
    WrongContext {
        expected: CompilationContextId,
        actual: CompilationContextId,
    },
    UnknownDeclaration(DeclId),
    ConflictingRegistration {
        uir_index: usize,
    },
    InvalidSource(SourceError),
    InvalidName {
        uir_index: usize,
    },
    InvalidLayout,
    NameArenaExhausted,
}

impl std::fmt::Display for DeclarationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeclarationError::WrongContext { .. } => {
                f.write_str("declaration belongs to a different compilation context")
            }
            DeclarationError::UnknownDeclaration(_) => {
                f.write_str("declaration is not registered or is the NONE sentinel")
            }
            DeclarationError::ConflictingRegistration { uir_index } => {
                write!(f, "conflicting registration for UIR body {uir_index}")
            }
            DeclarationError::InvalidSource(error) => {
                write!(f, "declaration source is invalid: {error}")
            }
            DeclarationError::InvalidName { uir_index } => {
                write!(f, "declaration name for UIR body {uir_index} is invalid")
            }
            DeclarationError::InvalidLayout => {
                f.write_str("UIR belongs to a different type and layout context")
            }
            DeclarationError::NameArenaExhausted => f.write_str("declaration name arena exhausted"),
        }
    }
}

impl std::error::Error for DeclarationError {}

fn push_name(
    names: &mut Vec<crate::extra::ExtraRange>,
    bytes: &mut Vec<u8>,
    base: usize,
    name: &str,
) -> Result<(), DeclarationError> {
    let offset = base
        .checked_add(bytes.len())
        .and_then(|offset| u32::try_from(offset).ok())
        .ok_or(DeclarationError::NameArenaExhausted)?;
    let len = u32::try_from(name.len()).map_err(|_| DeclarationError::NameArenaExhausted)?;
    bytes.extend_from_slice(name.as_bytes());
    names.push(crate::extra::ExtraRange { offset, len });
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RegistrationKey {
    Sourced { source: SourceId, uir_index: usize },
    SourceLess { uir: ArtifactId, uir_index: usize },
}

#[derive(Debug)]
pub struct DeclarationTable {
    context: CompilationContextId,
    declarations: Vec<FunctionDecl>,
    names: Vec<crate::extra::ExtraRange>,
    name_bytes: Vec<u8>,
    by_registration_key: HashMap<RegistrationKey, DeclId>,
}

impl DeclarationTable {
    pub(crate) fn new(context: CompilationContextId) -> Self {
        Self {
            context,
            declarations: Vec::new(),
            names: Vec::new(),
            name_bytes: Vec::new(),
            by_registration_key: HashMap::new(),
        }
    }

    pub(crate) fn register_functions(
        &mut self,
        uir: &Uir,
        pool: &crate::types::InternPool,
    ) -> Result<Vec<DeclId>, DeclarationError> {
        let mut ids = Vec::with_capacity(uir.func_bodies.len());
        let mut new_declarations = Vec::with_capacity(uir.func_bodies.len());
        let mut new_names = Vec::with_capacity(uir.func_bodies.len());
        let mut new_name_bytes = Vec::with_capacity(uir.func_bodies.len().saturating_mul(16));
        let mut new_mappings = Vec::with_capacity(uir.func_bodies.len());
        let name_base = self.name_bytes.len();
        for (uir_index, body) in uir.func_bodies.iter().enumerate() {
            let key = if body.span.source == SourceId::NONE {
                RegistrationKey::SourceLess {
                    uir: uir.identity(),
                    uir_index,
                }
            } else {
                RegistrationKey::Sourced {
                    source: body.span.source,
                    uir_index,
                }
            };
            if let Some(existing) = self.by_registration_key.get(&key).copied() {
                let declaration = &self.declarations[existing.index()];
                let registered_name = &self.name_bytes[self.names[existing.index()].as_range()];
                if registered_name == pool.str(body.name).as_bytes()
                    && declaration.span == body.span
                {
                    ids.push(existing);
                    continue;
                }
                return Err(DeclarationError::ConflictingRegistration { uir_index });
            }
            let id = DeclId {
                context: self.context,
                index: u32::try_from(self.declarations.len() + new_declarations.len())
                    .expect("declaration index space exhausted"),
            };
            new_declarations.push(FunctionDecl {
                id,
                name: body.name,
                uir_index,
                span: body.span,
            });
            push_name(
                &mut new_names,
                &mut new_name_bytes,
                name_base,
                pool.str(body.name),
            )?;
            new_mappings.push((key, id));
            ids.push(id);
        }
        self.declarations.reserve(new_declarations.len());
        self.names.reserve(new_names.len());
        self.name_bytes.reserve(new_name_bytes.len());
        self.by_registration_key.reserve(new_mappings.len());
        self.declarations.extend(new_declarations);
        self.names.extend(new_names);
        self.name_bytes.extend(new_name_bytes);
        for (key, id) in new_mappings {
            self.by_registration_key.insert(key, id);
        }
        Ok(ids)
    }

    pub fn get(&self, id: DeclId) -> Result<&FunctionDecl, DeclarationError> {
        if id.context != self.context {
            return Err(DeclarationError::WrongContext {
                expected: self.context,
                actual: id.context,
            });
        }
        self.declarations
            .get(id.index())
            .filter(|declaration| declaration.id == id)
            .ok_or(DeclarationError::UnknownDeclaration(id))
    }

    pub fn name(&self, id: DeclId) -> Result<&str, DeclarationError> {
        self.get(id)?;
        let range = self.names[id.index()].as_range();
        Ok(std::str::from_utf8(&self.name_bytes[range])
            .expect("declaration names are validated UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compilation::CompilationContext;
    use crate::extra::ExtraRange;
    use crate::span::Span;
    use crate::types::InternPool;
    use crate::uir::{FuncBody, Uir};
    use std::path::PathBuf;

    fn function(pool: &mut InternPool, name: &str, source: SourceId, start: usize) -> FuncBody {
        FuncBody {
            name: pool.intern_str(name),
            params: vec![],
            return_type: None,
            body: ExtraRange { offset: 0, len: 0 },
            span: Span::in_source(source, start, start + 4),
        }
    }

    #[test]
    fn allocation_is_source_ordered_unique_and_preserves_spans() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut pool = InternPool::new();
        let mut uir = Uir::new().with_layout_context(&pool);
        uir.func_bodies
            .push(function(&mut pool, "main", source, 10));
        uir.func_bodies
            .push(function(&mut pool, "main", source, 20));

        let ids = context.register_functions(&uir, &pool).unwrap();

        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].index(), 0);
        assert_eq!(ids[1].index(), 1);
        assert_ne!(ids[0], ids[1]);
        let first = context.declarations().get(ids[0]).unwrap();
        assert_eq!(first.name, pool.intern_str("main"));
        assert_eq!(first.uir_index, 0);
        assert_eq!(first.span, Span::in_source(source, 10, 14));
        assert_eq!(
            context.declarations().get(ids[1]).unwrap().span,
            Span::in_source(source, 20, 24)
        );
        assert_eq!(format!("{:?}", ids[0]), "DeclId(0)");
    }

    #[test]
    fn identical_registration_is_idempotent() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut pool = InternPool::new();
        let mut uir = Uir::new().with_layout_context(&pool);
        uir.func_bodies
            .push(function(&mut pool, "main", source, 10));
        let first = context.register_functions(&uir, &pool).unwrap();

        let second = context.register_functions(&uir, &pool).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn sourceless_registration_distinguishes_uirs_and_replays_identity() {
        let mut context = CompilationContext::new();
        let mut pool = InternPool::new();
        let make_uir = |pool: &mut InternPool| {
            let mut uir = Uir::new().with_layout_context(pool);
            uir.func_bodies.push(FuncBody {
                name: pool.intern_str("main"),
                params: vec![],
                return_type: None,
                body: ExtraRange { offset: 0, len: 0 },
                span: Span::EMPTY,
            });
            uir
        };
        let first_uir = make_uir(&mut pool);
        let second_uir = make_uir(&mut pool);

        let first = context.register_functions(&first_uir, &pool).unwrap();
        let second = context.register_functions(&second_uir, &pool).unwrap();

        assert_ne!(first[0], second[0]);
        assert_eq!(
            context.register_functions(&first_uir, &pool).unwrap(),
            first
        );
    }

    #[test]
    fn identical_registration_accepts_different_pool_name_ids() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut first_pool = InternPool::new();
        let mut first_uir = Uir::new().with_layout_context(&first_pool);
        first_uir
            .func_bodies
            .push(function(&mut first_pool, "main", source, 10));
        let first = context.register_functions(&first_uir, &first_pool).unwrap();
        let mut second_pool = InternPool::new();
        second_pool.intern_str("padding");
        let mut second_uir = Uir::new().with_layout_context(&second_pool);
        second_uir
            .func_bodies
            .push(function(&mut second_pool, "main", source, 10));
        assert_ne!(
            first_uir.func_bodies[0].name,
            second_uir.func_bodies[0].name
        );

        let second = context
            .register_functions(&second_uir, &second_pool)
            .expect("equal source metadata and name text must be idempotent");
        assert_eq!(first, second);
    }

    #[test]
    fn registration_rejects_foreign_sources_before_mutation() {
        let mut foreign = CompilationContext::new();
        let foreign_source =
            foreign.register_source(PathBuf::from("foreign.ryo"), "fn helper".into());
        let mut context = CompilationContext::new();
        let local_source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut pool = InternPool::new();
        let mut combined = Uir::new().with_layout_context(&pool);
        combined
            .func_bodies
            .push(function(&mut pool, "main", local_source, 10));
        combined
            .func_bodies
            .push(function(&mut pool, "helper", foreign_source, 20));

        let error = context
            .register_functions(&combined, &pool)
            .expect_err("a context must reject another context's source");
        assert!(error.to_string().contains("different compilation context"));

        let mut local = Uir::new().with_layout_context(&pool);
        local
            .func_bodies
            .push(function(&mut pool, "main", local_source, 10));
        let ids = context.register_functions(&local, &pool).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].index(), 0);
        assert_eq!(context.declarations().name(ids[0]).unwrap(), "main");
    }

    #[test]
    fn registration_rejects_uir_from_another_pool() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut producer = InternPool::new();
        let mut supplied = InternPool::new();
        supplied.intern_str("other");
        let mut uir = Uir::new();
        uir.func_bodies
            .push(function(&mut producer, "main", source, 10));
        let uir = uir.with_layout_context(&producer);

        let error = context
            .register_functions(&uir, &supplied)
            .expect_err("UIR must be registered through its producer pool");
        assert!(error.to_string().contains("layout"), "{error}");

        let ids = context.register_functions(&uir, &producer).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(context.declarations().name(ids[0]).unwrap(), "main");
    }

    #[test]
    fn invalid_declaration_names_roll_back() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut other = InternPool::new();
        other.intern_str("padding");
        let foreign_name = other.intern_str("main");
        let mut pool = InternPool::new();
        let mut uir = Uir::new().with_layout_context(&pool);
        uir.func_bodies.push(FuncBody {
            name: foreign_name,
            params: vec![],
            return_type: None,
            body: ExtraRange { offset: 0, len: 0 },
            span: Span::in_source(source, 0, 4),
        });

        let error = context
            .register_functions(&uir, &pool)
            .expect_err("an out-of-range name ID must be rejected");
        assert!(error.to_string().contains("declaration name"));
        uir.func_bodies[0].name = pool.intern_bytes(&[0xff]);
        let error = context
            .register_functions(&uir, &pool)
            .expect_err("a non-UTF-8 declaration name must be rejected");
        assert!(error.to_string().contains("declaration name"));

        uir.func_bodies[0].name = pool.intern_str("main");
        let ids = context.register_functions(&uir, &pool).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].index(), 0);
    }

    #[test]
    fn conflicting_registration_rolls_back() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut pool = InternPool::new();
        let mut uir = Uir::new().with_layout_context(&pool);
        uir.func_bodies
            .push(function(&mut pool, "main", source, 10));
        let original_uir = uir.clone();
        let original = context.register_functions(&uir, &pool).unwrap();
        uir.func_bodies[0].name = pool.intern_str("other");

        let result = context.register_functions(&uir, &pool);
        assert!(matches!(
            result,
            Err(DeclarationError::ConflictingRegistration { .. })
        ));
        assert_eq!(
            context.declarations().get(original[0]).unwrap().name,
            pool.intern_str("main")
        );
        assert_eq!(
            context.register_functions(&original_uir, &pool).unwrap(),
            original
        );
    }

    #[test]
    fn source_revision_allocates_fresh_declarations() {
        let mut context = CompilationContext::new();
        let old_source = context.register_source(PathBuf::from("main.ryo"), "old".into());
        let new_source = context.replace_source(old_source, "new".into()).unwrap();
        let mut pool = InternPool::new();
        let old_uir = {
            let mut uir = Uir::new().with_layout_context(&pool);
            uir.func_bodies
                .push(function(&mut pool, "main", old_source, 10));
            uir
        };
        let old_id = context.register_functions(&old_uir, &pool).unwrap()[0];
        let new_uir = {
            let mut uir = Uir::new().with_layout_context(&pool);
            uir.func_bodies
                .push(function(&mut pool, "main", new_source, 10));
            uir
        };
        let new_id = context.register_functions(&new_uir, &pool).unwrap()[0];

        assert_ne!(old_id, new_id);
        assert_eq!(
            context.declarations().get(old_id).unwrap().span.source,
            old_source
        );
        assert_eq!(
            context.declarations().get(new_id).unwrap().span.source,
            new_source
        );
    }

    #[test]
    fn same_name_in_separate_contexts_is_not_conflated() {
        let mut first = CompilationContext::new();
        let mut second = CompilationContext::new();
        let first_source = first.register_source(PathBuf::from("first.ryo"), "fn same".into());
        let second_source = second.register_source(PathBuf::from("second.ryo"), "fn same".into());
        let mut pool = InternPool::new();
        let first_id = first
            .register_functions(
                &{
                    let mut uir = Uir::new().with_layout_context(&pool);
                    uir.func_bodies
                        .push(function(&mut pool, "same", first_source, 10));
                    uir
                },
                &pool,
            )
            .unwrap()[0];
        let second_id = second
            .register_functions(
                &{
                    let mut uir = Uir::new().with_layout_context(&pool);
                    uir.func_bodies
                        .push(function(&mut pool, "same", second_source, 10));
                    uir
                },
                &pool,
            )
            .unwrap()[0];

        assert_eq!(first_id.index(), second_id.index());
        assert!(second.declarations().get(first_id).is_err());
        assert!(first.declarations().get(second_id).is_err());
        assert_eq!(
            first.declarations().get(first_id).unwrap().name,
            pool.intern_str("same")
        );
        assert_eq!(
            second.declarations().get(second_id).unwrap().name,
            pool.intern_str("same")
        );
    }

    #[test]
    fn name_offset_overflow_returns_error_without_mutation() {
        for base in [u32::MAX as usize, usize::MAX] {
            let mut names = Vec::new();
            let mut bytes = vec![b'x'];

            assert_eq!(
                push_name(&mut names, &mut bytes, base, "main"),
                Err(DeclarationError::NameArenaExhausted)
            );
            assert!(names.is_empty());
            assert_eq!(bytes, vec![b'x']);
        }
    }

    #[test]
    fn unknown_and_none_ids_are_rejected() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut pool = InternPool::new();
        let mut uir = Uir::new().with_layout_context(&pool);
        uir.func_bodies
            .push(function(&mut pool, "main", source, 10));
        let id = context.register_functions(&uir, &pool).unwrap()[0];
        let unknown = DeclId::from_raw_parts(id.raw_parts().0, 999);

        assert!(matches!(
            context.declarations().get(unknown),
            Err(DeclarationError::UnknownDeclaration(_))
        ));
        assert!(context.declarations().get(DeclId::NONE).is_err());
    }

    #[test]
    fn registration_revisits_source_revisions_without_churn() {
        let mut context = CompilationContext::new();
        let old_source = context.register_source(PathBuf::from("main.ryo"), "old".into());
        let new_source = context.replace_source(old_source, "new".into()).unwrap();
        let mut pool = InternPool::new();
        let old_uir = {
            let mut uir = Uir::new().with_layout_context(&pool);
            uir.func_bodies
                .push(function(&mut pool, "main", old_source, 10));
            uir
        };
        let new_uir = {
            let mut uir = Uir::new().with_layout_context(&pool);
            uir.func_bodies
                .push(function(&mut pool, "main", new_source, 10));
            uir
        };
        let old_ids = context.register_functions(&old_uir, &pool).unwrap();
        let new_ids = context.register_functions(&new_uir, &pool).unwrap();

        assert_eq!(
            context.register_functions(&old_uir, &pool).unwrap(),
            old_ids
        );
        assert_eq!(
            context.register_functions(&new_uir, &pool).unwrap(),
            new_ids
        );
    }

    #[test]
    fn conflict_after_new_allocation_rolls_back() {
        let mut context = CompilationContext::new();
        let old_source = context.register_source(PathBuf::from("main.ryo"), "old".into());
        let new_source = context.replace_source(old_source, "new".into()).unwrap();
        let mut pool = InternPool::new();
        let original_uir = {
            let mut uir = Uir::new().with_layout_context(&pool);
            uir.func_bodies
                .push(function(&mut pool, "main", old_source, 10));
            uir.func_bodies
                .push(function(&mut pool, "helper", old_source, 20));
            uir
        };
        let original = context.register_functions(&original_uir, &pool).unwrap();
        let mut conflicting = Uir::new().with_layout_context(&pool);
        conflicting
            .func_bodies
            .push(function(&mut pool, "main", new_source, 10));
        conflicting
            .func_bodies
            .push(function(&mut pool, "other", old_source, 20));

        let table_shape = (
            context.declarations().declarations.len(),
            context.declarations().names.len(),
            context.declarations().name_bytes.len(),
        );
        assert!(matches!(
            context.register_functions(&conflicting, &pool),
            Err(DeclarationError::ConflictingRegistration { uir_index: 1 })
        ));
        assert_eq!(
            table_shape,
            (
                context.declarations().declarations.len(),
                context.declarations().names.len(),
                context.declarations().name_bytes.len(),
            )
        );
        let next_unused = DeclId::from_raw_parts(original[0].raw_parts().0, original.len() as u32);
        assert!(matches!(
            context.declarations().get(next_unused),
            Err(DeclarationError::UnknownDeclaration(_))
        ));
        assert_eq!(
            context.register_functions(&original_uir, &pool).unwrap(),
            original
        );

        conflicting.func_bodies[1].name = pool.intern_str("helper");
        let registered = context.register_functions(&conflicting, &pool).unwrap();
        assert_eq!(registered, vec![next_unused, original[1]]);
        assert_eq!(
            context.declarations().get(next_unused).unwrap().name,
            pool.intern_str("main")
        );
        assert_eq!(
            context.declarations().get(next_unused).unwrap().span.source,
            new_source
        );
    }
}
