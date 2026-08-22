//! Surface-syntax AST.
//!
//! Identifiers, type names, and string literals are stored as
//! `StringId` handles into the compilation's `InternPool`.
//! Tree-drawing lives in `crate::ast_pretty`.
//!
//! ## Storage shape
//!
//! Three parallel arenas, mirroring the UIR encoding in
//! `crate::uir`:
//!
//! - `nodes: Vec<Node>` — fixed-size `(tag, data)` pairs. Statements
//!   and expressions share one arena and one index type; sub-nodes
//!   are *not* nested or boxed, they live as their own entries
//!   elsewhere in the same array and are referred to by [`NodeRef`]
//!   indices.
//! - `extra: Vec<u32>` — variable-size payloads (call argument lists,
//!   block bodies, parameter lists, packed `Ident`/`TypeExpr`
//!   headers). Anything that doesn't fit in a single [`NodeData`]
//!   lives here, indexed by an [`ExtraRange`].
//! - `spans: Vec<SimpleSpan>` — parallel to `nodes`, one span per
//!   [`NodeRef`]. Storing spans out-of-band keeps `Node` small —
//!   only diagnostics and the pretty-printer ever read spans.
//!
//! The top-level statement list is itself a range into `extra`
//! ([`Ast::top_level`]); everything below it is reached by following
//! [`NodeRef`]s out of those statements.
//!
//! ## Why `NonZeroU32` for `NodeRef`
//!
//! `NodeRef(NonZeroU32)` makes `Option<NodeRef>` a single 32-bit
//! slot via niche-filling, and the raw `0` word doubles as the
//! `None` sentinel when optional children are packed into `extra`.
//! The 0 slot in `nodes` is reserved as a never-emitted sentinel so
//! all valid refs are non-zero.
//!
//! ## Trusted producer
//!
//! The AST has exactly one producer (the parser) and two consumers
//! (`astgen` and `ast_pretty`), and the producer is trusted: view
//! decoders (`call_view`, `if_stmt_view`, …) `debug_assert` the tag
//! and `unreachable!` on mismatch instead of returning an error,
//! because a malformed AST is a compiler bug, not user input. If a
//! second producer ever lands (cached trees, plugins), the decode
//! paths must first be converted to report an internal-error `Diag`.

use crate::tir::ParamMode;
use crate::types::StringId;
use chumsky::span::{SimpleSpan, Span as _};
use std::fmt;
use std::num::NonZeroU32;

// ---------- NodeRef ----------

/// Index into [`Ast::nodes`].
///
/// The wrapped `NonZeroU32` *is* the array index directly: slot 0
/// of `nodes` is reserved as an unreachable sentinel, so every
/// valid ref lands in `1..nodes.len()`. The niche-filled
/// representation makes `Option<NodeRef>` a single 32-bit slot.
///
/// One index type covers both statements and expressions — like
/// UIR's `InstRef`, which mixes both in one instruction stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeRef(NonZeroU32);

impl NodeRef {
    /// Convert from a `usize` array index. Caller guarantees `idx`
    /// is in `1..nodes.len()` (slot 0 is reserved).
    ///
    /// Panics if `idx` is zero or exceeds `u32::MAX` — an AST with
    /// more than `u32::MAX` nodes cannot be addressed by `NodeRef`
    /// and is rejected here rather than silently truncated.
    fn from_index(idx: usize) -> Self {
        let raw = u32::try_from(idx).expect("NodeRef index out of range (>= 2^32)");
        NodeRef(NonZeroU32::new(raw).expect("NodeRef index must be >= 1"))
    }

    /// Array index into `nodes`. Equal to [`Self::raw`] cast to
    /// `usize`.
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
    /// sentinel slot; optional children use [`Self::opt_from_raw`]).
    pub fn from_raw(raw: u32) -> Self {
        NodeRef(NonZeroU32::new(raw).expect("NodeRef raw must be non-zero"))
    }

    /// Encode an optional ref as a raw word: `0` for `None`.
    fn opt_raw(r: Option<NodeRef>) -> u32 {
        r.map_or(0, NodeRef::raw)
    }

    /// Decode a raw word produced by [`Self::opt_raw`]: `0` is `None`.
    fn opt_from_raw(raw: u32) -> Option<NodeRef> {
        NonZeroU32::new(raw).map(NodeRef)
    }
}

// ---------- ExtraRange ----------

/// A `[offset, offset+len)` slice of the `extra: Vec<u32>` arena.
/// Mirrors `uir::ExtraRange` (a shared definition is tracked
/// separately).
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

// ---------- Node tags ----------

/// All AST node kinds: statement forms first, then expression forms.
/// Both live in the same arena and share [`NodeRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeTag {
    // Statements.
    /// Variable payload in `extra` — see [`var_decl_extra`].
    VarDecl,
    /// Variable payload in `extra` — see [`function_def_extra`].
    FunctionDef,
    /// `return <expr>`; operand in `NodeData::OptNode` (`0` for bare
    /// `return`).
    Return,
    /// Expression statement; operand in `NodeData::Node`.
    ExprStmt,
    /// Variable payload in `extra`, mirroring `uir::if_stmt`.
    IfStmt,
    /// Variable payload in `extra` — see [`assign_or_decl_extra`].
    AssignOrDecl,
    /// Variable payload in `extra` — see [`compound_assign_extra`].
    CompoundAssign,
    /// Variable payload in `extra` — see [`while_loop_extra`].
    WhileLoop,
    /// Variable payload in `extra` — see [`for_range_extra`].
    ForRange,
    Break,
    Continue,
    /// Placeholder for a statement the parser could not parse. The
    /// parser emits the diagnostic itself and recovers at the next
    /// statement boundary, producing this node so later passes keep
    /// a well-formed (partial) AST; astgen lowers it to nothing.
    Error,

    // Expressions.
    /// `NodeData::Wide` holds the `i64` bit pattern (lo, hi).
    LiteralInt,
    /// `NodeData::Str` holds the interned string's `StringId` raw value.
    LiteralStr,
    /// `NodeData::Bool`.
    LiteralBool,
    /// `NodeData::Wide` holds the `f64::to_bits()` pattern (lo, hi).
    LiteralFloat,
    /// `NodeData::Str` holds the identifier's `StringId` raw value.
    Ident,
    /// `NodeData::BinOp`.
    BinaryOp,
    /// `NodeData::UnOp`.
    UnaryOp,
    /// Variable payload in `extra` — see [`call_extra`].
    Call,
    /// Variable payload in `extra` — see [`method_call_extra`].
    MethodCall,
    /// Call-site mutable-borrow marker `&expr` (M8.3); operand in
    /// `NodeData::Node`.
    Borrow,
    /// Slice projection `base[start:end]` (M8.4); `NodeData::Slice`.
    Slice,
}

// ---------- Node data ----------

