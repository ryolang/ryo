use crate::artifact::CompilationContextId;
use crate::declaration::DeclarationTable;
use crate::source::{SourceDb, SourceError, SourceId};
use std::path::PathBuf;

#[derive(Debug)]
pub struct CompilationContext {
    sources: SourceDb,
    declarations: DeclarationTable,
}

impl CompilationContext {
    pub fn new() -> Self {
        let context_id = CompilationContextId::fresh();
        Self {
            sources: SourceDb::new(context_id),
            declarations: DeclarationTable::new(context_id),
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

    pub fn declarations(&self) -> &DeclarationTable {
        &self.declarations
    }

    pub fn declarations_mut(&mut self) -> &mut DeclarationTable {
        &mut self.declarations
    }
}

impl Default for CompilationContext {
    fn default() -> Self {
        Self::new()
    }
}
