# Ryo Implementation Roadmap

This roadmap outlines the planned development of the Ryo programming language compiler and runtime. Each milestone focuses on delivering specific, tangible capabilities while building toward a complete language implementation.

**Development Timeline:** Each milestone is designed for approximately 2-4 weeks of development (assuming ~8 hours/week), but timelines should remain flexible to ensure quality over speed.

## Current Status

| Milestone | Status | Notes |
|-----------|--------|-------|
| Milestone 1: Lexer Basics | ✅ **COMPLETE** | All tokens implemented and tested. `ryo lex` command fully functional. |
| Milestone 2: Parser & AST | 🔄 In Progress | Partial implementation exists (arithmetic expressions work). Full expansion needed for variable declarations and function definitions. |
| Milestone 3: Cranelift Integration | ✅ **COMPLETE** | Working code generation to native code via Cranelift. `ryo run` command functional. |
| Milestones 4+ | ⏳ Planned | Future features in design phase. |

## Guiding Principles

* **Iterate:** Get something working end-to-end quickly, then refine
* **Test Early, Test Often:** Integrate basic testing from the start
* **Focus:** Each milestone adds a specific, visible capability
* **Simplicity First:** Implement the simplest version that meets the immediate goal
* **Quality of Life:** Include documentation, basic error reporting, and simple tooling

## Phase 1: Core Foundation

### Milestone 1: Setup & Lexer Basics ✅ COMPLETE

**Goal:** Parse basic Ryo syntax into tokens

**Tasks:**
- ✅ Set up Rust project (`cargo new ryo_compiler`)
- ✅ Add dependencies (`logos`, `chumsky`, `clap`)
- ✅ Define core tokens (`Token` enum in `src/lexer.rs`) using `logos`:
  - ✅ Keywords: `fn`, `if`, `else`, `return`, `mut`, `struct`, `enum`, `match`
  - ✅ Identifiers, integer literals, basic operators (`=`, `+`, `-`, `*`, `/`, `:`)
  - ✅ Punctuation: `(`, `)`, `{`, `}` (braces reserved for f-string interpolation in later milestones)
  - ✅ Handle whitespace/comments (Python-style `#` comments)
- ✅ Write comprehensive tests for the lexer (19 unit tests)
- ✅ Create simple CLI harness (`src/main.rs`) using `clap`

**Visible Progress:** `ryo lex <file.ryo>` prints token stream ✅

**Completion Date:** November 9, 2025
**Implementation Details:**
- All Milestone 1 keywords and operators successfully tokenized
- Comments handled correctly (skipped from token stream)
- Comprehensive test suite covers edge cases (keyword keyword-as-part-of-identifier distinction, comment handling, etc.)
- CLI tested with realistic Ryo code samples
- **Design Decision:** Struct literals use parentheses with named arguments `Point(x=1, y=2)`, not braces. Curly braces are reserved exclusively for f-string interpolation (e.g., `f"Hello {name}"`) which will be implemented in later milestones.

### Milestone 2: Parser & AST Basics
**Goal:** Parse simple variable declarations and integer literals into an Abstract Syntax Tree

**Tasks:**
- Define basic AST nodes in `src/ast.rs`:
  - `struct Program`, `struct Statement`, `enum StmtKind::VarDecl`
  - `struct Expression`, `enum ExprKind::Literal`, `struct Ident`, `struct TypeExpr`
  - Include spans (`chumsky::SimpleSpan`)
- Implement parser using `chumsky` (`src/parser.rs`)
- Parse `ident: type = literal_int` structure
- Integrate parser with lexer output in `main.rs`
- Update CLI: `ryo parse <file.ryo>` prints generated AST
- Write basic parser tests

**Visible Progress:** `ryo parse <file.ryo>` shows structure of simple variable declarations

### Milestone 3: "Hello, Exit Code!" (Cranelift Integration)
**Goal:** Compile minimal Ryo program to native code that returns an exit code

**Tasks:**
- Add `cranelift`, `cranelift-module`, `cranelift-jit` dependencies
- Create basic code generation module (`src/codegen.rs`)
- Implement logic to translate simple `VarDecl` AST into Cranelift IR
- Generate code for main function that loads value and returns it
- Use `cranelift-object` to write object file OR `cranelift-jit` to execute directly
- Update CLI: `ryo run <file.ryo>` compiles and runs code

**Visible Progress:** `ryo run my_program.ryo` executes and exits with specified code (**Major milestone!**)

