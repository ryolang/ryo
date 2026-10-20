//! AST → UIR structural translation.
//!
//! Named for symmetry with Zig's `AstGen.zig` — the responsibility
//! (lowering an AST into the first IR) is the same. Pure
//! structural translation: resolves syntactic type annotations
//! (`int`/`bool`/`str`) to `TypeId` because those come from
//! `TypeExpr` nodes and have no useful "no types yet"
//! representation. No types are attached to instructions — those
//! are filled in by sema in a later pass (the per-`InstRef`
//! `TypeTable`; Phase 4 will emit TIR instead).
//!
//! Identifier names come pre-interned as `StringId` from the parser,
//! so this stage is allocation-light: it copies handles around.
//!
//! ## Output
//!
//! [`generate`] returns the canonical [`Uir`]. The pipeline driver
//! threads it directly into `sema::analyze` and from there into
//! `codegen::compile`; there is no intermediate tree-shaped IR.

use chumsky::span::{SimpleSpan, Span as _};
use ryo_core::ast;
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::types::{
    EnumVariantDef, InternPool, StringId, StructFlags, TypeId, TypeKind, VariantKind,
};
use ryo_core::uir::{
    InstRef, InstTag, Uir, UirBuilder, UirEnumDecl, UirEnumField, UirEnumVariant, UirParam,
    UirStructDecl, UirStructField,
};
use std::collections::{HashMap, HashSet};

type Span = SimpleSpan;

fn synthetic_span() -> Span {
    crate::spans::cspan(SimpleSpan::new((), 0..0))
}

/// Pre-interned `StringId`s for the primitive type names.
///
/// Phase 2 made identifiers `StringId` handles, so `resolve_type`
/// used to call `pool.str(name)` on every type annotation just to
/// reach the &str-keyed match. Interning the names once at
/// the top of `generate` lets subsequent comparisons be a `StringId`
/// equality check (`u32` compare) instead of a `pool.str` lookup
/// followed by a string compare.
struct Primitives {
    int: StringId,
    str_: StringId,
    strview: StringId,
    bytes: StringId,
    bytesview: StringId,
    bool_: StringId,
    float: StringId,
}

impl Primitives {
    fn new(pool: &mut InternPool) -> Self {
        Primitives {
            int: pool.intern_str("int"),
            str_: pool.intern_str("str"),
            strview: pool.intern_str("strview"),
            bytes: pool.intern_str("bytes"),
            bytesview: pool.intern_str("bytesview"),
            bool_: pool.intern_str("bool"),
            float: pool.intern_str("float"),
        }
    }
}

/// Type-annotation resolution: primitive names plus the struct and enum
/// declarations from `generate`'s pre-scan (M9, M11); primitives win.
///
/// `enum_variant_fields` is the variant directory `EnumLit` lowering
/// consults — variants in declaration order with each payload's field
/// names in declaration order. Built from the AST alone: ready before
/// the define DFS and covers enums that later fail.
struct TypeResolver {
    prims: Primitives,
    struct_types: HashMap<StringId, TypeId>,
    enum_types: HashMap<StringId, TypeId>,
    enum_variant_fields: HashMap<TypeId, Vec<(StringId, VariantKind, Vec<StringId>)>>,
    main: StringId,
    range: StringId,
}

impl TypeResolver {
    fn is_primitive(&self, name: StringId) -> bool {
        let p = &self.prims;
        name == p.int
            || name == p.str_
            || name == p.strview
            || name == p.bytes
            || name == p.bytesview
            || name == p.bool_
            || name == p.float
    }

    /// Resolve any type expression: a plain (or legacy `&name`) name,
    /// an anonymous struct type literal `{q: int, r: int}` (M10), or
    /// the positional form `(int, str)` (M10, ≡ `{0: int, 1: str}`).
    fn resolve_type(
        &self,
        texpr: &ast::TypeExpr,
        ast: &ast::Ast,
        pool: &mut InternPool,
        sink: &mut DiagSink,
        defined_enums: &HashSet<TypeId>,
    ) -> TypeId {
        match &texpr.kind {
            ast::TypeExprKind::Name { name, is_view } => {
                self.resolve_name(*name, *is_view, texpr.span, pool, sink)
            }
            ast::TypeExprKind::Anon { fields } => {
                let fields = ast.type_field_list(*fields);
                self.resolve_anon_fields(fields, ast, pool, sink, defined_enums)
            }
            ast::TypeExprKind::Positional(elems) => {
                // (int, str) is sugar over {0: int, 1: str}: intern
                // the positional names before pool dedup so every
                // spelling of the shape lands on one TypeId.
                let elems = ast.type_expr_list(*elems);
                let mut fields = Vec::with_capacity(elems.len());
                for (i, elem) in elems.iter().enumerate() {
                    let name = pool.intern_str(&i.to_string());
                    fields.push((name, *elem));
                }
                self.resolve_anon_fields(&fields, ast, pool, sink, defined_enums)
            }
        }
    }

