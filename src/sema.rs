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

use crate::builtins;
use crate::diag::{Diag, DiagCode, DiagSink};
use crate::tir::{Tir, TirBuilder, TirParam, TirRef, TirTag};
use crate::types::{InternPool, StringId, TypeId, TypeKind};
use crate::uir::{CallView, FuncBody, InstData, InstRef, InstTag, Span, Uir, VarDeclView};
use std::collections::{HashMap, VecDeque};

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

struct Scope<'a> {
    parent: Option<&'a Scope<'a>>,
    bindings: HashMap<StringId, TypeId>,
}

impl<'a> Scope<'a> {
    fn new() -> Self {
        Scope {
            parent: None,
            bindings: HashMap::new(),
        }
    }

    fn insert(&mut self, name: StringId, ty: TypeId) {
        self.bindings.insert(name, ty);
    }

    fn lookup(&self, name: StringId) -> Option<TypeId> {
        self.bindings
            .get(&name)
            .copied()
            .or_else(|| self.parent?.lookup(name))
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
    file_path: &str,
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
    file_path: &'a str,
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
}

impl<'a> Sema<'a> {
    /// Drive sema to fixpoint and return one [`Tir`] per UIR
    /// function body, in source order.
    pub fn run(
        uir: &'a Uir,
        pool: &'a mut InternPool,
        sink: &'a mut DiagSink,
        source: &'a str,
        file_path: &'a str,
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
        file_path: &'a str,
    ) -> Self {
        let n = uir.func_bodies.len();
        let mut name_to_decl = HashMap::with_capacity(n);
        for (i, body) in uir.func_bodies.iter().enumerate() {
            // First definition wins on duplicates; the second-and-
            // beyond will type-check against the first, which keeps
            // the pipeline robust until a dedicated redefinition
            // pass lands.
            name_to_decl
                .entry(body.name)
                .or_insert(DeclId::from_index(i));
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
            self.signatures.insert(
                body.name,
                FunctionSig {
                    params: body.params.iter().map(|p| p.ty).collect(),
                    return_type: body.return_type,
                },
            );
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
        scope.insert(param.name, param.ty);
    }

    let params: Vec<TirParam> = body
        .params
        .iter()
        .map(|p| TirParam {
            name: p.name,
            ty: p.ty,
            span: p.span,
        })
        .collect();

    let mut fcx = FuncCtx {
        builder: TirBuilder::new(body.name, params, body.return_type, body.span),
        // Mapping from UIR `InstRef` to the TIR ref Sema emitted for
        // it inside *this* function. UIR is tree-shaped today so
        // this is defensive — but it's the right invariant before
        // future SSA-shaped UIR with shared sub-expressions.
        inst_map: vec![None; sema.uir.instructions.len()],
        return_type: body.return_type,
    };

    let mut stmt_refs: Vec<TirRef> = Vec::with_capacity(sema.uir.body_stmts(body).len());
    for stmt_ref in sema.uir.body_stmts(body) {
        stmt_refs.push(analyze_stmt(sema, &mut fcx, &mut scope, stmt_ref));
    }

    fcx.builder.finish(&stmt_refs)
}

/// Per-function emission state. Lives only for the duration of one
/// `analyze_function` call; the `inst_map` and `TirBuilder` arenas
/// are scoped to a single body.
struct FuncCtx {
    builder: TirBuilder,
    inst_map: Vec<Option<TirRef>>,
    return_type: TypeId,
}

fn analyze_stmt(sema: &mut Sema<'_>, fcx: &mut FuncCtx, scope: &mut Scope, r: InstRef) -> TirRef {
    let inst = sema.uir.inst(r);
    let span = sema.uir.span(r);
    match inst.tag {
        InstTag::VarDecl => {
            let view = sema.uir.var_decl_view(r);
            let init_tir = analyze_expr(sema, fcx, scope, view.initializer);
            let inferred = fcx.builder.ty_of(init_tir);
            // A void value (e.g. `x = print(...)`) cannot be bound
            // to a name. Reject and recover with the error sentinel
            // so downstream uses don't cascade.
            let inferred = if inferred == sema.pool.void() {
                sema.sink.emit(Diag::error(
                    sema.uir.span(view.initializer),
                    DiagCode::VoidValueInExpression,
                    format!(
                        "cannot bind '{}' to a 'void' value: the right-hand side has no value",
                        sema.pool.str(view.name),
                    ),
                ));
                sema.pool.error_type()
            } else {
                inferred
            };
            let resolved = resolve_var_decl_type(&view, inferred, sema);
            scope.insert(view.name, resolved);
            fcx.builder
                .var_decl(view.name, view.mutable, resolved, init_tir, span)
        }
        InstTag::Return => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Return must carry InstData::UnOp"),
            };
            let val_tir = analyze_expr(sema, fcx, scope, operand);
            let actual = fcx.builder.ty_of(val_tir);
            if fcx.return_type == sema.pool.void() {
                if !sema.pool.is_error(actual) {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::TypeMismatch,
                        format!(
                            "cannot return a value from a function with return type 'void' (got '{}')",
                            sema.pool.display(actual),
                        ),
                    ));
                }
            } else if !sema.pool.compatible(actual, fcx.return_type) {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!(
                        "return type mismatch: function expects '{}', got '{}'",
                        sema.pool.display(fcx.return_type),
                        sema.pool.display(actual),
                    ),
                ));
            }
            fcx.builder
                .unary(TirTag::Return, sema.pool.void(), val_tir, span)
        }
        InstTag::ReturnVoid => {
            if fcx.return_type != sema.pool.void() && !sema.pool.is_error(fcx.return_type) {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::TypeMismatch,
                    format!(
                        "missing return value: function expects '{}'",
                        sema.pool.display(fcx.return_type),
                    ),
                ));
            }
            fcx.builder.return_void(sema.pool.void(), span)
        }
        InstTag::ExprStmt => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("ExprStmt must carry InstData::UnOp"),
            };
            let val_tir = analyze_expr(sema, fcx, scope, operand);
            fcx.builder
                .unary(TirTag::ExprStmt, sema.pool.void(), val_tir, span)
        }
        InstTag::IfStmt => {
            let view = sema.uir.if_stmt_view(r);

            let cond_tir = analyze_expr(sema, fcx, scope, view.cond);
            check_condition_bool(sema, fcx, cond_tir, view.cond);

            let then_tirs = analyze_block(sema, fcx, scope, &view.then_stmts);

            let mut elif_tirs = Vec::with_capacity(view.elif_branches.len());
            for elif in &view.elif_branches {
                let elif_cond_tir = analyze_expr(sema, fcx, scope, elif.cond);
                check_condition_bool(sema, fcx, elif_cond_tir, elif.cond);
                let elif_body_tirs = analyze_block(sema, fcx, scope, &elif.body);
                elif_tirs.push((elif_cond_tir, elif_body_tirs));
            }

            let else_tirs = view
                .else_stmts
                .as_ref()
                .map(|stmts| analyze_block(sema, fcx, scope, stmts));

            fcx.builder.if_stmt(
                cond_tir,
                &then_tirs,
                &elif_tirs,
                else_tirs.as_deref(),
                sema.pool.void(),
                span,
            )
        }
        other => panic!(
            "analyze_stmt: instruction at %{} is not a statement (tag={:?})",
            r.index(),
            other
        ),
    }
}

