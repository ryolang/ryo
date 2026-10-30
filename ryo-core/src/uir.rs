// Some helpers (the `Display` dump, primitive `from_raw` /
// `as_range` accessors, and decoders for tags not yet emitted from
// astgen) are reachable only from the `ryo ir --emit=uir` flag
// and from future phases (comptime). Allow until
// then so CI's `-Dwarnings` doesn't fire on shape-only scaffolding.
#![allow(dead_code)]

//! Untyped Intermediate Representation (UIR).
//!
//! UIR is the direct structural analogue of Zig's ZIR (`src/Zir.zig`):
//! a flat instruction stream produced by `astgen` from the AST and
//! consumed by `sema`.
//!
//! ## Storage shape
//!
//! Three parallel arenas:
//!
//! - `instructions: Vec<Inst>` — fixed-size `(tag, data)` pairs. One
//!   entry per instruction; sub-expressions are *not* nested, they
//!   live as their own entries elsewhere in the same array and are
//!   referred to by [`InstRef`] indices.
//! - `extra: Vec<u32>` — variable-size payloads (call argument lists,
//!   function body statement lists, packed `VarDecl` headers). Mirrors
//!   the `extra: ArrayListUnmanaged(u32)` Zig uses in `Zir.zig` /
//!   `InternPool.zig`. Anything that doesn't fit in a single
//!   `InstData` lives here, indexed by an [`ExtraRange`]. Per-payload
//!   layouts (`*_extra` modules owning `encode`/`decode`/views) mostly
//!   live in `wires`; the exceptions are `destructure_extra` (in
//!   `destructure`), `match_extra` (in `match_`), and `enum_lit_extra`,
//!   which is shared with TIR and lives in `crate::extra`. The
//!   `wire_fixtures` module (test-only) pins each layout's exact words.
//! - `spans: Vec<Span>` — parallel to `instructions`, one span per
//!   `InstRef`. Storing spans out-of-band keeps `Inst` itself small
//!   (the tagged-enum payload already costs more than a `u32`; piling
//!   a start/end span pair on top would double the per-inst footprint
//!   for no reason — only diagnostics ever read spans).
//!
//! Function-level metadata lives in `func_bodies`. A function's body
//! is a range into `extra` listing the [`InstRef`]s of the top-level
//! statements in execution order; expression sub-trees are reached by
//! following [`InstRef`]s out of those statements.
//!
//! ## Why `NonZeroU32` for `InstRef`
//!
//! `InstRef(NonZeroU32)` makes `Option<InstRef>` a single 32-bit slot
//! via niche-filling. The 0 slot in `instructions` is reserved as a
//! never-emitted sentinel so all valid refs are non-zero. This
//! mirrors Zig's `Zir.Inst.Index` / `Zir.Inst.OptionalIndex` pair.
//!
//! ## Trusted producer
//!
//! UIR has exactly one producer (`astgen`) and one consumer (`sema`),
//! and the producer is trusted: view decoders (`call_view`,
//! `if_stmt_view`, …) `debug_assert` the tag and `unreachable!` on
//! mismatch instead of returning an error, because malformed UIR is a
//! compiler bug, not user input. If a second producer ever lands
//! (cached IR, plugins, an alternative front end), the decode paths
//! must first be converted to report an internal-error `Diag` — see
//! the `unreachable!` sites in this file.

use crate::ast::CompoundOp;
use crate::tir::ParamMode;
use crate::types::{InternPool, StringId, TypeId, VariantKind};
use std::num::NonZeroU32;

pub use crate::span::Span;

// ---------- InstRef ----------

/// Index into [`Uir::instructions`].
///
/// The wrapped `NonZeroU32` *is* the array index directly: slot 0
/// of `instructions` is reserved as an unreachable sentinel, so
/// every valid ref lands in `1..instructions.len()`. The
/// niche-filled representation makes `Option<InstRef>` a single
/// 32-bit slot.
///
/// Both [`Self::index`] (returning `usize`) and [`Self::raw`]
/// (returning `u32`) hand back the stored value unchanged. There
/// is no "0-based vs 1-based" translation: pick "slot 0 is the
/// sentinel" and the question doesn't arise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstRef(NonZeroU32);

impl ExtraValue for InstRef {
    fn from_extra(raw: u32) -> Self {
        Self::from_raw(raw)
    }

    fn to_extra(self) -> u32 {
        self.raw()
    }
}

impl InstRef {
    /// Convert from a `usize` array index. Caller guarantees `idx`
    /// is in `1..instructions.len()` (slot 0 is reserved).
    ///
    /// Panics if `idx` is zero or exceeds `u32::MAX` — a UIR with
    /// more than `u32::MAX` instructions cannot be addressed by
    /// `InstRef` and is rejected here rather than silently
    /// truncated.
    fn from_index(idx: usize) -> Self {
        let raw = u32::try_from(idx).expect("InstRef index out of range (>= 2^32)");
        InstRef(NonZeroU32::new(raw).expect("InstRef index must be >= 1"))
    }

    /// Array index into `instructions`. Equal to [`Self::raw`] cast
    /// to `usize`.
    pub fn index(self) -> usize {
        self.0.get() as usize
    }

    /// Stored handle as `u32`, for serialization into the `extra`
    /// arena. Equal to [`Self::index`] cast to `u32`.
    pub fn raw(self) -> u32 {
        self.0.get()
    }

    /// Reconstruct from a raw `u32` previously produced by
    /// [`Self::raw`]. Panics on `0` (would alias the reserved
    /// sentinel slot).
    pub fn from_raw(raw: u32) -> Self {
        InstRef(NonZeroU32::new(raw).expect("InstRef raw must be non-zero"))
    }
}

// ---------- ExtraRange ----------

pub use crate::extra::enum_lit_extra;
pub use crate::extra::{ExtraList, ExtraRange, ExtraValue};

// ---------- Instruction tags ----------

