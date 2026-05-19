# Ryo Compiler Developer Guide

This guide is for agents extending the Ryo compiler. For language design, see `/docs/CLAUDE.md`.

## Design Inspiration

- **Compiler architecture (lexer → parser → UIR → Sema → TIR → Codegen):** takes inspiration from the Zig compiler — see [`/docs/dev/zig_reference.md`](../docs/dev/zig_reference.md).
- **Concurrency:** takes inspiration from Go — see [`/docs/dev/go_reference.md`](../docs/dev/go_reference.md).
- **Ownership pass (`src/ownership.rs`, M8.1+):** takes inspiration from Mojo — see [`/docs/dev/mojo_reference.md`](../docs/dev/mojo_reference.md). Zig has no borrow checker, so it stops being a useful reference once M8.1 introduces move semantics. Mojo's MLIR-based lifetime/ASAP-destruction passes are the closest published precedent for what Ryo's spec already commits to (no annotated lifetimes, parameters borrow by default, eager destruction at last use). Sema and the IRs themselves remain Zig-shaped.
- **`shared[T]` refcounting & ARC optimizer (`src/arc_optimizer.rs`, post-M11):** takes inspiration from Swift — see [`/docs/dev/arc_optimizer.md`](../docs/dev/arc_optimizer.md). Swift's SIL ARC optimizer (aggressive retain/release elision, stack promotion, copy-on-write for collections) is the model. The performance promise of `shared[T]` in spec 5.6 depends on this pass actually existing and working; without it `shared[T]` benchmarks badly.
- **Comparison reference for Rust:** see [`/docs/dev/rust_reference.md`](../docs/dev/rust_reference.md). Rust's `rustc_borrowck` and `Arc<T>` story is the obvious comparison point for both the ownership pass and `shared[T]`. Diagnostic UX bar is set against Rust's renderer.

## Code Quality

Always run before committing (CI enforces these with `-Dwarnings`):
```bash
cargo fmt                   # Auto-format (CI runs --check)
cargo clippy --all-targets  # Lint (CI: warnings are errors)
```

## Rust Patterns ([Microsoft Rust Guidelines](https://microsoft.github.io/rust-guidelines/agents/all.txt))

- `// SAFETY:` comment on every `unsafe` block explaining soundness
- `debug_assert!` for internal invariants — zero cost in release builds
- Checked/saturating arithmetic for spans, offsets, indices — no silent overflow
- `PathBuf`/`&Path` for file paths, not `String`/`&str`; short-lived borrows across passes
- FFI: `#[repr(C)]` structs, no `String`/`Vec` across boundaries, safe wrappers for unsafe calls

## Compilation Pipeline

```text
Source → Lexer → Indent Preprocessor → Parser → AstGen → UIR → Sema → TIR → Ownership → TIR' → Codegen → Linker → Executable
```

(The **Ownership** stage lands in M8.1 — see `docs/dev/mojo_reference.md` and `docs/superpowers/specs/2026-05-11-milestone-8.1-heap-str-and-move-semantics-design.md`. Before M8.1 the pipeline goes straight from Sema to Codegen.)

The middle-end is split into two flat-arena IRs modeled after Zig's ZIR/AIR:

- **UIR** (`uir.rs`) — Untyped IR. Flat `(tag, data)` instruction stream in a program-wide arena, produced by `astgen.rs` from the AST. Sub-expressions are not nested; they live as their own entries reached via `InstRef` indices. Side arenas: `extra: Vec<u32>` for variable-size payloads, `spans` parallel to instructions.
- **TIR** (`tir.rs`) — Typed IR. Same flat shape as UIR but **one arena per function body**, and every instruction carries its resolved `TypeId`. Produced by `sema.rs` from UIR and consumed by `codegen.rs`. Per-function arenas make generic/inline duplication a `Tir::clone` away.

**Mapping to Zig.** When cross-referencing the Zig compiler source for reference:

