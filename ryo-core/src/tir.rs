// Some helpers (the `Display` dump, primitive `from_raw` /
// `as_range` accessors, and `unreachable` builder) are reachable
// only from the `ryo ir --emit=tir` flag and from
// future phases (lazy sema, comptime). Allow until then so CI's
// `-Dwarnings` doesn't fire on shape-only scaffolding.
#![allow(dead_code)]

//! Typed Intermediate Representation (TIR).
//!
//! TIR is the direct structural analogue of Zig's AIR
//! (`src/Air.zig`): a flat instruction stream produced by `sema`
//! from UIR and consumed by `codegen`. Where UIR carried no types
//! and lived in a single program-wide arena, TIR is **per-function-
//! body** and every instruction carries its resolved [`TypeId`].
//!
//! ## Why per-function
//!
//! Phase 5 (lazy sema) and the comptime / generics milestones that
//! ride on top of it duplicate function bodies — one TIR per
//! generic instantiation, one per inline expansion. Keeping each
//! body in its own arena makes "make N typed copies of this body"
//! a `Tir::clone` away. A single program-wide arena (UIR's shape)
//! would force renumbering on every duplication.
//!
//! ## Storage shape
//!
//! Per [`Tir`]:
//!
//! - `instructions: Vec<TypedInst>` — fixed-size `(tag, ty, data)`
//!   triples, one per instruction. Sub-expressions live as their
//!   own entries and are reached via [`TirRef`] indices, never
//!   nested.
//! - `extra: Vec<u32>` — variable-size payloads (call argument
//!   lists, packed `VarDecl` headers, body statement lists). Mirrors
//!   the sidecar arena from UIR / `InternPool`.
//! - `spans: Vec<Span>` — parallel to `instructions`, one span per
//!   `TirRef`. Out-of-band so `TypedInst` itself stays compact.
//!
//! ## Why `NonZeroU32` for `TirRef`
//!
//! `TirRef(NonZeroU32)` makes `Option<TirRef>` a single 32-bit slot
//! via niche-filling. Slot 0 of `instructions` is reserved as a
//! never-emitted sentinel so all valid refs are non-zero. Same
//! invariant as [`crate::uir::InstRef`].
//!
//! ## Trusted producer
//!
//! TIR has exactly one producer (`sema`) and one consumer (`codegen`),
//! and the producer is trusted: view decoders and codegen's per-tag
//! dispatch `unreachable!` on malformed IR instead of returning an
//! error, because malformed TIR is a compiler bug, not user input. If
//! a second producer ever lands (cached IR, plugins, an alternative
//! front end), the decode paths must first be converted to report an
//! internal-error `Diag` — see the `unreachable!` sites in this file.

use crate::ast::CompoundOp;
use crate::types::{InternPool, StringId, TypeId};
use chumsky::span::{SimpleSpan, Span as _};
use std::collections::HashSet;
use std::num::NonZeroU32;

mod version;

pub type Span = SimpleSpan;

// ---------- TirRef ----------

/// Index into a single [`Tir`]'s `instructions` — a `TirRef` from one
/// body is meaningless in another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TirRef(NonZeroU32);

impl ExtraValue for TirRef {
    fn from_extra(raw: u32) -> Self {
        Self::from_raw(raw)
    }

    fn to_extra(self) -> u32 {
        self.raw()
    }
}

impl TirRef {
    fn from_index(idx: usize) -> Self {
        let raw = u32::try_from(idx).expect("TirRef index out of range (>= 2^32)");
        // Real indices must stay below the param-sentinel band
        // (`> u32::MAX / 2`); see the invariant on [`TirRef::param`].
        debug_assert!(
            raw <= u32::MAX / 2,
            "TirRef index entered the param-sentinel band: function bodies \
             must stay below 2^31 instructions"
        );
        TirRef(NonZeroU32::new(raw).expect("TirRef index must be >= 1"))
    }

    pub fn index(self) -> usize {
        self.0.get() as usize
    }

    pub fn raw(self) -> u32 {
        self.0.get()
    }

    pub fn from_raw(raw: u32) -> Self {
        TirRef(NonZeroU32::new(raw).expect("TirRef raw must be non-zero"))
    }

    /// Param sentinel: `u32::MAX - idx`, so sentinels occupy the top
    /// of the `u32` range and real instruction indices the bottom.
    /// Ownership / codegen use these as map keys for param-origin
    /// values; they are never valid indices into `instructions`.
    ///
    /// Sentinels land at `> u32::MAX / 2`, so the encoding stays
    /// collision-free only while a body has fewer than 2^31
    /// instructions (a `debug_assert!` in `from_index` enforces it)
    /// and `idx` stays below 2^31.
    pub fn param(idx: usize) -> Self {
        // Same domain as `from_index`: `idx` must stay below 2^31 or the
        // sentinel collides with real instruction indices (and the
        // `u32::MAX - idx` subtraction wraps past the sentinel band).
        debug_assert!(
            idx < (1 << 31),
            "TirRef param index out of domain: param indices must stay below 2^31"
        );
        Self::from_raw(u32::MAX - idx as u32)
    }

    /// True for sentinel refs produced by [`TirRef::param`].
    pub const fn is_param(self) -> bool {
        self.0.get() > u32::MAX / 2
    }

    /// The param index for sentinel refs, `None` for real instructions.
    pub const fn as_param_index(self) -> Option<u32> {
        if self.is_param() {
            Some(u32::MAX - self.0.get())
        } else {
            None
        }
    }
}

// ---------- ExtraRange ----------

pub use crate::extra::enum_lit_extra;
pub use crate::extra::{ExtraList, ExtraRange, ExtraValue};

// ---------- Instruction tags ----------

