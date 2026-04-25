//! Interned type and string pool, modelled on Zig's `InternPool.zig`.
//!
//! Storage shape:
//! - `items: Vec<Item>` — fixed-size `(tag, data)` pairs, one per
//!   interned type. `data` is either an inline payload (no payload
//!   for primitives, type id otherwise) or an index into `extra`.
//! - `extra: Vec<u32>` — variable-size payloads (tuple element
//!   lists, future function signatures). Dedup hashes the *content*
//!   of the extra range via `hashbrown::HashTable`, not a cloned
//!   `Box<[TypeId]>` — so adding a new variable-payload variant
//!   doesn't pay a clone-per-intern cost.
//! - String dedup uses the same `HashTable<StringId>` shape: the
//!   table stores only the handle and probes via the arena view of
//!   the bytes, so we don't carry the bytes a second time as owned
//!   `String` keys.
//! - `string_bytes: Vec<u8>` + `strings: Vec<(u32, u32)>` — a single
//!   byte arena holds every interned identifier and string literal,
//!   with `(offset, len)` pairs keyed by `StringId`. This is what
//!   lets later phases drop `&'a str` slices from `Token` and
//!   shrink HIR's `String` count.
//!
//! Primitive types live at fixed item indices (0..=4), populated by
//! `new()`. The `const fn` accessors (`void`, `bool_`, `int`, ...)
//! return those indices without consulting the dedup table — hot
//! paths never hash.
//!
//! `TypeId` stays a plain `Copy` newtype rather than an `enum(u32)`
//! with named primitive variants. The doc's risk register flagged
//! that the typed-enum encoding fights Rust's borrow checker; the
//! newtype keeps every other payoff and we can revisit if the
//! exhaustiveness loss bites later.

use hashbrown::{DefaultHashBuilder, HashTable};
use std::fmt;
use std::hash::{BuildHasher, Hasher};

// ---------- TypeId ----------

/// A compact, copyable handle to an interned type.
///
/// Primitive ids are stable: `TypeId(0..=4)` are `void`, `bool`,
/// `int`, `str`, `error`. Use the `const fn` accessors on
/// `InternPool` instead of constructing these directly.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TypeId(u32);

impl TypeId {
    /// Internal handle value. Exposed for ZIR/AIR encoding (Phase 3+)
    /// where instructions reference types as raw u32s alongside
    /// other ids in the `extra` arena.
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Reconstruct a `TypeId` from a raw `u32` previously produced by
    /// [`Self::raw`]. Used by UIR/TIR decoders that pull packed
    /// payloads back out of the `extra` arena.
    pub const fn from_raw(raw: u32) -> Self {
        TypeId(raw)
    }
}

// ---------- StringId ----------

/// Handle to an interned UTF-8 byte sequence in the InternPool.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct StringId(u32);

impl StringId {
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Reconstruct a `StringId` from a raw `u32` previously produced
    /// by [`Self::raw`]. Used by UIR/TIR decoders that pull packed
    /// payloads back out of the `extra` arena.
    pub const fn from_raw(raw: u32) -> Self {
        StringId(raw)
    }
}

// ---------- Internal storage ----------

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
enum Tag {
    Void,
    Bool,
    Int,
    Str,
    Error,
    /// Variable payload: `data` is the index into `extra` of an
    /// `(n_elems: u32, elem_0: u32, ..., elem_{n-1}: u32)` block.
    Tuple,
    // Reserved for later phases — not constructed today:
    //   Func, Struct, Enum, Option, ErrorUnion.
    // Adding any of those is a new `Tag` variant and a new arm in
    // `kind`/`Display`; storage shape is already in place.
}

#[derive(Copy, Clone, Debug)]
struct Item {
    tag: Tag,
    /// For primitives, ignored. For Tuple, an index into `extra`.
    data: u32,
}

// Stable indices for primitive types. `new()` interns them in this
// order so the `const fn` accessors below can return them directly.
const ID_VOID: u32 = 0;
const ID_BOOL: u32 = 1;
const ID_INT: u32 = 2;
const ID_STR: u32 = 3;
const ID_ERROR: u32 = 4;

// ---------- Public TypeKind facade ----------

