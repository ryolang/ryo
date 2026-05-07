#!/bin/bash
set -e

echo "Building benchmarks..."
rustc -O fib.rs -o fib_rs
go build -o fib_go fib.go
swiftc -O fib.swift -o fib_swift
kotlinc fib.kt -include-runtime -d fib.jar
cargo build --release > /dev/null 2>&1
ryo_bin="../../target/release/ryo"
$ryo_bin build fib.ryo > /dev/null

echo ""
echo "-------------------"
echo "Language Versions"
echo "-------------------"
echo "Rust:     $(rustc --version | cut -d' ' -f2)"
echo "Kotlin:   $(kotlinc -version 2>&1 | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' | head -1)"
echo "Go:       $(go version | awk '{print $3}' | sed 's/go//')"
echo "Swift:    $(swiftc --version | head -1 | awk '{print $4}')"
echo "Ryo:      $($ryo_bin --version 2>&1 | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' || echo 'dev')"
echo "Bun:      $(~/.bun/bin/bun --version)"
echo "Elixir:   $(elixir --version | grep Elixir | awk '{print $2}')"
echo "Ruby:     $(ruby --version | awk '{print $2}')"
echo "Python:   $(uv run --python 3.14 python --version | awk '{print $2}')"
echo "Julia:    $(julia --version 2>/dev/null | awk '{print $3}' || echo 'not installed')"
echo "Java:     $(java -version 2>&1 | head -1 | awk -F '"' '{print $2}')"

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

    printf "%-12s %s\n" "[$name]" "$mem_out"
}

# Run them once just to collect memory usage
measure_mem "Rust" ./fib_rs
measure_mem "Kotlin" java -jar fib.jar
measure_mem "Go" ./fib_go
measure_mem "Swift" ./fib_swift
measure_mem "Ryo (AOT)" ./fib
measure_mem "Ryo (JIT)" $ryo_bin run fib.ryo
measure_mem "Bun (TS)" ~/.bun/bin/bun run fib.ts
measure_mem "Elixir" elixir fib.exs
measure_mem "Ruby" ruby fib.rb
measure_mem "Python" uv run --python 3.14 fib.py
measure_mem "Julia" julia fib.jl

echo ""
echo "-------------------"
echo "Running Benchmarks (fib 40) using hyperfine"
echo "-------------------"

hyperfine --warmup 1 \
  './fib_rs' \
  'java -jar fib.jar' \
  './fib_go' \
  './fib_swift' \
  './fib' \
  "$ryo_bin run fib.ryo" \
  "$HOME/.bun/bin/bun run fib.ts" \
  'elixir fib.exs' \
  'ruby fib.rb' \
  'uv run --python 3.14 fib.py' \
  'julia fib.jl'