| Ryo  | Zig file       | Role |
|------|----------------|------|
| UIR  | `src/Zir.zig`  | Flat, untyped instruction stream emitted by `astgen` from the AST. Input to Sema. |
| TIR  | `src/Air.zig`  | Flat, fully-typed instruction stream emitted by Sema, one per function body. Input to codegen. |

Ryo does not produce or consume Zig's ZIR/AIR — these are upstream design references only.

**Niche-filled refs.** `InstRef` (UIR) and `TirRef` (TIR) wrap `NonZeroU32`, so `Option<InstRef>` / `Option<TirRef>` fit in a single 32-bit slot. Slot 0 of each `instructions` arena is a reserved sentinel that is never emitted — do not hand out `InstRef(0)` / `TirRef(0)`, and do not assume index 0 is a real instruction when iterating.

See `docs/dev/pipeline_alignment.md` for the full design rationale (motivation, phase plan, comptime/generics implications).

**Module map** (all of `src/`, in pipeline order):

| File | Role |
|------|------|
| `main.rs` | CLI definition (clap) and command dispatch |
| `pipeline.rs` | Orchestrates lex → parse → astgen → sema → codegen → link → run |
| `lexer.rs` | Logos tokenizer; emits interned `Token` stream (`StringId`/`i64` payloads) |
| `indent.rs` | CPython-style `Indent`/`Dedent` token insertion over raw lexer output |
| `parser.rs` | Chumsky parser over `Token` → AST |
| `ast.rs` | Surface-syntax AST; identifiers/types/strings stored as `StringId` |
| `astgen.rs` | AST → UIR structural lowering (no type checking) |
| `uir.rs` | Untyped IR data structures (flat, program-wide arena) |
| `sema.rs` | Worklist-driven semantic analysis: UIR → one TIR per function body (type-check only; ownership lives in `ownership.rs`) |
| `tir.rs` | Typed IR data structures (flat, per-function arena) |
| `ownership.rs` | Post-sema, pre-codegen ownership pass (M8.1+). Per-`TirRef` move/borrow tracking with DX-grade diagnostics. Mojo-inspired. Mutates each `Tir` in place by inserting `TirTag::Free`. |
| `arc_optimizer.rs` | Refcount elision pass for `shared[T]` values (post-M11; design in `docs/dev/arc_optimizer.md`). Runs after `ownership.rs` on the same TIR; removes redundant retain/release pairs and (optionally) stack-promotes non-escaping shared values. Swift-inspired. Perf-only — never changes program meaning. |
| `types.rs` | `InternPool` for types and strings; `TypeId` / `StringId` newtypes |
| `diag.rs` | Structured diagnostics: `Diag`, `DiagCode`, `DiagSink` |
| `builtins.rs` | Builtin function registry (currently `print`) |
| `codegen.rs` | Cranelift IR generation from TIR (JIT and AOT) |
| `linker.rs` | Executable linking via the managed Zig toolchain |
| `toolchain.rs` | Zig toolchain download / version pinning / path resolution |
| `errors.rs` | Top-level `CompilerError` enum |

Dependencies flow top-to-bottom. The `InternPool` from `types.rs` threads through every stage so identifiers and string literals stay as `Copy` `StringId` handles instead of owned `String`s. `sema.rs` drives decls through `Unresolved → InProgress → Resolved/Failed` with cycle detection wired in for future inferred return types, comptime, and generics. The crate is intentionally a single flat module set (~7.8K lines); see `docs/dev/project_structure.md` for the future workspace-split plan.

## Adding a New Language Feature

Follow this sequence:

### 1. Add Token (lexer.rs)
Use Logos attributes on the `Token` enum:
```rust
#[token("keyword")]  // Exact match
Keyword,
#[regex(r"[0-9]+")]  // Regex match
Number(&'a str),
```

### 2. Add AST Node (ast.rs)
- Add variant to `StmtKind` or `ExprKind`
- Define a struct if complex (like `VarDecl`, `FunctionDef`)
- Include `span: SimpleSpan` for error reporting
- All nodes use `SimpleSpan` from Chumsky