fn analyze_block(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    stmts: &[InstRef],
) -> Vec<TirRef> {
    let mut child_scope = Scope {
        parent: Some(scope),
        bindings: HashMap::new(),
    };
    let mut tirs = Vec::with_capacity(stmts.len());
    for stmt_ref in stmts {
        tirs.push(analyze_stmt(sema, fcx, &mut child_scope, *stmt_ref));
    }
    tirs
}

fn check_condition_bool(sema: &mut Sema<'_>, fcx: &FuncCtx, cond_tir: TirRef, cond_uir: InstRef) {
    let cond_ty = fcx.builder.ty_of(cond_tir);
    if !sema.pool.is_error(cond_ty) && cond_ty != sema.pool.bool_() {
        sema.sink.emit(Diag::error(
            sema.uir.span(cond_uir),
            DiagCode::ConditionNotBool,
            format!(
                "condition must be 'bool', got '{}'",
                sema.pool.display(cond_ty),
            ),
        ));
    }
}

fn resolve_var_decl_type(view: &VarDeclView, inferred: TypeId, sema: &mut Sema<'_>) -> TypeId {
    match view.ty {
        Some(annotated) if !sema.pool.compatible(annotated, inferred) => {
            // Anchor the squiggle on the offending value (the
            // initializer) rather than on the whole `[mut] name [:
            // type] = expr` decl span — the type came from the
            // annotation but the *mismatch* is the initializer's
            // fault.
            sema.sink.emit(Diag::error(
                sema.uir.span(view.initializer),
                DiagCode::TypeMismatch,
                format!(
                    "type mismatch: '{}' annotated '{}', initializer is '{}'",
                    sema.pool.str(view.name),
                    sema.pool.display(annotated),
                    sema.pool.display(inferred),
                ),
            ));
            annotated
        }
        Some(annotated) => annotated,
        None => inferred,
    }
}

