# Cargo Workspace Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor the Ryo monolithic compiler from a single-crate setup into a modular, five-crate Cargo workspace, preserving 100% test compatibility and root-level developer workflows.

**Architecture:** We will implement a bottom-up incremental migration: extract core data structures to `ryo-core`, compiler passes to `ryo-frontend`, code-generation/linking/toolchains to `ryo-backend`, pipeline orchestration to `ryo-driver`, and CLI to the `ryo` binary crate. Unidirectional dependencies will be strictly enforced through crate boundaries.

**Tech Stack:** Rust (Edition 2024), Cargo Workspaces, Cranelift, Logos, Chumsky, and Zig (managed linker).

## Global Constraints
- **Crate Versioning:** All sub-crates must be pinned to version `0.1.0` using Rust Edition `2024`.
- **Cargo Profiles:** Workspace-wide panic and optimize behaviors must be defined in the virtual manifest.
- **Tab Indentation:** No spaces may be mixed with tabs in Ryo source files (Ryo language compiler rules).
- **Compilation Correctness:** Every task must compile clean with zero errors or warnings before proceeding.
- **Git Commit Prefixes:** Commits must follow conventions: `refactor:`, `chore:`, `test:` with concise, clear messages.

---

### Task 1: Scaffolding and `ryo-core` Extraction

Extract AST, IRs (UIR, TIR), types, diagnostics, errors, and ownership types into `ryo-core`.

**Files:**
- Create: `ryo-core/Cargo.toml`
- Create: `ryo-core/src/lib.rs`
- Create: `ryo-core/src/ownership.rs` (extract type models from `src/ownership.rs`)
- Create: `ryo-core/src/ast.rs` (move from `src/ast.rs`)
- Create: `ryo-core/src/uir.rs` (move from `src/uir.rs`)
- Create: `ryo-core/src/tir.rs` (move from `src/tir.rs`)
- Create: `ryo-core/src/types.rs` (move from `src/types.rs`)
- Create: `ryo-core/src/diag.rs` (move from `src/diag.rs`)
- Create: `ryo-core/src/errors.rs` (move from `src/errors.rs`)
- Modify: `Cargo.toml` (declare workspace members and dependency)
- Modify: `src/ownership.rs` (remove types moved to `ryo-core/src/ownership.rs`)
- Modify: `src/lib.rs` or `src/main.rs` (register temporary dependencies)

**Interfaces:**
- Consumes: Nothing (self-contained leaf node of the workspace).
- Produces: `ryo-core::ast`, `ryo-core::uir`, `ryo-core::tir`, `ryo-core::types`, `ryo-core::diag`, `ryo-core::errors`, `ryo-core::ownership`.

- [ ] **Step 1.1: Create `ryo-core/Cargo.toml`**

Write the manifest for the core library.
```toml
[package]
name = "ryo-core"
version = "0.1.0"
edition = "2024"

[dependencies]
ariadne = "0.6"
chumsky = "0.12"
hashbrown = "0.17"
```

- [ ] **Step 1.2: Initialize the Workspace in root `Cargo.toml`**

Update the root `Cargo.toml` to list the initial workspace members and add a temporary dependency to `ryo-core`.
```toml
[workspace]
members = ["runtime", "ryo-core"]

[dependencies]
ryo-core = { path = "ryo-core" }
# Keep remaining dependencies for the monolith...
```

- [ ] **Step 1.3: Move files to `ryo-core`**

Create `ryo-core/src/` and move core files.
```bash
mkdir -p ryo-core/src
git mv src/ast.rs ryo-core/src/ast.rs
git mv src/uir.rs ryo-core/src/uir.rs
git mv src/tir.rs ryo-core/src/tir.rs
git mv src/types.rs ryo-core/src/types.rs
git mv src/diag.rs ryo-core/src/diag.rs
git mv src/errors.rs ryo-core/src/errors.rs
```

- [ ] **Step 1.4: Split `ownership.rs` to extract data structures**

Create `ryo-core/src/ownership.rs` and move the following data models:
- `BranchId`
- `FreePoint`
- `IfBranchIds`
- `OwnershipSidecar`
- `FunctionSidecar`
and their `Default`, `Clone`, and `Debug` implementations.

