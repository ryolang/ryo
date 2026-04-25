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
use std::collections::hash_map::Entry;
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
    /// Sentinel for resolution failure. Sema substitutes this in for
    /// any expression whose type could not be determined (unknown
    /// variable, unknown type annotation, etc.) so that downstream
    /// checks keep running without cascading. Type comparisons treat
    /// `Error` as compatible with anything; see
    /// [`InternPool::is_error`] / [`InternPool::compatible`].
    Error,
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
    error: TypeId,
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
            error: TypeId(0),
        };
        pool.void = pool.intern(TypeKind::Void);
        pool.bool_ = pool.intern(TypeKind::Bool);
        pool.int = pool.intern(TypeKind::Int);
        pool.str_ = pool.intern(TypeKind::Str);
        pool.error = pool.intern(TypeKind::Error);
        pool
    }

    pub fn intern(&mut self, k: TypeKind) -> TypeId {
        match self.dedup.entry(k) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                let id = TypeId(
                    u32::try_from(self.kinds.len())
                        .expect("type pool overflow: more than u32::MAX types interned"),
                );
                self.kinds.push(e.key().clone());
                e.insert(id);
                id
            }
        }
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
    /// Sentinel returned when type resolution fails. See
    /// [`TypeKind::Error`] for semantics.
    pub fn error_type(&self) -> TypeId {
        self.error
    }

    /// Whether `id` is the resolution-failure sentinel.
    pub fn is_error(&self, id: TypeId) -> bool {
        id == self.error
    }

    /// Compatibility predicate that absorbs the `Error` sentinel.
    ///
    /// Sema uses this anywhere it would otherwise emit a
    /// type-mismatch diagnostic, so a single failure upstream
    /// doesn't cascade into a wave of follow-on errors.
    pub fn compatible(&self, a: TypeId, b: TypeId) -> bool {
        a == b || self.is_error(a) || self.is_error(b)
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
            TypeKind::Error => write!(f, "<error>"),
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

    #[test]
    fn error_type_is_distinct_from_primitives() {
        let pool = InternPool::new();
        let err = pool.error_type();
        assert_ne!(err, pool.void());
        assert_ne!(err, pool.bool_());
        assert_ne!(err, pool.int());
        assert_ne!(err, pool.str_());
        // Stable across repeated calls.
        assert_eq!(err, pool.error_type());
    }

    #[test]
    fn is_error_only_true_for_error_sentinel() {
        let pool = InternPool::new();
        assert!(pool.is_error(pool.error_type()));
        assert!(!pool.is_error(pool.int()));
        assert!(!pool.is_error(pool.bool_()));
        assert!(!pool.is_error(pool.void()));
        assert!(!pool.is_error(pool.str_()));
    }

    #[test]
    fn compatible_is_reflexive_and_distinguishes_primitives() {
        let pool = InternPool::new();
        assert!(pool.compatible(pool.int(), pool.int()));
        assert!(pool.compatible(pool.bool_(), pool.bool_()));
        assert!(!pool.compatible(pool.int(), pool.bool_()));
        assert!(!pool.compatible(pool.str_(), pool.int()));
    }

    #[test]
    fn compatible_absorbs_error_sentinel_in_either_position() {
        let pool = InternPool::new();
        let err = pool.error_type();
        // Symmetry: Error compatible with every primitive on both sides.
        for &t in &[pool.int(), pool.bool_(), pool.str_(), pool.void()] {
            assert!(pool.compatible(err, t), "Error vs {:?}", pool.kind(t));
            assert!(pool.compatible(t, err), "{:?} vs Error", pool.kind(t));
        }
        // Error vs Error is also compatible (trivially via reflexivity).
        assert!(pool.compatible(err, err));
    }
}
