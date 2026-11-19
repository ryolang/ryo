**Status:** Design (v0.2+) — temporary migration artifact; delete after this stage lands and the durable compiler documentation is updated.

# Unresolved UIR and centralized semantic resolution

## Intent

Complete I-285 and the relevant part of I-109 in one reviewable PR. Make AstGen a structural AST-to-UIR lowering pass, make Sema the single owner of semantic resolution, and make instruction ownership explicit without replacing the shared UIR instruction arena. Preserve accepted programs, diagnostics, ownership restrictions, and execution behavior.

I-292 applies at every new boundary: unresolved UIR and resolved TIR must be inspectable, failed publication must be tested, and representative programs must execute identically before and after the migration.

## Goals

- Preserve unresolved type expressions and declaration references in UIR, including variable, parameter, return, nominal-field, variant, and error-family annotations.
- Move primitive and nominal type resolution, nominal declaration and definition, dependency checks, error-union resolution, and variant/error declaration resolution into Sema.
- Replace instruction-range guesses with an exact instruction-to-body map and dense body-local instruction slots.
- Resolve explicit signatures before bodies and finalize inferred error dependencies through an explicit worklist, never recursive body analysis.
- Move the final missing-return check from TIR visitation in Sema to the Flow boundary while retaining divergence queries needed during expression typing.
- Keep unresolved or failed artifacts available for diagnostics, but never publish them as executable backend input.

## Non-goals

- Per-function UIR instruction, extra, or span arenas.
- Target-specific physical layout or ABI changes; those belong to I-293 and I-289.
- Flow self-containment, ownership separation, cleanup optimization, inlining, generics, imports, or comptime.
- New syntax, accepted programs, diagnostic rules, ownership rules, or runtime behavior.

## Anti-goals

- Moving only parameter and return annotations while AstGen still resolves nominal declarations, enum variants, error families, or method-call fallbacks.
- Maintaining parallel semantic resolvers in AstGen and Sema.
- Deriving body ownership from arena order, maximum instruction indices, or lexical ranges.
- Replacing dense instruction side tables with per-instruction hash maps on hot paths.
- Recursively analyzing an inferred callee while its caller is suspended on the Rust call stack.
- Treating recovery TIR or a clean-looking arena shape as proof that an artifact is executable.

## Constraints

- Keep the existing program-wide UIR instruction, extra, and span arenas and their `InstRef` representation.
- Keep flat arrays, `u32` typed indices, interned text, and dense per-body vectors.
- Keep the existing `InternPool` layout computation behind Sema until the target-layout stage.
- Preserve the compilation-owned `SourceId` and `DeclId` contracts from I-284 and I-288, including source-revision invalidation and backend publication checks.
- Preserve diagnostic codes, spans, messages, and first-wins duplicate lookup behavior. Keep emission deterministic at each stage. Moving checks may change their sequence relative to other stages or bodies; compare normalized fixtures instead of adding buffering machinery solely to recreate the old cross-stage order.
- Avoid new dependencies and avoid broad rewrites of TIR, Flow, ownership, or backend code.

## Approaches

### Selected: shared arena with computed owner and local-slot map

Keep the shared instruction arena. At the Sema boundary, compute an exact owner and a dense body-local slot for every reachable instruction. Add separate flat storage for unresolved type expressions. This contains the migration around semantic resolution and preserves the existing wire/reference model.

Tradeoffs: ownership is validated rather than intrinsic to the storage type, the complete child traversal must be maintained exhaustively, and Sema must translate a global `InstRef` to its body-local slot when accessing dense maps.

### Rejected for this PR: per-function UIR arenas

Giving every function its own instruction, extra, and span arena would make cross-body references unrepresentable and simplify future independent body cloning. It would also rewrite UIR builders, views, dumps, Sema access, wire fixtures, and many tests. That storage migration should be evaluated independently after centralized resolution; combining both would obscure regressions.

### Rejected: annotation-only migration

Preserving concrete `TypeId`s in nominal side tables or resolved enum/error payloads would leave declaration environments in AstGen. It would not satisfy the purpose of I-285 and would reproduce the existing name-based resolution split.

## Representation

### Unresolved type expressions

Add a UIR-owned `TypeExprId` with the same nonzero-index discipline as `InstRef`. Store nodes and variable-length children in flat UIR side arenas:

- `Name { name, is_view }`
- `Anon` with `(field name, TypeExprId)` children
- `Positional` with ordered `TypeExprId` children
- `ErrorUnion { error_set, success, inferred }`

Every node retains its source span. Positional field names are synthesized during Sema resolution, not during AstGen.

Replace resolved annotation payloads as follows:

