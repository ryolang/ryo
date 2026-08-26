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
rustc -O string_building.rs -o string_building_rs
swiftc -O string_building.swift -o string_building_swift
ryo_bin="../../target/release/ryo"
$ryo_bin build string_building.ryo > /dev/null

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
measure_mem "Rust" ./string_building_rs
measure_mem "Swift" ./string_building_swift
measure_mem "Ryo (AOT)" ./string_building
measure_mem "Ryo (JIT)" $ryo_bin run string_building.ryo

echo ""
echo "-------------------"
echo "Running Benchmarks (50,000 concat iterations) using hyperfine"
echo "-------------------"

hyperfine --warmup 3 --shell=none \
  './string_building_rs' \
  './string_building_swift' \
  './string_building' \
  "$ryo_bin run string_building.ryo"