/// All UIR instruction kinds.
///
/// Reserved (commented-out) tags are listed where their phase lands —
/// adding a tag is the intended extension point. Mirrors the
/// "reserved variants" pattern from Zig's `Zir.Inst.Tag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InstTag {
    // Literals — terminal, no operands.
    IntLiteral,
    FloatLiteral,
    StrLiteral,
    /// `b"..."` payload; data is `InstData::Str` (StringId of the decoded bytes). (M8.4.2)
    BytesLiteral,
    BoolLiteral,

    /// Identifier reference, unresolved. Sema turns this into either
    /// a local/param read or a diagnostic.
    Var,

    /// Compiler-generated temporary read, used by nested destructuring
    /// and catch lowering. The payload is [`InstData::Var`] like
    /// [`InstTag::Var`], but sema resolves it exclusively through the
    /// compiler-temp side table — never the user scope — so source-level
    /// `Var` references can neither observe nor collide with temporaries.
    TempVar,

    // Binary arithmetic / comparison. Both operands in `data.bin_op`.
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,

    // Unary. Operand in `data.un_op`.
    Neg,

    /// Function call. Variable payload in `extra` — see `call_extra`.
    Call,

    /// Variable declaration with optional annotation. Variable payload
    /// in `extra` — see `var_decl_extra`.
    VarDecl,

    /// `return <expr>`. Operand in `data.un_op`.
    Return,

    /// `return` with no expression.
    ReturnVoid,

    /// Top-level expression statement (`expr` whose value is
    /// discarded). Operand in `data.un_op`. Distinct from [`Self::Return`]
    /// so codegen knows whether to discard the produced value or feed
    /// it to a terminator.
    ExprStmt,

    // Logical operators. Both operands in `data.bin_op`.
    And,
    Or,

    // Logical not. Operand in `data.un_op`.
    Not,

    /// If/elif/else statement. Variable payload in `extra`.
    IfStmt,

    /// Assignment or declaration (syntax ambiguity resolved later). Variable
    /// payload in `extra` — see `assign_or_decl_extra`.
    AssignOrDecl,

    /// Compound assignment (`+=`, `-=`, etc.). Variable payload in `extra` —
    /// see `compound_assign_extra`.
    CompoundAssign,

    /// `while cond: body`. Variable payload in `extra` — see `while_loop_extra`.
    WhileLoop,

    /// `for i in range(start, end): body`. Variable payload in `extra` — see `for_range_extra`.
    ForRange,

    /// `break` statement.
    Break,

    /// `continue` statement.
    Continue,

    /// Method call (e.g. `receiver.name(args)`). Variable payload in `extra` — see `method_call_extra`.
    MethodCall,

    /// Call-site mutable-borrow marker `&expr` (M8.3). The operand is
    /// the inner expression's ref; the `&` carries no runtime op —
    /// codegen decides pass-by-pointer from `ParamMode::Inout`.
    Borrow,

    /// Slice projection `base[start:end]` (M8.4, final spec §3).
    /// Bounds optional; see [`InstData::Slice`]. Type-checks to `strview`
    /// in sema.
    Slice,

    /// Scalar indexing `base[index]`; `InstData::BinOp` (lhs=base,
    /// rhs=index). Sema gates to bytes/bytesview (M8.4.2).
    Index,

    /// Struct literal `Name{field=value, ...}` (M9). Variable payload
    /// in `extra` — see `struct_lit_extra`. The field initializers
    /// are in source order; sema canonicalizes them against the
    /// declaration.
    StructLit,

    /// Enum variant construction `Name::Variant(args...)` (M11).
    /// Variable payload in `extra` — see [`enum_lit_extra`]. The
    /// enum type and declaration-order variant index head the
    /// payload; args are `(field_idx, value)` pairs keyed by
    /// declaration-order payload-field index (tuple variants use the
    /// synthesized `"0"`, `"1"`, … names). Unit variants encode
    /// `argc = 0`.
    EnumLit,

    /// Declared-error construction candidate. The inline data preserves
    /// the source name for shadowing; the shared enum-literal wire holds
    /// positional field indices or named-field `StringId`s.
    ErrorConstruct,

    /// Field access `object.field` (M9); see [`InstData::FieldAccess`].
    FieldAccess,

    /// Field-path assignment `p.x = v` (M9). Variable payload in
    /// `extra` — see `field_assign_extra`.
    FieldAssign,

    /// Compound field-path assignment `p.x += v` (M9). Variable
    /// payload in `extra` — see `compound_field_assign_extra`.
    CompoundFieldAssign,

    /// Destructuring assignment `pattern = value` (M10). Variable
    /// payload in `extra` — see `destructure_extra`; the value
    /// expression is inline in [`InstData::Destructure`]. Astgen
    /// lowers nested patterns into a chain: each nested sub-pattern
    /// becomes its own `Destructure` over a synthesized
    /// `__ryo_destructure_N` temp bound by the enclosing one.
    Destructure,

    /// `match scrutinee:` expression (M12). The scrutinee ref is inline
    /// in [`InstData::Match`]; the arm list is a variable payload in
    /// `extra` — see the `uir::match_` module for the wire layout.
    Match,

    /// Compiler-generated error-union dispatch. The two hidden arms use
    /// the match wire; `binding` is the optional shared `catch as` name.
    Catch,

    /// Placeholder for a statement the parser recovered from (R10:
    /// `StmtKind::Error`). No operands. Sema lowers it to a TIR
    /// `Unreachable` with the error type, so downstream flow analysis
    /// (e.g. the E0036 missing-return check) suppresses cascading
    /// diagnostics the same way it does for sema-internal errors.
    Unreachable,
    // Reserved for the comptime milestone:
    //   ComptimeBlock, Decl.
}

// ---------- Instruction data ----------

/// Per-instruction inline payload.
///
/// Kept as a safe `enum` rather than Zig's `extern union` to avoid
/// `unsafe`. The discriminant costs a few bytes per `Inst`; that's
/// fine for now — Cranelift, not UIR, dominates compile-time memory.
#[derive(Debug, Clone, Copy)]
pub enum InstData {
    /// No operands (used by [`InstTag::ReturnVoid`]).
    None,
    Int(i64),
    Float(f64),
    Str(StringId),
    Bool(bool),
    /// Identifier name for [`InstTag::Var`] and [`InstTag::TempVar`].
    Var(StringId),
    /// Single operand, used by unary ops, [`InstTag::Return`], and
    /// [`InstTag::ExprStmt`].
    UnOp(InstRef),
    /// Call-site mutable-borrow marker `&expr`; operand is the inner
    /// ref. See [`InstTag::Borrow`].
    Borrow(InstRef),
    /// Slice projection. `start`/`end` are `None` for the `s[start:]`,
    /// `s[:end]`, `s[:]` shorthands. `Option<InstRef>` niche-packs to 32
    /// bits, so this stays inline — no `extra` arena needed.
    Slice {
        base: InstRef,
        start: Option<InstRef>,
        end: Option<InstRef>,
    },
    /// Both operands of a binary op.
    BinOp {
        lhs: InstRef,
        rhs: InstRef,
    },
    /// Field access `object.field` (M9). Two 32-bit handles, so this
    /// stays inline — no `extra` arena needed.
    FieldAccess {
        object: InstRef,
        field: StringId,
    },
    /// Error-construction candidate name and its shared enum-literal
    /// argument wire. Sema resolves the name after value bindings.
    ErrorConstruct {
        name: StringId,
        args: ExtraRange,
    },
    /// Destructuring assignment `pattern = value` (M10): the value
    /// expression plus a plan of `(selector, bind)` pairs in
    /// `destructure_extra`. `by_position` selects the selector
    /// encoding: element index (positional patterns) or field-name
    /// [`StringId`] (brace patterns).
    Destructure {
        value: InstRef,
        plan: ExtraRange,
        by_position: bool,
    },
    /// `match scrutinee:` expression (M12): the scrutinee ref plus a
    /// range into `extra` of the length-prefixed arm list; see the
    /// `uir::match_` module for the wire layout.
    Match {
        scrutinee: InstRef,
        arms: ExtraRange,
    },
    Catch {
        scrutinee: InstRef,
        arms: ExtraRange,
    },
    /// Range into `extra` for variable-size payloads.
    Extra(ExtraRange),
}