- `UirParam` stores a `TypeExprId`.
- `FuncBody` stores an optional return `TypeExprId`; `None` means the source omitted a return annotation and therefore denotes `void`.
- The variable-declaration wire stores an optional `TypeExprId` instead of an optional `TypeId`.
- Struct, enum, and nominal-error fields store `TypeExprId`s. Their declaration side tables contain no resolved nominal `TypeId`.
- Error-family declarations retain source member names, payload structure, visibility, attributes, and spans. Qualified names may be interned structurally, but Sema owns their declaration and type identity.

AstGen copies the AST type-expression graph into these arenas. It does not declare or define types in `InternPool`, resolve primitive or nominal names, check type cycles, or reject view-typed fields.

### Unresolved variant and error references

UIR variant construction and error-construction candidates retain declaration names and source shape, not a `TypeId`, declaration-order variant index, or payload-field index. Preserve:

- optional family/root and variant names;
- positional, named-brace, named-parenthesized, mixed-field-recovery, and unit forms;
- argument expressions in source order;
- named argument `StringId`s, with an explicit positional marker where needed.

Match patterns likewise retain their unqualified or root-qualified source names. AstGen must not rewrite error-family patterns into resolved qualified declarations.

TIR keeps its resolved enum/error representation. The currently shared enum-literal wire remains the resolved TIR wire; UIR gets its own unresolved wire and literal fixtures. Sema resolves variant and field identities and emits the existing TIR payload.

### Structural work remaining in AstGen

AstGen still flattens expressions and statements, interns source names, synthesizes compiler temporaries, preserves source forms and spans, creates implicit `main`, and reports syntactic or unsupported-structure diagnostics. Checks that inspect only source shape, such as the explicit-main/top-level-statement conflict and the unsupported `for` iterator spelling, may remain there.

## Instruction ownership and dense slots

Introduce a computed `UirBodyMap` containing:

- `Vec<Option<BodySlot>>` indexed by global `InstRef`;
- one `Vec<InstRef>` per function body in deterministic traversal order;
- `BodySlot { body: u32, local: u32 }`, where `local` indexes that body's dense instruction list.

Compute the map once from the current UIR arenas at the start of Sema. Use an iterative traversal from each body's top-level statements through every inline and wire-encoded child, including nested statements, match/catch arms, destructuring plans, variant/error arguments, and loop payloads. Adding a UIR child shape must require updating this traversal.

Rules:

- Repeated references within one body reuse the original slot and do not duplicate the instruction.
- A reference reachable from two different bodies is an invalid cross-body reference and fails Sema before declaration registration or TIR emission.
- An unreachable arena instruction has no owner and remains `None`; it is not executable and does not enlarge a body map.
- Invalid references and malformed ownership produce an internal diagnostic rather than a panic at the publication boundary.

The map is computed from the current arenas rather than cached beside publicly mutable storage, so later raw-arena edits cannot leave a stale cached owner table. A map is valid only for the UIR view from which it was computed and is never published independently.

Sema sizes each `FuncCtx.inst_map` to the body's instruction count and indexes it by `BodySlot.local`. Direct-call graph construction scans the computed per-body lists, eliminating the current contiguous-range assumption. The global map remains a dense `Vec`; no hot path uses a per-instruction `HashMap`.

## Centralized Sema resolution

Add a responsibility-owned Sema resolution module and move the existing type resolver, nominal definer, and error declaration registry out of AstGen. Adapt them to unresolved UIR accessors rather than AST accessors.

Resolution proceeds in these phases:

1. Validate the UIR body map and register function declarations with the compilation context.
2. Predeclare primitive knowledge, compiler-known errors, and nominal shells in one Sema namespace. Diagnose duplicate and cross-kind declarations with stable first-wins lookup.
3. Resolve nominal fields and define layouts with explicit pending/in-progress/defined/failed states and dependency tracking. Reject by-value cycles and view fields at their existing spans. Failed definitions remain inspectable but cannot produce executable signatures.
4. Resolve every function's parameter and return signature from UIR type expressions. Store resolved signatures by `DeclId`; bodies no longer read concrete types from UIR. Calls still bind through the first visible declaration for a name.
5. Build the direct named-call graph from exact body ownership, reject recursive components that contain inferred error unions, and schedule inferred dependencies before their callers.
6. Analyze ready bodies in deterministic order, finalize inferred error unions and deferred coercions, then publish completed TIR slots in current-UIR source order.
7. Build Flow and perform final return validation before ownership and backend publication.

Sema owns the resolved `struct_types`, `enum_types`, and `error_types` tables used by expression, pattern, and ownership typing. Anonymous structural types and error unions are interned only during Sema resolution. Annotation results may be cached in dense type-expression side tables when a node can be revisited; failure caching must preserve the original diagnostic without duplicating it.

