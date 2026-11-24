use crate::span::Span;
use crate::tir::ParamMode;
use crate::types::{StringId, TypeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BindingId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScopeId(u32);

impl BindingId {
    pub const fn from_index(index: usize) -> Self {
        assert!(
            index <= u32::MAX as usize,
            "BindingId index out of range (>= 2^32)"
        );
        Self(index as u32)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl ScopeId {
    pub const fn from_index(index: usize) -> Self {
        assert!(
            index <= u32::MAX as usize,
            "ScopeId index out of range (>= 2^32)"
        );
        Self(index as u32)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug)]
pub enum BindingKind {
    Param(ParamMode),
    Local { mutable: bool, borrowed: bool },
    Temporary,
}

#[derive(Debug)]
pub struct BindingInfo {
    pub name: StringId,
    pub ty: TypeId,
    pub kind: BindingKind,
    pub scope: ScopeId,
    pub span: Span,
}

impl BindingInfo {
    pub fn new(name: StringId, ty: TypeId, kind: BindingKind, scope: ScopeId, span: Span) -> Self {
        Self {
            name,
            ty,
            kind,
            scope,
            span,
        }
    }
}

pub struct BindingTable {
    bindings: Vec<BindingInfo>,
}

impl BindingTable {
    pub fn new(bindings: Vec<BindingInfo>) -> Self {
        Self { bindings }
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn get(&self, id: BindingId) -> Option<&BindingInfo> {
        self.bindings.get(id.index())
    }

    pub fn require(&self, id: BindingId) -> Result<&BindingInfo, String> {
        self.get(id)
            .ok_or_else(|| format!("unregistered binding {}", id.raw()))
    }
}
