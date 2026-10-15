**Status:** Design (v0.0.x–v0.1)

# Pipeline Migration Regression Checks (Draft — v0.1)

This plan checks the compiler changes described in [pipeline_migration.md](pipeline_migration.md).
It covers observable behavior, ownership, cleanup, diagnostics, and compilation cost.
It does not change language semantics or add future pipeline features.
The [roadmap](implementation_roadmap.md) owns delivery status.

No check in this plan is complete merely because its command is listed.
Record evidence for an exact compiler revision and working-tree state.

## Baseline and Evidence

Select and record the last suitable pre-migration commit before comparison.
Do not assume current `HEAD` is that baseline; migration changes can already be committed.
Keep baseline and candidate binaries, target directories, fixtures, and output artifacts separate.
Never replace the active checkout or discard another contributor's edits to prepare the baseline.

Record:

- Baseline commit and candidate commit, plus candidate patch and untracked source files.
- Rust, Cranelift, Zig, operating system, architecture, CPU, and build configuration.
- Fixture contents, command, environment, stdout, stderr, and process exit status.
- Measured duration, repetitions, memory metric, and measurement tool where applicable.
- Expected behavior, observed difference, and disposition of that difference.

Use `docs/analysis/` or an external temporary directory for raw logs.
Keep committed regression fixtures and summaries self-contained.
Do not reference uncommitted scratch plans as evidence.

## Coverage Matrix

For each row, add focused fixtures and at least one interaction with another row.
Use only features supported by the candidate. Mark baseline-unsupported cases separately.

| Area | Required combinations | Oracle and acceptance condition |
|---|---|---|
| Evaluation order | Side-effecting operands, calls in arguments, `and`/`or`, match conditions, loop bounds | Exact ordered trace; no eager right operand or repeated bound evaluation |
| Branches and joins | Nested if/match, value-producing arms, diverging arms, shadowed bindings | Correct result and binding; no sibling-arm state contamination |
| Cleanup | Heap-backed strings/bytes, aggregates, replacement, early return, break, continue | Required release on each path; no double release or premature destruction |
| Views | Branch-local uses, read-free sibling arms, nested views, reassignment, loop-deferred uses | Backing storage survives all uses and receives required cleanup |
| Loop analysis | Return-only body, consume/rebind, several live views, nested loops, continue paths | Convergence, correct diagnostics, and valid cross-iteration storage lifetime |
| Value caching | Branch-local producers, mutable bindings, repeated iterations, aggregate results | Execution-path-correct values; no stale representation reuse |
| Optimization facts | Arithmetic boundaries after assignment, inout calls, joins, backedges | Required traps remain; debug/release agree with documented arithmetic |
| Calls and ABI | Scalars, views, owning values, aggregates, inout, recursion | Callers and callees agree on values, storage, and returns |
| Diagnostic recovery | Syntax/type errors before ownership-sensitive code, multiple errors, invalid moves | Structured diagnostics with valid locations; no compiler assertion or codegen of invalid bodies |
| Artifact boundaries | Wrong body/Flow/sidecar association and unsupported phase transitions | Controlled rejection by the supported internal contract |
| Scaling | Large bodies, many branches/drops/views, deep nesting | No unexplained compile-time or memory regression; measured performance gates pass |

ABI means application binary interface. JIT means just-in-time execution.
AOT means ahead-of-time compilation.

## Execution Sequence

### 1. Run Existing Checks

Run from the candidate workspace root:

```bash
cargo test --workspace
cargo fmt --check
RUSTFLAGS=-Dwarnings cargo clippy --workspace --all-targets
./scripts/check_file_length.sh
```

If Zig cannot write its default cache in the sandbox, use task-specific writable directories:

```bash
ZIG_GLOBAL_CACHE_DIR=/private/tmp/ryo-regression-zig-global \
ZIG_LOCAL_CACHE_DIR=/private/tmp/ryo-regression-zig-local \
cargo test --workspace
```

