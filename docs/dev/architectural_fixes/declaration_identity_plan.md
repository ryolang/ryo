# Function Declaration Identity and Direct Calls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every function a compilation-owned `DeclId` and carry it through Sema, TIR bodies, direct calls, and backend-preparation validation.

**Architecture:** Add one declaration table to `CompilationContext`, then replace Sema's private function index with the shared identity. Preserve the existing name only as a diagnostic/backend-compatibility label; backend symbol generation and binding/nominal identities remain separate work.

**Tech Stack:** Rust workspace, Logos, Chumsky, existing UIR/TIR flat `u32` arenas, Ariadne, Cranelift, Cargo tests, JIT/AOT.

**Spec:** `docs/dev/architectural_fixes/proposal_declaration_identity.md`

## Global Constraints

- Preserve syntax, semantics, accepted programs, duplicate-name behavior, diagnostics, and JIT/AOT execution.
- Do not add `BindingId`, nominal-type identity, generic instances, backend symbols, modules, or annotation-resolution changes.
- `CompilationContext` owns the only declaration table and shares one `CompilationContextId` with its source database.
- Do not create a temporary context inside Sema or allocate replacement IDs during expression analysis/backend emission.
- `DeclId::NONE` is limited to synthetic test bodies and is rejected by declaration lookup and backend preparation.
- Existing name-based backend symbols are compatibility behavior only and are not a second Sema resolver.
- Every task ends green and committed; the temporary proposal and plan are deleted after final review.

## Review Focus

- Conflicting registration: return `ConflictingRegistration` and leave the original declaration and returned IDs unchanged; pinned by `conflicting_registration_rolls_back` in Task 1.
- Source replacement: allocate fresh IDs for the new revision and keep old IDs available only for old diagnostics/artifacts; pinned by `source_revision_allocates_fresh_declarations` in Task 1.
- Equal function names in separate contexts: resolve each context independently and reject cross-context use; pinned by `same_name_in_separate_contexts_is_not_conflated` in Tasks 1 and 2.
- `DeclId::NONE` or wrong-context executable artifacts: reject before backend preparation; pinned by `backend_rejects_missing_and_foreign_declarations` in Task 3.
- Direct-call wires: preserve context, index, compatibility name, argument modes, and artifact stamps after encoding/decoding; pinned by literal fixtures in Task 3.

---

### Task 1: Declaration Identity and Registration Lifecycle

**Files:**
- Create: `ryo-core/src/declaration.rs`
- Modify: `ryo-core/src/compilation.rs`
- Modify: `ryo-core/src/lib.rs`
- Test: inline `declaration.rs` tests

**Interfaces:**
- Consumes: `CompilationContextId`, `SourceId`, `Span`, `StringId`, and `Uir.func_bodies`.
- Produces:
  - `DeclId { context: CompilationContextId, index: u32 }`, `DeclId::NONE`, `DeclId::index() -> usize`, and crate-private `raw_parts`/`from_raw_parts` for TIR codecs.
  - `FunctionDecl { id, name, uir_index, span }`.
  - `DeclarationError::{WrongContext, UnknownDeclaration, ConflictingRegistration}`, with `Display` and `Error`.
  - `DeclarationTable::register_functions(&Uir) -> Result<Vec<DeclId>, DeclarationError>` and `get(DeclId) -> Result<&FunctionDecl, DeclarationError>`.
  - `CompilationContext::declarations() -> &DeclarationTable` and `declarations_mut() -> &mut DeclarationTable`.

- [ ] **Step 1: Write failing lifecycle tests**

Add tests for source-ordered unique allocation and span preservation, idempotent exact registration, `conflicting_registration_rolls_back`, `source_revision_allocates_fresh_declarations`, `same_name_in_separate_contexts_is_not_conflated`, unknown IDs, `DeclId::NONE`, and equal-index wrong-context rejection.

- [ ] **Step 2: Run tests and confirm the API is absent**

Run: `cargo test -p ryo-core declaration::tests`
Expected: FAIL to compile because `declaration`, `DeclId`, and `DeclarationTable` do not exist.

