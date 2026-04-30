// Some helpers (the `Display` dump, primitive `from_raw` /
// `as_range` accessors, and decoders for tags not yet emitted from
// astgen) are reachable only from the `ryo ir --emit=uir` flag
// (still TODO) and from future phases (TIR, comptime). Allow until
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
//!   `InstData` lives here, indexed by an [`ExtraRange`].
//! - `spans: Vec<Span>` — parallel to `instructions`, one span per
//!   `InstRef`. Storing spans out-of-band keeps `Inst` itself small
//!   (the tagged-enum payload already costs more than a `u32`; piling
//!   `SimpleSpan` on top would double the per-inst footprint for no
//!   reason — only diagnostics ever read spans).
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

use crate::types::{InternPool, StringId, TypeId};
use chumsky::span::{SimpleSpan, Span as _};
use std::fmt;
use std::num::NonZeroU32;

pub type Span = SimpleSpan;

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

/// A `[offset, offset+len)` slice of the `extra: Vec<u32>` arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtraRange {
    pub offset: u32,
    pub len: u32,
}

impl ExtraRange {
    pub fn as_range(self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        start..start + self.len as usize
    }
}

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
    BoolLiteral,

    /// Identifier reference, unresolved. Sema turns this into either
    /// a local/param read or a diagnostic.
    Var,

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

    /// Function call. Variable payload in `extra` — see [`call_extra`].
    Call,

    /// Variable declaration with optional annotation. Variable payload
    /// in `extra` — see [`var_decl_extra`].
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
    // Reserved for the comptime milestone:
    //   ComptimeBlock, Decl.
}

// ---------- Instruction data ----------

