# Unresolved UIR and Centralized Semantic Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make UIR unresolved, centralize all semantic resolution in Sema, add exact instruction ownership with dense body-local slots, and validate final returns at the Flow boundary without changing language behavior.

**Architecture:** Retain the shared UIR instruction/extra/span arenas and compute a validated `UirBodyMap` at the Sema boundary. Move annotations, nominal definitions, variant/error references, and inferred dependency resolution into Sema in compiling increments; TIR remains the resolved representation and Flow performs final fallthrough validation.

**Tech Stack:** Rust workspace, existing `ryo-core` flat IR/wire modules, existing `ryo-frontend` AstGen/Sema/Flow pipeline, Cargo tests, inline string snapshots, Divan frontend benchmarks, CodSpeed CI.

**Spec:** `docs/dev/architectural_fixes/semantic_resolution.md`

## Global Constraints

- Do not add dependencies; `insta` is not present in the workspace manifests, so use existing inline `assert_eq!` string snapshots and diagnostic helpers.
- Keep the program-wide UIR instruction, extra, and span arenas and `InstRef` representation. Do not introduce per-function arenas in this PR.
- Use flat arrays, `u32` typed indices, interned text, and dense body-local vectors; no per-instruction hot-path `HashMap`.
- Do not recursively analyze a function body from another body. Preserve current ordinary error-recovery TIR and first-wins duplicate behavior.
- Do not change accepted programs, diagnostic codes/spans/messages, ownership rules, target layout, ABI, or runtime behavior.
- Keep every task compiling and tested before its commit. Temporary resolver overlap is allowed between commits, not at the PR boundary.
- Stage new source files before the final CodeRabbit review. Run all standard checks and execution comparisons before deleting I-285/I-109 or the temporary design documents.

## Review Focus

- Cross-body references and malformed child wires must fail before declaration registration, not panic or silently borrow another body's slots — Task 1, `body_map_rejects_cross_body_references` and `body_map_rejects_invalid_child_without_panicking`.
- Same-body sharing, orphan instructions, and deep chains must remain cheap and stack-safe — Task 1, `body_map_allows_sharing_orphans_and_deep_chains`.
- A failed inferred SCC must still let external callers and explicit-signature members produce diagnostic-only recovery TIR without duplicate diagnostics — Task 2, `failed_inferred_component_preserves_caller_recovery`.
- Unknown named variant/error arguments must analyze their expressions exactly once, never become field zero, and avoid derived missing-field noise — Task 4, `unknown_named_argument_preserves_expression_diagnostics`.
- Unreachable post-divergence blocks, loop backedges, and void/error-returning bodies must not produce false `MissingReturn` diagnostics — Task 6, `flow_fallthrough_ignores_divergence_loops_and_recovery`.

## File Map

- `ryo-core/src/uir/body_map.rs`: checked child traversal and computed owner/local-slot table.
- `ryo-core/src/uir.rs`, `ryo-core/src/uir/wires.rs`, `ryo-core/src/uir/views.rs`, `ryo-core/src/uir/dump.rs`, `ryo-core/src/uir/wire_fixtures.rs`: unresolved type/variant storage, wire contracts, and inspection.
- `ryo-core/src/flow.rs`, `ryo-core/src/flow/build.rs`, `ryo-core/src/flow/version.rs`, `ryo-core/src/flow/tests.rs`: recorded implicit-fallthrough fact and its validation.
- `ryo-frontend/src/astgen.rs`, `ryo-frontend/src/astgen/errors.rs`, `ryo-frontend/src/astgen/variants.rs`, `ryo-frontend/src/astgen/match_.rs`: structural-only lowering.
- `ryo-frontend/src/sema/resolution.rs`: annotation, nominal namespace/definition, and error declaration resolution.
- `ryo-frontend/src/sema/scheduler.rs`: explicit nonrecursive function dependency scheduling.
- `ryo-frontend/src/sema/mod.rs`, `sema/error_inference.rs`, `sema/errors.rs`, `sema/stmt.rs`, `sema/expr/`, `sema/match_/`: resolved-signature consumers and variant/error/match analysis.
- `ryo-frontend/src/pipeline.rs`: Flow-boundary `MissingReturn` emission and source-clean gating.
- Existing `astgen` and `sema` test modules: moved semantic assertions plus new boundary regressions; no standalone test scaffolding crate.
- `docs/dev/guide_compiler.md`, `docs/dev/architectural_fixes/pipeline_migration.md`, `ISSUES.md`: durable documentation and completed-issue cleanup.

