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

## 10. Build the Python Harness

Use Python 3's standard library; no third-party packages are needed. Keep the script and its output outside the repository. The components have these responsibilities:

- `argparse` parses the base seed, case count, AOT frequency, compiler identity, output directory, timeout, and per-stream byte limit.
- `random.Random` gives each case an independent, reproducible generator. Use it to choose valid types and control-flow shapes, or to mutate an AST-like program description before rendering source. Do not use it to corrupt arbitrary bytes.
- `subprocess.Popen` isolates each compiler invocation with an argument list and `shell=False`. Two `threading.Thread` readers stream stdout and stderr to per-case files, preserving at most the configured number of bytes per stream and discarding the rest. `wait(timeout=...)` contains hangs; record a timeout instead of retrying indefinitely.
- `pathlib.Path` gives each case a separate source, log, and AOT-binary directory. Separate paths prevent parallel cases from overwriting one another. `os.name` selects the platform's executable suffix.
- `json` writes the seed, source hash, commands, exit statuses, classification, and log paths to a machine-readable manifest. `platform` and `sys` record host details and the exact harness command so the report identifies the complete campaign.
- `concurrent.futures.ThreadPoolExecutor` runs independent generation tracks or compiler processes concurrently. Give each worker its own seed and directory; do not share a generator or result file between workers.
- `hashlib.sha256` identifies the exact source bytes. Include the compiler commit as well: a seed alone is not a sufficient repro identity after generator changes.
- `tempfile.TemporaryDirectory` and `shutil.copy2` manage scratch binaries and preserved repros. Keep the manifest and minimized failures in a chosen output directory; let temporary build products disappear when the campaign ends.

The following skeleton is runnable as written. It includes a valid-print control and a valid nonzero-exit mutation to exercise the exit-status oracle. Replace `generate(seed, track)` with the feature's type-directed generators and structure-aware mutations. Supply each valid case's expected exit and stdout, or each invalid case's expected diagnostic. Keep expected-invalid cases separate from valid cases rather than treating any compiler error as a successful mutation.

