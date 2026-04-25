# Ryo Compiler Developer Guide

This guide is for agents extending the Ryo compiler. For language design, see `/docs/CLAUDE.md`.

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
Source → Lexer → Indent Preprocessor → Parser → Lower (HIR) → Codegen → Linker → Executable
```

**Key files:** `pipeline.rs` (orchestrates the full pipeline), `builtins.rs` (built-in functions like `print`), `errors.rs` (`CompilerError` enum and diagnostics), `toolchain.rs` (Zig linker download/management).

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

### 4. Add HIR Node (hir.rs)
HIR is fully typed — add variant to `HirStmt` or `HirExprKind`. All expressions have `.ty: Type` field.

### 5. Add Lowering Case (lower.rs)
In `lower_stmt()` or `lower_expr()`, resolve types and use scope for lookups. Type errors return `Err(String)`.
```rust
ast::StmtKind::MyFeature(expr) => {
    let hir_expr = lower_expr(expr, scope, signatures)?;
    out.push(HirStmt::MyFeature(hir_expr, stmt.span));
}
```

### 6. Add Codegen (codegen.rs)
Add a match arm in `compile_function()` where `HirStmt` variants are dispatched. Use `Self::eval_expr()` to evaluate sub-expressions. Common patterns: `builder.ins().iconst()` for constants, `.iadd()` for add, `.call()` for calls. Store locals as `Variable` in `locals` HashMap.
```rust
HirStmt::MyFeature(expr, _) => {
    let val = Self::eval_expr(&mut builder, expr, &mut func_ctx)?;
    // use val...
}
```

### 7. Add Test
- **Integration tests** (`tests/integration_tests.rs`): end-to-end compilation and execution, error handling, CLI commands. Use when the test compiles and runs a `.ryo` file.
- **Inline unit tests** (`mod tests` in each `.rs`): isolated module behavior — lexer tokens, parser output, lowering logic. Use when testing a single pipeline stage.

## Error Handling

Middle-end stages emit structured `Diag` values (see `src/diag.rs`). `ast_lower` and `sema` accumulate diagnostics through a `DiagSink` so analysis can continue past the first error; `parse_source` builds `Diag` values directly from `chumsky::error::Rich` and renders them inline (no sink — the parser stops at the first round of errors anyway). All three converge on the same Ariadne-backed `render_diags` and surface as a single `CompilerError::Diagnostics(Vec<Diag>)` from the passes that use the sink (and from `parse_source` when parsing fails). Other stages still use string-typed `CompilerError` variants: `IoError`, `CodegenError`, `LinkError`, `ToolchainError`, `ExecutionError`.

## Testing

```bash
cargo test                      # All tests
cargo test test_name            # Specific test
cargo test -- --nocapture       # Show output
```

## Binary Inspection

`objdump -d` / `otool -tV` (disassembly), `nm` (symbols), `xxd` (hex dump).

## Related Documentation

- `docs/dev/compilation_pipeline.md` — detailed pipeline documentation
- `docs/dev/implementation_roadmap.md` — feature roadmap
- `docs/CLAUDE.md` — language design and syntax rules