/// Payload-free kind discriminator.
///
/// Variable-payload variants (Tuple) carry no inline data here;
/// callers fetch element lists via `InternPool::tuple_elements`.
/// This shape mirrors Zig's `Type.Tag` and keeps the `kind` accessor
/// allocation-free.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TypeKind {
    Void,
    Bool,
    /// Machine-word signed integer; width parameterization deferred.
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
    /// Variable-arity tuple. Reserved variant proving the
    /// sidecar-`extra` encoding works; not currently constructible
    /// from user syntax.
    Tuple,
}

// ---------- Pool ----------

/// Interned types and strings, modelled on Zig's `InternPool.zig`.
///
/// Both dedup tables are `hashbrown::HashTable<Handle>` rather than
/// `HashMap<Key, Handle>` — they store only the handle (`TypeId` /
/// `StringId`) and recover the key bytes from the arena (`extra`,
/// `string_bytes`) on probe. This lets `intern_str` and `tuple`
/// look up by `&str` / `&[TypeId]` without allocating a Vec or
/// String on every call.
#[derive(Debug)]
pub struct InternPool {
    items: Vec<Item>,
    extra: Vec<u32>,
    /// Dedup for variable-payload types. Stores `TypeId`s; equality
    /// and hash are computed by reading the matching slice out of
    /// `extra` (see `tuple_eq` / `tuple_hash`).
    type_dedup: HashTable<TypeId>,

    string_bytes: Vec<u8>,
    /// `(offset, len)` into `string_bytes`, indexed by `StringId`.
    strings: Vec<(u32, u32)>,
    /// Dedup for interned strings. Stores `StringId`s; equality and
    /// hash are computed by reading the matching byte range out of
    /// `string_bytes`.
    string_dedup: HashTable<StringId>,

    /// Single shared `BuildHasher` so probe-time and resize-time
    /// hashes match. `DefaultHashBuilder` is hashbrown's default
    /// (foldhash); fine for a compiler with no DoS surface.
    hasher: DefaultHashBuilder,
}

fn hash_bytes(hasher: &DefaultHashBuilder, bytes: &[u8]) -> u64 {
    hasher.hash_one(bytes)
}

fn hash_typeids(hasher: &DefaultHashBuilder, ids: &[TypeId]) -> u64 {
    // Hash the underlying u32 sequence directly so the same hash
    // is reachable from a `&[TypeId]` query and from rehashing
    // a `&[u32]` slice read out of `extra` during table resize.
    let mut h = hasher.build_hasher();
    for id in ids {
        h.write_u32(id.0);
    }
    h.finish()
}

/// Read a tuple `TypeId`'s element slice out of `extra`.
///
/// Returns `None` if `id` does not point at a `Tag::Tuple` item;
/// callers in the dedup-probe path use `None` as "not equal".
fn tuple_extra_slice<'a>(items: &[Item], extra: &'a [u32], id: TypeId) -> Option<&'a [u32]> {
    let item = items[id.0 as usize];
    if !matches!(item.tag, Tag::Tuple) {
        return None;
    }
    let start = item.data as usize;
    let n = extra[start] as usize;
    Some(&extra[start + 1..start + 1 + n])
}

fn tuple_eq(items: &[Item], extra: &[u32], candidate: TypeId, query: &[TypeId]) -> bool {
    let Some(stored) = tuple_extra_slice(items, extra, candidate) else {
        return false;
    };
    if stored.len() != query.len() {
        return false;
    }
    stored.iter().zip(query).all(|(&raw, q)| raw == q.0)
}

fn tuple_hash(
    items: &[Item],
    extra: &[u32],
    hasher: &DefaultHashBuilder,
    candidate: TypeId,
) -> u64 {
    let stored =
        tuple_extra_slice(items, extra, candidate).expect("non-tuple in tuple dedup table");
    let mut h = hasher.build_hasher();
    for raw in stored {
        h.write_u32(*raw);
    }
    h.finish()
}

fn str_bytes<'a>(strings: &[(u32, u32)], string_bytes: &'a [u8], id: StringId) -> &'a [u8] {
    let (offset, len) = strings[id.0 as usize];
    &string_bytes[offset as usize..(offset + len) as usize]
}

fn str_eq(strings: &[(u32, u32)], string_bytes: &[u8], candidate: StringId, query: &[u8]) -> bool {
    str_bytes(strings, string_bytes, candidate) == query
}

fn str_hash(
    strings: &[(u32, u32)],
    string_bytes: &[u8],
    hasher: &DefaultHashBuilder,
    candidate: StringId,
) -> u64 {
    hash_bytes(hasher, str_bytes(strings, string_bytes, candidate))
}

