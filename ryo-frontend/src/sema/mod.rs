//! Semantic analysis: type-check UIR and emit TIR.
//!
//! Sema consumes the flat [`Uir`] produced by `astgen` and emits one
//! [`Tir`] per function body, fully typed. Codegen consumes the
//! resulting `&[Tir]` directly.
//!
//! ## Phase 5 — worklist driver
//!
//! Explicit signatures are registered before body analysis. A call-graph
//! pre-pass rejects recursive components that would require fixed-point
//! inference, and a deterministic scheduler finalizes inferred callees before
//! any caller body reads their unions.
//!
//! - [`Sema`] owns a [`DeclState`] table indexed by [`DeclId`] (one
//!   id per function body in `uir.func_bodies`).
//! - Ready declarations transition from `Unresolved` → `InProgress` →
//!   `Resolved` in source order, with TIR landed in the corresponding slot of
//!   `Sema::results`. Rejected inferred components transition directly to
//!   `Failed` and release their callers for diagnostic recovery.
//! - An inferred body records its concrete error members and deferred
//!   coercions. Finalization patches the concrete union into that body's
//!   signature and TIR before the result is published.
//!
//! Tests at the bottom of this file include a
//! `cfg(any())`-gated block of comptime / generics smoke tests
//! — infrastructure-only stubs awaiting those milestones.
//!
//! ## Error handling
//!
//! Sema continues past errors. When an expression's type can't be
//! determined, a [`TirTag::Unreachable`] instruction is emitted in
//! its place with `ty = pool.error_type()`, downstream type
//! comparisons treat the error sentinel as compatible with anything
//! (`InternPool::compatible`), and the diagnostic flows into the
//! shared [`DiagSink`]. The driver consults `sink.has_errors()` to
//! decide whether to proceed to codegen — codegen itself must never
//! see an `Unreachable`.
//!
//! Warnings (W0001/W0002/W0003 case A) are buffered on [`Sema`] and returned
//! to the caller, which flushes them only when the whole unit —
//! including the ownership pass — is error-free. A warning from an
//! early decl must not pile onto an error discovered by any later
//! stage.

use ryo_core::compilation::CompilationContext;
use ryo_core::declaration::{DeclId, DeclarationError};
use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::tir::{ParamMode, Tir, TirBuilder, TirParam, TirRef};
use ryo_core::types::{InternPool, StringId, TypeId, TypeKind};
use ryo_core::uir::{
    FuncBody, InstRef, InstTag, Span, TypeExprId, Uir, UirBodyMap, UirTypeExprKind,
};
use std::collections::HashMap;
use std::path::Path;

mod builtins;
pub(crate) use builtins::*;
mod call;
pub(crate) use call::*;
mod diff;
pub(crate) use diff::*;
mod error_inference;
mod errors;
mod expr;
pub(crate) use expr::*;
mod match_;
pub(crate) use match_::*;
mod scheduler;
mod stmt;
pub(crate) use stmt::*;

// ---------- Decl table ----------

/// Tri-state resolution status for a single declaration.
///
/// Mirrors Zig's `Module.semaDecl` state machine:
///
/// - `Unresolved` — never visited; lazy.
/// - `InProgress` — currently being analyzed; the cycle sentinel.
/// - `Resolved` — TIR landed in `Sema::results[sema.body_index(decl)]`
///   (eager state for everything that follows). The slot is the declaration's
///   position in the current UIR, so the variant carries no payload.
/// - `Failed` — analysis bailed out; downstream callers should
///   suppress cascade errors but not stack-overflow trying to
///   resolve again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclState {
    Unresolved,
    InProgress,
    Resolved,
    /// Recursive inferred signatures are rejected before body analysis;
    /// callers recover through the error sentinel without cascading.
    Failed,
}

#[cfg(test)]
std::thread_local! {
    static TEST_ACTIVE_BODY: std::cell::Cell<Option<DeclId>> = const { std::cell::Cell::new(None) };
}

#[derive(Clone)]
struct FunctionSig {
    params: Vec<TypeId>,
    return_type: TypeId,
}

struct TypeNames {
    int: StringId,
    str_: StringId,
    strview: StringId,
    bytes: StringId,
    bytesview: StringId,
    bool_: StringId,
    float: StringId,
    void: StringId,
    never: StringId,
    utf8_error: StringId,
}

impl TypeNames {
    fn intern(pool: &mut InternPool) -> Self {
        Self {
            int: pool.intern_str("int"),
            str_: pool.intern_str("str"),
            strview: pool.intern_str("strview"),
            bytes: pool.intern_str("bytes"),
            bytesview: pool.intern_str("bytesview"),
            bool_: pool.intern_str("bool"),
            float: pool.intern_str("float"),
            void: pool.intern_str("void"),
            never: pool.intern_str("never"),
            utf8_error: pool.intern_str("Utf8Error"),
        }
    }
}

struct Binding {
    ty: TypeId,
    mutable: bool,
    /// True when the binding names a parameter declared with the
    /// default (borrow) mode — reading it borrows, so a `match` on it
    /// is a borrowed scrutinee (E0133 gates by-move payload binds).
    /// Locals, `move` params, and `inout` params are owned places for
    /// this purpose (`inout` mutation borrows are the ownership pass's
    /// jurisdiction).
    borrowed: bool,
    used: std::cell::Cell<bool>,
    dead_store_span: Option<Span>,
    dead_store_help: Option<&'static str>,
}

pub(crate) enum ScopeParent<'a> {
    Borrowed(&'a Scope<'a>),
    Owned(Box<Scope<'a>>),
}