#[derive(Debug, Clone, Copy)]
pub struct Inst {
    pub tag: InstTag,
    pub data: InstData,
}

// ---------- Function bodies ----------

#[derive(Debug, Clone)]
pub struct UirParam {
    pub name: StringId,
    pub ty: TypeId,
    pub mode: ParamMode,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FuncBody {
    pub name: StringId,
    pub params: Vec<UirParam>,
    pub return_type: TypeId,
    /// Range into `extra` of [`InstRef::raw`] handles for the
    /// function's top-level statements, in execution order.
    pub body: ExtraRange,
    pub span: Span,
}

// ---------- Struct declarations (M9) ----------

/// A struct declaration registered by astgen, with field types
/// already resolved through the `InternPool`. Metadata, not an
/// instruction: sema reads this table to type-check
/// [`InstTag::StructLit`] / [`InstTag::FieldAccess`].
#[derive(Debug, Clone)]
pub struct UirStructDecl {
    pub name: StringId,
    /// The struct's nominal type, declared and defined in the pool
    /// by astgen's two-phase pass.
    pub ty: TypeId,
    /// Fields in declaration order.
    pub fields: Vec<UirStructField>,
    pub span: Span,
}

/// One field of a [`UirStructDecl`].
#[derive(Debug, Clone, Copy)]
pub struct UirStructField {
    pub name: StringId,
    pub ty: TypeId,
    pub span: Span,
}

// ---------- Enum declarations (M11) ----------

/// An enum declaration registered by astgen, with variant payload
/// field types already resolved through the `InternPool`. Metadata,
/// not an instruction: sema reads this table to type-check
/// [`InstTag::EnumLit`].
#[derive(Debug, Clone)]
pub struct UirEnumDecl {
    pub name: StringId,
    /// The enum's nominal type, declared and defined in the pool
    /// by astgen's two-phase pass.
    pub ty: TypeId,
    /// Variants in declaration order.
    pub variants: Vec<UirEnumVariant>,
    pub span: Span,
}

/// One variant of a [`UirEnumDecl`].
#[derive(Debug, Clone)]
pub struct UirEnumVariant {
    pub name: StringId,
    pub kind: VariantKind,
    /// Payload fields in declaration order. Tuple-variant fields
    /// carry the synthesized `"0"`, `"1"`, … names (astgen interns
    /// them) so all payload handling is uniform.
    pub fields: Vec<UirEnumField>,
    pub span: Span,
}

/// One payload field of a [`UirEnumVariant`].
#[derive(Debug, Clone, Copy)]
pub struct UirEnumField {
    pub name: StringId,
    pub ty: TypeId,
    pub span: Span,
}

/// A nominal error declaration with its single variant payload.
#[derive(Debug, Clone)]
pub struct UirErrorDecl {
    pub name: StringId,
    pub ty: TypeId,
    pub payload: UirEnumVariant,
    pub span: Span,
}

// ---------- Top-level UIR ----------

#[derive(Debug, Clone)]
pub struct Uir {
    pub instructions: Vec<Inst>,
    pub extra: Vec<u32>,
    pub spans: Vec<Span>,
    pub func_bodies: Vec<FuncBody>,
    /// Struct declarations in source order (M9). Side table — struct
    /// decls lower to no instructions.
    pub struct_decls: Vec<UirStructDecl>,
    /// Enum declarations in source order (M11). Side table — enum
    /// decls lower to no instructions.
    pub enum_decls: Vec<UirEnumDecl>,
    /// Nominal error declarations in source order.
    pub error_decls: Vec<UirErrorDecl>,
}

impl Default for Uir {
    fn default() -> Self {
        Self::new()
    }
}

impl Uir {
    pub fn new() -> Self {
        // Slot 0 is the reserved sentinel — never read, never
        // referenced. Pushing a placeholder keeps `InstRef` indices
        // 1-based without runtime checks on every read.
        let placeholder_span = Span::EMPTY;
        Uir {
            instructions: vec![Inst {
                tag: InstTag::ReturnVoid,
                data: InstData::None,
            }],
            extra: Vec::new(),
            spans: vec![placeholder_span],
            func_bodies: Vec::new(),
            struct_decls: Vec::new(),
            enum_decls: Vec::new(),
            error_decls: Vec::new(),
        }
    }

    /// Lookup an instruction by reference.
    pub fn inst(&self, r: InstRef) -> &Inst {
        &self.instructions[r.index()]
    }

    /// Lookup the source span attached to an instruction.
    pub fn span(&self, r: InstRef) -> Span {
        self.spans[r.index()]
    }

    /// Borrow the top-level statement references from the extra arena.
    /// The view decodes each reference as it is read and does not allocate.
    pub fn body_stmts(&self, body: &FuncBody) -> ExtraList<'_, InstRef> {
        ExtraList::new(&self.extra[body.body.as_range()])
    }
}

// ---------- Builder ----------

/// Mutable handle for emitting UIR. `astgen` is its only caller in
/// production; tests use it directly.
pub struct UirBuilder {
    uir: Uir,
    /// M10: monotonic counter for nested-destructuring temp names
    /// (`__ryo_destructure_N`); see [`UirBuilder::next_destructure_temp`].
    destructure_temps_minted: u32,
}

impl Default for UirBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl UirBuilder {
    pub fn new() -> Self {
        UirBuilder {
            uir: Uir::new(),
            destructure_temps_minted: 0,
        }
    }

    pub fn finish(self) -> Uir {
        self.uir
    }

    fn push(&mut self, tag: InstTag, data: InstData, span: Span) -> InstRef {
        let idx = self.uir.instructions.len();
        self.uir.instructions.push(Inst { tag, data });
        self.uir.spans.push(span);
        InstRef::from_index(idx)
    }

    pub fn int_literal(&mut self, value: i64, span: Span) -> InstRef {
        self.push(InstTag::IntLiteral, InstData::Int(value), span)
    }

    pub fn float_literal(&mut self, value: f64, span: Span) -> InstRef {
        self.push(InstTag::FloatLiteral, InstData::Float(value), span)
    }

    pub fn str_literal(&mut self, value: StringId, span: Span) -> InstRef {
        self.push(InstTag::StrLiteral, InstData::Str(value), span)
    }

    pub fn bytes_literal(&mut self, value: StringId, span: Span) -> InstRef {
        self.push(InstTag::BytesLiteral, InstData::Str(value), span)
    }

    pub fn bool_literal(&mut self, value: bool, span: Span) -> InstRef {
        self.push(InstTag::BoolLiteral, InstData::Bool(value), span)
    }