impl InternPool {
    pub fn new() -> Self {
        let mut pool = Self {
            items: Vec::with_capacity(8),
            extra: Vec::new(),
            type_dedup: HashTable::new(),
            string_bytes: Vec::new(),
            strings: Vec::new(),
            string_dedup: HashTable::new(),
            hasher: DefaultHashBuilder::default(),
        };
        // Order matters: must match ID_VOID..ID_ERROR.
        pool.items.push(Item {
            tag: Tag::Void,
            data: 0,
        });
        pool.items.push(Item {
            tag: Tag::Bool,
            data: 0,
        });
        pool.items.push(Item {
            tag: Tag::Int,
            data: 0,
        });
        pool.items.push(Item {
            tag: Tag::Str,
            data: 0,
        });
        pool.items.push(Item {
            tag: Tag::Error,
            data: 0,
        });
        debug_assert!(pool.items.len() == (ID_ERROR + 1) as usize);
        pool
    }

    // ----- Type accessors -----

    pub fn kind(&self, id: TypeId) -> TypeKind {
        match self.items[id.0 as usize].tag {
            Tag::Void => TypeKind::Void,
            Tag::Bool => TypeKind::Bool,
            Tag::Int => TypeKind::Int,
            Tag::Str => TypeKind::Str,
            Tag::Error => TypeKind::Error,
            Tag::Tuple => TypeKind::Tuple,
        }
    }

    pub const fn void(&self) -> TypeId {
        TypeId(ID_VOID)
    }
    pub const fn bool_(&self) -> TypeId {
        TypeId(ID_BOOL)
    }
    pub const fn int(&self) -> TypeId {
        TypeId(ID_INT)
    }
    pub const fn str_(&self) -> TypeId {
        TypeId(ID_STR)
    }
    pub const fn error_type(&self) -> TypeId {
        TypeId(ID_ERROR)
    }

    pub fn is_error(&self, id: TypeId) -> bool {
        id.0 == ID_ERROR
    }

    /// Compatibility predicate that absorbs the `Error` sentinel.
    /// Used anywhere sema would otherwise emit a type-mismatch.
    pub fn compatible(&self, a: TypeId, b: TypeId) -> bool {
        a == b || self.is_error(a) || self.is_error(b)
    }

    /// Intern a tuple type. Dedups on element-id sequence with no
    /// per-call key allocation: the probe hashes `elems` directly
    /// and compares it against each candidate's slice in `extra`.
    #[allow(dead_code)]
    pub fn tuple(&mut self, elems: &[TypeId]) -> TypeId {
        let hash = hash_typeids(&self.hasher, elems);

        // Probe path: fully immutable. Borrow `items` and `extra`
        // through `self` for the closure.
        let items = &self.items;
        let extra = &self.extra;
        if let Some(&id) = self
            .type_dedup
            .find(hash, |&candidate| tuple_eq(items, extra, candidate, elems))
        {
            return id;
        }

        // Insert path: append the payload to `extra`, allocate a
        // fresh `TypeId`, then register it with the dedup table.
        let extra_idx = u32::try_from(self.extra.len())
            .expect("extra arena overflow: more than u32::MAX u32 entries");
        self.extra.push(elems.len() as u32);
        for e in elems {
            self.extra.push(e.0);
        }
        let id = TypeId(
            u32::try_from(self.items.len())
                .expect("type pool overflow: more than u32::MAX types interned"),
        );
        self.items.push(Item {
            tag: Tag::Tuple,
            data: extra_idx,
        });

        // Disjoint-field reborrows so the resize-time hasher
        // closure can read `items`/`extra` while `type_dedup` is
        // mutably borrowed.
        let items = &self.items;
        let extra = &self.extra;
        let hasher = &self.hasher;
        self.type_dedup.insert_unique(hash, id, |&candidate| {
            tuple_hash(items, extra, hasher, candidate)
        });
        id
    }