impl<'a> ScopeParent<'a> {
    pub(crate) fn scope(&self) -> &Scope<'a> {
        match self {
            Self::Borrowed(scope) => scope,
            Self::Owned(scope) => scope,
        }
    }
}

impl<'a> From<&'a Scope<'a>> for ScopeParent<'a> {
    fn from(scope: &'a Scope<'a>) -> Self {
        Self::Borrowed(scope)
    }
}

pub(crate) struct Scope<'a> {
    parent: Option<ScopeParent<'a>>,
    bindings: HashMap<StringId, Binding>,
}

impl<'a> Scope<'a> {
    fn new() -> Self {
        Scope {
            parent: None,
            bindings: HashMap::new(),
        }
    }

    fn insert_binding(&mut self, name: StringId, ty: TypeId, mutable: bool, borrowed: bool) {
        self.bindings.insert(
            name,
            Binding {
                ty,
                mutable,
                borrowed,
                used: std::cell::Cell::new(false),
                dead_store_span: None,
                dead_store_help: None,
            },
        );
    }

    fn insert_dead_store_binding(
        &mut self,
        name: StringId,
        ty: TypeId,
        mutable: bool,
        borrowed: bool,
        span: Span,
    ) {
        self.insert_binding(name, ty, mutable, borrowed);
        if let Some(binding) = self.bindings.get_mut(&name) {
            binding.dead_store_span = Some(span);
        }
    }

    fn insert_direct_catch_binding(&mut self, name: StringId, ty: TypeId, span: Span) {
        self.insert_dead_store_binding(name, ty, false, false, span);
        if let Some(binding) = self.bindings.get_mut(&name) {
            binding.dead_store_help = Some("if no arm needs the shared error, write bare `catch:`");
        }
    }

    fn drain_dead_stores(&self, sema: &mut Sema<'_>) {
        let mut dead: Vec<(StringId, Span, Option<&'static str>)> = self
            .bindings
            .iter()
            .filter_map(|(&name, binding)| {
                (!binding.used.get())
                    .then_some(binding.dead_store_span)
                    .flatten()
                    .map(|span| (name, span, binding.dead_store_help))
            })
            .collect();
        dead.sort_by_key(|(name, span, _)| (span.start, span.end, name.raw()));
        for (name, span, help) in dead {
            let diag = Diag::warning(
                span,
                DiagCode::DeadStore,
                format!("value `{}` is declared but never used", sema.pool.str(name)),
            );
            sema.warnings.push(match help {
                Some(help) => diag.with_help(help),
                None => diag,
            });
        }
    }

    pub(crate) fn take_parent(&mut self) -> Option<ScopeParent<'a>> {
        self.parent.take()
    }

    fn contains_in_current(&self, name: StringId) -> bool {
        self.bindings.contains_key(&name)
    }

    fn lookup(&self, name: StringId) -> Option<TypeId> {
        if let Some(binding) = self.bindings.get(&name) {
            binding.used.set(true);
            Some(binding.ty)
        } else {
            self.parent.as_ref()?.scope().lookup(name)
        }
    }

    fn lookup_full(&self, name: StringId) -> Option<(TypeId, bool)> {
        self.bindings
            .get(&name)
            .map(|b| (b.ty, b.mutable))
            .or_else(|| self.parent.as_ref()?.scope().lookup_full(name))
    }

    /// Whether the nearest binding for `name` is a borrowed parameter
    /// (see [`Binding::borrowed`]); false when unbound. Match scrutinee
    /// analysis uses this to detect a borrowed scrutinee from a plain
    /// parameter read, mirroring the `&expr` case.
    fn lookup_borrowed(&self, name: StringId) -> bool {
        self.bindings
            .get(&name)
            .map(|b| b.borrowed)
            .unwrap_or_else(|| {
                self.parent
                    .as_ref()
                    .is_some_and(|parent| parent.scope().lookup_borrowed(name))
            })
    }
}

// ---------- Public entrypoint ----------

pub struct SemaReport {
    pub tirs: Vec<Tir>,
    pub warnings: Vec<Diag>,
    pub inferred: Vec<InferredSignature>,
}

pub struct InferredSignature {
    pub name: StringId,
    pub return_type: TypeId,
}

/// Analyze `uir` and emit one [`Tir`] per function body.
///
/// Thin wrapper around [`Sema::run`] kept as the stable façade
/// callers (tests, benches) use. Flushes the buffered warnings into
/// `sink` when the unit is error-free — see [`analyze_buffered`] for
/// the deferred variant the pipeline driver uses.
pub fn analyze(
    uir: &Uir,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    source: &str,
    file_path: &Path,
    context: &mut CompilationContext,
) -> Vec<Tir> {
    let (tirs, warnings) = analyze_buffered(uir, pool, sink, source, file_path, context);
    if !sink.has_errors() {
        for d in warnings {
            sink.emit(d);
        }
    }
    tirs
}

/// Like [`analyze`], but returns the buffered warnings (W0001/W0002/W0003
/// case A) instead of flushing them, so the caller can hold them past
/// later pipeline stages (ownership) and emit them only when the whole
/// unit — every stage — is error-free.
pub fn analyze_buffered(
    uir: &Uir,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    source: &str,
    file_path: &Path,
    context: &mut CompilationContext,
) -> (Vec<Tir>, Vec<Diag>) {
    let report = analyze_buffered_report(uir, pool, sink, source, file_path, context);
    (report.tirs, report.warnings)
}

pub fn analyze_buffered_report(
    uir: &Uir,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    source: &str,
    file_path: &Path,
    context: &mut CompilationContext,
) -> SemaReport {
    Sema::run(uir, pool, sink, source, file_path, context)
}

// ---------- Sema driver ----------

