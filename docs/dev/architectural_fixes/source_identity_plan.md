# Compilation Context and Source Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish compilation-owned immutable source revisions and preserve their identities from lexing through diagnostics without changing language behavior.

**Architecture:** Add a context-tagged `SourceId` and source database in `ryo-core`, then propagate source-aware spans through the existing flat IRs. Keep the current single-file driver shape, but resolve every diagnostic label through the source database instead of a single input/name pair.

**Tech Stack:** Rust workspace, Logos, Chumsky, Ariadne 0.6, existing UIR/TIR flat `u32` side arenas, Cargo tests, JIT/AOT through the existing Cranelift backend.

**Spec:** `docs/dev/architectural_fixes/proposal_source_identity.md`

## Global Constraints

- Preserve language syntax, semantics, accepted programs, diagnostic codes, and execution behavior.
- Keep source ownership in `ryo-core`, parsing in `ryo-frontend`, and rendering in `ryo-driver`; retain existing crate boundaries and add no dependencies.
- Do not add `DeclId`, `BindingId`, modules, generics, `InternPool` ownership changes, target layout, CLIF source locations, or persistent source identities.
- A source-less or invalid span must never silently select the first registered file.
- Source text and display paths are immutable after registration; replacing text creates a new revision and leaves prior revisions readable.
- All parser/IR changes must preserve the existing flat-arena and checked-`u32` conventions.
- Every task ends green and committed. Temporary compatibility adapters added for task isolation must be removed in Task 5.
- Run the repository's full verification and CodeRabbit gates before completion.

## Review Focus

- Same local revision index from a different compilation context: reject it rather than reading the other context's source; pinned by `wrong_context_with_same_local_index_is_rejected` in Task 1.
- Replacement through a stale revision: return an error and leave both current text and prior revisions unchanged; pinned by `stale_replacement_fails_without_mutation` in Task 1.
- Source-less truncation/internal diagnostics: render without a filename or source excerpt; pinned by `source_less_diagnostic_does_not_borrow_a_filename` in Task 5.
- Unicode before primary and secondary labels in different files: convert each file's byte offsets independently and show both correct excerpts; pinned by `cross_file_labels_use_their_own_sources` in Task 5.
- Match side-arena cloning/decoding with identical offsets in different sources: preserve the full source handle and update content stamps; pinned by the wire round-trip and stamp tests in Tasks 2 and 4.

---

### Task 1: Compilation-Owned Source Revisions

**Files:**
- Create: `ryo-core/src/compilation.rs`
- Create: `ryo-core/src/source.rs`
- Modify: `ryo-core/src/artifact.rs`
- Modify: `ryo-core/src/lib.rs`

**Interfaces:**
- Consumes: existing process-local identity pattern in `artifact.rs`.
- Produces:
  - `CompilationContextId: Copy + Eq + Hash`, allocated with a nonzero process-unique `u32` tag.
  - `SourceId: Copy + Eq + Hash`, containing a context tag and local revision index; `SourceId::NONE` is explicit.
  - `CompilationContext::new()`, `register_source(name: PathBuf, text: String) -> SourceId`, `replace_source(previous: SourceId, text: String) -> Result<SourceId, SourceError>`, and `sources(&self) -> &SourceDb`.
  - `Source::id() -> SourceId`, `Source::name() -> &Path`, and `Source::text() -> &str`.
  - `SourceDb::get(SourceId) -> Result<&Source, SourceError>`, `require_current(SourceId) -> Result<&Source, SourceError>`, and `validate_range(SourceId, Range<usize>) -> Result<(), SourceError>`.
  - `SourceId::raw_parts()` and `SourceId::from_raw_parts()` remain crate-private for span/wire tests and codecs.

- [ ] **Step 1: Write failing source lifecycle tests**

Add inline `source.rs` tests covering:
- registration returns owned text/path and current lookup succeeds;
- replacement returns a different ID, retains the old text through `get`, and makes only the new ID current;
- `stale_replacement_fails_without_mutation` leaves the current ID/text and old lookup unchanged;
- `wrong_context_with_same_local_index_is_rejected` uses two contexts whose first IDs have equal local indices;
- unknown revision, source-less range, reversed range, out-of-bounds range, and a range splitting a multi-byte character are rejected.

- [ ] **Step 2: Run the tests and confirm the new API is absent**

Run: `cargo test -p ryo-core source::tests`
Expected: FAIL to compile because `source`, `CompilationContext`, and `SourceId` do not exist.

- [ ] **Step 3: Implement the context and immutable source database**

