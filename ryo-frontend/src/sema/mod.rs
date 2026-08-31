//! Semantic analysis: type-check UIR and emit TIR.
//!
//! Sema consumes the flat [`Uir`] produced by `astgen` and emits one
//! [`Tir`] per function body, fully typed. Codegen consumes the
//! resulting `&[Tir]` directly.
//!
//! ## Phase 5 — worklist driver
//!
//! Earlier phases ran sema as a top-down recursion: collect every
//! signature, then walk every body in source order. That worked
//! because today's language has no construct (inferred return
//! types, comptime, generics) that makes one body's analysis
//! depend on another body's analysis. Phase 5 keeps the same
//! observable behaviour but reframes the driver as a worklist:
//!
//! - [`Sema`] owns a [`DeclState`] table indexed by [`DeclId`] (one
//!   id per function body in `uir.func_bodies`).
//! - The queue is seeded with every decl in source order. Popping
//!   transitions a decl from `Unresolved` → `InProgress` → either
//!   `Resolved` (TIR landed in the corresponding slot of
//!   `Sema::results`, which is parallel to `uir.func_bodies`) or
//!   `Failed`.
//! - Cycle detection is dormant for today's feature set — bodies
//!   only depend on callee *signatures*, which are resolved eagerly
//!   in a separate first pass — but [`Sema::require_decl`] hits
//!   `DeclState::InProgress` and emits a [`DiagCode::CycleInResolution`]
//!   diagnostic the moment future work (inferred return types,
//!   comptime evaluation) makes a body depend on another body
//!   mid-analysis. That's the prerequisite Phase 5 was for; the
//!   features ride on top.
//!
//! Tests at the bottom of this file include a
//! `cfg(any())`-gated block of comptime / generics smoke tests
//! — infrastructure-only stubs per pipeline_alignment.md §5.3
//! commit 5.
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

use ryo_core::diag::{Diag, DiagCode, DiagSink};
use ryo_core::tir::{ParamMode, Tir, TirBuilder, TirParam, TirRef};
use ryo_core::types::{InternPool, StringId, TypeId};
use ryo_core::uir::{FuncBody, InstRef, InstTag, Span, Uir};
use std::collections::{HashMap, VecDeque};
use std::path::Path;

mod builtins;
pub(crate) use builtins::*;
mod call;
pub(crate) use call::*;
mod expr;
pub(crate) use expr::*;
mod stmt;
pub(crate) use stmt::*;

// ---------- Decl table ----------

/// Index into `uir.func_bodies`. One [`DeclId`] per function the
/// driver may need to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeclId(u32);

impl DeclId {
    fn from_index(idx: usize) -> Self {
        DeclId(u32::try_from(idx).expect("DeclId index out of range"))
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Tri-state resolution status for a single declaration.
///
/// Mirrors Zig's `Module.semaDecl` state machine:
///
/// - `Unresolved` — never visited; lazy.
/// - `InProgress` — currently being analyzed; the cycle sentinel.
/// - `Resolved` — TIR landed in `Sema::results[decl.index()]`
///   (eager state for everything that follows). The slot index
///   is `DeclId.0` itself, so the variant carries no payload.
/// - `Failed` — analysis bailed out; downstream callers should
///   suppress cascade errors but not stack-overflow trying to
///   resolve again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclState {
    Unresolved,
    InProgress,
    Resolved,
    /// Reserved: a decl whose resolution gave up. Today every
    /// body still emits a well-formed TIR (with `Unreachable` slots
    /// in place of failed expressions) so `Failed` is unreachable
    /// from sources — §4.5 exit criterion. Comptime / inferred
    /// returns are the first features that can transition a decl
    /// into this state.
    #[allow(dead_code)]
    Failed,
}

struct FunctionSig {
    params: Vec<TypeId>,
    return_type: TypeId,
}

struct Binding {
    ty: TypeId,
    mutable: bool,
}

pub(crate) struct Scope<'a> {
    parent: Option<&'a Scope<'a>>,
    bindings: HashMap<StringId, Binding>,
}

impl<'a> Scope<'a> {
    fn new() -> Self {
        Scope {
            parent: None,
            bindings: HashMap::new(),
        }
    }

    fn insert_binding(&mut self, name: StringId, ty: TypeId, mutable: bool) {
        self.bindings.insert(name, Binding { ty, mutable });
    }

    fn contains_in_current(&self, name: StringId) -> bool {
        self.bindings.contains_key(&name)
    }

    fn lookup(&self, name: StringId) -> Option<TypeId> {
        self.bindings
            .get(&name)
            .map(|b| b.ty)
            .or_else(|| self.parent?.lookup(name))
    }