/// Worklist-driven sema state. Lives only for the duration of one
/// `Sema::run` call.
pub struct Sema<'a> {
    uir: &'a Uir,
    pool: &'a mut InternPool,
    sink: &'a mut DiagSink,
    context: &'a mut CompilationContext,
    /// Buffered warnings (W0001/W0002/W0003 case A). `Sema::run` returns
    /// them to the caller instead of emitting, so a warning from an
    /// early decl never piles onto an error found by a later stage
    /// (ownership); the caller flushes them only when the whole unit
    /// is error-free. Errors always go to `sink` directly.
    warnings: Vec<Diag>,
    source: &'a str,
    file_path: &'a Path,
    /// Resolution status, parallel to `uir.func_bodies`.
    decl_state: Vec<DeclState>,
    declaration_ids: Vec<DeclId>,
    registration_error: Option<DeclarationError>,
    body_map: Option<UirBodyMap>,
    /// Function name → decl id. Built once at the top of `run` and
    /// shared with `check_call`. A duplicate definition keeps the
    /// first one seen and emits a hard redefinition error.
    name_to_decl: HashMap<StringId, DeclId>,
    /// Eagerly-resolved signatures, keyed by declaration identity.
    /// Out-of-order definitions and recursive / mutually-recursive calls
    /// type-check because callee signatures land here in a single
    /// pass before any body is analyzed.
    signatures: HashMap<DeclId, FunctionSig>,
    type_names: TypeNames,
    /// Dense resolution cache indexed by `TypeExprId`; the two slots
    /// distinguish ordinary annotations from function-return position.
    type_expr_cache: Vec<[Option<TypeId>; 2]>,
    /// Per-decl emitted TIR slot. Filled as decls transition to
    /// `Resolved`. Result extraction drains this in source order.
    results: Vec<Option<Tir>>,
    inference: Vec<error_inference::InferenceState>,
    /// Struct name → interned struct type (M9), populated from
    /// `uir.struct_decls` (which carries only fully-defined structs;
    /// astgen already diagnosed duplicates / cycles / unknown field
    /// types). Struct literals and field accesses resolve against
    /// this table.
    struct_types: HashMap<StringId, TypeId>,
    /// Enum name → interned enum type (M11), populated from
    /// `uir.enum_decls` (which carries failed enums as error-typed,
    /// variant-less decls, exactly like structs — astgen already
    /// diagnosed duplicates / cycles / bad payloads). Variant
    /// constructions and bare `EnumName.Variant` unit accesses resolve
    /// against this table.
    enum_types: HashMap<StringId, TypeId>,
    /// Nominal error name → declared error type, used by constructor
    /// and match resolution.
    pub(crate) error_types: HashMap<StringId, TypeId>,
    /// Interned ids for the names compared on hot paths (builtin
    /// dispatch, materialize intercepts, reserved names). Built once
    /// in `Sema::new`.
    names: crate::builtins::BuiltinNameIds,
    /// `BUILTINS` as (interned name id, builtin) pairs, so `check_call`
    /// resolves a builtin callee by `StringId` equality instead of a
    /// `pool.str` + table scan. A `Vec` scan of integer compares beats
    /// a per-`Sema` hash-map build at this table size (~20 entries) —
    /// the map's SipHash inserts dominated compile-fixed cost, which
    /// matters for tiny programs. Built once in `Sema::new`.
    builtin_by_id: Vec<(StringId, &'static crate::builtins::BuiltinFunction)>,
    /// Refs that appear as direct arguments of some call anywhere in
    /// the program. `&expr` (UIR `Borrow`) is only meaningful as a call
    /// argument to an `inout` parameter; the `Borrow` arm in
    /// `analyze_expr` rejects any `Borrow` inst outside this set. UIR
    /// insts are unique per use, so a program-wide set is precise — a
    /// `Borrow` that is a call arg in one function can never be a stray
    /// `&` in another. Dense side table indexed by `InstRef::index()`;
    /// slot 0 is the unused sentinel and stays `false`.
    call_arg_refs: Vec<bool>,
}

/// Every direct call-argument `InstRef` in the program. Scans
/// `Call` and `MethodCall` instructions (method-call args count; the
/// receiver does not — `(&x).len()` is not a valid borrow position).
fn collect_call_arg_refs(uir: &Uir, map: &UirBodyMap) -> Vec<bool> {
    let mut set = vec![false; uir.instructions.len()];
    for body in 0..uir.func_bodies.len() {
        for &r in map.instructions(body) {
            match uir.inst(r).tag {
                InstTag::Call => {
                    for arg in uir.call_view(r).args {
                        set[arg.index()] = true;
                    }
                }
                InstTag::MethodCall => {
                    for arg in uir.method_call_view(r).args {
                        set[arg.index()] = true;
                    }
                }
                _ => {}
            }
        }
    }
    set
}

impl<'a> Sema<'a> {
    /// Drive the declaration worklist and return one [`Tir`] per UIR
    /// function body (source order) alongside the buffered warnings.
    /// The caller decides when to flush the warnings — see
    /// [`analyze`] (immediate) and [`analyze_buffered`] (deferred).
    pub fn run(
        uir: &'a Uir,
        pool: &'a mut InternPool,
        sink: &'a mut DiagSink,
        source: &'a str,
        file_path: &'a Path,
        context: &'a mut CompilationContext,
    ) -> SemaReport {
        let mut sema = Sema::new(uir, pool, sink, source, file_path, context);
        if sema.body_map.is_none() || sema.registration_error.is_some() {
            return SemaReport {
                tirs: Vec::new(),
                warnings: Vec::new(),
                inferred: Vec::new(),
            };
        }
        sema.register_structs();
        sema.register_enums();
        errors::register_errors(&mut sema);
        sema.resolve_signatures();
        sema.drive();
        let inferred = sema.inferred_signatures();
        // Take the warnings before `collect_results`, which consumes
        // `sema`.
        let warnings = std::mem::take(&mut sema.warnings);
        SemaReport {
            tirs: sema.collect_results(),
            warnings,
            inferred,
        }
    }

