# Design Specification: Cargo Workspace Refactor

**Date:** 2026-07-02  
**Status:** Approved  
**Topic:** Transitioning Ryo from a single-crate structure to a modular multi-crate Cargo workspace.

---

## 1. Goal & Philosophy

As Ryo has grown past 16,000 lines of code, managing the codebase under a single crate adds build-time overhead and risks tight coupling. This refactor transitions Ryo to a Cargo workspace following the **"few crates, done well"** philosophy. 

Our core objectives are:
1.  **Strict Boundary Enforcement:** Prevent accidental compile-time circular dependencies between phases (e.g. Frontend using Backend types directly).
2.  **Unidirectional Dependency Flow:** Core types flow up to the intermediate passes; frontend and backend compile independently; driver orchestrates both; binary exposes driver to the user.
3.  **No Codebase Regression:** Ensure 100% of the 468 tests continue to compile and pass cleanly, including JIT and AOT tests.
4.  **Preserved Ergonomics:** Root-level workflows (like `cargo run -- build <file>`) must remain fully functional without extra command line arguments.

---

## 2. Architecture & Crate Layout

```text
               ┌─────────────┐
               │  ryo (CLI)  │
               └──────┬──────┘
                      │
               ┌──────▼──────┐
               │ ryo-driver  │
               └─┬─────────┬─┘
                 │         │
      ┌──────────▼──┐   ┌──▼──────────┐
      │ryo-frontend │   │ ryo-backend │
      └──────────┬──┘   └──┬──────────┘
                 │         │
               ┌─▼─────────▼─┐
               │  ryo-core   │
               └─────────────┘
```

The workspace will contain six members under the virtual manifest:

| Crate Name | Path | Crate Type | Purpose & Contents |
| :--- | :--- | :--- | :--- |
| **`ryo-core`** | `ryo-core/` | `lib` | Shared data structures and IR definitions with zero logical compiler passes. Holds `ast.rs`, `uir.rs`, `tir.rs`, `types.rs`, `diag.rs`, `errors.rs`, and ownership sidecar models. |
| **`ryo-frontend`**| `ryo-frontend/` | `lib` | Parser and semantic analysis. Holds `lexer.rs`, `indent.rs`, `parser.rs`, `astgen.rs`, `sema.rs`, `builtins.rs`, and the ownership analysis pass. |
| **`ryo-backend`** | `ryo-backend/` | `lib` | Compilation backend. Holds `codegen.rs`, `linker.rs`, `toolchain.rs`, and `runtime_lib.rs`. |
| **`ryo-driver`** | `ryo-driver/` | `lib` | Orchestrator. Coordinates the full pipeline from `lexer` to `linker` via `pipeline.rs`. |
| **`ryo`** | `ryo/` | `bin` | CLI Entrypoint. Contains the `clap` argument parsing and dispatches commands to the driver. |
| **`ryo-runtime`** | `runtime/` | `lib` / `staticlib` | Existing static C-runtime library (remains untouched in its current location). |

---

## 3. Decoupling the Ownership Pass (`ownership.rs`)

To maintain a strict separation of concerns where `ryo-backend` has no dependency on `ryo-frontend`, we split `src/ownership.rs` into two pieces:

1.  **`ryo-core/src/ownership.rs` (Data Models):**
    *   Defines the sidecars and identifiers populated during type analysis and consumed during code generation: `BranchId`, `FreePoint`, `IfBranchIds`, `OwnershipSidecar`, and `FunctionSidecar`.
2.  **`ryo-frontend/src/ownership.rs` (Analysis Logic):**
    *   Implements the actual Mojo-inspired borrow checker / move validator analysis: `check`, `visit_expr`, flow lattice, and diagnostics validation.

---

## 4. Manifest Definitions

### Root `Cargo.toml` (Virtual Manifest)
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

### `ryo-core/Cargo.toml`
```toml
[package]
name = "ryo-core"
version = "0.1.0"
edition = "2024"

[dependencies]
ariadne = { workspace = true }
chumsky = { workspace = true }
hashbrown = { workspace = true }
```

### `ryo-frontend/Cargo.toml`
```toml
[package]
name = "ryo-frontend"
version = "0.1.0"
edition = "2024"

[dependencies]
ryo-core = { workspace = true }
logos = { workspace = true }
chumsky = { workspace = true }
hashbrown = { workspace = true }
```

### `ryo-backend/Cargo.toml`
```toml
[package]
name = "ryo-backend"
version = "0.1.0"
edition = "2024"

[dependencies]
ryo-core = { workspace = true }
ryo-runtime = { workspace = true }
cranelift = { workspace = true }
cranelift-jit = { workspace = true }
cranelift-module = { workspace = true }
cranelift-object = { workspace = true }
target-lexicon = { workspace = true }
ureq = { workspace = true }
tar = { workspace = true }
xz2 = { workspace = true }
dirs = { workspace = true }

[build-dependencies]
sha2 = { workspace = true }
```

### `ryo-driver/Cargo.toml`
```toml
[package]
name = "ryo-driver"
version = "0.1.0"
edition = "2024"

[dependencies]
ryo-core = { workspace = true }
ryo-frontend = { workspace = true }
ryo-backend = { workspace = true }
```

### `ryo/Cargo.toml`
```toml
[package]
name = "ryo"
version = "0.1.0"
edition = "2024"

[dependencies]
ryo-driver = { workspace = true }
clap = { workspace = true }
```

---

## 5. Build Scripts Location

1.  **`ryo-backend/build.rs`**:
    *   Compiles `ryo-runtime` into a static library on-demand.
    *   Exposes `RYO_RUNTIME_LIB` and `RYO_RUNTIME_HASH` to `runtime_lib.rs`.
2.  **`ryo/build.rs`**:
    *   Exposes `RYO_VERSION` (using git tags and hashes) to `main.rs`.

---

## 6. Migration Sequence (Bottom-Up)

1.  **Crate Setup**: Scaffold the directory structures and initial `Cargo.toml` files for all five sub-crates.
2.  **Extract `ryo-core`**: 
    *   Move files to `ryo-core/src/`. Create `ryo-core/src/lib.rs`.
    *   Create temporary dependency from root package to `ryo-core`. Update imports in root files.
3.  **Extract `ryo-frontend`**:
    *   Move files to `ryo-frontend/src/`. Split `ownership.rs` into type definitions and check logic.
    *   Create temporary dependency from root package to `ryo-frontend`. Update imports.
4.  **Extract `ryo-backend`**:
    *   Move backend files. Set up `ryo-backend/build.rs` and verify static runtime compiles.
    *   Create temporary dependency from root package to `ryo-backend`.
5.  **Extract `ryo-driver`**:
    *   Move `pipeline.rs` to `ryo-driver`. Set up dependency to `ryo-core`, `ryo-frontend`, and `ryo-backend`.
6.  **Convert CLI and Root**:
    *   Move `main.rs` to `ryo/src/main.rs`.
    *   Set up `ryo/build.rs` for versioning.
    *   Delete the root package section in root `Cargo.toml` to complete its virtualization.
7.  **Verify & Test**: Run workspace checks, lint checks, and integration tests to confirm 100% compliance.
