# CLAUDE.md - Ryo Programming Language Reference Guide

> **Context Document for AI Assistants and Contributors**
>
> This document provides comprehensive context about the Ryo programming language project, its architecture, conventions, and development practices. Last updated: 2025-11-05

---

## Table of Contents

1. [Project Overview](#project-overview)
2. [Language Inspirations](#language-inspirations)
3. [Current Implementation Status](#current-implementation-status)
4. [Architecture & Code Organization](#architecture--code-organization)
5. [Syntax Conventions](#syntax-conventions)
6. [Development Workflow](#development-workflow)
7. [Key Design Decisions](#key-design-decisions)
8. [Common Tasks](#common-tasks)
9. [Areas Needing Attention](#areas-needing-attention)

---

## Project Overview

**Ryo** is an early-stage (pre-alpha) statically-typed, compiled programming language that aims to combine:
- **Simplicity of Python**: Readable syntax, type inference, f-strings, tab indentation
- **Performance & Safety of Rust/Go**: Memory safety without GC, ownership model, native compilation
- **Developer Ergonomics**: No steep learning curve, familiar async/await, built-in tooling

### Design Philosophy

1. **Ownership Lite**: Simplified memory management without manual lifetimes
2. **Python-Inspired Syntax**: Colons and indentation instead of braces
3. **Compile-Time Safety**: Static typing with inference, exhaustive pattern matching
4. **No Garbage Collection**: Predictable performance through ownership
5. **Async-First Concurrency**: Python-familiar async/await for I/O-bound work

### Project Goals

**Current Focus**: Build a proof-of-concept compiler demonstrating core language features
**Long-Term Vision**: Production-ready language for web backends, CLI tools, and systems programming

---

## Language Inspirations

Ryo draws inspiration from several modern programming languages, taking the best ideas from each:

### 🐍 Python - Syntax & Developer Experience
- **Clean, readable syntax** with colons and indentation
- **Type inference** for reduced boilerplate
- **F-strings** for intuitive string formatting
- **Familiar async/await** syntax for asynchronous programming
- **Tab-based indentation** (enforced at compile-time)

**Rationale**: Python's syntax makes code accessible to developers of all skill levels. By adopting its readability, Ryo lowers the barrier to entry while maintaining compile-time safety.

### 🦀 Rust - Ownership & Type Safety
- **Ownership model** for memory safety without garbage collection
- **Algebraic data types** (enums with associated data)
- **Pattern matching** with exhaustive checks
- **Trait system** for polymorphism
- **Result/Option types** for explicit error handling

**Rationale**: Rust's ownership system eliminates entire classes of bugs (use-after-free, data races) while maintaining performance. Ryo simplifies this concept with "Ownership Lite" - removing lifetime annotations while keeping the core safety guarantees.

### 🔥 Mojo - Ownership Simplified
- **Ownership without lifetimes** - simpler mental model
- **Value semantics** with clear ownership transfer
- **Progressive complexity** - start simple, add complexity when needed

**Rationale**: Mojo demonstrates that ownership doesn't require Rust's complexity. Ryo follows this philosophy, making memory safety accessible to Python developers.

### 🔷 Go - Simplicity & Concurrency
- **Simplicity as a core value** - fewer language features, done well
- **Fast compilation** times for rapid development
- **Built-in concurrency primitives** (future: CSP-style channels)
- **Single, standard toolchain** (no build configuration hell)
- **Pragmatic approach** to language design

**Rationale**: Go proves that simplicity and performance aren't mutually exclusive. Ryo adopts this pragmatic philosophy while adding modern type safety.

### ⚡ Zig - Simplicity, Error Handling & Compile-Time
- **No hidden control flow** - explicit is better than implicit
- **Comptime execution** for zero-cost abstractions without macros
- **Simple error handling** with explicit error sets
- **No operator overloading** - predictable code behavior
- **Minimal runtime** requirements

**Rationale**: Zig's `comptime` provides powerful metaprogramming without complex macro systems. Ryo adopts this for configuration, code generation, and type introspection.

### Synthesis: The Best of All Worlds

Ryo isn't just a collection of features - it's a carefully designed synthesis:

```ryo
# Python-like syntax
fn calculate_total(items: List[Item]) -> Result[float, Error]:
    # Rust/Mojo-like ownership (simplified)
    total = 0.0
    for item in items:
        # Zig-like explicit error handling
        price = item.price()?
        total += price

    # Go-like simplicity, Python-like f-strings
    print(f"Total: ${total:.2f}")
    return Ok(total)

# Comptime from Zig
comptime fn generate_validators[T]():
    # Type introspection at compile time
    return build_validators_for[T]()
```

**Key Differentiators**:
- **Easier than Rust**: No lifetime annotations, simpler borrow checker
- **Safer than Python**: Compile-time memory safety, no null/undefined
- **More expressive than Go**: Generics, algebraic types, pattern matching
- **More familiar than Zig**: Python-like syntax instead of C-like
- **Performance**: No GC, compiles to native code, predictable memory usage

---

## Current Implementation Status

### ✅ What's Actually Implemented

The current codebase is a **working arithmetic expression compiler**, not a full language yet.

#### 1. Lexer (`src/lexer.rs`)
- **Library**: Logos
- **Tokens**: Integers, operators (`+`, `-`, `*`, `/`), parentheses
- **Status**: ✅ Fully functional for arithmetic expressions
- **Missing**: Keywords, identifiers, strings, more complex tokens

```rust
#[derive(Logos, Debug, PartialEq, Eq, Hash, Clone)]
pub enum Token<'a> {
    Int(&'a str),
    Add, Sub, Mul, Div,
    LParen, RParen,
    // ...
}
```

#### 2. Parser (`src/parser.rs`)
- **Library**: Chumsky (parser combinator)
- **Features**:
  - Recursive descent parsing
  - Operator precedence: Unary → Mul/Div → Add/Sub
  - Parenthesized expressions
- **Status**: ✅ Well-tested (14 unit tests)
- **Missing**: Full language grammar

#### 3. AST (`src/ast.rs`)
- **Nodes**: `Int`, `Neg`, `Add`, `Sub`, `Mul`, `Div`
- **Features**: Pretty-printing for debugging
- **Status**: ✅ Complete for arithmetic
- **Missing**: Variables, functions, types, statements

#### 4. Code Generator (`src/codegen.rs`)
- **Backend**: Cranelift JIT compiler
- **Output**: Native object files (`.o` or `.obj`)
- **Features**:
  - Cross-platform (uses `target_lexicon::Triple`)
  - Position-independent code (PIC)
  - Generates `main` function returning integer
- **Status**: ✅ Working for arithmetic expressions
- **Missing**: Full language features (variables, control flow, etc.)

#### 5. CLI (`src/main.rs`)
- **Library**: Clap
- **Commands**:
  - `ryo lex <file>`: Display token stream
  - `ryo run <file>`: Compile and execute
- **Features**:
  - Error reporting with Ariadne
  - Linker fallback chain: `zig cc` → `clang` → `cc`
  - Cross-platform support (Windows/Unix)
- **Status**: ✅ Functional for basic operations

#### 6. Evaluator (`src/evaluator.rs`)
- **Status**: ⚠️ Implemented but **unused** in current pipeline
- **Purpose**: AST interpretation (for REPL/testing)

### ❌ What's NOT Implemented (Yet)

All features described in `README.md` and `docs/specification.md` are **design goals**:

- ❌ Variables (immutable by default, `mut` for mutable)
- ❌ Functions (beyond single `main`)
- ❌ Type system (only integers exist)
- ❌ Control flow (`if`, `for`, `match`)
- ❌ Structs, enums, traits
- ❌ Memory management (ownership/borrowing)
- ❌ Standard library
- ❌ Async/await runtime
- ❌ Pattern matching
- ❌ Error handling (`Result`, `Optional`)

### Example of Current Capabilities

**Input file** (arithmetic expression):
```
2 + 3 * 4
```

**Compilation pipeline**:
```
Source → Lexer → Parser → Codegen → Object File → Linker → Executable
```

**Output**:
```
[Result] => 14
```

---

## Architecture & Code Organization

### File Structure

```
ryox/
├── src/
│   ├── main.rs           # CLI entry point, compilation pipeline
│   ├── lexer.rs          # Tokenization (Logos)
│   ├── parser.rs         # Parsing (Chumsky)
│   ├── ast.rs            # Abstract Syntax Tree
│   ├── codegen.rs        # Code generation (Cranelift)
│   └── evaluator.rs      # AST interpreter (unused)
├── docs/
│   ├── specification.md  # Language specification
│   ├── proposals.md      # Future feature proposals
│   ├── design_issues.md  # Known design problems
│   ├── getting_started.md
│   ├── examples/         # Example .ryo files (design examples)
│   └── dev/              # Development documentation
├── examples/             # Test .ryo files
├── tests/
│   └── integration_tests.rs
├── Cargo.toml
├── README.md
├── TODO.md
└── CLAUDE.md            # This file
```

### Key Dependencies

```toml
[dependencies]
ariadne = "0.5"          # Error reporting
chumsky = "0.11"         # Parser combinators
clap = "4.0"             # CLI argument parsing
cranelift = "0.124.0"    # Code generation backend
cranelift-jit = "0.124.0"
cranelift-module = "0.124.0"
cranelift-object = "0.124.0"
logos = "0.15"           # Lexical analysis
target-lexicon = "0.13"  # Target platform info
```

### Compilation Pipeline (src/main.rs)

```rust
fn run_file(file: &Path) -> Result<(), CompilerError> {
    let input = read_source_file(file)?;           // 1. Read source
    let expr = parse_source(&input)?;              // 2. Lex + Parse
    display_input_and_ast(&input, &expr);          // 3. Debug output

    let obj_bytes = compile_to_object(&expr)?;     // 4. Codegen
    let (obj_filename, exe_filename) =
        get_output_filenames(file);                // 5. Generate names
    write_object_file(obj_bytes, &obj_filename)?;  // 6. Write .o file
    link_executable(&obj_filename, &exe_filename)?;// 7. Link
    let result = execute_program(&exe_filename)?;  // 8. Run

    display_result(result);                        // 9. Show result
    Ok(())
}
```

### Error Handling Strategy

```rust
enum CompilerError {
    IoError(std::io::Error),
    ParseError(String),
    CodegenError(String),
    LinkError(String),
    ExecutionError(String),
}

impl From<std::io::Error> for CompilerError {
    fn from(error: std::io::Error) -> Self {
        CompilerError::IoError(error)
    }
}
```

**Pattern**: Use custom error types with `From` implementations for seamless conversion.

---

## Syntax Conventions

### ⚠️ CRITICAL: Python-Style Syntax is MANDATORY

**As of 2025-11-05**, all Ryo code examples **must** use Python-style colons and indentation, **NOT** curly braces.

### ✅ Correct Ryo Syntax

#### Variables
```ryo
# Immutable by default (no 'let' keyword)
pi = 3.14                    # Immutable float (type inferred)
name = "Alice"               # Immutable string
count: int = 42              # Immutable with explicit type

# Mutable variables use 'mut'
mut counter = 0              # Mutable integer (type inferred)
mut temperature: float = 98.6 # Mutable with explicit type
counter += 1                 # Can modify mutable variables

# Type inference (bidirectional type checking)
x = 5                        # Inferred as int
y = x + 10                   # Inferred as int
result = x * 2.5             # Type error: cannot multiply int and float (localized error)
```

#### Functions
```ryo
fn greet(name: &str) -> str:
    return f"Hello, {name}!"

fn main():
    message = greet("World")
    print(message)
```

#### Control Flow
```ryo
# If statements
if x > 0:
    print("positive")
elif x < 0:
    print("negative")
else:
    print("zero")

# For loops
for i in range(10):
    print(i)

# While loops
while condition:
    do_something()

# Match expressions
match value:
    Optional.Some(x): print(x)
    Optional.None: print("none")
```

#### Async Functions
```ryo
async fn fetch_data() -> Result[Data, Error]:
    response = await http.get("https://api.example.com")
    data = await response.json()
    return Ok(data)

async fn main():
    result = await fetch_data()
    print(result)
```

#### Structs and Enums
```ryo
# Definition uses colon
struct Point:
    x: float
    y: float

enum Result[T, E]:
    Ok(T)
    Err(E)

# Literals use braces (correct!)
point = Point{x: 3.14, y: 2.71}
result = Result.Ok(42)
```

#### Traits and Implementations
```ryo
trait Drawable:
    fn draw(self)
    fn area(self) -> float

impl Drawable for Circle:
    fn draw(self):
        print(f"Drawing circle")

    fn area(self) -> float:
        return 3.14159 * self.radius * self.radius
```

### What Uses Braces (Exceptions)

1. **F-string interpolation** (correct, keep as-is):
   ```ryo
   name = "Alice"
   print(f"Hello, {name}!")  # Braces for interpolation
   ```

2. **Struct/enum literals** (correct, keep as-is):
   ```ryo
   point = Point{x: 1.0, y: 2.0}
   user = User{name: "Bob", age: 30}
   ```

3. **Pattern destructuring** (correct, keep as-is):
   ```ryo
   match shape:
       Shape.Circle{radius}: print(radius)
       Shape.Rectangle{width, height}: print(width, height)
   ```

4. **Import grouping** (correct, keep as-is):
   ```ryo
   import utils.{math, strings, collections}
   ```

### Common Documentation Pitfalls

When writing or updating documentation:

1. ❌ **NEVER** write: `fn name() { ... }`
2. ✅ **ALWAYS** write: `fn name():`
3. ❌ **NEVER** write: `for x in y { ... }`
4. ✅ **ALWAYS** write: `for x in y:`
5. ⚠️ **EXCEPTION**: When documenting Rust/Go/C code for comparison, use their native syntax

### Indentation Rules

- **Use TABS** (not spaces) for indentation
- One tab = one indentation level
- Mixing tabs and spaces is a **compile-time error**
- Comments follow the same indentation as the code they document

```ryo
fn example():
	# This is indented with one tab
	if condition:
		# This is indented with two tabs
		do_something()
```

---

## Development Workflow

### Building the Project

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run tests
cargo test

# Run with example
cargo run -- run examples/simple.txt
```

### Testing the Compiler

```bash
# Tokenize a file
cargo run -- lex examples/expr.txt

# Compile and run
cargo run -- run examples/expr.txt

# Expected output for "2 + 3 * 4":
# [Input Expression]
# 2 + 3 * 4
#
# [AST]
# └── Add
#     ├── Int(2)
#     └── Mul
#         ├── Int(3)
#         └── Int(4)
#
# [Codegen]
# ...
# [Result] => 14
```

### Inspecting Generated Code

#### Linux (GNU binutils)
```bash
objdump -d output.o        # Disassemble
objdump -h output.o        # Section headers
objdump -x output.o        # All headers
nm output.o                # Symbol table
```

#### macOS
```bash
otool -tV output.o         # Disassemble
otool -h output.o          # Headers
otool -l output.o          # Load commands
nm output.o                # Symbol table
```

#### Cross-platform
```bash
xxd output.o | less        # Hex dump
hexdump -C output.o | less # Canonical hex+ASCII dump
```

### Git Workflow

**Branch naming convention**: `claude/<description>-<session-id>`

Example:
```bash
git checkout -b claude/feature-parser-improvements-abc123xyz
git add .
git commit -m "Add: Description of changes"
git push -u origin claude/feature-parser-improvements-abc123xyz
```

**Commit message format**:
```
<Type>: <Short description>

<Detailed explanation of what changed and why>

Changes:
- Bullet point of change 1
- Bullet point of change 2

Fixes: #issue-number (if applicable)
```

**Types**: `Add`, `Fix`, `Update`, `Refactor`, `Docs`, `Test`, `Chore`

---

## Key Design Decisions

### 1. Memory Management: Ownership Lite

**Decision**: Simplified ownership model without explicit lifetimes
- Values are owned by one binding
- Moves by default on assignment
- Borrowing for function parameters
- RAII with `Drop` trait

**Rationale**: Balance between Rust's safety and Python's simplicity

**Status**: ⚠️ Design phase, not implemented

### 2. No Garbage Collection

**Decision**: Deterministic memory management via ownership
- Predictable performance (no GC pauses)
- Lower memory overhead
- Simpler runtime

**Rationale**: Target systems programming and performance-critical applications

### 3. Python-Style Syntax

**Decision**: Use colons and indentation instead of braces
- `fn name():` instead of `fn name() {}`
- Tab-based indentation (strict)

**Rationale**:
- Familiar to Python developers (target audience)
- Reduces visual noise
- Enforces consistent formatting

**Implementation**: All documentation updated 2025-11-05

### 4. Async/Await First

**Decision**: Built-in async runtime, Python-familiar syntax
- `async fn name():`
- `await expression`

**Rationale**:
- Most backend/network code is I/O-bound
- Python developers know async/await
- Better ergonomics than callbacks

**Future**: Optional CSP channels for specialized use cases

### 5. Static Dispatch by Default

**Decision**: Traits use static dispatch initially
- Monomorphization like Rust
- No runtime overhead

**Future**: Add `dyn Trait` for dynamic dispatch when needed

**Rationale**: Performance first, flexibility later

### 6. Cranelift Backend

**Decision**: Use Cranelift instead of LLVM
- Faster compilation
- Simpler integration
- JIT support for REPL
- Suitable for AOT compilation

**Tradeoffs**:
- Less mature optimizer than LLVM
- Smaller ecosystem
- Good enough for project goals

### 7. Bidirectional Type Checking

**Decision**: Use bidirectional type checking instead of full Hindley-Milner inference
- Function signatures require type annotations
- Local variables inferred from initialization
- Comptime contexts have enhanced inference

**Examples**:
```ryo
# Function signatures need types
fn add(a: int, b: int) -> int:
    result = a + b              # Local variable inferred: int
    return result

# Clear, localized errors
x = 5
y = 3.14
z = x + y  # Error: cannot add int and float
```

**Rationale**:
- **Better error messages**: Localized, understandable errors vs. cryptic HM failures
- **Simpler to implement**: More practical than complete Hindley-Milner
- **Familiar**: Matches Rust, TypeScript, Swift developer expectations
- **Good documentation**: Function signatures serve as API contracts
- **Ergonomic**: Local code stays concise with inference

**Status**: Design phase, not implemented

**Comparison to alternatives**:
- **vs. Hindley-Milner**: Simpler implementation, better errors, but less "magic"
- **vs. No inference**: Much more ergonomic, less boilerplate
- **vs. Python**: Static type safety with similar ergonomics

---

## Common Tasks

### Adding a New Token

1. Add to `src/lexer.rs`:
```rust
#[derive(Logos, Debug, PartialEq, Eq, Hash, Clone)]
pub enum Token<'a> {
    // ... existing tokens
    #[token("new_keyword")]
    NewKeyword,
}
```

2. Update `Display` implementation
3. Add tests to `src/parser.rs`

### Adding a New AST Node

1. Add to `src/ast.rs`:
```rust
pub enum Expr {
    // ... existing variants
    NewNode(Box<Expr>, Box<Expr>),
}
```

2. Update `pretty_print` implementation
3. Add to `src/evaluator.rs` (if using interpreter)
4. Add to `src/codegen.rs` code generation

### Adding Parser Rules

1. In `src/parser.rs`, update the `parser()` function:
```rust
pub fn parser<'a, I>() -> impl Parser<'a, I, Expr, extra::Err<Rich<'a, Token<'a>>>>
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    recursive(|p| {
        // ... add new grammar rules
    })
}
```

2. Add unit tests in the same file

### Updating Documentation

**Critical**: Always use Python-style syntax in `.md` files

```bash
# Check for incorrect syntax
grep -n "fn.*{" docs/*.md

# Should return no results (except in Rust/Go comparison sections)
```

**Files to update**:
- `README.md` - Main project description
- `docs/specification.md` - Language spec
- `docs/proposals.md` - Future features
- `docs/getting_started.md` - Tutorial
- `docs/design_issues.md` - Known issues

### Running Integration Tests

```bash
cargo test --test integration_tests
```

Currently, integration tests are minimal. Need to expand test coverage.

---

## Areas Needing Attention

### High Priority

1. **Parser Expansion** (Next Step)
   - Add keywords: `let`, `mut`, `if`, `fn`, `return`
   - Add identifiers and variable references
   - Add function definitions
   - Add basic statements

2. **Type System Foundation**
   - Add `Type` enum (Int, Float, Bool, Str, etc.)
   - Implement bidirectional type checking
   - Function signatures require type annotations
   - Local variable type inference from initialization
   - Enhanced inference for comptime contexts

3. **Semantic Analysis**
   - Symbol table / scope management
   - Name resolution
   - Type checking
   - Borrow checking (simplified)

4. **Standard Library Skeleton**
   - Core types (`Result`, `Optional`)
   - Basic I/O (`print`, file operations)
   - Collections (`List`, `Map`)
   - String operations

### Medium Priority

5. **REPL Implementation**
   - Use Cranelift JIT mode
   - Interactive evaluation
   - State persistence between commands

6. **Error Recovery**
   - Better parse error messages
   - Error recovery in parser
   - Multi-error reporting

7. **Testing Infrastructure**
   - Comprehensive parser tests
   - Codegen tests
   - End-to-end integration tests
   - Property-based testing

8. **Documentation**
   - Complete language specification
   - Tutorial series
   - API documentation
   - Contributing guide

### Low Priority (Future)

9. **Optimization**
   - Basic optimizations in Cranelift
   - Dead code elimination
   - Constant folding

10. **Tooling**
    - Language server (LSP)
    - Syntax highlighting
    - Package manager
    - Build system

11. **Advanced Features**
    - Async runtime
    - Pattern matching
    - Trait system
    - Generics
    - Macros/metaprogramming

---

## Design Issues to Resolve

See `docs/design_issues.md` for detailed discussion. Summary:

### Critical Issues 🔴

1. **Tuple Syntax Ambiguity**: `(...)` used for types, values, grouping, unit
2. **Implicit Borrow vs Move**: Inconsistent defaults for assignments vs function calls
3. **Keywords vs Types**: `Result`, `Optional` etc. shouldn't be keywords
4. **Generic Syntax Undefined**: How to write `fn foo[T](...)`?
5. **Method Self Parameter**: `mut self` ambiguous (borrow or move?)
6. **Error Trait System**: How does `?` work across different error types?

### Moderate Issues ⚠️

7. **Async Main**: Should `async fn main()` be allowed?
8. **Channel Operators**: `<-` listed but CSP not implemented
9. **Static Dispatch Only**: Need dynamic dispatch plan
10. **Array vs Slice**: Syntax for fixed vs dynamic arrays unclear

**Action**: Review and resolve before implementing respective features

---

## Documentation Conventions

### Code Examples in Markdown

Always use triple-backtick fenced code blocks with language tag:

```markdown
### Example Section

\`\`\`ryo
fn example():
    print("Hello, Ryo!")
\`\`\`

\`\`\`bash
# Shell commands
cargo run -- run example.ryo
\`\`\`

\`\`\`rust
// Rust comparison (braces OK here)
fn example() {
    println!("Hello, Rust!");
}
\`\`\`
```

### Documentation Comments

In source code (future):
```ryo
#: This is a documentation comment
#: It can span multiple lines
fn documented_function():
    pass

struct Point:
    x: int  #: X coordinate
    y: int  #: Y coordinate
```

### Section Headers

```markdown
# Top-Level (Document Title)
## Major Section
### Subsection
#### Minor Subsection
```

---

## Helpful Resources

### Referenced in TODO.md

**Language Design**:
- [Mojo Manual - Ownership](https://docs.modular.com/mojo/manual/values/ownership)
- [Type Inference Course](https://course.ccs.neu.edu/cs4410sp19/lec_type-inference_notes.html)
- [Rustc Dev Guide - Type Inference](https://rustc-dev-guide.rust-lang.org/type-inference.html)

**Implementation Guides**:
- [Create Your Own Language with Rust](https://github.com/ehsanmok/create-your-own-lang-with-rust/tree/master)
- [createlang.rs](https://createlang.rs)
- [Rust by Example](https://doc.rust-lang.org/rust-by-example)

**Compiler Infrastructure**:
- [MLIR Documentation](https://mlir.llvm.org/docs/Dialects/)
- [Compiler Explorer](https://github.com/compiler-explorer/compiler-explorer)

**Async Runtimes**:
- [Smol - Small async runtime](https://github.com/smol-rs/smol)
- [Pollster - Minimal executor](https://github.com/zesterer/pollster)
- [Zig's async I/O discussion](https://kristoff.it/blog/zig-new-async-io/)

**Similar Projects**:
- [Tao Language](https://github.com/zesterer/tao)
- [Rhai - Embedded scripting](https://github.com/rhaiscript/rhai)
- [Buzz Language](https://buzz-lang.dev/guide/)
- [edlang](https://github.com/edg-l/edlang)

---

## Testing Strategy

### Current Test Coverage

- ✅ Parser: 14 unit tests in `src/parser.rs`
- ⚠️ Integration: Minimal tests in `tests/integration_tests.rs`
- ❌ Lexer: No dedicated tests (covered by parser tests)
- ❌ Codegen: No tests
- ❌ End-to-end: No comprehensive test suite

### Test File Locations

```
tests/
└── integration_tests.rs    # Integration tests

src/
├── parser.rs               # Parser unit tests (mod tests)
└── ...                     # Other unit tests inline
```

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

## Contribution Guidelines

### Before Implementing Features

1. ✅ Check `docs/design_issues.md` for known problems
2. ✅ Review `docs/specification.md` for design decisions
3. ✅ Check `TODO.md` for planned work
4. ✅ Discuss on GitHub issues if major change

### Code Style

- **Rust**: Follow `rustfmt` (run `cargo fmt`)
- **Ryo examples**: Python-style syntax (colons, tabs)
- **Comments**: Explain *why*, not *what*
- **Tests**: Add tests for new features

### Pull Request Checklist

- [ ] Code follows Rust style guidelines (`cargo fmt`)
- [ ] All tests pass (`cargo test`)
- [ ] New features have tests
- [ ] Documentation updated if needed
- [ ] Ryo examples use Python-style syntax (colons, no braces)
- [ ] Commit messages are descriptive
- [ ] No breaking changes without discussion

---

## Version History

- **2025-11-05**: Documentation syntax standardization
  - Replaced C-style braces with Python-style colons across all docs
  - Updated README.md, docs/specification.md, docs/proposals.md, docs/design_issues.md
  - Established syntax conventions in this document

---

## Contact & Resources

- **Repository**: https://github.com/ryolang/ryox
- **Issues**: https://github.com/ryolang/ryox/issues
- **Documentation**: `docs/` directory
- **Discord**: (link placeholder)

---

## Quick Reference Card

### Build & Run
```bash
cargo build                       # Build debug
cargo build --release             # Build release
cargo run -- lex <file>          # Tokenize file
cargo run -- run <file>          # Compile and run
cargo test                        # Run tests
```

### File Extensions
- `.ryo` - Ryo source files
- `.o` / `.obj` - Object files (generated)
- Executable - No extension (Unix) or `.exe` (Windows)

### Syntax Quick Reference
```ryo
# Functions
fn name(param: Type) -> RetType:
    return value

# Control flow
if condition:
    do_something()

for item in collection:
    process(item)

match value:
    Pattern1: handle1()
    Pattern2: handle2()

# Structs
struct Name:
    field: Type

point = Name{field: value}  # Literal uses braces

# Traits
trait TraitName:
    fn method(self)

impl TraitName for Type:
    fn method(self):
        implementation()
```

---

**This document is living documentation. Update it as the project evolves.**

Last major update: 2025-11-05 - Documentation syntax standardization