    fn new(
        uir: &'a Uir,
        pool: &'a mut InternPool,
        sink: &'a mut DiagSink,
        source: &'a str,
        file_path: &'a Path,
        context: &'a mut CompilationContext,
    ) -> Self {
        let n = uir.func_bodies.len();
        let body_map = match uir.compute_body_map() {
            Ok(map) => Some(map),
            Err(error) => {
                sink.emit(Diag::error(
                    Span::EMPTY,
                    DiagCode::InternalError,
                    format!("invalid UIR body ownership: {error}"),
                ));
                None
            }
        };
        let (declaration_ids, registration_error) = if body_map.is_some() {
            match context.register_functions(uir, pool) {
                Ok(ids) => (ids, None),
                Err(error) => {
                    sink.emit(Diag::error(
                        Span::EMPTY,
                        DiagCode::InternalError,
                        format!("declaration registration failed: {error}"),
                    ));
                    (Vec::new(), Some(error))
                }
            }
        } else {
            (Vec::new(), None)
        };
        let mut name_to_decl = HashMap::with_capacity(n);
        for (body, &decl) in uir.func_bodies.iter().zip(&declaration_ids) {
            // First definition wins on duplicates: calls bind to the
            // first declaration and the duplicate still gets analyzed
            // (so its own errors surface), but the redefinition itself
            // is a hard error.
            match name_to_decl.entry(body.name) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    sink.emit(Diag::error(
                        body.span,
                        DiagCode::DuplicateDeclaration,
                        format!(
                            "function '{}' is defined more than once",
                            pool.str(body.name)
                        ),
                    ));
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(decl);
                }
            }
        }
        let mut results = Vec::with_capacity(n);
        for _ in 0..n {
            results.push(None);
        }
        let names = crate::builtins::BuiltinNameIds::intern(pool);
        let type_names = TypeNames::intern(pool);
        let builtin_by_id: Vec<(StringId, &'static crate::builtins::BuiltinFunction)> =
            crate::builtins::BUILTINS
                .iter()
                .map(|b| (pool.intern_str(b.name), b))
                .collect();
        Sema {
            uir,
            pool,
            sink,
            context,
            warnings: Vec::new(),
            source,
            file_path,
            decl_state: vec![DeclState::Unresolved; n],
            declaration_ids,
            registration_error,
            name_to_decl,
            signatures: HashMap::with_capacity(n),
            type_names,
            type_expr_cache: vec![[None, None]; uir.type_exprs.len()],
            results,
            inference: (0..n)
                .map(|_| error_inference::InferenceState::default())
                .collect(),
            struct_types: HashMap::new(),
            enum_types: HashMap::new(),
            error_types: HashMap::new(),
            names,
            builtin_by_id,
            call_arg_refs: body_map
                .as_ref()
                .map_or_else(Vec::new, |map| collect_call_arg_refs(uir, map)),
            body_map,
        }
    }

    /// Populate `struct_types` from `uir.struct_decls` (M9) and
    /// validate field types. First-wins on a duplicate name — astgen
    /// already emitted `DuplicateDeclaration` for it and kept the
    /// first decl's `TypeId` in the pool.
    ///
    /// Rule 6 / E3: struct fields must be owned values — a view-typed
    /// field (`strview`, `bytesview`) is a projection and is rejected
    /// at its own span (`uir.struct_decls` carries per-field spans
    /// for exactly this).
    fn register_structs(&mut self) {
        for decl in &self.uir.struct_decls {
            for field in &decl.fields {
                if self.pool.is_view(field.ty) {
                    self.sink.emit(Diag::error(
                        field.span,
                        DiagCode::ViewFieldType,
                        format!(
                            "struct fields must be owned values; '{}' is a projection (Rule 6)",
                            self.pool.display(field.ty),
                        ),
                    ));
                }
            }
            self.struct_types.entry(decl.name).or_insert(decl.ty);
        }
    }

    /// Populate `enum_types` from `uir.enum_decls` (M11) and validate
    /// payload field types. First-wins on a duplicate name — astgen
    /// already emitted `DuplicateDeclaration` and kept the first decl's
    /// `TypeId`. Failed enums ride along error-typed and variant-less
    /// (their own diagnostic is already in the sink), so the field
    /// check only runs on defined enums.
    ///
    /// Rule 6 / E3 applies verbatim to payload fields (spec §2): they
    /// must be owned values — a view-typed field (`strview`,
    /// `bytesview`) is rejected at its own span, exactly like struct
    /// fields in [`Sema::register_structs`].
    fn register_enums(&mut self) {
        for decl in &self.uir.enum_decls {
            for variant in &decl.variants {
                for field in &variant.fields {
                    if self.pool.is_view(field.ty) {
                        self.sink.emit(Diag::error(
                            field.span,
                            DiagCode::ViewFieldType,
                            format!(
                                "enum payload fields must be owned values; '{}' is a projection (Rule 6)",
                                self.pool.display(field.ty),
                            ),
                        ));
                    }
                }
            }
            self.enum_types.entry(decl.name).or_insert(decl.ty);
        }
    }

    fn resolve_type_expr(&mut self, id: TypeExprId, allow_inferred: bool) -> TypeId {
        let cache_slot = usize::from(allow_inferred);
        if let Some(resolved) = self.type_expr_cache[id.index()][cache_slot] {
            return resolved;
        }
        let expr = self.uir.type_expr(id);
        let resolved = match expr.kind {
            UirTypeExprKind::Name { name, is_view } => {
                self.resolve_type_name(name, is_view, expr.span)
            }
            UirTypeExprKind::Anon { fields } => {
                let fields = self.uir.type_field_list(fields).to_vec();
                self.resolve_anon_fields(&fields)
            }
            UirTypeExprKind::Positional(elements) => {
                let fields = self
                    .uir
                    .type_expr_list(elements)
                    .iter()
                    .enumerate()
                    .map(|(index, &element)| (self.pool.intern_str(&index.to_string()), element))
                    .collect::<Vec<_>>();
                self.resolve_anon_fields(&fields)
            }
            UirTypeExprKind::ErrorUnion {
                error_set,
                success,
                inferred,
            } => {
                let error_set = self.uir.type_expr_list(error_set).to_vec();
                self.resolve_error_union(&error_set, success, inferred, expr.span, allow_inferred)
            }
        };
        self.type_expr_cache[id.index()][cache_slot] = Some(resolved);
        resolved
    }

    fn resolve_type_name(&mut self, name: StringId, is_view: bool, span: Span) -> TypeId {
        if is_view {
            let message = if name == self.type_names.str_ {
                "`&str` was renamed to `strview` (final spec Q5)".to_string()
            } else {
                format!(
                    "unknown view type: '&{}' (view types are named: `strview`)",
                    self.pool.str(name)
                )
            };
            self.sink
                .emit(Diag::error(span, DiagCode::UnknownType, message));
            return self.pool.error_type();
        }
        let names = &self.type_names;
        if name == names.int {
            self.pool.int()
        } else if name == names.str_ {
            self.pool.str_()
        } else if name == names.strview {
            self.pool.str_view()
        } else if name == names.bytes {
            self.pool.bytes()
        } else if name == names.bytesview {
            self.pool.bytes_view()
        } else if name == names.bool_ {
            self.pool.bool_()
        } else if name == names.float {
            self.pool.float()
        } else if let Some(&ty) = self.struct_types.get(&name) {
            ty
        } else if let Some(&ty) = self.enum_types.get(&name) {
            ty
        } else if let Some(&ty) = self.error_types.get(&name) {
            ty
        } else if let Some(members) = self
            .uir
            .error_families
            .iter()
            .find(|family| family.name == name)
            .map(|family| family.members.clone())
        {
            if members.iter().all(|&member| self.type_has_layout(member)) {
                self.pool.error_set(&members)
            } else {
                self.pool.error_type()
            }
        } else {
            let qualified = self.pool.str(name).to_string();
            if let Some((root, member)) = qualified.split_once('.')
                && let Some(source_members) = self
                    .uir
                    .error_families
                    .iter()
                    .find(|family| self.pool.str(family.name) == root)
                    .map(|family| family.member_names.clone())
            {
                let available = source_members
                    .iter()
                    .map(|name| format!("'{}'", self.pool.str(*name)))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.sink.emit(Diag::error(
                    span,
                    DiagCode::UnknownVariant,
                    format!(
                        "error family '{root}' has no member '{member}' (members: {available})"
                    ),
                ));
                return self.pool.error_type();
            }
            self.sink.emit(Diag::error(
                span,
                DiagCode::UnknownType,
                format!("unknown type: '{}'", self.pool.str(name)),
            ));
            self.pool.error_type()
        }
    }

    fn resolve_anon_fields(&mut self, fields: &[(StringId, TypeExprId)]) -> TypeId {
        let mut pairs = Vec::with_capacity(fields.len());
        for &(name, field) in fields {
            let ty = self.resolve_type_expr(field, false);
            if self.pool.is_view(ty) {
                self.sink.emit(Diag::error(
                    self.uir.type_expr(field).span,
                    DiagCode::ViewFieldType,
                    format!(
                        "struct fields must be owned values; '{}' is a projection (Rule 6)",
                        self.pool.display(ty)
                    ),
                ));
            }
            pairs.push((name, ty));
        }

        let mut seen = Vec::with_capacity(fields.len());
        let mut duplicate = false;
        for &(name, field) in fields {
            if seen.contains(&name) {
                self.sink.emit(Diag::error(
                    self.uir.type_expr(field).span,
                    DiagCode::DuplicateStructField,
                    format!(
                        "field '{}' is specified more than once",
                        self.pool.str(name)
                    ),
                ));
                duplicate = true;
            } else {
                seen.push(name);
            }
        }
        if duplicate || pairs.iter().any(|&(_, ty)| !self.type_has_layout(ty)) {
            return self.pool.error_type();
        }
        self.pool.anon_struct(&pairs)
    }

    fn resolve_error_union(
        &mut self,
        error_set: &[TypeExprId],
        success: TypeExprId,
        inferred: bool,
        span: Span,
        allow_inferred: bool,
    ) -> TypeId {
        if inferred && !allow_inferred {
            self.sink.emit(Diag::error(
                span,
                DiagCode::TypeMismatch,
                "inferred error unions are only valid in function-return position",
            ));
            return self.pool.error_type();
        }
        if !inferred && error_set.is_empty() {
            self.sink.emit(Diag::error(
                span,
                DiagCode::TypeMismatch,
                "an explicit error union must contain at least one error",
            ));
            return self.pool.error_type();
        }
        if inferred {
            let success = self.resolve_error_union_success(success);
            if self.pool.is_error(success) {
                return self.pool.error_type();
            }
            return self.pool.inferred_error_union(success);
        }

        let mut members = Vec::with_capacity(error_set.len());
        let mut failed = false;
        for &member in error_set {
            let ty = self.resolve_type_expr(member, false);
            if self.pool.is_error(ty) {
                failed = true;
                continue;
            }
            match self.pool.kind(ty) {
                TypeKind::DeclaredError | TypeKind::ErrorSet => {
                    let expanded = self.pool.error_set_view(ty).members;
                    if expanded.iter().any(|&member| !self.type_has_layout(member)) {
                        failed = true;
                    }
                    members.extend(expanded);
                }
                _ => {
                    self.sink.emit(Diag::error(
                        self.uir.type_expr(member).span,
                        DiagCode::TypeMismatch,
                        "error union members must be declared errors or error sets",
                    ));
                    failed = true;
                }
            }
        }
        let success = self.resolve_error_union_success(success);
        if failed || self.pool.is_error(success) {
            return self.pool.error_type();
        }
        let error_set = self.pool.error_set(&members);
        self.pool.error_union(error_set, success)
    }

    fn resolve_error_union_success(&mut self, id: TypeExprId) -> TypeId {
        let expr = self.uir.type_expr(id);
        let success = match expr.kind {
            UirTypeExprKind::Name {
                name,
                is_view: false,
            } if name == self.type_names.void => self.pool.void(),
            UirTypeExprKind::Name {
                name,
                is_view: false,
            } if name == self.type_names.never => self.pool.never(),
            _ => self.resolve_type_expr(id, false),
        };
        if self.pool.is_error(success) {
            return success;
        }
        let invalid = match self.pool.kind(success) {
            TypeKind::Never => Some("error union success type cannot be 'never'"),
            TypeKind::View(_) => Some("error union success type must be owned, not a view"),
            TypeKind::DeclaredError
            | TypeKind::ErrorSet
            | TypeKind::ErrorUnion
            | TypeKind::InferredErrorUnion => {
                Some("error types, error sets, and error unions cannot be success types")
            }
            _ => None,
        };
        if let Some(message) = invalid {
            self.sink
                .emit(Diag::error(expr.span, DiagCode::TypeMismatch, message));
            return self.pool.error_type();
        }
        if !self.type_has_layout(success) {
            return self.pool.error_type();
        }
        success
    }

    fn type_has_layout(&self, ty: TypeId) -> bool {
        match self.pool.kind(ty) {
            TypeKind::Error | TypeKind::Never | TypeKind::InferredErrorUnion => false,
            TypeKind::Struct => self.pool.is_defined_struct(ty),
            TypeKind::Enum => self.uir.enum_decls.iter().any(|decl| decl.ty == ty),
            TypeKind::DeclaredError => {
                self.uir.error_decls.iter().any(|decl| decl.ty == ty)
                    || self.error_types.get(&self.type_names.utf8_error) == Some(&ty)
            }
            _ => true,
        }
    }

    /// Populate parameter metadata and explicit return types up front.
    /// Inferred returns start as placeholders and are replaced when their
    /// declaration finalizes; calls never clone the unresolved placeholder.
    fn resolve_signatures(&mut self) {
        for index in 0..self.uir.func_bodies.len() {
            let body = &self.uir.func_bodies[index];
            let name = body.name;
            let span = body.span;
            let param_exprs = body
                .params
                .iter()
                .map(|param| param.type_expr)
                .collect::<Vec<_>>();
            let return_expr = body.return_type;
            check_reserved_name(
                self,
                name,
                span,
                "is a reserved builtin and cannot be used as a function name",
            );
            let params = param_exprs
                .into_iter()
                .map(|id| self.resolve_type_expr(id, false))
                .collect();
            let return_type =
                return_expr.map_or(self.pool.void(), |id| self.resolve_type_expr(id, true));
            let decl = self.declaration_ids[index];
            self.signatures.insert(
                decl,
                FunctionSig {
                    params,
                    return_type,
                },
            );
        }
    }

    fn drive(&mut self) {
        let has_inferred = self
            .declaration_ids
            .iter()
            .copied()
            .any(|decl| error_inference::is_inferred(self, decl));
        let dependencies = if has_inferred {
            let graph = error_inference::direct_named_call_graph(self);
            error_inference::reject_recursive_inferred_components(self, &graph);
            error_inference::inferred_dependencies(self, &graph)
        } else {
            vec![Vec::new(); self.uir.func_bodies.len()]
        };
        let failed: Vec<bool> = self
            .decl_state
            .iter()
            .map(|&state| state == DeclState::Failed)
            .collect();
        let mut scheduler = scheduler::DeclarationScheduler::new(&dependencies, &failed);
        while let Some(body) = scheduler.next() {
            let decl = self.declaration_ids[body as usize];
            self.resolve_decl(decl);
            scheduler.complete(body);
        }

        for index in 0..self.uir.func_bodies.len() {
            if self.decl_state[index] == DeclState::Unresolved {
                self.sink.emit(Diag::error(
                    self.uir.func_bodies[index].span,
                    DiagCode::InternalError,
                    format!(
                        "declaration scheduler stalled before analyzing '{}'",
                        self.pool.str(self.uir.func_bodies[index].name)
                    ),
                ));
                self.decl_state[index] = DeclState::Failed;
            }
        }
    }

    fn inferred_signatures(&self) -> Vec<InferredSignature> {
        self.uir
            .func_bodies
            .iter()
            .enumerate()
            .filter_map(|(index, body)| {
                self.inference[index]
                    .final_union
                    .map(|return_type| InferredSignature {
                        name: body.name,
                        return_type,
                    })
            })
            .collect()
    }

    /// Pull every resolved TIR out of the per-decl slots, in current-UIR
    /// source order. Decls that ended in `Failed` produce no
    /// `Tir`; their diagnostics already live in the sink.
    fn collect_results(self) -> Vec<Tir> {
        self.results.into_iter().flatten().collect()
    }

    #[inline]
    fn body_map(&self) -> &UirBodyMap {
        self.body_map
            .as_ref()
            .expect("validated Sema must have a body map")
    }

    /// Return the current-UIR body slot for a declaration registered by this Sema.
    #[inline]
    fn body_index(&self, decl: DeclId) -> usize {
        let index = decl.index();
        if self.declaration_ids.get(index) == Some(&decl) {
            index
        } else {
            self.body_index_fallback(decl)
        }
    }

    #[cold]
    #[inline(never)]
    fn body_index_fallback(&self, decl: DeclId) -> usize {
        self.context
            .declarations()
            .get(decl)
            .expect("Sema declaration must be registered")
            .uir_index
    }

    /// Ensure a callee's analysis state is consistent with its use
    /// from the currently-analyzing body. Today this only matters
    /// for cycle detection: callee *signatures* are eagerly
    /// resolved, so the check is "is this decl currently
    /// `InProgress`?" — which is the cycle sentinel.
    ///
    /// Returns `false` and emits a [`DiagCode::CycleInResolution`]
    /// diagnostic when a cycle is detected. The caller should fall
    /// back to the error type for whatever it was trying to
    /// compute.
    /// Reserved for the comptime / lazy-resolution era: the cycle
    /// sentinel for on-demand decl resolution. Production never calls
    /// it today (signatures resolve eagerly instead); it is exercised
    /// by the `require_decl_reports_cycle_when_in_progress` substrate
    /// test, which the comptime milestones will lean on. Private, so
    /// the lib-target dead-code analysis needs the allow.
    #[allow(dead_code)]
    fn require_decl(&mut self, callee: DeclId, span: Span, name: StringId) -> bool {
        match self.decl_state[self.body_index(callee)] {
            DeclState::Unresolved | DeclState::Resolved => true,
            DeclState::Failed => true, // cascade-suppress; the original error is already in the sink
            DeclState::InProgress => {
                self.sink.emit(Diag::error(
                    span,
                    DiagCode::CycleInResolution,
                    format!(
                        "cyclic dependency while resolving '{}'",
                        self.pool.str(name),
                    ),
                ));
                false
            }
        }
    }

    fn resolve_decl(&mut self, decl: DeclId) {
        let index = self.body_index(decl);
        match self.decl_state[index] {
            DeclState::Resolved | DeclState::Failed | DeclState::InProgress => return,
            DeclState::Unresolved => {}
        }
        #[cfg(test)]
        TEST_ACTIVE_BODY.with(|active| {
            assert_eq!(
                active.get(),
                None,
                "body analysis entered a second declaration"
            );
            active.set(Some(decl));
        });
        self.decl_state[index] = DeclState::InProgress;

        let body = &self.uir.func_bodies[index];
        let signature = self.signatures[&decl].clone();
        let tir = analyze_function(self, decl, body, &signature);

        self.results[index] = Some(tir);
        if error_inference::is_inferred(self, decl) {
            error_inference::finalize_inferred_function(self, decl);
        }
        self.decl_state[index] = DeclState::Resolved;
        #[cfg(test)]
        TEST_ACTIVE_BODY.with(|active| active.set(None));
    }
}

