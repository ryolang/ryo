//! Surface-syntax AST.
//!
//! Identifiers, type names, and string literals are stored as
//! `StringId` handles into the compilation's `InternPool`.
//! Tree-drawing lives in `crate::ast_pretty`.
//!
//! ## Storage shape
//!
//! Typed arenas instead of a pointer tree: expressions and
//! statements each live in their own flat `Vec` and refer to their
//! children by index, so the tree is never boxed or recursively
//! owned.
//!
//! - `exprs: Vec<Expr>` — indexed by [`ExprId`]. Slot 0 is a
//!   reserved sentinel that is never handed out.
//! - `stmts: Vec<Stmt>` — indexed by [`StmtId`], same sentinel
//!   convention.
//! - `expr_lists: Vec<ExprId>` / `stmt_lists: Vec<StmtId>` — side
//!   arenas for variable-length child lists (call arguments, block
//!   bodies). A node that owns such a list stores an [`ExprList`] /
//!   [`StmtList`] range into the side arena.
//! - `top_level: Vec<StmtId>` — the program's statements in source
//!   order; everything below is reached by following ids out of them.
//!
//! `Expr`/`Stmt` carry their `span` inline and a plain Rust enum
//! payload ([`ExprKind`]/[`StmtKind`]); consumers write ordinary
//! exhaustive `match`es and follow child ids through
//! [`Ast::expr`]/[`Ast::stmt`]/[`Ast::expr_list`]/[`Ast::stmt_list`].
//! Payload structs with scalar metadata (`Ident`, `TypeExpr`,
//! `Param`, `VarDecl`, `FunctionDef`, `IfStmt`, `ElifBranch`) are
//! carried inline in the variants — only *nodes* are arena-allocated.
//!
//! ## Why `NonZeroU32` for the ids
//!
//! `ExprId(NonZeroU32)` / `StmtId(NonZeroU32)` make
//! `Option<ExprId>` / `Option<StmtId>` a single 32-bit slot via
//! niche-filling, matching the UIR/TIR index conventions. The
//! reserved 0 slot in each arena keeps every valid id non-zero.
//!
//! ## Parser-state integration
//!
//! The parser builds directly into the arenas with `Ast` as the
//! chumsky state object; see the `Inspector` impl at the bottom of
//! this file for rewind truncation.

use crate::tir::ParamMode;
use crate::types::StringId;
use chumsky::span::{SimpleSpan, Span as _};
use std::fmt;
use std::num::NonZeroU32;

// ---------- Ids ----------

/// Index into [`Ast::exprs`].
///
/// The wrapped `NonZeroU32` *is* the array index directly: slot 0
/// of `exprs` is reserved as an unreachable sentinel, so every
/// valid id lands in `1..exprs.len()`. The niche-filled
/// representation makes `Option<ExprId>` a single 32-bit slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExprId(NonZeroU32);

impl ExprId {
    /// Convert from a `usize` array index. Caller guarantees `idx`
    /// is in `1..exprs.len()` (slot 0 is reserved).
    ///
    /// Panics if `idx` is zero or exceeds `u32::MAX` — an AST with
    /// more than `u32::MAX` expressions cannot be addressed by
    /// `ExprId` and is rejected here rather than silently truncated.
    fn from_index(idx: usize) -> Self {
        let raw = u32::try_from(idx).expect("ExprId index out of range (>= 2^32)");
        ExprId(NonZeroU32::new(raw).expect("ExprId index must be >= 1"))
    }

    /// Array index into `exprs`.
    pub fn index(self) -> usize {
        self.0.get() as usize
    }
}

/// Index into [`Ast::stmts`]. Same layout and sentinel convention
/// as [`ExprId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StmtId(NonZeroU32);

impl StmtId {
    /// Convert from a `usize` array index; see [`ExprId::from_index`].
    fn from_index(idx: usize) -> Self {
        let raw = u32::try_from(idx).expect("StmtId index out of range (>= 2^32)");
        StmtId(NonZeroU32::new(raw).expect("StmtId index must be >= 1"))
    }

    /// Array index into `stmts`.
    pub fn index(self) -> usize {
        self.0.get() as usize
    }
}

// ---------- List ranges ----------

/// A `[offset, offset+len)` slice of the `expr_lists` side arena —
/// the argument list of a [`ExprKind::Call`] or
/// [`ExprKind::MethodCall`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExprList {
    offset: u32,
    len: u32,
}