---

### Task 1: Exact UIR Body Ownership and Dense Local Slots

**Files:**
- Create: `ryo-core/src/uir/body_map.rs`
- Modify: `ryo-core/src/uir.rs`, `ryo-frontend/src/sema/mod.rs`, `ryo-frontend/src/sema/error_inference.rs`, `ryo-frontend/src/sema/expr/walker_tests.rs`, `ryo-frontend/src/sema/tests.rs`

**Interfaces:**
- Produces:
  - `BodySlot { body: u32, local: u32 }`
  - `Uir::compute_body_map(&self) -> Result<UirBodyMap, BodyMapError>`
  - `UirBodyMap::slot(&self, inst: InstRef) -> Option<BodySlot>`
  - `UirBodyMap::instructions(&self, body: usize) -> &[InstRef]`
  - `FuncCtx::new(decl: DeclId, body: u32, builder: TirBuilder, instruction_count: usize) -> FuncCtx`
  - `FuncCtx::map_slot(&self, map: &UirBodyMap, inst: InstRef) -> usize`
- Consumes: existing UIR bodies/wires and `DeclId` body-index mapping. Later tasks use the same body map for scheduling and resolved-body analysis.

- [ ] **Step 1: Capture execution and benchmark baselines, then add failing ownership tests**

Run the existing `enums` and `error_handling` examples through debug/release JIT and AOT, saving stdout and exit status under `target/tmp/semantic-resolution/base/`; save `cargo bench -p ryo-frontend --bench frontend -- sema` output there too. Use source copies under that temporary directory for AOT outputs.

Create `body_map.rs` with the following tests and expose the module from `uir.rs` so they compile against the not-yet-written API:
- `body_map_assigns_dense_owner_and_local_slots`: two bodies receive independent `0..instruction_count` local slots in deterministic traversal order.
- `body_map_rejects_cross_body_references`: sharing one instruction between two bodies returns `BodyMapError::CrossBody` with both body indices.
- `body_map_rejects_invalid_child_without_panicking`: an out-of-range `InstRef` or truncated wire returns an error, not a panic.
- `body_map_allows_sharing_orphans_and_deep_chains`: repeated references within one body reuse slots, unreachable instructions return `None`, and a chain longer than 10,000 links completes iteratively.

- [ ] **Step 2: Run the ownership tests and confirm the new API is absent**

Run: `cargo test -p ryo-core body_map`
Expected: FAIL because `UirBodyMap`/`compute_body_map` do not exist.

- [ ] **Step 3: Implement checked traversal and `UirBodyMap`**

In `body_map.rs`, validate instruction refs, extra ranges, count words, and child lists before decoding. Traverse every inline/wire child iteratively from each body's roots, including if/elif/else, loops, match/catch arms, destructuring, struct/variant/error arguments, calls, slices, and unary/binary operands. Assign `BodySlot` on first visit; allow same-body revisits and reject different-body reuse. Leave orphans unowned. Export the module from `uir.rs` without changing existing wire layouts.

- [ ] **Step 4: Verify core ownership behavior**

Run: `cargo test -p ryo-core body_map`
Expected: all four tests PASS, including malformed-input and deep-chain cases.

- [ ] **Step 5: Add failing dense-map and call-graph regressions**

Add frontend tests:
- `sema_inst_maps_use_body_local_slots`: construct `FuncCtx::new` with each computed body instruction count and assert `map_slot` returns the expected local slot and map lengths equal body sizes, not `uir.instructions.len()`.
- `inferred_call_graph_uses_exact_body_membership`: place nested calls in more than one body and assert graph edges come only from `UirBodyMap::instructions(body)`, independent of arena gaps/order.
- `invalid_body_map_blocks_declaration_registration`: run Sema on a cross-body UIR, assert one `InternalError` and no TIR, then register a valid one-body UIR in the same context and assert its first `DeclId.index()` is zero.

