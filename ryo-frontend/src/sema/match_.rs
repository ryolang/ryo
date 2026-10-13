//! `match` expression analysis (M12) — pattern resolution, the
//! E0129-E0136 diagnostic family, exhaustiveness checking, and typed
//! TIR `Match` emission. Split from `expr.rs` so the parent file stays
//! under the 2000-line CI gate.
//!
//! Analysis runs in three passes. RESOLUTION validates the scrutinee
//! type (E0136) and resolves each arm's pattern against it — literal
//! kinds (E0131), variant names and payload shapes (E0132, reusing the
//! M11 construction checks E0120/E0121/E0122/E0125), anonymous-struct
//! shapes (reusing the M10 destructure checks E0113/E0114/E0040) — and
//! collects the per-arm results in a [`MatchAnalysis`] scratch struct
//! for the follow-on passes.
//!
//! The second pass adds EXHAUSTIVENESS + REDUNDANCY over the resolved arms:
//! enum scrutinees cover variant indices (a `u128` bitset up to 128
//! variants, a `Vec<bool>` fallback above), bool covers its two
//! literals, int/str/anon-struct scrutinees are uncoverable without a
//! wildcard arm (anon patterns carry no M12 discriminant — structural
//! lowering is M12.5). Incomplete coverage without a `_:` arm is
//! E0129, listing the missing variants in declaration order (or the
//! kind name); every arm after a wildcard, and every duplicate
//! discriminant before it, is W0005 (warning severity, buffered like
//! W0002/W0003 so it never piles onto an error).
//!
//! The third pass adds ARM TYPING and the typed emission: each cleanly
//! resolved arm is typed in its own child `Scope` seeded with the
//! payload bindings (E0133 gates by-move binds out of borrowed
//! scrutinees; the duplicate/reserved binding checks mirror M10
//! destructure insertion), the arm types are joined — every value arm
//! must agree on one `TypeId` (E0134), `never`/error arms join freely,
//! a falling statement arm is legal only when no arm produces a value
//! (else E0135) — and a clean match emits `TirTag::Match` typed as the
//! joined result, preceded by one `FieldAccess` owner token per by-move
//! binding (the convention documented on `TirBuilder::match_expr`).
//! Anon-pattern arms encode `Discriminant::Wildcard` as a coverage
//! placeholder (structural lowering is M12.5) and carry their literal
//! field constraints on the wire as a per-arm GUARD list so codegen
//! can evaluate them. An errored match recovers with the error-typed
//! `Unreachable` sentinel (§4.5; codegen never runs on a unit with
//! errors).
//!
//! ## Span anchoring
//!
//! The UIR match wire carries no per-pattern or per-arm spans (deferred
//! — see I-212; the AST pattern spans die in astgen). Pattern-
//! level diagnostics therefore anchor on the arm's BODY span — the
//! first body instruction's span, the nearest per-arm location
//! available — reconstructed by [`arm_span`]; scrutinee diagnostics
//! anchor on the scrutinee expression. If a future wire revision
//! carries pattern spans, re-anchor here.
//!
//! ## Continuity
//!
//! One broken arm never suppresses another: every arm is resolved
//! independently, and an arm that errored records `had_error` so
//! exhaustiveness excludes it instead of piling derived diagnostics on
//! top (E0129 still fires when the remaining, clean arms leave gaps).
//! W0005 likewise never suppresses E0129 or any other diagnostic. The
//! same holds into arm typing: a resolution-broken arm is skipped for
//! typing (its own diagnostic already fired), while its siblings still
//! type-check, and one E0133 bind never blocks the next arm's binds.

use super::{
    FuncCtx, Scope, Sema, analyze_block, analyze_expr, check_reserved_name, field_list, ordinal,
    with_struct_shape_notes,
};
use ryo_core::ast::LitPattern;
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{Discriminant, MatchBinding, TirRef, TypedMatchArm, TypedMatchBody};
use ryo_core::types::{StringId, StructField, TypeId, TypeKind, VariantKind};
use ryo_core::uir::{
    InstData, InstRef, InstTag, Span, UirAnonFieldsView, UirArgTarget, UirArmBodyView,
    UirFieldTarget, UirMatchArmView, UirPatternView, UirVariantArgsView,
};
use std::collections::HashMap;

/// Resolution + exhaustiveness results for one `match` expression.
/// Computed by the resolution pass and the [`check_exhaustiveness`]
/// walk; consumed by [`type_and_emit_match`], which seeds the
/// per-arm child scopes from [`MatchArmAnalysis::bindings`] and emits
/// the typed TIR `Match` from the whole analysis. The exhaustiveness
/// facts annotate the record for the follow-on tasks — the typed
/// emission reads only the resolution payloads — which is why they
/// carry a targeted dead-code allow below. `had_error` is the shared
/// error flag: set by resolution, grown by typing.
#[allow(dead_code)]
struct MatchAnalysis {
    /// True when resolution already failed (the scrutinee's own
    /// diagnostic or any arm's pattern error) — the match recovers
    /// error-typed no matter how typing goes.
    had_error: bool,
    /// The analyzed scrutinee operand.
    scrutinee: TirRef,
    /// Whether the scrutinee is borrowed — an explicit `&expr`
    /// or a read of a borrow-mode parameter. Gates E0133 at bind time;
    /// Copy binds and wildcard arms stay legal on borrows.
    scrutinee_borrowed: bool,
    /// The scrutinee type arms were resolved against: the real type, or
    /// the error sentinel when E0136 fired (arms then skip their checks
    /// quietly — the original diagnostic already explains).
    scrutinee_ty: TypeId,
    arms: Vec<MatchArmAnalysis>,
    /// True when every scrutinee value is covered — all enum
    /// variants / both bool literals marked, or a `_:` arm exists.
    exhaustive: bool,
    /// True when any arm is `_` (the catch-all).
    has_wildcard: bool,
}

/// Resolution results for one arm.
struct MatchArmAnalysis {
    /// Reconstructed arm span (see the module header).
    span: Span,
    /// The resolved selector: `Some` for wildcard / literal / variant
    /// arms. `None` for anonymous-shape arms (no M12 discriminant
    /// exists — structural lowering is M12.5) and for arms whose
    /// pattern errored. The coverage walk counts only `Some`
    /// discriminants, so both exclusions fall out naturally.
    discriminant: Option<Discriminant>,
    /// Source-faithful rendering of the pattern (see [`render_pattern`])
    /// — quoted in W0005's message, since the wire carries no pattern
    /// spans to slice instead.
    pattern_text: String,
    /// Payload bindings in arm-written order: canonical field index,
    /// bound name, field type. Literal and wildcard targets bind
    /// nothing. Arm typing inserts these into the arm scope.
    bindings: Vec<MatchBinding>,
    /// Anonymous-shape literal constraints (`{x=0}`, `(0, _)`):
    /// (canonical field index, literal) in written order. Only
    /// meaningful for anon arms; field-equality lowering (and the wire
    /// encoding for it) is M12.5 — M12 keeps them resolution-only.
    anon_literals: Vec<(u32, LitPattern)>,
    /// True when the pattern produced a diagnostic. The match recovers
    /// error-typed when any arm (or the scrutinee) errored; arm typing
    /// skips the arm's typing so the original error doesn't cascade.
    had_error: bool,
}

