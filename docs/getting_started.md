# Getting Started with Ryo

This guide covers installation, your first program, and basic syntax. For the complete language design, see the [Language Specification](specification.md).

## Installation

### From Latest Build

```bash
curl -fsSL https://raw.githubusercontent.com/ryolang/ryo/main/install.sh | sh
export PATH="$HOME/.ryo/bin:$PATH"
ryo --version
```

### From Source

Requires **Rust 1.91+** ([Install Rust](https://rustup.rs/)). Zig linker is managed automatically.

```bash
git clone https://github.com/ryolang/ryo.git
cd ryo
cargo build --release
```

### What Gets Installed

- Ryo compiler in `~/.ryo/bin/`
- Zig linker (auto-downloaded) in `~/.ryo/toolchain/`
- Tools: `ryo run` (JIT), `ryo build` (AOT), `ryo lex`, `ryo parse`, `ryo ir`

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

## Basic Syntax

### Variables

Variables are immutable by default. No declaration keyword needed — the compiler infers types.

```ryo
fn main():
	name = "Alice"          # inferred as str, immutable
	count: int = 10         # explicit type annotation
	mut total = 0           # mutable variable
	total = total + count
```

### Functions

```ryo
fn add(x: int, y: int) -> int:
	return x + y

fn main():
	result = add(3, 4)
	print("done\n")
```

Function signatures require type annotations. Return types can be omitted for functions that return nothing.

### Control Flow

```ryo
fn classify(n: int) -> str:
	if n > 0:
		return "positive"
	elif n == 0:
		return "zero"
	else:
		return "negative"
```

### Comments

```ryo
# Single-line comments start with #
x = 42  # inline comment
```

### Naming Conventions

| Convention | Used for | Examples |
|---|---|---|
| `snake_case` | Variables, functions, modules | `user_name`, `get_config` |
| `PascalCase` | Structs, enums, traits | `UserProfile`, `TempScale` |
| Lowercase | Built-in types | `int`, `str`, `float`, `bool` |

## Next Steps

- **[Examples](https://github.com/ryolang/ryo/tree/main/examples)** — Working programs you can compile and run today
- **[Language Specification](specification.md)** — Complete language design
- **[Implementation Roadmap](https://github.com/ryolang/ryo/blob/main/docs/dev/implementation_roadmap.md)** — Development milestones
