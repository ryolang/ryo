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

```ryo title="hello.ryo"
import net.http

struct User:
    id: int
    name: str

async fn fetch_user(id: int) -> Result[User, HttpError] {
    response = await http.get(f"https://api.example.com/users/{id}")
    user = await response.json[User]()
    return Ok(user)
}

fn main() -> Result[(), Error] {
    user = async_runtime.run(fetch_user(1))?
    print(f"Hello, {user.name}!")
    return Ok(())
}
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
