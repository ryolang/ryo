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
use std::collections::{HashMap, HashSet, VecDeque};
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
    /// `&` in another.
    call_arg_refs: HashSet<InstRef>,
}

/// Every direct call-argument `InstRef` in the program. Scans
/// `Call` and `MethodCall` instructions (method-call args count; the
/// receiver does not — `(&x).len()` is not a valid borrow position).
fn collect_call_arg_refs(uir: &Uir) -> HashSet<InstRef> {
    let mut set = HashSet::new();
    for (i, inst) in uir.instructions.iter().enumerate().skip(1) {
        let r = InstRef::from_raw(i as u32);
        match inst.tag {
            InstTag::Call => set.extend(uir.call_view(r).args),
            InstTag::MethodCall => set.extend(uir.method_call_view(r).args),
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
mod tests {
    use super::*;
    use crate::astgen;
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use chumsky::Parser;
    use chumsky::input::Input;
    use chumsky::span::{SimpleSpan, Span as _};
    use ryo_core::tir::{Tir, TirData, TirTag};

    fn sp() -> Span {
        SimpleSpan::new((), 0..0)
    }

    type RunOk = (Vec<Tir>, InternPool);

    /// Lex + parse + astgen + sema. Returns either the typed TIR
    /// (alongside the pool) or the diagnostics that stopped one of
    /// those stages.
    fn run(input: &str) -> Result<RunOk, Vec<Diag>> {
        let mut pool = InternPool::new();
        let mut lex_sink = DiagSink::new();
        let tokens = lex(input, &mut pool, &mut lex_sink);
        assert!(
            !lex_sink.has_errors(),
            "lex errors: {:?}",
            lex_sink.into_diags()
        );
        let token_stream = tokens[..].split_token_span((0..input.len()).into());
        let mut ast = ryo_core::ast::Ast::new();
        program_parser()
            .parse_with_state(token_stream, &mut ast)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let uir = astgen::generate(&ast, &mut pool, &mut sink);
        if sink.has_errors() {
            return Err(sink.into_diags());
        }
        let tirs = analyze(&uir, &mut pool, &mut sink, input, Path::new("<test>"));
        if sink.has_errors() {
            return Err(sink.into_diags());
        }
        Ok((tirs, pool))
    }

    /// Variant that returns TIR even when sema reported errors —
    /// used to assert the "Unreachable + diag" invariant from §4.5.
    fn run_with_errors(input: &str) -> (Vec<Tir>, Vec<Diag>, InternPool) {
        let mut pool = InternPool::new();
        let mut lex_sink = DiagSink::new();
        let tokens = lex(input, &mut pool, &mut lex_sink);
        assert!(
            !lex_sink.has_errors(),
            "lex errors: {:?}",
            lex_sink.into_diags()
        );
        let token_stream = tokens[..].split_token_span((0..input.len()).into());
        let mut ast = ryo_core::ast::Ast::new();
        program_parser()
            .parse_with_state(token_stream, &mut ast)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let uir = astgen::generate(&ast, &mut pool, &mut sink);
        let tirs = analyze(&uir, &mut pool, &mut sink, input, Path::new("<test>"));
        (tirs, sink.into_diags(), pool)
    }

    fn first_msg(diags: &[Diag]) -> &str {
        &diags[0].message
    }

    fn any_code(diags: &[Diag], code: DiagCode) -> bool {
        diags.iter().any(|d| d.code == code)
    }

    fn count_code(diags: &[Diag], code: DiagCode) -> usize {
        diags.iter().filter(|d| d.code == code).count()
    }

    fn tir_named<'a>(tirs: &'a [Tir], pool: &InternPool, name: &str) -> &'a Tir {
        let id = pool.find_str(name).expect("name should be interned");
        tirs.iter()
            .find(|t| t.name == id)
            .unwrap_or_else(|| panic!("no function named {:?}", name))
    }

    fn stmt_at(tir: &Tir, i: usize) -> TirRef {
        tir.body_stmts()[i]
    }

    #[test]
    fn inout_param_is_assignable() {
        // Mutating an inout param by name must NOT raise ImmutableAssign.
        let (_tirs, diags, _pool) = run_with_errors("fn inc(inout x: int):\n\tx += 1\n");
        assert!(
            !any_code(&diags, DiagCode::ImmutableAssign),
            "inout param must be mutable; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_typechecks_to_inner_type() {
        // &c where c: int and callee expects inout int — no type error.
        let (_tirs, diags, _pool) = run_with_errors(
            "fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 0\n\tinc(&c)\n",
        );
        assert!(
            !any_code(&diags, DiagCode::TypeMismatch),
            "&c into inout int must typecheck; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_without_inout_rejected() {
        // `&c` passed to a plain (non-inout) `int` parameter.
        let (_tirs, diags, _pool) = run_with_errors(
            "fn f(x: int):\n\tprint(int_to_str(x))\nfn main():\n\tmut c = 0\n\tf(&c)\n",
        );
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "&c into a non-inout param must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn inout_without_borrow_rejected() {
        // plain `c` passed to an `inout int` parameter.
        let (_tirs, diags, _pool) =
            run_with_errors("fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 0\n\tinc(c)\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "inout param without & must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_of_immutable_rejected() {
        // `&c` where c is not declared `mut`.
        let (_tirs, diags, _pool) =
            run_with_errors("fn inc(inout x: int):\n\tx += 1\nfn main():\n\tc = 0\n\tinc(&c)\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "& of an immutable binding must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_of_mut_local_ok() {
        // `&c` where c is `mut` — the happy path.
        let (_tirs, diags, _pool) = run_with_errors(
            "fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 0\n\tinc(&c)\n",
        );
        assert!(
            !any_code(&diags, DiagCode::BorrowMismatch),
            "& of a mut local must be allowed; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_outside_call_rejected() {
        // `x = &c` — a `&` that is not a call argument is not a
        // mutation marker for anything; it must be an error, not a
        // silent no-op.
        let (_tirs, diags, _pool) = run_with_errors("fn main():\n\tmut c = 5\n\tx = &c\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "& outside call-argument position must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_nested_in_binop_rejected() {
        // `inc(1 + &c)` — the `&` is buried inside an expression;
        // it is not the call argument itself, so it is rejected here (and
        // check_call separately reports the missing `&` on the inout arg).
        let (_tirs, diags, _pool) = run_with_errors(
            "fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 1\n\tinc(1 + &c)\n",
        );
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "& nested inside a larger expression must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_as_inout_arg_still_ok() {
        // Regression guard: a direct call argument keeps working.
        let (_tirs, diags, _pool) = run_with_errors(
            "fn inc(inout x: int):\n\tx += 1\nfn main():\n\tmut c = 0\n\tinc(&c)\n",
        );
        assert!(
            !any_code(&diags, DiagCode::BorrowMismatch),
            "& as a direct inout call argument must stay valid; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_of_borrowed_param_rejected() {
        // Review coverage: `&` on a plain (borrowed) param — immutable.
        let (_tirs, diags, _pool) =
            run_with_errors("fn f(s: str):\n\tstr_push(&s, \"x\")\nfn main():\n\tf(\"hi\")\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "& of a borrowed param must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_of_move_param_rejected() {
        // Review coverage: `&` on a `move` param — immutable binding.
        let (_tirs, diags, _pool) =
            run_with_errors("fn f(move s: str):\n\tstr_push(&s, \"x\")\nfn main():\n\tf(\"hi\")\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "& of a move param must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn borrow_of_for_loop_var_rejected() {
        // Review coverage: `&` on a for-loop variable — immutable,
        // block-scoped.
        let (_tirs, diags, _pool) = run_with_errors(
            "fn inc(inout x: int):\n\tx += 1\nfn main():\n\tfor i in range(0, 3):\n\t\tinc(&i)\n",
        );
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "& of a for-loop variable must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn str_push_immutable_first_arg_rejected() {
        // Review coverage: str_push replays the lvalue check (builtins
        // bypass check_call) — `&s` with a non-`mut` s must be E0033.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts = \"hi\"\n\tstr_push(&s, \"x\")\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "str_push(&immutable, ..) must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn print_with_borrow_arg_rejected() {
        // `print(&s)` — print's parameter is not `inout`, so the `&`
        // marker must be rejected, not silently discarded.
        let (_tirs, diags, _pool) = run_with_errors("fn main():\n\tmut s = \"hi\"\n\tprint(&s)\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "print(&s) must be rejected (param is not inout); got {:?}",
            diags
        );
    }

    #[test]
    fn conversion_builtin_with_borrow_arg_rejected() {
        // `int_to_str(&c)` — conversion builtins are not `inout` either.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\tmut c = 1\n\tprint(int_to_str(&c))\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "int_to_str(&c) must be rejected; got {:?}",
            diags
        );
    }

    #[test]
    fn str_push_borrow_suffix_rejected() {
        // `str_push(&s, &x)` — the suffix parameter is Borrow, not
        // `inout`, so `&` on it must be rejected through the same check.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\tmut s = \"hi\"\n\tmut x = \"yo\"\n\tstr_push(&s, &x)\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "str_push's suffix must reject `&`; got {:?}",
            diags
        );
    }

    #[test]
    fn str_push_valid_still_ok() {
        // Regression guard: the valid form stays clean.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\tmut s = \"hi\"\n\tstr_push(&s, \"x\")\n");
        assert!(
            !any_code(&diags, DiagCode::BorrowMismatch),
            "str_push(&s, \"x\") must stay valid; got {:?}",
            diags
        );
    }

    #[test]
    fn fills_types_on_flat_integer_var() {
        let (tirs, pool) = run("x = 42").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        // Implicit-main is now void; the var-decl itself is still int.
        assert_eq!(main.return_type, pool.void());
        let var_decl = stmt_at(main, 0);
        assert_eq!(main.inst(var_decl).ty, pool.int());
    }

    #[test]
    fn infers_string_literal_type() {
        let (tirs, pool) = run("x = \"hello\"").unwrap();
        let main = &tirs[0];
        let v = main.var_decl_view(stmt_at(main, 0));
        assert_eq!(main.inst(v.initializer).ty, pool.str_());
    }

    #[test]
    fn typed_variable_annotation_honored() {
        let (tirs, pool) = run("x: int = 42").unwrap();
        let main = &tirs[0];
        assert_eq!(main.inst(stmt_at(main, 0)).ty, pool.int());
    }

    #[test]
    fn bool_annotation_resolves() {
        let (tirs, pool) = run("x: bool = true").unwrap();
        let main = &tirs[0];
        assert_eq!(main.inst(stmt_at(main, 0)).ty, pool.bool_());
    }

    #[test]
    fn variable_reference_type_resolved() {
        let (tirs, pool) = run("x = 42\ny = x").unwrap();
        let main = &tirs[0];
        let stmts = main.body_stmts();
        let v = main.var_decl_view(stmts[1]);
        assert_eq!(main.inst(stmts[1]).ty, pool.int());
        assert_eq!(main.inst(v.initializer).ty, pool.int());
        assert!(matches!(main.inst(v.initializer).tag, TirTag::Var));
    }

    #[test]
    fn undefined_variable_rejected() {
        let diags = run("x = y").unwrap_err();
        assert!(any_code(&diags, DiagCode::UndefinedVariable));
    }

    #[test]
    fn undefined_function_rejected() {
        let diags = run("x = not_a_fn()").unwrap_err();
        assert!(any_code(&diags, DiagCode::UndefinedFunction));
    }

    #[test]
    fn sema_continues_past_first_error_and_collects_multiple() {
        let diags = run("a = x\nb = y\n").unwrap_err();
        let undefs = diags
            .iter()
            .filter(|d| d.code == DiagCode::UndefinedVariable)
            .count();
        assert_eq!(undefs, 2, "got: {:#?}", diags);
    }

    #[test]
    fn unknown_type_does_not_cascade() {
        let diags = run("x: nope = 1").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnknownType));
        assert!(
            !any_code(&diags, DiagCode::TypeMismatch),
            "unexpected cascade: {:#?}",
            diags
        );
    }

    #[test]
    fn function_call_return_type_resolved() {
        let code = "fn double(x: int) -> int:\n\treturn x * 2\n\nfn main():\n\tn = double(3)\n";
        let (tirs, pool) = run(code).unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let var = stmt_at(main, 0);
        let view = main.var_decl_view(var);
        let init = main.inst(view.initializer);
        assert_eq!(init.ty, pool.int());
        assert!(matches!(init.tag, TirTag::Call));
    }

    #[test]
    fn void_function_signature() {
        let (tirs, pool) = run("fn greet():\n\tprint(\"hi\")\n\nfn main():\n\tgreet()\n").unwrap();
        let greet = tir_named(&tirs, &pool, "greet");
        assert_eq!(greet.return_type, pool.void());
        let main = tir_named(&tirs, &pool, "main");
        assert_eq!(main.return_type, pool.void());
    }

    #[test]
    fn print_call_has_void_type() {
        // `print(...)` as a bare expression statement is the
        // canonical M8a form. The Call instruction itself is
        // typed `void` since print returns no value.
        let (tirs, pool) = run("print(\"Hello\\n\")").unwrap();
        let main = &tirs[0];
        let stmt_ref = stmt_at(main, 0);
        let stmt = main.inst(stmt_ref);
        assert!(matches!(stmt.tag, TirTag::ExprStmt));
        let call_ref = match stmt.data {
            TirData::UnOp(o) => o,
            other => panic!("expected ExprStmt UnOp, got {:?}", other),
        };
        let call = main.inst(call_ref);
        assert!(matches!(call.tag, TirTag::Call));
        assert_eq!(call.ty, pool.void());
    }

    #[test]
    fn binding_to_void_value_rejected() {
        // The pre-M8a `_ = print(...)` / `msg = print(...)`
        // workarounds are now compile errors: a void value can't
        // be bound to a name.
        let diags = run("msg = print(\"Hello\")").unwrap_err();
        assert!(any_code(&diags, DiagCode::VoidValueInExpression));
    }

    #[test]
    fn int_equality_yields_bool() {
        let (tirs, pool) = run("x = 1 == 2").unwrap();
        let main = &tirs[0];
        let v = main.var_decl_view(stmt_at(main, 0));
        assert_eq!(main.inst(v.initializer).ty, pool.bool_());
        assert!(matches!(main.inst(v.initializer).tag, TirTag::ICmpEq));
    }

    #[test]
    fn mixed_type_equality_rejected() {
        let diags = run("x = 1 == true").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
        assert!(first_msg(&diags).contains("type mismatch in '=='"));
    }

    #[test]
    fn string_equality_accepted() {
        let (tirs, _pool) = run("x = \"a\" == \"b\"").unwrap();
        // The equality produces a bool-typed StrCmpEq instruction.
        let body = &tirs[0];
        let has_str_eq = body.instructions.iter().any(|i| i.tag == TirTag::StrCmpEq);
        assert!(has_str_eq, "expected StrCmpEq in TIR");
    }

    #[test]
    fn bool_arithmetic_rejected() {
        let diags = run("x = true + 1").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn bool_arithmetic_same_type_rejected_as_unsupported_op() {
        let diags = run("x = true + false").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnsupportedOperator));
        assert!(!any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn bool_literal_true_type() {
        let (tirs, pool) = run("x = true").unwrap();
        let main = &tirs[0];
        let v = main.var_decl_view(stmt_at(main, 0));
        assert_eq!(main.inst(v.initializer).ty, pool.bool_());
        assert!(matches!(main.inst(v.initializer).data, TirData::Bool(true)));
    }

    #[test]
    fn print_arity_rejected_in_sema() {
        let diags = run("print(\"a\", \"b\")").unwrap_err();
        assert!(any_code(&diags, DiagCode::ArityMismatch));
    }

    #[test]
    fn return_type_mismatch_rejected() {
        let diags = run("fn answer() -> int:\n\treturn \"hello\"\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn call_arity_mismatch_rejected() {
        let code =
            "fn add(a: int, b: int) -> int:\n\treturn a + b\n\nfn main():\n\tx = add(1, 2, 3)\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::ArityMismatch));
    }

    #[test]
    fn call_argument_type_mismatch_rejected() {
        let code = "fn f(a: int) -> int:\n\treturn a\n\nfn main():\n\tx = f(true)\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn main_with_return_type_rejected() {
        let diags = run("fn main() -> int:\n\treturn 0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::MainSignature));
    }

    #[test]
    fn main_with_params_rejected() {
        let diags = run("fn main(x: int):\n\tprint(\"hi\")\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::MainSignature));
    }

    #[test]
    fn return_value_in_void_function_rejected() {
        let diags = run("fn greet():\n\treturn 1\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn bare_return_in_void_function_accepted() {
        let (_tirs, _pool) = run("fn greet():\n\treturn\n").unwrap();
    }

    #[test]
    fn void_function_without_explicit_return_accepted() {
        let (_tirs, _pool) = run("fn greet():\n\tprint(\"hi\")\n").unwrap();
    }

    #[test]
    fn nonvoid_falloff_reports_missing_return() {
        let diags = run("fn f() -> int:\n\tx = 1\n").unwrap_err();
        assert_eq!(
            count_code(&diags, DiagCode::MissingReturn),
            1,
            "expected exactly one MissingReturn; got {:?}",
            diags
        );
    }

    #[test]
    fn exhaustive_if_else_returns_accepted() {
        let code = "fn f(x: int) -> int:\n\tif x > 0:\n\t\treturn 1\n\telse:\n\t\treturn 2\n";
        let (_tirs, _pool) = run(code).unwrap();
    }

    #[test]
    fn exhaustive_elif_chain_returns_accepted() {
        let code = "fn f(x: int) -> int:\n\tif x > 0:\n\t\treturn 1\n\telif x < 0:\n\t\treturn 0 - 1\n\telse:\n\t\treturn 0\n";
        let (_tirs, _pool) = run(code).unwrap();
    }

    #[test]
    fn if_without_else_still_reports_missing_return() {
        let code = "fn f(x: int) -> int:\n\tif x > 0:\n\t\treturn 1\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::MissingReturn));
    }

    #[test]
    fn return_inside_while_does_not_count() {
        // Conservative: a loop body can execute zero times, so a
        // return inside `while` never satisfies return-flow.
        let code = "fn f(x: int) -> int:\n\twhile x > 0:\n\t\treturn 1\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::MissingReturn));
    }

    #[test]
    fn trailing_return_after_if_accepted() {
        let code = "fn f(x: int) -> int:\n\tif x > 0:\n\t\treturn 1\n\treturn 0\n";
        let (_tirs, _pool) = run(code).unwrap();
    }

    #[test]
    fn panic_tail_counts_as_diverging() {
        // `panic` returns `never` — a function ending in it cannot
        // fall through, so no MissingReturn.
        let code = "fn f() -> int:\n\tpanic(\"boom\")\n";
        let (_tirs, _pool) = run(code).unwrap();
    }

    #[test]
    fn never_var_decl_rejected() {
        // Binding a `never` value is an error — `panic` diverges and
        // produces no value to bind. No MissingReturn cascade: the
        // user's intent was to diverge.
        let diags = run("fn f() -> int:\n\tx = panic(\"boom\")\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::VoidValueInExpression));
        assert!(
            !any_code(&diags, DiagCode::MissingReturn),
            "no cascading MissingReturn; got {:?}",
            diags
        );
    }

    #[test]
    fn never_assign_rejected() {
        let diags = run("fn f() -> int:\n\tmut x = 1\n\tx = panic(\"boom\")\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::VoidValueInExpression));
        assert!(
            !any_code(&diags, DiagCode::MissingReturn),
            "no cascading MissingReturn; got {:?}",
            diags
        );
    }

    #[test]
    fn never_compound_assign_rejected() {
        let diags = run("fn f() -> int:\n\tmut x = 1\n\tx += panic(\"boom\")\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::VoidValueInExpression));
        assert!(
            !any_code(&diags, DiagCode::MissingReturn),
            "no cascading MissingReturn; got {:?}",
            diags
        );
    }

    #[test]
    fn return_panic_rejected() {
        // `return <never>` is rejected like every other value
        // position: `panic` may only appear as a bare statement.
        let diags = run("fn f() -> int:\n\treturn panic(\"boom\")\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::VoidValueInExpression));
    }

    #[test]
    fn never_binop_operand_rejected() {
        let diags = run("fn f() -> int:\n\treturn 1 + panic(\"boom\")\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::VoidValueInExpression));
    }

    #[test]
    fn never_call_arg_rejected() {
        let code = "fn g(x: int):\n\tprint(\"{x}\")\n\nfn main():\n\tg(panic(\"boom\"))\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::VoidValueInExpression));
    }

    #[test]
    fn genuine_missing_return_reported_alongside_other_errors() {
        // An unrelated error doesn't mask return-flow: this function
        // genuinely lacks a return, so both diags fire.
        let (_tirs, diags, _pool) = run_with_errors("fn f() -> int:\n\tx = missing_var + 1\n");
        assert!(any_code(&diags, DiagCode::UndefinedVariable));
        assert!(any_code(&diags, DiagCode::MissingReturn));
    }

    #[test]
    fn stmt_error_sentinel_suppresses_missing_return() {
        // A statement that failed analysis lowers to the Unreachable
        // sentinel; its structure is unknown, so return-flow must not
        // cascade a MissingReturn on top of the real error.
        let (_tirs, diags, _pool) = run_with_errors("fn f() -> int:\n\tx += 1\n");
        assert!(any_code(&diags, DiagCode::UndefinedAssignTarget));
        assert!(
            !any_code(&diags, DiagCode::MissingReturn),
            "no cascading MissingReturn; got {:?}",
            diags
        );
    }

    #[test]
    fn vardecl_annotation_initializer_mismatch_rejected() {
        let diags = run("x: int = \"hello\"").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn neg_on_bool_rejected() {
        let diags = run("x = -true").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnsupportedOperator));
    }

    #[test]
    fn nested_expression_types_all_filled() {
        let (tirs, _) = run("x = (1 + 2) * -3").unwrap();
        for tir in &tirs {
            for idx in 1..tir.instructions.len() {
                let inst = &tir.instructions[idx];
                // Every emitted instruction has *some* TypeId. The
                // error sentinel is allowed only at Unreachable; an
                // error-free run shouldn't have any.
                assert!(!matches!(inst.tag, TirTag::Unreachable));
            }
        }
    }

    /// §4.5 exit criterion: a UIR with a deliberate type error
    /// produces (a) TIR for the rest of the function, (b) an
    /// `Unreachable` instruction at the failure point, (c) exactly
    /// one diagnostic in the sink.
    #[test]
    fn type_error_emits_unreachable_and_keeps_going() {
        // `-true` is the sole error; the print after it should
        // still appear in the function's TIR body.
        let src = "fn main():\n\tx = -true\n\tprint(\"after\")\n";
        let (tirs, diags, _pool) = run_with_errors(src);
        assert_eq!(diags.len(), 1, "got: {:#?}", diags);
        assert_eq!(diags[0].code, DiagCode::UnsupportedOperator);

        let main = &tirs[0];
        // Function body still has both statements.
        assert_eq!(main.body_stmts().len(), 2);

        // Find the Unreachable inserted in place of the failed
        // initializer.
        let mut saw_unreachable = false;
        for idx in 1..main.instructions.len() {
            if matches!(main.instructions[idx].tag, TirTag::Unreachable) {
                saw_unreachable = true;
                break;
            }
        }
        assert!(saw_unreachable, "expected an Unreachable instruction");
    }

    // ---------- Phase 5: worklist driver ----------

    /// §5.4 exit criterion: source order doesn't matter for type
    /// resolution. A caller defined *before* its callee still
    /// type-checks, because signatures are eagerly resolved before
    /// any body is walked.
    #[test]
    fn forward_reference_resolves() {
        let code = "fn main():\n\tn = helper(2)\n\nfn helper(x: int) -> int:\n\treturn x + 1\n";
        let (tirs, pool) = run(code).unwrap();
        // Both decls produced TIR.
        assert_eq!(tirs.len(), 2);
        // Source order is preserved in the result vec.
        assert_eq!(pool.str(tirs[0].name), "main");
        assert_eq!(pool.str(tirs[1].name), "helper");
    }

    /// §5.4: mutual recursion with explicit return types is *not* a
    /// cycle — bodies depend only on callee signatures, which
    /// resolve eagerly. The previous recursive driver also handled
    /// this; the worklist version mustn't regress it (and mustn't
    /// stack-overflow trying).
    #[test]
    fn mutual_recursion_does_not_trigger_cycle() {
        let code = "fn a() -> int:\n\treturn b()\n\nfn b() -> int:\n\treturn a()\n\nfn main():\n\tx = a()\n";
        let (tirs, _diags, _pool) = run_with_errors(code);
        assert_eq!(tirs.len(), 3);
    }

    /// Direct unit test of the cycle sentinel: bypass the public
    /// driver, drive `require_decl` against an `InProgress` slot
    /// directly. This is the substrate the comptime / inferred-
    /// return-type milestones will lean on; today the path is
    /// unreachable through source code.
    #[test]
    fn require_decl_reports_cycle_when_in_progress() {
        // Build a minimal valid UIR with one function so Sema::new
        // is happy.
        let mut pool = InternPool::new();
        let mut sink = DiagSink::new();
        let main_id = pool.intern_str("main");

        let mut b = ryo_core::uir::UirBuilder::new();
        let zero = b.int_literal(0, sp());
        let ret = b.unary(InstTag::Return, zero, sp());
        b.add_function(main_id, vec![], pool.int(), &[ret], sp());
        let uir = b.finish();

        let mut sema = Sema::new(&uir, &mut pool, &mut sink, "test", Path::new("<test>"));
        sema.resolve_signatures();
        // Pretend we're mid-resolving `main`.
        sema.decl_state[0] = DeclState::InProgress;
        let ok = sema.require_decl(DeclId::from_index(0), sp(), main_id);
        assert!(!ok, "require_decl must reject an InProgress decl");
        let diags = sink.into_diags();
        assert!(
            diags.iter().any(|d| d.code == DiagCode::CycleInResolution),
            "got: {:#?}",
            diags
        );
    }

    /// Source-order stability: when sema emits errors from multiple
    /// functions, the diagnostic order matches the order the decls
    /// appear in source. The risk register flagged worklist
    /// non-determinism; this test pins the seed-in-source-order
    /// invariant.
    ///
    /// Note on what the assertion below actually proves:
    /// `run_with_errors` returns `sink.into_diags()`, which yields
    /// diagnostics in **emission order** — the order in which
    /// `DiagSink::emit` was called, not a sorted-by-span order.
    /// Because `first` and `second` each emit exactly one
    /// `UndefinedVariable` diag, the assertion
    /// `undef[0].span.start < undef[1].span.start` is a proxy for
    /// "`first` was resolved before `second`": the worklist popped
    /// decls in source order, so the diag from the body at the
    /// lower span fired first. If `DiagSink::into_diags` ever
    /// starts sorting by span, this test would silently keep
    /// passing while no longer testing resolution order — update
    /// the assertion to inspect emission-side state if that
    /// changes. (Pipeline-level rendering does sort by span via
    /// `render_diags`, but that's a separate path; sema's tests
    /// observe the unsorted sink directly.)
    #[test]
    fn diagnostics_ordered_by_decl_then_position() {
        let code = "fn first() -> int:\n\treturn missing_a\n\nfn second() -> int:\n\treturn missing_b\n\nfn main():\n\tprint(\"go\")\n";
        let (_tirs, diags, _pool) = run_with_errors(code);
        let undef: Vec<_> = diags
            .iter()
            .filter(|d| d.code == DiagCode::UndefinedVariable)
            .collect();
        assert_eq!(undef.len(), 2, "got: {:#?}", diags);
        // `first` is at a lower span start than `second`, so its
        // diagnostic must come out first.
        assert!(undef[0].span.start < undef[1].span.start);
    }

    // ---------- Substrate stubs for comptime / generics ----------
    //
    // Per pipeline_alignment.md §5.3 commit 5, these are
    // `cfg(any())`-gated ("never compiled") infrastructure tests.
    // They exist as named hooks so the comptime / generics
    // milestones land as concrete test bodies — no follow-on
    // refactor needed to make space for them. `cfg(any())` is the
    // idiomatic Rust spelling of the doc's `cfg(unimplemented)`.
    #[cfg(any())]
    mod future {
        use super::*;

        #[test]
        fn comptime_block_evaluates_to_value() {
            // `comptime { 1 + 2 }` → Sema must evaluate, not emit
            // TIR; result interned as a value, substituted at use.
            unimplemented!("comptime — requires Phase 5 evaluator on top of the worklist");
        }

        #[test]
        fn generic_call_triggers_monomorphization() {
            // `fn id[T](x: T) -> T: return x` + `id(42)` + `id(true)`
            // → two TIRs from one UIR body, keyed on (DeclId,
            // [TypeId]).
            unimplemented!("generics — requires monomorphization on top of the worklist");
        }

        #[test]
        fn comptime_cycle_reports_diagnostic() {
            // A comptime block whose evaluation requires its own
            // result must emit `DiagCode::CycleInResolution`.
            unimplemented!("comptime cycle — exercises require_decl through a body-body edge");
        }
    }

    // ---- M7: float / ordering / modulo ----

    #[test]
    fn sema_float_literal_has_float_type() {
        let (tirs, pool) = run("x = 1.5").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let v = main.var_decl_view(stmt_at(main, 0));
        let init = main.inst(v.initializer);
        assert!(matches!(init.tag, TirTag::FloatConst));
        assert_eq!(init.ty, pool.float());
    }

    #[test]
    fn sema_float_arithmetic_lowers_to_fadd() {
        let (tirs, pool) = run("x: float = 1.0\ny = x + 2.5").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let v = main.var_decl_view(stmt_at(main, 1));
        let init = main.inst(v.initializer);
        assert!(matches!(init.tag, TirTag::FAdd));
        assert_eq!(init.ty, pool.float());
    }

    #[test]
    fn sema_int_division_and_modulo_stay_int() {
        let (tirs, pool) = run("a = 10\nb = 3\nq = a / b\nr = a % b").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let q = main.var_decl_view(stmt_at(main, 2));
        let r = main.var_decl_view(stmt_at(main, 3));
        let q_init = main.inst(q.initializer);
        let r_init = main.inst(r.initializer);
        assert!(matches!(q_init.tag, TirTag::ISDiv));
        assert!(matches!(r_init.tag, TirTag::IMod));
        assert_eq!(q_init.ty, pool.int());
        assert_eq!(r_init.ty, pool.int());
    }

    #[test]
    fn sema_ordering_returns_bool() {
        let (tirs, pool) = run("x = 1 < 2").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let v = main.var_decl_view(stmt_at(main, 0));
        let init = main.inst(v.initializer);
        assert!(matches!(init.tag, TirTag::ICmpLt));
        assert_eq!(init.ty, pool.bool_());
    }

    #[test]
    fn sema_float_ordering_lowers_to_fcmp() {
        let (tirs, pool) = run("x = 1.0 <= 2.0").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let v = main.var_decl_view(stmt_at(main, 0));
        assert!(matches!(main.inst(v.initializer).tag, TirTag::FCmpLe));
    }

    #[test]
    fn sema_mixed_int_float_arithmetic_errors() {
        let (_tirs, diags, _pool) = run_with_errors("x = 1 + 2.0");
        assert!(
            diags.iter().any(|d| d.code == DiagCode::TypeMismatch
                && d.message.contains("'+'")
                && d.message.contains("int")
                && d.message.contains("float")),
            "diags: {:?}",
            diags
        );
    }

    #[test]
    fn sema_mixed_int_float_ordering_errors() {
        let (_tirs, diags, _pool) = run_with_errors("x = 1 < 2.0");
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn sema_modulo_on_float_errors() {
        let (_tirs, diags, _pool) = run_with_errors("x = 1.0 % 2.0");
        assert!(
            diags.iter().any(|d| d.code == DiagCode::UnsupportedOperator
                && d.message.contains("'%'")
                && d.message.contains("float")),
            "diags: {:?}",
            diags
        );
    }

    #[test]
    fn sema_ordering_on_bool_errors() {
        let (_tirs, diags, _pool) = run_with_errors("x = true < false");
        assert!(
            diags.iter().any(|d| d.code == DiagCode::UnsupportedOperator
                && d.message.contains("'<'")
                && d.message.contains("bool")),
            "diags: {:?}",
            diags
        );
    }

    // ---- M8b: assert builtin validation ----

    #[test]
    fn assert_true_literal_ok() {
        run("fn main():\n\tassert(true, \"ok\")\n").unwrap();
    }

    #[test]
    fn assert_expression_condition_ok() {
        run("fn main():\n\tassert(1 == 1, \"math works\")\n").unwrap();
    }

    #[test]
    fn assert_wrong_arity_zero_args() {
        let err = run("fn main():\n\tassert()\n").unwrap_err();
        assert!(any_code(&err, DiagCode::ArityMismatch));
    }

    #[test]
    fn assert_wrong_arity_one_arg() {
        let err = run("fn main():\n\tassert(true)\n").unwrap_err();
        assert!(any_code(&err, DiagCode::ArityMismatch));
    }

    #[test]
    fn assert_wrong_arity_three_args() {
        let err = run("fn main():\n\tassert(true, \"msg\", \"extra\")\n").unwrap_err();
        assert!(any_code(&err, DiagCode::ArityMismatch));
    }

    #[test]
    fn assert_condition_must_be_bool() {
        let err = run("fn main():\n\tassert(42, \"not bool\")\n").unwrap_err();
        assert!(any_code(&err, DiagCode::TypeMismatch));
    }

    #[test]
    fn assert_message_must_be_string_literal() {
        let err = run("fn main():\n\tassert(true, 42)\n").unwrap_err();
        assert!(any_code(&err, DiagCode::BuiltinArgKind));
    }

    // ---- M8c: panic builtin validation ----

    #[test]
    fn panic_wrong_arity_zero_args() {
        let err = run("fn main():\n\tpanic()\n").unwrap_err();
        assert!(any_code(&err, DiagCode::ArityMismatch));
    }

    #[test]
    fn panic_wrong_arity_two_args() {
        let err = run("fn main():\n\tpanic(\"a\", \"b\")\n").unwrap_err();
        assert!(any_code(&err, DiagCode::ArityMismatch));
    }

    #[test]
    fn panic_message_must_be_string_literal() {
        let err = run("fn main():\n\tpanic(42)\n").unwrap_err();
        assert!(any_code(&err, DiagCode::BuiltinArgKind));
    }

    #[test]
    fn panic_string_literal_ok() {
        run("fn main():\n\tpanic(\"oops\")\n").unwrap();
    }

    #[test]
    fn panic_call_has_never_type() {
        let (tirs, pool) = run("fn main():\n\tpanic(\"boom\")\n").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let call_ref = stmt_at(main, 0);
        let inner = match main.inst(call_ref).data {
            TirData::UnOp(operand) => operand,
            _ => panic!("expected ExprStmt wrapping the call"),
        };
        assert_eq!(main.inst(inner).ty, pool.never());
    }

    #[test]
    fn panic_lowers_to_ryo_panic_call() {
        let (tirs, pool) = run("fn main():\n\tpanic(\"boom\")\n").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        // Statement 0 is ExprStmt wrapping the call
        let stmt = stmt_at(main, 0);
        let call_ref = match main.inst(stmt).data {
            TirData::UnOp(operand) => operand,
            _ => panic!("expected ExprStmt"),
        };
        let call = main.inst(call_ref);
        assert!(matches!(call.tag, TirTag::Call));
        let view = main.call_view(call_ref);
        assert_eq!(pool.str(view.name), "__ryo_panic");
        // Two args: pointer (str) and length (int)
        assert_eq!(view.args.len(), 2);
    }

    #[test]
    fn assert_desugars_to_if_with_panic() {
        let (tirs, pool) = run("fn main():\n\tassert(false, \"oops\")\n").unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let stmt = stmt_at(main, 0);
        let inner_ref = match main.inst(stmt).data {
            TirData::UnOp(operand) => operand,
            _ => panic!("expected ExprStmt"),
        };
        // assert desugars to IfStmt
        assert!(
            matches!(main.inst(inner_ref).tag, TirTag::IfStmt),
            "assert should desugar to IfStmt, got {:?}",
            main.inst(inner_ref).tag
        );
    }

    #[test]
    fn reserved_ryo_prefix_rejected() {
        let errors = run_with_errors("fn __ryo_hack():\n\tprint(\"nope\")\n").1;
        assert!(any_code(&errors, DiagCode::ReservedIdentifier));
    }

    // ---- M8c1: mutability + assignment ----

    #[test]
    fn reassign_mut_variable() {
        let result = run("fn main():\n\tmut x = 1\n\tx = 2\n");
        assert!(
            result.is_ok(),
            "reassignment to mut should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn reassign_immutable_rejected() {
        let diags = run("fn main():\n\tx = 1\n\tx = 2\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ImmutableAssign));
    }

    #[test]
    fn compound_assign_mut_ok() {
        let result = run("fn main():\n\tmut x = 10\n\tx += 5\n");
        assert!(
            result.is_ok(),
            "compound assign to mut should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn compound_assign_immutable_rejected() {
        let diags = run("fn main():\n\tx = 10\n\tx += 5\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ImmutableAssign));
    }

    #[test]
    fn compound_assign_undeclared_rejected() {
        let diags = run("fn main():\n\ty += 5\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::UndefinedAssignTarget));
    }

    #[test]
    fn div_by_zero_literal_rejected() {
        let diags = run("fn main():\n\tx = 1 / 0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn mod_by_zero_literal_rejected() {
        let diags = run("fn main():\n\tx = 1 % 0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn compound_div_by_zero_literal_rejected() {
        let diags = run("fn main():\n\tmut x = 10\n\tx /= 0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn compound_mod_by_zero_literal_rejected() {
        let diags = run("fn main():\n\tmut x = 10\n\tx %= 0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn div_by_neg_zero_literal_rejected() {
        // `-0` parses as unary minus over the zero literal, not as a
        // signed literal — the check must see through the Neg.
        let diags = run("fn main():\n\tx = 1 / -0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn mod_by_neg_zero_literal_rejected() {
        let diags = run("fn main():\n\tx = 1 % -0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn compound_div_by_neg_zero_literal_rejected() {
        let diags = run("fn main():\n\tmut x = 10\n\tx /= -0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn compound_mod_by_neg_zero_literal_rejected() {
        let diags = run("fn main():\n\tmut x = 10\n\tx %= -0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn div_by_const_expr_zero_rejected() {
        let diags = run("fn main():\n\tx = 1 / (2 - 2)\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn div_by_double_neg_zero_rejected() {
        let diags = run("fn main():\n\tx = 1 / -(-0)\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn compound_div_by_const_expr_zero_rejected() {
        let diags = run("fn main():\n\tmut x = 10\n\tx /= (5 - 5)\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DivisionByZero));
    }

    #[test]
    fn const_add_overflow_rejected() {
        // i64::MAX + 1: §18 traps overflow in all build modes, so a
        // constant expression that would trap is a compile error.
        let diags = run("fn main():\n\tx = 9223372036854775807 + 1\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ConstEvalFailure));
    }

    #[test]
    fn const_mul_overflow_rejected() {
        let diags = run("fn main():\n\tx = 9223372036854775807 * 2\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ConstEvalFailure));
    }

    #[test]
    fn const_neg_min_overflow_rejected() {
        // `(0 - MAX) - 1` const-evals to i64::MIN; negating it overflows.
        let diags = run("fn main():\n\tx = -((0 - 9223372036854775807) - 1)\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ConstEvalFailure));
    }

    #[test]
    fn const_eval_at_boundary_ok() {
        // i64::MIN itself is reachable without overflow.
        let result = run("fn main():\n\tx = (0 - 9223372036854775807) - 1\n");
        assert!(
            result.is_ok(),
            "i64::MIN should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn non_constant_zero_divisor_still_compiles() {
        // `x - x` is not a constant expression; the runtime guard
        // catches it instead of a compile error.
        let result = run("fn main():\n\tx = 5\n\ty = 1 / (x - x)\n");
        assert!(
            result.is_ok(),
            "non-constant divisor should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn float_div_by_zero_literal_allowed() {
        // IEEE 754: float division by zero yields inf, no diagnostic.
        let result = run("fn main():\n\tx = 1.0 / 0.0\n");
        assert!(
            result.is_ok(),
            "float division by zero should compile: {:?}",
            result.err()
        );
    }

    #[test]
    fn reassign_type_mismatch_rejected() {
        let diags = run("fn main():\n\tmut x = 1\n\tx = true\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    #[test]
    fn assign_or_decl_creates_immutable_binding() {
        let result = run("fn main():\n\tx = 42\n\ty = x\n");
        assert!(
            result.is_ok(),
            "bare x = 42 should declare immutable x readable later: {:?}",
            result.err()
        );
    }

    #[test]
    fn duplicate_decl_same_scope_rejected() {
        let diags = run("fn main():\n\tx = 1\n\tmut x = 2\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DuplicateDeclaration));
    }

    #[test]
    fn duplicate_mut_decl_same_scope_rejected() {
        let diags = run("fn main():\n\tmut x = 1\n\tmut x = 2\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DuplicateDeclaration));
    }

    #[test]
    fn duplicate_function_definition_rejected() {
        let diags =
            run("fn foo():\n\tx = 1\nfn foo():\n\ty = 2\nfn main():\n\tz = 3\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DuplicateDeclaration));
    }

    #[test]
    fn distinct_function_definitions_ok() {
        run("fn foo():\n\tx = 1\nfn bar():\n\ty = 2\nfn main():\n\tz = 3\n").unwrap();
    }

    #[test]
    fn cross_scope_reassign_mut() {
        let result = run("fn main():\n\tmut x = 1\n\tif true:\n\t\tx = 2\n");
        assert!(
            result.is_ok(),
            "cross-scope reassign of mut should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn cross_scope_reassign_immutable_rejected() {
        let diags = run("fn main():\n\tx = 1\n\tif true:\n\t\tx = 2\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ImmutableAssign));
    }

    #[test]
    fn explicit_shadow_with_mut_ok() {
        let result = run("fn main():\n\tx = 1\n\tif true:\n\t\tmut x = 2\n");
        assert!(
            result.is_ok(),
            "explicit shadow with mut should work: {:?}",
            result.err()
        );
    }

    #[test]
    fn percent_assign_on_float_rejected() {
        let diags = run("fn main():\n\tmut x = 1.0\n\tx %= 2.0\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::FloatModulo));
    }

    #[test]
    fn compound_assign_all_int_ops() {
        for op in ["+=", "-=", "*=", "/=", "%="] {
            let code = format!("fn main():\n\tmut x = 10\n\tx {} 3\n", op);
            let result = run(&code);
            assert!(
                result.is_ok(),
                "{} on int should succeed: {:?}",
                op,
                result.err()
            );
        }
    }

    #[test]
    fn compound_assign_float_arithmetic_ops() {
        for op in ["+=", "-=", "*=", "/="] {
            let code = format!("fn main():\n\tmut x = 1.0\n\tx {} 0.5\n", op);
            let result = run(&code);
            assert!(
                result.is_ok(),
                "{} on float should succeed: {:?}",
                op,
                result.err()
            );
        }
    }

    #[test]
    fn compound_assign_on_bool_rejected() {
        let diags = run("fn main():\n\tmut x = true\n\tx += 1\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnsupportedOperator));
    }

    #[test]
    fn compound_assign_rhs_type_mismatch_rejected() {
        let diags = run("fn main():\n\tmut x = 10\n\tx += true\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    // ---- Recursive / self-referential calls ----

    #[test]
    fn direct_recursion_accepted() {
        let code = "fn countdown(n: int) -> int:\n\tif n == 0:\n\t\treturn 0\n\treturn countdown(n - 1)\n\nfn main():\n\tx = countdown(5)\n";
        let result = run(code);
        assert!(
            result.is_ok(),
            "direct recursion should type-check: {:?}",
            result.err()
        );
    }

    #[test]
    fn direct_recursion_return_type_resolved() {
        let code = "fn fib(n: int) -> int:\n\tif n < 2:\n\t\treturn n\n\treturn fib(n - 1) + fib(n - 2)\n\nfn main():\n\tx = fib(10)\n";
        let (tirs, pool) = run(code).unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let v = main.var_decl_view(stmt_at(main, 0));
        assert_eq!(main.inst(v.initializer).ty, pool.int());
    }

    #[test]
    fn mutual_recursion_accepted() {
        let code = "fn is_even(n: int) -> bool:\n\tif n == 0:\n\t\treturn true\n\treturn is_odd(n - 1)\n\nfn is_odd(n: int) -> bool:\n\tif n == 0:\n\t\treturn false\n\treturn is_even(n - 1)\n\nfn main():\n\tx = is_even(4)\n";
        let result = run(code);
        assert!(
            result.is_ok(),
            "mutual recursion should type-check: {:?}",
            result.err()
        );
    }

    #[test]
    fn mutual_recursion_return_types_resolved() {
        let code = "fn is_even(n: int) -> bool:\n\tif n == 0:\n\t\treturn true\n\treturn is_odd(n - 1)\n\nfn is_odd(n: int) -> bool:\n\tif n == 0:\n\t\treturn false\n\treturn is_even(n - 1)\n\nfn main():\n\tx = is_even(4)\n\ty = is_odd(3)\n";
        let (tirs, pool) = run(code).unwrap();
        let main = tir_named(&tirs, &pool, "main");
        let vx = main.var_decl_view(stmt_at(main, 0));
        let vy = main.var_decl_view(stmt_at(main, 1));
        assert_eq!(main.inst(vx.initializer).ty, pool.bool_());
        assert_eq!(main.inst(vy.initializer).ty, pool.bool_());
    }

    #[test]
    fn recursive_call_wrong_arity_rejected() {
        let code = "fn f(n: int) -> int:\n\treturn f(n, n)\n\nfn main():\n\tx = f(1)\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::ArityMismatch));
    }

    #[test]
    fn recursive_call_wrong_arg_type_rejected() {
        let code = "fn f(n: int) -> int:\n\treturn f(true)\n\nfn main():\n\tx = f(1)\n";
        let diags = run(code).unwrap_err();
        assert!(any_code(&diags, DiagCode::TypeMismatch));
    }

    // ---- M8c2: while loops ----

    #[test]
    fn while_loop_basic() {
        let result = run("fn main():\n\twhile true:\n\t\tbreak\n");
        assert!(
            result.is_ok(),
            "basic while should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn while_non_bool_condition_rejected() {
        let diags = run("fn main():\n\twhile 42:\n\t\tbreak\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ConditionNotBool));
    }

    #[test]
    fn break_outside_loop_rejected() {
        let diags = run("fn main():\n\tbreak\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::BreakOutsideLoop));
    }

    #[test]
    fn continue_outside_loop_rejected() {
        let diags = run("fn main():\n\tcontinue\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ContinueOutsideLoop));
    }

    #[test]
    fn break_inside_while_ok() {
        let result = run("fn main():\n\twhile true:\n\t\tbreak\n");
        assert!(
            result.is_ok(),
            "break inside while should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn continue_inside_while_ok() {
        let result = run("fn main():\n\tmut n = 3\n\twhile n > 0:\n\t\tn -= 1\n\t\tcontinue\n");
        assert!(
            result.is_ok(),
            "continue inside while should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn while_body_has_own_scope() {
        let diags = run(
            "fn main():\n\twhile true:\n\t\tx = 1\n\t\tbreak\n\tassert(x == 1, \"unreachable\")\n",
        )
        .unwrap_err();
        assert!(any_code(&diags, DiagCode::UndefinedVariable));
    }

    #[test]
    fn nested_while_break_inner_only() {
        let result = run(
            "fn main():\n\tmut i = 2\n\twhile i > 0:\n\t\twhile true:\n\t\t\tbreak\n\t\ti -= 1\n",
        );
        assert!(
            result.is_ok(),
            "nested while with inner break: {:?}",
            result.err()
        );
    }

    // ---- M8c3: for-range loops ----

    #[test]
    fn for_range_basic() {
        let result = run("fn main():\n\tfor i in range(0, 5):\n\t\tx = i\n");
        assert!(
            result.is_ok(),
            "basic for-range should succeed: {:?}",
            result.err()
        );
    }

    #[test]
    fn for_range_non_int_start_rejected() {
        let diags = run("fn main():\n\tfor i in range(true, 5):\n\t\tx = i\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::RangeArgType));
    }

    #[test]
    fn for_range_non_int_end_rejected() {
        let diags = run("fn main():\n\tfor i in range(0, 1.5):\n\t\tx = i\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::RangeArgType));
    }

    #[test]
    fn for_range_loop_var_immutable() {
        let diags = run("fn main():\n\tfor i in range(0, 5):\n\t\ti = 10\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ImmutableAssign));
    }

    #[test]
    fn for_range_loop_var_not_visible_after() {
        let diags = run("fn main():\n\tfor i in range(0, 5):\n\t\tx = i\n\ty = i\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::UndefinedVariable));
    }

    #[test]
    fn for_range_duplicate_decl_in_body_rejected() {
        let diags = run("fn main():\n\tfor i in range(0, 5):\n\t\tmut i = 10\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::DuplicateDeclaration));
    }

    #[test]
    fn for_range_shadow_in_nested_scope_ok() {
        let result = run(
            "fn main():\n\tfor i in range(0, 5):\n\t\tif true:\n\t\t\tmut i = 10\n\t\t\tx = i\n",
        );
        assert!(
            result.is_ok(),
            "shadow in nested scope should work: {:?}",
            result.err()
        );
    }

    #[test]
    fn for_range_break_continue_ok() {
        let result = run(
            "fn main():\n\tfor i in range(0, 10):\n\t\tif i == 3:\n\t\t\tcontinue\n\t\tif i == 7:\n\t\t\tbreak\n",
        );
        assert!(
            result.is_ok(),
            "break/continue in for should work: {:?}",
            result.err()
        );
    }

    #[test]
    fn range_redefinition_as_variable_rejected() {
        let diags = run("fn main():\n\trange = 42\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
    }

    #[test]
    fn range_redefinition_as_mut_rejected() {
        let diags = run("fn main():\n\tmut range = 42\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
    }

    #[test]
    fn range_as_function_name_rejected() {
        let diags = run(
            "fn range(a: int, b: int) -> int:\n\treturn a + b\n\nfn main():\n\tprint(\"hi\")\n",
        )
        .unwrap_err();
        assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
    }

    #[test]
    fn range_called_outside_loop_gives_helpful_error() {
        let diags = run("fn main():\n\trange(0, 5)\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
    }

    #[test]
    fn reserved_name_no_cascade_undefined_variable() {
        let diags = run("fn main():\n\trange = 42\n\tprint(range)\n").unwrap_err();
        assert!(any_code(&diags, DiagCode::ReservedBuiltinName));
        assert!(
            !any_code(&diags, DiagCode::UndefinedVariable),
            "should not cascade into UndefinedVariable"
        );
    }

    // ---- M8.4: strview slices / views (final spec §3) ----

    #[test]
    fn slice_yields_str_view() {
        let (tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts: str = \"hello\"\n\tx = s[1:3]\n");
        assert!(diags.is_empty(), "got {:?}", diags);
        let main = &tirs[0];
        assert!(main.instructions.iter().any(|i| i.tag == TirTag::Slice));
    }

    #[test]
    fn slice_shorthand_forms_yield_views() {
        // `s[a:]`, `s[:b]`, `s[:]` — bounds omitted (§3.1).
        let (tirs, diags, pool) = run_with_errors(
            "fn main():\n\ts: str = \"hello\"\n\ta = s[1:]\n\tb = s[:2]\n\tc = s[:]\n",
        );
        assert!(diags.is_empty(), "got {:?}", diags);
        let main = tir_named(&tirs, &pool, "main");
        let slices: Vec<_> = main
            .instructions
            .iter()
            .filter(|i| i.tag == TirTag::Slice)
            .collect();
        assert_eq!(slices.len(), 3, "one Slice inst per slice expression");
        assert!(
            slices.iter().all(|i| i.ty == pool.str_view()),
            "every slice is typed strview"
        );
    }

    #[test]
    fn view_param_declared_type_and_borrow_mode() {
        let (tirs, diags, pool) = run_with_errors("fn f(x: strview):\n\tprint(x)\n");
        assert!(diags.is_empty(), "got {:?}", diags);
        let f = tir_named(&tirs, &pool, "f");
        assert_eq!(f.params[0].ty, pool.str_view());
        assert_eq!(f.params[0].mode, ParamMode::Borrow);
    }

    #[test]
    fn view_param_accepts_owned_str_via_conversion() {
        let (tirs, diags, pool) = run_with_errors(
            "fn shout(text: strview):\n\tprint(text)\n\nfn main():\n\ts: str = \"hi\"\n\tshout(s)\n",
        );
        assert!(diags.is_empty(), "got {:?}", diags);
        let main = tir_named(&tirs, &pool, "main");
        assert!(
            main.instructions.iter().any(|i| i.tag == TirTag::ViewOfStr),
            "owned str → strview param must insert a ViewOfStr conversion (§3.4)"
        );
    }

    #[test]
    fn view_param_accepts_view_arg_directly() {
        // E4: a view argument to a view parameter needs no conversion.
        let (tirs, diags, pool) = run_with_errors(
            "fn shout(text: strview):\n\tprint(text)\n\nfn main():\n\ts: str = \"hi\"\n\tshout(s[0:1])\n",
        );
        assert!(diags.is_empty(), "got {:?}", diags);
        let main = tir_named(&tirs, &pool, "main");
        assert!(
            !main.instructions.iter().any(|i| i.tag == TirTag::ViewOfStr),
            "view → view must not insert ViewOfStr"
        );
    }

    #[test]
    fn view_passes_to_owned_str_param_via_reborrow() {
        // P6': a view passed to an owned `str` parameter re-borrows —
        // sema inserts a ViewAsStr conversion (cap=0 triple at codegen,
        // no allocation, call-scoped).
        let (tirs, diags, pool) = run_with_errors(
            "fn show(s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\tshow(s[0:1])\n",
        );
        assert!(diags.is_empty(), "got {:?}", diags);
        let main = tir_named(&tirs, &pool, "main");
        assert!(
            main.instructions.iter().any(|i| i.tag == TirTag::ViewAsStr),
            "strview → str param must insert a ViewAsStr re-borrow (P6')"
        );
    }

    #[test]
    fn view_binding_to_str_still_rejected() {
        // The re-borrow is call-scoped only: binding-form conversion
        // (`x: str = view`) stays E0012.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = s[0:1]\n");
        assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
    }

    #[test]
    fn view_to_move_str_param_still_rejected() {
        // E2 (final spec §3.3): the re-borrow is borrow-mode only — a
        // `move` str parameter would let the view escape the call.
        let (_tirs, diags, _pool) = run_with_errors(
            "fn eat(move s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\teat(s[0:1])\n",
        );
        assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
    }

    #[test]
    fn view_return_rejected() {
        let (_tirs, diags, _pool) =
            run_with_errors("fn bad(s: str) -> strview:\n\treturn s[0:1]\n");
        assert!(
            any_code(&diags, DiagCode::ReturnBorrowedValue),
            "got {:?}",
            diags
        );
    }

    #[test]
    fn move_and_inout_view_params_rejected() {
        let (_tirs, diags, _pool) = run_with_errors("fn f(move x: strview):\n\tprint(x)\n");
        assert!(
            diags.iter().any(|d| d.code == DiagCode::TypeMismatch
                && d.message.contains("already a borrow")),
            "got {:?}",
            diags
        );
        let (_tirs, diags, _pool) = run_with_errors("fn g(inout x: strview):\n\tprint(x)\n");
        assert!(
            diags.iter().any(|d| d.code == DiagCode::TypeMismatch
                && d.message.contains("already a borrow")),
            "got {:?}",
            diags
        );
    }

    #[test]
    fn owned_annotation_over_slice_rejected() {
        // P6: an owned copy of viewed contents needs an explicit copy
        // API (future work); plain annotation is a mismatch.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = s[0:1]\n");
        assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
    }

    #[test]
    fn owned_to_view_binding_rejected() {
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: strview = s\n");
        assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
    }

    #[test]
    fn view_annotation_over_slice_ok() {
        let (tirs, diags, pool) =
            run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: strview = s[0:1]\n");
        assert!(diags.is_empty(), "got {:?}", diags);
        let main = tir_named(&tirs, &pool, "main");
        assert_eq!(main.inst(stmt_at(main, 1)).ty, pool.str_view());
    }

    #[test]
    fn print_and_len_accept_views() {
        let (_tirs, diags, _pool) = run_with_errors(
            "fn main():\n\ts: str = \"hello\"\n\tprint(s[0:2])\n\tprint(int_to_str(s[0:2].len()))\n",
        );
        assert!(diags.is_empty(), "got {:?}", diags);
    }

    #[test]
    fn is_empty_accepts_view() {
        let (_tirs, diags, _pool) = run_with_errors(
            "fn main():\n\ts: str = \"hi\"\n\tif s[0:1].is_empty():\n\t\tprint(\"e\")\n",
        );
        assert!(diags.is_empty(), "got {:?}", diags);
    }

    #[test]
    fn mixed_str_view_equality_accepted() {
        let (_tirs, diags, _pool) = run_with_errors(
            "fn main():\n\ts: str = \"he\"\n\tif s == s[0:2]:\n\t\tprint(\"eq\")\n",
        );
        assert!(diags.is_empty(), "got {:?}", diags);
    }

    #[test]
    fn mixed_equality_wraps_str_side_in_view() {
        // §3.3/§3.4: the owned side of a mixed comparison is converted.
        let (tirs, diags, pool) =
            run_with_errors("fn main():\n\ts: str = \"he\"\n\tx = s == s[0:2]\n");
        assert!(diags.is_empty(), "got {:?}", diags);
        let main = tir_named(&tirs, &pool, "main");
        assert!(main.instructions.iter().any(|i| i.tag == TirTag::ViewOfStr));
        assert!(main.instructions.iter().any(|i| i.tag == TirTag::StrCmpEq));
    }

    #[test]
    fn view_view_equality_accepted() {
        let (tirs, diags, pool) = run_with_errors(
            "fn main():\n\ts: str = \"he\"\n\tif s[0:1] != s[1:2]:\n\t\tprint(\"ne\")\n",
        );
        assert!(diags.is_empty(), "got {:?}", diags);
        let main = tir_named(&tirs, &pool, "main");
        assert!(main.instructions.iter().any(|i| i.tag == TirTag::StrCmpNe));
        assert!(
            !main.instructions.iter().any(|i| i.tag == TirTag::ViewOfStr),
            "view vs view needs no conversion"
        );
    }

    #[test]
    fn view_concat_rejected() {
        // `+` is owned-str concatenation only; a view operand falls
        // through to E0015 UnsupportedOperator.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts: str = \"ab\"\n\tx = s[0:1] + s[0:1]\n");
        assert!(
            any_code(&diags, DiagCode::UnsupportedOperator),
            "got {:?}",
            diags
        );
    }

    #[test]
    fn slice_of_int_rejected() {
        let (_tirs, diags, _pool) = run_with_errors("fn main():\n\tn: int = 5\n\tx = n[0:1]\n");
        assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
    }

    #[test]
    fn slice_bound_must_be_int() {
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx = s[0.5:1]\n");
        assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
    }

    #[test]
    fn str_push_accepts_view_suffix() {
        // 4b: str_push's suffix parameter is `strview`; a slice passes
        // directly, keeping [Inout, Borrow] modes.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\tmut s = \"hi\"\n\tstr_push(&s, s[0:1])\n");
        assert!(diags.is_empty(), "got {:?}", diags);
    }

    #[test]
    fn str_materialize_typechecks_to_owned_str() {
        // M8.4.1.2: `str(view)` materializes an owned copy — the result
        // type-checks as `str` and lowers to the synthesized
        // `__ryo_str_from_view` call.
        let (tirs, diags, pool) =
            run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = str(s[0:1])\n\tprint(x)\n");
        assert!(diags.is_empty(), "got {:?}", diags);
        let main = tir_named(&tirs, &pool, "main");
        let init = main.var_decl_view(stmt_at(main, 1)).initializer;
        let call = main.inst(init);
        assert!(matches!(call.tag, TirTag::Call), "got {:?}", call.tag);
        assert_eq!(call.ty, pool.str_(), "materialize result is owned str");
        assert_eq!(pool.str(main.call_view(init).name), "__ryo_str_from_view");
    }

    #[test]
    fn str_materialize_rejects_non_view_arg() {
        // An owned `str` argument is NOT accepted — materializing it
        // would be a same-type copy, which is the future `Clone` trait's
        // job; the call form is `strview`-only.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = str(s)\n");
        assert!(any_code(&diags, DiagCode::TypeMismatch), "got {:?}", diags);
        assert!(
            first_msg(&diags).contains("str() argument must be strview"),
            "got {:?}",
            diags
        );
    }

    #[test]
    fn str_materialize_arity_mismatch() {
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts: str = \"hi\"\n\tx: str = str(s[0:1], s[1:2])\n");
        assert!(any_code(&diags, DiagCode::ArityMismatch), "got {:?}", diags);
    }

    #[test]
    fn str_materialize_borrow_arg_rejected() {
        // Builtins never take `inout`: `&` is rejected exactly like the
        // table builtins (mirrors `int_to_str(&c)`).
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\tmut s: str = \"hi\"\n\tx: str = str(&s)\n");
        assert!(
            any_code(&diags, DiagCode::BorrowMismatch),
            "got {:?}",
            diags
        );
    }

    #[test]
    fn user_str_function_shadows_materialize_intercept() {
        // Least surprise: a user-defined `fn str` wins over the call-form
        // intercept — `str(41)` resolves to the user function, so an int
        // argument is fine and no strview diagnostic fires.
        let (_tirs, diags, _pool) = run_with_errors(
            "fn str(x: int) -> int:\n\treturn x + 1\n\nfn main():\n\ty: int = str(41)\n\tprint(int_to_str(y))\n",
        );
        assert!(diags.is_empty(), "got {:?}", diags);
    }

    #[test]
    fn w0003_materialize_arg_to_borrowed_str_param_warns() {
        // W0003 case A: `str(view)` fed straight into a borrowed `str`
        // parameter is redundant — the view would pass via the P6'
        // re-borrow (cap=0, no allocation).
        let (_tirs, diags, _pool) = run_with_errors(
            "fn show(s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\tshow(str(s[0:1]))\n",
        );
        assert_eq!(
            count_code(&diags, DiagCode::RedundantMaterialize),
            1,
            "expected exactly one W0003; got {:?}",
            diags
        );
    }

    #[test]
    fn w0003_materialize_arg_to_move_param_does_not_warn() {
        // A `move` parameter cannot be served by the re-borrow — the
        // materialized copy is legitimate there.
        let (_tirs, diags, _pool) = run_with_errors(
            "fn eat(move s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\teat(str(s[0:1]))\n",
        );
        assert!(
            !any_code(&diags, DiagCode::RedundantMaterialize),
            "got {:?}",
            diags
        );
    }

    #[test]
    fn w0003_materialize_arg_to_inout_param_does_not_warn() {
        // An `inout` parameter cannot be served by the re-borrow either
        // (its argument requires `&`) — the copy is legitimate there.
        // The call below is rejected for the missing `&`; W0003 must
        // stay silent on that error path too.
        let (_tirs, diags, _pool) = run_with_errors(
            "fn eat(inout s: str):\n\tprint(s)\n\nfn main():\n\ts: str = \"hi\"\n\teat(str(s[0:1]))\n",
        );
        assert!(
            !any_code(&diags, DiagCode::RedundantMaterialize),
            "got {:?}",
            diags
        );
    }

    #[test]
    fn w0003_materialize_arg_to_print_warns() {
        // W0003 case A, builtin shape: `print` accepts `strview`
        // arguments directly — materializing first buys nothing.
        let (_tirs, diags, _pool) =
            run_with_errors("fn main():\n\ts: str = \"hi\"\n\tprint(str(s[0:1]))\n");
        assert_eq!(
            count_code(&diags, DiagCode::RedundantMaterialize),
            1,
            "expected exactly one W0003; got {:?}",
            diags
        );
    }

    #[test]
    fn w0003_materialize_suffix_to_str_push_warns() {
        // W0003 case A, builtin shape: str_push's suffix is `strview` —
        // materializing it first is a redundant allocation. (Source and
        // suffix come from DISTINCT strings so Rule 7 stays quiet.)
        let (_tirs, diags, _pool) = run_with_errors(
            "fn main():\n\tmut s: str = \"hi\"\n\tt: str = \"yo\"\n\tstr_push(&s, str(t[0:1]))\n",
        );
        assert_eq!(
            count_code(&diags, DiagCode::RedundantMaterialize),
            1,
            "expected exactly one W0003; got {:?}",
            diags
        );
    }
}
