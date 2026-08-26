#!/bin/bash
set -e

# Check for prerequisites
if ! command -v hyperfine &> /dev/null; then
    echo "Error: 'hyperfine' is not installed or not in PATH. Please install it to run performance benchmarks."
    exit 1
fi

if ! command -v rustc &> /dev/null; then
    echo "Error: 'rustc' is not installed or not in PATH."
    exit 1
fi

if ! command -v swiftc &> /dev/null; then
    echo "Error: 'swiftc' is not installed or not in PATH."
    exit 1
fi

echo "Building benchmarks..."
(cd ../.. && cargo build --release > /dev/null)
rustc -O doubling_concat.rs -o doubling_concat_rs
swiftc -O doubling_concat.swift -o doubling_concat_swift
ryo_bin="../../target/release/ryo"
$ryo_bin build doubling_concat.ryo > /dev/null

echo ""
echo "-------------------"
echo "Compiler Version"
echo "-------------------"
echo "Rust:     $(rustc --version | cut -d' ' -f2)"
echo "Swift:    $(swiftc --version | head -1 | awk '{for (i = 1; i < NF; i++) if ($i == "Swift" && $(i+1) == "version") { print $(i+2); exit }}')"
echo "Ryo:      $($ryo_bin --version 2>&1 || echo 'dev')"

echo ""
echo "-------------------"
echo "Memory Usage (Maximum Resident Set Size)"
echo "-------------------"
_OS="$(uname -s)"
measure_mem() {
    local name=$1
    shift

    local mem_kb
    local mem_out
    case "$_OS" in
      Darwin*)
        # /usr/bin/time -l reports bytes on macOS; convert to KB
        mem_kb=$( ( /usr/bin/time -l "$@" > /dev/null ) 2>&1 | awk '/maximum resident set size/ {printf "%d", $1 / 1024; exit}' )
        ;;
      Linux*)
        mem_kb=$( { /usr/bin/time -f "%M" "$@" > /dev/null; } 2>&1 | tail -n1 )
        ;;
      *)
        mem_kb=""
        ;;
    esac

    if [[ -n "$mem_kb" ]]; then
      mem_out=$(awk -v kb="$mem_kb" 'BEGIN { printf "%.2f MB", kb / 1024 }')
    else
      mem_out="N/A"
    fi

    printf "%-28s %s\n" "[$name]" "$mem_out"
}

# Run once each to collect memory usage
measure_mem "Rust" ./doubling_concat_rs
measure_mem "Swift" ./doubling_concat_swift
measure_mem "Ryo (AOT)" ./doubling_concat
measure_mem "Ryo (JIT)" $ryo_bin run doubling_concat.ryo

echo ""
echo "-------------------"
echo "Running Benchmarks (20 doublings, 16 B → 16 MiB) using hyperfine"
echo "-------------------"

hyperfine --warmup 3 --shell=none \
  './doubling_concat_rs' \
  './doubling_concat_swift' \
  './doubling_concat' \
  "$ryo_bin run doubling_concat.ryo"
