<div align="center">
  <img src="./docs/assets/ryo.svg" alt="Ryo Logo" width="120" />
  <h1>Ryo Programming Language</h1>
  <p><strong>Ryo /ˈraɪoʊ/ — Looks like Python. Runs like Rust. Quacks like Ryduck.</strong></p>
  <p>
    <img src="https://img.shields.io/badge/status-pre--alpha-orange?style=for-the-badge" alt="Status">
    <img src="https://img.shields.io/github/actions/workflow/status/ryolang/ryo/ci.yml?branch=main&style=for-the-badge" alt="Build">
    <img src="https://img.shields.io/badge/license-MIT-blue?style=for-the-badge" alt="License">
  </p>
</div>

<br>

**Ryo** is a statically-typed language designed for high **Developer Experience**. It combines the ergonomics of Python, the memory safety of Rust ("Ownership Lite"), and the concurrency model of Go.

It targets web backends, CLI tools, and scripting/notebook environments via AOT and JIT compilation.

## ⚡ Key Features

*   **Pythonic Syntax:** Indentation-based, readable, and concise.
*   **Ownership Lite:** No Garbage Collector. Memory safety via Mojo-style borrowing (functions borrow by default, assignments move).
*   **Green Threads:** "Colorless" concurrency with a work-stealing scheduler. No `async`/`await` needed.
*   **Rich Errors:** Zig-style Error Unions (`!T`) with payload data.
*   **Fast:** Built on **Cranelift** for instant compile times and native performance.

**This project contains the source code for the Ryo compiler, standard library, and tooling.**

> [!WARNING]
> Ryo is currently in the **early stages of development** (pre-alpha). The language design is stabilizing, but the compiler is under active construction. It is **not yet ready for production use**. We welcome contributors!

## Current Implementation Status (Milestone 8b)

**✅ What's Working Now:**

The Ryo compiler currently implements through **Milestone 8b: Conditionals & Logical Operators**:

- **Types:** `int`, `float`, `bool`, `str` (literals)
- **Operators:** arithmetic (`+`, `-`, `*`, `/`, `%`), comparison (`==`, `!=`, `<`, `>`, `<=`, `>=`), logical (`and`, `or`, `not`), unary negation (`-`)
- **Variables:** immutable by default, `mut` keyword, type annotations, type inference
- **Functions:** definitions with parameters and return types, calls, forward references, recursion
- **Control flow:** `if`/`elif`/`else` with block scoping, short-circuit `and`/`or`
- **Builtins:** `print()` for string literals
- **Compilation:** AOT (native binary) and JIT execution via Cranelift backend
- **Tooling:** `ryo run`, `ryo build`, `ryo lex`, `ryo parse`, `ryo ir --emit=uir|tir|clif`

**See the full roadmap:** [Implementation Roadmap](docs/dev/implementation_roadmap.md)

**Try it now:** [Quick Start Guide](docs/quickstart.md) - Build and run your first Ryo program in 5 minutes!

## Quick Installation (Latest Build)

The easiest way to try Ryo is using our latest build:

```bash
# Download and run the installer
curl -fsSL https://raw.githubusercontent.com/ryolang/ryo/main/install.sh | sh

# Then add to your PATH
export PATH="$HOME/.ryo/bin:$PATH"

# Verify installation
ryo --version

# Update to the latest dev
curl -fsSL https://raw.githubusercontent.com/ryolang/ryo/main/install.sh | sh -s -- --force
```

**Note:** Dev builds are manually triggered and contain the latest features. They may be unstable. You can also build from source (below).


## Features Overview