/// All TIR instruction kinds.
///
/// Compared with [`crate::uir::InstTag`], TIR tags are *lowered*: the
/// type information that disambiguates polymorphic UIR ops lives in
/// [`TypedInst::ty`], and the tag itself names the concrete machine
/// operation — the shape lets new operations slot in as new arms
/// without reshuffling sema or codegen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TirTag {
    // Constants — terminal, no operands.
    IntConst,
    FloatConst,
    BoolConst,
    StrConst,
    /// `b"..."` literal (M8.4.2). Payload in `TirData::Str`.
    BytesConst,

    /// Read of a local (parameter or `let`-bound), resolved to a
    /// `StringId` for codegen's `HashMap<StringId, Variable>` lookup.
    Var,

    // Integer arithmetic / comparison. Both operands in
    // `TirData::BinOp`. `ICmpEq` / `ICmpNe` work for any operand
    // type that lowers to a Cranelift `icmp` (today: int, bool).
    IAdd,
    ISub,
    IMul,
    ISDiv,
    IMod,
    ICmpEq,
    ICmpNe,
    ICmpLt,
    ICmpLe,
    ICmpGt,
    ICmpGe,

    // String concatenation.
    StrConcat,

    // String equality.
    StrCmpEq,
    StrCmpNe,

    /// `bytes + bytes` concatenation (M8.4.2). Payload in `TirData::BinOp`.
    BytesConcat,

    /// `bytes`/`bytesview` equality (M8.4.2). Payload in `TirData::BinOp`.
    BytesCmpEq,
    /// `bytes`/`bytesview` inequality (M8.4.2). Payload in `TirData::BinOp`.
    BytesCmpNe,
    /// `bytes`/`bytesview` scalar indexing (M8.4.2): bounds-checked byte
    /// load → `int` (`u8` at M17.1). Payload in `TirData::BinOp`.
    BytesIndex,

    /// Memberwise struct equality (M9.1). Payload in `TirData::BinOp`; result `bool`.
    StructEq,
    /// Memberwise struct inequality (M9.1). Payload in `TirData::BinOp`; result `bool`.
    StructNe,

    /// Discriminant-and-payload enum equality (M11). Payload in
    /// `TirData::BinOp`; result `bool`.
    EnumEq,
    /// Discriminant-and-payload enum inequality (M11). Payload in
    /// `TirData::BinOp`; result `bool`.
    EnumNe,

    /// Read the `len` field of a str fat pointer. `TirData::UnOp`.
    StrLen,

    // Float arithmetic / comparison.
    FAdd,
    FSub,
    FMul,
    FDiv,
    FCmpEq,
    FCmpNe,
    FCmpLt,
    FCmpLe,
    FCmpGt,
    FCmpGe,

    /// Integer negation. Operand in `TirData::UnOp`.
    INeg,

    /// Float negation. Operand in `TirData::UnOp`.
    FNeg,

    /// Function call (user or builtin). Variable payload in `extra`
    /// — see [`call_extra`].
    Call,

    /// Variable declaration with an initializer. Variable payload in
    /// `extra` — see [`var_decl_extra`]; `TypedInst.ty` carries the
    /// *variable's* resolved type (Phase-3 side-table behaviour).
    VarDecl,

    /// Reassignment to an existing mutable variable. Variable payload in `extra` — see [`assign_extra`].
    Assign,

    /// Compound assignment (`+=`, `-=`, etc.) to a mutable variable;
    /// variable payload in `extra` — see [`compound_assign_extra`].
    CompoundAssign,

    /// Slice projection `base[start:end]` → `strview` (M8.4).
    Slice,
    /// Explicit owner → view representation conversion (drops `cap`),
    /// inserted by sema at view-parameter call sites and mixed
    /// owner/view equality operands. Operand in `data.un_op`. Owner
    /// pairs come from the pool's `owner_view` table (`str → strview`,
    /// `bytes → bytesview`, M8.4); the one cross-family use is
    /// `str`/`strview`.as_bytes() → `bytesview` — same representation
    /// conversion (drop `cap`, promote-on-view), different projected
    /// type.
    ToView,
    /// View → owner re-borrow (final spec P6'): materializes the cap=0 fat
    /// triple — no allocation, call-scoped. Inserted by sema when a view
    /// is passed to an owned borrow parameter. `TirData::UnOp`.
    ViewAsOwner,

    /// `print()`-gate rewrite (M9.1): `int`/`float`/`bool`/struct/enum
    /// args render via their Debug repr (`print(x)` → `print(DebugRepr(x))`,
    /// like bytes → `__ryo_bytes_repr`). The operand kind rides on the
    /// operand's type (M11 enums included) — no payload bits of its own.
    /// Borrows operand; owned `str` out.
    DebugRepr,

    /// `return <expr>`. Operand in `TirData::UnOp`.
    Return,

    /// `return` with no expression.
    ReturnVoid,

    /// Top-level expression statement (value discarded). Operand in `TirData::UnOp`.
    ExprStmt,

    // Logical operators (short-circuit in codegen).
    BoolAnd,
    BoolOr,
    BoolNot,

    /// If/elif/else statement. Variable payload in `extra`.
    IfStmt,

    /// `while cond: body`. Variable payload in `extra` — see [`while_loop_extra`].
    WhileLoop,

    /// `for var in range(start, end): body`. Variable payload in `extra` — see [`for_range_extra`].
    ForRange,

    /// `break` statement.
    Break,

    /// `continue` statement.
    Continue,

    /// Inserted by sema at the point of an unrecoverable type error
    /// so the rest of the body still produces well-formed TIR. Has
    /// `ty == pool.error_type()`. Codegen must never see one — the
    /// driver short-circuits on `sink.has_errors()`.
    Unreachable,

    /// Struct literal `Name{field = value, ...}` (M9). Variable
    /// payload in `extra` — see [`struct_lit_extra`]. Entries are in
    /// canonical (declaration-order) field order; `TypedInst.ty` is
    /// the struct type.
    StructLit,

    /// Enum variant construction `Name::Variant(args...)` (M11).
    /// Variable payload in `extra` — see [`enum_lit_extra`]. The
    /// enum type and declaration-order variant index head the
    /// payload; args are `(field_idx, value)` pairs keyed by
    /// declaration-order payload-field index (tuple variants use the
    /// synthesized `"0"`, `"1"`, … names). Unit variants encode
    /// `argc = 0`; `TypedInst.ty` is the enum type.
    EnumLit,

    /// Field access `object.field` (M9). Payload in
    /// `TirData::FieldAccess` (object + canonical field index);
    /// `TypedInst.ty` is the field type.
    FieldAccess,

    /// Field-path assignment `p.x = v` (M9). Variable payload in
    /// `extra` — see [`field_assign_extra`]; `TypedInst.ty` is the
    /// field type.
    FieldAssign,

    /// Compound field-path assignment `p.x += v` (M9). Variable payload
    /// in `extra` — see [`compound_field_assign_extra`]; `TypedInst.ty` is the field type.
    CompoundFieldAssign,

    /// Destructuring assignment `pattern = value` (M10): `(a, b) = e`,
    /// `{q, r} = e`. Variable payload in `extra` — see
    /// [`destructure_extra`]; `TypedInst.ty` is the rhs struct type.
    /// Codegen moves each bound field out of the rhs slot (or drops it
    /// inline for a wildcard); the ownership pass consumes the rhs
    /// owner at the statement and adopts the per-bound-field owner
    /// tokens (see [`TirBuilder::destructure`]'s convention) as fresh
    /// owners.
    Destructure,

    /// `match scrutinee:` expression (M12): resolved arms over a typed
    /// scrutinee. Variable payload in `extra` — see [`match_extra`]
    /// and the `tir::match_` module for the wire layout.
    /// `TypedInst.ty` is the match expression's result type (the join
    /// of the arm bodies). The ownership pass consumes the
    /// by-move-binding owner tokens (see [`TirBuilder::match_expr`]'s
    /// convention) the way it does for [`TirTag::Destructure`].
    Match,
}