// ---------- Per-function analysis ----------

fn analyze_function(
    sema: &mut Sema<'_>,
    decl: DeclId,
    body: &FuncBody,
    signature: &FunctionSig,
) -> Tir {
    let mut scope = Scope::new();
    for (param, &param_type) in body.params.iter().zip(&signature.params) {
        // An `inout` parameter is mutable inside the callee body (like a
        // `mut` local); `move` and borrowed params are immutable.
        let is_mutable = param.mode == ParamMode::Inout;
        // Parameters are a user binding form like any other: the
        // '__ryo_' prefix and reserved builtins are rejected here too
        // (the error-typed binding keeps the body's uses suppressed).
        if check_reserved_name(
            sema,
            param.name,
            param.span,
            "is a reserved builtin and cannot be used as a parameter name",
        ) {
            scope.insert_binding(param.name, sema.pool.error_type(), is_mutable, false);
            continue;
        }
        // The default mode is `Borrow`: such a parameter is a borrowed
        // binding, which sema's match scrutinee analysis reads back via
        // `Scope::lookup_borrowed` (E0133's borrowed-scrutinee gate).
        scope.insert_binding(
            param.name,
            param_type,
            is_mutable,
            param.mode == ParamMode::Borrow,
        );
    }

    // W0002: warn on `move` annotations applied to Copy-typed
    // parameters. Copy types (int, float, bool) are duplicated on
    // every read regardless of the annotation, so `move` is
    // redundant noise. `move` on `str` (and other heap types) stays
    // silent — that's the whole reason the keyword exists. `strview`
    // views are excluded here: `move`/`inout` on a view is an
    // *error* (see below), and the warning would only cascade.
    for (param, &param_type) in body.params.iter().zip(&signature.params) {
        if param.mode == ParamMode::Move
            && sema.pool.is_copy(param_type)
            && !sema.pool.is_view(param_type)
        {
            let name = sema.pool.str(param.name).to_string();
            let ty_str = sema.pool.display(param_type).to_string();
            sema.warnings.push(Diag::warning(
                param.span,
                DiagCode::RedundantMove,
                format!(
                    "redundant 'move' on Copy-typed parameter '{}': {} values are copied on every read",
                    name, ty_str,
                ),
            ));
        }
    }

    // M8.4: `strview` is already a borrow, so `move` / `inout` on a view
    // parameter is meaningless (final spec §3.3 E2, §3.4); views
    // cannot be returned either (§3.3 E1 / Rule 5).
    for (param, &param_type) in body.params.iter().zip(&signature.params) {
        if param.mode != ParamMode::Borrow && sema.pool.is_view(param_type) {
            let mode_str = match param.mode {
                ParamMode::Move => "move",
                _ => "inout",
            };
            sema.sink.emit(Diag::error(
                param.span,
                DiagCode::TypeMismatch,
                format!(
                    "views cannot be `{}` parameters — `strview` is already a borrow",
                    mode_str,
                ),
            ));
        }
    }
    if sema.pool.is_view(signature.return_type) {
        sema.sink.emit(Diag::error(
            body.span,
            DiagCode::ReturnBorrowedValue,
            format!(
                "functions cannot return views (`{}`) — return an owned value instead (Rule 5)",
                sema.pool.display(signature.return_type)
            ),
        ));
    }

    let params: Vec<TirParam> = body
        .params
        .iter()
        .zip(&signature.params)
        .map(|(param, &ty)| TirParam {
            name: param.name,
            ty,
            mode: param.mode,
            span: param.span,
        })
        .collect();

    let body_index = sema.body_index(decl);
    let instruction_count = sema.body_map().instructions(body_index).len();
    let mut fcx = FuncCtx::new(
        decl,
        u32::try_from(body_index).expect("UIR body index out of range"),
        TirBuilder::new(body.name, params, signature.return_type, body.span)
            .with_decl(decl)
            .with_layout_context(sema.pool),
        instruction_count,
    );

    let uir = sema.uir;
    let body_stmts = uir.body_stmts(body);
    let mut stmt_refs: Vec<TirRef> = Vec::with_capacity(body_stmts.len());
    for stmt_ref in body_stmts {
        stmt_refs.push(analyze_stmt(sema, &mut fcx, &mut scope, stmt_ref));
    }
    scope.drain_dead_stores(sema);

    let tir = fcx.builder.finish(&stmt_refs);

    // Return-flow analysis: a non-void function must return (or
    // diverge via `never`) on every path through its body. Error-
    // typed returns already produced their diagnostic — skip to
    // avoid a cascade.
    if fcx.return_type != sema.pool.void()
        && !sema.pool.is_error(fcx.return_type)
        && !tir.body_definitely_returns(sema.pool)
    {
        sema.sink.emit(Diag::error(
            body.span,
            DiagCode::MissingReturn,
            format!(
                "missing return: function '{}' expects '{}' but can reach the end of its body without returning",
                sema.pool.str(body.name),
                sema.pool.display(fcx.return_type),
            ),
        ));
    }

    tir
}