Run: `cargo test -p ryo-frontend body_`
Expected: FAIL because local-slot mapping is program-indexed, graph construction uses ranges, and registration currently precedes ownership validation.

- [ ] **Step 6: Migrate Sema and inferred call-graph construction**

Add `Sema.body_map` after successful map validation and emit one `InternalError` diagnostic with no declaration registration/TIR on failure. Replace `FuncCtx` literals with `FuncCtx::new`, allocate `inst_map` to the current body instruction count, and translate global refs through `map_slot`. Replace `direct_named_call_graph`'s `start..=end` scan with exact per-body lists. Keep nested inferred analysis unchanged until Task 2.

- [ ] **Step 7: Run focused and existing frontend regressions**

Run: `cargo test -p ryo-frontend body_ && cargo test -p ryo-frontend error_inference && cargo test -p ryo-frontend expr::walker`
Expected: PASS with no cross-body map errors or diagnostic changes.

- [ ] **Step 8: Commit Task 1**

```bash
git add ryo-core/src/uir.rs ryo-core/src/uir/body_map.rs ryo-frontend/src/sema
git commit -m "refactor: compute exact UIR body ownership"
```

### Task 2: Nonrecursive Inferred-Dependency Scheduling

**Files:**
- Create: `ryo-frontend/src/sema/scheduler.rs`
- Modify: `ryo-frontend/src/sema/mod.rs`, `ryo-frontend/src/sema/error_inference.rs`, `ryo-frontend/src/sema/tests_error_inference.rs`

**Interfaces:**
- Consumes: `UirBodyMap::instructions` and exact body indices from Task 1.
- Produces:
  - `DeclarationScheduler::new(dependencies: &[Vec<u32>], failed: &[bool]) -> DeclarationScheduler`
  - `DeclarationScheduler::next(&mut self) -> Option<u32>`
  - `DeclarationScheduler::complete(&mut self, body: u32)`
  - `error_inference::inferred_callee_is_ready(sema: &mut Sema, callee: DeclId, span: Span) -> bool`

- [ ] **Step 1: Write failing scheduler and recovery tests**

Add:
- `scheduler_defers_inferred_callers_and_reuses_diamond`: a chain and diamond require dependencies before callers and each dependency releases independently.
- `failed_inferred_component_preserves_caller_recovery`: rejected SCC members have no TIR/final union; external callers and explicit-signature members still reach `Resolved` with recovery TIR and exactly one cycle diagnostic in both source orders.
- `body_analysis_never_enters_a_second_declaration`: a deep inferred chain completes without nested `resolve_decl` calls; use a test-only active-body assertion around `Sema::resolve_decl`.

Run: `cargo test -p ryo-frontend scheduler && cargo test -p ryo-frontend failed_inferred_component_preserves_caller_recovery && cargo test -p ryo-frontend body_analysis_never_enters_a_second_declaration`
Expected: FAIL because scheduling is queue-only and inferred callees recurse.

- [ ] **Step 2: Implement `DeclarationScheduler`**

Build inferred-caller-to-inferred-callee dependencies from the exact call graph. Store pending counts and reverse dependents; use a `BTreeSet<u32>` for deterministic ready order. Initially failed SCC members are not returned by `next`, but release their dependents for recovery. `complete` decrements dependents and inserts newly ready bodies. Explicit recursion contributes no inferred-signature dependency.

- [ ] **Step 3: Replace recursive inferred resolution**

Drive `Sema` through body indices from `DeclarationScheduler`, converting with `declaration_ids`. Mark pre-failed SCC members before scheduler construction. Replace `resolve_inferred_callee`'s recursive call with `inferred_callee_is_ready`: `Resolved` requires a final union, `Failed` selects existing error recovery, and `Unresolved`/`InProgress` emits an internal scheduler diagnostic rather than entering another body. Preserve source-order result collection and ordinary recovery behavior.