```python
import argparse
import concurrent.futures
import hashlib
import json
import os
import platform
import random
import shutil
import subprocess
import sys
import tempfile
import threading
from pathlib import Path


def generate(seed, track):
    rng = random.Random(seed)
    value = rng.randrange(100)
    if track == "control":
        return {
            "seed": seed,
            "family": "valid-print",
            "source": f'print("{value}")\n',
            "expected_valid": True,
            "expected_exit": 0,
            "expected_stdout": f"{value}",
        }
    return {
        "seed": seed,
        "family": "nonzero-exit",
        "source": f'fn main():\n\tprint("{value}")\n\tprocess_exit(3)\n',
        "expected_valid": True,
        "expected_exit": 3,
        "expected_stdout": f"{value}",
    }


def execute(command, case_dir, name, timeout, max_output_bytes):
    process = subprocess.Popen(
        command,
        shell=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    truncated = {"stdout": False, "stderr": False}

    def copy_stream(source, destination_path, stream_name):
        with destination_path.open("wb") as destination:
            remaining = max_output_bytes
            while chunk := source.read(65536):
                kept = chunk[:remaining]
                destination.write(kept)
                remaining -= len(kept)
                if len(kept) < len(chunk):
                    truncated[stream_name] = True
        source.close()

    streams = (("stdout", process.stdout), ("stderr", process.stderr))
    threads = [
        threading.Thread(
            target=copy_stream,
            args=(source, case_dir / f"{name}.{stream_name}", stream_name),
        )
        for stream_name, source in streams
    ]
    for thread in threads:
        thread.start()
    try:
        status = process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
        status = "timeout"
    for thread in threads:
        thread.join()
    return {
        "command": command,
        "status": status,
        "stdout": (case_dir / f"{name}.stdout").read_bytes(),
        "truncated": any(truncated.values()),
    }


def run_case(case, args, track_dir):
    case_dir = track_dir / str(case["seed"])
    case_dir.mkdir()
    source = case_dir / "case.ryo"
    source.write_text(case["source"], encoding="utf-8")
    ryo = str(args.ryo.resolve())
    jit = execute(
        [ryo, "run", str(source)], case_dir, "jit", args.timeout, args.max_output_bytes
    )
    stderr = (case_dir / "jit.stderr").read_text(encoding="utf-8", errors="replace")
    jit_exact = (
        case["expected_valid"]
        and jit["status"] == case["expected_exit"]
        and not jit["truncated"]
        and jit["stdout"] == case["expected_stdout"].encode()
    )

    if jit["status"] == "timeout":
        outcome = "timeout"
    elif jit["truncated"]:
        outcome = "output_truncated"
    elif "panicked at" in stderr:
        outcome = "compiler_panic"
    elif case["expected_valid"]:
        if jit["status"] != case["expected_exit"]:
            outcome = "unexpected_exit"
        elif not jit_exact:
            outcome = "output_mismatch"
        else:
            outcome = "ok"
    else:
        diagnostic = case.get("expected_diagnostic")
        valid_diagnostic = (
            jit["status"] == 1
            and diagnostic is not None
            and f"[{diagnostic}]" in stderr
            and "internal error" not in stderr
        )
        outcome = "expected_diagnostic" if valid_diagnostic else "invalid_oracle_failure"

    aot = None
    if case["expected_valid"] and case["seed"] % args.aot_every == 0:
        with tempfile.TemporaryDirectory(dir=case_dir) as scratch:
            scratch_source = Path(scratch) / "case.ryo"
            shutil.copy2(source, scratch_source)
            binary = scratch_source.with_suffix(".exe" if os.name == "nt" else "")
            build = execute(
                [ryo, "build", str(scratch_source)],
                case_dir, "aot-build", args.timeout, args.max_output_bytes,
            )
            aot = {
                "build_command": build["command"],
                "build_status": build["status"],
                "build_stdout": str(case_dir / "aot-build.stdout"),
                "build_stderr": str(case_dir / "aot-build.stderr"),
                "build_truncated": build["truncated"],
            }
            if build["status"] == 0 and not build["truncated"]:
                run = execute(
                    [str(binary)], case_dir, "aot-run", args.timeout, args.max_output_bytes
                )
                aot["run_command"] = run["command"]
                aot["run_status"] = run["status"]
                aot["run_stdout"] = str(case_dir / "aot-run.stdout")
                aot["run_stderr"] = str(case_dir / "aot-run.stderr")
                aot["expected_exit"] = run["status"] == case["expected_exit"]
                aot["exact_output"] = (
                    not run["truncated"]
                    and run["stdout"] == case["expected_stdout"].encode()
                )
                aot["output_truncated"] = run["truncated"]
                if outcome == "ok" and not (aot["expected_exit"] and aot["exact_output"]):
                    outcome = "aot_failure"
            elif outcome == "ok":
                outcome = "aot_build_failure"

    return {
        **{key: value for key, value in case.items() if key != "source"},
        "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
        "source": str(source),
        "jit_command": jit["command"],
        "jit_status": jit["status"],
        "jit_stdout": str(case_dir / "jit.stdout"),
        "jit_stderr": str(case_dir / "jit.stderr"),
        "jit_truncated": jit["truncated"],
        "aot": aot,
        "outcome": outcome,
        "backend_completed": (
            case["expected_valid"]
            and jit["status"] == case["expected_exit"]
            and not jit["truncated"]
        ),
        "jit_exact": jit_exact,
    }


def run_track(name, seeds, args):
    track_dir = args.out / name
    track_dir.mkdir(parents=True)
    results = [run_case(generate(seed, name), args, track_dir) for seed in seeds]
    summary = {
        "track": name,
        "families": sorted({r["family"] for r in results}),
        "generated": len(results),
        "valid_generated": sum(r["expected_valid"] for r in results),
        "backend_completed": sum(r["backend_completed"] for r in results),
        "jit_exact": sum(r["jit_exact"] for r in results),
        "expected_diagnostics": sum(r["outcome"] == "expected_diagnostic" for r in results),
        "aot_executed": sum(r["aot"] is not None and "run_status" in r["aot"] for r in results),
        "aot_exact": sum(
            bool(
                r["aot"]
                and r["aot"].get("exact_output")
                and r["aot"].get("expected_exit")
            )
            for r in results
        ),
    }
    (track_dir / "results.json").write_text(
        json.dumps({"summary": summary, "cases": results}, indent=2) + "\n",
        encoding="utf-8",
    )
    return summary


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ryo", type=Path, required=True)
    parser.add_argument("--compiler-commit", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--count", type=int, default=20)
    parser.add_argument("--aot-every", type=int, default=5)
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--max-output-bytes", type=int, default=1_048_576)
    args = parser.parse_args()
    if args.count < 1 or args.aot_every < 1 or args.max_output_bytes < 1:
        parser.error("--count, --aot-every, and --max-output-bytes must be positive")
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    shutil.copy2(__file__, args.out / "harness.py")
    seeds = list(range(args.seed, args.seed + args.count))
    tracks = {"control": seeds[::2], "mutation": seeds[1::2]}
    with concurrent.futures.ThreadPoolExecutor() as executor:
        futures = [executor.submit(run_track, name, values, args) for name, values in tracks.items()]
        summaries = [future.result() for future in futures]
    manifest = {
        "compiler": str(args.ryo.resolve()),
        "compiler_commit": args.compiler_commit,
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": platform.python_version(),
        },
        "harness_command": [sys.executable, *sys.argv],
        "parameters": {
            "seed": args.seed,
            "count": args.count,
            "aot_every": args.aot_every,
            "timeout": args.timeout,
            "max_output_bytes": args.max_output_bytes,
        },
        "seed_range": [seeds[0], seeds[-1]],
        "mutation_families": {
            summary["track"]: summary["families"] for summary in summaries
        },
        "aot_selection": {
            "strategy": "seed_modulo",
            "modulus": args.aot_every,
            "remainder": 0,
        },
        "tracks": summaries,
    }
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
```

