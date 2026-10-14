# Compiler Architecture & Developer Guide

This guide is for agents and contributors extending the Ryo compiler. Load it when working on compiler internals; the always-on rules live in the root `AGENTS.md`.

### Design Inspiration

- **Compiler architecture (lexer → parser → UIR → Sema → TIR → Flow → ownership/cleanup → codegen):** takes inspiration from the Zig compiler — see [`/docs/dev/pl_references/zig.md`](pl_references/zig.md). TIR stays Zig AIR-shaped; the Flow IR layer between TIR and codegen (explicit blocks/terminators/places — rustc-MIR-shaped in role, not in name) is Ryo's own addition, documented in [`docs/dev/pipeline_migration.md`](pipeline_migration.md).
- **Concurrency:** takes inspiration from Go — see [`/docs/dev/pl_references/go.md`](pl_references/go.md).
- **Ownership pass (`ryo-frontend/src/ownership/mod.rs` & `ryo-core/src/ownership.rs`):** takes inspiration from Mojo — see [`/docs/dev/pl_references/mojo.md`](pl_references/mojo.md). Zig has no borrow checker, so it is not a useful reference for move semantics. Mojo's MLIR-based lifetime/ASAP-destruction passes are the closest published precedent for what Ryo's spec commits to (no annotated lifetimes, parameters borrow by default, eager destruction at last use). Sema and the IRs themselves remain Zig-shaped.
- **`shared[T]` refcounting & ARC optimizer (planned; status lives in `docs/dev/implementation_roadmap.md`):** takes inspiration from Swift — see [`/docs/dev/arc_optimizer.md`](arc_optimizer.md). Swift's SIL ARC optimizer (aggressive retain/release elision, stack promotion, copy-on-write for collections) is the model. The performance promise of `shared[T]` in spec 5.6 depends on this pass actually existing and working; without it `shared[T]` benchmarks badly.
- **Comparison reference for Rust:** see [`/docs/dev/pl_references/rust.md`](pl_references/rust.md). Rust's `rustc_borrowck` and `Arc<T>` story is the obvious comparison point for both the ownership pass and `shared[T]`. Diagnostic UX bar is set against Rust's renderer.

### Rust Patterns ([Microsoft Rust Guidelines](https://microsoft.github.io/rust-guidelines/agents/all.txt))

- `// SAFETY:` comment on every `unsafe` block explaining soundness
- `debug_assert!` for internal invariants — zero cost in release builds
- Checked/saturating arithmetic for spans, offsets, indices — no silent overflow
- `PathBuf`/`&Path` for file paths, not `String`/`&str`; short-lived borrows across passes
- FFI: `#[repr(C)]` structs, no `String`/`Vec` across boundaries, safe wrappers for unsafe calls

### Compilation Pipeline

```text
Source → Lexer → Indent Preprocessor → Parser → AstGen → UIR → Sema → TIR → Flow → Ownership → Cleanup (DropOps) → Codegen → Linker → Executable
```

(**Flow** — `ryo-core/src/flow.rs` — is the canonical control-flow graph built once per function body after sema: indexed blocks with explicit terminators, a place table for storage identity, and per-step read/write/view facts. Both consumers drive from it: the **Ownership** pass analyzes Flow blocks/terminators post-sema (see `docs/dev/pl_references/mojo.md` and `ryo-frontend/src/ownership/mod.rs`), its cleanup decisions are explicit `DropOp`s positioned on Flow blocks, and **Codegen** maps Flow blocks to Cranelift blocks and dispatches terminators — neither pass reconstructs control flow from TIR statement lists. The authoritative design and phase contracts live in `docs/dev/pipeline_migration.md`; see `docs/dev/pattern_matching.md` for the match representation contracts.)

The middle-end is split into two flat-arena IRs modeled after Zig's ZIR/AIR:

- **UIR** (`ryo-core/src/uir.rs`) — Untyped IR. Flat `(tag, data)` instruction stream in a program-wide arena, produced by `astgen.rs` from the AST. Sub-expressions are not nested; they live as their own entries reached via `InstRef` indices. Side arenas: `extra: Vec<u32>` for variable-size payloads, `spans` parallel to instructions.
- **TIR** (`ryo-core/src/tir.rs`) — Typed IR. Same flat shape as UIR but **one arena per function body**, and every instruction carries its resolved `TypeId`. Produced by `sema.rs` from UIR and consumed by `codegen/`. Per-function arenas make generic/inline duplication a `Tir::clone` away.