### 3. Add Parser Rule (parser.rs)
Use Chumsky combinators: `just(Token::X)` for exact match, `.then()` for sequence, `.or_not()` for optional, `.repeated()` for repetition, `.map_with()` to capture span.
```rust
let my_feature = just(Token::Keyword).ignore_then(expression_parser())
    .map_with(|expr, e| Statement { span: e.span(), kind: StmtKind::MyFeature(expr) });
```

### 4. Add UIR Instruction (uir.rs)
UIR is **untyped**. Add a tag to the `Inst` tag enum (and a payload in `InstData` if needed). For variable-size payloads (arg lists, body statement lists), encode them into the `extra: Vec<u32>` arena and reference them via `ExtraRange`. Add a span entry parallel to the instruction. Avoid nesting: each sub-expression is its own `InstRef`.

### 5. Add AstGen Case (astgen.rs)
In `astgen::generate` (and the per-stmt/per-expr helpers), translate the new AST node into UIR instructions. AstGen does *no* type checking — it only flattens the tree, interns identifiers via `InternPool`, and emits diagnostics through the `DiagSink` for structural issues.
```rust
ast::StmtKind::MyFeature(expr) => {
    let expr_ref = self.gen_expr(expr);
    self.emit(Inst::my_feature(expr_ref), stmt.span)
}
```

### 6. Add Sema Case (sema.rs) → emits TIR
In `sema::analyze`, type-check the UIR instruction and emit the typed equivalent into the per-function `Tir`. Resolve types via `InternPool`, look up names in the active scope, and push `Diag` values into the `DiagSink` on type errors (analysis continues — do not bail). Every emitted `TypedInst` carries its resolved `TypeId`.

### 7. Add Codegen (codegen.rs)
Add a match arm in `compile_function()` where `TirInst` variants are dispatched. Use `Self::eval_expr()` to evaluate sub-expressions (which are themselves `TirRef`s into the same per-function arena). Common patterns: `builder.ins().iconst()` for ints, `.f64const()` for floats, `.iadd()`/`.fadd()` for add, `.call()` for calls. Store locals as Cranelift `Variable`s in the `locals` map.
```rust
TirInst::MyFeature { expr, .. } => {
    let val = Self::eval_expr(&mut builder, expr, &mut func_ctx)?;
    // use val...
}
```

### 8. Add Test
- **Integration tests** (`tests/integration_tests.rs`): end-to-end compilation and execution, error handling, CLI commands. Use when the test compiles and runs a `.ryo` file.
- **Inline unit tests** (`mod tests` in each `.rs`): isolated module behavior — lexer tokens, parser output, lowering logic. Use when testing a single pipeline stage.

## Error Handling

Middle-end stages emit structured `Diag` values (see `src/diag.rs`). `astgen::generate` and `sema::analyze` accumulate diagnostics through a `DiagSink` so analysis can continue past the first error; `parse_source` (in `pipeline.rs`) builds `Diag` values directly from `chumsky::error::Rich` and renders them inline (no sink — the parser stops at the first round of errors anyway). All three converge on the same Ariadne-backed `render_diags` and surface as a single `CompilerError::Diagnostics(Vec<Diag>)` from the passes that use the sink (and from `parse_source` when parsing fails). Other stages still use string-typed `CompilerError` variants: `IoError`, `CodegenError`, `LinkError`, `ToolchainError`, `ExecutionError`.

## Testing

```bash
cargo test                      # All tests
cargo test test_name            # Specific test
cargo test -- --nocapture       # Show output
```

## Binary Inspection

`objdump -d` / `otool -tV` (disassembly), `nm` (symbols), `xxd` (hex dump).

## Related Documentation

- `docs/dev/pipeline_alignment.md` — detailed UIR/TIR pipeline design and Zig alignment rationale
- `docs/dev/project_structure.md` — crate layout and workspace-split guidance
- `docs/dev/implementation_roadmap.md` — feature roadmap
- `docs/CLAUDE.md` — language design and syntax rules