    /// Copy out a tuple type's element list.
    ///
    /// Returns by value rather than by `&[TypeId]` because element
    /// ids are stored as raw `u32`s in the `extra` arena;
    /// reinterpreting that as `&[TypeId]` requires
    /// `#[repr(transparent)]` on `TypeId` plus an unsafe transmute.
    /// Only used today by `Display` for diagnostic formatting and
    /// by tests, neither of which is hot. Tracked as a follow-up
    /// in ISSUES.md if it ever shows up in a profile.
    #[allow(dead_code)]
    pub fn tuple_elements_vec(&self, id: TypeId) -> Vec<TypeId> {
        let item = self.items[id.0 as usize];
        debug_assert!(matches!(item.tag, Tag::Tuple));
        let start = item.data as usize;
        let n = self.extra[start] as usize;
        self.extra[start + 1..start + 1 + n]
            .iter()
            .map(|&r| TypeId(r))
            .collect()
    }

    // ----- String interning -----

    /// Look up an already-interned string without inserting.
    ///
    /// Returns `None` if `s` has never been passed to `intern_str`.
    /// Useful for read-only consumers (e.g. codegen) that need to
    /// resolve a known name like `"main"` against the pool without
    /// taking `&mut`.
    pub fn find_str(&self, s: &str) -> Option<StringId> {
        let hash = hash_bytes(&self.hasher, s.as_bytes());
        self.string_dedup
            .find(hash, |&candidate| {
                str_eq(&self.strings, &self.string_bytes, candidate, s.as_bytes())
            })
            .copied()
    }

    pub fn intern_str(&mut self, s: &str) -> StringId {
        let hash = hash_bytes(&self.hasher, s.as_bytes());

        // Probe path: hash `s` directly and compare against each
        // candidate's bytes in `string_bytes`. No String alloc.
        let strings = &self.strings;
        let string_bytes = &self.string_bytes;
        if let Some(&id) = self.string_dedup.find(hash, |&candidate| {
            str_eq(strings, string_bytes, candidate, s.as_bytes())
        }) {
            return id;
        }

        // Insert path.
        let offset = u32::try_from(self.string_bytes.len())
            .expect("string arena overflow: more than u32::MAX bytes");
        let len = u32::try_from(s.len()).expect("string too large: more than u32::MAX bytes");
        self.string_bytes.extend_from_slice(s.as_bytes());
        let id = StringId(
            u32::try_from(self.strings.len())
                .expect("string table overflow: more than u32::MAX strings interned"),
        );
        self.strings.push((offset, len));

        let strings = &self.strings;
        let string_bytes = &self.string_bytes;
        let hasher = &self.hasher;
        self.string_dedup.insert_unique(hash, id, |&candidate| {
            str_hash(strings, string_bytes, hasher, candidate)
        });
        id
    }

    pub fn str(&self, id: StringId) -> &str {
        let (offset, len) = self.strings[id.0 as usize];
        let bytes = &self.string_bytes[offset as usize..(offset + len) as usize];
        // SAFETY: `intern_str` only ever pushes valid UTF-8 from
        // `&str::as_bytes`, and the arena is append-only.
        unsafe { std::str::from_utf8_unchecked(bytes) }
    }

    /// Returns a `Display` adapter that renders `id` using `self`.
    pub fn display(&self, id: TypeId) -> DisplayType<'_> {
        DisplayType { pool: self, id }
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
            TypeKind::Tuple => {
                let elems = self.pool.tuple_elements_vec(self.id);
                write!(f, "(")?;
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", self.pool.display(*e))?;
                }
                write!(f, ")")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_have_stable_ids() {
        let pool = InternPool::new();
        assert_ne!(pool.void(), pool.bool_());
        assert_ne!(pool.int(), pool.str_());
        assert_ne!(pool.void(), pool.error_type());
    }

    #[test]
    fn primitive_kind_lookup_matches() {
        let pool = InternPool::new();
        assert_eq!(pool.kind(pool.int()), TypeKind::Int);
        assert_eq!(pool.kind(pool.bool_()), TypeKind::Bool);
        assert_eq!(pool.kind(pool.str_()), TypeKind::Str);
        assert_eq!(pool.kind(pool.void()), TypeKind::Void);
        assert_eq!(pool.kind(pool.error_type()), TypeKind::Error);
    }

    #[test]
    fn display_round_trips_primitives() {
        let pool = InternPool::new();
        assert_eq!(format!("{}", pool.display(pool.int())), "int");
        assert_eq!(format!("{}", pool.display(pool.str_())), "str");
        assert_eq!(format!("{}", pool.display(pool.void())), "void");
        assert_eq!(format!("{}", pool.display(pool.bool_())), "bool");
        assert_eq!(format!("{}", pool.display(pool.error_type())), "<error>");
    }