/// Per-instruction inline payload.
///
/// Kept as a safe `enum` rather than Zig's `extern union` (per the
/// pipeline_alignment.md risk register: avoid `unsafe`). The
/// discriminant costs a few bytes per `Inst`; that's fine for now —
/// Cranelift, not UIR, dominates compile-time memory.
#[derive(Debug, Clone, Copy)]
pub enum InstData {
    /// No operands (used by [`InstTag::ReturnVoid`]).
    None,
    Int(i64),
    Float(f64),
    Str(StringId),
    Bool(bool),
    /// Identifier name for [`InstTag::Var`].
    Var(StringId),
    /// Single operand, used by unary ops, [`InstTag::Return`], and
    /// [`InstTag::ExprStmt`].
    UnOp(InstRef),
    /// Both operands of a binary op.
    BinOp {
        lhs: InstRef,
        rhs: InstRef,
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

// ---------- Top-level UIR ----------

#[derive(Debug, Clone)]
pub struct Uir {
    pub instructions: Vec<Inst>,
    pub extra: Vec<u32>,
    pub spans: Vec<Span>,
    pub func_bodies: Vec<FuncBody>,
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
        let placeholder_span = SimpleSpan::new((), 0..0);
        Uir {
            instructions: vec![Inst {
                tag: InstTag::ReturnVoid,
                data: InstData::None,
            }],
            extra: Vec::new(),
            spans: vec![placeholder_span],
            func_bodies: Vec::new(),
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

    /// Slice of [`InstRef`]s for the top-level statements of a body.
    pub fn body_stmts(&self, body: &FuncBody) -> Vec<InstRef> {
        self.extra[body.body.as_range()]
            .iter()
            .copied()
            .map(InstRef::from_raw)
            .collect()
    }
}

// ---------- Variable-payload encoding ----------

/// Layout in `extra` for [`InstTag::Call`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  argc:  u32
///   [2..2+argc] args: InstRef.raw()
/// ```
pub mod call_extra {
    pub const NAME: usize = 0;
    pub const ARGC: usize = 1;
    pub const ARGS: usize = 2;
}

/// Layout in `extra` for [`InstTag::VarDecl`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  flags: u32  (bit 0 = mutable)
///   [2]  ty:    u32  (TypeId, or `TY_NONE_SENTINEL` if no annotation)
///   [3]  init:  InstRef.raw()
/// ```
///
/// `TY_NONE_SENTINEL` is `u32::MAX`, which is outside any plausible
/// `TypeId` range. Sema replaces it with the inferred type (and emits
/// a TIR instruction whose `ty` slot is real); codegen never sees
/// `TY_NONE_SENTINEL` after Phase 4 lands.
pub mod var_decl_extra {
    pub const NAME: usize = 0;
    pub const FLAGS: usize = 1;
    pub const TY: usize = 2;
    pub const INIT: usize = 3;
    pub const LEN: usize = 4;

    pub const FLAG_MUTABLE: u32 = 1 << 0;
    pub const TY_NONE_SENTINEL: u32 = u32::MAX;
}

// ---------- Builder ----------

/// Mutable handle for emitting UIR. `astgen` is its only caller in
/// production; tests use it directly.
pub struct UirBuilder {
    uir: Uir,
}

impl Default for UirBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl UirBuilder {
    pub fn new() -> Self {
        UirBuilder { uir: Uir::new() }
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

    pub fn bool_literal(&mut self, value: bool, span: Span) -> InstRef {
        self.push(InstTag::BoolLiteral, InstData::Bool(value), span)
    }

    pub fn var_ref(&mut self, name: StringId, span: Span) -> InstRef {
        self.push(InstTag::Var, InstData::Var(name), span)
    }

    pub fn unary(&mut self, tag: InstTag, operand: InstRef, span: Span) -> InstRef {
        debug_assert!(matches!(
            tag,
            InstTag::Neg | InstTag::Not | InstTag::Return | InstTag::ExprStmt
        ));
        self.push(tag, InstData::UnOp(operand), span)
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
    /// `InternPool::tuple`.
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
        let offset = self.extra_offset();
        self.uir.extra.push(name.raw());
        self.uir.extra.push(Self::len_u32(args.len()));
        for arg in args {
            self.uir.extra.push(arg.raw());
        }
        let len = Self::len_u32(call_extra::ARGS + args.len());
        self.push(
            InstTag::Call,
            InstData::Extra(ExtraRange { offset, len }),
            span,
        )
    }

    /// Emits a `VarDecl` with the header packed into `extra`.
    /// `ty` of `None` is encoded as [`var_decl_extra::TY_NONE_SENTINEL`].
    pub fn var_decl(
        &mut self,
        name: StringId,
        mutable: bool,
        ty: Option<TypeId>,
        initializer: InstRef,
        span: Span,
    ) -> InstRef {
        let offset = self.extra_offset();
        self.uir.extra.push(name.raw());
        self.uir.extra.push(if mutable {
            var_decl_extra::FLAG_MUTABLE
        } else {
            0
        });
        self.uir.extra.push(match ty {
            Some(t) => t.raw(),
            None => var_decl_extra::TY_NONE_SENTINEL,
        });
        self.uir.extra.push(initializer.raw());
        self.push(
            InstTag::VarDecl,
            InstData::Extra(ExtraRange {
                offset,
                len: Self::len_u32(var_decl_extra::LEN),
            }),
            span,
        )
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
        let elif_words: usize = elif_branches
            .iter()
            .map(|(_, body)| 2 + body.len())
            .sum();
        let total = 1
            + 1 + then_stmts.len()
            + 1 + elif_words
            + 1 + else_stmts.map_or(0, |s| 1 + s.len());
        self.uir.extra.reserve(total);

        let offset = self.extra_offset();
        self.uir.extra.push(cond.raw());
        self.push_ref_list(then_stmts);
        self.uir.extra.push(Self::len_u32(elif_branches.len()));
        for (elif_cond, elif_body) in elif_branches {
            self.uir.extra.push(elif_cond.raw());
            self.push_ref_list(elif_body);
        }
        match else_stmts {
            Some(stmts) => {
                self.uir.extra.push(1);
                self.push_ref_list(stmts);
            }
            None => {
                self.uir.extra.push(0);
            }
        }
        let len = Self::len_u32(self.uir.extra.len() - offset as usize);
        self.push(
            InstTag::IfStmt,
            InstData::Extra(ExtraRange { offset, len }),
            span,
        )
    }
}

// ---------- Read-side helpers ----------

/// Decoded view of an [`InstTag::Call`] payload.
pub struct CallView {
    pub name: StringId,
    pub args: Vec<InstRef>,
}

/// Decoded view of an [`InstTag::VarDecl`] payload.
pub struct VarDeclView {
    pub name: StringId,
    pub mutable: bool,
    /// `None` when the source had no annotation.
    pub ty: Option<TypeId>,
    pub initializer: InstRef,
}

pub struct ElifView {
    pub cond: InstRef,
    pub body: Vec<InstRef>,
}

pub struct IfStmtView {
    pub cond: InstRef,
    pub then_stmts: Vec<InstRef>,
    pub elif_branches: Vec<ElifView>,
    pub else_stmts: Option<Vec<InstRef>>,
}

impl Uir {
    pub fn call_view(&self, r: InstRef) -> CallView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::Call));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("Call must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let name = StringId::from_raw(slice[call_extra::NAME]);
        let argc = slice[call_extra::ARGC] as usize;
        let args = slice[call_extra::ARGS..call_extra::ARGS + argc]
            .iter()
            .copied()
            .map(InstRef::from_raw)
            .collect();
        CallView { name, args }
    }

    pub fn var_decl_view(&self, r: InstRef) -> VarDeclView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::VarDecl));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("VarDecl must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let name = StringId::from_raw(slice[var_decl_extra::NAME]);
        let mutable = slice[var_decl_extra::FLAGS] & var_decl_extra::FLAG_MUTABLE != 0;
        let ty_raw = slice[var_decl_extra::TY];
        let ty = if ty_raw == var_decl_extra::TY_NONE_SENTINEL {
            None
        } else {
            Some(TypeId::from_raw(ty_raw))
        };
        let initializer = InstRef::from_raw(slice[var_decl_extra::INIT]);
        VarDeclView {
            name,
            mutable,
            ty,
            initializer,
        }
    }