/// Per-node inline payload, mirroring the `Data` union in Zig's
/// `lib/std/zig/Ast.zig`: a small set of named `u32`-word shapes
/// reused per tag. Kept as a safe `enum` rather than Zig's `extern
/// union` (per the pipeline_alignment.md risk register: avoid
/// `unsafe`). Literals wider than 32 bits split their bit pattern
/// across the two words of `Wide`; nodes with variable-size
/// payloads keep them in `extra` behind an [`ExtraRange`].
#[derive(Debug, Clone, Copy)]
pub enum NodeData {
    /// No operands (`Break`, `Continue`, `Error`). Zig: no payload.
    None,
    /// One child ref (`ExprStmt`, `Borrow`). Zig: `node`.
    Node(u32),
    /// Optional child ref, `0` = none (`Return`). Zig: `opt_node`.
    OptNode(u32),
    /// `StringId` raw word (`LiteralStr`, `Ident`). Zig: `token`.
    Str(u32),
    /// Boolean literal payload (`LiteralBool`).
    Bool(bool),
    /// 64-bit literal bit pattern, lo/hi halves (`LiteralInt`,
    /// `LiteralFloat`).
    Wide(u32, u32),
    /// `BinaryOp`: lhs/rhs refs + [`BinaryOperator`] discriminant.
    /// Zig: `node_and_node` plus the tag-split op — Ryo keeps one
    /// tag for all binary ops and stores the operator inline.
    BinOp { lhs: u32, op: u32, rhs: u32 },
    /// `UnaryOp`: operand ref + [`UnaryOperator`] discriminant.
    UnOp { operand: u32, op: u32 },
    /// `Slice`: base ref + bounds, `0` when omitted (`s[start:]`,
    /// `s[:end]`, `s[:]`). Zig: `opt_node_and_opt_node` plus base.
    Slice { base: u32, start: u32, end: u32 },
    /// Variable-size payload in `extra` (`Call`, `MethodCall`,
    /// `VarDecl`, `FunctionDef`, `IfStmt`, `AssignOrDecl`,
    /// `CompoundAssign`, `WhileLoop`, `ForRange`). Zig: `extra_range`.
    Extra(ExtraRange),
}

#[derive(Debug, Clone, Copy)]
pub struct Node {
    pub tag: NodeTag,
    pub data: NodeData,
}

// ---------- Identifiers and types ----------

/// An identifier (variable / function / type name) with span info.
///
/// Storing a `StringId` rather than `String` keeps the AST `Copy`-ish
/// for the name fields (the rest of the struct is small) and lets
/// later passes compare identifiers without a string compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ident {
    pub name: StringId,
    pub span: SimpleSpan,
}

impl Ident {
    pub fn new(name: StringId, span: SimpleSpan) -> Self {
        Ident { name, span }
    }
}

/// A type expression. Currently just a name like `int`, `bool`, etc.,
/// plus the `is_view` flag for legacy `&name` view syntax (M8.4
/// pre-Q5) — post-M8.4.1 that syntax only feeds the targeted
/// migration error in astgen.
///
/// Field order keeps the struct at 24 bytes: `span` (16 B, align 8)
/// first, then `name` (4 B) and `is_view` (1 B) pack into the tail
/// padding. Declaring `name` before `span` would grow it to 32 B.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeExpr {
    pub span: SimpleSpan,
    pub name: StringId,
    /// Legacy `&name` view-syntax flag (M8.4 pre-Q5): the annotation used
    /// the retired `&` prefix. Post-M8.4.1 this only feeds astgen's
    /// targeted migration error; it never constructs a view type.
    pub is_view: bool,
}

impl TypeExpr {
    pub fn new(name: StringId, span: SimpleSpan) -> Self {
        TypeExpr {
            span,
            name,
            is_view: false,
        }
    }

    pub fn view(name: StringId, span: SimpleSpan) -> Self {
        TypeExpr {
            span,
            name,
            is_view: true,
        }
    }
}

/// Decoded view of one function parameter. The parser hands these to
/// [`Ast::function_def`]; decoders hand them back out of
/// [`Ast::function_def_view`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Param {
    pub name: Ident,
    pub type_annotation: TypeExpr,
    pub mode: ParamMode,
    pub span: SimpleSpan,
}

// ---------- Operators and literals ----------

/// Literal payloads, decoded from [`NodeTag::Literal*`] nodes via
/// [`Ast::literal_view`]. `Float` round-trips its exact bit pattern
/// through `NodeData`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Literal {
    Int(i64),
    Str(StringId),
    Bool(bool),
    Float(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BinaryOperator {
    Add = 0,
    Sub = 1,
    Mul = 2,
    Div = 3,
    Mod = 4,
    Eq = 5,
    NotEq = 6,
    Lt = 7,
    Gt = 8,
    LtEq = 9,
    GtEq = 10,
    And = 11,
    Or = 12,
}

impl BinaryOperator {
    pub fn from_raw(v: u32) -> Self {
        match v {
            0 => Self::Add,
            1 => Self::Sub,
            2 => Self::Mul,
            3 => Self::Div,
            4 => Self::Mod,
            5 => Self::Eq,
            6 => Self::NotEq,
            7 => Self::Lt,
            8 => Self::Gt,
            9 => Self::LtEq,
            10 => Self::GtEq,
            11 => Self::And,
            12 => Self::Or,
            _ => unreachable!("invalid BinaryOperator discriminant: {v}"),
        }
    }
}

impl fmt::Display for BinaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryOperator::Add => write!(f, "+"),
            BinaryOperator::Sub => write!(f, "-"),
            BinaryOperator::Mul => write!(f, "*"),
            BinaryOperator::Div => write!(f, "/"),
            BinaryOperator::Mod => write!(f, "%"),
            BinaryOperator::Eq => write!(f, "=="),
            BinaryOperator::NotEq => write!(f, "!="),
            BinaryOperator::Lt => write!(f, "<"),
            BinaryOperator::Gt => write!(f, ">"),
            BinaryOperator::LtEq => write!(f, "<="),
            BinaryOperator::GtEq => write!(f, ">="),
            BinaryOperator::And => write!(f, "and"),
            BinaryOperator::Or => write!(f, "or"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum UnaryOperator {
    Neg = 0,
    Not = 1,
}

impl UnaryOperator {
    pub fn from_raw(v: u32) -> Self {
        match v {
            0 => Self::Neg,
            1 => Self::Not,
            _ => unreachable!("invalid UnaryOperator discriminant: {v}"),
        }
    }
}

impl fmt::Display for UnaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnaryOperator::Neg => write!(f, "-"),
            UnaryOperator::Not => write!(f, "not"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CompoundOp {
    Add = 0,
    Sub = 1,
    Mul = 2,
    Div = 3,
    Mod = 4,
}

impl CompoundOp {
    pub fn from_raw(v: u32) -> Self {
        match v {
            0 => Self::Add,
            1 => Self::Sub,
            2 => Self::Mul,
            3 => Self::Div,
            4 => Self::Mod,
            _ => unreachable!("invalid CompoundOp discriminant: {v}"),
        }
    }
}

impl fmt::Display for CompoundOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Add => write!(f, "+="),
            Self::Sub => write!(f, "-="),
            Self::Mul => write!(f, "*="),
            Self::Div => write!(f, "/="),
            Self::Mod => write!(f, "%="),
        }
    }
}

// ---------- Top-level AST ----------

/// A complete Ryo program: the flat node arena plus the `extra`
/// range listing the top-level statements in source order.
#[derive(Debug, Clone)]
pub struct Ast {
    pub nodes: Vec<Node>,
    pub extra: Vec<u32>,
    pub spans: Vec<SimpleSpan>,
    /// Range into `extra` of [`NodeRef::raw`] handles for the
    /// program's top-level statements, in source order.
    pub top_level: ExtraRange,
    /// Span covering the first through last top-level statement;
    /// `0..0` for an empty program. Kept for the pretty-printer's
    /// `Program (start..end)` header.
    pub span: SimpleSpan,
}

impl Default for Ast {
    fn default() -> Self {
        Self::new()
    }
}