impl ExprList {
    fn as_range(self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        start..start + self.len as usize
    }
}

/// A `[offset, offset+len)` slice of the `stmt_lists` side arena —
/// a block body, an elif body, or a function body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StmtList {
    offset: u32,
    len: u32,
}

impl StmtList {
    fn as_range(self) -> std::ops::Range<usize> {
        let start = self.offset as usize;
        start..start + self.len as usize
    }
}

// ---------- Expressions ----------

/// A single expression: kind plus inline source span.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: SimpleSpan,
}

/// The kind of expression. Children are [`ExprId`] indices into the
/// arena, never boxed subtrees; argument lists are [`ExprList`]
/// ranges into the `expr_lists` side arena.
///
/// Every payload field is `Copy`, so the whole enum is `Copy` and
/// matching on `ast.expr(id).kind` needs no references.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExprKind {
    Literal(Literal),
    Ident(StringId),
    BinaryOp(ExprId, BinaryOperator, ExprId),
    UnaryOp(UnaryOperator, ExprId),
    Call(StringId, ExprList),
    MethodCall {
        receiver: ExprId,
        method: StringId,
        args: ExprList,
    },
    /// Call-site mutable borrow marker: `&expr` (M8.3). The inner
    /// expression must resolve to an assignable lvalue (checked in sema).
    Borrow(ExprId),
    /// Slice projection `base[start:end]` (M8.4). Either bound may be
    /// omitted (`s[start:]`, `s[:end]`, `s[:]`). Yields `strview` in sema.
    Slice {
        base: ExprId,
        start: Option<ExprId>,
        end: Option<ExprId>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Literal {
    Int(i64),
    Str(StringId),
    Bool(bool),
    Float(f64),
}

// ---------- Statements ----------

/// A single statement: kind plus inline source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: SimpleSpan,
}

/// The kind of statement. Expression children are [`ExprId`]
/// indices; statement children live in the `stmt_lists` side arena
/// behind [`StmtList`] ranges. Structured payloads (`VarDecl`,
/// `FunctionDef`, `IfStmt`) are carried inline in the variant.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    VarDecl(VarDecl),
    FunctionDef(FunctionDef),
    Return(Option<ExprId>),
    ExprStmt(ExprId),
    IfStmt(IfStmt),
    AssignOrDecl {
        target: Ident,
        value: ExprId,
    },
    CompoundAssign {
        target: Ident,
        op: CompoundOp,
        value: ExprId,
    },
    WhileLoop {
        cond: ExprId,
        body: StmtList,
    },
    ForRange {
        var: Ident,
        iterator: Ident,
        start: ExprId,
        end: ExprId,
        body: StmtList,
    },
    Break,
    Continue,
    /// Placeholder for a statement the parser could not parse. The
    /// parser emits the diagnostic itself and recovers at the next
    /// statement boundary, producing this node so later passes keep
    /// a well-formed (partial) AST; astgen lowers it to nothing.
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IfStmt {
    pub cond: ExprId,
    pub then_block: StmtList,
    pub elif_branches: Vec<ElifBranch>,
    pub else_block: Option<StmtList>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElifBranch {
    pub cond: ExprId,
    pub block: StmtList,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VarDecl {
    pub mutable: bool,
    pub name: Ident,
    pub type_annotation: Option<TypeExpr>,
    pub initializer: ExprId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDef {
    pub name: Ident,
    /// Scalar metadata, not nodes — kept inline rather than arena'd.
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: StmtList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Param {
    pub name: Ident,
    pub type_annotation: TypeExpr,
    pub mode: ParamMode,
    pub span: SimpleSpan,
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

// ---------- Operators ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
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
    And,
    Or,
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
pub enum UnaryOperator {
    Neg,
    Not,
}

impl fmt::Display for UnaryOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnaryOperator::Neg => write!(f, "-"),
            UnaryOperator::Not => write!(f, "not"),
        }
    }
}

/// Compound-assignment operators. Kept `#[repr(u32)]` with
/// `from_raw` because UIR/TIR serialize the discriminant into their
/// `extra` arenas (`uir::compound_assign_view` decodes it back).
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

/// A complete Ryo program: the typed expression/statement arenas,
/// their list side arenas, and the top-level statement ids in
/// source order.
#[derive(Debug, Clone)]
pub struct Ast {
    exprs: Vec<Expr>,
    stmts: Vec<Stmt>,
    expr_lists: Vec<ExprId>,
    stmt_lists: Vec<StmtId>,
    top_level: Vec<StmtId>,
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
        // Slot 0 of each arena is the reserved sentinel — never
        // read, never referenced. Pushing a placeholder keeps the
        // ids 1-based without runtime checks on every read. The
        // placeholder payloads are arbitrary but valid.
        Ast {
            exprs: vec![Expr {
                kind: ExprKind::Literal(Literal::Bool(false)),
                span: SimpleSpan::new((), 0..0),
            }],
            stmts: vec![Stmt {
                kind: StmtKind::Error,
                span: SimpleSpan::new((), 0..0),
            }],
            expr_lists: Vec::new(),
            stmt_lists: Vec::new(),
            top_level: Vec::new(),
            span: SimpleSpan::new((), 0..0),
        }
    }

    /// Lookup an expression by id.
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.index()]
    }

    /// Lookup a statement by id.
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[id.index()]
    }

    /// Source span attached to an expression.
    pub fn expr_span(&self, id: ExprId) -> SimpleSpan {
        self.exprs[id.index()].span
    }

    /// Source span attached to a statement.
    pub fn stmt_span(&self, id: StmtId) -> SimpleSpan {
        self.stmts[id.index()].span
    }

    /// The element slice behind an [`ExprList`] range.
    pub fn expr_list(&self, list: ExprList) -> &[ExprId] {
        &self.expr_lists[list.as_range()]
    }

    /// The element slice behind a [`StmtList`] range.
    pub fn stmt_list(&self, list: StmtList) -> &[StmtId] {
        &self.stmt_lists[list.as_range()]
    }

    /// The program's top-level statements in source order.
    pub fn top_level_stmts(&self) -> &[StmtId] {
        &self.top_level
    }

    /// Record the top-level statement list (called once by the
    /// parser at end of input) and stamp the program span from the
    /// first and last statements.
    pub fn set_top_level(&mut self, stmts: &[StmtId]) {
        self.top_level = stmts.to_vec();
        self.span = match (stmts.first(), stmts.last()) {
            (Some(&first), Some(&last)) => {
                SimpleSpan::new((), self.stmt_span(first).start..self.stmt_span(last).end)
            }
            _ => SimpleSpan::new((), 0..0),
        };
    }
}

