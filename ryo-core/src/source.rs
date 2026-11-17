use crate::artifact::CompilationContextId;
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceId {
    context: CompilationContextId,
    revision: u32,
}

impl SourceId {
    pub const NONE: Self = Self {
        context: CompilationContextId::NONE,
        revision: u32::MAX,
    };

    #[cfg(test)]
    pub(crate) const fn raw_parts(self) -> (u32, u32) {
        (self.context.raw(), self.revision)
    }

    #[cfg(test)]
    pub(crate) const fn from_raw_parts(context: u32, revision: u32) -> Self {
        Self {
            context: CompilationContextId::from_raw(context),
            revision,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceError {
    WrongContext {
        expected: CompilationContextId,
        actual: CompilationContextId,
    },
    UnknownRevision(SourceId),
    StaleRevision(SourceId),
    InvalidRange {
        source: SourceId,
        start: usize,
        end: usize,
    },
    RevisionExhausted,
}

#[derive(Debug)]
pub struct Source {
    id: SourceId,
    name: PathBuf,
    text: String,
    lineage: usize,
}

impl Source {
    pub fn id(&self) -> SourceId {
        self.id
    }

    pub fn name(&self) -> &Path {
        &self.name
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Debug)]
pub struct SourceDb {
    context: CompilationContextId,
    revisions: Vec<Source>,
    current: Vec<SourceId>,
}

impl SourceDb {
    pub(crate) fn new(context: CompilationContextId) -> Self {
        Self {
            context,
            revisions: Vec::new(),
            current: Vec::new(),
        }
    }

    pub(crate) fn register(&mut self, name: PathBuf, text: String) -> SourceId {
        let revision = u32::try_from(self.revisions.len()).expect("source revision space exhausted");
        let id = SourceId {
            context: self.context,
            revision,
        };
        let lineage = self.current.len();
        self.revisions.push(Source {
            id,
            name,
            text,
            lineage,
        });
        self.current.push(id);
        id
    }

    pub(crate) fn replace(
        &mut self,
        previous: SourceId,
        text: String,
    ) -> Result<SourceId, SourceError> {
        let (lineage, name) = {
            let source = self.get(previous)?;
            if self.current.get(source.lineage) != Some(&previous) {
                return Err(SourceError::StaleRevision(previous));
            }
            (source.lineage, source.name.clone())
        };
        let revision = u32::try_from(self.revisions.len())
            .map_err(|_| SourceError::RevisionExhausted)?;
        let id = SourceId {
            context: self.context,
            revision,
        };
        let replacement = Source {
            id,
            name,
            text,
            lineage,
        };
        self.revisions.push(replacement);
        self.current[lineage] = id;
        Ok(id)
    }

    pub fn get(&self, id: SourceId) -> Result<&Source, SourceError> {
        self.validate_context(id)?;
        self.revisions
            .get(id.revision as usize)
            .filter(|source| source.id == id)
            .ok_or(SourceError::UnknownRevision(id))
    }

    pub fn require_current(&self, id: SourceId) -> Result<&Source, SourceError> {
        let source = self.get(id)?;
        if self.current.get(source.lineage) == Some(&id) {
            Ok(source)
        } else {
            Err(SourceError::StaleRevision(id))
        }
    }

    pub fn validate_range(&self, id: SourceId, range: Range<usize>) -> Result<(), SourceError> {
        let source = self.get(id)?;
        let valid = range.start <= range.end
            && range.end <= source.text.len()
            && source.text.is_char_boundary(range.start)
            && source.text.is_char_boundary(range.end);
        if valid {
            Ok(())
        } else {
            Err(SourceError::InvalidRange {
                source: id,
                start: range.start,
                end: range.end,
            })
        }
    }

    fn validate_context(&self, id: SourceId) -> Result<(), SourceError> {
        if id.context == self.context {
            Ok(())
        } else {
            Err(SourceError::WrongContext {
                expected: self.context,
                actual: id.context,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SourceId;
    use crate::compilation::CompilationContext;
    use std::path::{Path, PathBuf};

    #[test]
    fn registration_owns_text_and_current_lookup() {
        let mut context = CompilationContext::new();
        let id = context.register_source(PathBuf::from("main.ryo"), "fn main():\n\tpass\n".into());

        let source = context.sources().require_current(id).unwrap();
        assert_eq!(source.id(), id);
        assert_eq!(source.name(), Path::new("main.ryo"));
        assert_eq!(source.text(), "fn main():\n\tpass\n");
    }

    #[test]
    fn replacement_retains_history_and_advances_current() {
        let mut context = CompilationContext::new();
        let old_id = context.register_source(PathBuf::from("main.ryo"), "old".into());
        let new_id = context.replace_source(old_id, "new".into()).unwrap();

        assert_ne!(old_id, new_id);
        assert_eq!(context.sources().get(old_id).unwrap().text(), "old");
        assert!(context.sources().require_current(old_id).is_err());
        assert_eq!(context.sources().require_current(new_id).unwrap().text(), "new");
    }

    #[test]
    fn stale_replacement_fails_without_mutation() {
        let mut context = CompilationContext::new();
        let old_id = context.register_source(PathBuf::from("main.ryo"), "old".into());
        let current_id = context.replace_source(old_id, "current".into()).unwrap();

        assert!(context.replace_source(old_id, "stale write".into()).is_err());
        assert_eq!(context.sources().get(old_id).unwrap().text(), "old");
        assert_eq!(
            context.sources().require_current(current_id).unwrap().text(),
            "current"
        );
        assert_eq!(context.sources().get(current_id).unwrap().text(), "current");
    }

    #[test]
    fn wrong_context_with_same_local_index_is_rejected() {
        let mut first = CompilationContext::new();
        let mut second = CompilationContext::new();
        let first_id = first.register_source(PathBuf::from("first.ryo"), "first".into());
        let second_id = second.register_source(PathBuf::from("second.ryo"), "second".into());

        assert_eq!(first_id.raw_parts().1, second_id.raw_parts().1);
        assert!(second.sources().get(first_id).is_err());
        assert!(second.sources().require_current(first_id).is_err());
        assert!(second.sources().validate_range(first_id, 0..1).is_err());
        assert_eq!(second.sources().get(second_id).unwrap().text(), "second");
    }

    #[test]
    fn unknown_revision_is_rejected() {
        let mut context = CompilationContext::new();
        let id = context.register_source(PathBuf::from("main.ryo"), "text".into());
        let (context_id, _) = id.raw_parts();
        let unknown = SourceId::from_raw_parts(context_id, u32::MAX);

        assert!(context.sources().get(unknown).is_err());
    }

    #[test]
    fn source_less_range_is_rejected() {
        let context = CompilationContext::new();
        assert!(context.sources().validate_range(SourceId::NONE, 0..0).is_err());
    }

    #[test]
    fn reversed_range_is_rejected() {
        let mut context = CompilationContext::new();
        let id = context.register_source(PathBuf::from("main.ryo"), "text".into());
        assert!(context.sources().validate_range(id, 3..2).is_err());
    }

    #[test]
    fn out_of_bounds_range_is_rejected() {
        let mut context = CompilationContext::new();
        let id = context.register_source(PathBuf::from("main.ryo"), "text".into());
        assert!(context.sources().validate_range(id, 0..5).is_err());
    }

    #[test]
    fn range_must_use_utf8_boundaries() {
        let mut context = CompilationContext::new();
        let id = context.register_source(PathBuf::from("main.ryo"), "😀x".into());

        assert!(context.sources().validate_range(id, 0..4).is_ok());
        assert!(context.sources().validate_range(id, 4..5).is_ok());
        assert!(context.sources().validate_range(id, 1..4).is_err());
        assert!(context.sources().validate_range(id, 0..2).is_err());
    }
}