## Phase 2: Essential Language Features

### Milestone 4: Functions & Basic Calls
**Goal:** Define and call simple functions with integer arguments and return values

**Tasks:**
- Extend AST: `StmtKind::FunctionDef`, `ExprKind::Call`
- Extend Parser: Parse `fn func_name(arg: type) -> type: ... return expr`
- Parse function calls `func_name(arg)`
- Extend Codegen: Generate Cranelift IR for function definitions and calls
- Handle `return` statements
- Write tests for function definition and calls

**Visible Progress:** Can compile and run programs using simple functions

### Milestone 5: Expressions & Operators
**Goal:** Support basic integer arithmetic expressions

**Tasks:**
- Extend AST: `ExprKind::BinaryOp`
- Extend Parser: Handle operator precedence for `+`, `-`, `*`
- Extend Codegen: Generate Cranelift IR for arithmetic operations
- Write tests for expressions

**Visible Progress:** `ryo run` can execute programs like `result: int = (2 + 3) * 5`

### Milestone 6: Control Flow & Booleans
**Goal:** Implement `if/else` statements and boolean logic

**Tasks:**
- Extend Lexer/Parser/AST:
  - `Token::KwIf/Else`, `Token::Bool`
  - Comparison operators (`==`, `!=`, `<`, etc.)
  - Logical `and`/`or`/`not`
  - `StmtKind::IfStmt`, `ExprKind::Literal(Bool)`
- Extend Codegen: Generate Cranelift IR for conditional branching and boolean operations
- Write tests for `if/else` and boolean expressions

**Visible Progress:** Can run programs with basic conditional logic

### Milestone 7: Single-Variant Error Types & Error Reporting
**Goal:** Introduce single-variant `error` keyword for error handling and improve compiler error messages

**Tasks:**
- Define `error` keyword in lexer/parser for single-variant error definitions
- Organize related errors using modules (`module name: error ErrorType`)
- Allow functions to return error types (`ErrorType!SuccessType`) and error unions (`(ErrorA | ErrorB)!SuccessType`)
- Implement automatic error union inference from `try` expressions
- Integrate `ariadne` for syntax/type error reporting using spans
- Improve error messages for parsing and basic type mismatches

**Visible Progress:** Better error messages! Can define and handle single-variant errors with automatic union composition

## Phase 3: Ownership & Memory Safety

### Milestone 8: Basic Ownership & String Type
**Goal:** Implement move semantics for heap-allocated `String` type and `Copy` for `int`

**Tasks:**
- Define simple `String` type (minimal runtime support for allocation)
- Implement basic type checking/semantic analysis pass (`src/checker.rs`)
- Track variable states (valid, moved)
- Implement `Copy` trait concept for primitives
- Issue errors on use-after-move
- Focus on simple variable assignments and function calls

**References:**
- https://www.modular.com/blog/mojo-vs-rust
- https://docs.modular.com/mojo/manual/values/

**Visible Progress:** Compiler prevents basic use-after-move errors

### Milestone 9: Immutable Borrows
**Goal:** Allow passing immutable references (`&String`) to functions

**Tasks:**
- Extend type system/checker: Handle `&T` type
- Implement borrow checking rule: Cannot mutate through immutable borrow
- Extend Codegen: Pass pointers for borrows

**Visible Progress:** Can pass strings by reference safely; compiler prevents mutation via `&`

### Milestone 10: Basic Collections & For Loops
**Goal:** Implement simple `List` and `for i in range(N):` loops

**Tasks:**
- Define basic `List` type (runtime allocation needed)
- Add `append` method
- Extend Parser/AST/Checker/Codegen for `for` loops (simple range only)
- **Decision:** Start with hardcoded `List<int>` before implementing generics

**Visible Progress:** Can write simple loops and use dynamic lists

### Milestone 11: Mutable Borrows & Error Handling Ergonomics
**Goal:** Implement mutable variables, mutable borrows, and `try`/`catch` operators

**Tasks:**
- Extend Parser/AST/Checker: Handle `mut` keyword
- Extend Checker: Implement mutable borrow rules (no aliasable mutable borrows)
- Extend Codegen: Handle loading/storing mutable variables, passing mutable pointers
- Extend Parser/Checker/Codegen: Implement `try` keyword for error propagation and `catch` for error handling

**Visible Progress:** Core ownership model complete! Error handling ergonomics improved with `try`/`catch`