/// The matchable scrutinee kinds (spec §4.6): enums by variant, exact
/// literals over `int` / `bool` / `str`, and anonymous structs by
/// shape. Named structs (nominal patterns), floats (literal patterns
/// deferred with NaN/IEEE), views, `bytes`, and `void` / `never` (no
/// value to match) are rejected with E0136.
fn is_matchable(sema: &Sema<'_>, ty: ryo_core::types::TypeId) -> bool {
    matches!(
        sema.pool.kind(ty),
        TypeKind::Enum | TypeKind::Int | TypeKind::Bool | TypeKind::Str | TypeKind::AnonStruct
    )
}

/// `match scrutinee:` (M12) — resolution, exhaustiveness, then arm
/// typing + typed `TirTag::Match` emission; see the module header for
/// the split of responsibilities.
pub(crate) fn analyze_match(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    r: InstRef,
    span: Span,
) -> TirRef {
    // Copy the shared UIR reference out of `sema` so the decoded view
    // survives the `&mut Sema` calls below (the UIR arena is immutable
    // for the whole analysis).
    let uir = sema.uir;
    let view = uir.match_view(r);
    let (scrutinee, scrutinee_borrowed) = analyze_scrutinee(sema, fcx, scope, view.scrutinee);
    // E0129's secondary underline when the scrutinee names an enum.
    let scrutinee_span = uir.span(view.scrutinee);
    let mut scrutinee_ty = fcx.builder.ty_of(scrutinee);

    // E0136: the scrutinee must be a matchable type. An error-typed
    // scrutinee (its own diagnostic already emitted — e.g. an undefined
    // name) recovers quietly, matching the struct-literal / enum-lit
    // failed-type precedent; everything else names the type and lists
    // what IS matchable, then resolution continues against the error
    // sentinel so later arms still get shape-checked where possible.
    let mut had_error = false;
    if !is_matchable(sema, scrutinee_ty) {
        if !sema.pool.is_error(scrutinee_ty) {
            let mut diag = Diag::error(
                scrutinee_span,
                DiagCode::InvalidMatchScrutinee,
                format!(
                    "cannot match on a value of type '{}' — match works on \
                     enums, int, bool, str, and anonymous structs",
                    sema.pool.display(scrutinee_ty),
                ),
            );
            if sema.pool.kind(scrutinee_ty) == TypeKind::Float {
                diag = diag.with_help("float literal patterns are not yet supported");
            }
            sema.sink.emit(diag);
        }
        had_error = true;
        scrutinee_ty = sema.pool.error_type();
    }

    let mut arms = Vec::with_capacity(view.arms.len());
    for arm in &view.arms {
        let arm_span = arm_span(sema, &arm.body, span);
        let analysis = resolve_arm(sema, scrutinee_ty, arm, arm_span);
        had_error |= analysis.had_error;
        arms.push(analysis);
    }

    // Exhaustiveness + redundancy over the resolved arms. An
    // error-typed scrutinee (E0136 recovery or an already-errored
    // expression) skips the walk: E0129 on top of the original
    // diagnostic would double-report.
    let exhaustiveness = if sema.pool.is_error(scrutinee_ty) {
        Exhaustiveness::default()
    } else {
        check_exhaustiveness(sema, scrutinee_ty, &arms, span, scrutinee_span)
    };

    type_and_emit_match(
        sema,
        fcx,
        scope,
        &view.arms,
        MatchAnalysis {
            had_error,
            scrutinee,
            scrutinee_borrowed,
            scrutinee_ty,
            arms,
            exhaustive: exhaustiveness.exhaustive,
            has_wildcard: exhaustiveness.has_wildcard,
        },
        span,
    )
}

/// Reconstruct an arm's span from its body (the UIR wire carries no
/// arm/pattern spans — see the module header).
fn arm_span(sema: &Sema<'_>, body: &UirArmBodyView<'_>, match_span: Span) -> Span {
    match body {
        UirArmBodyView::Expr(r) => sema.uir.span(*r),
        UirArmBodyView::Stmts(words) => words
            .first()
            .map(|&w| sema.uir.span(InstRef::from_raw(w)))
            .unwrap_or(match_span),
    }
}

// ---------- Arm typing + typed TIR emission ----------

/// Analyze the scrutinee expression, detecting whether the scrutinee is
/// BORROWED — E0133's gate. Three shapes: an explicit `&expr` scrutinee
/// (`match &o:`, M8.2 — a legal borrow position; the match inspects the
/// value without consuming it), a bare read of a parameter declared in
/// the default borrow mode, and a field projection chain rooted at such
/// a parameter (`match w.inner:` — the projection borrows with its
/// root). The `&` marker lowers away, exactly like `&` at an `inout`
/// call argument: the TIR operand is the inner value.
fn analyze_scrutinee(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    scrutinee: InstRef,
) -> (TirRef, bool) {
    let inst = sema.uir.inst(scrutinee);
    if inst.tag == InstTag::Borrow {
        let inner = match inst.data {
            InstData::Borrow(inner) => inner,
            _ => unreachable!("Borrow must carry InstData::Borrow"),
        };
        return (analyze_expr(sema, fcx, scope, inner), true);
    }
    let borrowed = match inst.data {
        InstData::Var(name) => scope.lookup_borrowed(name),
        // Walk `a.b.c` down to its root: a field projection of a
        // borrowed parameter (`match w.inner:`) is borrowed exactly
        // like a bare `w` read — a by-move bind out of it is E0133.
        InstData::FieldAccess { .. } => {
            let mut root = scrutinee;
            while let InstData::FieldAccess { object, .. } = sema.uir.inst(root).data {
                root = object;
            }
            matches!(sema.uir.inst(root).data, InstData::Var(name) if scope.lookup_borrowed(name))
        }
        _ => false,
    };
    (analyze_expr(sema, fcx, scope, scrutinee), borrowed)
}

/// One arm's typed body before it is handed to [`TirBuilder::match_expr`].
enum TypedArmBody {
    Expr(TirRef),
    Stmts(Vec<TirRef>),
}

/// How one arm participates in the result-type join (see
/// [`type_and_emit_match`] for the rules).
enum ArmClass {
    /// Yields a value of the given type (never `void` / `never` /
    /// error) — the join-candidate pool.
    Value(TypeId),
    /// A void-producing expression arm — participates as `void`.
    VoidExpr,
    /// A statement arm that can fall through — the E0135 shape; not a
    /// join participant.
    FallingStmt,
    /// Diverges (`never`) or errored — joins freely.
    Diverges,
}

/// One fully typed arm: the emission payload plus its join class.
struct TypedArm {
    discriminant: Discriminant,
    bindings: Vec<MatchBinding>,
    /// Anon-arm literal guards (`{x=0}` constraints), in written order —
    /// The resolution payloads, forwarded to the wire verbatim. Empty
    /// for wildcard / literal / variant arms.
    guards: Vec<(u32, LitPattern)>,
    body: TypedArmBody,
    /// Arm span (the R11 body re-anchor) — the E0134/E0135 anchor.
    span: Span,
    class: ArmClass,
}