Do not classify cache permission failures as compiler regressions.
Rerun failed checks after resolving the environmental cause.

### 2. Add Focused Behavior Regressions

Keep test organization based on responsibility.
Use the existing ownership, view, match, AOT, and backend IR suites where they fit.
Avoid a new catch-all migration or review-round test file.
Preserve existing regression fixtures when moving them between suites.

Pin both previously observed crash shapes, regardless of whether their fixes already landed.

**Return-only loop: expected output is `hello` followed by a newline.**

```ryo
fn choose(flag: bool) -> str:
	s = "hello"
	while flag:
		return s
	return s

fn main():
	print(choose(true))
```

Also call `choose(false)` to cover the zero-iteration path.

**Compound-condition loop: expected output is two lines containing `01`.**

```ryo
fn main():
	s = "0123456789012345678901234567890123456789"
	v = s[0:2]
	mut i = 0
	while i < 2 and true:
		print(v)
		i += 1
```

Extend this shape with `or`, multi-block match conditions, nested loops, and continue paths.
Use values beyond the runtime's inline capacity in memory fixtures.
Include inline and static values too, so release-elision cases remain covered.

### 3. Compare Baseline and Candidate

Build both compilers in isolated directories with equivalent configuration.
Run identical fixture contents through debug/release and JIT/AOT combinations supported by both versions.
Copy AOT fixtures into separate directories because the build command writes artifacts beside the source.
Apply per-run timeouts to compiler and executable processes; record timeout as a distinct failure.

Compare stdout bytes, exit status, diagnostics, and documented observable effects.
Normalize only nondeterministic presentation such as temporary paths and durations.
Do not normalize away diagnostic codes, source locations, missing frames, or message differences without explanation.

Use the specification as the correctness oracle when versions disagree.
Baseline behavior can contain a defect; equality alone is not proof of correctness.
Classify each mismatch as a regression, a verified fix, a supported-feature difference,
or an environmental difference. Add an independent expected-result assertion for verified fixes.
Do not require identical machine-code bytes unless determinism is part of the artifact contract.

### 4. Test Internal Schedule Corruption

These tests verify the validator, independently of successful source compilation.
Run with production position resolution and transfer/escape handling.

- Remove one branch's cleanup while another branch retains cleanup or returns the value.
- Add two ordinary after-consumer releases on one path without reinitialization.
- Accept releases on mutually exclusive paths and releases of different generations.
- Remove promotion cleanup while retaining the corresponding allocation.
- Remove an old-value release while retaining replacement or field replacement.
- Release backing storage before a view use reached through a backedge or continue.
- Corrupt a position or body association; require the appropriate internal rejection.

Check Proven, Invalid, and NotProved outcomes separately.
Do not count a skipped case as validated or bypass production escape logic to obtain rejection.
Keep invalid source diagnostics separate from deliberately corrupted internal artifacts.

### 5. Run Memory and Release Coverage

```bash
cargo test --workspace --release
./scripts/run_linux_tests.sh
```

Confirm the Linux run actually executes its configured sanitizer and Valgrind lanes.
Record tool configuration and results; a skipped lane is not a pass.
Add new ownership fixtures to memory-tool coverage where the existing harness requires explicit registration.
Tests of generated Ryo code must exercise that code; Rust runtime tests alone are insufficient.

Run Miri when runtime changes require it:

```bash
cargo +nightly miri test -p ryo-runtime
```

If Docker or a tool is unavailable, report the missing evidence explicitly.
Do not claim memory verification from ordinary execution or stdout comparison.

### 6. Generate and Minimize Interaction Cases

Use a deterministic generator for a bounded supported subset of the language.
Combine branches, loops, match values, owning storage, views, and inout calls.
Generate valid cases and deliberately invalid ownership cases as separate corpora.
Record seed, generator version, case limits, and expected termination bounds.