    pub fn if_stmt_view(&self, r: InstRef) -> IfStmtView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, InstTag::IfStmt));
        let range = match inst.data {
            InstData::Extra(rng) => rng,
            _ => unreachable!("IfStmt must carry InstData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let mut pos = 0;

        let cond = InstRef::from_raw(slice[pos]);
        pos += 1;

        let then_stmts = read_ref_list(slice, &mut pos);

        let elif_count = slice[pos] as usize;
        pos += 1;
        let mut elif_branches = Vec::with_capacity(elif_count);
        for _ in 0..elif_count {
            let elif_cond = InstRef::from_raw(slice[pos]);
            pos += 1;
            let body = read_ref_list(slice, &mut pos);
            elif_branches.push(ElifView {
                cond: elif_cond,
                body,
            });
        }

        let has_else = slice[pos] != 0;
        pos += 1;
        let else_stmts = if has_else {
            Some(read_ref_list(slice, &mut pos))
        } else {
            None
        };

        IfStmtView {
            cond,
            then_stmts,
            elif_branches,
            else_stmts,
        }
    }
}

fn read_ref_list(slice: &[u32], pos: &mut usize) -> Vec<InstRef> {
    let count = slice[*pos] as usize;
    *pos += 1;
    let refs = slice[*pos..*pos + count]
        .iter()
        .copied()
        .map(InstRef::from_raw)
        .collect();
    *pos += count;
    refs
}

// ---------- Pretty-printer ----------

/// Renderable wrapper for `Uir::dump`, modelled on Zig's
/// `Zir.dumpHir` listing format.
pub struct UirDump<'a> {
    pub uir: &'a Uir,
    pub pool: &'a InternPool,
}

impl Uir {
    /// Render a Zig-style listing: `%N = <op> <operands>` per line,
    /// grouped per function. Used by the (forthcoming) `ryo ir
    /// --emit=uir` command and by tests.
    pub fn dump<'a>(&'a self, pool: &'a InternPool) -> UirDump<'a> {
        UirDump { uir: self, pool }
    }
}

impl<'a> fmt::Display for UirDump<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let uir = self.uir;
        let pool = self.pool;

        // Section 1: per-function signature and the ordered list of
        // body-statement refs, so a reader can see what each function
        // actually executes.
        for body in &uir.func_bodies {
            write!(f, "fn {}(", pool.str(body.name))?;
            for (i, p) in body.params.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}: {}", pool.str(p.name), pool.display(p.ty))?;
            }
            writeln!(f, ") -> {}", pool.display(body.return_type))?;

            write!(f, "  body:")?;
            for r in uir.body_stmts(body) {
                write!(f, " %{}", r.index())?;
            }
            writeln!(f)?;
        }

        // Section 2: every instruction in index order, Zig-ZIR-style.
        // Slot 0 is the reserved sentinel (see `Uir::new`); skip it.
        if uir.instructions.len() > 1 {
            writeln!(f, "\ninstructions:")?;
            for idx in 1..uir.instructions.len() {
                let r = InstRef::from_index(idx);
                write_inst(f, uir, pool, r, 0)?;
            }
        }
        Ok(())
    }
}

