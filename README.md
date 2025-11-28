![Ryo](./docs/assets/ryo.png)
# Ryo Programming Language ⚡

**Ryo /ˈraɪoʊ/: Productive, Safe, Fast.**

<div align="left">
  <img src="https://img.shields.io/badge/status-pre--alpha-orange?style=flat-square" alt="Status">
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="License">
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

## Quick Example

> **Note:** This example shows Ryo's planned features. Most are not yet implemented.

```ryo
# src/main.ryo - Design example showing future features

error UnknownUser(str)

# Returns an UnkownUser error or nothing
fn process_user(user: &str) -> !void:
    if user != "Alice":
        return UnknownUser(user.copy())

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
    process_user(user) catch |e|:
        print(e)
```


## 🧬 Documentation

See [`docs/index.md`](docs/index.md) for a better introduction.
