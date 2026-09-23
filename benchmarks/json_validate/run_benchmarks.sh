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

if ! command -v python3 &> /dev/null; then
    echo "Error: 'python3' is not installed or not in PATH."
    exit 1
fi

echo "Building benchmarks..."
(cd ../.. && cargo build --release > /dev/null)
rustc -O json_validate.rs -o json_validate_rs
ryo_bin="../../target/release/ryo"
$ryo_bin build json_validate.ryo > /dev/null

echo ""
echo "-------------------"
echo "Compiler Version"
echo "-------------------"
echo "Rust:     $(rustc --version | cut -d' ' -f2)"
echo "Python:   $(python3 --version | cut -d' ' -f2)"
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
measure_mem "Rust" ./json_validate_rs
measure_mem "Python" python3 json_validate.py
measure_mem "Ryo (AOT)" ./json_validate
measure_mem "Ryo (JIT)" $ryo_bin run json_validate.ryo

echo ""
echo "-------------------"
echo "Running Benchmarks (30,000-record JSON document, 12 validation passes) using hyperfine"
echo "-------------------"

hyperfine --warmup 3 --shell=none \
  './json_validate_rs' \
  'python3 json_validate.py' \
  './json_validate' \
  "$ryo_bin run json_validate.ryo"