    pub fn var_ref(&mut self, name: StringId, span: Span) -> InstRef {
        self.push(InstTag::Var, InstData::Var(name), span)
    }

    /// Emits a read of a compiler-generated temporary; see
    /// [`InstTag::TempVar`].
    pub fn temp_var_ref(&mut self, name: StringId, span: Span) -> InstRef {
        self.push(InstTag::TempVar, InstData::Var(name), span)
    }

    pub fn unary(&mut self, tag: InstTag, operand: InstRef, span: Span) -> InstRef {
        debug_assert!(matches!(
            tag,
            InstTag::Neg | InstTag::Not | InstTag::Return | InstTag::ExprStmt
        ));
        self.push(tag, InstData::UnOp(operand), span)
    }

    /// Emit a call-site mutable-borrow marker `&expr` (M8.3). The
    /// `&` is a marker, not an op — sema lowers it to its inner ref
    /// and codegen decides pass-by-pointer from `ParamMode::Inout`.
    pub fn borrow(&mut self, inner: InstRef, span: Span) -> InstRef {
        self.push(InstTag::Borrow, InstData::Borrow(inner), span)
    }

    /// Emit a slice projection `base[start:end]` (M8.4, final spec
    /// §3). `start`/`end` are `None` for the `s[start:]`, `s[:end]`,
    /// `s[:]` shorthands.
    pub fn slice(
        &mut self,
        base: InstRef,
        start: Option<InstRef>,
        end: Option<InstRef>,
        span: Span,
    ) -> InstRef {
        self.push(InstTag::Slice, InstData::Slice { base, start, end }, span)
    }

    /// Emit a scalar indexing `base[index]` (M8.4.2). Sema gates the
    /// base to bytes/bytesview and the index to `int`.
    pub fn index(&mut self, base: InstRef, index: InstRef, span: Span) -> InstRef {
        self.push(
            InstTag::Index,
            InstData::BinOp {
                lhs: base,
                rhs: index,
            },
            span,
        )
    }

    pub fn binary(&mut self, tag: InstTag, lhs: InstRef, rhs: InstRef, span: Span) -> InstRef {
        debug_assert!(matches!(
            tag,
            InstTag::Add
                | InstTag::Sub
                | InstTag::Mul
                | InstTag::Div
                | InstTag::Mod
                | InstTag::Eq
                | InstTag::NotEq
                | InstTag::Lt
                | InstTag::Gt
                | InstTag::LtEq
                | InstTag::GtEq
                | InstTag::And
                | InstTag::Or
        ));
        self.push(tag, InstData::BinOp { lhs, rhs }, span)
    }

    pub fn return_void(&mut self, span: Span) -> InstRef {
        self.push(InstTag::ReturnVoid, InstData::None, span)
    }

    /// Current `extra.len()` as a checked `u32`. The `extra` arena
    /// is addressed by `u32` offsets in [`ExtraRange`]; a UIR that
    /// outgrows `u32::MAX` words of payload cannot be encoded and
    /// is rejected here rather than silently truncated. Mirrors the
    /// overflow handling in `InternPool::intern_str` /
    /// `InternPool::anon_struct`.
    fn extra_offset(&self) -> u32 {
        u32::try_from(self.uir.extra.len()).expect("UIR extra arena exceeded u32::MAX words")
    }

    /// Convert a length-shaped `usize` (e.g. `args.len()`) to `u32`.
    /// Panics on overflow for the same reason as [`Self::extra_offset`].
    fn len_u32(len: usize) -> u32 {
        u32::try_from(len).expect("UIR list length exceeded u32::MAX")
    }

    /// Emits a `Call` with name and arg list packed into `extra`.
    pub fn call(&mut self, name: StringId, args: &[InstRef], span: Span) -> InstRef {
        let range = call_extra::encode(&mut self.uir.extra, name, args);
        self.push(InstTag::Call, InstData::Extra(range), span)
    }

    /// Emits a `VarDecl` with the header packed into `extra`.
    /// `ty` of `None` is encoded as `var_decl_extra::TY_NONE_SENTINEL`.
    pub fn var_decl(
        &mut self,
        name: StringId,
        mutable: bool,
        ty: Option<TypeId>,
        initializer: InstRef,
        span: Span,
    ) -> InstRef {
        let range = var_decl_extra::encode(&mut self.uir.extra, name, mutable, ty, initializer);
        self.push(InstTag::VarDecl, InstData::Extra(range), span)
    }

    /// Push a function body. `stmts` is the list of top-level
    /// statement [`InstRef`]s in execution order.
    pub fn add_function(
        &mut self,
        name: StringId,
        params: Vec<UirParam>,
        return_type: TypeId,
        stmts: &[InstRef],
        span: Span,
    ) {
        let offset = self.extra_offset();
        for r in stmts {
            self.uir.extra.push(r.raw());
        }
        let len = Self::len_u32(stmts.len());
        self.uir.func_bodies.push(FuncBody {
            name,
            params,
            return_type,
            body: ExtraRange { offset, len },
            span,
        });
    }

    fn push_ref_list(&mut self, refs: &[InstRef]) {
        self.uir.extra.push(Self::len_u32(refs.len()));
        for r in refs {
            self.uir.extra.push(r.raw());
        }
    }

    pub fn if_stmt(
        &mut self,
        cond: InstRef,
        then_stmts: &[InstRef],
        elif_branches: &[(InstRef, Vec<InstRef>)],
        else_stmts: Option<&[InstRef]>,
        span: Span,
    ) -> InstRef {
        let range = if_stmt_extra::encode(
            &mut self.uir.extra,
            cond,
            then_stmts,
            elif_branches,
            else_stmts,
        );
        self.push(InstTag::IfStmt, InstData::Extra(range), span)
    }

    pub fn assign_or_decl(&mut self, name: StringId, value: InstRef, span: Span) -> InstRef {
        let range = assign_or_decl_extra::encode(&mut self.uir.extra, name, value);
        self.push(InstTag::AssignOrDecl, InstData::Extra(range), span)
    }

    pub fn compound_assign(
        &mut self,
        name: StringId,
        op: CompoundOp,
        value: InstRef,
        span: Span,
    ) -> InstRef {
        let range = compound_assign_extra::encode(&mut self.uir.extra, name, op, value);
        self.push(InstTag::CompoundAssign, InstData::Extra(range), span)
    }

    pub fn while_loop(&mut self, cond: InstRef, body: &[InstRef], span: Span) -> InstRef {
        let range = while_loop_extra::encode(&mut self.uir.extra, cond, body);
        self.push(InstTag::WhileLoop, InstData::Extra(range), span)
    }

    pub fn for_range(
        &mut self,
        var_name: StringId,
        start: InstRef,
        end: InstRef,
        body: &[InstRef],
        span: Span,
    ) -> InstRef {
        let range = for_range_extra::encode(&mut self.uir.extra, var_name, start, end, body);
        self.push(InstTag::ForRange, InstData::Extra(range), span)
    }

