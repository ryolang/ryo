---
title: Ryo Programming Language
hide:
  - title
  - navigation
  - toc
---

# Home {.hide}

<div style="display: flex; align-items: center; gap: 12px; flex-wrap: wrap;">
    <img src="assets/ryo_transparent.svg" alt="Ryo" style="height: 60px;">
  <span style="font-size: 2.5rem; font-weight: 600;">Ryo</span>
</div>


*Productive, Safe, and Fast Programming Language*

<p>
  <img src="https://img.shields.io/github/stars/ryolang/ryo?style=flat&logo=github&color=ffc83d" alt="GitHub Stars">
  <img src="https://img.shields.io/badge/status-pre--alpha-orange?style=flat" alt="Development Status">
  <img src="https://img.shields.io/badge/license-MIT-blue?style=flat" alt="License">
</p>

Ryo **/ˈraɪoʊ/** (Rye-oh) is a modern programming language designed for developers who love **Python's simplicity** but need **memory safety and native performance**. Built with familiar async/await patterns and compile-time safety checks.

!!! warning "Development Status"
    Ryo is in **pre-alpha development**. Not ready for production use.

## Why use Ryo?

1. **Built by developers who love Python** — Ryo brings Python's simplicity to systems programming, with f-strings, clean syntax, and familiar patterns, but compiled to fast native code.

2. **Memory-safe** — Ownership and borrowing prevent memory errors at compile time without a garbage collector. No null pointer exceptions, no use-after-free bugs.

3. **Seamless async/await** — Built-in concurrency with familiar async/await patterns and a high-performance runtime for scalable applications.

4. **Static typing with inference** — Catch errors at compile time while keeping code concise. Type inference means less boilerplate.

5. **Modern tooling** — Integrated package manager, fast compiler via Cranelift, and built-in testing. Everything you need in one tool.

## Safety Guarantees

Ryo's design prioritizes preventing entire classes of bugs at compile time:

- **No null pointer exceptions** — Optional types (`?T`) are explicit and must be handled with `orelse`
- **No silent errors** — Error types (`E!T`) must be handled with `try` or `catch`
- **No direct unwrap allowed** — The compiler rejects any attempt to use an error or optional value without proper handling
- **No use-after-free** — Ownership and borrowing rules prevent accessing freed memory

```ryo
# src/hello.ryo
import net.http

struct User:
    id: int
    name: str
```

```ryo
# src/http/errors.ryo
error NetworkFailure(reason: str)
error InvalidResponse
```

```ryo
# src/main.ryo
import http

async fn fetch_user(id: int) -> (http.NetworkFailure | http.InvalidResponse)!User:
    response = try await http.get(f"https://api.example.com/users/{id}")
    user = try await response.json[User]()
    return user

fn main():
    user = fetch_user(1) catch |e|:
        match e:
            http.NetworkFailure(reason):
                print(f"Network error: {reason}")
            http.InvalidResponse:
                print("Invalid response")
        return
    print(f"Hello, {user.name}!")
```

## Next Steps

**Getting Started**

- [Install Ryo](installation.md) — Set up the compiler and tools
- [Tutorial](getting_started.md) — Learn the language basics
- [Examples](examples/README.md) — See more code examples

**Reference**

- [Language Specification](specification.md) — Complete language reference
- [Standard Library](std.md) — Built-in functions and modules

**Community**

- [GitHub](https://github.com/ryolang/ryo) — Source code and issues