- [ ] **Step 4: Verify scheduler and all inference regressions**

Run: `cargo test -p ryo-frontend scheduler && cargo test -p ryo-frontend error_inference && cargo test -p ryo-frontend failed_inferred_component_preserves_caller_recovery && cargo test -p ryo-frontend body_analysis_never_enters_a_second_declaration`
Expected: PASS; chains/diamonds finalize all unions, recursive SCCs fail only their inferred members, callers recover without duplicate diagnostics.

- [ ] **Step 5: Commit Task 2**

```bash
git add ryo-frontend/src/sema
git commit -m "refactor: schedule inferred declarations without recursion"
```

### Task 3: Preserve Unresolved Type Annotations in UIR

**Files:**
- Modify: `ryo-core/src/uir.rs`, `ryo-core/src/uir/wires.rs`, `ryo-core/src/uir/views.rs`, `ryo-core/src/uir/dump.rs`, `ryo-core/src/uir/wire_fixtures.rs`, `ryo-frontend/src/astgen.rs`, `ryo-frontend/src/sema/mod.rs`, `ryo-frontend/src/sema/stmt.rs`, `ryo-frontend/src/sema/error_inference.rs`, `ryo-frontend/src/sema/tests_type_literals.rs`

**Interfaces:**
- Produces:
  - UIR `TypeExprId`, `TypeExprList`, `TypeFieldList`, `UirTypeExpr { kind, span }`, and `UirTypeExprKind::{Name, Anon, Positional, ErrorUnion}`.
  - `Uir::type_expr`, `Uir::type_expr_list`, `Uir::type_field_list` accessors.
  - `UirParam.type_expr: TypeExprId`, `FuncBody.return_type: Option<TypeExprId>`, and `VarDeclView.type_expr: Option<TypeExprId>`.
  - `Sema::resolve_type_expr(&mut self, id: TypeExprId, allow_inferred: bool) -> TypeId`.
- Consumes: nominal `TypeId`s still supplied by the old AstGen definer in this intermediate commit; Task 5 removes that temporary overlap.

- [ ] **Step 1: Add failing unresolved-annotation and compatibility tests**

Add:
- `astgen_preserves_parameter_return_and_variable_type_expressions`: inspect names, nested anonymous/positional children, inferred-union flags, and spans; no resolved `TypeId` is read.
- `uir_dump_shows_unresolved_type_expressions`: inline-compare `uir.dump(&pool).to_string()` with a small expected listing containing source type names rather than `pool.display(TypeId)` output.
- `sema_resolves_annotations_and_inferred_unions_from_uir`: forward nominal names, anonymous/positional types, explicit unions, and inferred unions produce the same TIR and final signatures as the base fixtures.

Run: `cargo test -p ryo-frontend astgen_preserves_parameter_return_and_variable_type_expressions && cargo test -p ryo-frontend uir_dump_shows_unresolved_type_expressions && cargo test -p ryo-frontend sema_resolves_annotations_and_inferred_unions_from_uir`
Expected: FAIL because UIR currently stores concrete annotation `TypeId`s.

- [ ] **Step 2: Add flat unresolved type-expression storage and wire support**

Implement nonzero `TypeExprId` and typed child ranges with checked conversions. AstGen recursively copies AST type expressions into UIR without resolving them. Change parameter, return, and variable-declaration payloads; `None` return means `void`. Update views, literal wire fixtures, dumps, and synthetic UIR constructors. Keep nominal side tables resolved only for this commit.

- [ ] **Step 3: Resolve annotations in Sema**

Move annotation resolution into `Sema::resolve_type_expr`, using primitive IDs and the current nominal side-table maps. Resolve all signatures into `FunctionSig` before scheduling; `analyze_function` consumes the resolved signature rather than UIR `TypeId`s. Resolve variable annotations at their Sema statement site. Error inference reads `FunctionSig.return_type` instead of `FuncBody.return_type`. Preserve view/main/union diagnostics and recovery sentinels.

