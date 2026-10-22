#!/bin/bash
# Builds the 12 AOT benchmark binaries both CodSpeed AOT jobs need
# (`ryo build` writes each binary next to its source; all are gitignored).
# Interim single-source for the list formerly duplicated in
# .github/workflows/codspeed.yml; I-246 will generate it from the
# suite.toml manifests.
set -euo pipefail

RYO_BIN="${1:-./target/release/ryo}"

"$RYO_BIN" build benchmarks/fibonacci/fib.ryo
"$RYO_BIN" build benchmarks/eager_destruction/eager_destruction.ryo
"$RYO_BIN" build benchmarks/string_building/string_building.ryo
"$RYO_BIN" build benchmarks/doubling_concat/doubling_concat.ryo
"$RYO_BIN" build benchmarks/many_small_strings/many_small_strings.ryo
"$RYO_BIN" build benchmarks/string_slicing/string_slicing.ryo
"$RYO_BIN" build benchmarks/mandelbrot/mandelbrot.ryo
"$RYO_BIN" build benchmarks/collatz/collatz.ryo
"$RYO_BIN" build benchmarks/struct_records/struct_records.ryo
"$RYO_BIN" build benchmarks/struct_records_reuse/struct_records_reuse.ryo
"$RYO_BIN" build benchmarks/struct_records_inout/struct_records_inout.ryo
"$RYO_BIN" build benchmarks/json_validate/json_validate.ryo