impl Ast {
    pub fn new() -> Self {
        // Slot 0 is the reserved sentinel — never read, never
        // referenced. Pushing a placeholder keeps `NodeRef` indices
        // 1-based without runtime checks on every read.
        Ast {
            nodes: vec![Node {
                tag: NodeTag::Error,
                data: NodeData::None,
            }],
            extra: Vec::new(),
            spans: vec![SimpleSpan::new((), 0..0)],
            top_level: ExtraRange { offset: 0, len: 0 },
            span: SimpleSpan::new((), 0..0),
        }
    }

    /// Lookup a node by reference.
    pub fn node(&self, r: NodeRef) -> &Node {
        &self.nodes[r.index()]
    }

    /// Tag of a node — the cheap dispatch key for consumers.
    pub fn tag(&self, r: NodeRef) -> NodeTag {
        self.nodes[r.index()].tag
    }

    /// Lookup the source span attached to a node.
    pub fn span(&self, r: NodeRef) -> SimpleSpan {
        self.spans[r.index()]
    }

    /// The program's top-level statements in source order.
    pub fn top_level_stmts(&self) -> Vec<NodeRef> {
        self.extra[self.top_level.as_range()]
            .iter()
            .copied()
            .map(NodeRef::from_raw)
            .collect()
    }

    /// Record the top-level statement list (called once by the
    /// parser at end of input) and stamp the program span from the
    /// first and last statements.
    pub fn set_top_level(&mut self, stmts: &[NodeRef]) {
        let offset = self.extra_offset();
        for r in stmts {
            self.extra.push(r.raw());
        }
        self.top_level = ExtraRange {
            offset,
            len: Self::len_u32(stmts.len()),
        };
        self.span = match (stmts.first(), stmts.last()) {
            (Some(&first), Some(&last)) => {
                SimpleSpan::new((), self.span(first).start..self.span(last).end)
            }
            _ => SimpleSpan::new((), 0..0),
        };
    }
}

// ---------- Variable-payload encoding ----------

/// Layout in `extra` for [`NodeTag::Call`]:
///
/// ```text
///   [0]  name:  StringId
///   [1]  argc:  u32
///   [2..2+argc] args: NodeRef.raw()
/// ```
pub mod call_extra {
    pub const NAME: usize = 0;
    pub const ARGC: usize = 1;
    pub const ARGS: usize = 2;
}

/// Layout in `extra` for [`NodeTag::MethodCall`]:
///
/// ```text
///   [0]  receiver: NodeRef.raw()
///   [1]  name:     StringId
///   [2]  argc:     u32
///   [3..3+argc] args: NodeRef.raw()
/// ```
pub mod method_call_extra {
    pub const RECEIVER: usize = 0;
    pub const NAME: usize = 1;
    pub const ARGC: usize = 2;
    pub const ARGS: usize = 3;
}

/// Layout in `extra` for [`NodeTag::VarDecl`]:
///
/// ```text
///   [0]  name:           StringId
///   [1]  name span start: u32
///   [2]  name span end:   u32
///   [3]  flags:           u32 (bit 0 = mutable, bit 1 = has
///                        type annotation, bit 2 = annotation
///                        is legacy `&name` view syntax)
///   [4]  ty name:         StringId (0 when flag bit 1 clear)
///   [5]  ty span start:   u32
///   [6]  ty span end:     u32
///   [7]  init:            NodeRef.raw()
/// ```
pub mod var_decl_extra {
    pub const NAME: usize = 0;
    pub const NAME_SPAN_START: usize = 1;
    pub const NAME_SPAN_END: usize = 2;
    pub const FLAGS: usize = 3;
    pub const TY_NAME: usize = 4;
    pub const TY_SPAN_START: usize = 5;
    pub const TY_SPAN_END: usize = 6;
    pub const INIT: usize = 7;
    pub const LEN: usize = 8;

    pub const FLAG_MUTABLE: u32 = 1 << 0;
    pub const FLAG_HAS_TY: u32 = 1 << 1;
    pub const FLAG_TY_VIEW: u32 = 1 << 2;
}

/// Layout in `extra` for [`NodeTag::AssignOrDecl`]:
///
/// ```text
///   [0]  name:           StringId
///   [1]  name span start: u32
///   [2]  name span end:   u32
///   [3]  value:           NodeRef.raw()
/// ```
pub mod assign_or_decl_extra {
    pub const NAME: usize = 0;
    pub const NAME_SPAN_START: usize = 1;
    pub const NAME_SPAN_END: usize = 2;
    pub const VALUE: usize = 3;
    pub const LEN: usize = 4;
}

/// Layout in `extra` for [`NodeTag::CompoundAssign`]:
///
/// ```text
///   [0]  name:           StringId
///   [1]  name span start: u32
///   [2]  name span end:   u32
///   [3]  op:              u32 (CompoundOp discriminant)
///   [4]  value:           NodeRef.raw()
/// ```
pub mod compound_assign_extra {
    pub const NAME: usize = 0;
    pub const NAME_SPAN_START: usize = 1;
    pub const NAME_SPAN_END: usize = 2;
    pub const OP: usize = 3;
    pub const VALUE: usize = 4;
    pub const LEN: usize = 5;
}

/// Layout in `extra` for [`NodeTag::WhileLoop`]:
///
/// ```text
///   [0]       cond:       NodeRef.raw()
///   [1]       body_count: u32
///   [2..2+n]  body stmts: NodeRef.raw() each
/// ```
pub mod while_loop_extra {
    pub const COND: usize = 0;
    pub const BODY_COUNT: usize = 1;
    pub const BODY_START: usize = 2;
}

/// Layout in `extra` for [`NodeTag::ForRange`]:
///
/// ```text
///   [0]  var name:           StringId
///   [1]  var span start:      u32
///   [2]  var span end:        u32
///   [3]  iterator name:       StringId
///   [4]  iterator span start: u32
///   [5]  iterator span end:   u32
///   [6]  start:               NodeRef.raw()
///   [7]  end:                 NodeRef.raw()
///   [8]  body_count:          u32
///   [9..9+n]  body stmts:     NodeRef.raw() each
/// ```
pub mod for_range_extra {
    pub const VAR_NAME: usize = 0;
    pub const VAR_SPAN_START: usize = 1;
    pub const VAR_SPAN_END: usize = 2;
    pub const ITER_NAME: usize = 3;
    pub const ITER_SPAN_START: usize = 4;
    pub const ITER_SPAN_END: usize = 5;
    pub const START: usize = 6;
    pub const END: usize = 7;
    pub const BODY_COUNT: usize = 8;
    pub const BODY_START: usize = 9;
}

/// Layout in `extra` for [`NodeTag::FunctionDef`]:
///
/// ```text
///   [0]  name:           StringId
///   [1]  name span start: u32
///   [2]  name span end:   u32
///   [3]  param_count:     u32
///   [4..4+10*n]  params, PARAM_LEN words each:
///        [0]  name:           StringId
///        [1]  name span start: u32
///        [2]  name span end:   u32
///        [3]  ty name:         StringId
///        [4]  ty span start:   u32
///        [5]  ty span end:     u32
///        [6]  flags:           u32 (bit 0 = legacy `&name` view)
///        [7]  mode:            u32 (ParamMode discriminant)
///        [8]  param span start: u32
///        [9]  param span end:   u32
///   then 5 return-type words:
///        [0]  has_ret: 0/1
///        [1]  ty name: StringId (0 when has_ret == 0)
///        [2]  ty span start: u32
///        [3]  ty span end: u32
///        [4]  flags: u32 (bit 0 = legacy `&name` view)
///   then the body:
///        [0]  body_count: u32
///        [1..1+n] body stmts: NodeRef.raw() each
/// ```
pub mod function_def_extra {
    pub const NAME: usize = 0;
    pub const NAME_SPAN_START: usize = 1;
    pub const NAME_SPAN_END: usize = 2;
    pub const PARAM_COUNT: usize = 3;
    pub const PARAMS_START: usize = 4;

