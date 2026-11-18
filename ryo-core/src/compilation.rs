use crate::artifact::CompilationContextId;
use crate::declaration::{DeclId, DeclarationError, DeclarationTable};
use crate::source::{SourceDb, SourceError, SourceId};
use crate::types::InternPool;
use crate::uir::Uir;
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

    pub fn register_functions(
        &mut self,
        uir: &Uir,
        pool: &InternPool,
    ) -> Result<Vec<DeclId>, DeclarationError> {
        uir.require_layout(pool)
            .map_err(|_| DeclarationError::InvalidLayout)?;
        for (uir_index, body) in uir.func_bodies.iter().enumerate() {
            if body.span.source != SourceId::NONE {
                self.sources
                    .get(body.span.source)
                    .map_err(DeclarationError::InvalidSource)?;
            }
            let valid_name = (body.name.raw() as usize) < pool.string_count()
                && std::str::from_utf8(pool.bytes_payload(body.name)).is_ok();
            if !valid_name {
                return Err(DeclarationError::InvalidName { uir_index });
            }
        }
        self.declarations.register_functions(uir, pool)
    }
}

impl Default for CompilationContext {
    fn default() -> Self {
        Self::new()
    }
}