    #[test]
    fn compatible_absorbs_error_sentinel() {
        let pool = InternPool::new();
        assert!(pool.compatible(pool.int(), pool.int()));
        assert!(!pool.compatible(pool.int(), pool.bool_()));
        assert!(pool.compatible(pool.int(), pool.error_type()));
        assert!(pool.compatible(pool.error_type(), pool.bool_()));
    }

    #[test]
    fn tuple_dedups_and_round_trips() {
        let mut pool = InternPool::new();
        let a = pool.tuple(&[pool.int(), pool.int()]);
        let b = pool.tuple(&[pool.int(), pool.int()]);
        assert_eq!(a, b);
        assert_eq!(pool.kind(a), TypeKind::Tuple);
        assert_eq!(pool.tuple_elements_vec(a), vec![pool.int(), pool.int()]);
        // Different element list -> distinct id.
        let c = pool.tuple(&[pool.int(), pool.bool_()]);
        assert_ne!(a, c);
    }

    #[test]
    fn one_thousand_duplicate_tuples_share_a_single_id() {
        // Phase 2 exit criterion: dedup-on-content keeps storage
        // flat. With the `HashTable<TypeId>` shape there is also no
        // per-call key allocation — the probe hashes the slice
        // directly — so neither `extra` nor `items` nor the dedup
        // table grow after the first insert.
        let mut pool = InternPool::new();
        let first = pool.tuple(&[pool.int(), pool.int(), pool.int()]);
        let extra_len = pool.extra.len();
        let items_len = pool.items.len();
        let table_len = pool.type_dedup.len();
        for _ in 0..1000 {
            let id = pool.tuple(&[pool.int(), pool.int(), pool.int()]);
            assert_eq!(id, first);
        }
        assert_eq!(pool.extra.len(), extra_len);
        assert_eq!(pool.items.len(), items_len);
        assert_eq!(pool.type_dedup.len(), table_len);
    }

    #[test]
    fn tuple_display_formats_recursively() {
        let mut pool = InternPool::new();
        let id = pool.tuple(&[pool.int(), pool.bool_()]);
        assert_eq!(format!("{}", pool.display(id)), "(int, bool)");
    }

    #[test]
    fn intern_str_dedups() {
        let mut pool = InternPool::new();
        let a = pool.intern_str("hello");
        let b = pool.intern_str("hello");
        assert_eq!(a, b);
        let c = pool.intern_str("world");
        assert_ne!(a, c);
        assert_eq!(pool.str(a), "hello");
        assert_eq!(pool.str(c), "world");
    }

    #[test]
    fn intern_str_handles_empty_and_unicode() {
        let mut pool = InternPool::new();
        let e = pool.intern_str("");
        let u = pool.intern_str("ηλο 🌍");
        assert_eq!(pool.str(e), "");
        assert_eq!(pool.str(u), "ηλο 🌍");
    }

    #[test]
    fn intern_str_does_not_duplicate_bytes() {
        // The dedup table stores `StringId` handles, not owned
        // `String` keys, so the byte content lives exactly once —
        // in `string_bytes`. Asserts the storage shape: after
        // interning N distinct strings, `string_bytes.len()` is
        // the sum of their byte lengths and nothing else.
        let mut pool = InternPool::new();
        let inputs = ["alpha", "beta", "gamma", "δελτα"];
        for s in &inputs {
            pool.intern_str(s);
        }
        let expected: usize = inputs.iter().map(|s| s.len()).sum();
        assert_eq!(pool.string_bytes.len(), expected);
        assert_eq!(pool.string_dedup.len(), inputs.len());
    }

    #[test]
    fn intern_str_repeated_does_not_grow_arena() {
        // Probe path: repeated `intern_str` of the same string
        // returns the same `StringId` and doesn't append to the
        // arena. Mirrors the 1000-tuple test on the string side.
        let mut pool = InternPool::new();
        let first = pool.intern_str("identifier");
        let bytes_len = pool.string_bytes.len();
        let table_len = pool.string_dedup.len();
        for _ in 0..1000 {
            assert_eq!(pool.intern_str("identifier"), first);
        }
        assert_eq!(pool.string_bytes.len(), bytes_len);
        assert_eq!(pool.string_dedup.len(), table_len);
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