### Variant, error, and method resolution

For an unresolved variant/error candidate, Sema uses lexical bindings and the resolved declaration namespace to select the same behavior as before:

- declared enum variant construction;
- declared nominal-error or error-family member construction;
- bare value or field access when shadowing applies;
- method-call fallback for a parenthesized candidate whose root is not an enum or error family;
- ordinary function-call or variable fallback for an unqualified unknown error-construction candidate;
- the existing unknown-type, unknown-variant, unknown-field, shadowing, and source-form diagnostics otherwise.

Named arguments are resolved to declaration-order field indices in Sema. Unknown named arguments remain reachable for expression diagnostics and use an unresolved/orphan marker until Sema emits the final diagnostic; they must not become valid field zero or trigger derived missing-field noise. All argument expressions are analyzed exactly once.

### Failure versus diagnostic recovery

Ordinary source errors in a signature or body retain the current recovery behavior: analysis continues, the declaration reaches `Resolved` with error-sentinel recovery TIR where one can be emitted, and the source-clean gate keeps that TIR diagnostic-only. An unknown or failed nominal annotation resolves to the error sentinel for this purpose; it does not by itself make the function declaration `Failed`.

Reserve `DeclState::Failed` for declarations that cannot produce an analyzable result: declaration-registration failure, members of a rejected recursive inferred component, or an internal resolution failure. Discard those declarations' provisional results. A failed inferred member unblocks its dependents instead of skipping them: callers and explicit-signature component members continue through the existing error-sentinel recovery path where current behavior emits recovery TIR. They must not repeat the original dependency diagnostic, and their recovery artifacts remain diagnostic-only.

### Explicit dependency worklist

Replace recursive `resolve_decl` calls from inferred-error analysis with dependency scheduling:

- Each declaration starts `Unresolved`, with a count of unresolved inferred-signature dependencies and a list of dependents.
- Ready declarations enter a stable queue in source order. A declaration whose inferred dependencies are unresolved is not analyzed and emits no provisional TIR.
- A ready declaration transitions `Unresolved -> InProgress -> Resolved` only after its TIR is complete and, for inferred returns, its final union and coercions are installed. Ordinary source errors use the recovery contract above and do not change this transition.
- A declaration that meets the narrow `Failed` contract discards its provisional result. Completing or failing a declaration releases its dependents; a failed inferred signature makes callers use error-sentinel recovery rather than nested analysis or skipped diagnostics.
- Dependents become ready in deterministic order as their dependency counts reach zero.
- Explicit recursion remains legal because explicit signatures are already resolved. Recursive components containing inferred unions are marked failed before body scheduling; explicit-signature members and external callers retain their recovery behavior.
- Body analysis never calls `resolve_decl` recursively. Reading an inferred callee signature requires the scheduler to have finalized or failed it already; an unresolved prerequisite is an internal scheduler error, not a request for nested body analysis.

Diagnostics generated during one body remain unchanged. Dependency order is already observable when an inferred callee must be analyzed before its caller; normalized diagnostic fixtures must verify the resulting order rather than relying on hash-map iteration.

## Flow return validation

Flow construction records whether execution can reach the function's implicit fallthrough. This fact comes from reachable Flow terminators, not a new TIR walk. A reachable `Return`, diverging call, or unreachable terminator is not fallthrough; loops and backedges do not create a false successful exit.

At the frontend Flow boundary, emit `MissingReturn` when a non-void, non-error-returning body can reach implicit fallthrough. Keep the existing body span and diagnostic text. Error-typed returns continue to suppress cascading missing-return diagnostics. The source-clean/publication gate prevents ownership or backend output from becoming executable after this or any other source diagnostic.

Remove Sema's final `body_definitely_returns` missing-return check. Retain TIR divergence queries used by expression typing; do not remove those merely because the final check moves.

## State and publication review

| Artifact or declaration | States | Valid publication | Failure and cleanup |
|---|---|---|---|
| UIR body ownership | Uncomputed, computed/validated, invalid | Computed map borrowed by Sema | Cross-body or malformed input emits an internal diagnostic; no declaration registration or TIR |
| Nominal declaration | Pending, in-progress, defined, failed | Defined layout and resolved field identities in Sema | Retain UIR and diagnostics; dependent executable signatures fail |
| Function signature | Unresolved, resolved, failed | Concrete explicit signature or inferred placeholder eligible for scheduled finalization | Ordinary errors use error-sentinel recovery; failed inferred prerequisites unblock callers into that recovery path |
| Function body | Unresolved, in-progress, resolved, failed | Completed TIR; inferred unions finalized before `Resolved` | Only narrowly failed declarations discard provisional TIR; ordinary errors produce diagnostic-only recovery TIR |
| Recovery TIR/Flow | Diagnostic-only, source-clean | Only source-clean artifacts may advance toward backend publication | Errors suppress executable publication even when recovery structure is inspectable |
| Final backend artifacts | Existing validated `BackendReady` state | Current declarations, sources, pool identities, calls, layouts, and cleanup all valid | Existing I-292 corruption and stale-artifact rejection remains mandatory |