/// Type every cleanly-resolved arm in its own child `Scope`
/// seeded with the payload bindings, join the arm types, and emit the
/// typed `TirTag::Match`. On any error — resolution, E0133-E0135, or
/// the scrutinee's own diagnostic — the match recovers with the
/// error-typed `Unreachable` sentinel (§4.5), so no partially-typed
/// `Match` ever reaches codegen.
///
/// Join rule (spec §4.6 "all arms must have same return type"): every
/// value arm must carry the identical `TypeId` — the first value arm's
/// type is the candidate, and every later arm whose type differs
/// (including a void-producing expression arm in a value match) is one
/// E0134, primary span on the FIRST offending arm body, a note per
/// other offender. `never` and error-typed arms join freely. A
/// statement arm that falls through is legal only when NO arm produces
/// a value (the match is `void`) — against a value candidate it is
/// E0135 at that arm. The match type is the candidate, `never` when
/// every arm is `never`/errored, and `void` otherwise.
///
/// Emission (clean path): one `FieldAccess` owner token per BY-MOVE
/// binding (`!pool.is_copy(binding.ty)`), in arm order then binding
/// order, immediately before the `Match` — the convention documented
/// on `TirBuilder::match_expr` and recovered by
/// [`Tir::match_by_move_owner_refs`]. Copy bindings copy out of the
/// scrutinee and skip the token. Anon-pattern arms encode
/// `Discriminant::Wildcard` as a coverage placeholder (structural
/// lowering is M12.5) and carry their literal field constraints as the
/// per-arm GUARD list, in written order — empty for every other arm;
/// the two encodings are orthogonal by design.
fn type_and_emit_match(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    uir_arms: &[UirMatchArmView<'_>],
    mut analysis: MatchAnalysis,
    span: Span,
) -> TirRef {
    let void = sema.pool.void();
    let mut arms_out: Vec<TypedArm> = Vec::with_capacity(analysis.arms.len());
    let mut owner_tokens: Vec<(u32, TypeId)> = Vec::new();

    // The UIR arm bodies (borrowed from the immutable UIR arena, kept
    // across the `&mut Sema` body analyses below) walk in lockstep with
    // the resolution results — both derive from the same wire order.
    debug_assert_eq!(
        uir_arms.len(),
        analysis.arms.len(),
        "resolution and typing walk the same arms"
    );

    for (arm, uir_arm) in analysis.arms.iter().zip(uir_arms) {
        // A resolution-broken arm carries its own diagnostic and its
        // shape is untrustworthy — skip its typing so nothing piles on
        // (it never reaches the wire: the match is error-typed).
        if arm.had_error {
            continue;
        }
        // The arm's bindings live in a child scope and die with it:
        // visible inside the arm, unresolvable after the match, free to
        // collide with names in sibling arms.
        let mut child_scope = Scope {
            parent: Some(scope),
            bindings: HashMap::new(),
        };
        let (owners, binding_error) = seed_arm_bindings(
            sema,
            &mut child_scope,
            &arm.bindings,
            analysis.scrutinee_borrowed,
            arm.span,
        );
        analysis.had_error |= binding_error;
        owner_tokens.extend(owners);

        let (body, class) = match uir_arm.body {
            UirArmBodyView::Expr(body_ref) => {
                let body_tir = analyze_expr(sema, fcx, &child_scope, body_ref);
                let ty = fcx.builder.ty_of(body_tir);
                let class = if sema.pool.is_error(ty) || sema.pool.is_never(ty) {
                    ArmClass::Diverges
                } else if ty == void {
                    ArmClass::VoidExpr
                } else {
                    ArmClass::Value(ty)
                };
                (TypedArmBody::Expr(body_tir), class)
            }
            UirArmBodyView::Stmts(_) => {
                let stmt_refs: Vec<InstRef> = uir_arm.body.stmts().collect();
                let tirs = analyze_block(sema, fcx, &child_scope, &stmt_refs);
                // The arm-termination sense (R13): a statement arm ending
                // in `return`/`break`/`continue` is `never` at the join;
                // only a body that can really fall through is the E0135
                // shape.
                let terminates = fcx.builder.block_definitely_terminates(&tirs, sema.pool);
                let class = if terminates {
                    ArmClass::Diverges
                } else {
                    ArmClass::FallingStmt
                };
                (TypedArmBody::Stmts(tirs), class)
            }
        };

        arms_out.push(TypedArm {
            discriminant: match arm.discriminant {
                Some(d) => d,
                None => Discriminant::Wildcard,
            },
            bindings: arm.bindings.clone(),
            guards: arm.anon_literals.clone(),
            body,
            span: arm.span,
            class,
        });
    }

    // ---- Join ----
    let candidate = arms_out.iter().find_map(|arm| match arm.class {
        ArmClass::Value(ty) => Some(ty),
        _ => None,
    });
    if let Some(expected) = candidate {
        let mut offenders: Vec<(Span, TypeId)> = Vec::new();
        for arm in &arms_out {
            let bad = match arm.class {
                ArmClass::Value(ty) if ty != expected => Some(ty),
                ArmClass::VoidExpr => Some(void),
                _ => None,
            };
            if let Some(ty) = bad {
                offenders.push((arm.span, ty));
            }
        }
        if !offenders.is_empty() {
            let (primary, first_ty) = offenders[0];
            let mut diag = with_struct_shape_notes(
                Diag::error(
                    primary,
                    DiagCode::ArmTypeMismatch,
                    format!(
                        "match arm type mismatch — expected '{}', found '{}'",
                        sema.pool.display(expected),
                        sema.pool.display(first_ty),
                    ),
                ),
                sema.pool,
                expected,
                first_ty,
            );
            for (note_span, ty) in &offenders[1..] {
                diag = diag.with_note(
                    Some(*note_span),
                    format!("this arm yields '{}'", sema.pool.display(*ty)),
                );
            }
            sema.sink.emit(diag);
            analysis.had_error = true;
        }
        for arm in &arms_out {
            if matches!(arm.class, ArmClass::FallingStmt) {
                sema.sink.emit(
                    Diag::error(
                        arm.span,
                        DiagCode::ArmNoValue,
                        format!(
                            "match arm falls through without producing a value — \
                             this match must yield '{}'",
                            sema.pool.display(expected),
                        ),
                    )
                    .with_help("end the arm with a value, or terminate it with `return`, `break`, or `continue`"),
                );
                analysis.had_error = true;
            }
        }
    }

    let match_ty = match candidate {
        Some(expected) => expected,
        None => {
            let all_diverge = !arms_out.is_empty()
                && arms_out
                    .iter()
                    .all(|arm| matches!(arm.class, ArmClass::Diverges));
            if all_diverge { sema.pool.never() } else { void }
        }
    };

    if analysis.had_error {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    // ---- Emission: owner tokens, then the Match itself ----
    for (field_index, ty) in &owner_tokens {
        fcx.builder
            .field_access(analysis.scrutinee, *field_index, *ty, span);
    }
    let typed_arms: Vec<TypedMatchArm<'_>> = arms_out
        .iter()
        .map(|arm| TypedMatchArm {
            discriminant: arm.discriminant,
            bindings: &arm.bindings,
            guards: &arm.guards,
            body: match &arm.body {
                TypedArmBody::Expr(body) => TypedMatchBody::Expr(*body),
                TypedArmBody::Stmts(stmts) => TypedMatchBody::Stmts(stmts),
            },
        })
        .collect();
    fcx.builder
        .match_expr(analysis.scrutinee, match_ty, &typed_arms, span)
}

/// Insert one arm's payload bindings into its child scope, mirroring
/// `analyze_destructure`'s insertion checks — the same-name collision
/// (DuplicateDeclaration) and reserved-name rejection — plus the E0133
/// gate: a non-`Copy` bind out of a BORROWED scrutinee would move out of
/// the borrow, so it is refused and bound error-typed instead (the arm
/// body still checks against the error type). Returns the by-move owner
/// tokens `(field_index, ty)` in binding order — Copy binds copy out of
/// the scrutinee and need no token.
fn seed_arm_bindings(
    sema: &mut Sema<'_>,
    child_scope: &mut Scope,
    bindings: &[MatchBinding],
    borrowed: bool,
    span: Span,
) -> (Vec<(u32, TypeId)>, bool) {
    let mut owners = Vec::new();
    let mut had_error = false;
    for binding in bindings {
        let name = binding.name;
        if child_scope.contains_in_current(name) {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::DuplicateDeclaration,
                format!(
                    "'{}' is already declared in this scope",
                    sema.pool.str(name),
                ),
            ));
            child_scope.insert_binding(name, sema.pool.error_type(), false, false);
            had_error = true;
            continue;
        }
        if check_reserved_name(
            sema,
            name,
            span,
            "is a reserved builtin and cannot be redefined",
        ) {
            child_scope.insert_binding(name, sema.pool.error_type(), false, false);
            had_error = true;
            continue;
        }
        if borrowed && !sema.pool.is_copy(binding.ty) {
            sema.sink.emit(
                Diag::error(
                    span,
                    DiagCode::MoveOutOfBorrowedScrutinee,
                    format!(
                        "cannot move out of a borrowed match scrutinee — \
                         binding '{}' has non-Copy type '{}'",
                        sema.pool.str(name),
                        sema.pool.display(binding.ty),
                    ),
                )
                .with_help(
                    "match on an owned value instead, or bind '_' and read \
                     the field through a helper",
                ),
            );
            child_scope.insert_binding(name, sema.pool.error_type(), false, false);
            had_error = true;
            continue;
        }
        child_scope.insert_binding(name, binding.ty, false, false);
        if !sema.pool.is_copy(binding.ty) {
            owners.push((binding.field_index, binding.ty));
        }
    }
    (owners, had_error)
}