    pub const PARAM_NAME: usize = 0;
    pub const PARAM_NAME_SPAN_START: usize = 1;
    pub const PARAM_NAME_SPAN_END: usize = 2;
    pub const PARAM_TY_NAME: usize = 3;
    pub const PARAM_TY_SPAN_START: usize = 4;
    pub const PARAM_TY_SPAN_END: usize = 5;
    pub const PARAM_FLAGS: usize = 6;
    pub const PARAM_MODE: usize = 7;
    pub const PARAM_SPAN_START: usize = 8;
    pub const PARAM_SPAN_END: usize = 9;
    pub const PARAM_LEN: usize = 10;

    pub const PARAM_FLAG_TY_VIEW: u32 = 1 << 0;

    pub const RET_LEN: usize = 5;
    pub const RET_FLAG_TY_VIEW: u32 = 1 << 0;
}

// ---------- Builder ----------

/// Push a `(tag, data)` node with its span and return its ref.
/// Slot 0 is the reserved sentinel, so the first real node lands
/// at index 1.
fn push(ast: &mut Ast, tag: NodeTag, data: NodeData, span: SimpleSpan) -> NodeRef {
    let idx = ast.nodes.len();
    ast.nodes.push(Node { tag, data });
    ast.spans.push(span);
    NodeRef::from_index(idx)
}

/// Pack a `usize` byte offset (from a `SimpleSpan`) into an `extra`
/// word. Sources larger than `u32::MAX` bytes cannot be encoded and
/// are rejected here rather than silently truncated.
fn offset_u32(off: usize) -> u32 {
    u32::try_from(off).expect("span offset exceeded u32::MAX")
}

impl Ast {
    /// Current `extra.len()` as a checked `u32`. The `extra` arena
    /// is addressed by `u32` offsets in [`ExtraRange`]; an AST that
    /// outgrows `u32::MAX` words of payload cannot be encoded and is
    /// rejected here rather than silently truncated.
    fn extra_offset(&self) -> u32 {
        u32::try_from(self.extra.len()).expect("AST extra arena exceeded u32::MAX words")
    }

    /// Convert a length-shaped `usize` (e.g. `args.len()`) to `u32`.
    /// Panics on overflow for the same reason as [`Self::extra_offset`].
    fn len_u32(len: usize) -> u32 {
        u32::try_from(len).expect("AST list length exceeded u32::MAX")
    }

    fn push_extra(&mut self, word: u32) {
        self.extra.push(word);
    }

    fn push_span_words(&mut self, span: SimpleSpan) {
        self.push_extra(offset_u32(span.start));
        self.push_extra(offset_u32(span.end));
    }

    fn push_ident(&mut self, ident: Ident) {
        self.push_extra(ident.name.raw());
        self.push_span_words(ident.span);
    }

    /// Append a count-prefixed ref list to `extra`, returning nothing;
    /// callers track the base offset themselves.
    fn push_ref_list(&mut self, refs: &[NodeRef]) {
        self.push_extra(Self::len_u32(refs.len()));
        for r in refs {
            self.push_extra(r.raw());
        }
    }

    pub fn literal(&mut self, lit: Literal, span: SimpleSpan) -> NodeRef {
        match lit {
            Literal::Int(v) => self.literal_int(v, span),
            Literal::Str(id) => self.literal_str(id, span),
            Literal::Bool(v) => self.literal_bool(v, span),
            Literal::Float(v) => self.literal_float(v, span),
        }
    }

    pub fn literal_int(&mut self, value: i64, span: SimpleSpan) -> NodeRef {
        let bits = value as u64;
        let data = NodeData::Wide(bits as u32, (bits >> 32) as u32);
        push(self, NodeTag::LiteralInt, data, span)
    }

    pub fn literal_float(&mut self, value: f64, span: SimpleSpan) -> NodeRef {
        let bits = value.to_bits();
        let data = NodeData::Wide(bits as u32, (bits >> 32) as u32);
        push(self, NodeTag::LiteralFloat, data, span)
    }

    pub fn literal_str(&mut self, value: StringId, span: SimpleSpan) -> NodeRef {
        push(self, NodeTag::LiteralStr, NodeData::Str(value.raw()), span)
    }

    pub fn literal_bool(&mut self, value: bool, span: SimpleSpan) -> NodeRef {
        push(self, NodeTag::LiteralBool, NodeData::Bool(value), span)
    }

    pub fn ident(&mut self, name: StringId, span: SimpleSpan) -> NodeRef {
        push(self, NodeTag::Ident, NodeData::Str(name.raw()), span)
    }

    pub fn binary(
        &mut self,
        op: BinaryOperator,
        lhs: NodeRef,
        rhs: NodeRef,
        span: SimpleSpan,
    ) -> NodeRef {
        let data = NodeData::BinOp {
            lhs: lhs.raw(),
            op: op as u32,
            rhs: rhs.raw(),
        };
        push(self, NodeTag::BinaryOp, data, span)
    }

    pub fn unary(&mut self, op: UnaryOperator, operand: NodeRef, span: SimpleSpan) -> NodeRef {
        let data = NodeData::UnOp {
            operand: operand.raw(),
            op: op as u32,
        };
        push(self, NodeTag::UnaryOp, data, span)
    }

    /// Call-site mutable-borrow marker `&expr` (M8.3).
    pub fn borrow(&mut self, inner: NodeRef, span: SimpleSpan) -> NodeRef {
        push(self, NodeTag::Borrow, NodeData::Node(inner.raw()), span)
    }

    /// Slice projection `base[start:end]` (M8.4). `start`/`end` are
    /// `None` for the `s[start:]`, `s[:end]`, `s[:]` shorthands.
    pub fn slice(
        &mut self,
        base: NodeRef,
        start: Option<NodeRef>,
        end: Option<NodeRef>,
        span: SimpleSpan,
    ) -> NodeRef {
        let data = NodeData::Slice {
            base: base.raw(),
            start: NodeRef::opt_raw(start),
            end: NodeRef::opt_raw(end),
        };
        push(self, NodeTag::Slice, data, span)
    }

    /// `return <expr>`, or bare `return` when `value` is `None`.
    pub fn return_stmt(&mut self, value: Option<NodeRef>, span: SimpleSpan) -> NodeRef {
        let data = NodeData::OptNode(NodeRef::opt_raw(value));
        push(self, NodeTag::Return, data, span)
    }

    pub fn expr_stmt(&mut self, value: NodeRef, span: SimpleSpan) -> NodeRef {
        push(self, NodeTag::ExprStmt, NodeData::Node(value.raw()), span)
    }

    pub fn break_stmt(&mut self, span: SimpleSpan) -> NodeRef {
        push(self, NodeTag::Break, NodeData::None, span)
    }

    pub fn continue_stmt(&mut self, span: SimpleSpan) -> NodeRef {
        push(self, NodeTag::Continue, NodeData::None, span)
    }