**Mapping to Zig.** When cross-referencing the Zig compiler source for reference:

| Ryo  | Zig file       | Role |
|------|----------------|------|
| UIR  | `src/Zir.zig`  | Flat, untyped instruction stream emitted by `astgen` from the AST. Input to Sema. |
| TIR  | `src/Air.zig`  | Flat, fully-typed instruction stream emitted by Sema, one per function body. Input to codegen. |

Ryo does not produce or consume Zig's ZIR/AIR — these are upstream design references only.

**Niche-filled refs.** `InstRef` (UIR) and `TirRef` (TIR) wrap `NonZeroU32`, so `Option<InstRef>` / `Option<TirRef>` fit in a single 32-bit slot. Slot 0 of each `instructions` arena is a reserved sentinel that is never emitted — do not hand out `InstRef(0)` / `TirRef(0)`, and do not assume index 0 is a real instruction when iterating.

See `docs/dev/pipeline_alignment.md` for what remains of the Zig-alignment plan (pending features, divergence notes, future considerations); the shipped phase plan lives in git history.

**Crate and Module Map** (distributed in workspace packages):

#### 1. Binary Executive Crate (`ryo`)
| File | Role |
|------|------|
| `ryo/src/main.rs` | CLI definition (clap) and command dispatch |
| `ryo/build.rs` | Git version status parsing and JIT/AOT runtime building script |

#### 2. Driver Orchestration Crate (`ryo-driver`)
| File | Role |
|------|------|
| `ryo-driver/src/pipeline.rs` | Orchestrates pipeline stages: lex → parse → astgen → sema → ownership → codegen → link → run |

#### 3. Frontend Compilation Crate (`ryo-frontend`)
| File | Role |
|---|---|
| `ryo-frontend/src/lexer.rs` | Logos-based tokenizer; emits interned `Token` stream |
| `ryo-frontend/src/indent.rs` | CPython-style Indent/Dedent token insertion over raw lexer output |
| `ryo-frontend/src/parser.rs` | Chumsky-based parser over `Token` (produces AST) |
| `ryo-frontend/src/astgen.rs` | AST → UIR structural lowering |
| `ryo-frontend/src/sema.rs` | Semantic analysis: type-checks UIR, emits one TIR per function body |
| `ryo-frontend/src/ownership/mod.rs` | Post-sema, pre-codegen ownership flow analysis |
| `ryo-frontend/src/builtins.rs` | Builtin function and runtime ABI callee registry |

#### 4. Code Generation and Linking Crate (`ryo-backend`)
| File | Role |
|------|------|
| `ryo-backend/src/codegen/` | Cranelift IR generation from TIR (JIT and AOT) |
| `ryo-backend/src/linker.rs` | Executable linking via the managed Zig toolchain |
| `ryo-backend/src/toolchain.rs` | Zig toolchain download / version pinning / path resolution |
| `ryo-backend/src/runtime_lib.rs` | Static runtime library extraction and caching |
| `ryo-backend/build.rs` | Compiles `ryo-runtime` on demand and exposes environment paths |

#### 5. Shared Core Crate (`ryo-core`)
| File | Role |
|------|------|
| `ryo-core/src/ast.rs` | Surface-syntax AST: typed arenas (`Vec<Expr>` / `Vec<Stmt>` indexed by `ExprId` / `StmtId`, with `ExprKind` / `StmtKind` payload enums, list side arenas, and inline spans); identifiers/types/strings stored as `StringId` |
| `ryo-core/src/uir.rs` | Untyped IR data structures (flat, program-wide arena) |
| `ryo-core/src/tir.rs` | Typed IR data structures (flat, per-function arena) |
| `ryo-core/src/flow.rs` | Canonical control flow per function body: indexed blocks + exhaustive terminators (branch/switch/loop-backedge/return/diverging call), the place table (binding identity never keyed by name), per-step read/write/view facts, and `DropOp` positions — the shared execution-structure + storage-identity model for ownership analysis and codegen |
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