// ---------- Exhaustiveness + redundancy ----------

/// The result of the coverage walk, annotated onto [`MatchAnalysis`]
/// for arm typing.
#[derive(Default)]
struct Exhaustiveness {
    /// Every scrutinee value is covered: full variant/bool coverage, or
    /// a `_:` arm exists (a wildcard satisfies exhaustiveness even when
    /// earlier arms were redundant).
    exhaustive: bool,
    /// At least one arm is the `_` catch-all.
    has_wildcard: bool,
}

/// Bitset over an enum's variant indices: the `u128` fast path covers
/// enums with up to 128 variants without allocating; wider enums (legal
/// but pathological) fall back to a `Vec<bool>` — the threshold is
/// exactly 128, the `u128` width.
enum VariantBits {
    Small(u128),
    Wide(Vec<bool>),
}

impl VariantBits {
    fn new(variant_count: usize) -> Self {
        if variant_count <= 128 {
            VariantBits::Small(0)
        } else {
            VariantBits::Wide(vec![false; variant_count])
        }
    }

    /// Mark variant `idx` as covered; returns true when it was
    /// ALREADY covered (a duplicate discriminant).
    fn insert(&mut self, idx: usize) -> bool {
        match self {
            VariantBits::Small(bits) => {
                let bit = 1u128 << idx;
                let duplicate = *bits & bit != 0;
                *bits |= bit;
                duplicate
            }
            VariantBits::Wide(seen) => {
                let duplicate = seen[idx];
                seen[idx] = true;
                duplicate
            }
        }
    }

    /// Every variant index below `total` is covered.
    fn all_set(&self, total: usize) -> bool {
        match self {
            VariantBits::Small(bits) => (0..total).all(|i| bits >> i & 1 != 0),
            VariantBits::Wide(seen) => seen.iter().all(|&b| b),
        }
    }

    /// The uncovered variant indices below `total`, in declaration
    /// order (index order).
    fn missing(&self, total: usize) -> Vec<usize> {
        (0..total)
            .filter(|&i| match self {
                VariantBits::Small(bits) => bits >> i & 1 == 0,
                VariantBits::Wide(seen) => !seen[i],
            })
            .collect()
    }
}

/// Coverage scratch per scrutinee kind (spec §4.6). Only `Some`
/// discriminants reach a `mark` — anon shapes contribute nothing
/// (M12.5) and errored arms were skipped by the walk — so the int/str
/// sets double as pure duplicate detection: the value space itself is
/// uncoverable without a wildcard.
enum Coverage {
    /// Enum scrutinee: one bit per variant index.
    Enum { bits: VariantBits, total: usize },
    /// bool scrutinee: bit 0 = `true` seen, bit 1 = `false` seen.
    Bool(u8),
    /// int scrutinee: literal values seen so far.
    Int(Vec<i64>),
    /// str scrutinee: literal values seen so far.
    Str(Vec<StringId>),
    /// Anonymous-struct scrutinee: nothing covers a value until a
    /// wildcard arm (anon patterns carry no M12 discriminant).
    Struct,
}

impl Coverage {
    fn new(sema: &Sema<'_>, ty: ryo_core::types::TypeId) -> Coverage {
        match sema.pool.kind(ty) {
            TypeKind::Enum => {
                let total = sema.pool.enum_view(ty).variants().count();
                Coverage::Enum {
                    bits: VariantBits::new(total),
                    total,
                }
            }
            TypeKind::Bool => Coverage::Bool(0),
            TypeKind::Int => Coverage::Int(Vec::new()),
            TypeKind::Str => Coverage::Str(Vec::new()),
            TypeKind::AnonStruct => Coverage::Struct,
            // The walk is gated to matchable, non-error scrutinee
            // types by `analyze_match`.
            _ => unreachable!("coverage for a non-matchable scrutinee type"),
        }
    }

    /// Record a matched discriminant; `Some(true)` when it was already
    /// covered (redundant). Kinds always agree with the scrutinee type
    /// here — E0131 gates literal kinds in resolution and errored arms
    /// never reach the walk.
    fn mark(&mut self, discriminant: &Discriminant) -> Option<bool> {
        match (self, discriminant) {
            (Coverage::Enum { bits, .. }, Discriminant::Variant(i)) => {
                Some(bits.insert(*i as usize))
            }
            (Coverage::Bool(seen), Discriminant::Bool(b)) => {
                // Bit 0 = `true` seen, bit 1 = `false` seen (the order
                // the missing list reports them in).
                let bit = if *b { 1 } else { 2 };
                let duplicate = *seen & bit != 0;
                *seen |= bit;
                Some(duplicate)
            }
            (Coverage::Int(seen), Discriminant::Int(v)) => Some(if seen.contains(v) {
                true
            } else {
                seen.push(*v);
                false
            }),
            (Coverage::Str(seen), Discriminant::Str(s)) => Some(if seen.contains(s) {
                true
            } else {
                seen.push(*s);
                false
            }),
            _ => {
                debug_assert!(
                    false,
                    "discriminant kind matches the scrutinee type (E0131 gates)"
                );
                None
            }
        }
    }

    /// The scrutinee's value space is fully covered.
    fn is_full(&self) -> bool {
        match self {
            Coverage::Enum { bits, total } => bits.all_set(*total),
            Coverage::Bool(seen) => *seen == 0b11,
            // Literal sets can never enumerate int/str; anon patterns
            // carry no discriminant at all.
            Coverage::Int(_) | Coverage::Str(_) | Coverage::Struct => false,
        }
    }
}

