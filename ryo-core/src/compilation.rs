use crate::artifact::CompilationContextId;
use crate::source::{SourceDb, SourceError, SourceId};
use std::path::PathBuf;

#[derive(Debug)]
pub struct CompilationContext {
    sources: SourceDb,
}

impl CompilationContext {
    pub fn new() -> Self {
        Self {
            sources: SourceDb::new(CompilationContextId::fresh()),
        }
    }

    pub fn register_source(&mut self, name: PathBuf, text: String) -> SourceId {
        self.sources.register(name, text)
    }

    pub fn replace_source(
        &mut self,
        previous: SourceId,
        text: String,
    ) -> Result<SourceId, SourceError> {
        self.sources.replace(previous, text)
    }

    pub fn sources(&self) -> &SourceDb {
        &self.sources
    }
}

impl Default for CompilationContext {
    fn default() -> Self {
        Self::new()
    }
}