    /// Placeholder for an unparseable statement recovered by the
    /// parser; astgen lowers it to nothing.
    pub fn error_stmt(&mut self, span: SimpleSpan) -> NodeRef {
        push(self, NodeTag::Error, NodeData::None, span)
    }

    /// Emits a `Call` with name and arg list packed into `extra`.
    pub fn call(&mut self, name: StringId, args: &[NodeRef], span: SimpleSpan) -> NodeRef {
        let offset = self.extra_offset();
        self.push_extra(name.raw());
        self.push_extra(Self::len_u32(args.len()));
        for arg in args {
            self.push_extra(arg.raw());
        }
        let len = Self::len_u32(call_extra::ARGS + args.len());
        push(
            self,
            NodeTag::Call,
            NodeData::Extra(ExtraRange { offset, len }),
            span,
        )
    }

    /// Emits a `MethodCall` with receiver, name, and arg list packed
    /// into `extra` — see [`method_call_extra`].
    pub fn method_call(
        &mut self,
        receiver: NodeRef,
        method: StringId,
        args: &[NodeRef],
        span: SimpleSpan,
    ) -> NodeRef {
        let offset = self.extra_offset();
        self.push_extra(receiver.raw());
        self.push_extra(method.raw());
        self.push_extra(Self::len_u32(args.len()));
        for arg in args {
            self.push_extra(arg.raw());
        }
        let len = Self::len_u32(method_call_extra::ARGS + args.len());
        push(
            self,
            NodeTag::MethodCall,
            NodeData::Extra(ExtraRange { offset, len }),
            span,
        )
    }

    /// Emits a `VarDecl` with the header packed into `extra` — see
    /// [`var_decl_extra`].
    pub fn var_decl(
        &mut self,
        mutable: bool,
        name: Ident,
        type_annotation: Option<TypeExpr>,
        initializer: NodeRef,
        span: SimpleSpan,
    ) -> NodeRef {
        use var_decl_extra as l;
        let offset = self.extra_offset();
        let mut flags = 0;
        if mutable {
            flags |= l::FLAG_MUTABLE;
        }
        if let Some(ty) = type_annotation {
            flags |= l::FLAG_HAS_TY;
            if ty.is_view {
                flags |= l::FLAG_TY_VIEW;
            }
        }
        self.push_extra(name.name.raw());
        self.push_span_words(name.span);
        self.push_extra(flags);
        match type_annotation {
            Some(ty) => {
                self.push_extra(ty.name.raw());
                self.push_span_words(ty.span);
            }
            None => {
                self.push_extra(0);
                self.push_extra(0);
                self.push_extra(0);
            }
        }
        self.push_extra(initializer.raw());
        push(
            self,
            NodeTag::VarDecl,
            NodeData::Extra(ExtraRange {
                offset,
                len: Self::len_u32(l::LEN),
            }),
            span,
        )
    }

    pub fn assign_or_decl(&mut self, target: Ident, value: NodeRef, span: SimpleSpan) -> NodeRef {
        let offset = self.extra_offset();
        self.push_ident(target);
        self.push_extra(value.raw());
        push(
            self,
            NodeTag::AssignOrDecl,
            NodeData::Extra(ExtraRange {
                offset,
                len: Self::len_u32(assign_or_decl_extra::LEN),
            }),
            span,
        )
    }

    pub fn compound_assign(
        &mut self,
        target: Ident,
        op: CompoundOp,
        value: NodeRef,
        span: SimpleSpan,
    ) -> NodeRef {
        let offset = self.extra_offset();
        self.push_ident(target);
        self.push_extra(op as u32);
        self.push_extra(value.raw());
        push(
            self,
            NodeTag::CompoundAssign,
            NodeData::Extra(ExtraRange {
                offset,
                len: Self::len_u32(compound_assign_extra::LEN),
            }),
            span,
        )
    }

    pub fn while_loop(&mut self, cond: NodeRef, body: &[NodeRef], span: SimpleSpan) -> NodeRef {
        let offset = self.extra_offset();
        self.push_extra(cond.raw());
        self.push_ref_list(body);
        let len = Self::len_u32(while_loop_extra::BODY_START + body.len());
        push(
            self,
            NodeTag::WhileLoop,
            NodeData::Extra(ExtraRange { offset, len }),
            span,
        )
    }

    pub fn for_range(
        &mut self,
        var: Ident,
        iterator: Ident,
        start: NodeRef,
        end: NodeRef,
        body: &[NodeRef],
        span: SimpleSpan,
    ) -> NodeRef {
        let offset = self.extra_offset();
        self.push_ident(var);
        self.push_ident(iterator);
        self.push_extra(start.raw());
        self.push_extra(end.raw());
        self.push_ref_list(body);
        let len = Self::len_u32(for_range_extra::BODY_START + body.len());
        push(
            self,
            NodeTag::ForRange,
            NodeData::Extra(ExtraRange { offset, len }),
            span,
        )
    }

    pub fn if_stmt(
        &mut self,
        cond: NodeRef,
        then_stmts: &[NodeRef],
        elif_branches: &[(NodeRef, Vec<NodeRef>)],
        else_stmts: Option<&[NodeRef]>,
        span: SimpleSpan,
    ) -> NodeRef {
        let offset = self.extra_offset();
        self.push_extra(cond.raw());
        self.push_ref_list(then_stmts);
        self.push_extra(Self::len_u32(elif_branches.len()));
        for (elif_cond, elif_body) in elif_branches {
            self.push_extra(elif_cond.raw());
            self.push_ref_list(elif_body);
        }
        match else_stmts {
            Some(stmts) => {
                self.push_extra(1);
                self.push_ref_list(stmts);
            }
            None => {
                self.push_extra(0);
            }
        }
        let len = Self::len_u32(self.extra.len() - offset as usize);
        push(
            self,
            NodeTag::IfStmt,
            NodeData::Extra(ExtraRange { offset, len }),
            span,
        )
    }

    /// Emits a `FunctionDef` with name, params, return type, and
    /// body packed into `extra` — see [`function_def_extra`].
    pub fn function_def(
        &mut self,
        name: Ident,
        params: &[Param],
        return_type: Option<TypeExpr>,
        body: &[NodeRef],
        span: SimpleSpan,
    ) -> NodeRef {
        use function_def_extra as l;
        let offset = self.extra_offset();
        self.push_ident(name);
        self.push_extra(Self::len_u32(params.len()));
        for p in params {
            self.push_ident(p.name);
            self.push_extra(p.type_annotation.name.raw());
            self.push_span_words(p.type_annotation.span);
            self.push_extra(if p.type_annotation.is_view {
                l::PARAM_FLAG_TY_VIEW
            } else {
                0
            });
            self.push_extra(p.mode as u32);
            self.push_span_words(p.span);
        }
        match return_type {
            Some(ty) => {
                self.push_extra(1);
                self.push_extra(ty.name.raw());
                self.push_span_words(ty.span);
                self.push_extra(if ty.is_view { l::RET_FLAG_TY_VIEW } else { 0 });
            }
            None => {
                self.push_extra(0);
                self.push_extra(0);
                self.push_extra(0);
                self.push_extra(0);
                self.push_extra(0);
            }
        }
        self.push_ref_list(body);
        let len = Self::len_u32(self.extra.len() - offset as usize);
        push(
            self,
            NodeTag::FunctionDef,
            NodeData::Extra(ExtraRange { offset, len }),
            span,
        )
    }
}