Leave the analysis functions (`check`, `visit_expr`, etc.) in `src/ownership.rs`.
Modify `src/ownership.rs` to import these structs from `ryo-core::ownership`:
```rust
use ryo_core::ownership::{BranchId, FreePoint, IfBranchIds, OwnershipSidecar, FunctionSidecar};
```

- [ ] **Step 1.5: Create `ryo-core/src/lib.rs`**

Create the entry point for the `ryo-core` library.
```rust
pub mod ast;
pub mod uir;
pub mod tir;
pub mod types;
pub mod diag;
pub mod errors;
pub mod ownership;
```

- [ ] **Step 1.6: Update root imports**

For each remaining file in `src/` (such as `lexer.rs`, `indent.rs`, `parser.rs`, `astgen.rs`, `sema.rs`, `codegen.rs`, `pipeline.rs`), change internal crate imports:
- Replace `crate::ast` $\rightarrow$ `ryo_core::ast`
- Replace `crate::uir` $\rightarrow$ `ryo_core::uir`
- Replace `crate::tir` $\rightarrow$ `ryo_core::tir`
- Replace `crate::types` $\rightarrow$ `ryo_core::types`
- Replace `crate::diag` $\rightarrow$ `ryo_core::diag`
- Replace `crate::errors` $\rightarrow$ `ryo_core::errors`
- Replace `crate::ownership` references to sidecars $\rightarrow$ `ryo_core::ownership`

- [ ] **Step 1.7: Run compiler check and unit tests**

Verify that `ryo-core` compiles and the root monolith builds and passes all tests.
```bash
cargo check
cargo test -p ryo-core
cargo test
```

- [ ] **Step 1.8: Commit**
```bash
git add ryo-core Cargo.toml src/
git commit -m "refactor: extract ryo-core crate"
```

---

### Task 2: `ryo-frontend` Extraction

Extract lexer, indent preprocessor, parser, AST lowered lowering (AstGen), semantic analysis, builtins, and the ownership validator pass into `ryo-frontend`.

**Files:**
- Create: `ryo-frontend/Cargo.toml`
- Create: `ryo-frontend/src/lib.rs`
- Create: `ryo-frontend/src/lexer.rs` (move from `src/lexer.rs`)
- Create: `ryo-frontend/src/indent.rs` (move from `src/indent.rs`)
- Create: `ryo-frontend/src/parser.rs` (move from `src/parser.rs`)
- Create: `ryo-frontend/src/astgen.rs` (move from `src/astgen.rs`)
- Create: `ryo-frontend/src/sema.rs` (move from `src/sema.rs`)
- Create: `ryo-frontend/src/builtins.rs` (move from `src/builtins.rs`)
- Create: `ryo-frontend/src/ownership.rs` (move from `src/ownership.rs` and import core sidecars)
- Modify: `Cargo.toml` (declare workspace member and dependency)
- Modify: `src/main.rs` (update temporary imports)

**Interfaces:**
- Consumes: `ryo-core` modules.
- Produces: `ryo-frontend::lexer`, `ryo-frontend::indent`, `ryo-frontend::parser`, `ryo-frontend::astgen`, `ryo-frontend::sema`, `ryo-frontend::builtins`, `ryo-frontend::ownership::check`.

- [ ] **Step 2.1: Create `ryo-frontend/Cargo.toml`**

Write the manifest for the frontend library.
```toml
[package]
name = "ryo-frontend"
version = "0.1.0"
edition = "2024"

[dependencies]
ryo-core = { path = "../ryo-core" }
logos = "0.16"
chumsky = "0.12"
hashbrown = "0.17"
```

- [ ] **Step 2.2: Add `ryo-frontend` to workspace**

Update root `Cargo.toml` workspace members and dependency.
```toml
[workspace]
members = ["runtime", "ryo-core", "ryo-frontend"]

[dependencies]
ryo-core = { path = "ryo-core" }
ryo-frontend = { path = "ryo-frontend" }
```

- [ ] **Step 2.3: Move files to `ryo-frontend`**