    fn lookup_full(&self, name: StringId) -> Option<(TypeId, bool)> {
        self.bindings
            .get(&name)
            .map(|b| (b.ty, b.mutable))
            .or_else(|| self.parent?.lookup_full(name))
    }
}

// ---------- Public entrypoint ----------

/// Analyze `uir` and emit one [`Tir`] per function body.
///
/// Thin wrapper around [`Sema::run`] kept as the stable façade
/// callers (the pipeline driver, tests) use. Equivalent to
/// `Sema::run(uir, pool, sink)` but spelled the way it always was.
pub fn analyze(
    uir: &Uir,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    source: &str,
    file_path: &Path,
) -> Vec<Tir> {
    Sema::run(uir, pool, sink, source, file_path)
}

// ---------- Sema driver ----------

/// Worklist-driven sema state. Lives only for the duration of one
/// `Sema::run` call.
pub struct Sema<'a> {
    uir: &'a Uir,
    pool: &'a mut InternPool,
    sink: &'a mut DiagSink,
    source: &'a str,
    file_path: &'a Path,
    /// Resolution status, parallel to `uir.func_bodies`.
    decl_state: Vec<DeclState>,
    /// Decls pending analysis.
    queue: VecDeque<DeclId>,
    /// Function name → decl id. Built once at the top of `run` and
    /// shared with `check_call`. A duplicate definition keeps the
    /// first one seen; sema doesn't currently report redefinitions
    /// (handled at a future astgen pass).
    name_to_decl: HashMap<StringId, DeclId>,
    /// Eagerly-resolved signatures, keyed by name. Out-of-order
    /// definitions and recursive / mutually-recursive calls
    /// type-check because callee signatures land here in a single
    /// pass before any body is analyzed.
    signatures: HashMap<StringId, FunctionSig>,
    /// Per-decl emitted TIR slot. Filled as decls transition to
    /// `Resolved`. Result extraction drains this in source order.
    results: Vec<Option<Tir>>,
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
fn collect_call_arg_refs(uir: &Uir) -> Vec<bool> {
    let mut set = vec![false; uir.instructions.len()];
    for (i, inst) in uir.instructions.iter().enumerate().skip(1) {
        let r = InstRef::from_raw(i as u32);
        match inst.tag {
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
    set
}

impl<'a> Sema<'a> {
    /// Drive sema to fixpoint and return one [`Tir`] per UIR
    /// function body, in source order.
    pub fn run(
        uir: &'a Uir,
        pool: &'a mut InternPool,
        sink: &'a mut DiagSink,
        source: &'a str,
        file_path: &'a Path,
    ) -> Vec<Tir> {
        let mut sema = Sema::new(uir, pool, sink, source, file_path);
        sema.resolve_signatures();
        sema.seed_worklist();
        sema.drive();
        sema.collect_results()
    }

    fn new(
        uir: &'a Uir,
        pool: &'a mut InternPool,
        sink: &'a mut DiagSink,
        source: &'a str,
        file_path: &'a Path,
    ) -> Self {
        let n = uir.func_bodies.len();
        let mut name_to_decl = HashMap::with_capacity(n);
        for (i, body) in uir.func_bodies.iter().enumerate() {
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
                    v.insert(DeclId::from_index(i));
                }
            }
        }
        let mut results = Vec::with_capacity(n);
        for _ in 0..n {
            results.push(None);
        }
        Sema {
            uir,
            pool,
            sink,
            source,
            file_path,
            decl_state: vec![DeclState::Unresolved; n],
            queue: VecDeque::with_capacity(n),
            name_to_decl,
            signatures: HashMap::with_capacity(n),
            results,
            call_arg_refs: collect_call_arg_refs(uir),
        }
    }

    /// Eagerly populate the signatures table.
    ///
    /// Today every signature is fully spelled in source — there is
    /// no inferred-return-type form — so this is a single linear
    /// scan. When inferred returns / generics arrive this becomes
    /// a per-decl "ensure signature resolved" call driven by the
    /// worklist; the rest of the driver doesn't need to know.
    fn resolve_signatures(&mut self) {
        for body in &self.uir.func_bodies {
            let name = self.pool.str(body.name);
            if name.starts_with("__ryo_") {
                self.sink.emit(Diag::error(
                    body.span,
                    DiagCode::ReservedIdentifier,
                    format!(
                        "identifiers starting with '__ryo_' are reserved for the compiler runtime: '{}'",
                        name,
                    ),
                ));
            }
            check_reserved_builtin(
                self,
                body.name,
                body.span,
                "is a reserved builtin and cannot be used as a function name",
            );
            // First definition wins, matching `name_to_decl` in
            // `Sema::new`: the duplicate body is still analyzed (and
            // its DuplicateDeclaration error already emitted), but it
            // must not overwrite the signature calls bind against.
            self.signatures.entry(body.name).or_insert(FunctionSig {
                params: body.params.iter().map(|p| p.ty).collect(),
                return_type: body.return_type,
            });
        }
    }

    /// Seed the worklist with every decl in source order.
    ///
    /// Source order is the stable visit order called for in the risk
    /// register ("Worklist driver introduces non-determinism in
    /// error order"). Diagnostics within one body remain ordered by
    /// the body's own walk; across bodies, errors come out in
    /// declaration order.
    fn seed_worklist(&mut self) {
        for i in 0..self.uir.func_bodies.len() {
            self.queue.push_back(DeclId::from_index(i));
        }
    }

    fn drive(&mut self) {
        while let Some(decl) = self.queue.pop_front() {
            self.resolve_decl(decl);
        }
    }

    /// Pull every resolved TIR out of the per-decl slots, in source
    /// (decl-id) order. Decls that ended in `Failed` produce no
    /// `Tir`; their diagnostics already live in the sink.
    fn collect_results(self) -> Vec<Tir> {
        self.results.into_iter().flatten().collect()
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
        match self.decl_state[callee.index()] {
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
        match self.decl_state[decl.index()] {
            DeclState::Resolved | DeclState::Failed | DeclState::InProgress => return,
            DeclState::Unresolved => {}
        }
        self.decl_state[decl.index()] = DeclState::InProgress;

        let body = &self.uir.func_bodies[decl.index()];
        let tir = analyze_function(self, body);

        self.results[decl.index()] = Some(tir);
        self.decl_state[decl.index()] = DeclState::Resolved;
    }
}

// ---------- Per-function analysis ----------

fn analyze_function(sema: &mut Sema<'_>, body: &FuncBody) -> Tir {
    let mut scope = Scope::new();
    for param in &body.params {
        // An `inout` parameter is mutable inside the callee body (like a
        // `mut` local); `move` and borrowed params are immutable.
        let is_mutable = param.mode == ParamMode::Inout;
        scope.insert_binding(param.name, param.ty, is_mutable);
    }

    // W0002: warn on `move` annotations applied to Copy-typed
    // parameters. Copy types (int, float, bool) are duplicated on
    // every read regardless of the annotation, so `move` is
    // redundant noise. `move` on `str` (and other heap types) stays
    // silent — that's the whole reason the keyword exists. `strview`
    // views are excluded here: `move`/`inout` on a view is an
    // *error* (see below), and the warning would only cascade.
    for param in &body.params {
        if param.mode == ParamMode::Move
            && sema.pool.is_copy(param.ty)
            && !sema.pool.is_view(param.ty)
        {
            let name = sema.pool.str(param.name).to_string();
            let ty_str = sema.pool.display(param.ty).to_string();
            sema.sink.emit(Diag::warning(
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
    for param in &body.params {
        if param.mode != ParamMode::Borrow && sema.pool.is_view(param.ty) {
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
    if sema.pool.is_view(body.return_type) {
        sema.sink.emit(Diag::error(
            body.span,
            DiagCode::ReturnBorrowedValue,
            "functions cannot return views (`strview`) — return an owned `str` instead (Rule 5)"
                .to_string(),
        ));
    }

    let params: Vec<TirParam> = body
        .params
        .iter()
        .map(|p| TirParam {
            name: p.name,
            ty: p.ty,
            mode: p.mode,
            span: p.span,
        })
        .collect();

    let mut fcx = FuncCtx {
        builder: TirBuilder::new(body.name, params, body.return_type, body.span),
        inst_map: vec![None; sema.uir.instructions.len()],
        return_type: body.return_type,
        loop_depth: 0,
    };

    let mut stmt_refs: Vec<TirRef> = Vec::with_capacity(sema.uir.body_stmts(body).len());
    for stmt_ref in sema.uir.body_stmts(body) {
        stmt_refs.push(analyze_stmt(sema, &mut fcx, &mut scope, stmt_ref));
    }

    let tir = fcx.builder.finish(&stmt_refs);

    // Return-flow analysis: a non-void function must return (or
    // diverge via `never`) on every path through its body. Error-
    // typed returns already produced their diagnostic — skip to
    // avoid a cascade.
    if fcx.return_type != sema.pool.void()
        && !sema.pool.is_error(fcx.return_type)
        && !tir.block_definitely_returns(&tir.body_stmts(), sema.pool)
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
    builder: TirBuilder,
    inst_map: Vec<Option<TirRef>>,
    return_type: TypeId,
    loop_depth: u32,
}

#[cfg(test)]
mod tests;
