Okay, let's outline a potential Rust codebase structure for the `ryo` compiler and integrated tooling, designed for clarity, modularity, and maintainability. This structure separates concerns into different crates within a Cargo workspace.

**Project Root (`ryo/`)**

```
ryo/
├── Cargo.toml         # Workspace definition
├── ryo/               # Main CLI binary crate (ryo command)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs    # Parses args (clap), dispatches to subcommands
├── ryo-core/          # Core language definitions (Tokens, AST, Types - no logic)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── token.rs   # Token enum definition (used by lexer)
│       ├── ast.rs     # AST node definitions (used by parser, analysis, codegen)
│       └── types.rs   # Representation of Ryo types (used by checker, codegen)
├── ryo-parser/        # Lexer and Parser logic
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs     # Main library entry, parsing function
│       ├── lexer.rs   # Lexer implementation (using logos + ryo-core::token)
│       └── parser.rs  # Parser implementation (using chumsky + ryo-core::ast)
├── ryo-checker/       # Semantic Analysis (Type Checking, Borrow Checking)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs     # Main library entry, checking function
│       ├── type_checker.rs # Type inference and checking logic
│       └── borrow_checker.rs # Ownership and borrowing rule enforcement
├── ryo-codegen-clif/  # Code Generation using Cranelift
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs     # Main library entry, codegen function (AST -> CLIF)
│       └── translator.rs # Logic to walk AST and emit Cranelift IR
├── ryo-runtime/       # Runtime support library (linked into compiled code)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── string.rs  # Runtime implementation for owned `str` (allocation, etc.)
│       ├── list.rs    # Runtime implementation for `List[T]`
│       ├── map.rs     # Runtime implementation for `Map[K,V]`
│       ├── channel.rs # Runtime implementation for `chan[T]` and scheduler interaction
│       ├── spawn.rs   # Runtime support for spawning tasks (needs executor)
│       └── panic.rs   # Panic handling (abort implementation)
├── ryo-pm/            # Package Management logic ("ryopkg" core)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── manifest.rs # Parsing and handling ryo.toml
│       ├── resolve.rs  # Dependency resolution logic
│       └── registry.rs # Interaction with package registry (ryopkgs.io)
├── ryo-driver/        # Orchestrates the compilation pipeline
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── compile.rs # Functions to run lexer -> parser -> checker -> codegen -> linker
│       └── jit.rs     # Functions to set up and run code via JIT (for REPL/run)
├── ryo-errors/        # Diagnostic reporting utilities
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs     # Wrappers around codespan-reporting/ariadne, diagnostic definitions
└── README.md
└── LICENSE
```

**Explanation of Crates:**

1.  **`ryo/` (Binary Crate):**
    *   The main user-facing executable (`ryo`).
    *   Uses `clap` to define subcommands (`build`, `run`, `test`, `new`, `add`, `repl`, etc.).
    *   Parses command-line arguments.
    *   Calls functions from `ryo-driver` or `ryo-pm` to execute the requested action.
    *   Contains minimal logic itself, mostly dispatching.

2.  **`ryo-core/` (Library Crate):**
    *   Defines fundamental data structures used across the compiler.
    *   **Tokens:** `Token` enum (defined using `logos` attributes perhaps, but the enum itself lives here).
    *   **AST:** Structs and enums defining the Abstract Syntax Tree. Includes span information.
    *   **Types:** Internal representation of Ryo types used during analysis (`struct Type { kind: TypeKind, span: Span }`).
    *   **No Logic:** This crate should contain definitions only, no complex processing, to avoid circular dependencies.

3.  **`ryo-parser/` (Library Crate):**
    *   Depends on `ryo-core` (for Token/AST definitions) and `ryo-errors`.
    *   Contains the **Lexer** implementation (e.g., using `logos`).
    *   Contains the **Parser** implementation (e.g., using `chumsky`) that consumes tokens and produces an AST (`ryo-core::ast::Program`).
    *   Handles syntax error reporting using `ryo-errors`.