No state transition uses declaration success as a substitute for source cleanliness. Nested inferred resolution is prevented by scheduling rather than guarded recursion. Failed and unresolved placeholders may remain in UIR or diagnostics, never in `BackendReady`.

## Inspection and regression coverage

### UIR and ownership contracts

- Snapshot unresolved UIR for forward references, anonymous and positional types, explicit and inferred error unions, enums, nominal errors, and error families. Show type-expression references and unresolved declaration names; resolved `TypeId`s and variant/field indices must not appear.
- Exercise every instruction and wire shape through ownership computation. Cover shared references within one body, cross-body rejection, orphan instructions, nested control flow, match/catch, destructuring, loops, and variant/error arguments.
- Assert body-local slots are dense and deterministic and that per-body TIR maps use body rather than program sizes.
- Corrupt the computed-map input and verify failure occurs before declaration registration and TIR publication.

### Resolution and diagnostics

- Compare normalized diagnostics with the pre-migration behavior for forward references, legal explicit recursion, invalid nominal and cross-kind cycles, duplicate declarations, unknown types/variants/fields, view fields, and invalid source forms.
- Cover explicit and inferred error dependencies, recursive inferred components, failed annotations, dependent failure, recovery, and deterministic diagnostic ordering.
- Move AstGen tests that asserted semantic diagnostics to the Sema boundary. AstGen tests assert unresolved structure instead.
- Verify source replacement still allocates fresh declaration identities and stale artifacts still fail backend preparation.

### Flow and execution

- Port all missing-return cases to the Flow boundary: simple fallthrough, if/else, loops, early return, break/continue, error propagation, explicit `Unreachable`, and diverging calls. Verify typing-time divergence behavior is unchanged.
- Run a representative corpus through debug and release JIT and AOT, including forward references, enums/error families, inferred errors, nested matches, loops, and ownership cleanup. Compare output and exit status with the base revision.
- Preserve existing TIR, Flow, cleanup, stale-artifact, wrong-context, and release-mode regression coverage.

### Performance and standard gates

Measure tiny and large frontend inputs on the same runner type. Check the one-time ownership traversal, dense local maps, and type-expression copying for fixed-cost and scaling regressions. Use CodSpeed for the final compile-time gate and compare affected benchmarks with `origin/main`.

Run `cargo test --workspace`, `RUSTFLAGS=-Dwarnings cargo clippy --workspace --all-targets`, `cargo fmt --check`, `./scripts/check_file_length.sh`, `git diff --check`, and a fresh CodeRabbit review before completion.

## Durable documentation and issue cleanup

During implementation, update the compiler guide and pipeline architecture document to describe unresolved UIR, Sema-owned resolution, exact body ownership, dependency scheduling, and Flow return validation. Remove obsolete statements that AstGen resolves annotations or that Sema performs final TIR missing-return analysis.

Delete I-285 and the completed I-109 entry when their acceptance criteria pass. I-292 remains open as the cross-cutting verification program. Delete this temporary design after the durable documentation matches the merged implementation.

## Builder decisions

The implementation may choose exact field names, wire flag values, module boundaries within Sema, and traversal order where tests establish determinism. It may split the work into small commits inside this PR, but it must not leave AstGen and Sema sharing semantic ownership at the PR boundary. Any discovered need for per-function arenas, target-layout changes, or new language behavior requires returning to design review.

## References

- Issues: I-285 (unresolved UIR and centralized Sema), I-109 (instruction ownership), I-292 (publication and execution verification), I-284 and I-288 (compilation and source identities), I-286 (later self-contained Flow), I-293 (later physical lowering).
- Spec: [Section 2](../../specification.md) (types and declarations), [Section 5](../../specification.md#5-memory-management-ownership-lite) (ownership), [Section 6](../../specification.md#6-functions--closures) (functions and returns), [Section 7](../../specification.md) (error handling and diagnostics).
- Dev: [Compiler guide](../guide_compiler.md), [Pipeline and Flow migration](pipeline_migration.md), [Pipeline alignment](pipeline_alignment.md), [Pattern matching](../pattern_matching.md).