Add `CompilationContextId` beside the existing process-local identity machinery in `artifact.rs`. Use checked allocation that cannot wrap to the `NONE` tag. Implement append-only source records and a separate current-revision table indexed by private lineage IDs. Validate replacement before appending so all error paths are rollback-free. Export only the two new modules and their required public types.

- [ ] **Step 4: Run core source tests**

Run: `cargo test -p ryo-core source::tests`
Expected: PASS, including same-index foreign-context rejection and stale-replacement rollback.

- [ ] **Step 5: Commit**

```bash
git add ryo-core/src/artifact.rs ryo-core/src/compilation.rs ryo-core/src/source.rs ryo-core/src/lib.rs
git commit -m "feat: add compilation-owned source revisions"
```

### Task 2: Source-Identified Spans and Artifact Stamps

**Files:**
- Modify: `ryo-core/src/span.rs`
- Modify: `ryo-core/src/ast.rs:840-856`
- Modify: `ryo-core/src/tir/version.rs`
- Modify: `ryo-core/src/ownership/version.rs`
- Modify: existing TIR/ownership version tests

**Interfaces:**
- Consumes: `SourceId` and `SourceDb` from Task 1.
- Produces:
  - `Span { source: SourceId, start: usize, end: usize }`.
  - `Span::EMPTY` and `Span::new(start, end)` are explicitly source-less.
  - `Span::in_source(source, start, end)`, `with_start(start)`, and `with_end(end)` preserve identity for derived spans.
  - `SourceDb::validate_span(Span) -> Result<(), SourceError>` delegates real spans to `validate_range` and accepts only `Span::EMPTY` as source-less.

- [ ] **Step 1: Write failing span and stamp tests**

Add tests proving:
- `Span::EMPTY.source == SourceId::NONE` and does not equal a real `0..0` span;
- `in_source` plus `with_start`/`with_end` retains the exact source handle;
- `Ast::set_top_level` joins first/last offsets without dropping the source;
- changing only a span's source changes TIR and ownership content stamps;
- `validate_span` rejects a nonempty source-less span and accepts a valid Unicode-boundary span.

- [ ] **Step 2: Run the focused tests and confirm failure**

Run: `cargo test -p ryo-core span::tests`
Expected: FAIL to compile because `Span` has no `source` field or source-aware constructors.

- [ ] **Step 3: Implement span identity and stamp coverage**

Add the source field and constructors without changing offset types. Replace production derived-span reconstruction in `Ast::set_top_level` with `with_end`. Hash `Span.source` wherever TIR or ownership stamps already hash offsets; keep the existing hasher and do not introduce persistence.

- [ ] **Step 4: Run core tests**

Run: `cargo test -p ryo-core`
Expected: PASS. Update only tests whose old expectation intentionally treated `Span::new` as a real parsed span; those must use `Span::in_source`.

- [ ] **Step 5: Commit**

```bash
git add ryo-core/src/span.rs ryo-core/src/source.rs ryo-core/src/ast.rs ryo-core/src/tir ryo-core/src/ownership
git commit -m "feat: attach source identities to spans"
```

### Task 3: Lexer, Parser, and AST Provenance

**Files:**
- Modify: `ryo-frontend/src/lexer.rs`
- Modify: `ryo-frontend/src/indent.rs`
- Modify: `ryo-frontend/src/spans.rs`
- Modify: `ryo-frontend/src/parse.rs`
- Modify: `ryo-frontend/src/grammar.rs` and `ryo-frontend/src/grammar/*.rs`
- Modify: frontend tests and `ryo-frontend/benches/frontend.rs`

**Interfaces:**
- Consumes: `Source`, `SourceId`, and source-aware spans from Tasks 1–2.
- Produces:
  - Temporary additive APIs `lexer::lex_with_source(&Source, &mut InternPool, &mut DiagSink) -> Vec<(Token, lexer::Span)>` and `parse::parse_source_with_source(&Source, &mut InternPool) -> Result<(ast::Ast, Vec<Diag>), Vec<Diag>>`; existing source-less `lex`/`parse_source` wrappers remain only to keep the driver green until Task 5.
  - `ParseState` stores the active `SourceId` and exposes a crate-private accessor for grammar span conversion.
  - `spans::cspan(SourceId, SimpleSpan) -> Span`.

- [ ] **Step 1: Add failing provenance tests**

Add tests using a `CompilationContext` and real registered `Source`:
- lexer diagnostics and recovered tokens carry `source.id()`;
- parser AST nodes, recovered nodes, top-level `Ast::span`, and parser diagnostics carry the same ID;
- an empty valid program remains explicitly source-less only where no real source span exists;
- no reachable real token/AST span equals `SourceId::NONE`.