Create `ryo-frontend/src/` and move the frontend files.
```bash
mkdir -p ryo-frontend/src
git mv src/lexer.rs ryo-frontend/src/lexer.rs
git mv src/indent.rs ryo-frontend/src/indent.rs
git mv src/parser.rs ryo-frontend/src/parser.rs
git mv src/astgen.rs ryo-frontend/src/astgen.rs
git mv src/sema.rs ryo-frontend/src/sema.rs
git mv src/builtins.rs ryo-frontend/src/builtins.rs
git mv src/ownership.rs ryo-frontend/src/ownership.rs
```

- [ ] **Step 2.4: Create `ryo-frontend/src/lib.rs`**

Create the entry point for the frontend library.
```rust
pub mod lexer;
pub mod indent;
pub mod parser;
pub mod astgen;
pub mod sema;
pub mod builtins;
pub mod ownership;
```

- [ ] **Step 2.5: Update frontend and root imports**

In the frontend files, update imports that previously pointed to `crate::` or root modules.
- Replace `use crate::*` imports with `use ryo_core::*` equivalents (e.g. `ryo_core::types::InternPool`).
- Update the remaining root modules (like `codegen.rs` and `pipeline.rs`) to import from `ryo_frontend::*` (e.g., `use ryo_frontend::sema::analyze;`).

- [ ] **Step 2.6: Run compiler check and unit tests**

Verify both sub-crates compile and the root monolith builds and passes all tests.
```bash
cargo check
cargo test -p ryo-frontend
cargo test
```

- [ ] **Step 2.7: Commit**
```bash
git add ryo-frontend Cargo.toml src/
git commit -m "refactor: extract ryo-frontend crate"
```

---

### Task 3: `ryo-backend` Extraction

Extract Cranelift codegen, Zig linking, toolchain, and runtime utilities into `ryo-backend`.

**Files:**
- Create: `ryo-backend/Cargo.toml`
- Create: `ryo-backend/build.rs`
- Create: `ryo-backend/src/lib.rs`
- Create: `ryo-backend/src/codegen.rs` (move from `src/codegen.rs`)
- Create: `ryo-backend/src/linker.rs` (move from `src/linker.rs`)
- Create: `ryo-backend/src/toolchain.rs` (move from `src/toolchain.rs`)
- Create: `ryo-backend/src/runtime_lib.rs` (move from `src/runtime_lib.rs`)
- Modify: `Cargo.toml` (declare workspace member and dependency)
- Modify: `src/main.rs` (update temporary imports)

**Interfaces:**
- Consumes: `ryo-core` modules. (Strictly no dependency on `ryo-frontend`!).
- Produces: `ryo-backend::codegen`, `ryo-backend::linker`, `ryo-backend::toolchain`.

- [ ] **Step 3.1: Create `ryo-backend/Cargo.toml`**

Write the manifest for the backend library.
```toml
[package]
name = "ryo-backend"
version = "0.1.0"
edition = "2024"

[dependencies]
ryo-core = { path = "../ryo-core" }
ryo-runtime = { path = "../runtime" }
cranelift = "0.131"
cranelift-jit = "0.131"
cranelift-module = "0.131"
cranelift-object = "0.131"
target-lexicon = "0.13.5"
ureq = "3"
tar = "0.4"
xz2 = "0.1"
dirs = "6"

[build-dependencies]
sha2 = "0.10"
```

- [ ] **Step 3.2: Configure the backend's build script**

Copy the runtime-building logic from root `build.rs` to `ryo-backend/build.rs`. This script compiles `ryo-runtime` and exposes `RYO_RUNTIME_LIB` and `RYO_RUNTIME_HASH` on demand.
```rust
// Save as ryo-backend/build.rs
use sha2::{Digest, Sha256};
use std::env;

fn main() {
    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    // (Incorporate the full runtime building logic verbatim from current build.rs)
    // Expose RYO_RUNTIME_LIB and RYO_RUNTIME_HASH as cargo environment variables.
}
```

- [ ] **Step 3.3: Add `ryo-backend` to workspace**