/// Walk the resolved arms in order: a wildcard arm sets `has_wildcard`
/// and makes every later arm redundant (W0005); duplicate
/// discriminants before the wildcard are W0005 on the later arm. When
/// coverage is incomplete and no wildcard exists, emit E0129 listing
/// the missing variants in declaration order (or the scrutinee kind).
/// Warnings buffer through `sema.warnings` like W0002/W0003 — they
/// flush only when the whole unit is error-free.
fn check_exhaustiveness(
    sema: &mut Sema<'_>,
    scrutinee_ty: ryo_core::types::TypeId,
    arms: &[MatchArmAnalysis],
    match_span: Span,
    scrutinee_span: Span,
) -> Exhaustiveness {
    let mut coverage = Coverage::new(sema, scrutinee_ty);
    let mut has_wildcard = false;
    for arm in arms {
        // Errored arms carry their own diagnostic (E0131/E0132/…); a
        // redundancy note on top would double-report. They contribute
        // nothing to coverage either way.
        if arm.had_error {
            continue;
        }
        if has_wildcard {
            warn_unreachable(sema, arm, "a previous `_` arm matches everything");
            continue;
        }
        match arm.discriminant {
            None => {
                // Anon-shape arm: no discriminant to mark (M12.5) and
                // no wildcard before it — it simply covers nothing.
            }
            Some(Discriminant::Wildcard) => has_wildcard = true,
            Some(ref d) if coverage.mark(d) == Some(true) => {
                warn_unreachable(sema, arm, "an earlier arm matches the same value");
            }
            Some(_) => {}
        }
    }
    let exhaustive = has_wildcard || coverage.is_full();
    if !exhaustive {
        emit_non_exhaustive(sema, scrutinee_ty, &coverage, match_span, scrutinee_span);
    }
    Exhaustiveness {
        exhaustive,
        has_wildcard,
    }
}

/// One W0005 per redundant arm, anchored on the arm (see the module
/// header) with the pattern's source text quoted.
fn warn_unreachable(sema: &mut Sema<'_>, arm: &MatchArmAnalysis, reason: &str) {
    sema.warnings.push(Diag::warning(
        arm.span,
        DiagCode::UnreachableArm,
        format!(
            "unreachable match arm — pattern `{}` can never match: {reason}",
            arm.pattern_text,
        ),
    ));
}

/// E0129: coverage is incomplete and no `_:` arm exists. The message
/// lists the missing variants by name in declaration order (bool: the
/// missing `true`/`false`; int/str/anon: the kind name); the help note
/// teaches the wildcard arm. Anchored on the match expression; enum
/// scrutinees get a secondary underline on the scrutinee.
fn emit_non_exhaustive(
    sema: &mut Sema<'_>,
    scrutinee_ty: ryo_core::types::TypeId,
    coverage: &Coverage,
    match_span: Span,
    scrutinee_span: Span,
) {
    let mut diag = match coverage {
        Coverage::Enum { bits, total } => {
            let names: Vec<String> = {
                let view = sema.pool.enum_view(scrutinee_ty);
                let names: Vec<StringId> = view.variants().map(|v| v.name).collect();
                bits.missing(*total)
                    .into_iter()
                    .map(|i| format!("'{}'", sema.pool.str(names[i])))
                    .collect()
            };
            let (noun, verb) = if names.len() == 1 {
                ("variant", "is")
            } else {
                ("variants", "are")
            };
            let ename = sema.pool.str(sema.pool.enum_view(scrutinee_ty).name());
            Diag::error(
                match_span,
                DiagCode::NonExhaustiveMatch,
                format!(
                    "non-exhaustive match — {noun} {} {verb} not covered",
                    names.join(", "),
                ),
            )
            .with_note(
                Some(scrutinee_span),
                format!("the scrutinee is enum '{ename}' here"),
            )
        }
        Coverage::Bool(seen) => {
            let mut names = Vec::new();
            if seen & 1 == 0 {
                names.push("'true'");
            }
            if seen & 2 == 0 {
                names.push("'false'");
            }
            let verb = if names.len() == 1 { "is" } else { "are" };
            Diag::error(
                match_span,
                DiagCode::NonExhaustiveMatch,
                format!(
                    "non-exhaustive match — {} {verb} not covered",
                    names.join(", ")
                ),
            )
        }
        Coverage::Int(_) => Diag::error(
            match_span,
            DiagCode::NonExhaustiveMatch,
            "non-exhaustive match on 'int' — literal arms cannot cover every integer value",
        ),
        Coverage::Str(_) => Diag::error(
            match_span,
            DiagCode::NonExhaustiveMatch,
            "non-exhaustive match on 'str' — literal arms cannot cover every string value",
        ),
        Coverage::Struct => Diag::error(
            match_span,
            DiagCode::NonExhaustiveMatch,
            format!(
                "non-exhaustive match on '{}' — struct patterns cannot cover every value",
                sema.pool.display(scrutinee_ty),
            ),
        ),
    };
    diag = diag.with_help("add a `_:` arm");
    sema.sink.emit(diag);
}

/// Source-faithful rendering of an arm's pattern for W0005's message.
/// The UIR wire carries no pattern spans (the module header), so the
/// quoted text is reconstructed from the resolved wire shape rather
/// than sliced from the source.
fn render_pattern(sema: &Sema<'_>, pattern: &UirPatternView<'_>) -> String {
    match pattern {
        UirPatternView::Wildcard => "_".to_string(),
        UirPatternView::Literal(lit) => render_literal(sema, *lit),
        UirPatternView::Anon { fields } => {
            let inner: Vec<String> = fields
                .fields()
                .map(|(selector, target)| {
                    let target = match target {
                        UirFieldTarget::Bind(name) => sema.pool.str(name).to_string(),
                        UirFieldTarget::Wildcard => "_".to_string(),
                        UirFieldTarget::Literal(lit) => render_literal(sema, lit),
                    };
                    if fields.positional {
                        target
                    } else {
                        format!("{}={target}", sema.pool.str(StringId::from_raw(selector)))
                    }
                })
                .collect();
            let (open, close) = if fields.positional {
                ("(", ")")
            } else {
                ("{", "}")
            };
            format!("{open}{}{close}", inner.join(", "))
        }
        UirPatternView::Variant {
            enum_name,
            variant,
            args,
        } => {
            let mut text = String::new();
            if let Some(ename) = enum_name {
                text.push_str(sema.pool.str(*ename));
                text.push('.');
            }
            text.push_str(sema.pool.str(*variant));
            if let Some(args) = args {
                let render_target = |target: UirArgTarget| match target {
                    UirArgTarget::Bind(name) => sema.pool.str(name).to_string(),
                    UirArgTarget::Wildcard => "_".to_string(),
                };
                if args.by_position {
                    let inner: Vec<String> = args.args().map(|(_, t)| render_target(t)).collect();
                    text.push('(');
                    text.push_str(&inner.join(", "));
                    text.push(')');
                } else {
                    let inner: Vec<String> = args
                        .args()
                        .map(|(name, t)| match name {
                            Some(name) => {
                                format!("{}={}", sema.pool.str(name), render_target(t))
                            }
                            None => render_target(t),
                        })
                        .collect();
                    text.push('{');
                    text.push_str(&inner.join(", "));
                    text.push('}');
                }
            }
            text
        }
    }
}

/// One literal's source spelling: decimal int (negatives arrive
/// pre-folded from the parser), quoted str, bare bool.
fn render_literal(sema: &Sema<'_>, lit: LitPattern) -> String {
    match lit {
        LitPattern::Int(v) => v.to_string(),
        LitPattern::Str(s) => format!("\"{}\"", sema.pool.str(s)),
        LitPattern::Bool(b) => if b { "true" } else { "false" }.to_string(),
    }
}

