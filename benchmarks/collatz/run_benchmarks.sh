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
rustc -O collatz.rs -o collatz_rs
swiftc -O collatz.swift -o collatz_swift
ryo_bin="../../target/release/ryo"
$ryo_bin build collatz.ryo > /dev/null

echo ""
echo "-------------------"
echo "Compiler Version"
echo "-------------------"
echo "Rust:     $(rustc --version | cut -d' ' -f2)"
echo "Swift:    $(swiftc --version | head -1 | awk '{print $4}')"
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
measure_mem "Rust" ./collatz_rs
measure_mem "Swift" ./collatz_swift
measure_mem "Ryo (AOT)" ./collatz
measure_mem "Ryo (JIT)" $ryo_bin run collatz.ryo

echo ""
echo "-------------------"
echo "Running Benchmarks (seeds 1..1,000,000) using hyperfine"
echo "-------------------"

hyperfine --warmup 3 --shell=none \
  './collatz_rs' \
  './collatz_swift' \
  './collatz' \
  "$ryo_bin run collatz.ryo"