### 2. Add AST Enum Variant (ryo-core/src/ast.rs)
The AST is a pair of typed arenas (`exprs` / `stmts`), not a pointer tree:
- Add a variant to `StmtKind` or `ExprKind` with `ExprId` / `StmtId` children (never boxed subtrees)
- Variable-size child lists (arg lists, block bodies) go into the `expr_lists` / `stmt_lists` side arenas behind `ExprList` / `StmtList` ranges; scalar payload structs (`VarDecl`, `FunctionDef`, …) stay inline in the variant
- Add a builder method on `Ast` (pushes the value with its inline span, returns the id)
- Consumers write plain exhaustive `match`es on `expr.kind` / `stmt.kind` — adding a variant makes the compiler flag every site you must update

### 3. Add Parser Rule (ryo-frontend/src/parser.rs)
Use Chumsky combinators: `just(Token::X)` for exact match, `.then()` for sequence, `.or_not()` for optional, `.repeated()` for repetition. The parser is stateful: the `Ast` arenas are the chumsky state (`extra::Full<_, Ast, _>`, entered via `parse_with_state`), so node-producing combinators push through `e.state()` and yield `ExprId` / `StmtId` (annotate the closure param as `e: &mut Mx<'a, '_, I>` so `e.state()` type-checks). Use `foldl_with` when folding needs state.
```rust
let my_feature = just(Token::Keyword).ignore_then(expr.clone())
    .map_with(|expr, e: &mut Mx<'a, '_, I>| {
        let span = e.span();
        e.state().my_feature(expr, span)
    });
```

⚠️ Never call `expression_parser()` inside a statement rule — take the shared parser as a parameter (see "Parser Grammar Construction").

### 4. Add UIR Instruction (ryo-core/src/uir.rs)
UIR is **untyped**. Add a tag to the `Inst` tag enum (and a payload in `InstData` if needed). For variable-size payloads (arg lists, body statement lists), encode them into the `extra: Vec<u32>` arena and reference them via `ExtraRange`. Add a span entry parallel to the instruction. Avoid nesting: each sub-expression is its own `InstRef`.

### 5. Add AstGen Case (ryo-frontend/src/astgen.rs)
In `astgen::generate` (and the per-stmt/per-expr helpers), match the new variant and translate it into UIR instructions, following child ids via `ast.expr(id)` / `ast.stmt(id)` / `ast.stmt_list(range)`. AstGen does *no* type checking — it only flattens the AST into UIR, interns identifiers via `InternPool`, and emits diagnostics through the `DiagSink` for structural issues.
```rust
ast::StmtKind::MyFeature(expr) => {
    let expr_ref = gen_expr(b, ast, *expr);
    let r = b.my_feature(expr_ref, ast.stmt_span(stmt));
    out.push(r);
}
```

### 6. Add Sema Case (ryo-frontend/src/sema.rs) → emits TIR
In `sema::analyze`, type-check the UIR instruction and emit the typed equivalent into the per-function `Tir`. Resolve types via `InternPool`, look up names in the active scope, and push `Diag` values into the `DiagSink` on type errors (analysis continues — do not bail). Every emitted `TypedInst` carries its resolved `TypeId`.

### 7. Add Codegen (ryo-backend/src/codegen/)
Add a match arm in `compile_function()` where `TirInst` variants are dispatched. Use `Self::eval_expr()` to evaluate sub-expressions (which are themselves `TirRef`s into the same per-function arena). Common patterns: `builder.ins().iconst()` for ints, `.f64const()` for floats, `.iadd()`/`.fadd()` for add, `.call()` for calls.

### 8. Run All Tests
```bash
cargo test
```

### 9. Update the Tree-sitter Grammar
Language-grammar changes (new keywords, literals, operators, statement forms) must also land in the editor grammar: the `tree-sitter-ryo` repo (standalone sibling checkout — regenerate the parser, extend `queries/highlights.scm`, add corpus tests). Without this, editor highlighting silently drifts from the language.

### Parser Grammar Construction