4.  **`ryo-checker/` (Library Crate):**
    *   Depends on `ryo-core` (for AST/Types) and `ryo-errors`.
    *   Performs **Semantic Analysis**.
    *   **Type Checking/Inference:** Resolves types, checks for type errors. Might produce a Typed AST or annotate the existing AST.
    *   **Borrow Checking:** Implements the "Ownership Lite" rules, tracking ownership and borrows, reporting violations.

5.  **`ryo-codegen-clif/` (Library Crate):**
    *   Depends on `ryo-core` (for AST/Types), `cranelift`, `cranelift-module`, `cranelift-object`.
    *   Translates the (potentially type-checked/annotated) AST into Cranelift Intermediate Language (CLIF).
    *   Interacts with `cranelift-module` to define functions, data, etc.
    *   Outputs either CLIF text, or uses `cranelift-object` to produce an object file (`.o`).

6.  **`ryo-runtime/` (Library Crate):**
    *   Contains the **runtime support** needed by compiled Ryo code. This gets linked (statically or dynamically) into the final executable.
    *   Implementation of heap allocation for `str`, `List`, `Map`.
    *   Core logic for channel operations, task spawning (`spawn`), and interaction with the chosen concurrency scheduler/executor (which might be part of this crate or an external dependency like `tokio`).
    *   Panic handling implementation (e.g., printing message and aborting).
    *   *(Note: This crate itself is Rust code, providing the "behind-the-scenes" support for Ryo features).*

7.  **`ryo-pm/` (Library Crate):**
    *   Depends on `toml`, potentially networking crates (`reqwest`?) for registry interaction.
    *   Core logic for package management:
        *   Parsing `ryo.toml`.
        *   Resolving dependency versions.
        *   Downloading packages from a registry.
        *   Managing the `ryo.lock` file.
    *   Called by the `ryo` binary for commands like `add`, `install`, `update`, `publish`.

8.  **`ryo-driver/` (Library Crate):**
    *   Depends on most other `ryo-*` library crates (`ryo-parser`, `ryo-checker`, `ryo-codegen-clif`, `ryo-pm`, `ryo-errors`).
    *   Orchestrates the different phases of compilation and execution.
    *   Provides high-level functions like `compile_project(path)`, `run_file(path)`, `setup_and_run_jit(...)`.
    *   Handles linking object files with the `ryo-runtime` and system libraries.
    *   Contains logic for setting up Cranelift's JIT engine (`cranelift-jit`) if used for the REPL or `ryo run <file>`.

9.  **`ryo-errors/` (Library Crate):**
    *   Depends on `codespan-reporting` or `ariadne`.
    *   Defines common error/diagnostic types used across the compiler.
    *   Provides utility functions for formatting and printing user-friendly error messages with source code snippets and spans.

**Workspace `Cargo.toml` (`ryo/Cargo.toml`):**

```toml
[workspace]
members = [
    "ryo",
    "ryo-core",
    "ryo-parser",
    "ryo-checker",
    "ryo-codegen-clif",
    "ryo-runtime",
    "ryo-pm",
    "ryo-driver",
    "ryo-errors",
]

# Optional: Define shared profiles (dev, release) for all crates
[profile.dev]
opt-level = 0
debug = true

[profile.release]
opt-level = 3
debug = false
lto = true
```

**Benefits of this Structure:**

*   **Modularity:** Each crate has a specific responsibility.
*   **Clear Dependencies:** Dependencies between phases are explicit.
*   **Reusability:** Core components (`ryo-core`, `ryo-parser`) could potentially be reused by other tools (LSP, formatters).
*   **Testability:** Each crate can be tested independently.
*   **Parallel Compilation:** Cargo can build independent crates in parallel.
*   **Maintainability:** Easier to navigate and refactor specific parts of the compiler.

This structure provides a robust starting point for building the Ryo compiler and its integrated tooling.