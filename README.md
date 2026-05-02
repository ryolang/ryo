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

> [!WARNING]
> Ryo is in **pre-alpha**. The language design is stabilizing but the compiler is under active construction. Not yet ready for production use.

## Key Features

*   **Pythonic Syntax:** Indentation-based, readable, and concise.
*   **Ownership Lite:** No Garbage Collector. Memory safety via Mojo-style borrowing (functions borrow by default, assignments move).
*   **Green Threads:** "Colorless" concurrency with a work-stealing scheduler. No `async`/`await` needed.
*   **Rich Errors:** Zig-style Error Unions (`!T`) with payload data.
*   **Fast:** Built on **Cranelift** for instant compile times and native performance.

## Quick Example

```ryo
fn main():
	print("Hello, World!\n")
```

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

```bash
cargo run -- run examples/classify.ryo
```

**More examples:** [`examples/`](examples/)

## Current Status (Milestone 8b)

The compiler implements through **Milestone 8b: Conditionals & Logical Operators**:

- **Types:** `int`, `float`, `bool`, `str` (literals)
- **Operators:** arithmetic, comparison, logical, unary negation
- **Variables:** immutable by default, `mut`, type annotations, type inference
- **Functions:** definitions, calls, forward references, recursion
- **Control flow:** `if`/`elif`/`else`, short-circuit `and`/`or`
- **Builtins:** `print()`
- **Compilation:** AOT (native binary) and JIT via Cranelift
- **Tooling:** `ryo run`, `ryo build`, `ryo lex`, `ryo parse`, `ryo ir --emit=uir|tir|clif`

See the [Implementation Roadmap](docs/dev/implementation_roadmap.md) for what's next.

## Installation

### From Latest Build

```bash
curl -fsSL https://raw.githubusercontent.com/ryolang/ryo/main/install.sh | sh
export PATH="$HOME/.ryo/bin:$PATH"
ryo --version
```

Dev builds are manually triggered and may be unstable. Update with `--force`:
```bash
curl -fsSL https://raw.githubusercontent.com/ryolang/ryo/main/install.sh | sh -s -- --force
```

### From Source

Requires **Rust 1.92+** ([Install Rust](https://rustup.rs/)). Zig linker is managed automatically.

```bash
git clone https://github.com/ryolang/ryo.git
cd ryo
cargo build --release
cargo run -- run examples/hello.ryo
```

**Next steps:** [Getting Started](docs/getting_started.md)

## Language Inspirations

*   **Python** — Clean syntax with colons and indentation, f-strings, type inference
*   **Rust** — Ownership model, algebraic data types, pattern matching, traits
*   **Mojo** — Simplified ownership without lifetimes, value semantics
*   **Go** — Simplicity as a core value, fast compilation, CSP concurrency
*   **Zig** — Comptime execution, explicit error handling, readable-by-default design

## Contributing

We welcome contributions — compiler development, standard library, tooling, documentation, language design, and examples. Check out the [open issues](https://github.com/ryolang/ryo/issues).

## Documentation

- [Language Specification](docs/specification.md) — Complete language design and syntax
- [Implementation Roadmap](docs/dev/implementation_roadmap.md) — Milestones and progress
- [Compilation Pipeline](docs/dev/pipeline_alignment.md) — How the compiler works
- [Proposals](docs/dev/proposals.md) — Future features
- [Design Issues](docs/dev/design_issues.md) — Open design decisions
- [CLAUDE.md](CLAUDE.md) — Project context for AI assistants and contributors

## License

Ryo is distributed under the terms of the MIT license. See [LICENSE](LICENSE) for details.