/// Per-function emission state. Lives only for the duration of one
/// `analyze_function` call; the `inst_map` and `TirBuilder` arenas
/// are scoped to a single body.
pub(crate) struct FuncCtx {
    decl: DeclId,
    body: u32,
    builder: TirBuilder,
    inst_map: Vec<Option<TirRef>>,
    chain_local_errors: Vec<InstRef>,
    return_type: TypeId,
    loop_depth: u32,
    /// Compiler-temp bindings synthesized by astgen for nested
    /// destructuring and catch lowering. Kept OUT of the user scope:
    /// user code can never collide with or observe them, and `TempVar`
    /// reads resolve them before any scope lookup.
    destructure_temps: HashMap<StringId, TypeId>,
    inferred_call_dependencies: HashMap<TirRef, DeclId>,
}

impl FuncCtx {
    pub(crate) fn new(
        decl: DeclId,
        body: u32,
        builder: TirBuilder,
        instruction_count: usize,
    ) -> Self {
        let return_type = builder.return_type();
        Self {
            decl,
            body,
            builder,
            inst_map: vec![None; instruction_count],
            chain_local_errors: Vec::new(),
            return_type,
            loop_depth: 0,
            destructure_temps: HashMap::new(),
            inferred_call_dependencies: HashMap::new(),
        }
    }