// ---------- Read-side views ----------

/// Decoded view of a [`NodeTag::VarDecl`] payload.
pub struct VarDeclView {
    pub mutable: bool,
    pub name: Ident,
    /// `None` when the source had no annotation.
    pub type_annotation: Option<TypeExpr>,
    pub initializer: NodeRef,
}

/// Decoded view of a [`NodeTag::FunctionDef`] payload.
pub struct FunctionDefView {
    pub name: Ident,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: Vec<NodeRef>,
}

pub struct AssignOrDeclView {
    pub target: Ident,
    pub value: NodeRef,
}

pub struct CompoundAssignView {
    pub target: Ident,
    pub op: CompoundOp,
    pub value: NodeRef,
}

pub struct WhileLoopView {
    pub cond: NodeRef,
    pub body: Vec<NodeRef>,
}

pub struct ForRangeView {
    pub var: Ident,
    pub iterator: Ident,
    pub start: NodeRef,
    pub end: NodeRef,
    pub body: Vec<NodeRef>,
}

pub struct ElifView {
    pub cond: NodeRef,
    pub block: Vec<NodeRef>,
}

pub struct IfStmtView {
    pub cond: NodeRef,
    pub then_block: Vec<NodeRef>,
    pub elif_branches: Vec<ElifView>,
    pub else_block: Option<Vec<NodeRef>>,
}

/// Decoded view of a [`NodeTag::Call`] payload.
pub struct CallView {
    pub name: StringId,
    pub args: Vec<NodeRef>,
}

/// Decoded view of a [`NodeTag::MethodCall`] payload.
pub struct MethodCallView {
    pub receiver: NodeRef,
    pub method: StringId,
    pub args: Vec<NodeRef>,
}

/// Decoded view of a [`NodeTag::BinaryOp`] payload.
pub struct BinaryOpView {
    pub lhs: NodeRef,
    pub op: BinaryOperator,
    pub rhs: NodeRef,
}

/// Decoded view of a [`NodeTag::UnaryOp`] payload.
pub struct UnaryOpView {
    pub op: UnaryOperator,
    pub operand: NodeRef,
}

/// Decoded view of a [`NodeTag::Slice`] payload.
pub struct SliceView {
    pub base: NodeRef,
    pub start: Option<NodeRef>,
    pub end: Option<NodeRef>,
}

impl Ast {
    /// Extra slice for a node whose `data` carries an `ExtraRange`.
    /// Trusted-producer decode: `debug_assert`s the tag and
    /// `unreachable!`s on a malformed payload.
    fn extra_slice(&self, r: NodeRef, tag: NodeTag) -> &[u32] {
        let node = self.node(r);
        debug_assert!(node.tag == tag);
        let range = match node.data {
            NodeData::Extra(rng) => rng,
            _ => unreachable!("{tag:?} must carry NodeData::Extra"),
        };
        &self.extra[range.as_range()]
    }

    /// Decode the [`Literal`] payload of a `Literal*` node. Float
    /// payloads come back bit-exact via `f64::from_bits`.
    pub fn literal_view(&self, r: NodeRef) -> Literal {
        let node = self.node(r);
        match (node.tag, node.data) {
            (NodeTag::LiteralInt, NodeData::Wide(lo, hi)) => {
                Literal::Int((u64::from(lo) | (u64::from(hi) << 32)) as i64)
            }
            (NodeTag::LiteralFloat, NodeData::Wide(lo, hi)) => {
                Literal::Float(f64::from_bits(u64::from(lo) | (u64::from(hi) << 32)))
            }
            (NodeTag::LiteralStr, NodeData::Str(id)) => Literal::Str(StringId::from_raw(id)),
            (NodeTag::LiteralBool, NodeData::Bool(v)) => Literal::Bool(v),
            _ => unreachable!("literal_view on non-literal node: {:?}", node.tag),
        }
    }

    /// The `StringId` behind an [`NodeTag::Ident`] node.
    pub fn ident_name(&self, r: NodeRef) -> StringId {
        let node = self.node(r);
        debug_assert!(node.tag == NodeTag::Ident);
        match node.data {
            NodeData::Str(id) => StringId::from_raw(id),
            _ => unreachable!("Ident must carry NodeData::Str"),
        }
    }

    pub fn binary_op_view(&self, r: NodeRef) -> BinaryOpView {
        let node = self.node(r);
        debug_assert!(node.tag == NodeTag::BinaryOp);
        match node.data {
            NodeData::BinOp { lhs, op, rhs } => BinaryOpView {
                lhs: NodeRef::from_raw(lhs),
                op: BinaryOperator::from_raw(op),
                rhs: NodeRef::from_raw(rhs),
            },
            _ => unreachable!("BinaryOp must carry NodeData::BinOp"),
        }
    }

    pub fn unary_op_view(&self, r: NodeRef) -> UnaryOpView {
        let node = self.node(r);
        debug_assert!(node.tag == NodeTag::UnaryOp);
        match node.data {
            NodeData::UnOp { operand, op } => UnaryOpView {
                op: UnaryOperator::from_raw(op),
                operand: NodeRef::from_raw(operand),
            },
            _ => unreachable!("UnaryOp must carry NodeData::UnOp"),
        }
    }

    pub fn slice_view(&self, r: NodeRef) -> SliceView {
        let node = self.node(r);
        debug_assert!(node.tag == NodeTag::Slice);
        match node.data {
            NodeData::Slice { base, start, end } => SliceView {
                base: NodeRef::from_raw(base),
                start: NodeRef::opt_from_raw(start),
                end: NodeRef::opt_from_raw(end),
            },
            _ => unreachable!("Slice must carry NodeData::Slice"),
        }
    }

    /// The operand of a [`NodeTag::Borrow`] node.
    pub fn borrow_inner(&self, r: NodeRef) -> NodeRef {
        let node = self.node(r);
        debug_assert!(node.tag == NodeTag::Borrow);
        match node.data {
            NodeData::Node(inner) => NodeRef::from_raw(inner),
            _ => unreachable!("Borrow must carry NodeData::Node"),
        }
    }

    /// The value of a [`NodeTag::Return`] node (`None` for bare
    /// `return`).
    pub fn return_value(&self, r: NodeRef) -> Option<NodeRef> {
        let node = self.node(r);
        debug_assert!(node.tag == NodeTag::Return);
        match node.data {
            NodeData::OptNode(v) => NodeRef::opt_from_raw(v),
            _ => unreachable!("Return must carry NodeData::OptNode"),
        }
    }

    /// The value of a [`NodeTag::ExprStmt`] node.
    pub fn expr_stmt_value(&self, r: NodeRef) -> NodeRef {
        let node = self.node(r);
        debug_assert!(node.tag == NodeTag::ExprStmt);
        match node.data {
            NodeData::Node(v) => NodeRef::from_raw(v),
            _ => unreachable!("ExprStmt must carry NodeData::Node"),
        }
    }

    pub fn call_view(&self, r: NodeRef) -> CallView {
        let slice = self.extra_slice(r, NodeTag::Call);
        let name = StringId::from_raw(slice[call_extra::NAME]);
        let argc = slice[call_extra::ARGC] as usize;
        let args = slice[call_extra::ARGS..call_extra::ARGS + argc]
            .iter()
            .copied()
            .map(NodeRef::from_raw)
            .collect();
        CallView { name, args }
    }