    pub fn break_stmt(&mut self, span: Span) -> InstRef {
        self.push(InstTag::Break, InstData::None, span)
    }

    pub fn continue_stmt(&mut self, span: Span) -> InstRef {
        self.push(InstTag::Continue, InstData::None, span)
    }

    /// Parser-recovery placeholder (`StmtKind::Error`, R10). Sema
    /// lowers it to a TIR `Unreachable` so flow analysis suppresses
    /// cascading diagnostics.
    pub fn unreachable(&mut self, span: Span) -> InstRef {
        self.push(InstTag::Unreachable, InstData::None, span)
    }

    /// Emits a `MethodCall` with receiver, name, and arg list packed into `extra`.
    pub fn method_call(
        &mut self,
        receiver: InstRef,
        name: StringId,
        args: &[InstRef],
        span: Span,
    ) -> InstRef {
        let range = method_call_extra::encode(&mut self.uir.extra, receiver, name, args);
        self.push(InstTag::MethodCall, InstData::Extra(range), span)
    }

    /// Emits a `StructLit` with the struct name and the source-order
    /// field initializers packed into `extra` (M9). `name` of `None`
    /// is encoded as `struct_lit_extra::UIR_ANON_STRUCT_NAME` (M10
    /// anonymous literal); sema infers the structural type from the
    /// field values.
    pub fn struct_lit(
        &mut self,
        name: Option<StringId>,
        fields: &[(StringId, InstRef)],
        span: Span,
    ) -> InstRef {
        let range = struct_lit_extra::encode(&mut self.uir.extra, name, fields);
        self.push(InstTag::StructLit, InstData::Extra(range), span)
    }

    /// Emits an `EnumLit` `Name::Variant(args...)` (M11) with the
    /// enum type, declaration-order variant index, and
    /// `(field_idx, value)` arg pairs packed into `extra`. `field_idx`
    /// selects the payload field within the variant; unit variants
    /// pass `&[]`. `positional` records the source form: astgen sets it
    /// for the parenthesized form so sema can enforce the Brace Law on
    /// named payloads (see [`enum_lit_extra::FLAG_POSITIONAL`]).
    pub fn enum_lit(
        &mut self,
        ty: TypeId,
        variant: u32,
        positional: bool,
        args: &[(u32, InstRef)],
        span: Span,
    ) -> InstRef {
        let range = enum_lit_extra::encode(&mut self.uir.extra, ty, variant, args, positional);
        self.push(InstTag::EnumLit, InstData::Extra(range), span)
    }

    pub fn error_construct(
        &mut self,
        name: StringId,
        ty: TypeId,
        positional: bool,
        args: &[(u32, InstRef)],
        span: Span,
    ) -> InstRef {
        let range = enum_lit_extra::encode(&mut self.uir.extra, ty, 0, args, positional);
        self.push(
            InstTag::ErrorConstruct,
            InstData::ErrorConstruct { name, args: range },
            span,
        )
    }

    /// Emits a `FieldAccess` `object.field` (M9).
    pub fn field_access(&mut self, object: InstRef, field: StringId, span: Span) -> InstRef {
        self.push(
            InstTag::FieldAccess,
            InstData::FieldAccess { object, field },
            span,
        )
    }

    /// Emits a `FieldAssign` `target = value` (M9); `target` is a
    /// `FieldAccess` chain ref.
    pub fn field_assign(&mut self, target: InstRef, value: InstRef, span: Span) -> InstRef {
        let range = field_assign_extra::encode(&mut self.uir.extra, target, value);
        self.push(InstTag::FieldAssign, InstData::Extra(range), span)
    }

    /// Emits a `CompoundFieldAssign` `target op= value` (M9).
    pub fn compound_field_assign(
        &mut self,
        target: InstRef,
        op: CompoundOp,
        value: InstRef,
        span: Span,
    ) -> InstRef {
        let range = compound_field_assign_extra::encode(&mut self.uir.extra, target, op, value);
        self.push(InstTag::CompoundFieldAssign, InstData::Extra(range), span)
    }

    /// Register a resolved struct declaration in the side table (M9).
    /// Struct decls are metadata, not instructions — sema reads them
    /// to check struct literals and field accesses.
    pub fn add_struct_decl(&mut self, decl: UirStructDecl) {
        self.uir.struct_decls.push(decl);
    }

    /// Register a resolved enum declaration in the side table (M11).
    /// Enum decls are metadata, not instructions — sema reads them
    /// to check enum-literal construction.
    pub fn add_enum_decl(&mut self, decl: UirEnumDecl) {
        self.uir.enum_decls.push(decl);
    }

    pub fn add_error_decl(&mut self, decl: UirErrorDecl) {
        self.uir.error_decls.push(decl);
    }
}

mod dump;
pub use dump::UirDump;
mod destructure;
pub use destructure::UirDestructureView;
pub(crate) use destructure::destructure_extra;
mod match_;
pub use match_::{
    UirAnonField, UirAnonFieldsView, UirArgTarget, UirArmBody, UirArmBodyView, UirCatchView,
    UirFieldTarget, UirMatchArm, UirMatchArmView, UirMatchView, UirPattern, UirPatternView,
    UirVariantArgs, UirVariantArgsView, match_extra,
};
mod wires;
pub(crate) use wires::{
    assign_or_decl_extra, call_extra, compound_assign_extra, compound_field_assign_extra,
    field_assign_extra, for_range_extra, if_stmt_extra, method_call_extra, struct_lit_extra,
    var_decl_extra, while_loop_extra,
};
mod views;
pub use views::{
    AssignOrDeclView, CallView, CompoundAssignView, CompoundFieldAssignView, ElifBranches,
    ElifView, EnumLitView, FieldAssignView, ForRangeView, IfStmtView, MethodCallView,
    StructLitView, VarDeclView, WhileLoopView,
};