// ---------- Builders ----------

impl Ast {
    /// Push an expression with its span and return its id. Slot 0
    /// is the reserved sentinel, so the first real expression lands
    /// at index 1.
    fn push_expr(&mut self, kind: ExprKind, span: SimpleSpan) -> ExprId {
        let idx = self.exprs.len();
        self.exprs.push(Expr { kind, span });
        ExprId::from_index(idx)
    }

    /// Push a statement with its span and return its id; see
    /// [`Self::push_expr`].
    fn push_stmt(&mut self, kind: StmtKind, span: SimpleSpan) -> StmtId {
        let idx = self.stmts.len();
        self.stmts.push(Stmt { kind, span });
        StmtId::from_index(idx)
    }

    /// Copy an argument list into the `expr_lists` side arena,
    /// returning its range. The arenas are addressed by `u32`
    /// offsets; an AST that outgrows `u32::MAX` list entries cannot
    /// be encoded and is rejected here rather than silently
    /// truncated.
    fn push_expr_list(&mut self, items: &[ExprId]) -> ExprList {
        let offset =
            u32::try_from(self.expr_lists.len()).expect("AST expr_lists arena exceeded u32::MAX");
        self.expr_lists.extend_from_slice(items);
        ExprList {
            offset,
            len: u32::try_from(items.len()).expect("AST expr list length exceeded u32::MAX"),
        }
    }

    /// Copy a block body into the `stmt_lists` side arena; see
    /// [`Self::push_expr_list`].
    fn push_stmt_list(&mut self, items: &[StmtId]) -> StmtList {
        let offset =
            u32::try_from(self.stmt_lists.len()).expect("AST stmt_lists arena exceeded u32::MAX");
        self.stmt_lists.extend_from_slice(items);
        StmtList {
            offset,
            len: u32::try_from(items.len()).expect("AST stmt list length exceeded u32::MAX"),
        }
    }