fn analyze_expr(sema: &mut Sema<'_>, fcx: &mut FuncCtx, scope: &Scope, r: InstRef) -> TirRef {
    if let Some(t) = fcx.inst_map[r.index()] {
        return t;
    }

    let inst = sema.uir.inst(r);
    let span = sema.uir.span(r);
    let emitted = match inst.tag {
        InstTag::IntLiteral => match inst.data {
            InstData::Int(v) => fcx.builder.int_const(v, sema.pool.int(), span),
            _ => unreachable!("IntLiteral must carry InstData::Int"),
        },
        InstTag::StrLiteral => match inst.data {
            InstData::Str(s) => fcx.builder.str_const(s, sema.pool.str_(), span),
            _ => unreachable!("StrLiteral must carry InstData::Str"),
        },
        InstTag::BoolLiteral => match inst.data {
            InstData::Bool(b) => fcx.builder.bool_const(b, sema.pool.bool_(), span),
            _ => unreachable!("BoolLiteral must carry InstData::Bool"),
        },
        InstTag::FloatLiteral => match inst.data {
            InstData::Float(v) => fcx.builder.float_const(v, sema.pool.float(), span),
            _ => unreachable!("FloatLiteral must carry InstData::Float"),
        },
        InstTag::Var => {
            let name = match inst.data {
                InstData::Var(s) => s,
                _ => unreachable!("Var must carry InstData::Var"),
            };
            match scope.lookup(name) {
                Some(t) => fcx.builder.var(name, t, span),
                None => {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UndefinedVariable,
                        format!("undefined variable: '{}'", sema.pool.str(name)),
                    ));
                    fcx.builder.unreachable(sema.pool.error_type(), span)
                }
            }
        }
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
        | InstTag::Or => {
            let (lhs, rhs) = match inst.data {
                InstData::BinOp { lhs, rhs } => (lhs, rhs),
                _ => unreachable!("binary op must carry InstData::BinOp"),
            };
            let l = analyze_expr(sema, fcx, scope, lhs);
            let r2 = analyze_expr(sema, fcx, scope, rhs);
            let lhs_ty = fcx.builder.ty_of(l);
            let rhs_ty = fcx.builder.ty_of(r2);
            check_binary_op(sema, fcx, inst.tag, lhs_ty, rhs_ty, l, r2, span)
        }
        InstTag::Neg => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Neg must carry InstData::UnOp"),
            };
            let sub = analyze_expr(sema, fcx, scope, operand);
            let sub_ty = fcx.builder.ty_of(sub);
            match sema.pool.kind(sub_ty) {
                TypeKind::Int => fcx.builder.unary(TirTag::INeg, sema.pool.int(), sub, span),
                TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
                _ => {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UnsupportedOperator,
                        format!(
                            "unary operator '-' not supported for type '{}'",
                            sema.pool.display(sub_ty),
                        ),
                    ));
                    fcx.builder.unreachable(sema.pool.error_type(), span)
                }
            }
        }
        InstTag::Not => {
            let operand = match inst.data {
                InstData::UnOp(o) => o,
                _ => unreachable!("Not must carry InstData::UnOp"),
            };
            let sub = analyze_expr(sema, fcx, scope, operand);
            let sub_ty = fcx.builder.ty_of(sub);
            match sema.pool.kind(sub_ty) {
                TypeKind::Bool => fcx
                    .builder
                    .unary(TirTag::BoolNot, sema.pool.bool_(), sub, span),
                TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
                _ => {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::UnsupportedOperator,
                        format!(
                            "logical operator 'not' requires 'bool' operand, got '{}'",
                            sema.pool.display(sub_ty),
                        ),
                    ));
                    fcx.builder.unreachable(sema.pool.error_type(), span)
                }
            }
        }
        InstTag::Call => {
            let view = sema.uir.call_view(r);
            // Translate args first (in source order) to fix their
            // TIR refs and types, *then* validate against the
            // signature so per-argument diagnostics carry the right
            // span and the call still emits a well-formed TIR Call.
            let mut arg_tirs = Vec::with_capacity(view.args.len());
            for a in &view.args {
                arg_tirs.push(analyze_expr(sema, fcx, scope, *a));
            }
            check_call(sema, fcx, &view, &arg_tirs, span)
        }
        other => panic!(
            "analyze_expr: instruction at %{} is not an expression (tag={:?})",
            r.index(),
            other
        ),
    };

    fcx.inst_map[r.index()] = Some(emitted);
    emitted
}