Update root `Cargo.toml`.
```toml
[workspace]
members = ["runtime", "ryo-core", "ryo-frontend", "ryo-backend"]

[dependencies]
ryo-core = { path = "ryo-core" }
ryo-frontend = { path = "ryo-frontend" }
ryo-backend = { path = "ryo-backend" }
```

- [ ] **Step 3.4: Move files to `ryo-backend`**

Create `ryo-backend/src/` and move the backend files.
```bash
mkdir -p ryo-backend/src
git mv src/codegen.rs ryo-backend/src/codegen.rs
git mv src/linker.rs ryo-backend/src/linker.rs
git mv src/toolchain.rs ryo-backend/src/toolchain.rs
git mv src/runtime_lib.rs ryo-backend/src/runtime_lib.rs
```

- [ ] **Step 3.5: Create `ryo-backend/src/lib.rs`**

Create the entry point for the backend library.
```rust
pub mod codegen;
pub mod linker;
pub mod toolchain;
pub mod runtime_lib;
```

- [ ] **Step 3.6: Update imports**

In the backend modules:
- Update all `crate::` imports to refer to `ryo_core::` modules.
In remaining root modules (`pipeline.rs`):
- Change `crate::codegen` $\rightarrow$ `ryo_backend::codegen`
- Change `crate::linker` $\rightarrow$ `ryo_backend::linker`
- Change `crate::toolchain` $\rightarrow$ `ryo_backend::toolchain`

- [ ] **Step 3.7: Run compiler check and unit tests**

Verify `ryo-backend` compiles and the root monolith builds and passes all tests.
```bash
cargo check
cargo test -p ryo-backend
cargo test
```

- [ ] **Step 3.8: Commit**
```bash
git add ryo-backend Cargo.toml src/
git commit -m "refactor: extract ryo-backend crate"
```

---

### Task 4: `ryo-driver` & Binary virtualisation

Move the pipeline driver, turn the root `Cargo.toml` into a completely virtual manifest, and establish the `ryo` CLI crate.

**Files:**
- Create: `ryo-driver/Cargo.toml`
- Create: `ryo-driver/src/lib.rs`
- Create: `ryo-driver/src/pipeline.rs` (move from `src/pipeline.rs`)
- Create: `ryo/Cargo.toml`
- Create: `ryo/build.rs` (git version environment script)
- Create: `ryo/src/main.rs` (move from `src/main.rs`)
- Modify: `Cargo.toml` (completely virtualise manifest, remove direct dependencies)

**Interfaces:**
- Consumes: `ryo-core`, `ryo-frontend`, `ryo-backend`.
- Produces: `ryo-driver::pipeline`, and the binary target executable `ryo`.

- [ ] **Step 4.1: Create `ryo-driver/Cargo.toml`**

Write the manifest for the driver library.
```toml
[package]
name = "ryo-driver"
version = "0.1.0"
edition = "2024"

[dependencies]
ryo-core = { path = "../ryo-core" }
ryo-frontend = { path = "../ryo-frontend" }
ryo-backend = { path = "../ryo-backend" }
```

- [ ] **Step 4.2: Move pipeline to `ryo-driver`**

Create `ryo-driver/src/` and move the pipeline.
```bash
mkdir -p ryo-driver/src
git mv src/pipeline.rs ryo-driver/src/pipeline.rs
```

- [ ] **Step 4.3: Create `ryo-driver/src/lib.rs`**

Create the entry point for the driver library.
```rust
pub mod pipeline;
```

- [ ] **Step 4.4: Update imports in `pipeline.rs`**

Update `ryo-driver/src/pipeline.rs` imports to use the correct library paths:
- Replace `crate::ast` $\rightarrow$ `ryo_core::ast` (and similar for uir/tir/types/diag/errors)
- Replace `crate::lexer` $\rightarrow$ `ryo_frontend::lexer` (and similar for indent/parser/astgen/sema/ownership)
- Replace `crate::codegen` $\rightarrow$ `ryo_backend::codegen` (and similar for linker/toolchain)

- [ ] **Step 4.5: Create `ryo/Cargo.toml`**