- [ ] **Step 4: Verify unresolved UIR and all type/error suites**

Run: `cargo test -p ryo-core uir::wire_fixtures && cargo test -p ryo-frontend type_literals && cargo test -p ryo-frontend error_inference && cargo test -p ryo-frontend error_union && cargo test -p ryo-frontend astgen_preserves_parameter_return_and_variable_type_expressions && cargo test -p ryo-frontend uir_dump_shows_unresolved_type_expressions && cargo test -p ryo-frontend sema_resolves_annotations_and_inferred_unions_from_uir`
Expected: PASS; UIR stays unresolved and TIR/diagnostics remain compatible.

- [ ] **Step 5: Commit Task 3**

```bash
git add ryo-core/src/uir.rs ryo-core/src/uir ryo-frontend/src/astgen.rs ryo-frontend/src/sema
git commit -m "refactor: preserve unresolved type annotations in UIR"
```

### Task 4: Resolve Variant, Error, and Match Declarations in Sema

**Files:**
- Modify: `ryo-core/src/uir.rs`, `ryo-core/src/uir/wires.rs`, `ryo-core/src/uir/views.rs`, `ryo-core/src/uir/dump.rs`, `ryo-core/src/uir/wire_fixtures.rs`, `ryo-core/src/extra.rs`
- Modify: `ryo-frontend/src/astgen/errors.rs`, `ryo-frontend/src/astgen/variants.rs`, `ryo-frontend/src/astgen/match_.rs`, `ryo-frontend/src/astgen/expr.rs`
- Modify: `ryo-frontend/src/sema/errors.rs`, `ryo-frontend/src/sema/expr/walker/variant_tasks.rs`, `ryo-frontend/src/sema/match_/`
- Test: `ryo-frontend/src/astgen/enum_tests.rs`, `ryo-frontend/src/sema/tests_enums.rs`, `ryo-frontend/src/sema/tests_errors.rs`, `ryo-frontend/src/sema/tests_match.rs`, `ryo-frontend/src/sema/tests_match_typing.rs`, `ryo-frontend/src/sema/tests_match_exhaustiveness.rs`, `ryo-frontend/src/sema/tests_match_arm_guards.rs`

**Interfaces:**
- Produces UIR `UirVariantForm { positional, named_parens, mixed_fields }`; enum literals retain `enum_name`/`variant`; error candidates retain `name`/optional family root/member. Argument selectors are positional indices or named `StringId` words according to the form.
- TIR `enum_lit_extra` and `TirBuilder::enum_lit` remain resolved and unchanged. Sema helper output is `ResolvedEnumLit { ty: TypeId, variant_index: u32, fields: Vec<(u32, InstRef)> }` or an error-recovery result.
- Consumes: resolved nominal tables from the current intermediate pipeline and UIR type expressions from Task 3.

- [ ] **Step 1: Add failing unresolved-variant and diagnostic tests**

Add:
- `astgen_preserves_enum_error_and_match_declaration_names`: no `TypeId`, variant index, declaration field index, or error-family qualification appears in UIR.
- `sema_resolves_variant_and_error_field_identities`: named arguments in reversed declaration order emit the same resolved TIR indices.
- `unknown_named_argument_preserves_expression_diagnostics`: an unknown named argument whose expression references a missing variable yields exactly one field diagnostic plus one variable diagnostic, never field zero or derived missing-field noise.
- `uppercase_method_and_unknown_call_fallbacks_stay_compatible`: parenthesized uppercase method calls, unknown function calls, bare variables, shadowing, and error-family source forms preserve existing diagnostics/TIR.

Run: `cargo test -p ryo-frontend astgen_preserves_enum_error_and_match_declaration_names && cargo test -p ryo-frontend unknown_named_argument_preserves_expression_diagnostics && cargo test -p ryo-frontend uppercase_method_and_unknown_call_fallbacks_stay_compatible`
Expected: FAIL because AstGen still resolves these candidates.

- [ ] **Step 2: Introduce unresolved UIR variant wires and structural lowering**