Run it against a freshly built compiler, for example:

```bash
python3 /tmp/ryo-error-fuzz/harness.py \
  --ryo target/debug/ryo \
  --compiler-commit "$(git rev-parse HEAD)" \
  --out /tmp/ryo-error-fuzz/run-001 \
  --seed 1000 --count 100 --aot-every 10 --timeout 15
```

`backend_completed` counts only valid programs that completed JIT compilation and execution with the expected exit status and untruncated output. It does not pretend that an exit code identifies the parser or semantic-analysis stage. For stage-specific coverage, use documented diagnostic categories or explicit compiler instrumentation, and count internal/backend failures separately. Compare `backend_completed` with the number of valid generated programs: a shrinking ratio is a reason to inspect the generator and oracles before reporting compiler defects. Preserve failing cases and their logs; the temporary AOT directory is only for build artifacts.

## 11. When to Use `cargo-fuzz`

[`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) is not limited to Rust source input. A Rust fuzz target can generate any structured data, including Ryo source programs. It is useful when you want a persistent, high-throughput, in-process harness that calls compiler crates directly and uses libFuzzer coverage guidance.

It is not a structure-aware generator and is not a convenient process-based compiler runner. You still need `arbitrary`, `mutatis`, or an equivalent type-directed generator, plus the same oracles, coverage counters, minimization, and independent verification described above. A new fuzz target also adds a nightly Rust requirement, a C++ compiler, Unix-like x86-64/AArch64 support, and harness code; do not add those dependencies merely to reproduce a small campaign.

For a small campaign that invokes `ryo run` and `ryo build` as separate processes, the standard-library Python harness in this guide is simpler. Consider `cargo-fuzz` when in-process execution and long-running coverage-guided fuzzing justify a dedicated Rust target.

## References

- Spec: [Ryo specification](../../docs/specification.md)
- Roadmap: [implementation_roadmap.md](../../docs/dev/implementation_roadmap.md)
- External: [Structure-Aware Fuzzing Experiment](https://fitzgen.com/2026/06/01/structure-aware-fuzzing-experiment.html)
- External: [How Fuzzy Are Your Fuzzers?](https://fitzgen.com/2022/10/24/how-fuzzy-are-your-fuzzers.html)
- External: [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz)
