---
title: Ryo Programming Language
hide:
  - title
  - navigation
  - toc
---

# Home {.hide}

<div style="display: flex; align-items: center; gap: 12px; flex-wrap: wrap;">
    <img src="assets/ryo.png" alt="Ryo" style="height: 60px;">
  <span style="font-size: 2.5rem; font-weight: 600;">Ryo</span>
</div>


*Productive, Safe, and Fast Programming Language*

<p>
  <img src="https://img.shields.io/github/stars/ryolang/ryo?style=flat&logo=github&color=ffc83d" alt="GitHub Stars">
  <img src="https://img.shields.io/badge/status-pre--alpha-orange?style=flat" alt="Development Status">
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat" alt="License">
</p>

**Ryo */ˈraɪoʊ/* (Rye-oh) is a statically-typed, compiled programming language that prioritizes developer experience while maintaining memory safety and competitive performance.** It combines Python's approachable syntax, Rust's memory safety (simplified), and Go's concurrency patterns into a cohesive language designed for modern backend development, CLI tools, and notebooks development.

**Design Philosophy**: Where trade-offs exist, Ryo explicitly chooses **developer productivity and debugging capability over raw performance optimization**. For example, Ryo includes automatic stack traces and rich error context (add some overhead) by default, with configuration options for performance-critical applications.

!!! warning "Development Status"
    Ryo is in **pre-implementation design phase**. Help welcome :)

---

## Why use Ryo?

### 1. **Python-Like Syntax, Compile-Time Safety**
- Clean, readable syntax with colons and indentation (tabs enforced)
- Strong static typing with bidirectional type inference
- No garbage collector—deterministic memory management
- Compiles to native code via Cranelift backend

### 2. **"Ownership Lite" Memory Model**
- Simplified Rust/Mojo-inspired ownership without manual lifetime annotations
- **Borrow-by-default for functions**, move-by-default for assignment
- Three access modes: immutable borrow (`&T`), mutable borrow (`&mut T`), move (`move`)
- RAII-based resource cleanup via `Drop` trait
- Escape hatch: `Shared[T]` (ARC) for shared ownership

### 3. **Rich Error Handling**
- **Error unions** (`ErrorType!SuccessType`) inspired by Zig
- Automatic error composition without wrapper types
- Exhaustive pattern matching on errors
- Automatic stack trace capture at error creation and propagation
- No exceptions—explicit `try`/`catch` operators

### 4. **Green Threads Concurrency**
- **Task/Future/Channel** primitives (Go-inspired)
- No "function coloring"—regular functions can perform async I/O
- Ambient runtime via thread-local storage (testable, swappable)
- Structured concurrency by default
- Work-stealing M:N scheduler

### 5. **Three-Tier Module System**
- **`pub`**: Public API (external packages can import)
- **`package`**: Package-internal (Swift 6-inspired)
- **No keyword**: Module-private (directory-scoped)
- Hierarchical modules based on directory structure
- Circular dependencies forbidden between modules

---

## At a Glance

| Aspect | Ryo's Approach |
|--------|----------------|
| **Memory Management** | Ownership + borrowing (no GC, no manual lifetimes) |
| **Error Handling** | Error unions with `try`/`catch` (explicit, exhaustive) |
| **Concurrency** | Green threads + Task/Future/Channel (colorless functions) |
| **Type System** | Static types with bidirectional inference |
| **Null Safety** | Optional types (`?T`) with `?.` chaining and `orelse` |
| **Performance** | Native code via Cranelift (with overhead for DX features) |
| **Tooling** | Integrated package manager, REPL, testing framework |
| **FFI** | C interop via `extern "C"` (requires `kind = "system"`) |

---

## Design Differentiators

### **Easier than Rust**
- No manual lifetime annotations
- Simpler borrowing rules (implicit immutable borrows for function parameters)
- Python-like syntax vs. C-like syntax

### **Safer than Python**
- Compile-time memory safety (no null pointer errors, no data races)
- Explicit error handling (no exceptions)
- Static typing with inference

### **More Expressive than Go**
- Algebraic data types (enums with associated data)
- Errors can contain associated data
- Pattern matching with exhaustiveness checking
- Ergonomic error handling with `try`

---

```ryo
# src/models.ryo
struct User:
    id: int
    name: str
```

```ryo
# src/errors.ryo
error InvalidResponse
```

```ryo
# src/main.ryo
import std.json
import std.net.http
import std.task
import models
import errors

fn fetch_user(id: int) -> (http.NetworkFailure | errors.InvalidResponse | json.ParseError)!models.User:
    # Green threads: no async/await needed - functions automatically suspend
    # try handle errors and returns http.NetworkFailure
    response = try http.get(f"https://api.example.com/users/{id}")
    
    # Parse JSON response (v0.1.0 uses runtime type checking, not generics)
    data = try response.body_json() orelse return errors.InvalidResponse
    user = models.User(
        id: data["id"],
        name: data["name"]
    )
    return user

fn main():
    # Spawn task - returns future
    user_future = task.run:
        return fetch_user(1)

    # Wait for result - green thread suspends, OS thread continues
    user = user_future.wait() catch |e|:
        match e:
            http.NetworkFailure(reason):
                print(f"Network error: {reason}")
            errors.InvalidResponse:
                print("Invalid response from server")
            json.ParseError(msg):
                print(f"JSON parse error: {msg}")
        return
    
    print(f"Hello, {user.name}!")
```

---

## Target Use Cases

**✅ Excellent fit for:**
- Web backends, APIs, microservices (I/O-bound workloads)
- CLI tools, build systems, developer tooling
- Network services and proxies
- WebAssembly applications
- Game development tooling and scripting
- Data processing pipelines

**⚠️ Consider alternatives for:**
- Ultra-low-latency systems (HFT, real-time audio/video)
- Bare-metal embedded systems with tight resource constraints
- Applications where debugging overhead is unacceptable

---

## Current Status

**Stage**: Pre-implementation design phase  
**Documentation**: Comprehensive specification (2,600+ lines)  
**Completeness**: Core design is complete; some features marked "planned for future"

### What's Specified
✅ Core syntax and semantics  
✅ Type system (primitives, collections, enums, errors, optionals)  
✅ Memory management model  
✅ Error handling system  
✅ Concurrency model (green threads)  
✅ Module system with visibility rules  
✅ Standard library architecture  

### Acknowledged Gaps
⏳ Formal grammar (EBNF/BNF)  
⏳ Detailed stdlib API signatures  
⏳ Precise borrow checker algorithm  
⏳ Some advanced features (comptime, generics, dyn traits)  

---

## Key Design Trade-offs

### DX vs. Performance
**Choice**: Rich debugging by default (overhead) with escape hatches  
**Rationale**: Most applications save more developer time than runtime performance  
**Escape hatch**: `--error-traces=minimal` or `=off` for performance-critical code

### Simplicity vs. Expressiveness
**Choice**: Simplified borrowing (no lifetimes) over maximum flexibility  
**Rationale**: Covers 90% of use cases with 10% of Rust's complexity  
**Escape hatch**: `Shared[T]` for complex ownership patterns

### Safety vs. Control
**Choice**: Safe by default, `unsafe` requires `kind = "system"` in `ryo.toml`  
**Rationale**: Application code stays safe; library authors can build safe abstractions  

---

## Next Steps for Reviewers

We welcome feedback on:

1. **Design Philosophy**: Is the DX-first approach justified?
2. **Ownership Model**: Is "Ownership Lite" practical for real-world code?
3. **Error Handling**: Are error unions intuitive compared to Result types or exceptions?
4. **Concurrency**: Does green threads + ambient runtime solve actual problems?
5. **Module System**: Is the three-tier visibility model appropriate?
6. **Gaps**: What's missing for a complete evaluation?

**Full Specification**: See [`docs/specification.md`](specification.md) for complete details (2,600+ lines).

**Community**

- [GitHub](https://github.com/ryolang/ryo) — Source code and issues

---

## Technical Foundation

- **Compiler Backend**: Cranelift (AOT, JIT, WebAssembly support)
- **Linker**: Zig (`zig cc`) for cross-compilation and C interop
- **Runtime**: Hybrid architecture (Rust runtime + Ryo stdlib)
- **Package Manager**: Cargo-inspired (`ryo.toml`, `ryopkgs.io` registry)
- **Testing**: Integrated framework with `#[test]` attributes

---
