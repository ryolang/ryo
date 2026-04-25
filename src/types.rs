//! Interned type system for the middle-end.
//!
//! `TypeId` is a `Copy` handle into an `InternPool` that owns the
//! canonical `TypeKind` for each distinct type in the program.
//!
//! The HIR and later stages store `TypeId` values; `TypeKind` is only
//! inspected via `InternPool::kind` when the shape of the type matters
//! (e.g. when lowering to Cranelift or formatting diagnostics).
//!
//! Only primitive variants of `TypeKind` are constructed today. The
//! commented-out variants are reserved so that adding structs, enums,
//! tuples, `?T`, `!T`, or function types later is a pure additive
//! change to this module instead of another rewrite of every consumer.

use std::collections::HashMap;
use std::fmt;

/// A compact, copyable handle to an interned type.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TypeId(u32);

/// Canonical structural description of a type.
///
/// Currently only primitive variants are constructed. Reserved variants
/// (documented below) are intentionally left out rather than stubbed so
/// that exhaustiveness checks catch every consumer when a new variant
/// is added:
///
/// - `Struct(StructId)`
/// - `Enum(EnumId)`
/// - `Tuple(Box<[TypeId]>)`
/// - `Option(TypeId)`
/// - `ErrorUnion { ok: TypeId, err: ErrSetId }`
/// - `Func { params: Box<[TypeId]>, ret: TypeId }`
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TypeKind {
    Void,
    Bool,
    /// Machine-word signed integer; width parameterization is deferred.
    Int,
    /// Placeholder; the slice ABI is a separate change.
    Str,
}

/// Canonical type pool. One long-lived instance per compile, threaded
/// explicitly through the pipeline — no globals, no thread-locals.
#[derive(Debug)]
pub struct InternPool {
    kinds: Vec<TypeKind>,
    dedup: HashMap<TypeKind, TypeId>,
    // Cached primitive ids (populated in `new`) so hot paths never hash.
    void: TypeId,
    bool_: TypeId,
    int: TypeId,
    str_: TypeId,
}

impl InternPool {
    pub fn new() -> Self {
        let mut pool = Self {
            kinds: Vec::with_capacity(4),
            dedup: HashMap::new(),
            void: TypeId(0),
            bool_: TypeId(0),
            int: TypeId(0),
            str_: TypeId(0),
        };
        pool.void = pool.intern(TypeKind::Void);
        pool.bool_ = pool.intern(TypeKind::Bool);
        pool.int = pool.intern(TypeKind::Int);
        pool.str_ = pool.intern(TypeKind::Str);
        pool
    }

    pub fn intern(&mut self, k: TypeKind) -> TypeId {
        if let Some(&id) = self.dedup.get(&k) {
            return id;
        }
        let id = TypeId(self.kinds.len() as u32);
        self.kinds.push(k.clone());
        self.dedup.insert(k, id);
        id
    }

    pub fn kind(&self, id: TypeId) -> &TypeKind {
        &self.kinds[id.0 as usize]
    }

    /// Returns a `Display` adapter that renders `id` using `self`.
    ///
    /// `TypeId` deliberately does not implement `Display` on its own,
    /// so callers cannot accidentally stringify an id without the pool.
    pub fn display(&self, id: TypeId) -> DisplayType<'_> {
        DisplayType { pool: self, id }
    }

    pub fn void(&self) -> TypeId {
        self.void
    }
    pub fn bool_(&self) -> TypeId {
        self.bool_
    }
    pub fn int(&self) -> TypeId {
        self.int
    }
    pub fn str_(&self) -> TypeId {
        self.str_
    }
}

impl Default for InternPool {
    fn default() -> Self {
        Self::new()
    }
}

pub struct DisplayType<'a> {
    pool: &'a InternPool,
    id: TypeId,
}

impl fmt::Display for DisplayType<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pool.kind(self.id) {
            TypeKind::Void => write!(f, "void"),
            TypeKind::Bool => write!(f, "bool"),
            TypeKind::Int => write!(f, "int"),
            TypeKind::Str => write!(f, "str"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_same_id_for_same_kind() {
        let mut pool = InternPool::new();
        let a = pool.intern(TypeKind::Int);
        let b = pool.intern(TypeKind::Int);
        assert_eq!(a, b);
    }

    #[test]
    fn primitives_have_stable_ids() {
        let pool = InternPool::new();
        assert_ne!(pool.void(), pool.bool_());
        assert_ne!(pool.int(), pool.str_());
        assert_ne!(pool.void(), pool.int());
    }

    #[test]
    fn primitive_accessors_match_interning() {
        let mut pool = InternPool::new();
        assert_eq!(pool.void(), pool.intern(TypeKind::Void));
        assert_eq!(pool.bool_(), pool.intern(TypeKind::Bool));
        assert_eq!(pool.int(), pool.intern(TypeKind::Int));
        assert_eq!(pool.str_(), pool.intern(TypeKind::Str));
    }

    #[test]
    fn display_round_trips() {
        let pool = InternPool::new();
        assert_eq!(format!("{}", pool.display(pool.int())), "int");
        assert_eq!(format!("{}", pool.display(pool.str_())), "str");
        assert_eq!(format!("{}", pool.display(pool.void())), "void");
        assert_eq!(format!("{}", pool.display(pool.bool_())), "bool");
    }

    #[test]
    fn kind_lookup_matches_insertion() {
        let pool = InternPool::new();
        assert_eq!(pool.kind(pool.int()), &TypeKind::Int);
        assert_eq!(pool.kind(pool.bool_()), &TypeKind::Bool);
        assert_eq!(pool.kind(pool.str_()), &TypeKind::Str);
        assert_eq!(pool.kind(pool.void()), &TypeKind::Void);
    }
}