    /// `{q: int, r: int}` (and its positional equivalent): resolve
    /// each field's type, reject view-typed fields through the M9
    /// `ViewFieldType` path (Rule 6 — views cannot live in struct
    /// fields), and intern the structural type in written order.
    fn resolve_anon_fields(
        &self,
        fields: &[(StringId, ast::TypeExpr)],
        ast: &ast::Ast,
        pool: &mut InternPool,
        sink: &mut DiagSink,
        defined_enums: &HashSet<TypeId>,
    ) -> TypeId {
        let mut pairs: Vec<(StringId, TypeId)> = Vec::with_capacity(fields.len());
        for &(fname, ref ftexpr) in fields {
            let fty = self.resolve_type(ftexpr, ast, pool, sink, defined_enums);
            if pool.is_view(fty) {
                sink.emit(Diag::error(
                    ftexpr.span,
                    DiagCode::ViewFieldType,
                    format!(
                        "struct fields must be owned values; '{}' is a projection (Rule 6)",
                        pool.display(fty)
                    ),
                ));
            }
            pairs.push((fname, fty));
        }
        // Structural identity keys on field names in order; a repeated
        // name would intern a shape whose layout and field lookups
        // disagree. Diagnose every repeat — same code and message as
        // the value-literal path — and absorb the type.
        let mut seen: Vec<StringId> = Vec::with_capacity(pairs.len());
        let mut duplicate = false;
        for &(fname, ref ftexpr) in fields {
            if seen.contains(&fname) {
                sink.emit(Diag::error(
                    ftexpr.span,
                    DiagCode::DuplicateStructField,
                    format!("field '{}' is specified more than once", pool.str(fname)),
                ));
                duplicate = true;
            } else {
                seen.push(fname);
            }
        }
        if duplicate {
            return pool.error_type();
        }
        // A field that failed to resolve — or names a type whose
        // definition failed — has no layout, and `anon_struct`
        // computes layout eagerly: interning would panic in
        // `size_align`. Absorb instead (the original diagnostic is in
        // the sink; `compatible` matches the error sentinel downstream).
        // Enum definedness comes from `defined_enums` — the pool has no
        // `is_defined_enum` (types.rs is at its line cap).
        if pairs.iter().any(|&(_, fty)| {
            pool.is_error(fty)
                || (matches!(pool.kind(fty), TypeKind::Struct) && !pool.is_defined_struct(fty))
                || (matches!(pool.kind(fty), TypeKind::Enum) && !defined_enums.contains(&fty))
        }) {
            return pool.error_type();
        }
        pool.anon_struct(&pairs)
    }

    fn resolve_name(
        &self,
        name: StringId,
        is_view: bool,
        span: Span,
        pool: &InternPool,
        sink: &mut DiagSink,
    ) -> TypeId {
        if is_view {
            // Legacy `&name` type syntax (M8.4 pre-Q5). Only `&str` was ever
            // valid; it is now a targeted migration error (final spec Q5).
            let msg = if name == self.prims.str_ {
                "`&str` was renamed to `strview` (final spec Q5)".to_string()
            } else {
                format!(
                    "unknown view type: '&{}' (view types are named: `strview`)",
                    pool.str(name)
                )
            };
            sink.emit(Diag::error(span, DiagCode::UnknownType, msg));
            return pool.error_type();
        }
        let p = &self.prims;
        if name == p.int {
            pool.int()
        } else if name == p.str_ {
            pool.str_()
        } else if name == p.strview {
            pool.str_view()
        } else if name == p.bytes {
            pool.bytes()
        } else if name == p.bytesview {
            pool.bytes_view()
        } else if name == p.bool_ {
            pool.bool_()
        } else if name == p.float {
            pool.float()
        } else if let Some(&ty) = self.struct_types.get(&name) {
            ty
        } else if let Some(&ty) = self.enum_types.get(&name) {
            ty
        } else {
            // Only resolve the &str on the unhappy path; the common
            // primitive path stays a pure `StringId` compare.
            sink.emit(Diag::error(
                span,
                DiagCode::UnknownType,
                format!("unknown type: '{}'", pool.str(name)),
            ));
            pool.error_type()
        }
    }
}

