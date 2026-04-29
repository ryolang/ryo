// Some helpers (the `Display` dump, primitive `from_raw` /
// `as_range` accessors, and `unreachable` builder) are reachable
// only from the (still TODO) `ryo ir --emit=tir` flag and from
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

use crate::types::{InternPool, StringId, TypeId};
use chumsky::span::{SimpleSpan, Span as _};
use std::fmt;
use std::num::NonZeroU32;

pub type Span = SimpleSpan;

// ---------- TirRef ----------

/// Index into a single [`Tir`]'s `instructions`. Refs are scoped to
/// the function body that produced them — a `TirRef` from one `Tir`
/// is meaningless in another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TirRef(NonZeroU32);

impl TirRef {
    fn from_index(idx: usize) -> Self {
        let raw = u32::try_from(idx).expect("TirRef index out of range (>= 2^32)");
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
}

// ---------- ExtraRange ----------

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

/// All TIR instruction kinds.
///
/// Compared with [`crate::uir::InstTag`], TIR tags are *lowered*:
/// the type information that disambiguates polymorphic UIR ops
/// (`Add` works for any numeric type once we have floats) lives in
/// [`TypedInst::ty`], and the tag itself names the concrete machine
/// operation. Today the language only has `int`, `bool`, and `str`,
/// so the lowered set is mostly a 1:1 rename — `IAdd`, `INeg` —
/// but the shape is what lets float/SIMD variants slot in as new
/// arms without reshuffling sema or codegen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TirTag {
    // Constants — terminal, no operands.
    IntConst,
    FloatConst,
    BoolConst,
    StrConst,

    /// Read of a local (parameter or `let`-bound). Resolved to a
    /// `StringId` so codegen's `HashMap<StringId, Variable>` lookup
    /// is the same as today; future phases may swap this for a
    /// `LocalSlot(u32)` once we have a proper local table.
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

    /// Function call (user or builtin). Variable payload in `extra`
    /// — see [`call_extra`].
    Call,

    /// Variable declaration with an initializer. Variable payload in
    /// `extra` — see [`var_decl_extra`]. The `ty` slot of the
    /// `TypedInst` carries the *variable's* resolved type (matches
    /// the side-table behaviour from the Phase-3 interim sema).
    VarDecl,

    /// `return <expr>`. Operand in `TirData::UnOp`.
    Return,

    /// `return` with no expression.
    ReturnVoid,

    /// Top-level expression statement (value discarded). Operand in
    /// `TirData::UnOp`.
    ExprStmt,

    /// Inserted by sema at the point of an unrecoverable type error
    /// so the rest of the body still produces well-formed TIR. Has
    /// `ty == pool.error_type()`. Codegen must never see one — the
    /// driver short-circuits on `sink.has_errors()`.
    Unreachable,
    // Reserved for the control-flow milestone:
    //   Br, CondBr, Block.
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
    BinOp { lhs: TirRef, rhs: TirRef },
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
    pub span: Span,
}

/// One function body's typed instruction stream.
///
/// Per the doc (§4.1): "TIR is per-function-body, not per-program."
/// Each `Tir` owns its own `instructions` / `extra` / `spans`
/// arenas; refs are scoped to the body. This is the shape that lets
/// monomorphization (Phase 5) clone-and-substitute one body without
/// renumbering everything else.
#[derive(Debug, Clone)]
pub struct Tir {
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
}

impl Tir {
    pub fn inst(&self, r: TirRef) -> &TypedInst {
        &self.instructions[r.index()]
    }

    pub fn span(&self, r: TirRef) -> Span {
        self.spans[r.index()]
    }

    pub fn body_stmts(&self) -> Vec<TirRef> {
        self.extra[self.body.as_range()]
            .iter()
            .copied()
            .map(TirRef::from_raw)
            .collect()
    }
}

// ---------- Variable-payload encoding ----------

/// Layout in `extra` for [`TirTag::Call`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  argc:  u32
///   [2..2+argc] args: TirRef.raw()
/// ```
pub mod call_extra {
    pub const NAME: usize = 0;
    pub const ARGC: usize = 1;
    pub const ARGS: usize = 2;
}