// ---------- Per-argument call convention ----------

/// Per-argument call convention, stamped by sema and read by the
/// ownership pass. M8.3 adds `Inout` (mutable borrow, call-site `&x`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ParamMode {
    Borrow = 0,
    Move = 1,
    Inout = 2,
}

impl ParamMode {
    fn to_u32(self) -> u32 {
        self as u32
    }

    /// Strict decode: `None` for any word not produced by
    /// [`ParamMode::to_u32`]. Callers must treat `None` as producer
    /// corruption, never as `Borrow`.
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(ParamMode::Borrow),
            1 => Some(ParamMode::Move),
            2 => Some(ParamMode::Inout),
            _ => None,
        }
    }
}

impl ExtraValue for ParamMode {
    fn from_extra(raw: u32) -> Self {
        Self::from_u32(raw).unwrap_or_else(|| {
            unreachable!("call_extra mode word {raw} not written by ParamMode::to_u32")
        })
    }

    fn to_extra(self) -> u32 {
        self.to_u32()
    }
}

// ---------- Instruction data ----------

/// Per-instruction inline payload. Same shape as UIR's
/// [`crate::uir::InstData`] but parameterized over [`TirRef`].
#[derive(Debug, Clone, Copy)]
pub enum TirData {
    None,
    Int(i64),
    Float(f64),
    Str(StringId),
    Bool(bool),
    Var(StringId),
    UnOp(TirRef),
    BinOp {
        lhs: TirRef,
        rhs: TirRef,
    },
    /// Slice projection. Bounds `None` for shorthands (codegen
    /// substitutes 0 / len-of-base).
    Slice {
        base: TirRef,
        start: Option<TirRef>,
        end: Option<TirRef>,
    },
    /// Field access (M9): object + canonical (declaration-order)
    /// field index. Two 32-bit handles, so this stays inline.
    FieldAccess {
        object: TirRef,
        field_index: u32,
    },
    Extra(ExtraRange),
}

#[derive(Debug, Clone, Copy)]
pub struct TypedInst {
    pub tag: TirTag,
    pub ty: TypeId,
    pub data: TirData,
}

// ---------- Function bodies ----------

#[derive(Debug, Clone)]
pub struct TirParam {
    pub name: StringId,
    pub ty: TypeId,
    pub mode: ParamMode,
    pub span: Span,
}

/// One function body's typed instruction stream.
///
/// Per the doc (§4.1): "TIR is per-function-body, not per-program."
/// Each `Tir` owns its own `instructions` / `extra` / `spans`
/// arenas; refs are scoped to the body — the shape that lets
/// monomorphization (Phase 5) clone-and-substitute one body without
/// renumbering everything else.
///
/// The body is tree-shaped: every instruction has at most one parent
/// (one incoming operand or body-statement edge), so the body roots
/// plus [`Tir::walk_operands`] edges form a forest, not a DAG.
/// Analyses that record one parent per instruction — e.g. the
/// ownership pass's first-parent-wins consumer map — rely on this;
/// [`TirBuilder::finish`] debug-asserts it on every built body.
#[derive(Debug, Clone)]
pub struct Tir {
    identity: crate::artifact::ArtifactId,
    pub name: StringId,
    pub params: Vec<TirParam>,
    pub return_type: TypeId,
    pub instructions: Vec<TypedInst>,
    pub extra: Vec<u32>,
    pub spans: Vec<Span>,
    /// Range into `extra` of [`TirRef::raw`] handles for the body's
    /// top-level statements, in execution order.
    pub body: ExtraRange,
    pub span: Span,
    /// Lazily computed per-body operand-subtree match table; see
    /// [`Tir::operand_match_memo`]. Not serialized.
    pub operand_match_memo: std::cell::OnceCell<Box<[bool]>>,
    /// `true` when this body contains at least one `Match`
    /// instruction — stamped by the builder (`match_expr` is the only
    /// producer), the O(1) gate that lets match-free bodies skip every
    /// value-position-match scan and unwrap without decoding a single
    /// instruction.
    pub contains_match: bool,
}

impl Tir {
    /// Identity of this immutable typed body version.
    pub fn identity(&self) -> crate::artifact::ArtifactId {
        self.identity
    }

    /// Look up the instruction for `r`.
    ///
    /// # Panics
    ///
    /// Panics if `r` is a param sentinel ([`TirRef::param`]). Sentinels
    /// are map keys for param-origin values in ownership/codegen and
    /// are by construction never valid indices into `instructions`;
    /// callers must resolve them to their owning instruction first.
    /// This is a hard `assert!` (not debug-only) so a contract
    /// violation fails with a clear message in release builds too,
    /// instead of a cryptic index-out-of-bounds. A raw-0 ref needs no
    /// guard here: `TirRef` is a `NonZeroU32` newtype, so slot 0 (the
    /// reserved arena sentinel) is unconstructible — `from_raw(0)`
    /// and `from_index(0)` already panic at construction.
    pub fn inst(&self, r: TirRef) -> &TypedInst {
        assert!(
            !r.is_param(),
            "Tir::inst called with a param sentinel ref (raw={}); \
             param sentinels are not instruction indices",
            r.raw()
        );
        &self.instructions[r.index()]
    }

    /// Look up the source span for `r`. Same param-sentinel contract
    /// as [`Tir::inst`].
    pub fn span(&self, r: TirRef) -> Span {
        assert!(
            !r.is_param(),
            "Tir::span called with a param sentinel ref (raw={}); \
             param sentinels are not instruction indices",
            r.raw()
        );
        self.spans[r.index()]
    }

