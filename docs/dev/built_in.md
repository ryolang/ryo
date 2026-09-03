**Status:** Design — partially absorbed into the spec; only the compiler-side boundary framing and internals remain here.

# Built-in vs Standard Library

The distinction between built-in and standard library is critical for Ryo's "General Purpose" and "DX-First" goals.

* **Built-in:** Elements the **compiler** must know to generate machine code (grammar, primitives, memory layout).
* **Standard Library:** Elements the **runtime** provides (I/O, OS interaction, complex logic).

The **built-in set should remain as small as possible** to keep the compiler simple, while the **standard library should feel seamless** via the implicit `core`/`builtin` module (spec §14).

> The language-level facts this doc used to carry now live in the spec: the primitive types (§4.2), `list`/`map` as built-in fundamental types (§4.7), the implicit `core`/`builtin` module with `Drop`, `print`, `panic`, `assert`, `range` (§14), and the hybrid Rust-runtime + Ryo-stdlib split (§14). What remains here is the compiler-side view: which internals the compiler must know, and how it delivers them.

---

### 1. Built-in (The Compiler's Domain)

These cannot be implemented in user code. The lexer/parser/codegen handles them directly.

#### A. Memory Primitives ("Ownership Lite" Mechanics)

The compiler needs these to run the Borrow Checker and Layout generation.

* `&T` (Immutable Reference)
* `inout T` (Mutable Reference)
* `*void` / `*T` (Unsafe C Pointers)
* `?T` (Optional/Nullable - Logic for `none` and `orelse` is hardcoded in codegen).

#### B. "Magic" Structs (Language Lang Items)

These are technically structs defined in the library, but the **compiler knows their internal layout** to support literals.

* **`&str` (String Slice):** The compiler creates these for string literals like `"hello"`, building the fat pointer (ptr + len).
* **`Error`:** The compiler generates the `!T` (Error Union) layout automatically.

---

### 2. Standard Library (The Runtime's Domain)

The package structure itself (`io`, `string`, `collections`, `net.http`, `os`, `task`, …) is specified in §14. What follows is what the spec deliberately does not pin down.

> **`no_std` scope & binary size.** `#![no_std]` applies only to the unconditional **floor** — the implicit `core`/`builtin` module and `std.mem` (alloc, `panic`, `str`, `Drop`). `std.sys` and the OS-bound packages (`net.http`, `os`, `time`, …) are OS-bound by nature and therefore *never* `no_std`; they are linked only when a program reaches them via `import`. **Availability** (batteries-included: every package ships with the toolchain, zero config) and **floor size** (`no_std`) are independent axes — an unreached package costs nothing in the binary. This is how Ryo stays batteries-included like Python/Go without every binary paying for TLS/crypto. The language-level counterpart of this floor is the planned **Runtime Profiles** (`core`/`hosted`) split in spec §19.

#### A. The "System" Module (`std.sys` - Hidden)

The unsafe glue layer between the Ryo `std` packages and the Rust runtime.

* `libc_malloc`, `libc_write`, `libc_open`.

---

### 3. Decision Matrix: Where Does It Go?

Use this checklist when implementing a feature:

| Feature | Is it syntax? | Does it need CPU Registers? | Is it OS specific? | **Verdict** |
| :--- | :--- | :--- | :--- | :--- |
| `if / else` | Yes | Yes | No | **Built-in** |
| `x + y` | Yes | Yes | No | **Built-in** |
| `"foo"` (Literal) | Yes | Yes (Data Section) | No | **Built-in** |
| `s.len()` | No | Yes (Read memory) | No | **Stdlib (Method)** |
| `print()` | No | No | Yes (Syscall) | **Stdlib (implicit `core`/`builtin`)** |
| `list[T]` | Yes (`[]`) | Yes (Layout) | No | **Built-in** |
| `File.open` | No | No | Yes | **Stdlib** |
| `task.spawn` | No | No | No (Runtime) | **Stdlib** |

### 4. The OS-Backed Module Recipe

> Every stdlib package that touches the OS follows the same three tiers: (1) a compiler builtin *only* if it needs magic syntax or literals, (2) an `extern "C"` shim in the Rust runtime (`std.sys`), (3) a safe Ryo wrapper (`std.<package>`). `net.http`, `os`, `encoding.json`, and `time` all follow this shape — see the JSON instance in [`std_ext.md`](std_ext.md) §7.

## References
- Spec: `docs/specification.md` §4.2 (primitive types), §4.7 (built-in collections), §14 (standard library: implicit `core`/`builtin` module, hybrid runtime split, package list)
- Spec: `docs/specification.md` §19 (Runtime Profiles — the language-level counterpart of the `no_std` floor)
- Dev: `std_ext.md` (the three-tier recipe applied to `encoding.json`)
- Roadmap: `docs/dev/implementation_roadmap.md`