/// Layout in `extra` for [`TirTag::VarDecl`]:
///
/// ```text
///   [0]  name:    StringId
///   [1]  flags:   u32  (bit 0 = mutable)
///   [2]  init:    TirRef.raw()
/// ```
///
/// Unlike UIR's `VarDecl`, there is no `TY_NONE_SENTINEL`: the
/// resolved variable type lives in the `TypedInst.ty` slot, never
/// `Option`-shaped at this layer.
pub mod var_decl_extra {
    pub const NAME: usize = 0;
    pub const FLAGS: usize = 1;
    pub const INIT: usize = 2;
    pub const LEN: usize = 3;

    pub const FLAG_MUTABLE: u32 = 1 << 0;
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
}

impl TirBuilder {
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

    pub fn var(&mut self, name: StringId, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::Var, ty, TirData::Var(name), span)
    }

    pub fn unary(&mut self, tag: TirTag, ty: TypeId, operand: TirRef, span: Span) -> TirRef {
        debug_assert!(matches!(
            tag,
            TirTag::INeg | TirTag::Return | TirTag::ExprStmt
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
        ));
        self.push(tag, ty, TirData::BinOp { lhs, rhs }, span)
    }

    pub fn return_void(&mut self, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::ReturnVoid, ty, TirData::None, span)
    }

    pub fn unreachable(&mut self, ty: TypeId, span: Span) -> TirRef {
        self.push(TirTag::Unreachable, ty, TirData::None, span)
    }

    fn extra_offset(&self) -> u32 {
        u32::try_from(self.extra.len()).expect("TIR extra arena exceeded u32::MAX words")
    }

    fn len_u32(len: usize) -> u32 {
        u32::try_from(len).expect("TIR list length exceeded u32::MAX")
    }

    /// Emit a `Call` with name and arg list packed into `extra`.
    /// `ty` is the call's *return* type.
    pub fn call(&mut self, name: StringId, args: &[TirRef], ty: TypeId, span: Span) -> TirRef {
        let offset = self.extra_offset();
        self.extra.push(name.raw());
        self.extra.push(Self::len_u32(args.len()));
        for a in args {
            self.extra.push(a.raw());
        }
        let len = Self::len_u32(call_extra::ARGS + args.len());
        self.push(
            TirTag::Call,
            ty,
            TirData::Extra(ExtraRange { offset, len }),
            span,
        )
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
        let offset = self.extra_offset();
        self.extra.push(name.raw());
        self.extra.push(if mutable {
            var_decl_extra::FLAG_MUTABLE
        } else {
            0
        });
        self.extra.push(initializer.raw());
        self.push(
            TirTag::VarDecl,
            var_ty,
            TirData::Extra(ExtraRange {
                offset,
                len: Self::len_u32(var_decl_extra::LEN),
            }),
            span,
        )
    }

    /// Finish: bake in the body statement list and produce the
    /// finished [`Tir`].
    pub fn finish(mut self, stmts: &[TirRef]) -> Tir {
        let offset = self.extra_offset();
        for r in stmts {
            self.extra.push(r.raw());
        }
        let len = Self::len_u32(stmts.len());
        Tir {
            name: self.name,
            params: self.params,
            return_type: self.return_type,
            instructions: self.instructions,
            extra: self.extra,
            spans: self.spans,
            body: ExtraRange { offset, len },
            span: self.span,
        }
    }
}

// ---------- Read-side helpers ----------

pub struct CallView {
    pub name: StringId,
    pub args: Vec<TirRef>,
}

pub struct VarDeclView {
    pub name: StringId,
    pub mutable: bool,
    pub initializer: TirRef,
}

impl Tir {
    pub fn call_view(&self, r: TirRef) -> CallView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::Call));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("Call must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let name = StringId::from_raw(slice[call_extra::NAME]);
        let argc = slice[call_extra::ARGC] as usize;
        let args = slice[call_extra::ARGS..call_extra::ARGS + argc]
            .iter()
            .copied()
            .map(TirRef::from_raw)
            .collect();
        CallView { name, args }
    }

    pub fn var_decl_view(&self, r: TirRef) -> VarDeclView {
        let inst = self.inst(r);
        debug_assert!(matches!(inst.tag, TirTag::VarDecl));
        let range = match inst.data {
            TirData::Extra(rng) => rng,
            _ => unreachable!("VarDecl must carry TirData::Extra"),
        };
        let slice = &self.extra[range.as_range()];
        let name = StringId::from_raw(slice[var_decl_extra::NAME]);
        let mutable = slice[var_decl_extra::FLAGS] & var_decl_extra::FLAG_MUTABLE != 0;
        let initializer = TirRef::from_raw(slice[var_decl_extra::INIT]);
        VarDeclView {
            name,
            mutable,
            initializer,
        }
    }
}

