//! Statement nodes.
//!
//! Split out of `ast.rs` to stay under the file-length limit (R2).
//! Statements live in the `stmts` arena declared on [`Ast`]; this
//! module holds the statement node types (`Stmt`, `StmtKind`) and
//! their structured payload structs (`IfStmt`, `ElifBranch`,
//! `VarDecl`, `StructAttrs`, `StructDef`, `EnumAttrs`, `EnumDef`,
//! `EnumVariantDecl`, `EnumPayload`, `StructLiteral`,
//! `VariantConstruct`, `VariantArgs`, `FunctionDef`, `Param`).
//! Expression children are [`ExprId`] indices; statement children
//! live in the `stmt_lists` side arena behind [`StmtList`] ranges.

use super::{
    CompoundOp, ElifList, EnumVariantDeclList, ExprId, ExprList, Ident, PatternId, StmtList,
    StructFieldDeclList, StructFieldInitList, TypeExpr,
};
use crate::span::Span;
use crate::tir::ParamMode;
use crate::types::VariantKind;

/// A single statement: kind plus inline source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

/// The kind of statement. Expression children are [`ExprId`]
/// indices; statement children live in the `stmt_lists` side arena
/// behind [`StmtList`] ranges. Structured payloads (`VarDecl`,
/// `FunctionDef`, `IfStmt`) are carried inline in the variant.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    VarDecl(VarDecl),
    FunctionDef(FunctionDef),
    /// `struct Name:` declaration (M9). The field list lives in the
    /// `struct_field_decls` side arena, in declaration order.
    StructDef(StructDef),
    /// `enum Name:` declaration (M11). The variant list lives in the
    /// `enum_variants` side arena, in declaration order.
    EnumDef(EnumDef),
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
    /// Field-path assignment `p.x = v` (M9). `target` is a
    /// `FieldAccess` chain expression rooted at an `Ident`; bare
    /// identifiers stay on `AssignOrDecl`.
    FieldAssign {
        target: ExprId,
        value: ExprId,
    },
    /// Compound field-path assignment `p.x += v` (M9). Same target
    /// shape as `FieldAssign`.
    CompoundFieldAssign {
        target: ExprId,
        op: CompoundOp,
        value: ExprId,
    },
    /// Destructuring assignment `pattern = value` (M10): `(a, b) = e`,
    /// `a, b = e`, `{q, r} = e`, `{x = quot} = e`. The pattern lives
    /// in the `patterns` arena; the parser guarantees it is not a bare
    /// binding (plain `a = e` stays [`StmtKind::AssignOrDecl`]).
    Destructure {
        target: PatternId,
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

// All fields are `Copy` handles, so the struct is too — matching on
// `StmtKind::IfStmt(...)` can move it out of a shared reference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IfStmt {
    pub cond: ExprId,
    pub then_block: StmtList,
    pub elif_branches: ElifList,
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

/// Attributes parsed off a `struct` declaration (M9.1): the
/// `#[derive(Eq)]` / `#[repr(C)]` bits. The parser is pool-less, so
/// the frontend recognizes the attribute vocabulary by the fixed
/// `StringId::ATTR_*` ids and stores plain booleans here; later passes
/// map them to type-pool flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StructAttrs {
    /// `#[derive(Eq)]` — synthesize memberwise `==` / `!=`.
    pub derive_eq: bool,
    /// `#[repr(C)]` — pin declaration-order field layout (today the
    /// only layout; recorded so a reordering optimizer stays honest).
    pub repr_c: bool,
}

/// A `struct` declaration (M9). All fields are `Copy` handles; the
/// field list itself lives in the `struct_field_decls` side arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructDef {
    pub name: Ident,
    pub fields: StructFieldDeclList,
    pub attrs: StructAttrs,
}

/// Attributes parsed off an `enum` declaration (M11, the I-193 slice):
/// only `#[derive(Eq)]` is accepted. `#[repr(C)]` on an enum is
/// rejected at the parse stage (E0117) — pinned layout is a struct
/// feature — so unlike [`StructAttrs`] there is no `repr_c` bit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnumAttrs {
    /// `#[derive(Eq)]` — synthesize tag + payload `==` / `!=`.
    pub derive_eq: bool,
}

/// An `enum` declaration (M11). All fields are `Copy` handles; the
/// variant list lives in the `enum_variants` side arena, in
/// declaration order. An empty body parses (the empty-enum diagnostic
/// is astgen's, not the parser's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumDef {
    pub name: Ident,
    pub variants: EnumVariantDeclList,
    pub attrs: EnumAttrs,
}

/// One variant line of an `enum` declaration (M11). All fields are
/// `Copy` handles; unit variants carry an empty `fields` range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumVariantDecl {
    pub name: Ident,
    pub payload: EnumPayload,
    pub span: Span,
}

/// The payload of an enum variant (M11). Named payloads
/// (`Rectangle(width: float)`) store their `name: type` decls in the
/// `struct_field_decls` arena; tuple payloads (`Circle(float)`) store
/// their bare types in the same arena under synthesized `"0"`, `"1"`,
/// … names (the M10 tuple-sugar convention), so all payload handling
/// downstream is uniform. Unit variants store an empty range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumPayload {
    pub kind: VariantKind,
    pub fields: StructFieldDeclList,
}

/// A struct literal `Name{field=value, ...}` (M9) or the anonymous
/// `{field=value, ...}` (M10). `name` is `None` for the anonymous
/// form — its type identity is the field shape, not a declared name.
/// All fields are `Copy` handles; the field initializers live in the
/// `struct_field_inits` side arena, in source order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructLiteral {
    pub name: Option<Ident>,
    pub fields: StructFieldInitList,
}

/// Enum variant construction (M11): `Shape.Circle(5.0)` positional or
/// `Shape.Rectangle{width=1.0}` named. All fields are `Copy` handles;
/// the argument lists live in the `expr_lists` (positional) and
/// `struct_field_inits` (named) side arenas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantConstruct {
    pub enum_name: Ident,
    pub variant: Ident,
    /// `None` reserves the unit spelling for sema's reinterpretation
    /// of bare `EnumName.Variant`; the parser always passes `Some`.
    pub args: Option<VariantArgs>,
}

/// The argument payload of a [`VariantConstruct`]: a parenthesized
/// positional list or a braced named-init list. Exactly one of the two
/// is `Some` (braces are construction-only per the Brace Law, D11;
/// declarations name payload fields with parens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantArgs {
    pub positional: Option<ExprList>,
    pub named: Option<StructFieldInitList>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDef {
    pub name: Ident,
    /// The one deliberate exception to the side-arena discipline:
    /// params are scalar metadata, not nodes, so they stay an
    /// inline `Vec`.
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: StmtList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Param {
    pub name: Ident,
    pub type_annotation: TypeExpr,
    pub mode: ParamMode,
    pub span: Span,
}
