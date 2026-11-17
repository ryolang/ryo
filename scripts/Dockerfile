# Dockerfile for running Ryo tests under Linux with leak-detection coverage.
#
# Two complementary detectors:
# - AddressSanitizer (ASan) for use-after-free / double-free in code that
#   IS instrumented (the runtime archive when built with -fsanitize=address;
#   note Cranelift-emitted main is not instrumented, so ASan's leak detection
#   misses leaks originating from generated code).
# - Valgrind for runtime-instrumented leak detection that DOES see
#   Cranelift-emitted allocations — the leaks a missing Free in codegen
#   would produce.
FROM rust:latest

# Match the CI Valgrind version; the base image's distribution package is older.
ARG VALGRIND_VERSION=3.26.0
ARG VALGRIND_SHA512=80ebf9197be602fdd4aad0e9277e6ef2fd25ede9cecb6e6d159f6f062c13f62ad907c6b6e7000e82b3404d8b4f9e7bf15813f60d9fed414c7b713a4280c82898
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    bzip2 \
    ca-certificates \
    curl \
    libc6-dbg \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL "https://sourceware.org/pub/valgrind/valgrind-${VALGRIND_VERSION}.tar.bz2" -o /tmp/valgrind.tar.bz2 \
    && echo "${VALGRIND_SHA512}  /tmp/valgrind.tar.bz2" | sha512sum -c - \
    && tar -xjf /tmp/valgrind.tar.bz2 -C /tmp \
    && cd "/tmp/valgrind-${VALGRIND_VERSION}" \
    && ./configure --prefix=/usr/local \
    && make -j"$(nproc)" \
    && make install \
    && rm -rf /tmp/valgrind.tar.bz2 "/tmp/valgrind-${VALGRIND_VERSION}"

# Set up the working directory inside the container
WORKDIR /usr/src/ryo

# Isolate cargo build cache inside the container to prevent conflict/pollution
# with the host's target directory (especially useful when host is macOS/Windows).
ENV CARGO_TARGET_DIR=/tmp/target

# Default command: build the compiler, pre-install the toolchain sequentially (avoiding parallel download race conditions), and run all tests
CMD ["bash", "-c", "cargo build && cargo run -- toolchain install && cargo test"]
