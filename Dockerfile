# Dockerfile for running Ryo tests under Linux with leak-detection coverage.
#
# Two complementary detectors:
# - AddressSanitizer (ASan) for use-after-free / double-free in code that
#   IS instrumented (the runtime archive when built with -fsanitize=address;
#   note Cranelift-emitted main is not instrumented, so ASan's leak detection
#   misses leaks originating from generated code).
# - Valgrind for runtime-instrumented leak detection that DOES see
#   Cranelift-emitted allocations. This is what catches I-058-shaped leaks.
FROM rust:latest

# libunwind: required for linking Ryo JIT/AOT binaries on Linux.
# valgrind:  runtime leak detector used by tests/valgrind_smoke.rs.
RUN apt-get update && apt-get install -y \
    libunwind-dev \
    valgrind \
    && rm -rf /var/lib/apt/lists/*

# Set up the working directory inside the container
WORKDIR /usr/src/ryo

# Isolate cargo build cache inside the container to prevent conflict/pollution
# with the host's target directory (especially useful when host is macOS/Windows).
ENV CARGO_TARGET_DIR=/tmp/target

# Default command: build the compiler, pre-install the toolchain sequentially (avoiding parallel download race conditions), and run all tests
CMD ["bash", "-c", "cargo build && cargo run -- toolchain install && cargo test"]
