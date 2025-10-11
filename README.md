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
*   **🛡️ Safe & Reliable:** Compile-time memory safety via "Ownership Lite" prevents dangling pointers, data races, and use-after-free errors without a GC. Explicit error handling with `Result` and `?` makes code robust. No `null` thanks to `Optional`.
*   **🚀 Fast & Efficient:** Compiled to native code using **Cranelift** for excellent performance. No garbage collection pauses mean predictable speed. Familiar async/await concurrency for scalable applications with excellent Python developer ergonomics.
*   **🧩 Modern Tooling:** Integrated package manager (`ryo`), fast compiler, interactive REPL (JIT), built-in testing framework.
*   **✨ Compile-Time Power (`comptime`):** Run code at compile time for metaprogramming, configuration loading, and pre-computation without runtime cost or complex macros.
*   **🧩 Pragmatic Interoperability:** Designed with C FFI for leveraging existing native code. (Future goals include exploring closer integration with ecosystems like Python).

## Features Overview

*   **Memory Management:** Ownership & Borrowing (Simplified, "Ownership Lite"), No GC, RAII (`Drop`), Immutable-by-Default variables for safer code.
*   **Concurrency:** Python-familiar async/await with high-performance async runtime
*   **Syntax:** Python-inspired, tab-indented, expressions, statements
*   **Types:** Static typing, type inference (for `var = val`), primitives (`int`, `float`, `bool`, `str`, `char`), tuples, `struct`, `enum` (ADTs), `trait` (static dispatch initially)
*   **Error Handling:** `Result[T, E]` and `Optional[T]` enums, `?` operator, `panic` (aborts)
*   **Compile-Time Execution:** `comptime` blocks and functions
*   **Tooling:** `ryo` command (compiler, runner, REPL, package manager frontend), `ryopkg` logic integrated, Cranelift backend (AOT/JIT/Wasm)
*   **FFI:** C interoperability via `extern "C"` and `unsafe` blocks, `ffi` stdlib module. (Future Goal: Explore deeper integration with other language ecosystems like Python).
*   **Standard Library:** Modular packages (`io`, `str`, `collections`, `http`, `json`, `os`, etc.)
*   **Future Concurrency Extensions:** CSP-style channels (`chan`, `select`) planned as optional additions for specialized use cases like actor systems and data pipelines

For full details, see the [Language Specification](docs/specification.md) (Link to spec file).

## Quick Example

```ryo
# src/main.ryo

fn greet(name: &str) -> str {
    return f"Hello, {name}! Welcome to Ryo."
}

fn main() {
    # Type inference like Python
    message = greet("World")
    print(message)

    # Safe collections
    numbers = [1, 2, 3, 4, 5]
    print(f"Numbers: {numbers}")

    # Memory safe - no null pointer exceptions!
    user = Optional.Some("Alice")
    match user:
        Optional.Some(name): print(f"User: {name}")
        Optional.None: print("No user found")
}
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
