# Benchmarks Directory Conventions

Entry point for agents working under `benchmarks/`. The full runner contract, checkpoint convention, and philosophy live in [README.md](README.md) and the per-suite READMEs — this file only covers how to run suites and refresh the checkpoint tables.

## Running Benchmarks

All suites are driven from the repo root by the central runner `run.py` (PEP 723 — `uv run` handles the interpreter):

```bash
uv run benchmarks/run.py run --benchmarks collatz     # one suite (drop the flag for all suites)
uv run benchmarks/run.py run --benchmarks fibonacci --languages rust,ryo_aot
uv run benchmarks/run.py run --benchmarks collatz --dry-run    # build + checksum gate only, no hyperfine
uv run benchmarks/run.py run --strict                  # fail before benchmarking if a toolchain is missing
uv run benchmarks/run.py run --ryo-bin PATH            # override the ryo binary (default: target/release/ryo)
uv run benchmarks/run.py run --json PATH               # incremental run JSON, written after each suite
```

* `collatz` is the fast smoke suite; fibonacci with all arms takes 20–60 min — run it in the background.
* `run` always discovers the git-ignored local suites (`csv_parse`, `json_parse`, `startup_latency`); `table`/`update-readme` skip them unless you pass `--include-ignored`.
* Never run two benchmarks concurrently — CPU contention skews the timings.

## Updating Checkpoint Tables

Run the full suite before each release and after any change that touches generated-code shape (see the checkpoint convention in [README.md](README.md)):

```bash
uv run benchmarks/run.py run             # measure; appends to benchmarks/data/<suite>.json
uv run benchmarks/run.py update-readme   # splice regenerated tables into suite READMEs, the root summary, and the matrix section
uv run benchmarks/run.py update-readme   # re-run: must change nothing (fixed point)
uv run benchmarks/run.py matrix          # print the matrix without splicing
```

* `benchmarks/data/<suite>.json` is the only source of truth — keyed `<suite>/<group>/<arm>`, newest-20 entries per key, fed only by real `run` invocations.
* Never hand-edit a generated block or copy numbers into it (`<!-- BENCH:START/END -->` per suite README, `<!-- BENCH-SUMMARY:START/END -->` and `<!-- BENCH-MATRIX:START/END -->` in the root README) — `update-readme` owns them.
* Hand-written dated checkpoint history lives below the generated block in each suite README.
* Prose citing specific numbers (e.g. "1.7 ms") goes stale as the table moves; `update-readme` warns about it on stderr — warning only, not an error.