    pub fn method_call_view(&self, r: NodeRef) -> MethodCallView {
        let slice = self.extra_slice(r, NodeTag::MethodCall);
        let receiver = NodeRef::from_raw(slice[method_call_extra::RECEIVER]);
        let method = StringId::from_raw(slice[method_call_extra::NAME]);
        let argc = slice[method_call_extra::ARGC] as usize;
        let args = slice[method_call_extra::ARGS..method_call_extra::ARGS + argc]
            .iter()
            .copied()
            .map(NodeRef::from_raw)
            .collect();
        MethodCallView {
            receiver,
            method,
            args,
        }
    }

    pub fn var_decl_view(&self, r: NodeRef) -> VarDeclView {
        use var_decl_extra as l;
        let slice = self.extra_slice(r, NodeTag::VarDecl);
        let name = read_ident(slice, l::NAME);
        let flags = slice[l::FLAGS];
        let type_annotation = if flags & l::FLAG_HAS_TY != 0 {
            Some(TypeExpr {
                span: read_span(slice, l::TY_SPAN_START),
                name: StringId::from_raw(slice[l::TY_NAME]),
                is_view: flags & l::FLAG_TY_VIEW != 0,
            })
        } else {
            None
        };
        VarDeclView {
            mutable: flags & l::FLAG_MUTABLE != 0,
            name,
            type_annotation,
            initializer: NodeRef::from_raw(slice[l::INIT]),
        }
    }

    pub fn assign_or_decl_view(&self, r: NodeRef) -> AssignOrDeclView {
        use assign_or_decl_extra as l;
        let slice = self.extra_slice(r, NodeTag::AssignOrDecl);
        AssignOrDeclView {
            target: read_ident(slice, l::NAME),
            value: NodeRef::from_raw(slice[l::VALUE]),
        }
    }

    pub fn compound_assign_view(&self, r: NodeRef) -> CompoundAssignView {
        use compound_assign_extra as l;
        let slice = self.extra_slice(r, NodeTag::CompoundAssign);
        CompoundAssignView {
            target: read_ident(slice, l::NAME),
            op: CompoundOp::from_raw(slice[l::OP]),
            value: NodeRef::from_raw(slice[l::VALUE]),
        }
    }

    pub fn while_loop_view(&self, r: NodeRef) -> WhileLoopView {
        let slice = self.extra_slice(r, NodeTag::WhileLoop);
        let cond = NodeRef::from_raw(slice[while_loop_extra::COND]);
        let mut pos = while_loop_extra::BODY_COUNT;
        let body = read_ref_list(slice, &mut pos);
        WhileLoopView { cond, body }
    }

    pub fn for_range_view(&self, r: NodeRef) -> ForRangeView {
        use for_range_extra as l;
        let slice = self.extra_slice(r, NodeTag::ForRange);
        let var = read_ident(slice, l::VAR_NAME);
        let iterator = read_ident(slice, l::ITER_NAME);
        let start = NodeRef::from_raw(slice[l::START]);
        let end = NodeRef::from_raw(slice[l::END]);
        let body_count = slice[l::BODY_COUNT] as usize;
        let body = slice[l::BODY_START..l::BODY_START + body_count]
            .iter()
            .copied()
            .map(NodeRef::from_raw)
            .collect();
        ForRangeView {
            var,
            iterator,
            start,
            end,
            body,
        }
    }

    pub fn if_stmt_view(&self, r: NodeRef) -> IfStmtView {
        let slice = self.extra_slice(r, NodeTag::IfStmt);
        let mut pos = 0;

        let cond = NodeRef::from_raw(slice[pos]);
        pos += 1;

        let then_block = read_ref_list(slice, &mut pos);

        let elif_count = slice[pos] as usize;
        pos += 1;
        let mut elif_branches = Vec::with_capacity(elif_count);
        for _ in 0..elif_count {
            let elif_cond = NodeRef::from_raw(slice[pos]);
            pos += 1;
            let block = read_ref_list(slice, &mut pos);
            elif_branches.push(ElifView {
                cond: elif_cond,
                block,
            });
        }

        let has_else = slice[pos] != 0;
        pos += 1;
        let else_block = if has_else {
            Some(read_ref_list(slice, &mut pos))
        } else {
            None
        };

        IfStmtView {
            cond,
            then_block,
            elif_branches,
            else_block,
        }
    }

    pub fn function_def_view(&self, r: NodeRef) -> FunctionDefView {
        use function_def_extra as l;
        let slice = self.extra_slice(r, NodeTag::FunctionDef);
        let name = read_ident(slice, l::NAME);
        let param_count = slice[l::PARAM_COUNT] as usize;
        let mut params = Vec::with_capacity(param_count);
        for i in 0..param_count {
            let base = l::PARAMS_START + i * l::PARAM_LEN;
            let p = &slice[base..base + l::PARAM_LEN];
            params.push(Param {
                name: read_ident(p, l::PARAM_NAME),
                type_annotation: TypeExpr {
                    span: read_span(p, l::PARAM_TY_SPAN_START),
                    name: StringId::from_raw(p[l::PARAM_TY_NAME]),
                    is_view: p[l::PARAM_FLAGS] & l::PARAM_FLAG_TY_VIEW != 0,
                },
                mode: match p[l::PARAM_MODE] {
                    0 => ParamMode::Borrow,
                    1 => ParamMode::Move,
                    2 => ParamMode::Inout,
                    v => unreachable!("invalid ParamMode discriminant: {v}"),
                },
                span: read_span(p, l::PARAM_SPAN_START),
            });
        }
        let mut pos = l::PARAMS_START + param_count * l::PARAM_LEN;
        let has_ret = slice[pos] != 0;
        let return_type = if has_ret {
            Some(TypeExpr {
                span: read_span(slice, pos + 2),
                name: StringId::from_raw(slice[pos + 1]),
                is_view: slice[pos + 4] & l::RET_FLAG_TY_VIEW != 0,
            })
        } else {
            None
        };
        pos += l::RET_LEN;
        let body = read_ref_list(slice, &mut pos);
        FunctionDefView {
            name,
            params,
            return_type,
            body,
        }
    }
}

/// Read a `u32` span pair at `slice[base..base+2]` back into a
/// `SimpleSpan`. Trusted producer: the words were written by
/// [`Ast::push_span_words`] from `usize` offsets.
fn read_span(slice: &[u32], base: usize) -> SimpleSpan {
    SimpleSpan::new((), slice[base] as usize..slice[base + 1] as usize)
}

/// Read an [`Ident`] header (name word + span pair) at `base`.
fn read_ident(slice: &[u32], base: usize) -> Ident {
    Ident {
        name: StringId::from_raw(slice[base]),
        span: read_span(slice, base + 1),
    }
}

/// Read a count-prefixed ref list at `*pos`, advancing past it.
fn read_ref_list(slice: &[u32], pos: &mut usize) -> Vec<NodeRef> {
    let count = slice[*pos] as usize;
    *pos += 1;
    let refs = slice[*pos..*pos + count]
        .iter()
        .copied()
        .map(NodeRef::from_raw)
        .collect();
    *pos += count;
    refs
}

// ---------- Parser-state integration ----------