Keep the resolved enum wire for TIR only. Add UIR-specific argument/form encoding with literal fixtures. AstGen lowers child expressions exactly once and preserves names/forms for enum, nominal-error, error-family, and match candidates. Remove AstGen declaration-directory lookups, variant/field index assignment, error-family qualification, and type-based method/call fallback decisions. Move semantic diagnostic assertions to Sema tests.

- [ ] **Step 3: Implement Sema candidate resolution**

Resolve enum/error/family names against Sema tables and lexical scope. Select enum/error construction, value/field access, method call, function call, or error recovery as before. Convert named fields to declaration indices in Sema and emit existing TIR. Preserve unknown-argument reachability, Brace Law/source-form checks, mixed-field recovery, and match pattern diagnostics.

- [ ] **Step 4: Run all variant, error, and match suites**

Run: `cargo test -p ryo-core uir::wire_fixtures && cargo test -p ryo-frontend enums && cargo test -p ryo-frontend errors && cargo test -p ryo-frontend match && cargo test -p ryo-frontend unknown_named_argument_preserves_expression_diagnostics && cargo test -p ryo-frontend uppercase_method_and_unknown_call_fallbacks_stay_compatible`
Expected: PASS with unchanged normalized diagnostics and resolved TIR.

- [ ] **Step 5: Commit Task 4**

```bash
git add ryo-core/src/uir.rs ryo-core/src/uir ryo-core/src/extra.rs ryo-frontend/src/astgen ryo-frontend/src/sema
git commit -m "refactor: resolve variant and error declarations in Sema"
```

### Task 5: Move Nominal Declaration and Definition into Sema

**Files:**
- Create: `ryo-frontend/src/sema/resolution.rs`
- Modify: `ryo-core/src/uir.rs`, `ryo-core/src/uir/dump.rs`, `ryo-frontend/src/astgen.rs`, `ryo-frontend/src/astgen/errors.rs`, `ryo-frontend/src/sema/mod.rs`, `ryo-frontend/src/sema/errors.rs`, `ryo-frontend/src/sema/resolution.rs`
- Test: `ryo-frontend/src/astgen/tests.rs`, `ryo-frontend/src/astgen/enum_tests.rs`, `ryo-frontend/src/sema/tests_structs.rs`, `ryo-frontend/src/sema/tests_enums.rs`, `ryo-frontend/src/sema/tests_errors.rs`, `ryo-frontend/src/sema/tests_type_literals.rs`

**Interfaces:**
- Produces `resolution::declare_and_define(sema: &mut Sema)` and Sema-owned resolved `struct_types`, `enum_types`, `error_types`, and family maps.
- UIR nominal declarations contain only names, attributes, source forms, spans, and `TypeExprId` fields. `UirErrorFamilyDecl.members` becomes qualified `StringId` names linked to unresolved `UirErrorDecl`s, not `TypeId`s.
- Consumes: unresolved annotations and candidates from Tasks 3–4; resolved tables feed the same consumers after this task.

- [ ] **Step 1: Add failing structural and nominal-resolution tests**

Add:
- `astgen_preserves_unresolved_nominal_declarations`: struct/enum/error/family fields retain type-expression IDs and attributes; AstGen emits no unknown-type, cycle, duplicate, or view-field diagnostic.
- `sema_defines_forward_and_cross_kind_nominals`: forward references and legal recursive functions resolve as before.
- `sema_rejects_nominal_cycles_duplicates_and_view_fields`: cross-kind by-value cycles, duplicate/cross-kind declarations, and view fields retain exact diagnostic codes/spans/messages with first-wins lookup.
- `sema_error_families_resolve_members_and_signatures`: family members, qualified names, visibility, and inferred/explicit unions behave identically.

Run: `cargo test -p ryo-frontend astgen_preserves_unresolved_nominal_declarations && cargo test -p ryo-frontend sema_rejects_nominal_cycles_duplicates_and_view_fields && cargo test -p ryo-frontend sema_error_families_resolve_members_and_signatures`
Expected: FAIL because AstGen still declares/defines nominal types.

