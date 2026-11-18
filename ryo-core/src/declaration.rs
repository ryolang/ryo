use crate::artifact::CompilationContextId;
#[cfg(test)]
use crate::source::SourceId;
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

    #[cfg(test)]
    pub(crate) const fn raw_parts(self) -> (u32, u32) {
        (self.context.raw(), self.index)
    }

    #[cfg(test)]
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
    pub name: StringId,
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
        }
    }
}

impl std::error::Error for DeclarationError {}

#[derive(Debug)]
pub struct DeclarationTable {
    context: CompilationContextId,
    declarations: Vec<FunctionDecl>,
    by_uir_index: HashMap<usize, DeclId>,
}

impl DeclarationTable {
    pub(crate) fn new(context: CompilationContextId) -> Self {
        Self {
            context,
            declarations: Vec::new(),
            by_uir_index: HashMap::new(),
        }
    }

    pub fn register_functions(&mut self, uir: &Uir) -> Result<Vec<DeclId>, DeclarationError> {
        let mut ids = Vec::with_capacity(uir.func_bodies.len());
        for (uir_index, body) in uir.func_bodies.iter().enumerate() {
            if let Some(existing) = self.by_uir_index.get(&uir_index).copied() {
                let declaration = &self.declarations[existing.index()];
                if declaration.name == body.name && declaration.span == body.span {
                    ids.push(existing);
                    continue;
                }
                if declaration.span.source == body.span.source {
                    return Err(DeclarationError::ConflictingRegistration { uir_index });
                }
            }
            let id = DeclId {
                context: self.context,
                index: u32::try_from(self.declarations.len())
                    .expect("declaration index space exhausted"),
            };
            self.declarations.push(FunctionDecl {
                id,
                name: body.name,
                uir_index,
                span: body.span,
            });
            self.by_uir_index.insert(uir_index, id);
            ids.push(id);
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
            return_type: pool.void(),
            body: ExtraRange { offset: 0, len: 0 },
            span: Span::in_source(source, start, start + 4),
        }
    }

    #[test]
    fn allocation_is_source_ordered_unique_and_preserves_spans() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut pool = InternPool::new();
        let mut uir = Uir::new();
        uir.func_bodies
            .push(function(&mut pool, "main", source, 10));
        uir.func_bodies
            .push(function(&mut pool, "main", source, 20));

        let ids = context.declarations_mut().register_functions(&uir).unwrap();

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
        let mut uir = Uir::new();
        uir.func_bodies
            .push(function(&mut pool, "main", source, 10));
        let first = context.declarations_mut().register_functions(&uir).unwrap();

        let second = context.declarations_mut().register_functions(&uir).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn conflicting_registration_rolls_back() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut pool = InternPool::new();
        let mut uir = Uir::new();
        uir.func_bodies
            .push(function(&mut pool, "main", source, 10));
        let original_uir = uir.clone();
        let original = context.declarations_mut().register_functions(&uir).unwrap();
        uir.func_bodies[0].name = pool.intern_str("other");

        let result = context.declarations_mut().register_functions(&uir);
        assert!(matches!(
            result,
            Err(DeclarationError::ConflictingRegistration { .. })
        ));
        assert_eq!(
            context.declarations().get(original[0]).unwrap().name,
            pool.intern_str("main")
        );
        assert_eq!(
            context
                .declarations_mut()
                .register_functions(&original_uir)
                .unwrap(),
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
            let mut uir = Uir::new();
            uir.func_bodies
                .push(function(&mut pool, "main", old_source, 10));
            uir
        };
        let old_id = context
            .declarations_mut()
            .register_functions(&old_uir)
            .unwrap()[0];
        let new_uir = {
            let mut uir = Uir::new();
            uir.func_bodies
                .push(function(&mut pool, "main", new_source, 10));
            uir
        };
        let new_id = context
            .declarations_mut()
            .register_functions(&new_uir)
            .unwrap()[0];

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
            .declarations_mut()
            .register_functions(&{
                let mut uir = Uir::new();
                uir.func_bodies
                    .push(function(&mut pool, "same", first_source, 10));
                uir
            })
            .unwrap()[0];
        let second_id = second
            .declarations_mut()
            .register_functions(&{
                let mut uir = Uir::new();
                uir.func_bodies
                    .push(function(&mut pool, "same", second_source, 10));
                uir
            })
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
    fn unknown_and_none_ids_are_rejected() {
        let mut context = CompilationContext::new();
        let source = context.register_source(PathBuf::from("main.ryo"), "fn main".into());
        let mut pool = InternPool::new();
        let mut uir = Uir::new();
        uir.func_bodies
            .push(function(&mut pool, "main", source, 10));
        let id = context.declarations_mut().register_functions(&uir).unwrap()[0];
        let unknown = DeclId::from_raw_parts(id.raw_parts().0, 999);

        assert!(matches!(
            context.declarations().get(unknown),
            Err(DeclarationError::UnknownDeclaration(_))
        ));
        assert!(context.declarations().get(DeclId::NONE).is_err());
    }
}
