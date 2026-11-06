# Structure-Aware Fuzzing Campaign Register

**Purpose:** record every structure-aware fuzz campaign run against the Ryo compiler
(process in `docs/guides/structure_aware_fuzzing.md`), the exact commit each one ran
against, and what it covered — so future campaigns don't rerun settled ground.

**Last updated:** 2026-11-04 (main HEAD `e82f503`; campaigns 1–7 registered).

## Campaign Register

### 1. M13 error types & unions — pilot + full campaign

- **Date:** 2026-10-30
- **Compiler under test:** worktree `.worktrees/feat-error-types-unions` (branch
  `feat/error-types-unions`, since squash-merged to main as `9366c5c` "feat: implement
  error types and unions (M13) (#212)"; `git diff` of the compiler crates between the
  branch and the squash merge is empty, so main's M13 content is what was fuzzed)
- **Tracks:** grammar, inference, ownership, backend (+ `verify-backend`,
  `verify-declarations` one-off checks)
- **Result: defects found and fixed on-branch.** The repro set preserved under
  `/tmp/ryo-m13-fuzz/*/repros/`:
  - `duplicate_named_field` — compiler panic `enum_view on undefined type` (SIGABRT)
    where a structured `E0040` was due; control (single declaration) diagnosed cleanly.
  - `struct_literal_union_coercion` — union field required coercion of a struct literal.
  - `empty_inferred_raw_binding` — `empty()` inferred union, raw binding.
  - `void_success_catch`, `empty_inferred_try_return`,
    `explicit_recursion_tail_call` — void-success catch dispatch, empty inferred error
    sets, aggregate recursion lowered without tail ABI.
  - Fixes shipped in branch commits `76f13f0`, `1a3661f`, `0f288e3`, `49755ad`,
    `7111338`, `481b0a3`, `c7dc0d6`, `02a1993` (all ancestors of the merged `9366c5c`).
- **Artifacts:** `/tmp/ryo-m13-fuzz/` (repros, logs, verification JSONs; bulk generated
  cases were cleaned — /tmp is ephemeral on macOS)

### 2. Error namespaces campaign

- **Date:** 2026-11-03
- **Compiler under test:** `a0b6ddf` "fix: address error namespace review findings" —
  PR #215 branch tip; binary preserved at `/private/tmp/ryo-error-namespace-fuzz/compiler/ryo-a0b6ddf`.
  Squash-merged to main as `e82f503` (current main HEAD).
- **Tracks:** grammar, inference, ownership, backend — 100 cases each (400 total),
  JIT + AOT subset, seeds 1000–1099
- **Result: clean** — no panics, crashes, timeouts, or output mismatches; valid cases
  reached backend; expected-invalid cases produced structured diagnostics (25–40 per
  track). Seed-verification and smoke runs preceded the full run.
- **Artifacts:** `/tmp/ryo-error-namespace-fuzz/run-001/` (manifest.json, harness.py,
  per-case logs)

### 3. Ownership & memory-corruption campaign

- **Date:** 2026-11-04
- **Compiler under test:** `e82f503` (main HEAD)
- **Tracks:** control, own-valid, own-invalid, memory — 120 cases each (480 total),
  AOT every 10th case, seeds 1000–1119; plus a hardened-allocator pass
  (`MallocScribble`/`MallocGuardEdges`/heap checking, 13 family representatives × JIT
  + AOT = 26 runs)
- **Families:** move + slot reuse, `inout` chains, struct/nested-aggregate moves,
  strview freeze + last-use lifting, re-slicing, error-payload moves in catch arms,
  `catch as err` + `.message()`; result-slot reuse in loops, large inactive payloads,
  destructor churn, `inout` string growth. Expected-invalid: use-after-move /
  double-move / field-after-move (`E0020`), frozen-owner move (`E0035`), escaped view
  (`E0022`).
- **Result: clean** — 100% of valid cases reached backend with exact JIT and AOT
  output; all 120 expected-invalid cases produced structured diagnostics; no panics,
  crashes, timeouts, or mismatches.
- **Artifacts:** `/tmp/ryo-ownership-fuzz/run-001/` (manifest.json, harness.py,
  per-case logs); harness at `/tmp/ryo-ownership-fuzz/harness.py`

### 4. Match/enums, UTF-8/bytes, and numeric-edge campaign

- **Date:** 2026-11-04
- **Compiler under test:** `e82f503` (main HEAD)
- **Tracks:** control, match, utf8, numeric — 120 cases each (480 total),
  AOT every 10th case, seeds 2000–2119; plus a hardened-allocator pass
  (`MallocScribble`/`MallocGuardEdges`/heap checking, 26 valid families × JIT + AOT
  and 24 runtime-guard cases under JIT = 50 runs)
- **match track:** exhaustive enum matches with shuffled arms, payload moves out of
  enums (arm binding + nested struct payload), reassign-across-variants loops,
  match-as-expression vs statement-return, derived `Eq` on payload enums, int literal
  arms probed at ±1 around the literals, `%`-comparison loops. Expected-invalid:
  use-after-move in arm (`E0020`), non-exhaustive match (`E0129`), or-patterns
  (`E0100`, deferred M12.5 feature), duplicate literal arm (warning `W0005`, first arm
  wins).
- **utf8 track:** 2-byte/4-byte char slicing at byte offsets, non-ASCII concat + len,
  `to_bytes()`/`to_str()` round-trips, invalid UTF-8 catch message, bytesview
  projection/indexing, view last-use lifting before `bytes_push`, byte repr escapes
  (`b"\xc3\xa9\0\xff"`). Expected-invalid: mid-codepoint slice and out-of-range slice
  (runtime guard, exit 101), frozen-owner mutation (`E0035`), escaped bytesview
  (`E0022`).
- **numeric track:** float formatting round-trips (values chosen to agree between Rust
  Debug and Python repr), NaN/Infinity semantics (`nan != nan`, `inf > 1e308`), i64
  boundary comparisons, truncated-division `%`/`/` sign semantics, near-overflow
  arithmetic in range, constant-folded overflow (`E0200` compile error — distinct from
  runtime overflow which traps at exit 101), div-by-zero (runtime exit 101, distinct
  from literal `1 / 0` → `E0037` at compile time), float modulo (`E0015`),
  `INT64_MIN / -1` (runtime "integer division overflow").
- **Result: clean** — 100% of valid cases reached backend with exact JIT and AOT
  output; all expected diagnostics and runtime errors matched; no panics, crashes,
  timeouts, or mismatches.
- **Artifacts:** `/tmp/ryo-tracks123-fuzz/run-001/` (manifest.json, harness.py,
  per-case logs); harness at `/tmp/ryo-tracks123-fuzz/harness.py`
- **Side finding (not a language defect):** the compiler aborts with SIGABRT when a
  multi-span diagnostic (e.g. `E0020` with a help note) is piped to a reader that
  closes early (`ryo run f.ryo | head -3`); single-span diagnostics and fully drained
  streams exit 1 cleanly. Filed as an issue; fuzz harnesses must drain streams.

### 5. Grammar/recovery torture + deep-recursion campaign

- **Date:** 2026-11-04
- **Compiler under test:** `e82f503` (main HEAD)
- **Tracks:** control, grammar, recursion — 120 cases each (360 total),
  AOT every 10th case, seeds 3000–3119; plus a hardened-allocator pass
  (22 valid families × JIT + AOT = 24 runs)
- **grammar track (recovery torture):** unterminated string/bytes literals,
  space indentation, missing colons, brace blocks and stray braces, unclosed
  parens/calls, illegal tokens (`@`, `` ` ``, `$`, `~`, `^`), non-ASCII
  identifiers, 2–4 independent errors per file (recovery reports all of them;
  the compiler caps at 100 diagnostics with an `E0101` advice), tab/space mixes,
  keywords as identifiers, out-of-range integer literals, dangling `elif`/`else`,
  duplicate parameter names (accepted — last binding shadows, prints the second
  argument; noted as a semantics question, not filed). All malformed inputs
  produced structured diagnostics — **no panics, aborts, or hangs in 112
  expected-invalid cases**.
- **recursion track:** linear recursion to 20k depth, mutual recursion to 5001,
  deep left-associative expressions to 6000 terms, parenthesized nesting to 9000,
  static call chains to 1500 functions, elif chains to 1500, nested blocks to 300,
  `fib` to 28, accumulator recursion to 20k, concatenation chains to 3000, call
  fans to 100 — all exact output in JIT and AOT. **Finding: chains past ~6500
  terms overflowed the compiler stack (SIGABRT) — filed as I-261** (the
  recursion was in astgen/sema/codegen/flow, not the parser), minimized repro
  preserved at `bug_reports/bug_parser_stack_overflow_expr_chain.ryo`; ten
  oversize probes reproduced the abort deterministically. **Fixed in
  `3747e17`** (iterative chain walks across astgen/sema/flow/codegen and
  the AST dump printer; 100k terms now compiles and prints 100000).
- **Result: one defect (I-261, parser stack depth), everything else clean** —
  no panics, timeouts, or output mismatches outside the known probe family.
- **Artifacts:** `/tmp/ryo-gramrec-fuzz/run-001/` (manifest.json, harness.py,
  per-case logs); repro in-repo at `bug_reports/`; harness at
  `/tmp/ryo-gramrec-fuzz/harness.py`

### 6. CI-grade memory checking on Linux (ASan/LSan + Valgrind)

- **Date:** 2026-11-04
- **Compiler under test:** `e82f503` (main HEAD), built natively inside the
  `ryo-linux-test` Docker image (linux/arm64, Valgrind 3.24)
- **Repo suite:** full `scripts/run_linux_tests.sh` — **754 tests, 0 failed**
  across 28 test binaries, including the `asan_smoke` (ASan+LSan) and
  `valgrind_smoke` suites, on Linux with sanitizers active.
- **Targeted fuzz-case pass:** all 49 fuzz-family representatives (ownership
  memory shapes, UTF-8/bytes, match, numeric, deep recursion) built AOT inside
  the container and run under `valgrind --leak-check=full
  --errors-for-leak-kinds=definite,indirect` with exact-stdout comparison:
  **48/49 clean — zero leaks, zero invalid accesses, exact output.** The
  memory/drop-critical families additionally ran JIT under Valgrind.
- **Findings:**
  - **I-262** (hygiene): the *compiler process* leaks ~2.4 KB of chumsky
    recursive-parser allocations per parse — visible only when valgrinding
    `ryo run` itself; the repo suite valgrinds compiled programs, so CI missed
    it. Unrelated to generated-code memory behavior.
  - `recursion-concat-chain` AOT hits Valgrind 3.24's ARM64 frontend gap
    (`unhandled instruction 0xF830EBE4`, `load_store` class) — a Valgrind
    limitation, not a program defect (native runs are clean, and the family
    passed the macOS hardened-allocator pass). Excluded from the 48/49 count.
- **Result: CI-grade memory evidence acquired; one compiler-hygiene leak
  filed (I-262), no generated-code memory defects.**
- **Artifacts:** `/tmp/ryo-linux-fuzz/` (manifest.json, results.json, container
  logs); harness script `/tmp/ryo-linux-fuzz/run_container.sh`

### 7. Compiler-input size torture

- **Date:** 2026-11-04
- **Compiler under test:** `e82f503` (main HEAD)
- **Track:** size — 120 cases, seeds 4000–4119, AOT every 6th case, per-stage
  wall times recorded; 11 families: many functions, long string/bytes literals,
  independent & chained statements, big structs, big matches, many string
  literals, mixed files, chain callers, and an oversize bytes-escape probe.
- **Result: 109/120 clean; the 11 crashes are the intentional I-264 probes**
  (bytes literals with 30k–55k hex escapes, deterministic SIGABRT). No panics,
  no timeouts, no output mismatches; AOT exact 18/18. I-264 is fixed in `6dede11` (raw-token lexing runs on an input-sized
  stack; the probes now pass).
- **Scaling evidence** (JIT wall time, this host): linear families — 10MB
  string literal 0.57s, 5MB bytes 0.3s, 8k functions 2.9s, 5000-field struct
  0.85s, 1000-arm match 0.35s, 1000-deep chain 0.8s. Quadratic families
  (I-263): statements 5k→5.0s, 10k→18.6s, 20k→71.5s; chained statements
  10k→10.7s→15k 22.6s; string-literal bindings 1k→1.2s, 3k→10.2s, 5k→28s.
- **Findings filed:** I-263 (quadratic per-statement compile cost; parse of the
  same file is 1.2s so the bottleneck is sema/ownership) and I-264 (bytes
  escape stack overflow past ~25k escapes — Logos DFA recursion per escape,
  not the escape decoder; fixed in `6dede11`, repro in `bug_reports/`).
- **Artifacts:** `/tmp/ryo-size-fuzz/run-001/` (manifest.json, harness.py,
  per-case logs and timings); harness at `/tmp/ryo-size-fuzz/harness.py`

## Coverage Baselines (do not rerun below these commits)

| Feature area | Fuzzed at | On main as | Notes |
|---|---|---|---|
| Error declarations, unions, `try`/`catch`, `.message()`, UTF-8 conversions, error ABI | M13 branch (→ `9366c5c`) + `a0b6ddf` | `e82f503` | two independent campaigns, one of which found and fixed the early defects |
| Error namespaces | `a0b6ddf` | `e82f503` | 400 cases, clean |
| Ownership (moves, borrows, strview, drops), payload moves, error-slot memory shape | `e82f503` | `e82f503` | 480 cases + hardened malloc, clean |
| Match/enums (arm order, payload moves, exhaustiveness, int-literal dispatch, `%` guards) | `e82f503` | `e82f503` | 480-case campaign, clean |
| UTF-8 slicing, bytes/bytesview, UTF-8 conversions | `e82f503` | `e82f503` | same campaign |
| Numeric edges (overflow guards, float formatting, NaN/Inf, boundary comparisons) | `e82f503` | `e82f503` | same campaign |
| General grammar recovery (malformed sources, multi-error files, bad tokens/indent) | `e82f503` | `e82f503` | 360-case campaign, clean except I-261 (fixed in 3747e17) |
| Deep recursion / deep programs (to 20k depth, 1500-fns chains, 9000 parens) | `e82f503` | `e82f503` | same campaign; oversize expr chains → I-261 (fixed in 3747e17) |
| Generated-code memory safety (Valgrind on AOT fuzz binaries, linux/arm64) | `e82f503` | `e82f503` | 48/49 clean; I-262 compiler-process leak; 1 valgrind ARM64 gap |
| Input size scaling (to 10MB literals, 8k fns, 20k stmts, 5k-field structs) | `e82f503` | `e82f503` | clean except I-263 quadratic costs; I-264 escape overflow fixed in `6dede11` |

## Not Yet Fuzzed (candidates, in priority order)

1. **Windows (UTM/x86_64 cross or CI job)** — all campaigns so far are
   macOS/Linux arm64 only; the Windows CI job runs tests but no fuzz cases.

## Rerun Policy

- A baseline above stays valid until **main advances with changes in that feature
  area** — check `git log <baseline>..HEAD -- <area crates>` before rerunning; if the
  diff is empty, the campaign still covers HEAD.
- Per the guide, fuzz after parser, type-system, ownership, control-flow, runtime, or
  backend changes; a PR-branch campaign (like #2) counts for main once merged if the
  squash content matches (verify with `git diff` as done above).
- Keep harnesses and outputs in `/tmp/<campaign>-fuzz/`, manifests recording the
  compiler commit, and append new campaigns to this register.