- [ ] **Step 3: Implement the declaration table**

Construct one table per `CompilationContext` with the same tag as `SourceDb`. Exact duplicate UIR metadata is idempotent. A different source revision appends new IDs. Conflicting registration for the same UIR body index returns `ConflictingRegistration` before mutation. `get` validates context and rejects unknown/`NONE` handles.

- [ ] **Step 4: Run lifecycle and full core tests**

Run: `cargo test -p ryo-core`
Expected: PASS with no warnings.

- [ ] **Step 5: Commit**

```bash
git add ryo-core/src/declaration.rs ryo-core/src/compilation.rs ryo-core/src/lib.rs
git commit -m "feat: add compilation-owned declaration identities"
```

### Task 2: Sema Resolution by `DeclId`

**Files:**
- Modify: `ryo-frontend/src/sema/mod.rs`
- Modify: `ryo-frontend/src/sema/call.rs`
- Modify: `ryo-frontend/src/sema/error_inference.rs`
- Modify: `ryo-frontend/src/sema/tests*.rs`
- Modify: `ryo-driver/src/pipeline/{mod,run,build,inspect}.rs`
- Modify: `ryo-frontend/benches/frontend.rs`

**Interfaces:**
- Consumes: Task 1's `DeclarationTable`, `DeclId`, `FunctionDecl`, and registration API.
- Produces:
  - `sema::analyze`, `analyze_buffered`, and `analyze_buffered_report` accept `&mut CompilationContext` and never construct a temporary context.
  - `Sema.name_to_decl: HashMap<StringId, DeclId>` and `Sema.signatures: HashMap<DeclId, FunctionSig>`.
  - `CallPlan::User` carries `DeclId`; `prepare_call` validates it with `DeclarationTable::get` before signature lookup.
  - All Sema declaration worklists, inferred-error dependencies, and `FuncCtx.decl` use the shared `DeclId`.

- [ ] **Step 1: Add failing Sema identity tests**

Cover ID-keyed signatures, recursive signatures, legal recursion, inferred error dependencies, duplicate spellings, failed dependencies, and same-name functions resolved independently in two contexts. Confirm the old private `DeclId` cannot satisfy these paths.

- [ ] **Step 2: Run focused tests and confirm failure**

Run: `cargo test -p ryo-frontend sema::tests`
Expected: FAIL because Sema has no context-owned declaration registration or ID-keyed signature resolution.

- [ ] **Step 3: Migrate Sema and callers**

Register functions at the start of `Sema::run`, emit an internal diagnostic and withhold executable TIR on conflicting registration, replace private `DeclId::from_index` uses with shared IDs, and validate direct callees before using their signatures. Update every driver, benchmark, and test caller to retain/pass the real context.

- [ ] **Step 4: Run frontend tests**

Run: `cargo test -p ryo-frontend`
Expected: PASS, including duplicate spellings, recursion, inferred errors, and diagnostics.

- [ ] **Step 5: Commit**

```bash
git add ryo-frontend/src/sema ryo-frontend/benches/frontend.rs ryo-driver/src/pipeline
git commit -m "feat: resolve function declarations through compilation context"
```

### Task 3: TIR Body/Direct-Call Identity and Backend Validation

**Files:**
- Modify: `ryo-core/src/tir.rs` and `ryo-core/src/tir/wires.rs`
- Modify: `ryo-core/src/tir/version.rs`
- Modify: `ryo-frontend/src/pipeline.rs`
- Modify: `ryo-frontend/src/sema/call.rs`
- Modify: `ryo-driver/src/pipeline/{run,build,inspect}.rs`
- Test: `ryo-core/src/tir/tests.rs` and `ryo-frontend/src/pipeline.rs` tests