// ---------- Pretty-printer ----------

/// Renderable wrapper for `Tir::dump`, modelled on Zig's
/// `Air.dumpAir` listing format. One section per function.
pub struct TirDump<'a> {
    pub tirs: &'a [Tir],
    pub pool: &'a InternPool,
}

pub fn dump<'a>(tirs: &'a [Tir], pool: &'a InternPool) -> TirDump<'a> {
    TirDump { tirs, pool }
}

impl<'a> fmt::Display for TirDump<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for tir in self.tirs {
            write!(f, "fn {}(", self.pool.str(tir.name))?;
            for (i, p) in tir.params.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}: {}", self.pool.str(p.name), self.pool.display(p.ty))?;
            }
            writeln!(f, ") -> {}", self.pool.display(tir.return_type))?;

            write!(f, "  body:")?;
            for r in tir.body_stmts() {
                write!(f, " %{}", r.index())?;
            }
            writeln!(f)?;

            // Skip slot 0 (reserved sentinel).
            for idx in 1..tir.instructions.len() {
                let r = TirRef::from_index(idx);
                write_inst(f, tir, self.pool, r)?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

fn write_inst(f: &mut fmt::Formatter<'_>, tir: &Tir, pool: &InternPool, r: TirRef) -> fmt::Result {
    let inst = tir.inst(r);
    write!(f, "  %{} : {} = ", r.index(), pool.display(inst.ty))?;
    match (inst.tag, inst.data) {
        (TirTag::IntConst, TirData::Int(v)) => writeln!(f, "iconst {}", v),
        (TirTag::FloatConst, TirData::Float(v)) => writeln!(f, "fconst {}", v),
        (TirTag::BoolConst, TirData::Bool(b)) => writeln!(f, "bconst {}", b),
        (TirTag::StrConst, TirData::Str(s)) => writeln!(f, "sconst {:?}", pool.str(s)),
        (TirTag::Var, TirData::Var(s)) => writeln!(f, "var {}", pool.str(s)),
        (op, TirData::BinOp { lhs, rhs }) => {
            writeln!(f, "{} %{}, %{}", bin_op_name(op), lhs.index(), rhs.index())
        }
        (op, TirData::UnOp(operand)) => writeln!(f, "{} %{}", un_op_name(op), operand.index()),
        (TirTag::ReturnVoid, TirData::None) => writeln!(f, "ret_void"),
        (TirTag::Unreachable, TirData::None) => writeln!(f, "unreachable"),
        (TirTag::Call, TirData::Extra(_)) => {
            let view = tir.call_view(r);
            write!(f, "call {}(", pool.str(view.name))?;
            for (i, a) in view.args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "%{}", a.index())?;
            }
            writeln!(f, ")")
        }
        (TirTag::VarDecl, TirData::Extra(_)) => {
            let view = tir.var_decl_view(r);
            let kw = if view.mutable { "mut " } else { "" };
            writeln!(
                f,
                "var_decl {}{} = %{}",
                kw,
                pool.str(view.name),
                view.initializer.index()
            )
        }
        (tag, data) => writeln!(f, "<malformed: {:?} / {:?}>", tag, data),
    }
}

fn bin_op_name(t: TirTag) -> &'static str {
    match t {
        TirTag::IAdd => "iadd",
        TirTag::ISub => "isub",
        TirTag::IMul => "imul",
        TirTag::ISDiv => "isdiv",
        TirTag::IMod => "imod",
        TirTag::ICmpEq => "icmp_eq",
        TirTag::ICmpNe => "icmp_ne",
        TirTag::ICmpLt => "icmp_lt",
        TirTag::ICmpLe => "icmp_le",
        TirTag::ICmpGt => "icmp_gt",
        TirTag::ICmpGe => "icmp_ge",
        TirTag::FAdd => "fadd",
        TirTag::FSub => "fsub",
        TirTag::FMul => "fmul",
        TirTag::FDiv => "fdiv",
        TirTag::FCmpEq => "fcmp_eq",
        TirTag::FCmpNe => "fcmp_ne",
        TirTag::FCmpLt => "fcmp_lt",
        TirTag::FCmpLe => "fcmp_le",
        TirTag::FCmpGt => "fcmp_gt",
        TirTag::FCmpGe => "fcmp_ge",
        _ => "?bin",
    }
}