/// Lower an [`ast::Ast`] to UIR, accumulating diagnostics in `sink`.
///
/// Returns the lowered UIR even on error (using `pool.error_type()`
/// for any annotation that failed to resolve) so subsequent passes
/// can keep type-checking and surface their own diagnostics. The
/// driver decides whether to proceed based on `sink.has_errors()`.
pub fn generate(program: &ast::Ast, pool: &mut InternPool, sink: &mut DiagSink) -> Uir {
    let mut func_defs: Vec<&ast::FunctionDef> = Vec::new();
    let mut top_level: Vec<ast::StmtId> = Vec::new();

    let main_id = pool.intern_str("main");
    let range_id = pool.intern_str("range");

    // Pre-scan (M9, M11): declare every top-level struct and enum
    // before any field or payload type is resolved (annotations may
    // name later-declared types); both kinds before either is defined.
    let mut struct_types: HashMap<StringId, TypeId> = HashMap::new();
    let mut struct_entries: HashMap<StringId, (TypeId, ast::StructDef, Span)> = HashMap::new();
    let mut struct_order: Vec<StringId> = Vec::new();
    let mut enum_types: HashMap<StringId, TypeId> = HashMap::new();
    let mut enum_entries: HashMap<StringId, (TypeId, ast::EnumDef, Span)> = HashMap::new();
    let mut enum_order: Vec<StringId> = Vec::new();

    for &stmt in program.top_level_stmts() {
        match &program.stmt(stmt).kind {
            ast::StmtKind::FunctionDef(def) => func_defs.push(def),
            // Parser-recovery placeholder: already diagnosed at the
            // parse stage, and not a "top-level statement" for the
            // explicit-main check.
            ast::StmtKind::Error => {}
            // Struct declarations (M9) are declarations, not
            // executable top-level statements — they must not trip
            // the explicit-main check and must not reach
            // `gen_implicit_main`. They lower to `uir.struct_decls`
            // entries below, not to instructions.
            ast::StmtKind::StructDef(def) => {
                let name = def.name.name;
                if struct_entries.contains_key(&name) {
                    sink.emit(Diag::error(
                        def.name.span,
                        DiagCode::DuplicateDeclaration,
                        format!("duplicate struct declaration: '{}'", pool.str(name)),
                    ));
                    continue;
                }
                if enum_entries.contains_key(&name) {
                    // Type names share one namespace across kinds (types.rs relies on this check).
                    sink.emit(Diag::error(
                        def.name.span,
                        DiagCode::DuplicateDeclaration,
                        format!(
                            "type '{}' is declared as both a struct and an enum",
                            pool.str(name),
                        ),
                    ));
                    continue;
                }
                let ty = pool.declare_struct(name);
                struct_types.insert(name, ty);
                struct_entries.insert(name, (ty, *def, program.stmt_span(stmt)));
                struct_order.push(name);
            }
            // Enum declarations (M11) are declarations like structs:
            // filtered out here, defined by the DFS, registered in
            // `uir.enum_decls`.
            ast::StmtKind::EnumDef(def) => {
                let name = def.name.name;
                if enum_entries.contains_key(&name) {
                    sink.emit(Diag::error(
                        def.name.span,
                        DiagCode::DuplicateDeclaration,
                        format!("duplicate enum declaration: '{}'", pool.str(name)),
                    ));
                    continue;
                }
                if struct_entries.contains_key(&name) {
                    sink.emit(Diag::error(
                        def.name.span,
                        DiagCode::DuplicateDeclaration,
                        format!(
                            "type '{}' is declared as both a struct and an enum",
                            pool.str(name),
                        ),
                    ));
                    continue;
                }
                let ty = pool.declare_enum(name);
                enum_types.insert(name, ty);
                enum_entries.insert(name, (ty, *def, program.stmt_span(stmt)));
                enum_order.push(name);
            }
            _ => top_level.push(stmt),
        }
    }

    // Variant directory for `EnumLit` lowering (see `TypeResolver`).
    let enum_variant_fields: HashMap<TypeId, Vec<(StringId, VariantKind, Vec<StringId>)>> =
        enum_entries
            .iter()
            .map(|(_, &(ety, def, _))| {
                let variants = program
                    .enum_variants(def.variants)
                    .iter()
                    .map(|v| {
                        let names = program
                            .struct_field_decls(v.payload.fields)
                            .iter()
                            .map(|&(fname, _)| fname)
                            .collect();
                        (v.name.name, v.payload.kind, names)
                    })
                    .collect();
                (ety, variants)
            })
            .collect();

    let types = TypeResolver {
        prims: Primitives::new(pool),
        struct_types,
        enum_types,
        enum_variant_fields,
        main: main_id,
        range: range_id,
    };

    // Define DFS with cycle detection (M9 structs, M11 enums): one
    // dependency-ordered DFS covers both kinds (cross-kind cycles
    // caught); a by-value self-reference has no finite layout
    // (`InfiniteSize`).
    let mut definer = TypeDefiner {
        structs: &struct_entries,
        enums: &enum_entries,
        types: &types,
        ast: program,
        states: struct_order
            .iter()
            .chain(&enum_order)
            .map(|&name| (name, DefState::Pending))
            .collect(),
        defined_enums: HashSet::new(),
        resolved_structs: HashMap::new(),
        resolved_enums: HashMap::new(),
    };
    for &name in struct_order.iter().chain(&enum_order) {
        definer.define(name, pool, sink);
    }

    let has_explicit_main = func_defs.iter().any(|def| def.name.name == main_id);

    if has_explicit_main && !top_level.is_empty() {
        // Anchor the diagnostic on the first stray top-level stmt;
        // pointing at "the program" with a 0..0 span is useless in
        // a renderer.
        let span = program.stmt_span(top_level[0]);
        sink.emit(Diag::error(
            span,
            DiagCode::TopLevelWithExplicitMain,
            "top-level statements are not allowed when fn main() is defined",
        ));
        // Fall through and lower anyway; sema can still report
        // problems inside `main`.
    }

    let mut b = UirBuilder::new();

    // Register defined structs in source order. Structs that failed
    // (cycle, unknown field type) stay declared-but-undefined in the
    // pool — the sink already holds their errors. They still ride
    // along as error-typed, field-less decls so sema can tell
    // "declared but failed" (quiet recovery at use sites) from
    // "never declared" (E0001).
    for &name in &struct_order {
        let (ty, fields) = if definer.states[&name] != DefState::Defined {
            (pool.error_type(), Vec::new())
        } else {
            let (ty, _, _) = struct_entries[&name];
            (
                ty,
                definer.resolved_structs.remove(&name).unwrap_or_default(),
            )
        };
        b.add_struct_decl(UirStructDecl {
            name,
            ty,
            fields,
            span: struct_entries[&name].2,
        });
    }

    // Register defined enums in source order; failed ones ride along as
    // error-typed, variant-less decls — the struct recovery mirrored.
    for &name in &enum_order {
        let (ty, variants) = if definer.states[&name] != DefState::Defined {
            (pool.error_type(), Vec::new())
        } else {
            let (ty, _, _) = enum_entries[&name];
            (ty, definer.resolved_enums.remove(&name).unwrap_or_default())
        };
        b.add_enum_decl(UirEnumDecl {
            name,
            ty,
            variants,
            span: enum_entries[&name].2,
        });
    }

    for func in &func_defs {
        gen_function_def(
            &mut b,
            program,
            func,
            &types,
            &definer.defined_enums,
            pool,
            sink,
        );
    }
    if !has_explicit_main {
        // Synthesize an implicit `main` from top-level statements.
        // User-defined helper functions still appear above;
        // without this, calls to them in top-level code would
        // dangle as "undefined function" errors in sema.
        gen_implicit_main(
            &mut b,
            program,
            &top_level,
            &types,
            &definer.defined_enums,
            pool,
            sink,
        );
    }

    b.finish()
}

/// Definition progress for one declared type in the define DFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefState {
    /// Declared in the pre-scan; definition not attempted yet.
    Pending,
    /// On the current DFS stack — a field pointing here is a cycle.
    InProgress,
    /// `define_struct` / `define_enum` has run; layout is in the pool.
    Defined,
    /// Abandoned after a diagnostic (cycle, empty enum, duplicate
    /// variant, unresolvable type); dependents must fail too.
    Failed,
}

/// Working state for the type define DFS (M9 structs, M11 enums):
/// one DFS covers both kinds, so cross-kind references order together.
struct TypeDefiner<'a> {
    structs: &'a HashMap<StringId, (TypeId, ast::StructDef, Span)>,
    enums: &'a HashMap<StringId, (TypeId, ast::EnumDef, Span)>,
    types: &'a TypeResolver,
    ast: &'a ast::Ast,
    states: HashMap<StringId, DefState>,
    /// Enum types the DFS has defined so far — the pool has no
    /// `is_defined_enum` probe (types.rs is at its line cap), and
    /// `enum_view` on an undefined enum is an out-of-bounds read.
    defined_enums: HashSet<TypeId>,
    resolved_structs: HashMap<StringId, Vec<UirStructField>>,
    resolved_enums: HashMap<StringId, Vec<UirEnumVariant>>,
}