/// Resolve one arm's pattern against the scrutinee type. Every kind
/// resolves independently — a broken arm never blocks the next.
fn resolve_arm(
    sema: &mut Sema<'_>,
    scrutinee_ty: ryo_core::types::TypeId,
    arm: &UirMatchArmView<'_>,
    span: Span,
) -> MatchArmAnalysis {
    let mut analysis = MatchArmAnalysis {
        span,
        discriminant: None,
        pattern_text: render_pattern(sema, &arm.pattern),
        bindings: Vec::new(),
        anon_literals: Vec::new(),
        had_error: false,
    };
    // An error-typed scrutinee (E0136 recovery or an already-errored
    // expression) makes every check uninformative — the arm is excluded
    // from exhaustiveness and binds nothing.
    if sema.pool.is_error(scrutinee_ty) {
        analysis.had_error = true;
        return analysis;
    }
    match &arm.pattern {
        UirPatternView::Wildcard => {
            analysis.discriminant = Some(Discriminant::Wildcard);
        }
        UirPatternView::Literal(lit) => {
            resolve_literal(sema, scrutinee_ty, *lit, &mut analysis);
        }
        UirPatternView::Variant {
            enum_name,
            variant,
            args,
        } => {
            resolve_variant(
                sema,
                scrutinee_ty,
                *enum_name,
                *variant,
                *args,
                &mut analysis,
            );
        }
        UirPatternView::Anon { fields } => {
            resolve_anon(sema, scrutinee_ty, *fields, &mut analysis);
        }
    }
    analysis
}

/// The literal's expected type and a human-readable kind name, for the
/// E0131 both-sides message.
fn literal_kind(sema: &Sema<'_>, lit: LitPattern) -> (ryo_core::types::TypeId, &'static str) {
    match lit {
        LitPattern::Int(_) => (sema.pool.int(), "integer"),
        LitPattern::Str(_) => (sema.pool.str_(), "string"),
        LitPattern::Bool(_) => (sema.pool.bool_(), "boolean"),
    }
}

/// A literal arm: the literal's kind must equal the scrutinee type
/// (int/bool/str scrutinees only — enums and anon shapes were gated by
/// the pattern-kind checks in [`resolve_variant`] / [`resolve_anon`]).
fn resolve_literal(
    sema: &mut Sema<'_>,
    scrutinee_ty: ryo_core::types::TypeId,
    lit: LitPattern,
    analysis: &mut MatchArmAnalysis,
) {
    let (lit_ty, kind) = literal_kind(sema, lit);
    if scrutinee_ty != lit_ty {
        let mut diag = Diag::error(
            analysis.span,
            DiagCode::PatternTypeMismatch,
            format!(
                "{kind} literal pattern cannot match a scrutinee of type '{}'",
                sema.pool.display(scrutinee_ty),
            ),
        );
        if sema.pool.kind(scrutinee_ty) == TypeKind::Enum {
            let ename = sema.pool.enum_view(scrutinee_ty).name();
            diag = diag.with_help(format!(
                "enum '{}' values match by variant name",
                sema.pool.str(ename),
            ));
        }
        sema.sink.emit(diag);
        analysis.had_error = true;
        return;
    }
    analysis.discriminant = Some(match lit {
        LitPattern::Int(v) => Discriminant::Int(v),
        LitPattern::Str(s) => Discriminant::Str(s),
        LitPattern::Bool(b) => Discriminant::Bool(b),
    });
}

/// A variant arm `Red` / `Color.Red` / `Circle(radius)` /
/// `Coords{x=px}`: resolve the enum (unqualified names against the
/// SCRUTINEE enum only — never the scope), the variant, and the
/// payload shape. Payload checks mirror [`super::analyze_variant_construct`]
/// (M11): full coverage of the payload, `_` to skip a slot, the Brace
/// Law's one-way-per-shape relaxed on the pattern side — positional
/// binds over named payloads bind in declaration order (spec §4.6).
fn resolve_variant(
    sema: &mut Sema<'_>,
    scrutinee_ty: ryo_core::types::TypeId,
    enum_name: Option<StringId>,
    variant: StringId,
    args: Option<UirVariantArgsView<'_>>,
    analysis: &mut MatchArmAnalysis,
) {
    if sema.pool.kind(scrutinee_ty) != TypeKind::Enum {
        sema.sink.emit(Diag::error(
            analysis.span,
            DiagCode::PatternTypeMismatch,
            format!(
                "variant pattern cannot match a scrutinee of type '{}' — \
                 variant patterns match enum values",
                sema.pool.display(scrutinee_ty),
            ),
        ));
        analysis.had_error = true;
        return;
    }
    if let Some(name) = enum_name {
        match sema.enum_types.get(&name) {
            Some(&ety) if ety == scrutinee_ty => {}
            Some(&ety) => {
                // Qualified path names a real enum that isn't the
                // scrutinee's — name both enums (E0132).
                let pattern_en = sema.pool.str(sema.pool.enum_view(ety).name()).to_string();
                let scrutinee_en = sema
                    .pool
                    .str(sema.pool.enum_view(scrutinee_ty).name())
                    .to_string();
                sema.sink.emit(Diag::error(
                    analysis.span,
                    DiagCode::UnknownMatchVariant,
                    format!(
                        "variant pattern '{pattern_en}.{v}' names enum '{pattern_en}', \
                         but the scrutinee is enum '{scrutinee_en}'",
                        v = sema.pool.str(variant),
                    ),
                ));
                analysis.had_error = true;
                return;
            }
            None => {
                // Never-declared (UnknownType, mirroring M11
                // construction's undeclared-enum handling) or a struct
                // name (UnknownEnum) — not E0132, which is reserved
                // for declared-but-different enums.
                if sema.struct_types.contains_key(&name) {
                    sema.sink.emit(Diag::error(
                        analysis.span,
                        DiagCode::UnknownEnum,
                        format!("'{}' is a struct, not an enum", sema.pool.str(name),),
                    ));
                } else {
                    sema.sink.emit(Diag::error(
                        analysis.span,
                        DiagCode::UnknownType,
                        format!("unknown type: '{}' in match pattern", sema.pool.str(name)),
                    ));
                }
                analysis.had_error = true;
                return;
            }
        }
    }

    // Decode the variant directory up front: the view borrows the pool
    // and the checks below need `&mut Sema` (the construction-site
    // precedent in analyze_variant_construct).
    let (ename, variants) = {
        let eview = sema.pool.enum_view(scrutinee_ty);
        (
            eview.name(),
            eview
                .variants()
                .map(|v| (v.name, v.kind, v.fields.to_vec()))
                .collect::<Vec<_>>(),
        )
    };
    let Some(vidx) = variants.iter().position(|&(n, _, _)| n == variant) else {
        emit_unknown_variant(sema, ename, variant, &variants, analysis);
        return;
    };
    let (vname, kind, fields) = &variants[vidx];
    let vname = sema.pool.str(*vname).to_string();
    let ename = sema.pool.str(ename).to_string();

    let payload_ok = match kind {
        VariantKind::Unit => {
            if args.is_some() {
                sema.sink.emit(Diag::error(
                    analysis.span,
                    DiagCode::UnitVariantConstructed,
                    format!("'{vname}' is a unit variant — match it as {ename}.{vname}"),
                ));
                false
            } else {
                true
            }
        }
        VariantKind::Tuple => resolve_tuple_payload(sema, &ename, &vname, fields, args, analysis),
        VariantKind::Named => resolve_named_payload(sema, &ename, &vname, fields, args, analysis),
    };
    if payload_ok {
        analysis.discriminant = Some(Discriminant::Variant(vidx as u32));
    } else {
        analysis.had_error = true;
    }
}