    /// Borrow the function body statement references from the extra arena.
    /// The view decodes each reference as it is read and does not allocate.
    pub fn body_stmts(&self) -> ExtraList<'_, TirRef> {
        ExtraList::new(&self.extra[self.body.as_range()])
    }
}

// ---------- Builder ----------

/// Mutable handle for emitting one function body's TIR. Sema is its
/// only caller in production; tests use it directly.
pub struct TirBuilder {
    name: StringId,
    params: Vec<TirParam>,
    return_type: TypeId,
    span: Span,
    instructions: Vec<TypedInst>,
    extra: Vec<u32>,
    spans: Vec<Span>,
    /// Set by `match_expr` — the only `Match` producer — and copied
    /// into the built [`Tir`]'s `contains_match`.
    saw_match: bool,
}

impl TirBuilder {
    pub fn name(&self) -> StringId {
        self.name
    }

    pub fn new(name: StringId, params: Vec<TirParam>, return_type: TypeId, span: Span) -> Self {
        // Slot 0 is the reserved sentinel — never read, never
        // referenced. Pushing a placeholder keeps `TirRef` indices
        // 1-based without runtime checks on every read.
        let placeholder_span = SimpleSpan::new((), 0..0);
        let placeholder = TypedInst {
            tag: TirTag::Unreachable,
            ty: TypeId::from_raw(u32::MAX),
            data: TirData::None,
        };
        TirBuilder {
            name,
            params,
            return_type,
            span,
            instructions: vec![placeholder],
            extra: Vec::new(),
            spans: vec![placeholder_span],
            saw_match: false,
        }
    }

    /// Type of an instruction the builder has already emitted.
    /// Sema needs this to type-check operands of sub-expressions
    /// it just translated, before the builder is `finish`ed into a
    /// `Tir`. Confined to type lookup so the builder's instruction
    /// arena stays an implementation detail.
    pub fn ty_of(&self, r: TirRef) -> TypeId {
        self.instructions[r.index()].ty
    }

    /// Return-flow query over the partially built body — the
    /// [`Tir::block_definitely_returns`] rules, runnable mid-analysis.
    /// Sema's match arm typing asks this of a statement arm's just-
    /// emitted stmt list to class the arm as `never` (terminating) or
    /// fallthrough. Rules live in the shared free function so the
    /// finished-`Tir` and in-progress-builder answers never drift.
    pub fn block_definitely_returns(&self, stmts: &[TirRef], pool: &InternPool) -> bool {
        terminates::block_terminates(
            &self.instructions,
            &self.extra,
            stmts.iter().copied(),
            pool,
            false,
        )
    }

    /// The arm-termination sense used by M12 match arm typing: like
    /// [`TirBuilder::block_definitely_returns`], but a statement arm
    /// ending in `break`/`continue` also counts as terminating (the
    /// arm is `never` at the join, never an E0135 fallthrough).
    pub fn block_definitely_terminates(&self, stmts: &[TirRef], pool: &InternPool) -> bool {
        terminates::block_terminates(
            &self.instructions,
            &self.extra,
            stmts.iter().copied(),
            pool,
            true,
        )
    }

    fn push(&mut self, tag: TirTag, ty: TypeId, data: TirData, span: Span) -> TirRef {
        let idx = self.instructions.len();
        self.instructions.push(TypedInst { tag, ty, data });
        self.spans.push(span);
        TirRef::from_index(idx)
    }