fn un_op_name(t: TirTag) -> &'static str {
    match t {
        TirTag::INeg => "ineg",
        TirTag::Return => "ret",
        TirTag::ExprStmt => "expr_stmt",
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
    fn tirref_option_is_one_word() {
        assert_eq!(
            std::mem::size_of::<Option<TirRef>>(),
            std::mem::size_of::<u32>()
        );
    }

    #[test]
    fn build_simple_function_and_dump() {
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let main = pool.intern_str("main");

        let mut b = TirBuilder::new(main, vec![], int_ty, sp());
        let lit1 = b.int_const(1, int_ty, sp());
        let lit2 = b.int_const(2, int_ty, sp());
        let add = b.binary(TirTag::IAdd, int_ty, lit1, lit2, sp());
        let ret = b.unary(TirTag::Return, pool.void(), add, sp());
        let tir = b.finish(&[ret]);

        assert_eq!(tir.body_stmts(), vec![ret]);
        let out = format!("{}", dump(std::slice::from_ref(&tir), &pool));
        assert!(out.contains("fn main() -> int"));
        assert!(out.contains("= iconst 1"));
        assert!(out.contains("= iadd %"));
        assert!(out.contains("= ret %"));
    }

    #[test]
    fn call_payload_round_trips() {
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let foo = pool.intern_str("foo");
        let main = pool.intern_str("main");

        let mut b = TirBuilder::new(main, vec![], int_ty, sp());
        let a = b.int_const(1, int_ty, sp());
        let bb = b.int_const(2, int_ty, sp());
        let call = b.call(foo, &[a, bb], int_ty, sp());
        let ret = b.unary(TirTag::Return, pool.void(), call, sp());
        let tir = b.finish(&[ret]);

        let view = tir.call_view(call);
        assert_eq!(view.name, foo);
        assert_eq!(view.args, vec![a, bb]);
    }

    #[test]
    fn var_decl_round_trips() {
        let mut pool = InternPool::new();
        let int_ty = pool.int();
        let x = pool.intern_str("x");
        let main = pool.intern_str("main");

        let mut b = TirBuilder::new(main, vec![], int_ty, sp());
        let init = b.int_const(42, int_ty, sp());
        let decl = b.var_decl(x, true, int_ty, init, sp());
        let zero = b.int_const(0, int_ty, sp());
        let ret = b.unary(TirTag::Return, pool.void(), zero, sp());
        let tir = b.finish(&[decl, ret]);

        let v = tir.var_decl_view(decl);
        assert_eq!(v.name, x);
        assert!(v.mutable);
        assert_eq!(v.initializer, init);
        assert_eq!(tir.inst(decl).ty, int_ty);
    }

    #[test]
    fn float_const_and_fadd_round_trip() {
        let mut pool = InternPool::new();
        let main = pool.intern_str("main");
        let mut b = TirBuilder::new(main, vec![], pool.int(), sp());

        // Float operands feed the float ops; integer operands feed
        // the integer ops. The TIR builder doesn't enforce operand
        // shape (sema does), but mirroring real usage keeps the
        // test honest.
        let lit_f1 = b.float_const(1.5, pool.float(), sp());
        let lit_f2 = b.float_const(2.5, pool.float(), sp());
        let lit_i1 = b.int_const(1, pool.int(), sp());
        let lit_i2 = b.int_const(2, pool.int(), sp());

        let fadd = b.binary(TirTag::FAdd, pool.float(), lit_f1, lit_f2, sp());
        let icmp_lt = b.binary(TirTag::ICmpLt, pool.bool_(), lit_i1, lit_i2, sp());
        let imod = b.binary(TirTag::IMod, pool.int(), lit_i1, lit_i2, sp());

        // Result types match the operator category: float arithmetic
        // stays float, ordering produces bool, integer modulo stays int.
        assert_eq!(b.ty_of(fadd), pool.float());
        assert_eq!(b.ty_of(icmp_lt), pool.bool_());
        assert_eq!(b.ty_of(imod), pool.int());
    }

    #[test]
    fn unreachable_inst_carries_error_type() {
        let mut pool = InternPool::new();
        let err_ty = pool.error_type();
        let main = pool.intern_str("main");

        let mut b = TirBuilder::new(main, vec![], pool.int(), sp());
        let u = b.unreachable(err_ty, sp());
        let tir = b.finish(&[u]);
        assert!(matches!(tir.inst(u).tag, TirTag::Unreachable));
        assert_eq!(tir.inst(u).ty, err_ty);
    }
}