/// E0132 for an unresolved variant name: names the scrutinee enum and
/// the identifier, with a did-you-mean help note over the enum's
/// variant list when a close name exists and a plain variant-list note
/// otherwise.
fn emit_unknown_variant(
    sema: &mut Sema<'_>,
    ename: StringId,
    variant: StringId,
    variants: &[(StringId, VariantKind, Vec<StructField>)],
    analysis: &mut MatchArmAnalysis,
) {
    let ename = sema.pool.str(ename).to_string();
    let vname = sema.pool.str(variant).to_string();
    let mut diag = Diag::error(
        analysis.span,
        DiagCode::UnknownMatchVariant,
        format!("enum '{ename}' has no variant '{vname}'"),
    );
    let names: Vec<&str> = variants.iter().map(|&(n, _, _)| sema.pool.str(n)).collect();
    if let Some(suggestion) = did_you_mean(&vname, &names) {
        diag = diag.with_help(format!("did you mean '{suggestion}'?"));
    } else {
        let listed = names
            .iter()
            .map(|n| format!("'{n}'"))
            .collect::<Vec<_>>()
            .join(", ");
        diag = diag.with_note(None, format!("enum '{ename}' declares: {listed}"));
    }
    sema.sink.emit(diag);
    analysis.had_error = true;
}

/// Positional payload args on a tuple variant (`Circle(radius, _)`):
/// exact arity — overflow names the offending position (E0122,
/// mirroring construction), underflow names the counts (E0120).
/// A brace form on a tuple variant names the user field per arg
/// (E0122) — tuple payloads have no user-spellable field names.
fn resolve_tuple_payload(
    sema: &mut Sema<'_>,
    ename: &str,
    vname: &str,
    fields: &[StructField],
    args: Option<UirVariantArgsView<'_>>,
    analysis: &mut MatchArmAnalysis,
) -> bool {
    let total = fields.len();
    let Some(args) = args else {
        sema.sink.emit(
            Diag::error(
                analysis.span,
                DiagCode::MissingVariantFields,
                format!(
                    "'{vname}' takes {} positional argument{}; got 0",
                    total,
                    if total == 1 { "" } else { "s" },
                ),
            )
            .with_help(format!(
                "write {ename}.{vname}({}) to skip fields you don't need",
                vec!["_"; total].join(", "),
            )),
        );
        return false;
    };
    if args.by_position {
        if args.len() > total {
            // Extra positional argument: name its position, not the
            // synthesized "0"/"1" field name (E0122, the construction
            // wording verbatim).
            for (i, _) in args.args().enumerate().skip(total) {
                sema.sink.emit(Diag::error(
                    analysis.span,
                    DiagCode::UnknownVariantField,
                    format!(
                        "found a {} positional argument — '{vname}' takes {total}",
                        ordinal(i + 1),
                    ),
                ));
            }
            return false;
        }
        if args.len() < total {
            sema.sink.emit(
                Diag::error(
                    analysis.span,
                    DiagCode::MissingVariantFields,
                    format!(
                        "'{vname}' takes {} positional argument{}; got {}",
                        total,
                        if total == 1 { "" } else { "s" },
                        args.len(),
                    ),
                )
                .with_help(format!(
                    "write {ename}.{vname}({}) to skip fields you don't need",
                    vec!["_"; total].join(", "),
                )),
            );
            return false;
        }
        for (i, (_, target)) in args.args().enumerate() {
            if let UirArgTarget::Bind(name) = target {
                analysis.bindings.push(MatchBinding {
                    field_index: fields[i].idx,
                    name,
                    ty: fields[i].ty,
                });
            }
        }
        return true;
    }
    // Brace form on a tuple variant: no user-spellable field exists.
    let mut ok = true;
    if args.is_empty() {
        sema.sink.emit(
            Diag::error(
                analysis.span,
                DiagCode::MissingVariantFields,
                format!(
                    "'{vname}' takes {} positional argument{}; got 0",
                    total,
                    if total == 1 { "" } else { "s" },
                ),
            )
            .with_help(format!(
                "write {ename}.{vname}({}) to skip fields you don't need",
                vec!["_"; total].join(", "),
            )),
        );
        ok = false;
    }
    for (name, _) in args.args() {
        let fname = name
            .map(|n| sema.pool.str(n).to_string())
            .unwrap_or_default();
        sema.sink.emit(
            Diag::error(
                analysis.span,
                DiagCode::UnknownVariantField,
                format!("variant '{vname}' of enum '{ename}' has no field '{fname}'"),
            )
            .with_help(format!(
                "'{vname}' is a tuple variant — use the paren form: {ename}.{vname}(...)"
            )),
        );
        ok = false;
    }
    ok
}

/// Named payload args (`Coords{x=px}` / positional-over-named
/// `Coords(a, b)`, which binds in declaration order per spec §4.6):
/// unknown fields (E0122), duplicates (E0121), and full coverage
/// (E0120, `_`-skippable) mirror the construction checks.
fn resolve_named_payload(
    sema: &mut Sema<'_>,
    ename: &str,
    vname: &str,
    fields: &[StructField],
    args: Option<UirVariantArgsView<'_>>,
    analysis: &mut MatchArmAnalysis,
) -> bool {
    let total = fields.len();
    let Some(args) = args else {
        let missing = field_list(sema.pool, fields);
        sema.sink.emit(
            Diag::error(
                analysis.span,
                DiagCode::MissingVariantFields,
                format!(
                    "missing field(s) {missing} in match pattern for variant \
                     '{vname}' of enum '{ename}'",
                ),
            )
            .with_help(format!(
                "write {ename}.{vname}{{{}}} to skip fields you don't need",
                fields
                    .iter()
                    .map(|f| format!("{}=_", sema.pool.str(f.name)))
                    .collect::<Vec<_>>()
                    .join(", "),
            )),
        );
        return false;
    };
    if args.by_position {
        if args.len() > total {
            for (i, _) in args.args().enumerate().skip(total) {
                sema.sink.emit(Diag::error(
                    analysis.span,
                    DiagCode::UnknownVariantField,
                    format!(
                        "found a {} positional argument — '{vname}' takes {total} fields",
                        ordinal(i + 1),
                    ),
                ));
            }
            return false;
        }
        if args.len() < total {
            sema.sink.emit(
                Diag::error(
                    analysis.span,
                    DiagCode::MissingVariantFields,
                    format!("'{vname}' takes {total} fields; got {}", args.len()),
                )
                .with_help(format!(
                    "write {ename}.{vname}({}) to skip fields you don't need",
                    vec!["_"; total].join(", "),
                )),
            );
            return false;
        }
        for (i, (_, target)) in args.args().enumerate() {
            if let UirArgTarget::Bind(name) = target {
                analysis.bindings.push(MatchBinding {
                    field_index: fields[i].idx,
                    name,
                    ty: fields[i].ty,
                });
            }
        }
        return true;
    }
    // Brace form: per-arg resolution against the declared fields.
    let mut covered: Vec<bool> = vec![false; total];
    // Once any field errored the coverage set is untrustworthy — the
    // derived missing-fields error is suppressed (the field_errored
    // precedent in analyze_variant_construct).
    let mut field_errored = false;
    for (name, target) in args.args() {
        let Some(fname) = name else {
            // Producer invariant: brace args carry their field name.
            debug_assert!(false, "brace payload arg without a field name");
            field_errored = true;
            continue;
        };
        let Some(field) = fields.iter().find(|f| f.name == fname) else {
            sema.sink.emit(Diag::error(
                analysis.span,
                DiagCode::UnknownVariantField,
                format!(
                    "variant '{vname}' of enum '{ename}' has no field '{}' (fields: {})",
                    sema.pool.str(fname),
                    field_list(sema.pool, fields),
                ),
            ));
            field_errored = true;
            continue;
        };
        if covered[field.idx as usize] {
            sema.sink.emit(Diag::error(
                analysis.span,
                DiagCode::DuplicateVariantField,
                format!(
                    "field '{}' is specified more than once in variant '{}' of enum '{}'",
                    sema.pool.str(fname),
                    vname,
                    ename,
                ),
            ));
            field_errored = true;
            continue;
        }
        covered[field.idx as usize] = true;
        if let UirArgTarget::Bind(bind) = target {
            analysis.bindings.push(MatchBinding {
                field_index: field.idx,
                name: bind,
                ty: field.ty,
            });
        }
    }
    if field_errored {
        return false;
    }
    let missing_ids: Vec<StringId> = fields
        .iter()
        .zip(&covered)
        .filter(|(_, c)| !**c)
        .map(|(f, _)| f.name)
        .collect();
    if !missing_ids.is_empty() {
        let missing = missing_ids
            .iter()
            .map(|&n| format!("'{}'", sema.pool.str(n)))
            .collect::<Vec<_>>()
            .join(", ");
        let first = sema.pool.str(missing_ids[0]).to_string();
        sema.sink.emit(
            Diag::error(
                analysis.span,
                DiagCode::MissingVariantFields,
                format!(
                    "missing field(s) {missing} in match pattern for variant '{}' of enum '{}'",
                    vname, ename,
                ),
            )
            .with_help(format!("add '{first} = _' to skip fields you don't need")),
        );
        return false;
    }
    true
}

