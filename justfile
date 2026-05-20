# Default recipe: build runtime then compiler
build:
    cargo build -p ryo-runtime --release
    cargo build

# Release build
build-release:
    cargo build -p ryo-runtime --release
    cargo build --release

# Run all tests (builds runtime first)
test:
    cargo build -p ryo-runtime --release
    cargo test

# Run clippy on everything
lint:
    cargo clippy -p ryo-runtime --all-targets
    cargo build -p ryo-runtime --release
    cargo clippy --all-targets

# Format check
fmt:
    cargo fmt --check
