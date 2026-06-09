#!/bin/bash
set -e

# Check for prerequisites
if ! command -v rustc &> /dev/null; then
    echo "Error: 'rustc' is not installed or not in PATH."
    exit 1
fi

if ! command -v hyperfine &> /dev/null; then
    echo "Error: 'hyperfine' is not installed or not in PATH. Please install it to run performance benchmarks."
    exit 1
fi

echo "Building benchmarks..."
rustc -O eager_destruction.rs -o eager_destruction_rs
rustc -O eager_destruction_manual_drop.rs -o eager_destruction_manual_drop_rs
cargo build --release > /dev/null
ryo_bin="../../target/release/ryo"
$ryo_bin build eager_destruction.ryo > /dev/null

echo ""
echo "-------------------"
echo "Language Versions"
echo "-------------------"
echo "Rust:     $(rustc --version | cut -d' ' -f2)"
echo "Ryo:      $($ryo_bin --version 2>&1 | grep -o '[0-9]\+\.[0-9]\+\.[0-9]' || echo 'dev')"

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

# Run them once to collect memory usage
measure_mem "Rust (Scope-Based)" ./eager_destruction_rs
measure_mem "Rust (Manual Drop)" ./eager_destruction_manual_drop_rs
measure_mem "Ryo (AOT, Eager)" ./eager_destruction
measure_mem "Ryo (JIT, Eager)" $ryo_bin run eager_destruction.ryo

echo ""
echo "-------------------"
echo "Running Benchmarks (recursion depth: 50,000) using hyperfine"
echo "-------------------"

hyperfine --warmup 3 --shell=none \
  './eager_destruction_rs' \
  './eager_destruction_manual_drop_rs' \
  './eager_destruction' \
  "$ryo_bin run eager_destruction.ryo"