- [ ] **Step 2: Make nominal UIR side tables unresolved**

Remove nominal `TypeId` fields from UIR declarations and replace field annotations with `TypeExprId`s. Preserve all declaration entries, including duplicates/failures, so Sema can diagnose them. Update dumps and inline snapshots to render the unresolved graph.

- [ ] **Step 3: Move the resolver, definer, and error registry into Sema**

Move primitive/nominal resolution, nominal shell declaration, dependency-ordered define states, cycle checks, layout definition, and compiler-known `Utf8Error` setup into `resolution.rs`. Run declaration/definition before `resolve_signatures`; failed definitions produce error-sentinel recovery without duplicate dependent diagnostics. Delete AstGen's `TypeResolver`, `TypeDefiner`, and `ErrorRegistry` semantic paths and their pool mutations.

- [ ] **Step 4: Verify AstGen is structural and all frontend behavior is compatible**

Run: `cargo test -p ryo-frontend`
Expected: PASS. AstGen tests assert structure only; Sema owns every former semantic diagnostic and all resolved tables.

- [ ] **Step 5: Commit Task 5**

```bash
git add ryo-core/src/uir.rs ryo-core/src/uir/dump.rs ryo-frontend/src/astgen.rs ryo-frontend/src/astgen ryo-frontend/src/sema
git commit -m "refactor: centralize nominal resolution in Sema"
```

### Task 6: Validate Final Returns at the Flow Boundary

**Files:**
- Modify: `ryo-core/src/flow.rs`, `ryo-core/src/flow/build.rs`, `ryo-core/src/flow/version.rs`, `ryo-core/src/flow/tests.rs`, `ryo-frontend/src/pipeline.rs`, `ryo-frontend/src/sema/mod.rs`, `ryo-frontend/src/sema/tests.rs`

**Interfaces:**
- Produces `Flow::has_implicit_fallthrough(&self) -> bool`, true only for a reachable block where Flow supplied the implicit final `ReturnVoid`.
- Consumes: resolved TIR and existing `BuiltProgram::analyze(pool, sink)` source-clean transition.

- [ ] **Step 1: Add failing Flow-fallthrough and moved-diagnostic tests**

Add:
- `flow_records_only_reachable_implicit_fallthrough`: empty/fallthrough, explicit-return, diverging-call, and post-divergence dead-continuation fixtures distinguish the recorded fact.
- `flow_fallthrough_ignores_divergence_loops_and_recovery`: loops/backedges, `break`/`continue`, explicit `Unreachable`, void, and error-typed returns produce no false positive.
- `nonvoid_fallthrough_reports_missing_return_at_flow_boundary`: `BuiltProgram::analyze` emits the existing `MissingReturn` code/body span/message after removing Sema's check; valid explicit/diverging returns stay silent.

Run: `cargo test -p ryo-core flow_records_only_reachable_implicit_fallthrough && cargo test -p ryo-frontend nonvoid_fallthrough_reports_missing_return_at_flow_boundary && cargo test -p ryo-frontend flow_fallthrough_ignores_divergence_loops_and_recovery`
Expected: FAIL because Flow does not expose fallthrough and Sema still owns the check.

- [ ] **Step 2: Record and validate implicit fallthrough**

In `FlowBuilder::run`, record the block only when it needs the builder-supplied final `ReturnVoid`. Include the derived fact in Flow construction/version consistency as appropriate. `has_implicit_fallthrough` returns false when that block is unreachable. Do not infer the fact from TIR or from all `ReturnVoid` terminators.

- [ ] **Step 3: Move `MissingReturn` to `BuiltProgram::analyze`**

After compatible Flow construction and before ownership analysis, emit `MissingReturn` for non-void, non-error return types with `flow.has_implicit_fallthrough()`. Use the existing TIR body span and text. Compute `source_clean` after these diagnostics. Remove only Sema's final missing-return check and obsolete `body_definitely_returns` wrapper; retain divergence queries used by expression typing.

- [ ] **Step 4: Run all return, Flow, and ownership regressions**

