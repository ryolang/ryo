# Structure-Aware Fuzzing for Ryo Features

**Status:** Complete

This guide explains how to fuzz Ryo language features without adding a fuzzer or generated programs to the repository. It is a repeatable process, not a one-off campaign. Use it after parser, type-system, ownership, control-flow, runtime, or backend changes. Error types and unions appear only as examples.

The approach follows the [structure-aware fuzzing experiment](https://fitzgen.com/2026/06/01/structure-aware-fuzzing-experiment.html) and the advice in [How Fuzzy Are Your Fuzzers?](https://fitzgen.com/2022/10/24/how-fuzzy-are-your-fuzzers.html): generate inputs that reach the code you mean to test, and measure that reach instead of trusting a large execution count.

## 1. Set Up an Isolated Campaign

Build the compiler under test, then put every harness, generated source, binary, log, minimized repro, and report outside the checkout:

```bash
cargo build
mkdir -p /tmp/ryo-error-fuzz/{grammar,inference,ownership,backend}
```

Use the resulting binary explicitly:

```bash
RYO=target/debug/ryo
"$RYO" run /tmp/ryo-error-fuzz/example.ryo
"$RYO" build /tmp/ryo-error-fuzz/example.ryo
```

Do not edit the compiler while fuzzing it. If the campaign must use a worktree, record its path and commit SHA in the report. Rebuild immediately before final verification so the repros run against the exact final source.

A small Python script using only the standard library is sufficient. No fuzzing dependency is required.

## 2. Start With Valid Seed Programs

Write a small seed corpus by hand. First enumerate the surface and execution paths of the feature under test: declarations and constructors, type forms, expressions and statements, valid and invalid control flow, ownership boundaries, runtime behavior, and backend representations. Then make each seed small and unambiguous.

For example, an error-union campaign might cover:

- unit, positional, and named error declarations and constructors;
- `E!T`, `(A | B)!T`, and inferred `!T`;
- direct returns and returned error unions;
- direct `catch as err:`, bare `catch:`, and nested handlers;
- `try` chains, diamonds, and early returns;
- explicit-only and inferred-signature recursion;
- raw bindings, reassignment, field assignment, and call arguments;
- `.message()` and valid/invalid UTF-8 conversion;
- scalar, string, struct, enum, and nested-aggregate payloads.

Verify every seed before using it. A broken seed corpus produces misleading failures and wastes triage time.

## 3. Mutate Structure, Not Bytes

Prefer valid, type-directed generation and structure-aware mutation over random bytes. Useful mutations include:

- change a declaration between unit, positional, and named payloads;
- reorder or duplicate union members and declarations;
- switch explicit signatures to inferred signatures;
- convert returned unions into `try` propagation;
- add direct, mutual, or mixed call-graph cycles;
- switch direct, bare, and nested catch forms;
- reorder arms and change payload bindings;
- vary whether a payload is consumed before `.message()`;
- change success and payload types across `int`, `str`, `void`, structs, and enums;
- alternate valid, invalid, empty, and non-ASCII UTF-8 inputs;
- reuse one result slot for success and error alternatives.

Keep a separate expected-invalid corpus for bad declarations, narrowing, invalid recursion, direct union use, non-exhaustive matches, borrowed payload moves, and malformed catch syntax. These should produce structured diagnostics, never a compiler panic or error-sentinel leak.

Use a deterministic base seed and derive a separate seed for each generated case. Record the seed, family, mutation, expected outcome, expected output, and source hash in a manifest.

## 4. Define Oracles Before Running

Classify cases before execution:

| Class | Required outcome |
|---|---|
| Valid | Compile and produce exact expected stdout in JIT and AOT |
| Expected-invalid | Structured language diagnostic; no panic, hang, sentinel leak, or backend execution |
| Inspection | Exact canonical inferred signatures, including `(no errors)` where appropriate |
| Ownership | Intended use-after-move diagnostic, or valid output with no double free or use-after-free |

Avoid explicit `panic` in generated programs unless panic behavior is the subject of the campaign. Otherwise a crash is meaningful evidence, not an expected result.

A clean `E0012`-style diagnostic is not automatically correct. Compare it with the language rule for that input. For example, a struct literal with a compatible success value in a union field must compile.

## 5. Measure Useful Coverage

Do not use the number of generated programs as evidence of effectiveness. Track at least:

- generated cases;
- parse accepted;
- sema accepted;
- backend reached;
- JIT success with exact output;
- AOT build and execution success with exact output;
- expected diagnostics;
- unexpected compiler panic/internal/codegen error;
- runtime crash or signal;
- timeout;
- output mismatch;
- memory-like failure.

A valid campaign should reach backend for every valid seed. If valid generation drifts toward early rejection, treat that as a fuzzer failure and fix the generator before interpreting results.

For special features, add dedicated counters. Examples include actual UTF-8 conversions, hidden-dispatch executions, non-empty error alternatives, aggregate indirect returns, and shared payload reads after a move. These counters catch a fuzzer that silently stops exercising its target.

## 6. Run Independent Tracks in Parallel

Independent domains can run concurrently when they use separate directories and do not share generated state:

1. **Grammar and recovery:** declarations, constructors, union syntax, precedence, malformed catch forms, and source `Ok`/`Err` rejection.
2. **Inference and coercion:** explicit/inferred signatures, empty sets, call graphs, widening, narrowing, assignments, and call arguments.
3. **Catch and ownership:** direct/bare/nested catch, payload moves, shared error access, messages, and cleanup.
4. **UTF-8 and backend ABI:** alternating valid/invalid conversions, nested aggregates, indirect returns, result-slot reuse, and large inactive payloads.

Give every command a timeout. Run a representative AOT subset, not only JIT. Include exact-output checks in both modes.

## 7. Triage Failures

Separate these categories immediately:

- expected language diagnostics;
- fuzzer/generator/oracle mistakes;
- wrong inferred types or coercions;
- compiler panics and error-sentinel leaks;
- backend/ABI failures;
- runtime crashes, output mismatches, and memory-like failures.

First inspect the complete stderr and exit status. Then compare with the closest working control: same declaration without constructor use, empty union without `try`, recursion with a scalar return, or `try` without `catch`. A control isolates the feature that actually causes the failure.

Do not treat every new failure as a new bug. Group by signature and root cause before minimizing.

## 8. Minimize and Verify

For each unique defect:

1. Start with a generated repro.
2. Remove unrelated declarations, functions, catches, payloads, and expressions one at a time.
3. Rerun after every deletion and preserve the exact failure signature.
4. Keep a working control beside the minimized repro.
5. Run the minimized repro at least twice through `ryo run` and once through `ryo build`.
6. Rebuild the final compiler and repeat verification on that exact binary.
7. Read only enough source to confirm the root cause; do not fix it during the fuzzing campaign.

Save the minimized source, stdout, stderr, exit status, backtrace where relevant, control outcome, and a short root-cause note.

## 9. Report Clearly

Include in every report:

- compiler path, commit SHA, host, and harness command;
- seed ranges and mutation families;
- exact coverage metrics and AOT selection;
- unique defects with repro paths and expected/actual behavior;
- root cause and working controls;
- minimization and independent verification method;
- coverage gaps, such as single-file programs, one architecture, or no external memory checker;
- confirmation that no repository files were changed.

Zero defects in one track is useful evidence only when its valid inputs reached backend and runtime. Keep that evidence separate from tracks that found defects.

## 10. When to Use `cargo-fuzz`

[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) is not limited to Rust source input. A Rust fuzz target can generate any structured data, including Ryo source programs. It is useful when you want a persistent, high-throughput, in-process harness that calls compiler crates directly and uses libFuzzer coverage guidance.

It is not a structure-aware generator and is not a convenient process-based compiler runner. You still need `arbitrary`, `mutatis`, or an equivalent type-directed generator, plus the same oracles, coverage counters, minimization, and independent verification described above. A new fuzz target also adds a nightly Rust requirement, a C++ compiler, Unix-like x86-64/AArch64 support, and harness code; do not add those dependencies merely to reproduce a small campaign.

For a small campaign that invokes `ryo run` and `ryo build` as separate processes, the standard-library Python harness in this guide is simpler. Consider `cargo-fuzz` when in-process execution and long-running coverage-guided fuzzing justify a dedicated Rust target.

## References

- Spec: [Ryo specification](../../docs/specification.md)
- Roadmap: [implementation_roadmap.md](../../docs/dev/implementation_roadmap.md)
- External: [Structure-Aware Fuzzing Experiment](https://fitzgen.com/2026/06/01/structure-aware-fuzzing-experiment.html)
- External: [How Fuzzy Are Your Fuzzers?](https://fitzgen.com/2022/10/24/how-fuzzy-are-your-fuzzers.html)
- External: [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz)