Write the manifest for the executable binary crate.
```toml
[package]
name = "ryo"
version = "0.1.0"
edition = "2024"

[dependencies]
ryo-driver = { path = "../ryo-driver" }
clap = { version = "4.6", features = ["derive"] }
```

- [ ] **Step 4.6: Create `ryo/build.rs`**

Extract the git status/versioning logic from root `build.rs` to `ryo/build.rs` to expose `RYO_VERSION` on compile.
```rust
// Save as ryo/build.rs
fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    // (Insert the git hash/versioning extraction code verbatim from current build.rs)
    println!("cargo:rustc-env=RYO_VERSION={version}");
}
```

- [ ] **Step 4.7: Move main to `ryo`**

Create `ryo/src/` and move the CLI main file.
```bash
mkdir -p ryo/src
git mv src/main.rs ryo/src/main.rs
```

- [ ] **Step 4.8: Clean up `main.rs`**

Delete all the internal `mod ast;`, `mod sema;`, etc. module definitions from `ryo/src/main.rs`. In their place, import from `ryo_driver` and `ryo_backend`:
```rust
use ryo_driver::pipeline;
// For status or status path action inside main:
use ryo_backend::toolchain;
```

- [ ] **Step 4.9: Final virtualisation of root `Cargo.toml`**

Convert the root manifest to a virtual manifest. Declare all shared dependencies in the workspace block and configure `default-members = ["ryo"]`.
```toml
[workspace]
members = [
    "runtime",
    "ryo-core",
    "ryo-frontend",
    "ryo-backend",
    "ryo-driver",
    "ryo",
]
default-members = ["ryo"]

[workspace.dependencies]
ariadne = "0.6"
chumsky = "0.12"
hashbrown = "0.17"
clap = { version = "4.6", features = ["derive"] }
cranelift = "0.131"
cranelift-jit = "0.131"
cranelift-module = "0.131"
cranelift-object = "0.131"
xz2 = "0.1"
logos = "0.16"
tar = "0.4"
target-lexicon = "0.13.5"
ureq = "3"
dirs = "6"
sha2 = "0.10"
tempfile = "3.27"
ryo-runtime = { path = "runtime" }
ryo-core = { path = "ryo-core" }
ryo-frontend = { path = "ryo-frontend" }
ryo-backend = { path = "ryo-backend" }
ryo-driver = { path = "ryo-driver" }

[profile.release]
panic = "abort"

[profile.dev]
panic = "abort"
```

Also, update individual crate manifests to use `{ workspace = true }` for dependencies (e.g. `chumsky = { workspace = true }`).

- [ ] **Step 4.10: Run full build and test verification**

Verify everything compiles cleanly under the new virtualized workspace.
```bash
cargo build
cargo test
```

- [ ] **Step 4.11: Commit**
```bash
git add Cargo.toml ryo/ ryo-driver/
git commit -m "refactor: virtualize root Cargo.toml and setup ryo binary"
```

---

### Task 5: Complete Workspace Verification and Cleanup

Perform final cleanup of any residual folders, double-check all integration tests, and ensure clippy/fmt are fully clean.

**Files:**
- Modify: `tests/integration_tests.rs` (ensure Cargo manifest references are correct)
- Delete: `src/` (ensure old monolith folder is removed)

**Interfaces:**
- Consumes: Fully compiled workspace.
- Produces: Green CI validation output.

- [ ] **Step 5.1: Clean up empty root `src/` folder**

Verify the root `src/` folder is empty and remove it.
```bash
rm -rf src/
```

- [ ] **Step 5.2: Verify integration tests**

Run all unit and integration tests under the workspace to ensure perfect execution.
```bash
cargo test
```

- [ ] **Step 5.3: Run Clippy**

Run cargo clippy on all targets to ensure clean Rust code and no warnings.
```bash
cargo clippy --all-targets -- -D warnings
```

- [ ] **Step 5.4: Run Format**

Run cargo format to verify proper file layout.
```bash
cargo fmt --check
```

- [ ] **Step 5.5: Final Commit**
```bash
git commit -a -m "chore: complete workspace cleanup and validation"
```