Grammar builders like `expression_parser()` construct the full parser graph eagerly on every call (`recursive()` allocates a fresh slot each time). Calling one inside a rule that `program_parser()` instantiates per parse multiplies that fixed cost for every parse of every file. Example: the M10 destructuring rule called it 3× at 2 instantiation sites — 6 full grammar builds per parse (~20% slower parse_snippet); fixed in 8ff1ab7 by building the grammar once in `top_level_statement_parser` and threading it into rules as an `expr: impl Parser<...> + Clone` parameter (`Rc` clones are cheap).

- **Adding a rule:** take the shared expression parser as a parameter; never build your own.
- **Reviewing:** any new `expression_parser()` call site in `parser.rs` is a red flag.
- **Verifying:** `cargo bench -p ryo-frontend -- parse_snippet` (A/B against the base commit; CodSpeed CI tracks these benches).
- **Hot choices pay for failed alternatives.** In chumsky every failed alternative in a `choice()` costs `Rich`-merging bookkeeping, and the postfix loop ends *every* expression with a failed op — so adding a `.`-led op as a naked alternative to the postfix choice taxed every expression in every program (two enum ops regressed `parse_large` ~10% even in enum-free code; fixed in 2830679 by parsing one shared `.` and choosing among tails). New postfix-shaped syntax enters through the shared `dot_op` discriminator, never as an extra raw alternative. When you factor a shared prefix out of ops, spans contract to the tail — re-stamp them (`PostfixOp::with_span`) or every diagnostic underline shifts.

## Error Handling

Middle-end stages emit structured `Diag` values (see `ryo-core/src/diag.rs`). `astgen::generate` and `sema::analyze` accumulate diagnostics through a `DiagSink` so analysis can continue past the first error; `parse_source` (in `ryo-driver/src/pipeline.rs`) shares the same model. `lexer::lex` never fails hard — it recovers so the parser still sees a well-formed token stream — and the parser synchronizes at statement boundaries, yielding a diagnostic plus a partial AST with `Error` placeholder nodes (R10), so one syntax error never suppresses diagnostics for the rest of the file. The only early bail is an indentation failure, where the token stream is empty and parsing would be meaningless. All stages converge on the same Ariadne-backed `render_diags` and surface as a single `CompilerError::Diagnostics(Vec<Diag>)`. Other stages still use string-typed `CompilerError` variants: `IoError`, `CodegenError`, `LinkError`, `ToolchainError`, `ExecutionError`.

## Testing

```bash
cargo test                      # All workspace tests
cargo test -p ryo-frontend      # Frontend-specific tests
cargo test -- --nocapture       # Show output
```

**Miri (runtime crate) runs with isolation enabled:** `open`, stdin reads, and process spawning are forbidden (stdout/stderr writes are allowed). Gate test bodies that need them behind `if cfg!(miri) { return; }` — subprocess blocks and temp-file I/O — while leaving the in-process unsafe coverage unconditional. The fd-based `read_line_from` path has no Miri coverage for this reason; ASan/Valgrind and normal runs carry it.

**Run Miri locally before pushing runtime changes** (`cargo +nightly miri test -p ryo-runtime`). Miri is installed on the dev machine, and any change to a runtime function that an existing test calls — especially one adding a libc/foreign call — can break the Miri job in ways `cargo test` cannot see (isolation-forbidden foreign calls fail only under Miri's interpreter).

## Binary Inspection

`objdump -d` / `otool -tV` (disassembly), `nm` (symbols), `xxd` (hex dump).

## References

- Dev: [pipeline_migration.md](pipeline_migration.md) — compiler pipeline + Flow IR design: representation contracts, phase boundaries, migration steps (authoritative for the Flow layer)
- Dev: [pipeline_alignment.md](pipeline_alignment.md) — UIR/TIR pipeline: remaining work (comptime/generics substrate), Zig divergence notes, future considerations
- Dev: [implementation_roadmap.md](implementation_roadmap.md) — feature roadmap
- Dev: [ryo-compiler-llm-instructions.md](ryo-compiler-llm-instructions.md) — **normative contributor rules (R1–R21)** for compiler work: data-oriented design, Rust discipline, diagnostics, Cranelift discipline, testing
- Dev: [ryo-slicing-and-memory-model-final-spec.md](ryo-slicing-and-memory-model-final-spec.md) — slicing/views/memory-model decisions (D1–D11); amends the base spec
- Docs: [docs/AGENTS.md](../AGENTS.md) — language design and syntax rules
