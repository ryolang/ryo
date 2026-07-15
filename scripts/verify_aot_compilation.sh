#!/usr/bin/env bash
set -euo pipefail

echo "Building Ryo compiler in release mode..."
cargo build --release

echo "Compiling fibonacci.ryo..."
./target/release/ryo build benchmarks/fibonacci/fib.ryo
mv fib benchmarks/fibonacci/fib
rm -f fib.o

echo "Compiling eager_destruction.ryo..."
./target/release/ryo build benchmarks/eager_destruction/eager_destruction.ryo
mv eager_destruction benchmarks/eager_destruction/eager_destruction
rm -f eager_destruction.o

echo "Running fibonacci binary..."
./benchmarks/fibonacci/fib

echo "Running eager_destruction binary..."
./benchmarks/eager_destruction/eager_destruction

echo "Local AOT Verification Succeeded!"