- [ ] **Step 2: Run the new tests and confirm failure**

Run: `cargo test -p ryo-frontend parse::tests lexer::tests`
Expected: FAIL because source-aware lexer/parser entry points do not exist.

- [ ] **Step 3: Implement source-aware lexing and parsing**

Thread `SourceId` through indentation diagnostics, lexer span construction, `ParseState`, all `cspan` call sites, and parser recovery. Preserve existing Chumsky parser construction and hot-path discriminators. Migrate frontend tests and benchmarks to the source-aware APIs. Register benchmark sources outside the timed parse loop so source-file I/O and registration are not charged to parsing.

- [ ] **Step 4: Run frontend tests and parser benchmarks**

Run: `cargo test -p ryo-frontend`
Expected: PASS.

Run: `cargo bench -p ryo-frontend -- parse_snippet`
Expected: benchmarks complete; record the result for later same-runner comparison, with no double-digit regression accepted silently.

- [ ] **Step 5: Commit**

```bash
git add ryo-frontend
git commit -m "feat: preserve source identity through parsing"
```

### Task 4: UIR/TIR Wire and Derived-Span Provenance

**Files:**
- Modify: `ryo-core/src/uir/match_.rs`
- Modify: `ryo-core/src/tir/match_.rs`
- Modify: `ryo-frontend/src/sema/match_.rs:721-735`
- Test: inline UIR/TIR match wire tests and `ryo-core/src/flow/tests.rs`

**Interfaces:**
- Consumes: crate-private `SourceId::raw_parts`/`from_raw_parts` and `Span::with_start` from Task 2.
- Produces: UIR and TIR match-arm headers encode context tag, local revision, start offset, and end offset before the existing pattern/discriminant payload. Decoded `MatchArmView` values return the original source-identified `Span`.

- [ ] **Step 1: Write failing literal wire tests**

For each IR, independently supply literal words and assert exact header positions, arm length, decoded offsets, and full source identity. Include two spans with equal offsets and different source handles. Add a derived `catch as` test proving the widened span retains its source, and a Flow-construction test proving the source-identified TIR remains compatible with its Flow artifact.

- [ ] **Step 2: Run match tests and confirm failure**

Run: `cargo test -p ryo-core match_`
Expected: FAIL because the new literal headers do not match the existing two-offset layout.

- [ ] **Step 3: Extend both wires and derived-span helpers**

Encode both source raw parts before the checked `u32` offsets, update `PATTERN_START`/`DISC_PAYLOAD` and dependent constants, and decode with `SourceId::from_raw_parts`. Update all literal fixtures and layout comments. Replace Sema's `Span::new(as_start, span.end)` with `span.with_start(as_start)`. Do not add a second Flow provenance side channel in this PR.

- [ ] **Step 4: Run core and frontend tests**

Run: `cargo test -p ryo-core -p ryo-frontend`
Expected: PASS, including UIR/TIR cloning, match decoding, Flow compatibility, and existing corruption tests.

- [ ] **Step 5: Commit**

```bash
git add ryo-core/src/uir/match_.rs ryo-core/src/tir/match_.rs ryo-core/src/flow/tests.rs ryo-frontend/src/sema/match_.rs
git commit -m "feat: preserve source identity in IR match wires"
```

### Task 5: Source-Aware Driver and Diagnostics

**Files:**
- Modify: `ryo-core/src/errors.rs`
- Modify: `ryo-driver/src/pipeline/mod.rs`
- Modify: `ryo-driver/src/pipeline/diagnostics.rs`
- Modify: `ryo-driver/src/pipeline/{run,build,inspect,emit}.rs`
- Modify: `ryo-driver/src/pipeline/tests.rs`
- Modify: `ryo-frontend/src/{lexer,parse}.rs` and remaining callers

**Interfaces:**
- Consumes: source-aware frontend APIs and all prior span/wire contracts.
- Produces:
  - Final `lexer::lex(&Source, &mut InternPool, &mut DiagSink) -> Vec<(Token, lexer::Span)>` and `parse::parse_source(&Source, &mut InternPool) -> Result<(ast::Ast, Vec<Diag>), Vec<Diag>>`; source-less compatibility wrappers are deleted.
  - `finalize_diags(Vec<Diag>, &SourceDb) -> Result<(), CompilerError>` and `fail_with_diags(Vec<Diag>, &SourceDb) -> CompilerError`.
  - `render_diags_with_writer<W: Write>(&[Diag], &SourceDb, &mut W) -> Result<(), CompilerError>` for deterministic tests; production `render_diags` delegates with locked stderr.
  - `CompilerError::InternalError(String)` for invalid source artifacts, with stable `Display` output.
  - A render cache keyed by `SourceId`, with one Ariadne `Source` and byte-to-character table per referenced source revision.