    pub(crate) fn map_slot(&self, map: &UirBodyMap, inst: InstRef) -> usize {
        let slot = map
            .slot(inst)
            .expect("analyzed instruction must have a body owner");
        debug_assert_eq!(slot.body, self.body);
        slot.local as usize
    }

    fn get(&self, map: &UirBodyMap, inst: InstRef) -> Option<TirRef> {
        self.inst_map[self.map_slot(map, inst)]
    }

    fn set(&mut self, map: &UirBodyMap, inst: InstRef, value: TirRef) {
        let slot = self.map_slot(map, inst);
        self.inst_map[slot] = Some(value);
    }

    fn clear(&mut self, map: &UirBodyMap, inst: InstRef) {
        let slot = self.map_slot(map, inst);
        self.inst_map[slot] = None;
    }
}

#[cfg(test)]
mod declaration_identity_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_borrows;
#[cfg(test)]
mod tests_bytes;

#[cfg(test)]
mod tests_destructure;
#[cfg(test)]
mod tests_enums;
#[cfg(test)]
mod tests_error_duplicate_fields;
#[cfg(test)]
mod tests_error_inference;
#[cfg(test)]
mod tests_error_union_coercion;
#[cfg(test)]
mod tests_errors;
#[cfg(test)]
mod tests_match;
#[cfg(test)]
mod tests_match_arm_guards;
#[cfg(test)]
mod tests_match_exhaustiveness;
#[cfg(test)]
mod tests_match_typing;
#[cfg(test)]
mod tests_reserved;
#[cfg(test)]
mod tests_str_views;
#[cfg(test)]
mod tests_structs;
#[cfg(test)]
mod tests_type_literals;
