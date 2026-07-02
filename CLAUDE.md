# Ryo Programming Language - Repository Conventions

**Ryo** is a pre-alpha statically-typed, compiled (AOT/JIT) programming language implemented in Rust. See README.md for language philosophy and design goals.

## Tech Stack & Layout

**Stack:** Rust compiler with Cranelift backend, Zig linker, Logos lexer, Chumsky parser.
**Layout:** Cargo workspace consisting of:
- `ryo/` (CLI binary crate)
- `ryo-core/` (shared models: AST, IRs (UIR, TIR), types, diagnostics, and errors)
- `ryo-frontend/` (lexer, indent preprocessor, parser, AST lowering, semantic analysis, builtins, ownership analysis)
- `ryo-backend/` (Cranelift code generation, Zig linking, toolchain management, runtime extraction)
- `ryo-driver/` (pipeline compilation orchestration driver)
- `runtime/` (Ryo static runtime library)
- `docs/` (spec, roadmap, examples)
- `experimental/` (design work)
- `.github/` (CI)

---

## File Naming Conventions

- **Ryo files:** lowercase with underscores (`error_handling.ryo`, `hello_world.ryo`)
- **Docs:** lowercase with underscores (`getting_started.md`). Special files uppercase (`README.md`, `CLAUDE.md`, `TODO.md`)
- **Rust files:** lowercase with underscores (`main.rs`, `ast.rs`) following Rust conventions

---

## Critical Syntax Rules

**⚠️ CRITICAL: Python-Style Syntax is MANDATORY**

All Ryo code examples **must** use Python-style colons and indentation, **NOT** curly braces. Braces are ONLY for f-strings.

**Tab Indentation:** Use TABS (not spaces). Mixing tabs/spaces is a compile-time error. One tab = one indentation level.

---

## Documentation Standards