fn write_inst(
    f: &mut fmt::Formatter<'_>,
    uir: &Uir,
    pool: &InternPool,
    r: InstRef,
    depth: usize,
) -> fmt::Result {
    // Print the instruction itself; sub-expressions are referenced by
    // `%idx` rather than recursively expanded — this is the whole
    // point of a flat IR. The depth parameter is reserved for future
    // block / control-flow nesting.
    let _ = depth;
    let inst = uir.inst(r);
    write!(f, "  %{} = ", r.index())?;
    match (inst.tag, inst.data) {
        (InstTag::IntLiteral, InstData::Int(v)) => writeln!(f, "int {}", v),
        (InstTag::FloatLiteral, InstData::Float(v)) => writeln!(f, "float {}", v),
        (InstTag::StrLiteral, InstData::Str(s)) => writeln!(f, "str {:?}", pool.str(s)),
        (InstTag::BoolLiteral, InstData::Bool(b)) => writeln!(f, "bool {}", b),
        (InstTag::Var, InstData::Var(s)) => writeln!(f, "var {}", pool.str(s)),
        (op, InstData::BinOp { lhs, rhs }) => {
            writeln!(f, "{} %{}, %{}", bin_op_name(op), lhs.index(), rhs.index())
        }
        (op, InstData::UnOp(operand)) => writeln!(f, "{} %{}", un_op_name(op), operand.index()),
        (InstTag::ReturnVoid, InstData::None) => writeln!(f, "ret_void"),
        (InstTag::Call, InstData::Extra(_)) => {
            let view = uir.call_view(r);
            write!(f, "call {}(", pool.str(view.name))?;
            for (i, a) in view.args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "%{}", a.index())?;
            }
            writeln!(f, ")")
        }
        (InstTag::VarDecl, InstData::Extra(_)) => {
            let view = uir.var_decl_view(r);
            let kw = if view.mutable { "mut " } else { "" };
            match view.ty {
                Some(t) => writeln!(
                    f,
                    "var_decl {}{}: {} = %{}",
                    kw,
                    pool.str(view.name),
                    pool.display(t),
                    view.initializer.index()
                ),
                None => writeln!(
                    f,
                    "var_decl {}{} = %{}",
                    kw,
                    pool.str(view.name),
                    view.initializer.index()
                ),
            }
        }
        (InstTag::IfStmt, InstData::Extra(_)) => {
            let view = uir.if_stmt_view(r);
            write!(f, "if_stmt cond=%{}", view.cond.index())?;
            write!(f, " then=[{}]", view.then_stmts.len())?;
            for elif in &view.elif_branches {
                write!(
                    f,
                    " elif(cond=%{}, body=[{}])",
                    elif.cond.index(),
                    elif.body.len()
                )?;
            }
            if let Some(else_s) = &view.else_stmts {
                write!(f, " else=[{}]", else_s.len())?;
            }
            writeln!(f)
        }
        (tag, data) => writeln!(f, "<malformed: {:?} / {:?}>", tag, data),
    }
}

fn bin_op_name(t: InstTag) -> &'static str {
    match t {
        InstTag::Add => "add",
        InstTag::Sub => "sub",
        InstTag::Mul => "mul",
        InstTag::Div => "div",
        InstTag::Mod => "mod",
        InstTag::Eq => "icmp_eq",
        InstTag::NotEq => "icmp_ne",
        InstTag::Lt => "icmp_lt",
        InstTag::Gt => "icmp_gt",
        InstTag::LtEq => "icmp_le",
        InstTag::GtEq => "icmp_ge",
        InstTag::And => "bool_and",
        InstTag::Or => "bool_or",
        _ => "?bin",
    }
}

fn un_op_name(t: InstTag) -> &'static str {
    match t {
        InstTag::Neg => "neg",
        InstTag::Not => "bool_not",
        InstTag::Return => "ret",
        InstTag::ExprStmt => "expr_stmt",
        _ => "?un",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> Span {
        SimpleSpan::new((), 0..0)
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
        assert_eq!(uir.body_stmts(body), vec![ret]);

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
        assert_eq!(view.args, vec![a, bb, cc]);
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
        assert_eq!(view.then_stmts, vec![then_ret]);
        assert!(view.elif_branches.is_empty());
        assert_eq!(view.else_stmts, Some(vec![else_ret]));
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
        assert_eq!(view.then_stmts, vec![then_ret]);
        assert_eq!(view.elif_branches.len(), 2);
        assert_eq!(view.elif_branches[0].cond, elif_cond1);
        assert_eq!(view.elif_branches[0].body, vec![elif1_ret]);
        assert_eq!(view.elif_branches[1].cond, elif_cond2);
        assert_eq!(view.elif_branches[1].body, vec![elif2_a, elif2_b]);
        assert_eq!(view.else_stmts, Some(vec![else_ret]));
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
        assert_eq!(uir.body_stmts(&uir.func_bodies[0]), vec![e1, e2, r]);
    }
}