    pub fn literal(&mut self, lit: Literal, span: SimpleSpan) -> ExprId {
        self.push_expr(ExprKind::Literal(lit), span)
    }

    pub fn literal_int(&mut self, value: i64, span: SimpleSpan) -> ExprId {
        self.literal(Literal::Int(value), span)
    }

    pub fn literal_float(&mut self, value: f64, span: SimpleSpan) -> ExprId {
        self.literal(Literal::Float(value), span)
    }

    pub fn literal_str(&mut self, value: StringId, span: SimpleSpan) -> ExprId {
        self.literal(Literal::Str(value), span)
    }

    pub fn ident(&mut self, name: StringId, span: SimpleSpan) -> ExprId {
        self.push_expr(ExprKind::Ident(name), span)
    }

    pub fn binary(
        &mut self,
        op: BinaryOperator,
        lhs: ExprId,
        rhs: ExprId,
        span: SimpleSpan,
    ) -> ExprId {
        self.push_expr(ExprKind::BinaryOp(lhs, op, rhs), span)
    }

    pub fn unary(&mut self, op: UnaryOperator, operand: ExprId, span: SimpleSpan) -> ExprId {
        self.push_expr(ExprKind::UnaryOp(op, operand), span)
    }

    pub fn call(&mut self, name: StringId, args: &[ExprId], span: SimpleSpan) -> ExprId {
        let args = self.push_expr_list(args);
        self.push_expr(ExprKind::Call(name, args), span)
    }

    pub fn method_call(
        &mut self,
        receiver: ExprId,
        method: StringId,
        args: &[ExprId],
        span: SimpleSpan,
    ) -> ExprId {
        let args = self.push_expr_list(args);
        self.push_expr(
            ExprKind::MethodCall {
                receiver,
                method,
                args,
            },
            span,
        )
    }

    /// Call-site mutable-borrow marker `&expr` (M8.3).
    pub fn borrow(&mut self, inner: ExprId, span: SimpleSpan) -> ExprId {
        self.push_expr(ExprKind::Borrow(inner), span)
    }

    /// Slice projection `base[start:end]` (M8.4). `start`/`end` are
    /// `None` for the `s[start:]`, `s[:end]`, `s[:]` shorthands.
    pub fn slice(
        &mut self,
        base: ExprId,
        start: Option<ExprId>,
        end: Option<ExprId>,
        span: SimpleSpan,
    ) -> ExprId {
        self.push_expr(ExprKind::Slice { base, start, end }, span)
    }

    /// `return <expr>`, or bare `return` when `value` is `None`.
    pub fn return_stmt(&mut self, value: Option<ExprId>, span: SimpleSpan) -> StmtId {
        self.push_stmt(StmtKind::Return(value), span)
    }

    pub fn expr_stmt(&mut self, value: ExprId, span: SimpleSpan) -> StmtId {
        self.push_stmt(StmtKind::ExprStmt(value), span)
    }

    pub fn break_stmt(&mut self, span: SimpleSpan) -> StmtId {
        self.push_stmt(StmtKind::Break, span)
    }

    pub fn continue_stmt(&mut self, span: SimpleSpan) -> StmtId {
        self.push_stmt(StmtKind::Continue, span)
    }

    /// Placeholder for an unparseable statement recovered by the
    /// parser; astgen lowers it to nothing.
    pub fn error_stmt(&mut self, span: SimpleSpan) -> StmtId {
        self.push_stmt(StmtKind::Error, span)
    }

    pub fn var_decl(
        &mut self,
        mutable: bool,
        name: Ident,
        type_annotation: Option<TypeExpr>,
        initializer: ExprId,
        span: SimpleSpan,
    ) -> StmtId {
        self.push_stmt(
            StmtKind::VarDecl(VarDecl {
                mutable,
                name,
                type_annotation,
                initializer,
            }),
            span,
        )
    }

    pub fn assign_or_decl(&mut self, target: Ident, value: ExprId, span: SimpleSpan) -> StmtId {
        self.push_stmt(StmtKind::AssignOrDecl { target, value }, span)
    }

    pub fn compound_assign(
        &mut self,
        target: Ident,
        op: CompoundOp,
        value: ExprId,
        span: SimpleSpan,
    ) -> StmtId {
        self.push_stmt(StmtKind::CompoundAssign { target, op, value }, span)
    }