    pub fn int_const(&mut self, value: i64, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::IntConst, ty, TirData::Int(value), span)
    }

    pub fn float_const(&mut self, value: f64, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::FloatConst, ty, TirData::Float(value), span)
    }

    pub fn bool_const(&mut self, value: bool, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::BoolConst, ty, TirData::Bool(value), span)
    }

    pub fn str_const(&mut self, value: StringId, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::StrConst, ty, TirData::Str(value), span)
    }

    pub fn bytes_const(&mut self, value: StringId, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::BytesConst, ty, TirData::Str(value), span)
    }

    pub fn var(&mut self, name: StringId, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::Var, ty, TirData::Var(name), span)
    }

    pub fn unary(&mut self, tag: TirTag, ty: TypeId, operand: TirRef, span: Span) -> TirRef {
        debug_assert!(matches!(
            tag,
            TirTag::INeg | TirTag::FNeg | TirTag::BoolNot | TirTag::Return | TirTag::ExprStmt
        ));
        self.push(tag, ty, TirData::UnOp(operand), span)
    }

    pub fn binary(
        &mut self,
        tag: TirTag,
        ty: TypeId,
        lhs: TirRef,
        rhs: TirRef,
        span: Span,
    ) -> TirRef {
        debug_assert!(matches!(
            tag,
            TirTag::IAdd
                | TirTag::ISub
                | TirTag::IMul
                | TirTag::ISDiv
                | TirTag::IMod
                | TirTag::ICmpEq
                | TirTag::ICmpNe
                | TirTag::ICmpLt
                | TirTag::ICmpLe
                | TirTag::ICmpGt
                | TirTag::ICmpGe
                | TirTag::StrConcat
                | TirTag::StrCmpEq
                | TirTag::StrCmpNe
                | TirTag::BytesConcat
                | TirTag::BytesCmpEq
                | TirTag::BytesCmpNe
                | TirTag::BytesIndex
                | TirTag::StructEq
                | TirTag::StructNe
                | TirTag::EnumEq
                | TirTag::EnumNe
                | TirTag::FAdd
                | TirTag::FSub
                | TirTag::FMul
                | TirTag::FDiv
                | TirTag::FCmpEq
                | TirTag::FCmpNe
                | TirTag::FCmpLt
                | TirTag::FCmpLe
                | TirTag::FCmpGt
                | TirTag::FCmpGe
                | TirTag::BoolAnd
                | TirTag::BoolOr
        ));
        self.push(tag, ty, TirData::BinOp { lhs, rhs }, span)
    }

    pub fn return_void(&mut self, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::ReturnVoid, ty, TirData::None, span)
    }

    pub fn unreachable(&mut self, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::Unreachable, ty, TirData::None, span)
    }

    /// General-purpose instruction emit for tags that don't fit the
    /// `unary` / `binary` debug-assert gates. Sema uses this for
    /// method-call lowerings like `StrLen`.
    pub fn push_typed(&mut self, tag: TirTag, data: TirData, ty: TypeId, span: Span) -> TirRef {
        self.push(tag, ty, data, span)
    }

    /// Slice projection `base[start:end]` → `strview` (final spec §3.1).
    /// `start` / `end` are `None` for the `s[start:]`, `s[:end]`,
    /// `s[:]` shorthands; codegen substitutes 0 / len-of-base.
    /// `view_ty` is the pool's `str_view()` — the builder holds no pool.
    pub fn slice(
        &mut self,
        base: TirRef,
        start: Option<TirRef>,
        end: Option<TirRef>,
        view_ty: TypeId,
        span: Span,
    ) -> TirRef {
        self.push(
            TirTag::Slice,
            view_ty,
            TirData::Slice { base, start, end },
            span,
        )
    }

    /// Explicit owner → view representation conversion (final spec
    /// §3.4): drops the `cap` word. Inserted by sema at view-parameter
    /// call sites and on the owned side of mixed owner/view equality.
    /// `view_ty` comes from the pool's `owner_view` table — except for
    /// `as_bytes()`, which crosses families (`str`/`strview` →
    /// `bytesview`); the representation conversion is the same.
    pub fn to_view(&mut self, inner: TirRef, view_ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::ToView, view_ty, TirData::UnOp(inner), span)
    }

    /// View → owner re-borrow (final spec P6'): materializes the cap=0
    /// fat triple at the call site — no allocation, call-scoped.
    /// Inserted by sema when a view is passed to an owned borrow
    /// parameter. `ty` is the pool's owner type for the view.
    pub fn view_as_owner(&mut self, inner: TirRef, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::ViewAsOwner, ty, TirData::UnOp(inner), span)
    }

    fn extra_offset(&self) -> u32 {
        u32::try_from(self.extra.len()).expect("TIR extra arena exceeded u32::MAX words")
    }

    fn len_u32(len: usize) -> u32 {
        u32::try_from(len).expect("TIR list length exceeded u32::MAX")
    }

    fn push_ref_list(&mut self, refs: &[TirRef]) {
        self.extra.push(Self::len_u32(refs.len()));
        for r in refs {
            self.extra.push(r.raw());
        }
    }

    /// Emit a `Call` with name, arg list, and per-arg call conventions
    /// packed into `extra`. `ty` is the call's *return* type.
    /// `modes` carries one [`ParamMode`] per argument (borrow vs move),
    /// stamped by sema from the callee signature.
    pub fn call(
        &mut self,
        name: StringId,
        args: &[TirRef],
        modes: &[ParamMode],
        ty: TypeId,
        span: Span,
    ) -> TirRef {
        let range = call_extra::encode(&mut self.extra, name, args, modes);
        self.push(TirTag::Call, ty, TirData::Extra(range), span)
    }

    /// Emit a `VarDecl`. `var_ty` is the variable's resolved type
    /// (post annotation / inference) and goes into the `TypedInst.ty`
    /// slot directly — there is no `None` shape here.
    pub fn var_decl(
        &mut self,
        name: StringId,
        mutable: bool,
        var_ty: TypeId,
        initializer: TirRef,
        span: Span,
    ) -> TirRef {
        let range = var_decl_extra::encode(&mut self.extra, name, mutable, initializer);
        self.push(TirTag::VarDecl, var_ty, TirData::Extra(range), span)
    }

    pub fn assign(&mut self, name: StringId, var_ty: TypeId, value: TirRef, span: Span) -> TirRef {
        let range = assign_extra::encode(&mut self.extra, name, value);
        self.push(TirTag::Assign, var_ty, TirData::Extra(range), span)
    }

    pub fn compound_assign(
        &mut self,
        name: StringId,
        op: CompoundOp,
        var_ty: TypeId,
        value: TirRef,
        span: Span,
    ) -> TirRef {
        let range = compound_assign_extra::encode(&mut self.extra, name, op, value);
        self.push(TirTag::CompoundAssign, var_ty, TirData::Extra(range), span)
    }

    pub fn if_stmt(
        &mut self,
        cond: TirRef,
        then_stmts: &[TirRef],
        elif_branches: &[(TirRef, Vec<TirRef>)],
        else_stmts: Option<&[TirRef]>,
        ty: TypeId,
        span: Span,
    ) -> TirRef {
        let range =
            if_stmt_extra::encode(&mut self.extra, cond, then_stmts, elif_branches, else_stmts);
        self.push(TirTag::IfStmt, ty, TirData::Extra(range), span)
    }

    pub fn while_loop(&mut self, cond: TirRef, body: &[TirRef], ty: TypeId, span: Span) -> TirRef {
        let range = while_loop_extra::encode(&mut self.extra, cond, body);
        self.push(TirTag::WhileLoop, ty, TirData::Extra(range), span)
    }

    pub fn for_range(
        &mut self,
        var_name: StringId,
        start: TirRef,
        end: TirRef,
        body: &[TirRef],
        ty: TypeId,
        span: Span,
    ) -> TirRef {
        let range = for_range_extra::encode(&mut self.extra, var_name, start, end, body);
        self.push(TirTag::ForRange, ty, TirData::Extra(range), span)
    }

    /// Emit a `StructLit` (M9). `fields` are `(field_index, value)`
    /// pairs in canonical (declaration) order — sema sorts before
    /// calling. `ty` is the struct type.
    pub fn struct_lit(&mut self, ty: TypeId, fields: &[(u32, TirRef)], span: Span) -> TirRef {
        let range = struct_lit_extra::encode(&mut self.extra, ty, fields);
        self.push(TirTag::StructLit, ty, TirData::Extra(range), span)
    }

    /// Emit an `EnumLit` `Name::Variant(args...)` (M11). `args` are
    /// `(field_idx, value)` pairs; `field_idx` selects the payload
    /// field within the variant (tuple variants use the synthesized
    /// `"0"`, `"1"`, … names). `ty` is the enum type; `variant` is
    /// the declaration-order variant index. Unit variants pass `&[]`.
    /// `positional` records the construction's source form for
    /// diagnostics; codegen ignores it (wire parity with UIR's
    /// [`enum_lit_extra::FLAG_POSITIONAL`]).
    pub fn enum_lit(
        &mut self,
        ty: TypeId,
        variant: u32,
        positional: bool,
        args: &[(u32, TirRef)],
        span: Span,
    ) -> TirRef {
        let range = enum_lit_extra::encode(&mut self.extra, ty, variant, args, positional);
        self.push(TirTag::EnumLit, ty, TirData::Extra(range), span)
    }

    /// Emit a `FieldAccess` (M9). `field_index` is the canonical
    /// declaration-order index; `ty` is the field type.
    pub fn field_access(
        &mut self,
        object: TirRef,
        field_index: u32,
        ty: TypeId,
        span: Span,
    ) -> TirRef {
        self.push(
            TirTag::FieldAccess,
            ty,
            TirData::FieldAccess {
                object,
                field_index,
            },
            span,
        )
    }

    /// Emit a `FieldAssign` `target = value` (M9); `ty` is the field
    /// type.
    pub fn field_assign(
        &mut self,
        target: TirRef,
        value: TirRef,
        ty: TypeId,
        span: Span,
    ) -> TirRef {
        let range = field_assign_extra::encode(&mut self.extra, target, value);
        self.push(TirTag::FieldAssign, ty, TirData::Extra(range), span)
    }

    /// Emit a `CompoundFieldAssign` `target op= value` (M9); `ty` is
    /// the field type.
    pub fn compound_field_assign(
        &mut self,
        target: TirRef,
        op: CompoundOp,
        ty: TypeId,
        value: TirRef,
        span: Span,
    ) -> TirRef {
        let range = compound_field_assign_extra::encode(&mut self.extra, target, op, value);
        self.push(TirTag::CompoundFieldAssign, ty, TirData::Extra(range), span)
    }

    pub fn break_stmt(&mut self, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::Break, ty, TirData::None, span)
    }

    pub fn continue_stmt(&mut self, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::Continue, ty, TirData::None, span)
    }

    /// Finish: bake in the body statement list and produce the
    /// finished [`Tir`].
    pub fn finish(mut self, stmts: &[TirRef]) -> Tir {
        let offset = self.extra_offset();
        for r in stmts {
            self.extra.push(r.raw());
        }
        let len = Self::len_u32(stmts.len());
        let tir = Tir {
            identity: crate::artifact::ArtifactId::fresh(),
            name: self.name,
            params: self.params,
            return_type: self.return_type,
            instructions: self.instructions,
            extra: self.extra,
            spans: self.spans,
            body: ExtraRange { offset, len },
            span: self.span,
            operand_match_memo: std::cell::OnceCell::new(),
            contains_match: self.saw_match,
        };
        #[cfg(debug_assertions)]
        tir.validate_tree_shape();
        tir
    }
}