*   **Memory Management:** Ownership & Borrowing (Simplified, "Ownership Lite"), No GC, RAII (`Drop`), Immutable-by-Default variables for safer code.
*   **Concurrency:** Built in primitives inspired by CSP's GO  model
*   **Syntax:** Python-inspired, tab-indented, expressions, statements
*   **Types:** Static typing with bidirectional type inference (like Rust/TypeScript), primitives (`int`, `float`, `bool`, `str`, `char`), tuples, `struct`, `enum` (ADTs), `trait` (static dispatch initially). Function signatures require types, local variables inferred. Variables are immutable by default (no `let` keyword), use `mut` for mutability
*   **Module System:** Directory-based modules with three access levels: `pub` (public), `package` (package-internal), and module-private (no keyword). Implicit discovery from filesystem structure (no `mod` declarations). See [Module System Design](CLAUDE.md#8-module-system-design) for details.
*   **Error Handling:** Type-safe, explicit error handling system:
    - **Single-variant errors only**: `error Timeout`, `error NotFound(str)`, `error HttpError(status: int, message: str)` - unified syntax for all error definitions
    - **Module-based error grouping**: Organize related errors in modules: `module math: error DivisionByZero`
    - **Error unions** with automatic composition: `fn process() -> !Data` infers `(FileError | ParseError)!Data` from `try` expressions
    - **Error trait** with automatic message generation: All errors implement `.message() -> str`
    - **Try/catch operators** for ergonomic error propagation and handling
    - **Optional types** (`?T` and `none`) for nullable values - no null pointer exceptions
    - **Exhaustive pattern matching by default**: All error unions require handling all error types exhaustively (or explicit `_` catch-all)
*   **Compile-Time Execution:** `comptime` blocks and functions
*   **Tooling:** `ryo` command (compiler, runner, REPL, package manager frontend), `ryopkg` logic integrated, Cranelift backend (AOT/JIT/Wasm)
*   **FFI:** C interoperability via `extern "C"` and `unsafe` blocks, `ffi` stdlib module. (Future Goal: Explore deeper integration with other language ecosystems like Python).
*   **Standard Library:** Modular packages (`io`, `str`, `collections`, `http`, `json`, `os`, etc.)
*   **Future Concurrency Extensions:** CSP-style channels (`chan`, `select`) planned as optional additions for specialized use cases like actor systems and data pipelines

For full details, see the [Language Specification](docs/specification.md) (Link to spec file).

## Language Inspirations

Ryo draws inspiration from the best features of modern programming languages:

*   **🐍 Python** - Clean syntax with colons and indentation, f-strings, type inference
*   **🦀 Rust** - Ownership model for memory safety, algebraic data types (enums), pattern matching, trait system
*   **🔥 Mojo** - Simplified ownership without lifetimes, value semantics, progressive complexity
*   **🔷 Go** - Simplicity as a core value, fast compilation, pragmatic concurrency (CSP channels planned)
*   **⚡ Zig** - Comptime execution for zero-cost abstractions, explicit error handling, no operator overloading, readable-by-default design

**The Result:** A language that's **easier than Rust** (no lifetimes), **safer than Python** (compile-time memory safety), **more expressive than Go** (generics, ADTs), and **more familiar than Zig** (Python-like syntax).

## Designed for the AI Era

As of 2026, most application code is written by AI agents and reviewed by humans. Ryo's design acknowledges this shift:

*   **Strict over convenient** - Verbose safety patterns (`task.spawn_detached` over `task.spawn`) cost the AI nothing but help the human reviewer instantly understand intent.
*   **Compiler strictness over runtime flexibility** - Strict compile-time enforcement catches errors before code reaches production. Warnings on unused `future[T]`, implicit move enforcement for task closures, and forbidden global mutable state.
*   **Predictable patterns over clever shortcuts** - A small set of orthogonal primitives with consistent behavior serves both AI writers and human readers.
*   **Readable by default** - `shared[mutex[map[str, int]]]` tells the reviewer at a glance: "shared, locked, concurrent state." Implicit where ceremony hurts clarity (parameter borrowing, type narrowing after checks), explicit where the reviewer needs to see intent (`try` for errors, `move` for ownership transfer, `shared` for concurrent state).

Python-style syntax, clean error messages, and readable stack traces serve the human side. The AI handles the ceremony; the human benefits from the clarity.

## Quick Example

```ryo
fn main():
	print("Hello, World!\n")
```

A more complete example showing conditionals and functions:

```ryo
fn classify(n: int) -> int:
	if n < 0:
		return -1
	elif n == 0:
		return 0
	else:
		return 1

fn in_range(x: int, lo: int, hi: int) -> bool:
	return x >= lo and x <= hi

fn main():
	c = classify(5)
	if in_range(c, 0, 1):
		print("positive\n")
```

> **Note:** Many features in the language spec (generics, error unions, closures, concurrency) are not yet implemented. See the roadmap for what's planned.

```bash
# Compile and run
cargo run -- run examples/classify.ryo

# Or see the compilation stages
cargo run -- lex examples/hello.ryo        # View tokens
cargo run -- parse examples/hello.ryo      # View AST
cargo run -- ir --emit=uir examples/hello.ryo  # View untyped IR
cargo run -- ir --emit=tir examples/hello.ryo  # View typed IR
cargo run -- ir examples/hello.ryo         # View AST + Cranelift IR
```

**More working examples:** See the `examples/` directory for programs you can compile and run today.

## Getting Started & Installation

### Prerequisites

Before building Ryo, you need:

1. **Rust toolchain** (1.91): [Install Rust](https://rustup.rs/)

> **Note:** Ryo automatically downloads and manages its own Zig toolchain for linking — no manual linker installation required.

### Building from Source

```bash
# Clone the repository
git clone https://github.com/ryolang/ryo.git
cd ryo

# Build the compiler (debug mode for development)
cargo build

# Or build optimized release version
cargo build --release

# Verify the build
cargo run -- --version
```

### Your First Program

Create a simple Ryo program:

```bash
# Create a file called hello.ryo
echo 'print("Hello, Ryo!\n")' > hello.ryo

# Compile and run
cargo run -- run hello.ryo
```

You should see the AST, codegen output, then:
```
Hello, Ryo!
[Result] => 0
```

**Next Steps:**

- **[Quick Start Guide](docs/quickstart.md)** - Complete hands-on tutorial (5 minutes)
- **[Getting Started](docs/getting_started.md)** - Language introduction and concepts
- **[Troubleshooting](docs/troubleshooting.md)** - Common issues and solutions
- **[Examples](examples/)** - Working code examples you can run today

**Note:** Advanced features like project creation (`ryo new`), package management, and REPL are planned for future milestones.

## Binary Inspection

When debugging codegen output, these tools help verify what the compiler produces:

```bash
# Disassembly — see generated machine code
objdump -d output.o          # Linux (GNU binutils)
otool -tV output.o           # macOS

# Symbol table — check exported/imported symbols
nm output.o                  # List symbols (both platforms)
nm -C output.o               # Demangle C++ / Rust symbols

# Raw bytes — inspect object file structure
xxd output.o | head -40      # Hex dump (first 40 lines)

# Full binary after linking
objdump -d my_program        # Disassemble final executable
file my_program              # Verify binary format and architecture
```

**Typical workflow:** Compile with `cargo run -- build my_file.ryo`, then inspect the generated `.o` file or final binary to verify codegen correctness.

## Contributing

We welcome contributions! Ryo is an ambitious project, and we need help with:

*   Compiler development (parsing, semantic analysis, borrow checking, code generation)
*   Standard library implementation
*   Tooling (`ryo` package manager, REPL, testing framework)
*   Documentation writing
*   Language design discussions
*   Writing examples and tutorials

Please read our [Contributing Guide](CONTRIBUTING.md) and check out the [open issues](https://github.com/ryolang/ryo/issues).

## Documentation

### Getting Started
- **[Quick Start Guide](docs/quickstart.md)** - Build and run your first program in 5 minutes
- **[Getting Started](docs/getting_started.md)** - Language introduction with examples
- **[Troubleshooting](docs/troubleshooting.md)** - Solutions to common problems

### Language Reference
- **[Language Specification](docs/specification.md)** - Complete language design and syntax
- **[Proposals](docs/proposals.md)** - Future features and enhancements
- **[Design Issues](docs/design_issues.md)** - Known design challenges and decisions

### Implementation
- **[Implementation Roadmap](docs/dev/implementation_roadmap.md)** - Development milestones and progress
- **[Compilation Pipeline](docs/dev/compilation_pipeline.md)** - How the compiler works (Lexer → Parser → AstGen → UIR → Sema → TIR → Codegen)
- **[CLAUDE.md](CLAUDE.md)** - Project context for AI assistants and contributors

### Examples
- **[Working Examples](examples/)** - Programs you can compile and run today (functions, conditionals, logical operators)

*More documentation will be added as the project progresses. See the [docs/](docs/) directory for all available documentation.*

## License

Ryo is distributed under the terms of both the MIT license. See [LICENSE](LICENSE) for details.