**Code examples:** Use fenced code blocks with language tag (````ryo`).
**Cross-references:** Use relative paths (`[spec](docs/specification.md)`).

---

## Build & Test Commands

Standard cargo commands work fully out-of-the-box (even on a clean checkout) because `build.rs` automatically compiles the `ryo-runtime` static library in a separate target directory if it isn't found.

```bash
cargo build                      # Automatically builds the runtime (if missing) and then compiles the compiler
cargo check                      # Check compiler for errors
cargo test                       # Run all unit + integration tests
./run_linux_tests.sh             # Build Docker image and run entire test suite in Linux (ASan + Valgrind leak detection)
cargo run -- run <file>          # JIT compile and execute
cargo run -- build <file>        # AOT compile to binary
cargo run -- toolchain install   # Download Zig linker
cargo run -- toolchain status    # Check Zig status
cargo clippy --all-targets       # Lint (warnings are errors)
cargo fmt --check                # Check code formatting style
```

**File extensions:** `.ryo` (source), `.md` (docs), `.rs` (Rust), `.o`/`.obj` (generated)

---

## CI

GitHub Actions runs on pushes to `main` and PRs targeting `main`: `cargo fmt --check` with `-Dwarnings`, `cargo clippy --all-targets` (warnings are errors), and `cargo test` (Ubuntu x64, Ubuntu ARM64, macOS). All three must pass for merge.

---

## Development Workflow

**Branch naming:** `feat/`, `docs/`, `fix/`, `chore/`, `design/` prefixes.

**Commit prefixes:** `feat:`, `fix:`, `docs:`, `spec:`, `dev:`, `roadmap:`, `test:`, `chore:`, `refactor:`.
Keep subjects under 72 chars. Add body for non-obvious changes.

IMPORTANT: Never author Claude on commits nor PRs.

---

## Issue Tracking

Non-immediate issues that affect architecture, correctness, or long-term code health go in `ISSUES.md`. Create an entry when you identify a problem that won't be resolved in the current session but must be addressed for better architecture or sustainability. Use the next sequential `I-XXX` number, pick the appropriate severity (Blocking / Correctness / Cleanup), and include Files, Summary, and Resolution fields.

Do **not** create issues for things you're fixing right now — just fix them. Do **not** use GitHub Issues for these; `ISSUES.md` is the single source of truth.

---

## Design Change Escalation

Ryo is pre-alpha. Design changes to the language specification require explicit human approval. Coherence fixes (resolving contradictions, filling documented gaps, tightening phrasing) can proceed as normal work, but anything that adds, removes, or alters a language feature stops for review.

Examples:
- **OK without approval:** Fixing contradictions between spec sections, clarifying ambiguous phrasing, adding missing details for documented features
- **Requires approval:** Adding new syntax, removing features, changing semantics, altering ownership rules, modifying error handling behavior

When in doubt, ask before making language design changes.

---

## Documentation Conventions

For docs-specific conventions when editing files in `docs/`, see `docs/CLAUDE.md`.

---

## Compiler Architecture & Developer Guide

This section is for agents extending the Ryo compiler.

### Design Inspiration

- **Compiler architecture (lexer → parser → UIR → Sema → TIR → Codegen):** takes inspiration from the Zig compiler — see [`/docs/dev/zig_reference.md`](docs/dev/zig_reference.md).
- **Concurrency:** takes inspiration from Go — see [`/docs/dev/go_reference.md`](docs/dev/go_reference.md).
- **Ownership pass (`ryo-frontend/src/ownership.rs` & `ryo-core/src/ownership.rs`, M8.1+):** takes inspiration from Mojo — see [`/docs/dev/mojo_reference.md`](docs/dev/mojo_reference.md). Zig has no borrow checker, so it stops being a useful reference once M8.1 introduces move semantics. Mojo's MLIR-based lifetime/ASAP-destruction passes are the closest published precedent for what Ryo's spec already commits to (no annotated lifetimes, parameters borrow by default, eager destruction at last use). Sema and the IRs themselves remain Zig-shaped.
- **`shared[T]` refcounting & ARC optimizer (`src/arc_optimizer.rs`, post-M11):** takes inspiration from Swift — see [`/docs/dev/arc_optimizer.md`](docs/dev/arc_optimizer.md). Swift's SIL ARC optimizer (aggressive retain/release elision, stack promotion, copy-on-write for collections) is the model. The performance promise of `shared[T]` in spec 5.6 depends on this pass actually existing and working; without it `shared[T]` benchmarks badly.
- **Comparison reference for Rust:** see [`/docs/dev/rust_reference.md`](docs/dev/rust_reference.md). Rust's `rustc_borrowck` and `Arc<T>` story is the obvious comparison point for both the ownership pass and `shared[T]`. Diagnostic UX bar is set against Rust's renderer.

### Rust Patterns ([Microsoft Rust Guidelines](https://microsoft.github.io/rust-guidelines/agents/all.txt))

- `// SAFETY:` comment on every `unsafe` block explaining soundness
- `debug_assert!` for internal invariants — zero cost in release builds
- Checked/saturating arithmetic for spans, offsets, indices — no silent overflow
- `PathBuf`/`&Path` for file paths, not `String`/`&str`; short-lived borrows across passes
- FFI: `#[repr(C)]` structs, no `String`/`Vec` across boundaries, safe wrappers for unsafe calls

### Compilation Pipeline

```text
Source → Lexer → Indent Preprocessor → Parser → AstGen → UIR → Sema → TIR → Ownership → TIR' → Codegen → Linker → Executable
```

(The **Ownership** stage lands in M8.1 — see `docs/dev/mojo_reference.md` and `docs/superpowers/specs/2026-05-11-milestone-8.1-heap-str-and-move-semantics-design.md`. Before M8.1 the pipeline goes straight from Sema to Codegen.)

The middle-end is split into two flat-arena IRs modeled after Zig's ZIR/AIR:

- **UIR** (`ryo-core/src/uir.rs`) — Untyped IR. Flat `(tag, data)` instruction stream in a program-wide arena, produced by `astgen.rs` from the AST. Sub-expressions are not nested; they live as their own entries reached via `InstRef` indices. Side arenas: `extra: Vec<u32>` for variable-size payloads, `spans` parallel to instructions.
- **TIR** (`ryo-core/src/tir.rs`) — Typed IR. Same flat shape as UIR but **one arena per function body**, and every instruction carries its resolved `TypeId`. Produced by `sema.rs` from UIR and consumed by `codegen.rs`. Per-function arenas make generic/inline duplication a `Tir::clone` away.

**Mapping to Zig.** When cross-referencing the Zig compiler source for reference:

| Ryo  | Zig file       | Role |
|------|----------------|------|
| UIR  | `src/Zir.zig`  | Flat, untyped instruction stream emitted by `astgen` from the AST. Input to Sema. |
| TIR  | `src/Air.zig`  | Flat, fully-typed instruction stream emitted by Sema, one per function body. Input to codegen. |

Ryo does not produce or consume Zig's ZIR/AIR — these are upstream design references only.

**Niche-filled refs.** `InstRef` (UIR) and `TirRef` (TIR) wrap `NonZeroU32`, so `Option<InstRef>` / `Option<TirRef>` fit in a single 32-bit slot. Slot 0 of each `instructions` arena is a reserved sentinel that is never emitted — do not hand out `InstRef(0)` / `TirRef(0)`, and do not assume index 0 is a real instruction when iterating.

See `docs/dev/pipeline_alignment.md` for the full design rationale (motivation, phase plan, comptime/generics implications).

**Crate and Module Map** (distributed in workspace packages):

#### 1. Binary Executive Crate (`ryo`)
| File | Role |
|------|------|
| `ryo/src/main.rs` | CLI definition (clap) and command dispatch |
| `ryo/build.rs` | Git version status parsing and JIT/AOT runtime building script |

#### 2. Driver Orchestration Crate (`ryo-driver`)
| File | Role |
|------|------|
| `ryo-driver/src/pipeline.rs` | Orchestrates pipeline stages: lex → parse → astgen → sema → codegen → link → run |

#### 3. Frontend Compilation Crate (`ryo-frontend`)
| File | Role |
|---|---|
| `ryo-frontend/src/lexer.rs` | Logos-based tokenizer; emits interned `Token` stream |
| `ryo-frontend/src/indent.rs` | CPython-style Indent/Dedent token insertion over raw lexer output |
| `ryo-frontend/src/parser.rs` | Chumsky-based parser over `Token` (produces AST) |
| `ryo-frontend/src/astgen.rs` | AST → UIR structural lowering |
| `ryo-frontend/src/sema.rs` | Semantic analysis: type-checks UIR, emits one TIR per function body |
| `ryo-frontend/src/ownership.rs` | Post-sema, pre-codegen ownership flow analysis |
| `ryo-frontend/src/builtins.rs` | Builtin function registry (currently `print`, `panic`, etc.) |

#### 4. Code Generation and Linking Crate (`ryo-backend`)
| File | Role |
|------|------|
| `ryo-backend/src/codegen.rs` | Cranelift IR generation from TIR (JIT and AOT) |
| `ryo-backend/src/linker.rs` | Executable linking via the managed Zig toolchain |
| `ryo-backend/src/toolchain.rs` | Zig toolchain download / version pinning / path resolution |
| `ryo-backend/src/runtime_lib.rs` | Static runtime library extraction and caching |
| `ryo-backend/build.rs` | Compiles `ryo-runtime` on demand and exposes environment paths |

#### 5. Shared Core Crate (`ryo-core`)
| File | Role |
|------|------|
| `ryo-core/src/ast.rs` | Surface-syntax AST; identifiers/types/strings stored as `StringId` |
| `ryo-core/src/uir.rs` | Untyped IR data structures (flat, program-wide arena) |
| `ryo-core/src/tir.rs` | Typed IR data structures (flat, per-function arena) |
| `ryo-core/src/ownership.rs` | Ownership pass side-tables and data structures (`BranchId`, `FreePoint`, etc.) |
| `ryo-core/src/types.rs` | `InternPool` for types and strings; `TypeId` / `StringId` newtypes |
| `ryo-core/src/diag.rs` | Structured diagnostics: `Diag`, `DiagCode`, `DiagSink` |
| `ryo-core/src/errors.rs` | Top-level `CompilerError` enum |

#### 6. Standard Runtime Library Crate (`runtime`)
| Path | Role |
|---|---|
| `runtime/src/` | Pure Rust/C implementation of the Ryo memory and core runtime library |

---

Dependencies flow unidirectionally from Frontend/Backend/Driver down to Core. The `InternPool` from `types.rs` threads through every stage so identifiers and string literals stay as `Copy` `StringId` handles instead of owned `String`s. `sema.rs` drives decls through `Unresolved → InProgress → Resolved/Failed` with cycle detection wired in.

## Adding a New Language Feature

Follow this sequence:

### 1. Add Token (ryo-frontend/src/lexer.rs)
Use Logos attributes on the `Token` enum:
```rust
#[token("keyword")]  // Exact match
Keyword,
#[regex(r"[0-9]+")]  // Regex match
Number(&'a str),
```

### 2. Add AST Node (ryo-core/src/ast.rs)
- Add variant to `StmtKind` or `ExprKind`
- Define a struct if complex (like `VarDecl`, `FunctionDef`)
- Include `span: SimpleSpan` for error reporting
- All nodes use `SimpleSpan` from Chumsky

### 3. Add Parser Rule (ryo-frontend/src/parser.rs)
Use Chumsky combinators: `just(Token::X)` for exact match, `.then()` for sequence, `.or_not()` for optional, `.repeated()` for repetition, `.map_with()` to capture span.
```rust
let my_feature = just(Token::Keyword).ignore_then(expression_parser())
    .map_with(|expr, e| Statement { span: e.span(), kind: StmtKind::MyFeature(expr) });
```

### 4. Add UIR Instruction (ryo-core/src/uir.rs)
UIR is **untyped**. Add a tag to the `Inst` tag enum (and a payload in `InstData` if needed). For variable-size payloads (arg lists, body statement lists), encode them into the `extra: Vec<u32>` arena and reference them via `ExtraRange`. Add a span entry parallel to the instruction. Avoid nesting: each sub-expression is its own `InstRef`.

### 5. Add AstGen Case (ryo-frontend/src/astgen.rs)
In `astgen::generate` (and the per-stmt/per-expr helpers), translate the new AST node into UIR instructions. AstGen does *no* type checking — it only flattens the tree, interns identifiers via `InternPool`, and emits diagnostics through the `DiagSink` for structural issues.
```rust
ast::StmtKind::MyFeature(expr) => {
    let expr_ref = self.gen_expr(expr);
    self.emit(Inst::my_feature(expr_ref), stmt.span)
}
```

### 6. Add Sema Case (ryo-frontend/src/sema.rs) → emits TIR
In `sema::analyze`, type-check the UIR instruction and emit the typed equivalent into the per-function `Tir`. Resolve types via `InternPool`, look up names in the active scope, and push `Diag` values into the `DiagSink` on type errors (analysis continues — do not bail). Every emitted `TypedInst` carries its resolved `TypeId`.

### 7. Add Codegen (ryo-backend/src/codegen.rs)
Add a match arm in `compile_function()` where `TirInst` variants are dispatched. Use `Self::eval_expr()` to evaluate sub-expressions (which are themselves `TirRef`s into the same per-function arena). Common patterns: `builder.ins().iconst()` for ints, `.f64const()` for floats, `.iadd()`/`.fadd()` for add, `.call()` for calls.

### 8. Run All Tests
```bash
cargo test
```

## Error Handling

Middle-end stages emit structured `Diag` values (see `ryo-core/src/diag.rs`). `astgen::generate` and `sema::analyze` accumulate diagnostics through a `DiagSink` so analysis can continue past the first error; `parse_source` (in `ryo-driver/src/pipeline.rs`) builds `Diag` values directly from `chumsky::error::Rich` and renders them inline (no sink — the parser stops at the first round of errors anyway). All three converge on the same Ariadne-backed `render_diags` and surface as a single `CompilerError::Diagnostics(Vec<Diag>)` from the passes that use the sink (and from `parse_source` when parsing fails). Other stages still use string-typed `CompilerError` variants: `IoError`, `CodegenError`, `LinkError`, `ToolchainError`, `ExecutionError`.

## Testing

```bash
cargo test                      # All workspace tests
cargo test -p ryo-frontend      # Frontend-specific tests
cargo test -- --nocapture       # Show output
```

## Binary Inspection

`objdump -d` / `otool -tV` (disassembly), `nm` (symbols), `xxd` (hex dump).

## Related Documentation

- `docs/dev/pipeline_alignment.md` — detailed UIR/TIR pipeline design and Zig alignment rationale
- `docs/dev/implementation_roadmap.md` — feature roadmap
- `docs/CLAUDE.md` — language design and syntax rules

---

## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, read graphify-out/GRAPH_REPORT.md for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code files in this session, run `graphify update .` to keep the graph current (AST-only, no API cost)