/// Collect the named type references in a struct field's or enum
/// payload's type expression, recursing into anonymous type literals.
fn named_refs_in(texpr: &ast::TypeExpr, ast: &ast::Ast, out: &mut Vec<(StringId, bool, Span)>) {
    match &texpr.kind {
        ast::TypeExprKind::Name { name, is_view } => out.push((*name, *is_view, texpr.span)),
        ast::TypeExprKind::Anon { fields } => {
            for (_, ft) in ast.type_field_list(*fields) {
                named_refs_in(ft, ast, out);
            }
        }
        ast::TypeExprKind::Positional(elems) => {
            for ft in ast.type_expr_list(*elems) {
                named_refs_in(ft, ast, out);
            }
        }
    }
}

impl TypeDefiner<'_> {
    /// Define one type's fields (struct) or variants (enum) in
    /// dependency order; diagnostics use names in hand (`pool.display(id)`
    /// panics on undefined types).
    fn define(&mut self, name: StringId, pool: &mut InternPool, sink: &mut DiagSink) {
        if self.states[&name] != DefState::Pending {
            return;
        }
        self.states.insert(name, DefState::InProgress);
        if self.structs.contains_key(&name) {
            self.define_struct(name, pool, sink);
        } else {
            self.define_enum(name, pool, sink);
        }
    }

    /// Enforce define-order / detect by-value cycles for one named
    /// type reference in a field or payload type: true when it makes
    /// the owner un-definable; other names are left for `resolve_type`.
    fn order_ref(
        &mut self,
        rname: StringId,
        rspan: Span,
        fname: StringId,
        pool: &mut InternPool,
        sink: &mut DiagSink,
    ) -> bool {
        let is_struct = self.types.struct_types.contains_key(&rname);
        if !is_struct && !self.types.enum_types.contains_key(&rname) {
            return false;
        }
        let noun = if is_struct { "struct" } else { "enum" };
        match self.states[&rname] {
            DefState::InProgress => {
                sink.emit(Diag::error(
                    rspan,
                    DiagCode::InfiniteSize,
                    format!(
                        "{noun} '{}' cannot contain itself by value: field '{}' has \
                         type '{}', which would make its size infinite",
                        pool.str(rname),
                        pool.str(fname),
                        pool.str(rname),
                    ),
                ));
                true
            }
            DefState::Pending => {
                self.define(rname, pool, sink);
                self.states[&rname] != DefState::Defined
            }
            DefState::Defined => false,
            DefState::Failed => true,
        }
    }

    /// Resolve one declaration field list (struct fields or an enum
    /// payload — same grammar), ordering named references first.
    /// Returns the triples and a failed flag.
    fn resolve_field_list(
        &mut self,
        decl_fields: &[(StringId, ast::TypeExpr)],
        pool: &mut InternPool,
        sink: &mut DiagSink,
    ) -> (Vec<(StringId, TypeId, Span)>, bool) {
        let mut fields = Vec::with_capacity(decl_fields.len());
        let mut failed = false;
        for &(fname, texpr) in decl_fields {
            // The order/cycle walk follows named references anywhere in
            // the type expression (anon types included).
            let mut refs = Vec::new();
            named_refs_in(&texpr, self.ast, &mut refs);
            for &(rname, is_view, rspan) in &refs {
                // Primitives skip the walk; `&name` view syntax errors in `resolve_type`.
                if is_view || self.types.is_primitive(rname) {
                    continue;
                }
                if self.order_ref(rname, rspan, fname, pool, sink) {
                    failed = true;
                }
            }
            let fty = self
                .types
                .resolve_type(&texpr, self.ast, pool, sink, &self.defined_enums);
            if pool.is_error(fty) {
                failed = true;
            }
            fields.push((fname, fty, texpr.span));
        }
        (fields, failed)
    }

    /// M9.1 `#[derive(Eq)]` gate, shared by structs and enums; one
    /// diagnostic per offending field.
    fn check_derive_eq(
        &mut self,
        name: StringId,
        fields: impl Iterator<Item = (StringId, TypeId, Span)>,
        pool: &InternPool,
        sink: &mut DiagSink,
    ) {
        for (fname, fty, fspan) in fields {
            if pool.is_eq_capable(fty) {
                continue;
            }
            sink.emit(Diag::error(
                fspan,
                DiagCode::DeriveFieldNotEq,
                format!(
                    "cannot derive 'Eq' for '{}': field '{}' of type '{}' is not Eq-capable",
                    pool.str(name),
                    pool.str(fname),
                    pool.display(fty),
                ),
            ));
        }
    }

    fn define_struct(&mut self, name: StringId, pool: &mut InternPool, sink: &mut DiagSink) {
        let (ty, def, _) = self.structs[&name];
        let decl_fields = self.ast.struct_field_decls(def.fields);
        let (fields, failed) = self.resolve_field_list(decl_fields, pool, sink);
        if failed {
            self.states.insert(name, DefState::Failed);
            return;
        }
        if def.attrs.derive_eq {
            self.check_derive_eq(name, fields.iter().copied(), pool, sink);
        }
        let field_types: Vec<(StringId, TypeId)> =
            fields.iter().map(|&(fname, fty, _)| (fname, fty)).collect();
        pool.define_struct(
            ty,
            name,
            &field_types,
            StructFlags {
                derive_eq: def.attrs.derive_eq,
                repr_c: def.attrs.repr_c,
            },
        );
        self.states.insert(name, DefState::Defined);
        self.resolved_structs.insert(
            name,
            fields
                .into_iter()
                .map(|(fname, fty, fspan)| UirStructField {
                    name: fname,
                    ty: fty,
                    span: fspan,
                })
                .collect(),
        );
    }

    fn define_enum(&mut self, name: StringId, pool: &mut InternPool, sink: &mut DiagSink) {
        let (ty, def, _) = self.enums[&name];
        let decls = self.ast.enum_variants(def.variants);
        // Pre-validation: `define_enum` panics on empty variants /
        // duplicate names.
        if decls.is_empty() {
            sink.emit(Diag::error(
                def.name.span,
                DiagCode::EmptyEnum,
                format!(
                    "enum '{}' must declare at least one variant",
                    pool.str(name)
                ),
            ));
            self.states.insert(name, DefState::Failed);
            return;
        }
        let mut variants: Vec<UirEnumVariant> = Vec::with_capacity(decls.len());
        let mut failed = false;
        let mut seen: Vec<StringId> = Vec::with_capacity(decls.len());
        for decl in decls {
            if seen.contains(&decl.name.name) {
                sink.emit(Diag::error(
                    decl.name.span,
                    DiagCode::DuplicateVariant,
                    format!(
                        "duplicate variant '{}' in enum '{}'",
                        pool.str(decl.name.name),
                        pool.str(name),
                    ),
                ));
                failed = true;
            } else {
                seen.push(decl.name.name);
            }
            // Payload fields resolve through `resolve_field_list`.
            let payload_fields = self.ast.struct_field_decls(decl.payload.fields);
            let (fields, payload_failed) = self.resolve_field_list(payload_fields, pool, sink);
            failed |= payload_failed;
            variants.push(UirEnumVariant {
                name: decl.name.name,
                kind: decl.payload.kind,
                fields: fields
                    .into_iter()
                    .map(|(fname, fty, fspan)| UirEnumField {
                        name: fname,
                        ty: fty,
                        span: fspan,
                    })
                    .collect(),
                span: decl.span,
            });
        }
        if failed {
            self.states.insert(name, DefState::Failed);
            return;
        }
        if def.attrs.derive_eq {
            let fields = variants
                .iter()
                .flat_map(|v| v.fields.iter())
                .map(|f| (f.name, f.ty, f.span));
            self.check_derive_eq(name, fields, pool, sink);
        }
        let defs: Vec<EnumVariantDef> = variants
            .iter()
            .map(|v| EnumVariantDef {
                name: v.name,
                kind: v.kind,
                fields: v.fields.iter().map(|f| (f.name, f.ty)).collect(),
            })
            .collect();
        pool.define_enum(
            ty,
            name,
            &defs,
            StructFlags {
                derive_eq: def.attrs.derive_eq,
                repr_c: false,
            },
        );
        self.states.insert(name, DefState::Defined);
        self.defined_enums.insert(ty);
        self.resolved_enums.insert(name, variants);
    }
}

