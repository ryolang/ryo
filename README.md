![Ryo](./docs/assets/ryo.jpg)
# Ryo Programming Language ⚡

[![Build Status](https://img.shields.io/github/actions/workflow/status/ryolang/ryox/ci.yml?branch=main&style=for-the-badge)](https://github.com/ryolang/ryox/actions)
[![License](https://img.shields.io/badge/license-MIT-blue?style=for-the-badge)](LICENSE)
[![Discord](https://img.shields.io/discord/your-discord-invite-code?label=discord&logo=discord&style=for-the-badge)](https://discord.gg/your-discord-invite-code)

**Ryo /ˈraɪoʊ/: Productive, Safe, Fast.**

Ryo is a new, statically-typed, compiled programming language designed for developers who love the **simplicity of Python** but need the **performance and memory safety** guarantees of languages like Rust or Go, without the steep learning curve.

Build reliable and efficient **web backends, CLI tools, and scripts** with an approachable syntax, powerful compile-time checks, and a familiar async/await concurrency model. Ryo manages memory safely via ownership and borrowing (simplified, no manual lifetimes) **without a garbage collector**, ensuring predictable performance and eliminating entire classes of bugs.

**This project contains the source code for the Ryo compiler, standard library, and tooling.**


> [!WARNING]
> Ryo is currently in the **early stages of development** (pre-alpha). The language design is stabilizing, but the compiler is under active construction. It is **not yet ready for production use**. We welcome contributors!

## Why Ryo?

*   **🐍 Simple & Productive:** Write clear, readable code with a clean syntax inspired by Python (tabs, f-strings, tuples, built-in `print`). Reduce boilerplate and focus on your logic.
*   **🛡️ Safe & Reliable:** Compile-time memory safety via "Ownership Lite" prevents dangling pointers, data races, and use-after-free errors without a GC. Explicit error handling with `error` types, `try`, and `catch` makes code robust. No null thanks to optional types (`?T` and `none`).
*   **🚀 Fast & Efficient:** Compiled to native code using **Cranelift** for excellent performance. No garbage collection pauses mean predictable speed. Familiar async/await concurrency for scalable applications with excellent Python developer ergonomics.
*   **🧩 Modern Tooling:** Integrated package manager (`ryo`), fast compiler, interactive REPL (JIT), built-in testing framework.
*   **✨ Compile-Time Power (`comptime`):** Run code at compile time for metaprogramming, configuration loading, and pre-computation without runtime cost or complex macros.
*   **🧩 Pragmatic Interoperability:** Designed with C FFI for leveraging existing native code. (Future goals include exploring closer integration with ecosystems like Python).

## Features Overview

*   **Memory Management:** Ownership & Borrowing (Simplified, "Ownership Lite"), No GC, RAII (`Drop`), Immutable-by-Default variables for safer code.
*   **Concurrency:** Python-familiar async/await with high-performance async runtime
*   **Syntax:** Python-inspired, tab-indented, expressions, statements
*   **Types:** Static typing with bidirectional type inference (like Rust/TypeScript), primitives (`int`, `float`, `bool`, `str`, `char`), tuples, `struct`, `enum` (ADTs), `trait` (static dispatch initially). Function signatures require types, local variables inferred. Variables are immutable by default (no `let` keyword), use `mut` for mutability
*   **Error Handling:** Type-safe, explicit error handling system:
    - **Single-variant errors** for simple cases: `error Timeout`, `error NotFound(str)`
    - **Multi-variant errors** with full ADT support: `error MathError: DivisionByZero, InvalidInput(str)`
    - **Error unions** with automatic composition: `fn process() -> !Data` infers `(FileError | ParseError | MathError)!Data`
    - **Error trait** with automatic message generation: All errors implement `.message() -> str`
    - **Try/catch operators** for ergonomic error propagation and handling
    - **Optional types** (`?T` and `none`) for nullable values - no null pointer exceptions
    - **Exhaustive pattern matching** for single errors, flexible non-exhaustive matching for error unions
*   **Compile-Time Execution:** `comptime` blocks and functions
*   **Tooling:** `ryo` command (compiler, runner, REPL, package manager frontend), `ryopkg` logic integrated, Cranelift backend (AOT/JIT/Wasm)
*   **FFI:** C interoperability via `extern "C"` and `unsafe` blocks, `ffi` stdlib module. (Future Goal: Explore deeper integration with other language ecosystems like Python).
*   **Standard Library:** Modular packages (`io`, `str`, `collections`, `http`, `json`, `os`, etc.)
*   **Future Concurrency Extensions:** CSP-style channels (`chan`, `select`) planned as optional additions for specialized use cases like actor systems and data pipelines

For full details, see the [Language Specification](docs/specification.md) (Link to spec file).

## Language Inspirations

Ryo draws inspiration from the best features of modern programming languages:

*   **🐍 Python** - Clean syntax with colons and indentation, f-strings, type inference, async/await
*   **🦀 Rust** - Ownership model for memory safety, algebraic data types (enums), pattern matching, trait system
*   **🔥 Mojo** - Simplified ownership without lifetimes, value semantics, progressive complexity
*   **🔷 Go** - Simplicity as a core value, fast compilation, pragmatic concurrency (CSP channels planned)
*   **⚡ Zig** - Comptime execution for zero-cost abstractions, explicit error handling, no hidden control flow

**The Result:** A language that's **easier than Rust** (no lifetimes), **safer than Python** (compile-time memory safety), **more expressive than Go** (generics, ADTs), and **more familiar than Zig** (Python-like syntax).

## Quick Example

```ryo
# src/main.ryo

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

error ProcessError:
    InvalidUser

fn process_user(user: ?str) -> ProcessError!str:
    name = user orelse return ProcessError.InvalidUser
    return f"Processing user: {name}"
```

Run with: `ryo run` (after building the `ryo` tool)

## Getting Started & Installation

Ryo is under heavy development. Currently, installation requires building from source.

**(Instructions for building from source will go here)**

```bash
# Example build steps (replace with actual steps)
git clone https://github.com/ryolang/ryo.git
cd ryo
cargo build --release
# Add target/release to your PATH
export PATH="$(pwd)/target/release:$PATH"

# Verify
ryo --version
```

Once installed, you can create and run projects:

```bash
# Create a new project
ryo new my_app
cd my_app

# Edit src/main.ryo
# ...

# Build and run
ryo run
```

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

*   [Language Specification](docs/specification.md)
*   *(Link to Tutorial)*
*   *(Link to Standard Library Docs)*
*   *(Link to Guides - Memory, Concurrency, etc.)*

*(More documentation will be added as the project progresses)*

## License

Ryo is distributed under the terms of both the MIT license. See [LICENSE](LICENSE) for details.