fn read_ref_list_view<'a>(slice: &'a [u32], pos: &mut usize) -> ExtraList<'a, TirRef> {
    let count = slice[*pos] as usize;
    *pos += 1;
    let words = &slice[*pos..*pos + count];
    *pos += count;
    ExtraList::new(words)
}

fn skip_ref_list(slice: &[u32], pos: &mut usize) {
    let count = slice[*pos] as usize;
    *pos += 1 + count;
}

// ---------- Structural reachability ----------

/// Distinguishes the two kinds of (parent, child) edges announced
/// by [`Tir::walk_operands`]:
///
/// * [`ChildKind::Operand`] — a direct data dependency (e.g. a
///   binary-op LHS, a call arg, a `VarDecl`'s initializer, an
///   `if`/`while`/`for` condition or range bound). Consumer of
///   the parent.
/// * [`ChildKind::BodyStmt`] — a statement nested inside an
///   `if`/`while`/`for` body. Reachable from the parent for
///   traversal, but not a consumer of the parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildKind {
    Operand,
    BodyStmt,
}

impl Tir {
    /// Visit every direct operand and body-statement edge of TIR
    /// instruction `r`, invoking `f(parent, child, kind)` for each
    /// `(parent, child)` edge in forward source order. **Shallow** —
    /// does not recurse on its own; callers' closures drive recursion
    /// (see [`Tir::collect_reachable`]). Avoids the O(2^N) re-walking
    /// that an internally-recursive walker would produce when callers
    /// also recurse via their closures.
    ///
    /// Single source of truth for TIR-shape coverage across the
    /// compiler's post-sema analyses (the ownership pass's last-use
    /// and consumer-of walks drive their recursion from here). Adding
    /// a new TIR shape requires updating exactly this function.
    pub fn walk_operands(&self, r: TirRef, f: &mut impl FnMut(TirRef, TirRef, ChildKind)) {
        let inst = *self.inst(r);
        match inst.data {
            TirData::UnOp(o) => {
                f(r, o, ChildKind::Operand);
            }
            TirData::BinOp { lhs, rhs } => {
                f(r, lhs, ChildKind::Operand);
                f(r, rhs, ChildKind::Operand);
            }
            TirData::Slice { base, start, end } => {
                // M8.4: a slice reads its base and bounds; the base read
                // is what keeps the owner live (final spec §3.2 P5).
                f(r, base, ChildKind::Operand);
                if let Some(s) = start {
                    f(r, s, ChildKind::Operand);
                }
                if let Some(e) = end {
                    f(r, e, ChildKind::Operand);
                }
            }
            TirData::FieldAccess { object, .. } => {
                f(r, object, ChildKind::Operand);
            }
            TirData::Extra(_) => match inst.tag {
                TirTag::Call => {
                    for arg in self.call_view(r).args {
                        f(r, arg, ChildKind::Operand);
                    }
                }
                TirTag::StructLit => {
                    for (_, value) in self.struct_lit_view(r).fields() {
                        f(r, value, ChildKind::Operand);
                    }
                }
                TirTag::EnumLit => {
                    for (_, value) in self.enum_lit_view(r).fields() {
                        f(r, value, ChildKind::Operand);
                    }
                }
                TirTag::VarDecl => {
                    let v = self.var_decl_view(r);
                    f(r, v.initializer, ChildKind::Operand);
                }
                TirTag::Assign => {
                    let v = self.assign_view(r);
                    f(r, v.value, ChildKind::Operand);
                }
                TirTag::CompoundAssign => {
                    let v = self.compound_assign_view(r);
                    f(r, v.value, ChildKind::Operand);
                }
                TirTag::FieldAssign => {
                    let v = self.field_assign_view(r);
                    f(r, v.target, ChildKind::Operand);
                    f(r, v.value, ChildKind::Operand);
                }
                TirTag::CompoundFieldAssign => {
                    let v = self.compound_field_assign_view(r);
                    f(r, v.target, ChildKind::Operand);
                    f(r, v.value, ChildKind::Operand);
                }
                TirTag::Destructure => {
                    // M10: only the rhs is announced. The per-bound-field
                    // owner tokens (FieldAccess insts emitted immediately
                    // before this instruction) are NOT operands: codegen
                    // never evaluates them, and announcing them would
                    // reach the rhs twice, breaking the tree-shape
                    // invariant.
                    f(r, self.destructure_view(r).rhs, ChildKind::Operand);
                }
                TirTag::Match => {
                    // M12: the scrutinee is an operand; each arm body is
                    // announced like an IfStmt branch — an operand for
                    // an expr arm, body statements for a statement arm.
                    // The per-by-move-binding owner tokens (FieldAccess
                    // insts emitted immediately before this instruction)
                    // are NOT operands: codegen never evaluates them,
                    // and announcing them would reach the scrutinee
                    // twice, breaking the tree-shape invariant.
                    let view = self.match_view(r);
                    f(r, view.scrutinee, ChildKind::Operand);
                    for arm in view.arms() {
                        match arm.body {
                            MatchBodyView::Expr(e) => f(r, e, ChildKind::Operand),
                            MatchBodyView::Stmts(_) => {
                                for s in arm.body.stmts() {
                                    f(r, s, ChildKind::BodyStmt);
                                }
                            }
                        }
                    }
                }
                TirTag::IfStmt => {
                    let v = self.if_stmt_view(r);
                    f(r, v.cond, ChildKind::Operand);
                    for s in v.then_stmts {
                        f(r, s, ChildKind::BodyStmt);
                    }
                    for elif in &v.elif_branches {
                        f(r, elif.cond, ChildKind::Operand);
                        for s in elif.body {
                            f(r, s, ChildKind::BodyStmt);
                        }
                    }
                    if let Some(stmts) = v.else_stmts {
                        for s in stmts {
                            f(r, s, ChildKind::BodyStmt);
                        }
                    }
                }
                TirTag::WhileLoop => {
                    let v = self.while_loop_view(r);
                    f(r, v.cond, ChildKind::Operand);
                    for s in v.body {
                        f(r, s, ChildKind::BodyStmt);
                    }
                }
                TirTag::ForRange => {
                    let v = self.for_range_view(r);
                    f(r, v.start, ChildKind::Operand);
                    f(r, v.end, ChildKind::Operand);
                    for s in v.body {
                        f(r, s, ChildKind::BodyStmt);
                    }
                }
                _ => {}
            },
            TirData::None
            | TirData::Int(_)
            | TirData::Float(_)
            | TirData::Str(_)
            | TirData::Bool(_)
            | TirData::Var(_) => {}
        }
    }