    pub fn while_loop(&mut self, cond: ExprId, body: &[StmtId], span: SimpleSpan) -> StmtId {
        let body = self.push_stmt_list(body);
        self.push_stmt(StmtKind::WhileLoop { cond, body }, span)
    }

    pub fn for_range(
        &mut self,
        var: Ident,
        iterator: Ident,
        start: ExprId,
        end: ExprId,
        body: &[StmtId],
        span: SimpleSpan,
    ) -> StmtId {
        let body = self.push_stmt_list(body);
        self.push_stmt(
            StmtKind::ForRange {
                var,
                iterator,
                start,
                end,
                body,
            },
            span,
        )
    }

    pub fn if_stmt(
        &mut self,
        cond: ExprId,
        then_stmts: &[StmtId],
        elif_branches: &[(ExprId, Vec<StmtId>)],
        else_stmts: Option<&[StmtId]>,
        span: SimpleSpan,
    ) -> StmtId {
        let then_block = self.push_stmt_list(then_stmts);
        let elif_branches = elif_branches
            .iter()
            .map(|(cond, block)| ElifBranch {
                cond: *cond,
                block: self.push_stmt_list(block),
            })
            .collect();
        let else_block = else_stmts.map(|stmts| self.push_stmt_list(stmts));
        self.push_stmt(
            StmtKind::IfStmt(IfStmt {
                cond,
                then_block,
                elif_branches,
                else_block,
            }),
            span,
        )
    }

    pub fn function_def(
        &mut self,
        name: Ident,
        params: &[Param],
        return_type: Option<TypeExpr>,
        body: &[StmtId],
        span: SimpleSpan,
    ) -> StmtId {
        let body = self.push_stmt_list(body);
        self.push_stmt(
            StmtKind::FunctionDef(FunctionDef {
                name,
                params: params.to_vec(),
                return_type,
                body,
            }),
            span,
        )
    }
}

// ---------- Parser-state integration ----------

