#!/bin/bash
set -e

echo "Building benchmarks..."
rustc -O fib.rs -o fib_rs
go build -o fib_go fib.go
swiftc -O fib.swift -o fib_swift
cargo build --release > /dev/null 2>&1
ryo_bin="../../target/release/ryo"
$ryo_bin build fib.ryo > /dev/null

echo "-------------------"
echo "Memory Usage (Maximum Resident Set Size)"
echo "-------------------"
measure_mem() {
    local name=$1
    shift
    # Run the command in a subshell, routing its stdout to /dev/null
    # then the time command's stderr is piped to grep
    local mem_out
    mem_out=$( ( /usr/bin/time -l "$@" > /dev/null ) 2>&1 | grep "maximum resident set size" | awk '{printf "%.2f MB", $1 / 1024 / 1024}' )
    printf "%-12s %s\n" "[$name]" "$mem_out"
}

# Run them once just to collect memory usage
measure_mem "Rust" ./fib_rs
measure_mem "Go" ./fib_go
measure_mem "Swift" ./fib_swift
measure_mem "Ryo (AOT)" ./fib
measure_mem "Ryo (JIT)" $ryo_bin run fib.ryo
measure_mem "Bun (TS)" ~/.bun/bin/bun run fib.ts
measure_mem "Elixir" elixir fib.exs
measure_mem "Python" python3 fib.py

echo ""
echo "-------------------"
echo "Running Benchmarks (fib 40) using hyperfine"
echo "-------------------"

hyperfine --warmup 1 \
  './fib_rs' \
  './fib_go' \
  './fib_swift' \
  './fib' \
  "$ryo_bin run fib.ryo" \
  '~/.bun/bin/bun run fib.ts' \
  'elixir fib.exs' \
  'python3 fib.py'