#[cfg(test)]
mod wire_fixtures;

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        Span::EMPTY
    }

    #[test]
    fn instref_option_is_one_word() {
        // The whole point of NonZeroU32: niche-filled Option.
        assert_eq!(
            std::mem::size_of::<Option<InstRef>>(),
            std::mem::size_of::<u32>()
        );
    }

    #[test]
    fn inst_stays_small() {
        // `Inst` is the per-instruction footprint of the whole UIR
        // arena, so it must not grow unnoticed. The
        // `Slice` payload (`base` + two niche-packed `Option<InstRef>`)
        // fits inline at 12 B — same budget as before.
        assert!(std::mem::size_of::<Inst>() <= 24, "Inst grew past 24 bytes");
    }

    #[test]
    fn slot_zero_is_reserved() {
        let uir = Uir::new();
        assert_eq!(uir.instructions.len(), 1);
        assert_eq!(uir.spans.len(), 1);
    }

    #[test]
    fn build_simple_function_and_dump() {
        let mut pool = InternPool::new();
        let main = pool.intern_str("main");

        let mut b = UirBuilder::new();
        // body of `fn main() -> int: return 1 + 2`
        let lit1 = b.int_literal(1, sp());
        let lit2 = b.int_literal(2, sp());
        let add = b.binary(InstTag::Add, lit1, lit2, sp());
        let ret = b.unary(InstTag::Return, add, sp());
        b.add_function(main, vec![], pool.int(), &[ret], sp());

        let uir = b.finish();
        assert_eq!(uir.func_bodies.len(), 1);
        let body = &uir.func_bodies[0];
        assert_eq!(
            uir.body_stmts(body).into_iter().collect::<Vec<_>>(),
            vec![ret]
        );

        // dump produces a deterministic listing.
        let out = format!("{}", uir.dump(&pool));
        assert!(out.contains("fn main() -> int"));
        assert!(out.contains("= int 1"));
        assert!(out.contains("= int 2"));
        assert!(out.contains("= add %"));
        assert!(out.contains("= ret %"));
    }

    #[test]
    fn call_payload_round_trips_through_extra() {
        let mut pool = InternPool::new();
        let foo = pool.intern_str("foo");

        let mut b = UirBuilder::new();
        let a = b.int_literal(1, sp());
        let bb = b.int_literal(2, sp());
        let cc = b.int_literal(3, sp());
        let call = b.call(foo, &[a, bb, cc], sp());

        let uir = b.finish();
        let view = uir.call_view(call);
        assert_eq!(view.name, foo);
        assert_eq!(view.args.iter().collect::<Vec<_>>(), vec![a, bb, cc]);
    }

    #[test]
    fn method_call_payload_round_trips_through_extra() {
        let mut pool = InternPool::new();
        let method = pool.intern_str("combine");

        let mut b = UirBuilder::new();
        let receiver = b.int_literal(1, sp());
        let a = b.int_literal(2, sp());
        let bb = b.int_literal(3, sp());
        let call = b.method_call(receiver, method, &[a, bb], sp());

        let uir = b.finish();
        let view = uir.method_call_view(call);
        assert_eq!(view.receiver, receiver);
        assert_eq!(view.name, method);
        assert_eq!(view.args.iter().collect::<Vec<_>>(), vec![a, bb]);
    }

    #[test]
    fn var_decl_round_trips_with_and_without_annotation() {
        let mut pool = InternPool::new();
        let x = pool.intern_str("x");
        let int_ty = pool.int();

        let mut b = UirBuilder::new();
        let init = b.int_literal(42, sp());
        let annotated = b.var_decl(x, false, Some(int_ty), init, sp());
        let inferred = b.var_decl(x, true, None, init, sp());
        let uir = b.finish();

        let v1 = uir.var_decl_view(annotated);
        assert_eq!(v1.name, x);
        assert!(!v1.mutable);
        assert_eq!(v1.ty, Some(int_ty));
        assert_eq!(v1.initializer, init);

        let v2 = uir.var_decl_view(inferred);
        assert!(v2.mutable);
        assert_eq!(v2.ty, None);
    }

    #[test]
    fn float_literal_round_trips() {
        let mut b = UirBuilder::new();
        let r = b.float_literal(2.5, sp());
        let inst = &b.uir.instructions[r.index()];
        assert!(matches!(inst.tag, InstTag::FloatLiteral));
        match inst.data {
            InstData::Float(v) => assert!((v - 2.5).abs() < 1e-12),
            _ => panic!("expected InstData::Float"),
        }
    }

    #[test]
    fn binary_accepts_ordering_and_modulo() {
        let mut b = UirBuilder::new();
        let l = b.int_literal(1, sp());
        let r = b.int_literal(2, sp());
        let _ = b.binary(InstTag::Lt, l, r, sp());
        let _ = b.binary(InstTag::Mod, l, r, sp());
    }

    #[test]
    fn if_stmt_round_trips_through_extra() {
        let mut b = UirBuilder::new();
        let cond = b.bool_literal(true, sp());
        let s1 = b.int_literal(1, sp());
        let then_ret = b.unary(InstTag::Return, s1, sp());
        let s2 = b.int_literal(2, sp());
        let else_ret = b.unary(InstTag::Return, s2, sp());

        let if_ref = b.if_stmt(cond, &[then_ret], &[], Some(&[else_ret]), sp());

        let uir = b.finish();
        let view = uir.if_stmt_view(if_ref);
        assert_eq!(view.cond, cond);
        assert_eq!(
            view.then_stmts.into_iter().collect::<Vec<_>>(),
            vec![then_ret]
        );
        assert_eq!(view.elif_branches.len(), 0);
        assert_eq!(
            view.else_stmts
                .map(|stmts| stmts.into_iter().collect::<Vec<_>>()),
            Some(vec![else_ret])
        );
    }

    #[test]
    fn if_stmt_with_elif_round_trips() {
        let mut b = UirBuilder::new();
        let cond = b.bool_literal(true, sp());
        let s1 = b.int_literal(1, sp());
        let then_ret = b.unary(InstTag::Return, s1, sp());

        let elif_cond1 = b.bool_literal(false, sp());
        let s2 = b.int_literal(2, sp());
        let elif1_ret = b.unary(InstTag::Return, s2, sp());

        let elif_cond2 = b.bool_literal(true, sp());
        let s3 = b.int_literal(3, sp());
        let s4 = b.int_literal(4, sp());
        let elif2_a = b.unary(InstTag::ExprStmt, s3, sp());
        let elif2_b = b.unary(InstTag::Return, s4, sp());

        let s5 = b.int_literal(5, sp());
        let else_ret = b.unary(InstTag::Return, s5, sp());

        let if_ref = b.if_stmt(
            cond,
            &[then_ret],
            &[
                (elif_cond1, vec![elif1_ret]),
                (elif_cond2, vec![elif2_a, elif2_b]),
            ],
            Some(&[else_ret]),
            sp(),
        );

        let uir = b.finish();
        let view = uir.if_stmt_view(if_ref);
        assert_eq!(view.cond, cond);
        assert_eq!(
            view.then_stmts.into_iter().collect::<Vec<_>>(),
            vec![then_ret]
        );
        let elifs = view.elif_branches.collect::<Vec<_>>();
        assert_eq!(elifs.len(), 2);
        assert_eq!(elifs[0].cond, elif_cond1);
        assert_eq!(
            elifs[0].body.into_iter().collect::<Vec<_>>(),
            vec![elif1_ret]
        );
        assert_eq!(elifs[1].cond, elif_cond2);
        assert_eq!(
            elifs[1].body.into_iter().collect::<Vec<_>>(),
            vec![elif2_a, elif2_b]
        );
        assert_eq!(
            view.else_stmts
                .map(|stmts| stmts.into_iter().collect::<Vec<_>>()),
            Some(vec![else_ret])
        );
    }

    #[test]
    fn borrowed_if_view_preserves_branch_order_and_empty_else() {
        let mut b = UirBuilder::new();
        let cond = b.bool_literal(true, sp());
        let then_stmt = b.int_literal(1, sp());
        let elif_cond1 = b.bool_literal(false, sp());
        let elif_cond2 = b.bool_literal(true, sp());
        let elif_stmt = b.int_literal(2, sp());
        let with_empty_else = b.if_stmt(
            cond,
            &[then_stmt],
            &[(elif_cond1, vec![]), (elif_cond2, vec![elif_stmt])],
            Some(&[]),
            sp(),
        );
        let without_else = b.if_stmt(cond, &[], &[], None, sp());
        let uir = b.finish();

        let view = uir.if_stmt_view(with_empty_else);
        assert_eq!(view.cond, cond);
        assert_eq!(
            view.then_stmts.into_iter().collect::<Vec<_>>(),
            vec![then_stmt]
        );
        let elifs = view.elif_branches.collect::<Vec<_>>();
        assert_eq!(elifs.len(), 2);
        assert_eq!(elifs[0].cond, elif_cond1);
        assert!(elifs[0].body.is_empty());
        assert_eq!(elifs[1].cond, elif_cond2);
        assert_eq!(
            elifs[1].body.into_iter().collect::<Vec<_>>(),
            vec![elif_stmt]
        );
        assert!(view.else_stmts.is_some_and(|stmts| stmts.is_empty()));

        let view = uir.if_stmt_view(without_else);
        assert!(view.then_stmts.is_empty());
        assert_eq!(view.elif_branches.len(), 0);
        assert!(view.else_stmts.is_none());
    }

    #[test]
    fn borrowed_loop_views_preserve_headers_and_bodies() {
        let mut pool = InternPool::new();
        let name = pool.intern_str("i");
        let mut b = UirBuilder::new();
        let cond = b.bool_literal(true, sp());
        let while_loop = b.while_loop(cond, &[], sp());
        let while_body = b.int_literal(3, sp());
        let while_loop_with_body = b.while_loop(cond, &[while_body], sp());
        let start = b.int_literal(0, sp());
        let end = b.int_literal(4, sp());
        let first = b.int_literal(1, sp());
        let second = b.int_literal(2, sp());
        let for_loop = b.for_range(name, start, end, &[first, second], sp());
        let for_loop_without_body = b.for_range(name, start, end, &[], sp());
        let uir = b.finish();

        let view = uir.while_loop_view(while_loop);
        assert_eq!(view.cond, cond);
        assert!(view.body.is_empty());

        let view = uir.while_loop_view(while_loop_with_body);
        assert_eq!(view.cond, cond);
        assert_eq!(view.body.into_iter().collect::<Vec<_>>(), vec![while_body]);

        let view = uir.for_range_view(for_loop);
        assert_eq!(view.var_name, name);
        assert_eq!(view.start, start);
        assert_eq!(view.end, end);
        assert_eq!(
            view.body.into_iter().collect::<Vec<_>>(),
            vec![first, second]
        );

        let view = uir.for_range_view(for_loop_without_body);
        assert_eq!(view.var_name, name);
        assert_eq!(view.start, start);
        assert_eq!(view.end, end);
        assert!(view.body.is_empty());

        assert!(uir.while_loop_view(while_loop).body.is_empty());
        assert_eq!(
            uir.while_loop_view(while_loop_with_body)
                .body
                .into_iter()
                .collect::<Vec<_>>(),
            vec![while_body]
        );
        assert_eq!(
            uir.for_range_view(for_loop)
                .body
                .into_iter()
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(uir.for_range_view(for_loop_without_body).body.is_empty());
    }

    #[test]
    fn body_stmts_preserves_order() {
        let mut pool = InternPool::new();
        let main = pool.intern_str("main");

        let mut b = UirBuilder::new();
        let s1 = b.int_literal(1, sp());
        let e1 = b.unary(InstTag::ExprStmt, s1, sp());
        let s2 = b.int_literal(2, sp());
        let e2 = b.unary(InstTag::ExprStmt, s2, sp());
        let s3 = b.int_literal(3, sp());
        let r = b.unary(InstTag::Return, s3, sp());
        b.add_function(main, vec![], pool.int(), &[e1, e2, r], sp());
        let uir = b.finish();
        assert_eq!(
            uir.body_stmts(&uir.func_bodies[0])
                .into_iter()
                .collect::<Vec<_>>(),
            vec![e1, e2, r]
        );
    }

    #[test]
    fn assign_or_decl_round_trips_through_extra() {
        let mut pool = InternPool::new();
        let x = pool.intern_str("x");

        let mut b = UirBuilder::new();
        let value = b.int_literal(42, sp());
        let assign_or_decl = b.assign_or_decl(x, value, sp());

        let uir = b.finish();
        let view = uir.assign_or_decl_view(assign_or_decl);
        assert_eq!(view.name, x);
        assert_eq!(view.value, value);
    }

    #[test]
    fn struct_lit_round_trips_through_extra() {
        let mut pool = InternPool::new();
        let point = pool.intern_str("Point");
        let x = pool.intern_str("x");
        let y = pool.intern_str("y");

        let mut b = UirBuilder::new();
        let one = b.float_literal(1.0, sp());
        let two = b.float_literal(2.0, sp());
        let lit = b.struct_lit(Some(point), &[(x, one), (y, two)], sp());

        let uir = b.finish();
        let view = uir.struct_lit_view(lit);
        assert_eq!(view.name, Some(point));
        assert_eq!(view.fields().collect::<Vec<_>>(), vec![(x, one), (y, two)]);
    }

    #[test]
    fn anon_struct_lit_round_trips_none_name() {
        let mut pool = InternPool::new();
        let x = pool.intern_str("x");

        let mut b = UirBuilder::new();
        let one = b.int_literal(1, sp());
        let lit = b.struct_lit(None, &[(x, one)], sp());

        let uir = b.finish();
        let view = uir.struct_lit_view(lit);
        assert_eq!(view.name, None);
        assert_eq!(view.fields().collect::<Vec<_>>(), vec![(x, one)]);
    }

    #[test]
    fn add_enum_decl_round_trips_name_variants_fields() {
        let mut pool = InternPool::new();
        let shape = pool.intern_str("Shape");
        let circle = pool.intern_str("Circle");
        let pair = pool.intern_str("Pair");
        let (n0, n1) = (pool.intern_str("0"), pool.intern_str("1"));
        let rect = pool.intern_str("Rect");
        let (w, h) = (pool.intern_str("w"), pool.intern_str("h"));
        let (int_ty, float_ty) = (pool.int(), pool.float());
        let enum_ty = pool.declare_enum(shape);

        let mut b = UirBuilder::new();
        b.add_enum_decl(UirEnumDecl {
            name: shape,
            ty: enum_ty,
            variants: vec![
                UirEnumVariant {
                    name: circle,
                    kind: VariantKind::Unit,
                    fields: vec![],
                    span: sp(),
                },
                UirEnumVariant {
                    name: pair,
                    kind: VariantKind::Tuple,
                    fields: vec![
                        UirEnumField {
                            name: n0,
                            ty: int_ty,
                            span: sp(),
                        },
                        UirEnumField {
                            name: n1,
                            ty: float_ty,
                            span: sp(),
                        },
                    ],
                    span: sp(),
                },
                UirEnumVariant {
                    name: rect,
                    kind: VariantKind::Named,
                    fields: vec![
                        UirEnumField {
                            name: w,
                            ty: float_ty,
                            span: sp(),
                        },
                        UirEnumField {
                            name: h,
                            ty: float_ty,
                            span: sp(),
                        },
                    ],
                    span: sp(),
                },
            ],
            span: sp(),
        });

        let uir = b.finish();
        assert_eq!(uir.enum_decls.len(), 1);
        let decl = &uir.enum_decls[0];
        assert_eq!(decl.name, shape);
        assert_eq!(decl.ty, enum_ty);
        assert_eq!(decl.variants.len(), 3);

        assert_eq!(decl.variants[0].name, circle);
        assert_eq!(decl.variants[0].kind, VariantKind::Unit);
        assert!(decl.variants[0].fields.is_empty());

        assert_eq!(decl.variants[1].name, pair);
        assert_eq!(decl.variants[1].kind, VariantKind::Tuple);
        assert_eq!(decl.variants[1].fields.len(), 2);
        assert_eq!(decl.variants[1].fields[0].name, n0);
        assert_eq!(decl.variants[1].fields[0].ty, int_ty);
        assert_eq!(decl.variants[1].fields[1].name, n1);
        assert_eq!(decl.variants[1].fields[1].ty, float_ty);

        assert_eq!(decl.variants[2].name, rect);
        assert_eq!(decl.variants[2].kind, VariantKind::Named);
        assert_eq!(decl.variants[2].fields.len(), 2);
        assert_eq!(decl.variants[2].fields[0].name, w);
        assert_eq!(decl.variants[2].fields[0].ty, float_ty);
        assert_eq!(decl.variants[2].fields[1].name, h);
        assert_eq!(decl.variants[2].fields[1].ty, float_ty);
    }

    #[test]
    fn enum_lit_round_trips_through_extra() {
        let mut pool = InternPool::new();
        let shape = pool.intern_str("Shape");
        let enum_ty = pool.declare_enum(shape);

        let mut b = UirBuilder::new();
        let w = b.float_literal(3.0, sp());
        let h = b.float_literal(4.0, sp());
        let lit = b.enum_lit(enum_ty, 1, false, &[(0, w), (1, h)], sp());

        let uir = b.finish();
        let view = uir.enum_lit_view(lit);
        assert_eq!(view.ty, enum_ty);
        assert_eq!(view.variant_index, 1);
        assert!(!view.positional, "braced named form clears the flag");
        assert_eq!(view.fields().collect::<Vec<_>>(), vec![(0, w), (1, h)]);

        // Wire layout is pinned: [TY, VARIANT, ARGC, FLAGS, (FIELD_IDX, REF) x ARGC].
        let inst = uir.inst(lit);
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            other => panic!("expected InstData::Extra, got {:?}", other),
        };
        let slice = &uir.extra[range.as_range()];
        assert_eq!(slice[0], enum_ty.raw());
        assert_eq!(slice[1], 1);
        assert_eq!(slice[2], 2);
        assert_eq!(slice[3], 0);
        assert_eq!(slice[4], 0);
        assert_eq!(slice[5], w.raw());
        assert_eq!(slice[6], 1);
        assert_eq!(slice[7], h.raw());
    }

    #[test]
    fn enum_lit_dump_lists_variant_and_fields() {
        let mut pool = InternPool::new();
        let shape = pool.intern_str("Shape");
        let pair = pool.intern_str("Pair");
        let float_ty = pool.float();
        let enum_ty = pool.declare_enum(shape);
        pool.define_enum(
            enum_ty,
            shape,
            &[crate::types::EnumVariantDef {
                name: pair,
                kind: crate::types::VariantKind::Tuple,
                fields: vec![(pair, float_ty), (pair, float_ty)],
            }],
            crate::types::StructFlags::default(),
        );

        let mut b = UirBuilder::new();
        let w = b.float_literal(3.0, sp());
        let h = b.float_literal(4.0, sp());
        b.enum_lit(enum_ty, 0, false, &[(0, w), (1, h)], sp());
        let uir = b.finish();

        let out = format!("{}", uir.dump(&pool));
        assert!(
            out.contains("= enum_lit Shape variant=0 {0: %1, 1: %2}"),
            "got:\n{out}"
        );
        assert!(!out.contains("<malformed"), "got:\n{out}");
    }

    #[test]
    fn enum_lit_positional_flag_round_trips() {
        let mut pool = InternPool::new();
        let shape = pool.intern_str("Shape");
        let enum_ty = pool.declare_enum(shape);

        let mut b = UirBuilder::new();
        let v = b.float_literal(5.0, sp());
        let lit = b.enum_lit(enum_ty, 0, true, &[(0, v)], sp());

        let uir = b.finish();
        let view = uir.enum_lit_view(lit);
        assert!(view.positional, "parenthesized form sets the flag");
        assert_eq!(view.fields().collect::<Vec<_>>(), vec![(0, v)]);
    }

    #[test]
    fn enum_lit_unit_variant_round_trips_empty_args() {
        let mut pool = InternPool::new();
        let shape = pool.intern_str("Shape");
        let enum_ty = pool.declare_enum(shape);

        let mut b = UirBuilder::new();
        let lit = b.enum_lit(enum_ty, 0, false, &[], sp());

        let uir = b.finish();
        let view = uir.enum_lit_view(lit);
        assert_eq!(view.ty, enum_ty);
        assert_eq!(view.variant_index, 0);
        assert!(!view.positional);
        assert_eq!(view.fields().count(), 0);
    }

    #[test]
    fn field_access_round_trips() {
        let mut pool = InternPool::new();
        let x = pool.intern_str("x");

        let mut b = UirBuilder::new();
        let obj = b.var_ref(x, sp());
        let access = b.field_access(obj, x, sp());

        let uir = b.finish();
        match uir.inst(access).data {
            InstData::FieldAccess { object, field } => {
                assert_eq!(object, obj);
                assert_eq!(field, x);
            }
            other => panic!("expected FieldAccess data, got {:?}", other),
        }
    }

    #[test]
    fn compound_assign_round_trips_through_extra() {
        let mut pool = InternPool::new();
        let x = pool.intern_str("x");

        let mut b = UirBuilder::new();
        let value = b.int_literal(10, sp());
        let compound_assign = b.compound_assign(x, CompoundOp::Add, value, sp());

        let uir = b.finish();
        let view = uir.compound_assign_view(compound_assign);
        assert_eq!(view.name, x);
        assert_eq!(view.op, CompoundOp::Add);
        assert_eq!(view.value, value);
    }
}
