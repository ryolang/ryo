# Getting Started with Ryo

This guide covers installation, your first program, and inspecting the compiler. To learn the language itself, see the [Language Reference](https://ryolang.org/reference/) — or try Ryo without installing anything in the [playground](https://play.ryolang.org/).

## Installation

### From Latest Build

```bash
curl -fsSL https://raw.githubusercontent.com/ryolang/ryo/main/install.sh | sh
export PATH="$HOME/.ryo/bin:$PATH"
ryo --version
```

### From Source

Requires **Rust 1.97.0+** ([Install Rust](https://rustup.rs/)). Zig linker is managed automatically.

```bash
git clone https://github.com/ryolang/ryo.git
cd ryo
cargo build --release
```

### What Gets Installed

- Ryo compiler in `~/.ryo/bin/`
- Zig linker (auto-downloaded) in `~/.ryo/toolchain/`
- Tools: `ryo run` (JIT), `ryo build` (AOT), `ryo lex`, `ryo parse`, `ryo ir`, `ryo toolchain`

### Uninstalling

```bash
rm -rf ~/.ryo
```

Remove the PATH entry from your shell profile (`.zshrc`, `.bashrc`, etc.).

## Your First Program

Create `hello.ryo`:

```ryo
fn main():
	print("Hello, Ryo!\n")
```

Run it:

```bash
# If installed via binary:
ryo run hello.ryo

# If built from source:
cargo run -- run hello.ryo
```

You can also compile to a standalone binary:

```bash
ryo build hello.ryo    # produces ./hello
./hello
```

## Exploring the Compiler Pipeline

Ryo exposes each compilation stage for inspection:

```bash
ryo lex hello.ryo              # View tokens
ryo parse hello.ryo            # View AST
ryo ir --emit=uir hello.ryo   # View untyped IR
ryo ir --emit=tir hello.ryo   # View typed IR
ryo ir --emit=clif hello.ryo  # View Cranelift IR
```

## Learning the Language

- **[Language Reference](https://ryolang.org/reference/)** — Types, variables, control flow, functions, built-ins, and the ownership model, documented as implemented. Every example compiles today.
- **[Ownership Lite walkthrough](https://ryolang.org/ownership/)** — Interactive tour of the memory model: the error, the four fixes, and the one rule behind them.
- **[Playground](https://play.ryolang.org/)** — Run Ryo in your browser.
- **[Examples](https://github.com/ryolang/ryo/tree/main/examples)** — Working programs you can compile and run today.
- **[Language Specification](specification.md)** — The complete language design, including features not yet implemented.
- **[Implementation Roadmap](https://github.com/ryolang/ryo/blob/main/docs/dev/implementation_roadmap.md)** — Development milestones.
