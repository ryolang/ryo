<div align="center">
  <img src="./docs/assets/ryo.svg" alt="Ryo Logo" width="120" />
  <h1>Ryo Programming Language</h1>
  <p><strong>Ryo /ˈraɪoʊ/ — Looks like Python. Runs like Rust. Quacks like Ryduck.</strong></p>
  <p>
    <img src="https://img.shields.io/badge/status-pre--alpha-orange?style=for-the-badge" alt="Status">
    <img src="https://img.shields.io/github/actions/workflow/status/ryolang/ryo/ci.yml?branch=main&style=for-the-badge" alt="Build">
    <img src="https://img.shields.io/badge/license-MIT-blue?style=for-the-badge" alt="License">
    <a href="https://app.codspeed.io/ryolang/ryo?utm_source=badge"><img src="https://img.shields.io/endpoint?url=https://codspeed.io/badge.json" alt="CodSpeed"/></a>
  </p>
</div>

<br>

**Ryo** is a statically-typed language designed for high **Developer Experience**. It pairs a Pythonic, indentation-based syntax with Rust-style memory safety ("Ownership Lite" — borrow by default, move on assignment, no garbage collector), Go-style "colorless" green-thread concurrency, and Zig-style error unions (`!T`). It targets web backends, CLI tools, and scripting/notebook environments, compiling to native code via **Cranelift** (AOT and JIT).

> [!WARNING]
> Ryo is in **pre-alpha**. The language design is stabilizing but the compiler is under active construction. Not yet ready for production use.

## Example

```ryo
fn main():
	print("Hello, World!\n")
```

```bash
cargo run -- run examples/hello.ryo
```

More examples in [`examples/`](examples/). For what the compiler implements today and what's next, see the [Implementation Roadmap](docs/dev/implementation_roadmap.md).

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

Requires **Rust 1.97.0+** ([Install Rust](https://rustup.rs/)). Zig linker is managed automatically.

```bash
git clone https://github.com/ryolang/ryo.git
cd ryo
cargo build --release
cargo run -- run examples/hello.ryo
```

**Next steps:** [Getting Started](docs/getting_started.md)

## Language Inspirations

Python (syntax, type inference), Rust (ownership, ADTs, pattern matching, traits), Mojo (lifetime-free ownership, value semantics), Go (simplicity, fast compilation, CSP concurrency), and Zig (comptime, explicit error handling).

## Getting Help & Contributing

Questions, bugs, and proposals go through the [issue tracker](https://github.com/ryolang/ryo/issues). Contributions are welcome across the compiler, standard library, tooling, documentation, and examples.

## Documentation

- [Language Specification](docs/specification.md) — Complete language design and syntax
- [Implementation Roadmap](docs/dev/implementation_roadmap.md) — Milestones and progress
- [Compilation Pipeline](docs/dev/pipeline_alignment.md) — How the compiler works
- [Proposals](docs/dev/proposals.md) — Future features
- [Design Issues](docs/dev/design_issues.md) — Open design decisions
- [CLAUDE.md](CLAUDE.md) — Project context for AI assistants and contributors

## License

Ryo is distributed under the terms of the MIT license. See [LICENSE](LICENSE) for details.