- [ ] **Step 1: Write failing renderer and driver tests**

Add tests for:
- `cross_file_labels_use_their_own_sources`, with Unicode before both labels and both display paths/excerpts asserted;
- `source_less_diagnostic_does_not_borrow_a_filename`;
- same-index foreign source, stale current-source lookup, out-of-bounds, and non-character-boundary spans returning `InternalError` without renderer panic;
- existing broken-pipe and non-pipe writer behavior;
- single-file parse, lex, IR, run, and build entry points registering one source and preserving current diagnostics.

- [ ] **Step 2: Run driver tests and confirm failure**

Run: `cargo test -p ryo-driver pipeline::diagnostics::tests`
Expected: FAIL because finalization still requires one input/name pair and has no source database cache.

- [ ] **Step 3: Implement source-aware rendering and remove adapters**

Resolve each report location by its span's `SourceId`; use Ariadne's multi-source cache support and each source's own byte-to-character table. Render source-less diagnostics without a source label. Map invalid source artifacts to `InternalError` rather than fallback source selection. Keep deterministic diagnostic ordering and broken-pipe handling.

At every CLI entry point, move the read `String` into `CompilationContext::register_source`, call `require_current` before parsing, and retain the context until all dependent artifacts are dropped. Remove `source_name` and all source-less lexer/parser adapters and their `input`/`source_name` finalization parameters.

- [ ] **Step 4: Run workspace tests**

Run: `cargo test --workspace`
Expected: PASS with no remaining caller treating spans as offset-only values.

- [ ] **Step 5: Commit**

```bash
git add ryo-core/src/errors.rs ryo-driver ryo-frontend
git commit -m "feat: resolve diagnostics through compilation sources"
```

### Task 6: Permanent Documentation, Cleanup, and Verification

**Files:**
- Modify: `docs/dev/guide_compiler.md`
- Modify: `docs/dev/architectural_fixes/pipeline_migration.md`
- Modify: `docs/dev/implementation_roadmap.md`
- Delete: `docs/dev/architectural_fixes/proposal_source_identity.md`
- Delete: `docs/dev/architectural_fixes/source_identity_plan.md`

**Interfaces:**
- Consumes: the complete implementation from Tasks 1–5.
- Produces: permanent architecture documentation and no temporary planning artifacts.

- [ ] **Step 1: Update permanent architecture documentation**

Document the compilation-owned source database, revision/current lookup distinction, source-identified spans, and source-aware diagnostic cache in the crate map and current pipeline. Remove the temporary roadmap pointer and delete the temporary proposal/plan after their contracts are represented in the permanent docs.

- [ ] **Step 2: Exercise valid JIT and AOT execution**

```bash
mkdir -p target/tmp/source_identity_smoke
cp examples/hello.ryo target/tmp/source_identity_smoke/hello.ryo
cargo run -- run target/tmp/source_identity_smoke/hello.ryo
cargo run -- build target/tmp/source_identity_smoke/hello.ryo
./target/tmp/source_identity_smoke/hello
```

Expected: both executions match the example's existing output and exit successfully.

- [ ] **Step 3: Exercise diagnostic and IR paths**

Run the existing `examples/div_zero.ryo` through `cargo run -- run` and confirm the existing E0037 location/text and nonzero result. Run `cargo run -- ir examples/hello.ryo --emit ast,uir,tir,flow` and confirm all sections emit without source validation errors.

- [ ] **Step 4: Run all required checks**

```bash
cargo fmt --check
RUSTFLAGS=-Dwarnings cargo clippy --workspace --all-targets
cargo test --workspace
./scripts/check_file_length.sh
cargo bench -p ryo-frontend -- parse_snippet
cargo bench -p ryo-frontend -- parse_large
```

Expected: all checks pass. Compare both parser benchmarks against `main` on this runner; fix or explicitly review any double-digit regression rather than merging silently.

- [ ] **Step 5: Run CodeRabbit and address valid findings**

Stage all new source files, then run `coderabbit review --agent --fresh` from this worktree. Verify each finding against current code, fix valid in-scope issues with regression tests, and rerun affected checks plus the full standard checks after fixes.

- [ ] **Step 6: Commit final documentation and cleanup**

```bash
git add -A
git commit -m "docs: record compilation source identity contracts"
git status --short
```

Expected: clean status except for files intentionally left by the user; remove only the smoke directory created above. No temporary plan, generated object, executable, log, or benchmark output remains in the change.