/// Anonymous-struct / tuple-sugar arm (`{x=0, y=_}` / `(0, _)`):
/// mirror the M10 destructure shape checks — exact positional arity
/// (E0113), known fields (E0114), no duplicates (E0040), full coverage
/// (E0114) — plus a literal-kind check (E0131) per literal field.
/// Mirrors rather than shares `analyze_destructure`: the selector
/// semantics diverge (match anon fields arrive canonicalized — pun and
/// rename collapsed to one bind case, literals ride inline — and there
/// are no compiler temps or next-uncovered pun fallbacks to handle).
fn resolve_anon(
    sema: &mut Sema<'_>,
    scrutinee_ty: ryo_core::types::TypeId,
    fields: UirAnonFieldsView<'_>,
    analysis: &mut MatchArmAnalysis,
) {
    if sema.pool.kind(scrutinee_ty) != TypeKind::AnonStruct {
        sema.sink.emit(Diag::error(
            analysis.span,
            DiagCode::PatternTypeMismatch,
            format!(
                "struct pattern cannot match a scrutinee of type '{}' — \
                 struct patterns match anonymous struct values",
                sema.pool.display(scrutinee_ty),
            ),
        ));
        analysis.had_error = true;
        return;
    }
    let shape = sema.pool.struct_view(scrutinee_ty);
    if fields.positional {
        if fields.len() != shape.fields.len() {
            let n = shape.fields.len();
            let m = fields.len();
            let detail = if m > n {
                let k = m - n;
                format!(
                    "{k} extra pattern element{} {} no field to match",
                    if k == 1 { "" } else { "s" },
                    if k == 1 { "has" } else { "have" },
                )
            } else {
                let uncovered = shape
                    .fields
                    .iter()
                    .skip(m)
                    .map(|f| format!("'{}'", sema.pool.str(f.name)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("field(s) {uncovered} not covered — write '_' or a literal")
            };
            sema.sink.emit(Diag::error(
                analysis.span,
                DiagCode::DestructureArity,
                format!(
                    "expected {n} fields, found {m} element{} — {detail}",
                    if m == 1 { "" } else { "s" },
                ),
            ));
            analysis.had_error = true;
            return;
        }
        for (i, (_, target)) in fields.fields().enumerate() {
            let field = &shape.fields[i];
            resolve_anon_target(sema, field, target, analysis);
        }
        return;
    }
    // Brace form: selectors are field-name ids; the written order is
    // the binding order.
    let mut covered: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut had_shape_error = false;
    for (selector, target) in fields.fields() {
        let name = StringId::from_raw(selector);
        let Some(field) = shape.fields.iter().find(|f| f.name == name) else {
            sema.sink.emit(Diag::error(
                analysis.span,
                DiagCode::DestructureUnknownField,
                format!(
                    "struct `{}` has no field `{}`",
                    sema.pool.display(scrutinee_ty),
                    sema.pool.str(name),
                ),
            ));
            had_shape_error = true;
            continue;
        };
        if !covered.insert(field.idx) {
            sema.sink.emit(Diag::error(
                analysis.span,
                DiagCode::DuplicateStructField,
                format!(
                    "field '{}' is specified more than once",
                    sema.pool.str(name)
                ),
            ));
            had_shape_error = true;
            continue;
        }
        resolve_anon_target(sema, field, target, analysis);
    }
    let missing_fields: Vec<&StructField> = shape
        .fields
        .iter()
        .filter(|f| !covered.contains(&f.idx))
        .collect();
    let missing: Vec<String> = missing_fields
        .iter()
        .map(|f| format!("`{}`", sema.pool.str(f.name)))
        .collect();
    if !missing.is_empty() && !had_shape_error {
        let (noun, them) = if missing.len() == 1 {
            ("field", "it")
        } else {
            ("fields", "them")
        };
        sema.sink.emit(Diag::error(
            analysis.span,
            DiagCode::DestructureUnknownField,
            format!(
                "match pattern on `{}` does not cover {noun} {} — \
                 add `{first} = _` for each to skip {them}",
                sema.pool.display(scrutinee_ty),
                missing.join(", "),
                first = sema.pool.str(missing_fields[0].name),
            ),
        ));
        had_shape_error = true;
    }
    if had_shape_error || analysis.had_error {
        analysis.had_error = true;
    }
}

/// One anon-shape field target: a bind records a payload binding, a
/// literal records a field-equality constraint (with an E0131 kind
/// check against the field's type), a wildcard binds nothing.
fn resolve_anon_target(
    sema: &mut Sema<'_>,
    field: &StructField,
    target: UirFieldTarget,
    analysis: &mut MatchArmAnalysis,
) {
    match target {
        UirFieldTarget::Bind(name) => analysis.bindings.push(MatchBinding {
            field_index: field.idx,
            name,
            ty: field.ty,
        }),
        UirFieldTarget::Wildcard => {}
        UirFieldTarget::Literal(lit) => {
            let (lit_ty, kind) = literal_kind(sema, lit);
            if lit_ty != field.ty {
                sema.sink.emit(Diag::error(
                    analysis.span,
                    DiagCode::PatternTypeMismatch,
                    format!(
                        "{kind} literal pattern cannot match field '{}' of type '{}'",
                        sema.pool.str(field.name),
                        sema.pool.display(field.ty),
                    ),
                ));
                analysis.had_error = true;
                return;
            }
            analysis.anon_literals.push((field.idx, lit));
        }
    }
}

/// The closest candidate within an edit distance of 2, if any —
/// the did-you-mean behind E0132. Identifiers here are short, so a
/// small bounded Levenshtein beats a fancier similarity metric, and no
/// new dependency is worth it for one diagnostic.
fn did_you_mean<'a>(name: &str, candidates: &[&'a str]) -> Option<&'a str> {
    candidates
        .iter()
        .copied()
        .filter(|c| edit_distance(name, c) <= 2)
        .min_by_key(|c| edit_distance(name, c))
}

/// Classic two-row Levenshtein distance over chars.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j + 1] + 1)
                .min(cur[j] + 1)
                .min(prev[j] + usize::from(ca != cb));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
