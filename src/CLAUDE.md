# Ryo Compiler Developer Guide

This guide is for agents extending the Ryo compiler. For language design, see `/docs/CLAUDE.md`.

## Code Quality

Always run before committing (CI enforces these with `-Dwarnings`):
```bash
cargo fmt --check           # CI: must pass with no diffs
cargo clippy --all-targets  # CI: warnings are errors
```
Run `cargo fmt` (without `--check`) to auto-fix formatting.

## Compilation Pipeline

```
Source → Lexer → Indent Preprocessor → Parser → Lower (HIR) → Codegen → Linker → Executable
```

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
Emit Cranelift IR in `compile_stmt()` or `compile_expr()`. Common patterns: `builder.ins().iconst()` for constants, `.iadd()` for add, `.call()` for calls. Store locals as `Variable` in `locals` HashMap.
```rust
HirStmt::MyFeature(expr, _) => {
    let val = self.compile_expr(expr, builder, fctx)?;
    builder.ins().iconst(self.int_type, val);
}
```

### 7. Add Test
Add to `tests/integration_tests.rs` or inline `mod tests`.

## Error Handling

`CompilerError` enum propagates errors: `IoError`, `ParseError`, `LowerError`, `CodegenError`, `LinkError`, `ToolchainError`. Convert with `.map_err(CompilerError::ParseError)`.

## Testing

```bash
cargo test                      # All tests
cargo test test_name            # Specific test
cargo test -- --nocapture       # Show output
```

## Binary Inspection

Verify codegen output (macOS/Linux):
```bash
objdump -d output.o  # disassembly
nm output.o          # symbols
```

## Related Documentation

- `docs/dev/compilation_pipeline.md` — detailed pipeline documentation
- `docs/dev/implementation_roadmap.md` — feature roadmap
- `docs/CLAUDE.md` — language design and syntax rules
