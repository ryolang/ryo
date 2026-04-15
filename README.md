![Ryo](./docs/assets/ryo.jpg)
# Ryo Programming Language ⚡

[![Build Status](https://img.shields.io/github/actions/workflow/status/ryolang/ryox/ci.yml?branch=main&style=for-the-badge)](https://github.com/ryolang/ryox/actions)
[![License](https://img.shields.io/badge/license-MIT-blue?style=for-the-badge)](LICENSE)
[![Discord](https://img.shields.io/discord/your-discord-invite-code?label=discord&logo=discord&style=for-the-badge)](https://discord.gg/your-discord-invite-code)

**Ryo /ˈraɪoʊ/: Productive, Safe, Fast.**

Ryo is a new, statically-typed, compiled programming language designed for developers who love the **simplicity of Python** but need the **performance and memory safety** guarantees of languages like Rust or Go, without the steep learning curve.

Build reliable and efficient **web backends, CLI tools, and scripts** with an approachable syntax, powerful compile-time checks, and a built in concurrency model. Ryo manages memory safely via ownership and borrowing (simplified, no manual lifetimes) **without a garbage collector**, ensuring predictable performance and eliminating entire classes of bugs.

**This project contains the source code for the Ryo compiler, standard library, and tooling.**


> [!WARNING]
> Ryo is currently in the **early stages of development** (pre-alpha). The language design is stabilizing, but the compiler is under active construction. It is **not yet ready for production use**. We welcome contributors!

## Current Implementation Status (Milestone 3)

**✅ What's Working Now:**

The Ryo compiler currently implements **Milestone 3: AOT Compilation**

**See the full roadmap:** [Implementation Roadmap](docs/implementation_roadmap.md)

**Try it now:** [Quick Start Guide](docs/quickstart.md) - Build and run your first Ryo program in 5 minutes!

## Why Ryo?

*   **🐍 Simple & Productive:** Write clear, readable code with a clean syntax inspired by Python (tabs, f-strings, tuples, built-in `print`). Reduce boilerplate and focus on your logic.
*   **🛡️ Safe & Reliable:** Compile-time memory safety via "Ownership Lite" prevents dangling pointers, data races, and use-after-free errors without a GC. Explicit error handling with `error` types, `try`, and `catch` makes code robust. No null thanks to optional types (`?T` and `none`).
*   **🚀 Fast & Efficient:** Compiled to native code using **Cranelift** for excellent performance. No garbage collection pauses mean predictable speed. Built it concurrency primitives for scalable applications with excellent Python developer ergonomics.
*   **🧩 Modern Tooling:** Integrated package manager (`ryo`), fast compiler, interactive REPL (JIT), built-in testing framework.
*   **✨ Compile-Time Power (`comptime`):** Run code at compile time for metaprogramming, configuration loading, and pre-computation without runtime cost or complex macros.
*   **🧩 Pragmatic Interoperability:** Designed with C FFI for leveraging existing native code. (Future goals include exploring closer integration with ecosystems like Python).

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
*   **⚡ Zig** - Comptime execution for zero-cost abstractions, explicit error handling, no hidden control flow

**The Result:** A language that's **easier than Rust** (no lifetimes), **safer than Python** (compile-time memory safety), **more expressive than Go** (generics, ADTs), and **more familiar than Zig** (Python-like syntax).

## Designed for the AI Era

As of 2026, most application code is written by AI agents and reviewed by humans. Ryo's design acknowledges this shift:

*   **Strict over convenient** - Verbose safety patterns (`task.spawn_detached` over `task.spawn`) cost the AI nothing but help the human reviewer instantly understand intent.
*   **Compiler strictness over runtime flexibility** - Strict compile-time enforcement catches errors before code reaches production. Warnings on unused `future[T]`, implicit move enforcement for task closures, and forbidden global mutable state.
*   **Predictable patterns over clever shortcuts** - A small set of orthogonal primitives with consistent behavior serves both AI writers and human readers.
*   **Explicit is readable** - `shared[mutex[map[str, int]]]` tells the reviewer at a glance: "shared, locked, concurrent state." No hidden magic.

Python-style syntax, clean error messages, and readable stack traces serve the human side. The AI handles the ceremony; the human benefits from the clarity.

## Quick Example

> **Note:** This example shows Ryo's planned features. Most are not yet implemented.

```ryo
# src/main.ryo - Design example showing future features

fn greet(name: &str) -> str:
    return f"Hello, {name}! Welcome to Ryo."

fn main():
    # Variables are immutable by default (no 'let' keyword)
    message = greet("World")  # Type inferred: str
    print(message)

    # Mutable variables use 'mut'
    mut counter = 0  # Type inferred: int
    counter += 1

    # Safe collections
    numbers = [1, 2, 3, 4, 5]
    print(f"Numbers: {numbers}")

    # Memory safe - optional types prevent null pointer exceptions!
    user: ?str = "Alice"

    # Safe optional chaining
    message = user?.len() orelse 0
    print(f"User message length: {message}")

    # Safe error handling
    print(process_user(user))
```


```ryo
# src/process/errors.ryo
error InvalidUser
```

```ryo
# src/main.ryo
import process

fn process_user(user: ?str) -> process.InvalidUser!str:
    name = user orelse return process.InvalidUser
    return f"Processing user: {name}"
```

```bash
# Compile and run
cargo run -- run arithmetic.ryo
# Output: [Result] => 0

# Or see the compilation stages
cargo run -- lex arithmetic.ryo    # View tokens
cargo run -- parse arithmetic.ryo  # View AST
cargo run -- ir arithmetic.ryo     # View IR info
```

**More working examples:** See `examples/milestone3/` directory for additional programs you can compile and run today!

## Getting Started & Installation

### Prerequisites

Before building Ryo, you need:

1. **Rust toolchain** (1.91): [Install Rust](https://rustup.rs/)
2. **A C linker** (one of the following):
   - **Zig** (recommended): [Download Zig](https://ziglang.org/download/)
   - **Clang**: Included with Xcode Command Line Tools (macOS) or build-essential (Linux)
   - **GCC/cc**: System default compiler

### Building from Source

```bash
# Clone the repository
git clone https://github.com/ryolang/ryox.git
cd ryox

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
# Create a file called first.ryo
echo "x = 42" > first.ryo

# Compile and run
cargo run -- run first.ryo
```

You should see output like:
```
[Input Source]
x = 42

[AST]
Program (0..6)
└── Statement [VarDecl] (0..6)
    VarDecl
      ├── name: x (0..1)
      └── initializer:
          Literal(Int(42)) (4..6)

[Codegen]
Generated object file: first.o
Linked with zig cc: first
[Result] => 0
```

**Next Steps:**

- **[Quick Start Guide](docs/quickstart.md)** - Complete hands-on tutorial (5 minutes)
- **[Getting Started](docs/getting_started.md)** - Language introduction and concepts
- **[Troubleshooting](docs/troubleshooting.md)** - Common issues and solutions
- **[Examples](examples/milestone3/)** - Working code examples you can run today

**Note:** Advanced features like project creation (`ryo new`), package management, and REPL are planned for future milestones.

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
- **[Compilation Pipeline](docs/dev/compilation_pipeline.md)** - How the compiler works (Milestone 3)
- **[CLAUDE.md](CLAUDE.md)** - Project context for AI assistants and contributors

### Examples
- **[Milestone 3 Examples](examples/milestone3/)** - Working code you can compile and run today

*More documentation will be added as the project progresses. See the [docs/](docs/) directory for all available documentation.*

## License

Ryo is distributed under the terms of both the MIT license. See [LICENSE](LICENSE) for details.
