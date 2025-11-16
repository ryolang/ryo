## Architecture & Code Organization

Apply the CRISP and SOLID patterns.

**Compilation pipeline**:
```
Source → Lexer → Parser → Codegen → Object File → Linker → Executable
```

## Testing Strategy

### Test File Locations

```
tests/
└── integration_tests.rs    # Integration tests

src/
├── parser.rs               # Parser unit tests (mod tests)
└── ...                     # Other unit tests inline
```

### Integration Tests

Located in `tests/integration_tests.rs`, these tests verify:
- End-to-end compilation and execution
- Filename-based output generation
- Error handling (parse errors, file not found)
- Lex command functionality
- Complex expression compilation

Tests MUST use temporary directories and automatic cleanup via RAII patterns.

### Running Tests

```bash
# All tests
cargo test

# Specific test
cargo test test_addition

# With output
cargo test -- --nocapture

# Integration tests only
cargo test --test integration_tests
```

## GNU Binutils for Linux
```shell
objdump -d output.o
objdump -h output.o
objdump -x output.o
```

## macOS
```shell
otool -tV output.o
otool -h output.o
otool -l output.o
```

```shell
nm output.o
```

```shell
xxd output.o | less
hexdump -C output.o | less
```

---

## Troubleshooting

### Common Build Issues

**Issue**: `error: linker 'cc' not found`
- **Solution**: Install build essentials
  ```bash
  # Ubuntu/Debian
  sudo apt install build-essential

  # macOS
  xcode-select --install

  # Or install Zig for zig cc
  ```

**Issue**: Cranelift version mismatch
- **Solution**: Update all Cranelift dependencies together
  ```bash
  cargo update -p cranelift -p cranelift-jit -p cranelift-module -p cranelift-object
  ```

### Runtime Issues

**Issue**: Generated executable crashes
- **Debug**: Check object file with `objdump` or `otool`
- **Check**: Linker used (`zig cc` vs `clang` vs `cc`)
- **Verify**: Target triple matches host architecture

**Issue**: Parse errors not helpful
- **Solution**: Use `ryo lex <file>` to check tokens first
- **Debug**: Add debug prints in parser (temporarily)

---

## Related Documentation

**Developer documentation:**
- `docs/dev/README.md` - Developer docs index
- `docs/dev/implementation_roadmap.md` - Development milestones and current status
- `docs/dev/project_structure.md` - Codebase architecture
- `docs/dev/compilation_pipeline.md` - Compilation details
- `docs/dev/token.rs.md` - Lexer implementation, to be deleted once implemented in source code
- `docs/dev/parser.rs.md` - Parser implementation, to be deleted once implemented in source code

---