fn lower_block(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmts: &[ast::StmtId],
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> Vec<InstRef> {
    let mut out = Vec::new();
    for &s in stmts {
        gen_stmt(b, ast, s, types, defined_enums, pool, sink, &mut out);
    }
    out
}

fn gen_implicit_main(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmts: &[ast::StmtId],
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    // Synthesize `fn main():` (void return). Codegen falls through
    // to an implicit `ret_void` if no explicit return is emitted —
    // we deliberately do *not* push a synthetic Return here so the
    // body shape matches an explicit `fn main():` written by the
    // user.
    let mut body_stmts: Vec<InstRef> = Vec::new();
    for &stmt in stmts {
        gen_stmt(
            b,
            ast,
            stmt,
            types,
            defined_enums,
            pool,
            sink,
            &mut body_stmts,
        );
    }

    let void_ty = pool.void();
    b.add_function(types.main, vec![], void_ty, &body_stmts, synthetic_span());
}

fn gen_function_def(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    func: &ast::FunctionDef,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) {
    let params: Vec<UirParam> = func
        .params
        .iter()
        .map(|p| UirParam {
            name: p.name.name,
            ty: types.resolve_type(&p.type_annotation, ast, pool, sink, defined_enums),
            mode: p.mode,
            span: p.span,
        })
        .collect();

    let return_type = match &func.return_type {
        Some(ty) => types.resolve_type(ty, ast, pool, sink, defined_enums),
        None => pool.void(),
    };

    // `fn main()` must be `fn main():` — no args, no return type.
    // Non-zero exit codes go through the future `exit(code)`
    // builtin (M24); main is always void in v0.1 and the C-ABI
    // shim emitted by codegen returns 0 to the OS. `types.main` is
    // interned once by `generate`; the probe is a `StringId` compare.
    if func.name.name == types.main {
        if !func.params.is_empty() {
            sink.emit(Diag::error(
                func.name.span,
                DiagCode::MainSignature,
                "fn main() must take no arguments",
            ));
        }
        if let Some(ret) = &func.return_type {
            sink.emit(Diag::error(
                ret.span,
                DiagCode::MainSignature,
                "fn main() must have no return type (use exit(code) for non-zero exit codes)",
            ));
        }
    }

    let body_stmts = lower_block(
        b,
        ast,
        ast.stmt_list(func.body),
        types,
        defined_enums,
        pool,
        sink,
    );

    b.add_function(
        func.name.name,
        params,
        return_type,
        &body_stmts,
        func.name.span,
    );
}

/// Lower a destructuring pattern (M10) to `InstTag::Destructure`
/// plan(s). Each pattern level becomes one instruction whose plan
/// entries are `(selector, bind)` pairs — positional patterns select
/// by canonical field index, brace patterns by field name, and a
/// `None` bind is a wildcard (the field is destroyed, not bound).
///
/// A nested pattern element (possible only inside a positional
/// pattern) cannot be expressed in one flat plan: astgen binds the
/// field to a synthesized `__ryo_destructure_N` temp and recurses,
/// emitting the sub-pattern's own `Destructure` over a `TempVar` read
/// of that temp as the following statement. `TempVar` is its own UIR
/// tag, so the read resolves only through sema's destructuring-temp
/// side table — never the user scope — and a user-spelled `Var`
/// reference cannot observe or hijack the temp.
#[allow(clippy::too_many_arguments)]
fn lower_destructure_pattern(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    pool: &mut InternPool,
    pattern: ast::PatternId,
    value: InstRef,
    span: Span,
    sink: &mut DiagSink,
    out: &mut Vec<InstRef>,
) {
    let pat = ast.pattern(pattern);
    match &pat.kind {
        ast::PatternKind::Positional(list) => {
            let mut plan: Vec<(u32, Option<StringId>)> = Vec::new();
            // Nested elements recurse as their own statements, which
            // must FOLLOW the enclosing Destructure (it binds the temp
            // their `Var` rhs resolves through) — collect and lower
            // them after this level's instruction is emitted.
            let mut nested: Vec<(ast::PatternId, StringId, Span)> = Vec::new();
            for (i, elem) in ast.pattern_list(*list).iter().enumerate() {
                let elem_pat = ast.pattern(*elem);
                match &elem_pat.kind {
                    ast::PatternKind::Bind(ident) => {
                        plan.push((i as u32, Some(ident.name)));
                    }
                    ast::PatternKind::Wildcard => plan.push((i as u32, None)),
                    ast::PatternKind::Anon { .. } | ast::PatternKind::Positional(_) => {
                        let n = b.next_destructure_temp();
                        let temp = pool.intern_str(&format!("__ryo_destructure_{n}"));
                        plan.push((i as u32, Some(temp)));
                        nested.push((*elem, temp, elem_pat.span));
                    }
                    // Literal and variant patterns parse only in match
                    // arms; the destructure grammar cannot produce them
                    // (see `parser/match_.rs`).
                    ast::PatternKind::Literal(_) | ast::PatternKind::Variant { .. } => {
                        unreachable!("destructure patterns bind or skip only (match arms only)")
                    }
                }
            }
            out.push(b.destructure(value, true, &plan, span));
            for (elem, temp, elem_span) in nested {
                let elem_value = b.temp_var_ref(temp, elem_span);
                lower_destructure_pattern(b, ast, pool, elem, elem_value, elem_span, sink, out);
            }
        }
        ast::PatternKind::Anon { fields } => {
            let mut plan: Vec<(u32, Option<StringId>)> = Vec::new();
            for field in ast.pattern_field_list(*fields) {
                let bind = match field.kind {
                    // A pun binds a local named after the field.
                    ast::PatternFieldKind::Pun => Some(field.name),
                    ast::PatternFieldKind::Rename(bind) => Some(bind),
                    ast::PatternFieldKind::Wildcard => None,
                    // The destructure grammar cannot produce literal
                    // fields — they parse only in match arms. Defensive
                    // rejection rather than a silent skip.
                    ast::PatternFieldKind::Literal(_) => {
                        sink.emit(Diag::error(
                            pat.span,
                            DiagCode::ParseError,
                            "literal patterns are valid only in match arms",
                        ));
                        None
                    }
                };
                plan.push((field.name.raw(), bind));
            }
            out.push(b.destructure(value, false, &plan, span));
        }
        ast::PatternKind::Wildcard | ast::PatternKind::Bind(_) => {
            unreachable!("parser guarantees a destructure target is a shaped pattern")
        }
        ast::PatternKind::Literal(_) | ast::PatternKind::Variant { .. } => {
            unreachable!("destructure patterns bind or skip only (match arms only)")
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn gen_stmt(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    stmt: ast::StmtId,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    out: &mut Vec<InstRef>,
) {
    let span = ast.stmt_span(stmt);
    match &ast.stmt(stmt).kind {
        ast::StmtKind::VarDecl(decl) => {
            let initializer = gen_expr(b, ast, decl.initializer, types, defined_enums, pool, sink);
            let ty = decl
                .type_annotation
                .as_ref()
                .map(|ann| types.resolve_type(ann, ast, pool, sink, defined_enums));
            let r = b.var_decl(decl.name.name, decl.mutable, ty, initializer, span);
            out.push(r);
        }
        ast::StmtKind::Return(value) => match value {
            Some(expr) => {
                let value = gen_expr(b, ast, *expr, types, defined_enums, pool, sink);
                out.push(b.unary(InstTag::Return, value, span));
            }
            None => {
                out.push(b.return_void(span));
            }
        },
        ast::StmtKind::ExprStmt(value) => {
            let value = gen_expr(b, ast, *value, types, defined_enums, pool, sink);
            out.push(b.unary(InstTag::ExprStmt, value, span));
        }
        ast::StmtKind::FunctionDef(_) => {
            sink.emit(Diag::error(
                span,
                DiagCode::NestedFunctionDef,
                "nested function definitions are not supported",
            ));
        }
        ast::StmtKind::AssignOrDecl { target, value } => {
            let value_ref = gen_expr(b, ast, *value, types, defined_enums, pool, sink);
            let r = b.assign_or_decl(target.name, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::CompoundAssign { target, op, value } => {
            let value_ref = gen_expr(b, ast, *value, types, defined_enums, pool, sink);
            let r = b.compound_assign(target.name, *op, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::FieldAssign { target, value } => {
            let target_ref = gen_expr(b, ast, *target, types, defined_enums, pool, sink);
            let value_ref = gen_expr(b, ast, *value, types, defined_enums, pool, sink);
            let r = b.field_assign(target_ref, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::CompoundFieldAssign { target, op, value } => {
            let target_ref = gen_expr(b, ast, *target, types, defined_enums, pool, sink);
            let value_ref = gen_expr(b, ast, *value, types, defined_enums, pool, sink);
            let r = b.compound_field_assign(target_ref, *op, value_ref, span);
            out.push(r);
        }
        ast::StmtKind::Destructure { target, value } => {
            let value_ref = gen_expr(b, ast, *value, types, defined_enums, pool, sink);
            lower_destructure_pattern(b, ast, pool, *target, value_ref, span, sink, out);
        }
        ast::StmtKind::IfStmt(if_stmt) => {
            let cond = gen_expr(b, ast, if_stmt.cond, types, defined_enums, pool, sink);
            let then_stmts = lower_block(
                b,
                ast,
                ast.stmt_list(if_stmt.then_block),
                types,
                defined_enums,
                pool,
                sink,
            );

            let elif_branches: Vec<_> = ast
                .elif_list(if_stmt.elif_branches)
                .iter()
                .map(|elif| {
                    let elif_cond = gen_expr(b, ast, elif.cond, types, defined_enums, pool, sink);
                    let elif_body = lower_block(
                        b,
                        ast,
                        ast.stmt_list(elif.block),
                        types,
                        defined_enums,
                        pool,
                        sink,
                    );
                    (elif_cond, elif_body)
                })
                .collect();

            let else_stmts = if_stmt.else_block.map(|stmts| {
                lower_block(
                    b,
                    ast,
                    ast.stmt_list(stmts),
                    types,
                    defined_enums,
                    pool,
                    sink,
                )
            });

            let r = b.if_stmt(
                cond,
                &then_stmts,
                &elif_branches,
                else_stmts.as_deref(),
                span,
            );
            out.push(r);
        }
        ast::StmtKind::WhileLoop { cond, body } => {
            let cond_ref = gen_expr(b, ast, *cond, types, defined_enums, pool, sink);
            let body_refs = lower_block(
                b,
                ast,
                ast.stmt_list(*body),
                types,
                defined_enums,
                pool,
                sink,
            );
            let r = b.while_loop(cond_ref, &body_refs, span);
            out.push(r);
        }
        ast::StmtKind::ForRange {
            var,
            iterator,
            start,
            end,
            body,
        } => {
            // `types.range` is interned once by `generate`; the
            // per-loop probe is a `StringId` compare (the `pool.str`
            // survives only in the diagnostic).
            if iterator.name != types.range {
                sink.emit(Diag::error(
                    iterator.span,
                    DiagCode::ParseError,
                    format!(
                        "only `range(...)` is supported in `for` loops in v0.1, got `{}`",
                        pool.str(iterator.name),
                    ),
                ));
            }
            let start_ref = gen_expr(b, ast, *start, types, defined_enums, pool, sink);
            let end_ref = gen_expr(b, ast, *end, types, defined_enums, pool, sink);
            let body_refs = lower_block(
                b,
                ast,
                ast.stmt_list(*body),
                types,
                defined_enums,
                pool,
                sink,
            );
            let r = b.for_range(var.name, start_ref, end_ref, &body_refs, span);
            out.push(r);
        }
        ast::StmtKind::Break => {
            let r = b.break_stmt(span);
            out.push(r);
        }
        ast::StmtKind::Continue => {
            let r = b.continue_stmt(span);
            out.push(r);
        }
        // Unparseable statement recovered by the parser. The parse
        // diagnostic was already emitted; lower it to a UIR
        // `Unreachable` sentinel so sema's TIR `Unreachable`
        // suppression keeps it from cascading (e.g. a spurious
        // E0036 missing-return when the broken statement was the
        // body's only `return`).
        ast::StmtKind::Error => {
            let r = b.unreachable(span);
            out.push(r);
        }
        // Struct/enum declarations are filtered out before lowering (see `generate`).
        ast::StmtKind::StructDef(_) => {}
        ast::StmtKind::EnumDef(_) => {}
    }
}

fn gen_expr(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    expr: ast::ExprId,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> InstRef {
    let span = ast.expr_span(expr);
    match ast.expr(expr).kind {
        ast::ExprKind::Literal(lit) => match lit {
            ast::Literal::Int(n) => b.int_literal(n, span),
            ast::Literal::Str(id) => b.str_literal(id, span),
            ast::Literal::Bool(v) => b.bool_literal(v, span),
            ast::Literal::Float(v) => b.float_literal(v, span),
            ast::Literal::Bytes(id) => b.bytes_literal(id, span),
        },
        ast::ExprKind::Ident(name) => b.var_ref(name, span),
        ast::ExprKind::BinaryOp(lhs, op, rhs) => {
            let l = gen_expr(b, ast, lhs, types, defined_enums, pool, sink);
            let r = gen_expr(b, ast, rhs, types, defined_enums, pool, sink);
            let tag = match op {
                ast::BinaryOperator::Add => InstTag::Add,
                ast::BinaryOperator::Sub => InstTag::Sub,
                ast::BinaryOperator::Mul => InstTag::Mul,
                ast::BinaryOperator::Div => InstTag::Div,
                ast::BinaryOperator::Eq => InstTag::Eq,
                ast::BinaryOperator::NotEq => InstTag::NotEq,
                ast::BinaryOperator::Lt => InstTag::Lt,
                ast::BinaryOperator::Gt => InstTag::Gt,
                ast::BinaryOperator::LtEq => InstTag::LtEq,
                ast::BinaryOperator::GtEq => InstTag::GtEq,
                ast::BinaryOperator::Mod => InstTag::Mod,
                ast::BinaryOperator::And => InstTag::And,
                ast::BinaryOperator::Or => InstTag::Or,
            };
            b.binary(tag, l, r, span)
        }
        ast::ExprKind::UnaryOp(op, operand) => {
            let s = gen_expr(b, ast, operand, types, defined_enums, pool, sink);
            let tag = match op {
                ast::UnaryOperator::Neg => InstTag::Neg,
                ast::UnaryOperator::Not => InstTag::Not,
            };
            b.unary(tag, s, span)
        }
        ast::ExprKind::Call(name, args) => {
            let arg_refs: Vec<InstRef> = ast
                .expr_list(args)
                .iter()
                .map(|&a| gen_expr(b, ast, a, types, defined_enums, pool, sink))
                .collect();
            b.call(name, &arg_refs, span)
        }
        ast::ExprKind::MethodCall {
            receiver,
            method,
            args,
        } => {
            let receiver_ref = gen_expr(b, ast, receiver, types, defined_enums, pool, sink);
            let arg_refs: Vec<InstRef> = ast
                .expr_list(args)
                .iter()
                .map(|&a| gen_expr(b, ast, a, types, defined_enums, pool, sink))
                .collect();
            b.method_call(receiver_ref, method, &arg_refs, span)
        }
        ast::ExprKind::Borrow(inner) => {
            let inner_ref = gen_expr(b, ast, inner, types, defined_enums, pool, sink);
            b.borrow(inner_ref, span)
        }
        ast::ExprKind::Slice { base, start, end } => {
            // Slice projection `base[start:end]` (final spec §3);
            // bounds are optional shorthands. Sema type-checks the
            // base and yields `strview`.
            let base_ref = gen_expr(b, ast, base, types, defined_enums, pool, sink);
            let start_ref = start.map(|e| gen_expr(b, ast, e, types, defined_enums, pool, sink));
            let end_ref = end.map(|e| gen_expr(b, ast, e, types, defined_enums, pool, sink));
            b.slice(base_ref, start_ref, end_ref, span)
        }
        ast::ExprKind::Index { base, index } => {
            // Scalar indexing `base[index]` (M8.4.2). Sema gates the
            // base to bytes/bytesview.
            let base_ref = gen_expr(b, ast, base, types, defined_enums, pool, sink);
            let index_ref = gen_expr(b, ast, index, types, defined_enums, pool, sink);
            b.index(base_ref, index_ref, span)
        }
        // Struct literal `Name{field=value, ...}` (M9) or the
        // anonymous `{field=value, ...}` (M10). The field
        // initializers stay in source order; sema canonicalizes them
        // against the declaration in `uir.struct_decls` (named) or
        // infers the structural type from the field values
        // (anonymous — the UIR name slot carries the
        // `UIR_ANON_STRUCT_NAME` sentinel).
        ast::ExprKind::StructLiteral(lit) => {
            let fields: Vec<(StringId, InstRef)> = ast
                .struct_field_inits(lit.fields)
                .iter()
                .map(|&(fname, e)| (fname, gen_expr(b, ast, e, types, defined_enums, pool, sink)))
                .collect();
            let name = lit.name.map(|ident| ident.name);
            b.struct_lit(name, &fields, span)
        }
        // Field access `object.field` (M9); chains fold left in the
        // AST, so each access lowers against its own object.
        ast::ExprKind::FieldAccess { object, field } => {
            let obj = gen_expr(b, ast, object, types, defined_enums, pool, sink);
            b.field_access(obj, field.name, span)
        }
        // Variant construction (M11) lowers to `EnumLit`.
        ast::ExprKind::VariantConstruct(c) => {
            variants::lower_variant_construct(b, ast, c, types, defined_enums, pool, sink, span)
        }
        // Match expressions (M12) lower to the UIR `Match` wire; see
        // `astgen/match_.rs` for the pattern/body mapping.
        ast::ExprKind::Match { scrutinee, arms } => match_::gen_match(
            b,
            ast,
            scrutinee,
            arms,
            types,
            defined_enums,
            pool,
            sink,
            span,
        ),
    }
}

mod match_;
mod variants;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod enum_tests;

#[cfg(test)]
mod match_tests;

#[cfg(test)]
mod type_literal_tests {
    use super::tests::{body_named, parse_and_lower};
    use super::*;
    use ryo_core::types::TypeKind;

    #[test]
    fn positional_type_expr_resolves_to_same_type_as_anon_fields() {
        // `TypeExprKind::Positional` resolution must intern the
        // "0"/"1" names and dedup against the spelled-out
        // `{0: int, 1: str}` form — tuple sugar and brace spelling
        // are one structural TypeId.
        let mut pool = InternPool::new();
        let f = pool.intern_str("f");
        let g = pool.intern_str("g");
        let int = pool.intern_str("int");
        let str_ = pool.intern_str("str");
        let zero = pool.intern_str("0");
        let one = pool.intern_str("1");
        let mut ast = ast::Ast::new();
        let int_ty = ast::TypeExpr::new(int, synthetic_span());
        let str_ty = ast::TypeExpr::new(str_, synthetic_span());
        let positional = ast.type_expr_positional(&[int_ty, str_ty], synthetic_span());
        let anon = ast.type_expr_anon(&[(zero, int_ty), (one, str_ty)], synthetic_span());
        let body = ast.return_stmt(None, synthetic_span());
        let f_ident = ast::Ident::new(f, synthetic_span());
        let g_ident = ast::Ident::new(g, synthetic_span());
        let f_def = ast.function_def(f_ident, &[], Some(positional), &[body], synthetic_span());
        let g_def = ast.function_def(g_ident, &[], Some(anon), &[body], synthetic_span());
        ast.set_top_level(vec![f_def, g_def]);

        let mut sink = DiagSink::new();
        let uir = generate(&ast, &mut pool, &mut sink);
        assert!(!sink.has_errors(), "diags: {:?}", sink.into_diags());

        let positional_ret = body_named(&uir, &pool, "f").return_type;
        let anon_ret = body_named(&uir, &pool, "g").return_type;
        assert_eq!(
            positional_ret, anon_ret,
            "positional and brace spellings are one TypeId"
        );
        assert!(matches!(pool.kind(positional_ret), TypeKind::AnonStruct));
        let view = pool.struct_view(positional_ret);
        let names: Vec<&str> = view.fields.iter().map(|f| pool.str(f.name)).collect();
        assert_eq!(names, ["0", "1"]);
        assert_eq!(view.fields[0].ty, pool.int());
        assert_eq!(view.fields[1].ty, pool.str_());
    }

    #[test]
    fn anon_type_literal_annotation_lowers_to_structural_type() {
        // `dm: {q: int, r: int} = ...` resolves the annotation to the
        // same anon TypeId the equivalent literal would infer.
        let (uir, pool) = parse_and_lower("fn main():\n\tdm: {q: int, r: int} = {q=1, r=2}\n")
            .expect("type literal annotation should resolve");
        let main = body_named(&uir, &pool, "main");
        let v = uir.var_decl_view(uir.body_stmts(main).at(0));
        let ty = v.ty.expect("annotation must produce a declared type");
        assert!(matches!(pool.kind(ty), TypeKind::AnonStruct));
        assert_eq!(pool.display(ty).to_string(), "{q: int, r: int}");
    }
}