    fn extra_slice(&self, r: TirRef) -> &[u32] {
        let range = match self.inst(r).data {
            TirData::Extra(range) => range,
            _ => unreachable!("instruction must carry TirData::Extra"),
        };
        &self.extra[range.as_range()]
    }

    /// Debug-only check of the tree-shape invariant documented on
    /// [`Tir`]: walking every operand / body-statement edge from the
    /// body roots, no instruction may be reached twice — each inst has
    /// at most one parent. Called from [`TirBuilder::finish`]; costs
    /// one O(N) walk per built body in debug builds only.
    #[cfg(debug_assertions)]
    fn validate_tree_shape(&self) {
        fn walk(tir: &Tir, r: TirRef, seen: &mut HashSet<TirRef>) {
            tir.walk_operands(r, &mut |_parent, child, _kind| {
                debug_assert!(
                    seen.insert(child),
                    "TIR body is not tree-shaped: instruction raw={} has more than one parent",
                    child.raw()
                );
                walk(tir, child, seen);
            });
        }
        let mut seen: HashSet<TirRef> = HashSet::new();
        for stmt in self.body_stmts() {
            debug_assert!(
                seen.insert(stmt),
                "TIR body is not tree-shaped: statement raw={} is listed twice in the body roots",
                stmt.raw()
            );
            walk(self, stmt, &mut seen);
        }
    }