## Phase 4: Concurrency & Interoperability

### Milestone 12: Basic Async/Await
**Goal:** Implement `async fn` and `await` for basic futures with single-threaded executor

**Tasks:**
- Extend Parser/AST/Checker/Codegen: Handle `async`/`await` keywords
- Transform async functions (state machines or using Rust's `async` blocks internally)
- Integrate minimal executor (e.g., `futures::executor::block_on` or `LocalPool`)
- Implement simple `async.sleep()` function

**Visible Progress:** Can define and run simple non-blocking async functions

### Milestone 13: Basic FFI
**Goal:** Allow Ryo to call simple external C/Rust functions

**Tasks:**
- Extend Parser/AST/Checker: Handle `extern "C" fn ...` declarations
- Extend Codegen: Generate calls using C ABI
- Write simple C or Rust library with test function
- Link against external library

**Visible Progress:** Ryo programs can call external native code

### Milestone 14: Core Language Complete
**Goal:** Finalize core language features and prepare for ecosystem development

**Tasks:**
- Comprehensive testing of all core features
- Performance benchmarking and optimization
- Error message improvements and user experience polish
- Documentation completion for core language
- Basic package manager functionality (`ryo pkg` commands)
- Prepare foundation for ecosystem development

**Visible Progress:** Ryo core language is feature-complete and ready for real-world use!

## Beyond Core Language (Post-v1.0)

### Essential Extensions
These features are planned for early post-v1.0 releases as they're critical for practical development:

- **Full Generics System:** Complete generic types with trait bounds (see [proposals.md](proposals.md#advanced-generics))
- **Enhanced Error Handling:** Standard error traits and improved `try`/`catch` ergonomics
- **Standard Library Expansion:** HTTP client, JSON parsing, file system operations
- **Package Registry:** Central package repository and dependency resolution

### Tooling & Developer Experience
Advanced tooling that will significantly improve the development experience:

- **Language Server Protocol (LSP):** IDE integration for autocompletion and diagnostics
- **Built-in Testing Framework:** Integrated test runner and benchmarking tools
- **Enhanced Package Manager:** Workspaces, private registries, and build optimization

### Specialized Features
Advanced features for specific use cases (see [proposals.md](proposals.md) for details):

- **CSP Concurrency Extensions:** Optional channels and `select` for specialized concurrent programming
- **Compile-time Execution (comptime):** Metaprogramming and compile-time computation
- **Foreign Function Interface (FFI):** Comprehensive C interoperability with `unsafe` blocks
- **Performance Optimizations:** Profile-guided optimization and cross-compilation support

## Implementation Notes

### Key Dependencies
- **Rust Toolchain:** Latest stable Rust
- **Parsing:** `logos` for lexing, `chumsky` for parsing
- **Code Generation:** `cranelift` family of crates
- **Error Reporting:** `ariadne` for beautiful error messages
- **CLI:** `clap` for command-line interface
- **Async Runtime:** `tokio` or `futures` for async/await support

### Testing Strategy
- Unit tests for each compiler phase
- Integration tests for end-to-end compilation
- Golden file tests for error messages
- Performance benchmarks for compilation speed
- Memory safety tests for ownership system

### Quality Assurance
- Continuous Integration with multiple platforms
- Code coverage tracking
- Fuzzing for parser robustness
- Memory leak detection
- Security audit for FFI boundaries

## Core Language Goals (v1.0)

The 14 milestones above represent the **minimum viable language** needed for Ryo v1.0. Upon completion, developers will have:

✅ **Memory Safety:** Ownership and borrowing prevent common memory errors
✅ **Async Concurrency:** Native async/await for scalable applications
✅ **Type Safety:** Static typing with inference catches errors at compile time
✅ **Error Handling:** Error types and error unions with `try`/`catch` for robust error management
✅ **Performance:** Compiled native code with predictable memory usage
✅ **Interoperability:** Basic FFI for integrating with existing C libraries
✅ **Developer Experience:** Clear error messages and basic tooling

This foundation enables building real applications including web services, CLI tools, and data processing pipelines while providing a solid base for ecosystem development.

## Development Timeline

**Estimated Total Time:** 56-112 weeks (14-28 months) for core language completion
**Target:** Each milestone 2-4 weeks, with flexibility for quality and iteration
**Approach:** Incremental development with working software at each milestone

This roadmap provides a structured path from initial prototype to a production-ready programming language while maintaining focus on delivering value at each milestone.