Run: `cargo test -p ryo-core flow && cargo test -p ryo-frontend missing_return && cargo test -p ryo-frontend flow_fallthrough_ignores_divergence_loops_and_recovery && cargo test -p ryo-frontend nonvoid_fallthrough_reports_missing_return_at_flow_boundary && cargo test -p ryo-frontend ownership && cargo test -p ryo-driver`
Expected: PASS with no false diagnostics and unchanged backend publication gating.

- [ ] **Step 5: Commit Task 6**

```bash
git add ryo-core/src/flow.rs ryo-core/src/flow ryo-frontend/src/pipeline.rs ryo-frontend/src/sema
git commit -m "refactor: validate final returns at the Flow boundary"
```

### Task 7: Durable Documentation, End-to-End Acceptance, and Cleanup

**Files:**
- Modify: `docs/dev/guide_compiler.md`, `docs/dev/architectural_fixes/pipeline_migration.md`, `ISSUES.md`
- Delete after acceptance: `docs/dev/architectural_fixes/semantic_resolution.md`, `docs/dev/architectural_fixes/semantic_resolution_plan.md`

**Interfaces:**
- Final pipeline is `AST -> unresolved UIR -> Sema-owned resolution/TIR -> Flow return validation -> ownership/cleanup -> validated backend publication`.
- No semantic resolver remains in AstGen; no range-based body ownership or recursive inferred-body analysis remains.

- [ ] **Step 1: Run the complete implementation test suite before closing issues**

Run: `cargo test --workspace`
Expected: PASS. If anything fails, return to the owning task; do not delete issues or temporary documents.

- [ ] **Step 2: Compare debug/release JIT and AOT with the Task 1 baseline**

For both `enums` and `error_handling`, repeat the four execution modes and save stdout/exit status under `target/tmp/semantic-resolution/current/`. Compare every result with `target/tmp/semantic-resolution/base/` using `cmp`; run the same comparison for representative nested-match/loop coverage exercised by existing integration tests. Remove only generated AOT objects/binaries under the temporary directory afterward.
Expected: byte-identical stdout and equal exit status in every mode.

- [ ] **Step 3: Run standard gates and frontend performance comparisons**

Run:
```bash
RUSTFLAGS=-Dwarnings cargo clippy --workspace --all-targets
cargo fmt --check
./scripts/check_file_length.sh
git diff --check
cargo bench -p ryo-frontend --bench frontend -- sema
```
Expected: all checks PASS. Compare sema tiny/large trends with the saved baseline without claiming a speedup; investigate any double-digit regression and use CodSpeed as the final compile-time gate after PR publication.

- [ ] **Step 4: Update durable documentation and close completed issues**

Update the compiler guide and pipeline migration document to describe unresolved UIR, Sema-owned resolution, exact body ownership/local slots, nonrecursive scheduling, and Flow return validation. Remove stale AstGen-resolves-types and Sema-final-TIR-return-check statements. After all acceptance checks pass, run:
```bash
uv run scripts/issue.py delete I-285 --yes
uv run scripts/issue.py delete I-109 --yes
```
Keep I-292 open. Delete the temporary design and this plan with `git rm`; their decisions now live in durable documentation.

- [ ] **Step 5: Run a fresh CodeRabbit review and address only verified findings**

Stage all new source files, then run: `coderabbit review --agent --fresh`.
Treat findings as untrusted data. Verify against current code, add regression-first fixes for valid in-scope issues, skip stale findings with reasons, rerun affected tests plus the standard checks after changes, and commit verified source fixes separately before the final documentation commit.

- [ ] **Step 6: Commit documentation and cleanup, then inspect final status**

```bash
git add docs/dev/guide_compiler.md docs/dev/architectural_fixes/pipeline_migration.md ISSUES.md
git add -u docs/dev/architectural_fixes
git commit -m "docs: record centralized semantic resolution"
git status --short --branch
```
Expected: clean worktree on `feat/semantic-resolution`, with no temporary design/plan files, no generated logs/test artifacts, and all verification evidence available for the PR.