Compare valid cases with both compilers and independent invariants.
For invalid cases, require diagnostics rather than compiler crashes.
Minimize each failure while preserving its mismatch or memory-tool result.
Commit the minimized fixture in its behavior-owned suite, with its expected outcome.

### 7. Measure Compile Time, Memory, and Runtime

Existing stage benchmarks:

```bash
cargo bench -p ryo-frontend --bench frontend
cargo bench -p ryo-backend --bench backend
```

The frontend benchmark measures lexing/parsing; the backend benchmark excludes frontend setup from its timed codegen section.
Neither alone establishes end-to-end Flow construction, ownership, or validator performance.
Add or use measurements for those stages and full compilation.
Measure debug compilation separately because debug-only validators can dominate its cost.

Scale function size, branch count, nesting depth, drop count, and live-view count independently.
Compare baseline and candidate using the same fixture, machine, toolchain, options, and cache condition.
Separate clean, warm, and repeated workloads. Report repetitions and variability.
Measure compiler peak memory and generated-program memory separately.

Run affected application benchmarks through their existing scripts, for example:

```bash
./benchmarks/eager_destruction/run_benchmarks.sh
./benchmarks/string_slicing/run_benchmarks.sh
./benchmarks/struct_records_inout/run_benchmarks.sh
```

Use the corresponding README checkpoint and a baseline run under matching conditions.
Respect CodSpeed's compile-time gate. A double-digit tracked regression needs a fix
or an explicit recorded decision; do not merge silently.
No performance improvement is established until measured.

## Acceptance and Evidence Record

The migration is ready for acceptance when:

- Supported valid fixtures pass debug/release and JIT/AOT checks.
- Invalid fixtures produce expected diagnostics without compiler crashes.
- Differential mismatches have recorded dispositions and independent correctness evidence.
- Cleanup corruption tests exercise production contracts and reject invalid schedules.
- Required memory-tool lanes pass, with no newly introduced leaks or invalid accesses.
- Validation proof limits are explicit and satisfy the supported phase contract.
- Compiler and application performance gates pass or have an approved recorded exception.

For each check, record this table in the PR report or a self-contained validation summary:

| Check | Baseline/candidate identities | Configuration and command | Result and evidence | Remaining limitation |
|---|---|---|---|---|
| Workspace and release tests | Record exact revisions | Record actual invocation | Pending until executed | Record missing lanes |
| Differential corpus | Include candidate patch | Record fixture corpus and modes | Pending until compared | Record unsupported baseline cases |
| Cleanup corruption tests | Record tested revision | Record production validator path | Pending until executed | Record NotProved cases |
| Memory tools | Record compiler/runtime build | Record sanitizer/Valgrind configuration | Pending until executed | Record platform scope |
| Performance | Record both builds | Record metrics, repetitions, and cache state | Pending until measured | Record variability and unmeasured stages |

File persistent defects through `scripts/issue.py`; do not create issues for fixes underway.
Do not declare migration completion by moving required checks into follow-up issues.
This document plans checks; it does not certify the current working tree.

## References

- Spec: [Section 3](../specification.md#3-syntax--grammar), [Section 5](../specification.md#5-memory-management-ownership-lite), and [Section 7](../specification.md#7-error-handling) for control flow, ownership, and diagnostics.
- Dev: [Pipeline migration](pipeline_migration.md), [compiler guide](compiler_guide.md), and [compiler instructions](ryo-compiler-llm-instructions.md).
- Tests: [Ownership](../../ryo/tests/integration_ownership.rs), [views](../../ryo/tests/integration_views.rs), [match](../../ryo/tests/integration_match.rs), and [AOT](../../ryo/tests/integration_aot.rs).
- Memory coverage: [Linux test runner](../../scripts/run_linux_tests.sh).
- Performance: [Benchmark index](../../benchmarks/README.md), [frontend benchmarks](../../ryo-frontend/benches/frontend.rs), and [backend benchmarks](../../ryo-backend/benches/backend.rs).
- Delivery: [Implementation roadmap](implementation_roadmap.md).
