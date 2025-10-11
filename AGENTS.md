# Ryo Programming Language

Ryo is a statically-typed, compiled programming language designed for productivity, safety, and performance. The current implementation is a minimal compiler that supports basic arithmetic expressions and demonstrates the full compilation pipeline from source code to native executables.

**Current Status:** Early development (Milestone 3 of implementation roadmap) - basic expression compilation working end-to-end.

## Development

### Commands

```bash
# Build the project
cargo build
cargo build --release

# Run all tests (unit + integration)
cargo test

# Run only integration tests
cargo test --test integration_tests

# Run a specific test
cargo test test_output_filename_generation

# Run with detailed output
cargo test -- --nocapture

# Use the Ryo compiler
cargo run -- run <filename.ryo>  # Compile and run
cargo run -- lex <filename.ryo>  # Show tokens
cargo run -- --help              # Show help

# Code quality
cargo check    # Check without building
cargo fmt      # Format code
cargo clippy   # Run lints
```

### Current Language Support

The compiler currently supports:
- Integer literals
- Basic arithmetic operators: `+`, `-`, `*`, `/`
- Parentheses for grouping
- Expression evaluation with proper operator precedence

Example working programs:
```ryo
1 + 2
5 * (10 + 2)
((10 + 5) * 2) - (8 / 4)
```

## Architecture

### Current Structure (Monolithic)

The current implementation uses a single-crate structure that will be refactored into a multi-crate workspace as the project grows:

- **`main.rs`**: CLI interface using clap, orchestrates compilation pipeline
- **`lexer.rs`**: Tokenization using logos (basic arithmetic operators + integers)
- **`parser.rs`**: Recursive descent parser using chumsky, produces AST
- **`ast.rs`**: Abstract Syntax Tree definitions with pretty-printing
- **`codegen.rs`**: Code generation using Cranelift backend
- **`evaluator.rs`**: Simple AST evaluator (legacy, not used in main pipeline)

### Compilation Pipeline

The current pipeline implements these phases:
1. **File Reading** → 2. **Lexing** → 3. **Parsing** → 4. **Code Generation** → 5. **Object File Writing** → 6. **Linking** → 7. **Execution**

### Error Handling

- Custom `CompilerError` enum with specific variants (IoError, ParseError, CodegenError, LinkError, ExecutionError)
- Ariadne integration for beautiful error reporting with source context
- Proper error propagation using `Result<T, CompilerError>` throughout

### Key Design Decisions

- **Filename-based output**: `input.ryo` → `input.o` + `input` executable
- **Linker fallback chain**: zig cc → clang → cc (for cross-platform compatibility)
- **Clean function separation**: Each compilation phase has dedicated functions
- **Integration testing**: Comprehensive tests covering end-to-end workflows

### Dependencies

- **Frontend**: `logos` (lexing), `chumsky` (parsing), `clap` (CLI)
- **Backend**: `cranelift` family (code generation), `target-lexicon` (target detection)
- **UX**: `ariadne` (error reporting)
- **Testing**: `tempfile` (integration tests)

## Future Architecture (Planned)

The planned multi-crate workspace structure includes:
- `ryo-core`: Shared definitions (Token, AST, Types)
- `ryo-parser`: Lexer and parser logic
- `ryo-checker`: Semantic analysis (type checking, borrow checking)
- `ryo-codegen-clif`: Cranelift code generation
- `ryo-runtime`: Runtime support library
- `ryo-driver`: Compilation pipeline orchestration
- `ryo`: Main CLI binary

## Testing

### Integration Tests

Located in `tests/integration_tests.rs`, these tests verify:
- End-to-end compilation and execution
- Filename-based output generation
- Error handling (parse errors, file not found)
- Lex command functionality
- Complex expression compilation

Tests use temporary directories and automatic cleanup via RAII patterns.

### Test Examples

```bash
# Test basic functionality
echo "1 + 2" > test.ryo
cargo run -- run test.ryo  # Should exit with code 3

# Test tokenization
cargo run -- lex test.ryo  # Shows token stream

# Test error handling
echo "1 + @ invalid" > error.ryo
cargo run -- run error.ryo  # Shows parse error with context
```

## Current Limitations

- No variables, functions, or control flow yet
- Only integer arithmetic expressions supported
- No standard library or imports
- Single-file compilation only
- No optimization passes implemented

## Implementation Notes

- **Exit codes as results**: Compiled programs return their computed value as the exit code
- **Clean builds**: Integration tests automatically clean up generated `.o` and executable files
- **Error handling**: Parse errors show exact location with visual indicators
- **Performance tracking**: Linking time is measured and reported
- **Cross-platform**: Automatic linker detection supports multiple environments

This represents the foundation for building toward the full Ryo language specification as outlined in the implementation roadmap.