#[allow(clippy::too_many_arguments)]
fn check_binary_op(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    tag: InstTag,
    lhs_ty: TypeId,
    rhs_ty: TypeId,
    lhs: TirRef,
    rhs: TirRef,
    span: Span,
) -> TirRef {
    if !sema.pool.compatible(lhs_ty, rhs_ty) {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::TypeMismatch,
            format!(
                "type mismatch in '{}': left is '{}', right is '{}'",
                bin_op_symbol(tag),
                sema.pool.display(lhs_ty),
                sema.pool.display(rhs_ty),
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    let kind_ty = if sema.pool.is_error(lhs_ty) {
        rhs_ty
    } else {
        lhs_ty
    };
    let is_equality = matches!(tag, InstTag::Eq | InstTag::NotEq);
    let is_ordering = matches!(
        tag,
        InstTag::Lt | InstTag::Gt | InstTag::LtEq | InstTag::GtEq
    );
    let is_modulo = matches!(tag, InstTag::Mod);
    let is_logical = matches!(tag, InstTag::And | InstTag::Or);
    let kind = sema.pool.kind(kind_ty);

    if is_logical {
        match kind {
            TypeKind::Bool => {
                let tir_tag = match tag {
                    InstTag::And => TirTag::BoolAnd,
                    InstTag::Or => TirTag::BoolOr,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            _ => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "logical operator '{}' requires 'bool' operands, got '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
    } else if is_equality {
        match kind {
            TypeKind::Int | TypeKind::Bool => {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::ICmpEq,
                    InstTag::NotEq => TirTag::ICmpNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Float => {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::FCmpEq,
                    InstTag::NotEq => TirTag::FCmpNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            TypeKind::Str => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "equality operator '{}' not supported for type 'str' (yet)",
                        bin_op_symbol(tag),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
            TypeKind::Void | TypeKind::Never | TypeKind::Tuple => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "equality operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
    } else if is_ordering {
        match kind {
            TypeKind::Int => {
                let tir_tag = match tag {
                    InstTag::Lt => TirTag::ICmpLt,
                    InstTag::LtEq => TirTag::ICmpLe,
                    InstTag::Gt => TirTag::ICmpGt,
                    InstTag::GtEq => TirTag::ICmpGe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Float => {
                let tir_tag = match tag {
                    InstTag::Lt => TirTag::FCmpLt,
                    InstTag::LtEq => TirTag::FCmpLe,
                    InstTag::Gt => TirTag::FCmpGt,
                    InstTag::GtEq => TirTag::FCmpGe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            }
            TypeKind::Str => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "ordering operator '{}' not supported for type 'str' (yet)",
                        bin_op_symbol(tag),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
            TypeKind::Bool | TypeKind::Void | TypeKind::Never | TypeKind::Tuple => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "ordering operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
        }
    } else if is_modulo {
        match kind {
            TypeKind::Int => fcx
                .builder
                .binary(TirTag::IMod, sema.pool.int(), lhs, rhs, span),
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            _ => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "modulo operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
    } else {
        // Arithmetic: +, -, *, /
        match kind {
            TypeKind::Int => {
                let tir_tag = match tag {
                    InstTag::Add => TirTag::IAdd,
                    InstTag::Sub => TirTag::ISub,
                    InstTag::Mul => TirTag::IMul,
                    InstTag::Div => TirTag::ISDiv,
                    _ => unreachable!(),
                };
                fcx.builder.binary(tir_tag, sema.pool.int(), lhs, rhs, span)
            }
            TypeKind::Float => {
                let tir_tag = match tag {
                    InstTag::Add => TirTag::FAdd,
                    InstTag::Sub => TirTag::FSub,
                    InstTag::Mul => TirTag::FMul,
                    InstTag::Div => TirTag::FDiv,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.float(), lhs, rhs, span)
            }
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            _ => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "arithmetic operator '{}' not supported for type '{}'",
                        bin_op_symbol(tag),
                        sema.pool.display(kind_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
    }
}

fn check_call(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    view: &CallView,
    arg_tirs: &[TirRef],
    span: Span,
) -> TirRef {
    let name_id = view.name;

    // Builtins short-circuit: they're not in `signatures` /
    // `name_to_decl`, so signature resolution and the worklist
    // never see them.
    if let Some(builtin) = builtins::lookup(sema.pool.str(name_id)) {
        return emit_builtin_call(sema, fcx, view, arg_tirs, span, builtin);
    }

    // Demand the callee. With eagerly-resolved signatures this is
    // a check that the decl exists *and* is not currently
    // `InProgress` (cycle). Today the latter is unreachable —
    // bodies don't depend on bodies — but the call sits here so
    // future inferred-return-type / comptime work picks up cycle
    // detection for free.
    let callee = match sema.name_to_decl.get(&name_id).copied() {
        Some(d) => d,
        None => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UndefinedFunction,
                format!("undefined function: '{}'", sema.pool.str(name_id)),
            ));
            return fcx.builder.unreachable(sema.pool.error_type(), span);
        }
    };
    if !sema.require_decl(callee, span, name_id) {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let sig = sema
        .signatures
        .get(&name_id)
        .expect("signature must be present once decl exists");

    // Snapshot the parts of `sig` we need so we can release the
    // borrow on `sema` before emitting per-argument diagnostics.
    let expected: Vec<TypeId> = sig.params.clone();
    let return_type = sig.return_type;

    if view.args.len() != expected.len() {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!(
                "call to '{}' has wrong arity: expected {} argument(s), got {}",
                sema.pool.str(name_id),
                expected.len(),
                view.args.len(),
            ),
        ));
    } else {
        for (idx, ((arg_uir, arg_tir), &exp_ty)) in view
            .args
            .iter()
            .zip(arg_tirs.iter())
            .zip(expected.iter())
            .enumerate()
        {
            let actual = fcx.builder.ty_of(*arg_tir);
            if !sema.pool.compatible(actual, exp_ty) {
                sema.sink.emit(Diag::error(
                    sema.uir.span(*arg_uir),
                    DiagCode::TypeMismatch,
                    format!(
                        "call to '{}': argument {} has type '{}', expected '{}'",
                        sema.pool.str(name_id),
                        idx + 1,
                        sema.pool.display(actual),
                        sema.pool.display(exp_ty),
                    ),
                ));
            }
        }
    }
    fcx.builder.call(name_id, arg_tirs, return_type, span)
}

/// Front-end validation for builtin calls.
///
fn emit_builtin_call(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    view: &CallView,
    arg_tirs: &[TirRef],
    span: Span,
    builtin: &'static crate::builtins::BuiltinFunction,
) -> TirRef {
    let name = sema.pool.str(view.name);
    match name {
        "print" => {
            // print keeps the old path: emit as a normal builtin Call
            let ret_ty = builtin.return_type(sema.pool);
            let call_ref = fcx.builder.call(view.name, arg_tirs, ret_ty, span);
            check_print_args(sema, view, span);
            call_ref
        }
        "panic" => emit_panic(sema, fcx, view, span),
        "assert" => emit_assert(sema, fcx, view, arg_tirs, span),
        _ => {
            let ret_ty = builtin.return_type(sema.pool);
            fcx.builder.call(view.name, arg_tirs, ret_ty, span)
        }
    }
}

fn emit_panic(sema: &mut Sema<'_>, fcx: &mut FuncCtx, view: &CallView, span: Span) -> TirRef {
    if view.args.len() != 1 {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!("panic() takes exactly 1 argument, got {}", view.args.len()),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if !matches!(sema.uir.inst(view.args[0]).tag, InstTag::StrLiteral) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.args[0]),
            DiagCode::BuiltinArgKind,
            "panic() argument must be a string literal",
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    let user_msg_id = match sema.uir.inst(view.args[0]).data {
        InstData::Str(id) => id,
        _ => unreachable!("StrLiteral tag implies Str data"),
    };
    let user_msg = sema.pool.str(user_msg_id).to_string();
    let (line, col) = byte_offset_to_line_col(sema.source, span.start);
    let func_name = sema.pool.str(fcx.builder.name());
    let formatted = format!(
        "panicked at {}:{}:{} in {}(): {}\n",
        sema.file_path, line, col, func_name, user_msg
    );
    let msg_len = formatted.len() as i64;
    let formatted_id = sema.pool.intern_str(&formatted);

    let str_ref = fcx.builder.str_const(formatted_id, sema.pool.str_(), span);
    let len_ref = fcx.builder.int_const(msg_len, sema.pool.int(), span);
    let panic_name = sema.pool.intern_str("__ryo_panic");
    fcx.builder
        .call(panic_name, &[str_ref, len_ref], sema.pool.never(), span)
}

fn emit_assert(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    view: &CallView,
    arg_tirs: &[TirRef],
    span: Span,
) -> TirRef {
    if view.args.len() != 2 {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!(
                "assert() takes exactly 2 arguments (condition, message), got {}",
                view.args.len()
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let cond_ty = fcx.builder.ty_of(arg_tirs[0]);
    if !sema.pool.compatible(cond_ty, sema.pool.bool_()) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.args[0]),
            DiagCode::TypeMismatch,
            format!(
                "assert() condition must be 'bool', got '{}'",
                sema.pool.display(cond_ty),
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    if !matches!(sema.uir.inst(view.args[1]).tag, InstTag::StrLiteral) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.args[1]),
            DiagCode::BuiltinArgKind,
            "assert() message must be a string literal",
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let user_msg_id = match sema.uir.inst(view.args[1]).data {
        InstData::Str(id) => id,
        _ => unreachable!("StrLiteral tag implies Str data"),
    };
    let user_msg = sema.pool.str(user_msg_id).to_string();
    let (line, col) = byte_offset_to_line_col(sema.source, span.start);
    let func_name = sema.pool.str(fcx.builder.name());
    let formatted = format!(
        "assertion failed at {}:{}:{} in {}(): {}\n",
        sema.file_path, line, col, func_name, user_msg
    );
    let msg_len = formatted.len() as i64;
    let formatted_id = sema.pool.intern_str(&formatted);

    // Build the negated condition: `not cond`
    let neg_cond = fcx
        .builder
        .unary(TirTag::BoolNot, sema.pool.bool_(), arg_tirs[0], span);

    // Build the panic call as the then-body
    let str_ref = fcx.builder.str_const(formatted_id, sema.pool.str_(), span);
    let len_ref = fcx.builder.int_const(msg_len, sema.pool.int(), span);
    let panic_name = sema.pool.intern_str("__ryo_panic");
    let panic_call = fcx
        .builder
        .call(panic_name, &[str_ref, len_ref], sema.pool.never(), span);
    let panic_stmt = fcx
        .builder
        .unary(TirTag::ExprStmt, sema.pool.void(), panic_call, span);

    // Desugar to: if not cond: __ryo_panic(msg_ptr, msg_len)
    fcx.builder
        .if_stmt(neg_cond, &[panic_stmt], &[], None, sema.pool.void(), span)
}

fn check_print_args(sema: &mut Sema<'_>, view: &CallView, span: Span) {
    if view.args.len() != 1 {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!("print() takes exactly 1 argument, got {}", view.args.len()),
        ));
        return;
    }
    if !matches!(sema.uir.inst(view.args[0]).tag, InstTag::StrLiteral) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.args[0]),
            DiagCode::BuiltinArgKind,
            "print() argument must be a string literal",
        ));
    }
}

// Column counts unicode codepoints, not bytes — matches editor conventions.
fn byte_offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn bin_op_symbol(tag: InstTag) -> &'static str {
    match tag {
        InstTag::Add => "+",
        InstTag::Sub => "-",
        InstTag::Mul => "*",
        InstTag::Div => "/",
        InstTag::Mod => "%",
        InstTag::Eq => "==",
        InstTag::NotEq => "!=",
        InstTag::Lt => "<",
        InstTag::Gt => ">",
        InstTag::LtEq => "<=",
        InstTag::GtEq => ">=",
        InstTag::And => "and",
        InstTag::Or => "or",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::astgen;
    use crate::lexer::lex;
    use crate::parser::program_parser;
    use crate::tir::{Tir, TirData};
    use chumsky::Parser;
    use chumsky::input::Input;
    use chumsky::span::{SimpleSpan, Span as _};

    fn sp() -> Span {
        SimpleSpan::new((), 0..0)
    }

    type RunOk = (Vec<Tir>, InternPool);

    /// Lex + parse + astgen + sema. Returns either the typed TIR
    /// (alongside the pool) or the diagnostics that stopped one of
    /// those stages.
    fn run(input: &str) -> Result<RunOk, Vec<Diag>> {
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).expect("lex ok");
        let token_stream = tokens[..].split_token_span((0..input.len()).into());
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let uir = astgen::generate(&program, &mut pool, &mut sink);
        if sink.has_errors() {
            return Err(sink.into_diags());
        }
        let tirs = analyze(&uir, &mut pool, &mut sink, input, "<test>");
        if sink.has_errors() {
            return Err(sink.into_diags());
        }
        Ok((tirs, pool))
    }

    /// Variant that returns TIR even when sema reported errors —
    /// used to assert the "Unreachable + diag" invariant from §4.5.
    fn run_with_errors(input: &str) -> (Vec<Tir>, Vec<Diag>, InternPool) {
        let mut pool = InternPool::new();
        let tokens = lex(input, &mut pool).expect("lex ok");
        let token_stream = tokens[..].split_token_span((0..input.len()).into());
        let program = program_parser()
            .parse(token_stream)
            .into_result()
            .expect("parse ok");

        let mut sink = DiagSink::new();
        let uir = astgen::generate(&program, &mut pool, &mut sink);
        let tirs = analyze(&uir, &mut pool, &mut sink, input, "<test>");
        (tirs, sink.into_diags(), pool)
    }

    fn first_msg(diags: &[Diag]) -> &str {
        &diags[0].message
    }

    fn any_code(diags: &[Diag], code: DiagCode) -> bool {
        diags.iter().any(|d| d.code == code)
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
    fn string_equality_rejected() {
        let diags = run("x = \"a\" == \"b\"").unwrap_err();
        assert!(any_code(&diags, DiagCode::UnsupportedOperator));
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
    fn print_with_non_literal_rejected_in_sema() {
        let diags = run("x = \"hi\"\nprint(x)").unwrap_err();
        assert!(any_code(&diags, DiagCode::BuiltinArgKind));
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

        let mut b = crate::uir::UirBuilder::new();
        let zero = b.int_literal(0, sp());
        let ret = b.unary(InstTag::Return, zero, sp());
        b.add_function(main_id, vec![], pool.int(), &[ret], sp());
        let uir = b.finish();

        let mut sema = Sema::new(&uir, &mut pool, &mut sink, "test", "<test>");
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
}