/// Chumsky parser-state implementation: the parser builds directly
/// into the arenas via `map_with`/`foldl_with` closures that call
/// `e.state()`, with the `Ast` itself as the state object.
///
/// A failed alternative (`or`/`choice` backtracking, `repeated`
/// iteration rollbacks, error recovery) can push nodes before it
/// fails; those ids never escape into a kept node, but without
/// rollback the arenas would accumulate dead entries. Like chumsky's
/// own `TruncateState`, the checkpoint records the four arena
/// lengths and `on_rewind` truncates them, so a rewound region
/// leaves the arenas exactly as it found them. Ids created before
/// the checkpoint sit below the truncation point and stay valid;
/// ids created after it are discarded together with the failed
/// output that held them. (`top_level` needs no checkpoint: it is
/// written once by the final combinator, after which nothing can
/// rewind.)
impl<'src, I: chumsky::input::Input<'src>> chumsky::inspector::Inspector<'src, I> for Ast {
    type Checkpoint = (usize, usize, usize, usize);

    fn on_token(&mut self, _: &I::Token) {}

    fn on_save<'parse>(&self, _: &chumsky::input::Cursor<'src, 'parse, I>) -> Self::Checkpoint {
        (
            self.exprs.len(),
            self.stmts.len(),
            self.expr_lists.len(),
            self.stmt_lists.len(),
        )
    }

    fn on_rewind<'parse>(
        &mut self,
        marker: &chumsky::input::Checkpoint<'src, 'parse, I, Self::Checkpoint>,
    ) {
        let &(exprs, stmts, expr_lists, stmt_lists) = marker.inspector();
        self.exprs.truncate(exprs);
        self.stmts.truncate(stmts);
        self.expr_lists.truncate(expr_lists);
        self.stmt_lists.truncate(stmt_lists);
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
    fn binary_operator_display_for_logical() {
        assert_eq!(format!("{}", BinaryOperator::And), "and");
        assert_eq!(format!("{}", BinaryOperator::Or), "or");
    }

    #[test]
    fn unary_operator_display_for_not() {
        assert_eq!(format!("{}", UnaryOperator::Not), "not");
    }

    #[test]
    fn literal_bool_variants_exist() {
        let _t = Literal::Bool(true);
        let _f = Literal::Bool(false);
    }

    #[test]
    fn type_expr_stays_small() {
        // `span` (16 B) + `name` (4 B) + `is_view` (1 B) must pack
        // into 24 B — see the field-order note on `TypeExpr`.
        assert_eq!(std::mem::size_of::<TypeExpr>(), 24);
    }

    #[test]
    fn optional_ids_stay_niche_filled() {
        // The `NonZeroU32` wrap exists for this: optional children
        // (`return` value, slice bounds) cost one word, not two.
        assert_eq!(std::mem::size_of::<Option<ExprId>>(), 4);
        assert_eq!(std::mem::size_of::<Option<StmtId>>(), 4);
    }

    #[test]
    fn slot_zero_is_reserved_sentinel() {
        let mut ast = Ast::new();
        let e = ast.literal_int(1, span(0, 1));
        let s = ast.expr_stmt(e, span(0, 1));
        // The first real ids land at index 1; slot 0 is never
        // handed out by the builders.
        assert_eq!(e.index(), 1);
        assert_eq!(s.index(), 1);
    }

    #[test]
    fn literal_float_bits_survive_exactly() {
        // The payload is stored inline, so decoding is bit-exact
        // (incl. NaN payloads and -0.0).
        let mut ast = Ast::new();
        for v in [0.0, -0.0, f64::NAN, f64::INFINITY, 1.5e300] {
            let id = ast.literal_float(v, span(0, 0));
            match ast.expr(id).kind {
                ExprKind::Literal(Literal::Float(out)) => assert_eq!(out.to_bits(), v.to_bits()),
                other => panic!("expected Float literal, got {:?}", other),
            }
        }
    }

    #[test]
    fn int_literal_round_trips_i64_extremes() {
        let mut ast = Ast::new();
        for v in [i64::MIN, -1, 0, 1, i64::MAX] {
            let id = ast.literal_int(v, span(0, 0));
            match ast.expr(id).kind {
                ExprKind::Literal(Literal::Int(out)) => assert_eq!(out, v),
                other => panic!("expected Int literal, got {:?}", other),
            }
        }
    }

    #[test]
    fn call_args_round_trip_through_side_arena() {
        let mut pool = InternPool::new();
        let name = pool.intern_str("f");
        let mut ast = Ast::new();
        let a = ast.literal_int(1, span(0, 1));
        let b = ast.literal_int(2, span(4, 5));
        let call = ast.call(name, &[a, b], span(0, 6));
        match ast.expr(call).kind {
            ExprKind::Call(n, args) => {
                assert_eq!(n, name);
                assert_eq!(ast.expr_list(args), &[a, b]);
            }
            other => panic!("expected Call, got {:?}", other),
        }
    }

    #[test]
    fn function_def_keeps_params_inline() {
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
        match &ast.stmt(def).kind {
            StmtKind::FunctionDef(def) => {
                assert_eq!(def.name.name, f);
                assert_eq!(def.params, vec![param]);
                assert_eq!(
                    def.return_type.map(|t| (t.name, t.is_view)),
                    Some((str_, false))
                );
                assert_eq!(ast.stmt_list(def.body), &[body_stmt]);
            }
            other => panic!("expected FunctionDef, got {:?}", other),
        }
    }

    #[test]
    fn if_stmt_round_trips_branches() {
        let mut ast = Ast::new();
        let cond = ast.literal_int(1, span(0, 1));
        let then_s = ast.break_stmt(span(1, 2));
        let elif_cond = ast.literal_int(2, span(2, 3));
        let elif_s = ast.continue_stmt(span(3, 4));
        let else_s = ast.error_stmt(span(4, 5));
        let node = ast.if_stmt(
            cond,
            &[then_s],
            &[(elif_cond, vec![elif_s])],
            Some(&[else_s]),
            span(0, 5),
        );
        match &ast.stmt(node).kind {
            StmtKind::IfStmt(if_stmt) => {
                assert_eq!(if_stmt.cond, cond);
                assert_eq!(ast.stmt_list(if_stmt.then_block), &[then_s]);
                assert_eq!(if_stmt.elif_branches.len(), 1);
                assert_eq!(if_stmt.elif_branches[0].cond, elif_cond);
                assert_eq!(ast.stmt_list(if_stmt.elif_branches[0].block), &[elif_s]);
                assert_eq!(
                    if_stmt.else_block.map(|l| ast.stmt_list(l)),
                    Some(&[else_s][..])
                );
            }
            other => panic!("expected IfStmt, got {:?}", other),
        }
    }
}