    /// Borrow a loop body from the extra arena without decoding it into a `Vec`.
    pub fn loop_body_view(&self, loop_inst: TirRef) -> Option<ExtraList<'_, TirRef>> {
        match self.inst(loop_inst).tag {
            TirTag::WhileLoop => Some(self.while_loop_body_view(loop_inst)),
            TirTag::ForRange => Some(self.for_range_body_view(loop_inst)),
            _ => None,
        }
    }

    /// Collect every `TirRef` reachable from `loop_inst`'s body —
    /// transitive operands AND nested body statements — into `set`.
    /// Used to classify instructions as inside-loop vs pre-loop
    /// without relying on raw-index comparisons (producer refs are
    /// emitted before their parent body stmt, so a producer's index
    /// can sit below the parent body-stmt's even though it's
    /// semantically inside).
    pub fn collect_loop_body_refs(&self, loop_inst: TirRef, set: &mut HashSet<TirRef>) {
        let Some(body) = self.loop_body_view(loop_inst) else {
            return;
        };
        for stmt in body {
            self.collect_reachable(stmt, set);
        }
    }

    /// Collect every `TirRef` reachable from `r` — transitive operands
    /// and nested body statements — into `set`.
    /// [`Tir::walk_operands`] is shallow; this helper drives the
    /// recursion.
    pub fn collect_reachable(&self, r: TirRef, set: &mut HashSet<TirRef>) {
        if !set.insert(r) {
            return;
        }
        self.walk_operands(r, &mut |_parent, child, _kind| {
            self.collect_reachable(child, set);
        });
    }

    /// Return `true` iff `target` is reachable from `root` — `target`
    /// is `root` itself or a transitive operand / nested body
    /// statement. Allocation-free short-circuiting DFS: returns at the
    /// first hit instead of materializing the reachable set the way
    /// [`Tir::collect_reachable`] does. Used for containment probes
    /// (e.g. `collect_jump_path`'s arm selection) where the set itself
    /// is never needed.
    pub fn contains_reachable(&self, root: TirRef, target: TirRef) -> bool {
        if root == target {
            return true;
        }
        let mut found = false;
        self.walk_operands(root, &mut |_parent, child, _kind| {
            if !found && self.contains_reachable(child, target) {
                found = true;
            }
        });
        found
    }

    /// Return `true` iff every execution path through `stmts` returns
    /// or diverges — i.e. control can never reach past the end of the
    /// block. Backs sema's return-flow analysis: a non-void function
    /// whose body block does not definitely return is rejected with
    /// `DiagCode::MissingReturn`.
    ///
    /// A block definitely returns when ANY of its statements does —
    /// everything after that point is unreachable.
    pub fn block_definitely_returns(&self, stmts: &[TirRef], pool: &InternPool) -> bool {
        terminates::block_terminates(
            &self.instructions,
            &self.extra,
            stmts.iter().copied(),
            pool,
            false,
        )
    }

    /// Return whether the function body returns on every execution path.
    pub fn body_definitely_returns(&self, pool: &InternPool) -> bool {
        terminates::block_terminates(
            &self.instructions,
            &self.extra,
            self.body_stmts(),
            pool,
            false,
        )
    }

    /// The arm-termination sense used by M12 match arm typing: like
    /// [`Tir::block_definitely_returns`], but a statement arm ending in
    /// `break`/`continue` also counts as terminating (the arm is
    /// `never` at the join, never an E0135 fallthrough).
    pub fn block_definitely_terminates(&self, stmts: &[TirRef], pool: &InternPool) -> bool {
        terminates::block_terminates(
            &self.instructions,
            &self.extra,
            stmts.iter().copied(),
            pool,
            true,
        )
    }

    /// Collect the set of TirRefs evaluated on the path that reaches
    /// `target` within `body`. Returns `true` if `target` was located.
    ///
    /// Walk-down rule: a body-stmt that does NOT contain `target` runs
    /// to completion before the target's stmt is reached, so all of
    /// its operand-reachable refs are on-path. The body-stmt that DOES
    /// contain `target` recurses: into the right `IfStmt` arm, or into
    /// a nested loop's body. For an `IfStmt` we collect the cond
    /// unconditionally (it always runs); we then locate the single arm
    /// (then / a specific elif / else) that contains `target` and only
    /// recurse into that arm — sibling arms are off-path. For elif
    /// arms we also collect each preceding elif's cond (those conds
    /// executed; their bodies were skipped). For a loop we collect the
    /// condition (`while`) / bounds (`for`) unconditionally (they run
    /// before any body target) and recurse into the body up to the
    /// target — a return nested in the body reaches the return-epilogue
    /// pass, whose covering scan needs the in-loop statements.
    ///
    /// Used by the ownership pass's jump/return-exit Free scheduling
    /// to decide whether an existing Free actually fires on the exit's
    /// path. A Free anchored in a sibling arm has its `after` ref
    /// outside this set, so it's correctly classified as not-covering.
    pub fn collect_jump_path(
        &self,
        body: &[TirRef],
        target: TirRef,
        set: &mut HashSet<TirRef>,
    ) -> bool {
        self.collect_jump_path_iter(body.iter().copied(), target, set)
    }

    /// Iterator-backed form of [`Tir::collect_jump_path`] for borrowed arena lists.
    pub fn collect_jump_path_iter(
        &self,
        body: impl IntoIterator<Item = TirRef>,
        target: TirRef,
        set: &mut HashSet<TirRef>,
    ) -> bool {
        for stmt in body {
            if stmt == target {
                set.insert(target);
                return true;
            }
            if self.contains_reachable(stmt, target) {
                // `stmt` contains the target. Descend along the right arm.
                match self.inst(stmt).tag {
                    TirTag::IfStmt => {
                        let view = self.if_stmt_view(stmt);
                        set.insert(stmt);
                        self.collect_reachable(view.cond, set);
                        // Locate the arm containing `target`; only that
                        // arm's statements are on-path. Earlier elif
                        // conds executed (their bodies skipped) so
                        // collect just the conds up to the chosen arm.
                        if view
                            .then_stmts
                            .iter()
                            .any(|s| self.contains_reachable(s, target))
                        {
                            return self.collect_jump_path_iter(view.then_stmts, target, set);
                        }
                        for elif in view.elif_branches.iter() {
                            self.collect_reachable(elif.cond, set);
                            if elif.body.iter().any(|s| self.contains_reachable(s, target)) {
                                return self.collect_jump_path_iter(elif.body, target, set);
                            }
                        }
                        if let Some(else_stmts) = view.else_stmts {
                            return self.collect_jump_path_iter(else_stmts, target, set);
                        }
                        return true;
                    }
                    TirTag::WhileLoop | TirTag::ForRange => {
                        // Inner loop containing the target: the
                        // statements before the target inside its body
                        // execute on the target's path (the target's own
                        // final iteration), so collect them — the
                        // condition/bounds always run before a body
                        // target as well. The old early-return assumed
                        // only the loop-exit pass (break/continue)
                        // consumes this set, but a Return nested in the
                        // body reaches the return-epilogue pass, which
                        // must see in-loop covering frees.
                        set.insert(stmt);
                        match self.inst(stmt).tag {
                            TirTag::WhileLoop => {
                                let view = self.while_loop_view(stmt);
                                self.collect_reachable(view.cond, set);
                                return self.collect_jump_path_iter(view.body, target, set);
                            }
                            _ => {
                                let view = self.for_range_view(stmt);
                                self.collect_reachable(view.start, set);
                                self.collect_reachable(view.end, set);
                                return self.collect_jump_path_iter(view.body, target, set);
                            }
                        }
                    }
                    _ => {
                        // Other shapes (ExprStmt, etc.). The stmt's
                        // operands are evaluated as part of reaching
                        // `target`.
                        self.collect_reachable(stmt, set);
                        return true;
                    }
                }
            }
            // `stmt` does not contain target — fully evaluated before target.
            self.collect_reachable(stmt, set);
        }
        false
    }
}

mod terminates;

mod dump;
pub use dump::{TirDump, dump};

#[cfg(test)]
mod tests;

mod destructure;
pub use destructure::{DestructureField, DestructureView, destructure_extra};

mod match_;
pub use match_::{
    Discriminant, MatchArmView, MatchBinding, MatchBodyView, MatchView, TypedMatchArm,
    TypedMatchBody, match_extra,
};

mod wires;
pub use wires::{
    assign_extra, call_extra, compound_assign_extra, compound_field_assign_extra,
    field_assign_extra, for_range_extra, if_stmt_extra, struct_lit_extra, var_decl_extra,
    while_loop_extra,
};
mod views;
pub use views::{
    AssignView, CallView, CompoundAssignView, CompoundFieldAssignView, EnumLitView,
    FieldAssignView, ForRangeView, StructLitView, TirElifIter, TirElifList, TirElifView,
    TirIfStmtView, VarDeclView, WhileLoopView,
};