**Interfaces:**
- Consumes: shared `DeclId`, ID-keyed Sema resolution, and `DeclarationTable::get`.
- Produces:
  - `Tir.decl: DeclId`, with `TirBuilder::with_decl` for production and `DeclId::NONE` only for synthetic test bodies.
  - `TirBuilder::call(callee: DeclId, name: StringId, args, modes, return_type, span)`.
  - `CallView { callee, name, args, modes }` with wire layout `[context, index, name, argc, args..., modes...]`.
  - `CleanupProgram::require_backend_transition(&CompilationContext)`, rejecting `NONE`, unknown, and wrong-context body/call identities before Cranelift; method calls and builtins keep their existing paths.

- [ ] **Step 1: Write failing wire and backend-boundary tests**

Pin literal direct-call words, round-trip `DeclId` and compatibility name/modes, show stamp changes after changing only the declaration ID, and add `backend_rejects_missing_and_foreign_declarations` for `NONE` and wrong-context body/call identities.

- [ ] **Step 2: Run wire tests and confirm failure**

Run: `cargo test -p ryo-core tir::tests`
Expected: FAIL because TIR bodies/calls lack `DeclId` and the new literal layout is unsupported.

- [ ] **Step 3: Implement TIR identity and backend validation**

Add body identity and call-wire encoding, preserve TIR cloning/dumps/stamps, and validate body/call IDs through `DeclarationTable::get` in `CleanupProgram::require_backend_transition`. Keep existing name-based backend symbol generation unchanged.

- [ ] **Step 4: Run core, frontend, and workspace tests**

Run: `cargo test --workspace`
Expected: PASS, including direct/recursive/inferred-error calls and existing corruption tests.

- [ ] **Step 5: Commit**

```bash
git add ryo-core/src/tir.rs ryo-core/src/tir ryo-frontend/src/pipeline.rs ryo-frontend/src/sema/call.rs ryo-driver/src/pipeline
git commit -m "feat: carry declaration identity through direct calls"
```

### Task 4: Documentation, Verification, and Cleanup

**Files:**
- Modify: `docs/dev/guide_compiler.md`
- Modify: `docs/dev/architectural_fixes/pipeline_migration.md`
- Modify: `docs/dev/implementation_roadmap.md`
- Delete: `docs/dev/architectural_fixes/proposal_declaration_identity.md`
- Delete: `docs/dev/architectural_fixes/declaration_identity_plan.md`

**Interfaces:**
- Consumes: complete implementation and validation evidence.
- Produces: permanent declaration-identity documentation and no temporary planning artifacts.

- [ ] **Step 1: Update permanent documentation and remove the temporary pointer**

Document declaration ownership, registration idempotence/conflict handling, source-revision invalidation, direct-call payloads, and backend validation. Remove the temporary roadmap entry and delete the proposal/plan after review tooling no longer needs them.

- [ ] **Step 2: Exercise direct, recursive, and inferred-error execution**

Use temporary programs under `target/tmp/declaration_identity_smoke` for direct calls, legal recursion, duplicate names, and inferred error propagation. Run each through JIT and AOT and confirm identical output and exit status.

- [ ] **Step 3: Run required checks and benchmarks**

```bash
cargo fmt --check
RUSTFLAGS=-Dwarnings cargo clippy --workspace --all-targets
cargo test --workspace
./scripts/check_file_length.sh
cargo bench -p ryo-frontend -- parse_snippet
cargo bench -p ryo-frontend -- parse_large
cargo bench -p ryo-frontend -- sema_snippet
cargo bench -p ryo-frontend -- sema_large
```

Compare parser and Sema benchmarks against `main` on the same runner. Fix or explicitly record any double-digit regression rather than merging silently.

- [ ] **Step 4: Run CodeRabbit and independent whole-branch review**

Stage all files and run `coderabbit review --agent --fresh`. Verify findings against current code, fix valid in-scope issues with regression coverage, and rerun the standard checks. Obtain a fresh whole-branch review and address important findings.

- [ ] **Step 5: Commit documentation and cleanup**

```bash
git add -A
git commit -m "docs: record declaration identity contracts"
git status --short
```

Expected: no temporary plan/proposal, generated object, executable, log, or benchmark output remains in the change. Keep the worktree for PR feedback, then push and create the PR if approved.