/// Chumsky parser-state implementation: the parser builds directly
/// into the arena via `map_with`/`foldl_with` closures that call
/// `e.state()`, with the `Ast` itself as the state object.
///
/// A failed alternative (`or`/`choice` backtracking, `repeated`
/// iteration rollbacks, error recovery) can push nodes before it
/// fails; those refs never escape into a kept node, but without
/// rollback the arena would accumulate dead entries. Like chumsky's
/// own `TruncateState`, the checkpoint records the three arena
/// lengths and `on_rewind` truncates them, so a rewound region
/// leaves the arena exactly as it found it. Refs created before the
/// checkpoint sit below the truncation point and stay valid; refs
/// created after it are discarded together with the failed output
/// that held them.
impl<'src, I: chumsky::input::Input<'src>> chumsky::inspector::Inspector<'src, I> for Ast {
    type Checkpoint = (usize, usize, usize);

    fn on_token(&mut self, _: &I::Token) {}

    fn on_save<'parse>(&self, _: &chumsky::input::Cursor<'src, 'parse, I>) -> Self::Checkpoint {
        (self.nodes.len(), self.extra.len(), self.spans.len())
    }

    fn on_rewind<'parse>(
        &mut self,
        marker: &chumsky::input::Checkpoint<'src, 'parse, I, Self::Checkpoint>,
    ) {
        let &(nodes, extra, spans) = marker.inspector();
        self.nodes.truncate(nodes);
        self.extra.truncate(extra);
        self.spans.truncate(spans);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::InternPool;

    fn span(start: usize, end: usize) -> SimpleSpan {
        SimpleSpan::new((), start..end)
    }

    #[test]
    fn binary_operator_display_for_equality() {
        assert_eq!(format!("{}", BinaryOperator::Eq), "==");
        assert_eq!(format!("{}", BinaryOperator::NotEq), "!=");
    }

    #[test]
    fn binary_operator_display_for_ordering_and_modulo() {
        assert_eq!(format!("{}", BinaryOperator::Lt), "<");
        assert_eq!(format!("{}", BinaryOperator::Gt), ">");
        assert_eq!(format!("{}", BinaryOperator::LtEq), "<=");
        assert_eq!(format!("{}", BinaryOperator::GtEq), ">=");
        assert_eq!(format!("{}", BinaryOperator::Mod), "%");
    }

    #[test]
    fn literal_float_variant_round_trips_payload() {
        let mut ast = Ast::new();
        let r = ast.literal_float(1.5, span(0, 0));
        match ast.literal_view(r) {
            Literal::Float(v) => assert_eq!(v, 1.5),
            other => panic!("expected Literal::Float, got {:?}", other),
        }
    }

    #[test]
    fn literal_float_bits_survive_exactly() {
        // The arena stores the `f64::to_bits()` pattern; decoding
        // must be bit-exact (incl. NaN payloads and -0.0).
        let mut ast = Ast::new();
        for v in [0.0, -0.0, f64::NAN, f64::INFINITY, 1.5e300] {
            let r = ast.literal_float(v, span(0, 0));
            match ast.literal_view(r) {
                Literal::Float(out) => assert_eq!(out.to_bits(), v.to_bits()),
                other => panic!("expected Literal::Float, got {:?}", other),
            }
        }
    }

    #[test]
    fn literal_bool_variants_exist() {
        let _t = Literal::Bool(true);
        let _f = Literal::Bool(false);
    }

    #[test]
    fn binary_operator_display_for_logical() {
        assert_eq!(format!("{}", BinaryOperator::And), "and");
        assert_eq!(format!("{}", BinaryOperator::Or), "or");
    }

    #[test]
    fn unary_operator_display_for_not() {
        assert_eq!(format!("{}", UnaryOperator::Not), "not");
    }

    #[test]
    fn type_expr_stays_small() {
        // `span` (16 B) + `name` (4 B) + `is_view` (1 B) must pack
        // into 24 B — see the field-order note on `TypeExpr`.
        assert_eq!(std::mem::size_of::<TypeExpr>(), 24);
    }

    #[test]
    fn slot_zero_is_reserved_sentinel() {
        let ast = Ast::new();
        assert_eq!(ast.nodes.len(), 1);
        assert_eq!(ast.spans.len(), 1);
    }

    #[test]
    fn int_literal_round_trips_i64_extremes() {
        let mut ast = Ast::new();
        for v in [i64::MIN, -1, 0, 1, i64::MAX] {
            let r = ast.literal_int(v, span(0, 0));
            match ast.literal_view(r) {
                Literal::Int(out) => assert_eq!(out, v),
                other => panic!("expected Literal::Int, got {:?}", other),
            }
        }
    }

    #[test]
    fn call_args_round_trip_through_extra() {
        let mut pool = InternPool::new();
        let name = pool.intern_str("f");
        let mut ast = Ast::new();
        let a = ast.literal_int(1, span(0, 1));
        let b = ast.literal_int(2, span(4, 5));
        let call = ast.call(name, &[a, b], span(0, 6));
        let view = ast.call_view(call);
        assert_eq!(view.name, name);
        assert_eq!(view.args, vec![a, b]);
    }

    #[test]
    fn var_decl_round_trips_header() {
        let mut pool = InternPool::new();
        let x = pool.intern_str("x");
        let int = pool.intern_str("int");
        let mut ast = Ast::new();
        let init = ast.literal_int(42, span(7, 9));
        let name = Ident::new(x, span(4, 5));
        let ty = TypeExpr::new(int, span(6, 7));
        let decl = ast.var_decl(true, name, Some(ty), init, span(0, 9));
        let view = ast.var_decl_view(decl);
        assert!(view.mutable);
        assert_eq!(view.name, name);
        assert_eq!(view.type_annotation, Some(ty));
        assert_eq!(view.initializer, init);
    }

    #[test]
    fn function_def_round_trips_params_and_body() {
        let mut pool = InternPool::new();
        let f = pool.intern_str("f");
        let s = pool.intern_str("s");
        let str_ = pool.intern_str("str");
        let mut ast = Ast::new();
        let body_stmt = ast.break_stmt(span(3, 4));
        let param = Param {
            name: Ident::new(s, span(1, 2)),
            type_annotation: TypeExpr::view(str_, span(2, 3)),
            mode: ParamMode::Inout,
            span: span(1, 3),
        };
        let def = ast.function_def(
            Ident::new(f, span(0, 1)),
            &[param],
            Some(TypeExpr::new(str_, span(4, 5))),
            &[body_stmt],
            span(0, 6),
        );
        let view = ast.function_def_view(def);
        assert_eq!(view.name.name, f);
        assert_eq!(view.params, vec![param]);
        assert_eq!(
            view.return_type.map(|t| (t.name, t.is_view)),
            Some((str_, false))
        );
        assert_eq!(view.body, vec![body_stmt]);
    }

    #[test]
    fn if_stmt_round_trips_branches() {
        let mut ast = Ast::new();
        let cond = ast.literal_bool(true, span(0, 1));
        let then_s = ast.break_stmt(span(1, 2));
        let elif_cond = ast.literal_bool(false, span(2, 3));
        let elif_s = ast.continue_stmt(span(3, 4));
        let else_s = ast.error_stmt(span(4, 5));
        let node = ast.if_stmt(
            cond,
            &[then_s],
            &[(elif_cond, vec![elif_s])],
            Some(&[else_s]),
            span(0, 5),
        );
        let view = ast.if_stmt_view(node);
        assert_eq!(view.cond, cond);
        assert_eq!(view.then_block, vec![then_s]);
        assert_eq!(view.elif_branches.len(), 1);
        assert_eq!(view.elif_branches[0].cond, elif_cond);
        assert_eq!(view.elif_branches[0].block, vec![elif_s]);
        assert_eq!(view.else_block, Some(vec![else_s]));
    }
}
