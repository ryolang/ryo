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

**Ryo** /ˈraɪoʊ/ (Rye-oh) is a statically-typed, compiled programming language that combines Python's approachable syntax, Rust's memory safety (simplified), and Go's concurrency patterns.

!!! warning "Development Status"
    Ryo is in **pre-alpha**. The compiler is under active construction. Help welcome :)

---

## Why Ryo?

| Aspect | Ryo's Approach |
|--------|----------------|
| **Syntax** | Python-like with colons and tab indentation |
| **Memory** | Ownership + borrowing — no GC, no manual lifetimes |
| **Errors** | Error unions with `try`/`catch` (explicit, exhaustive) |
| **Concurrency** | Green threads + Task/Future/Channel (colorless functions) |
| **Types** | Static with bidirectional inference |
| **Null Safety** | Optional types (`?T`) with `?.` chaining and `orelse` |
| **Performance** | Native code via Cranelift (AOT, JIT, WebAssembly) |

---

```ryo
import std.net.http
import std.json
import std.task

fn fetch_user(id: int) -> (http.NetworkFailure | json.ParseError)!User:
	response = try http.get(f"https://api.example.com/users/{id}")
	data = try response.body_json()
	return User(id: data["id"], name: data["name"])

fn main():
	user_future = task.run:
		return fetch_user(1)

	user = user_future.wait() catch |e|:
		match e:
			http.NetworkFailure(reason):
				print(f"Network error: {reason}")
			json.ParseError(msg):
				print(f"JSON parse error: {msg}")
		return

	print(f"Hello, {user.name}!")
```

---

## Get Started

- **[Getting Started](getting_started.md)** — Install Ryo and write your first program
- **[Language Specification](specification.md)** — Complete language design (2,600+ lines)
- **[Implementation Roadmap](dev/implementation_roadmap.md)** — What's built and what's next
- **[Examples](../examples/)** — Working programs you can run today
- **[GitHub](https://github.com/ryolang/ryo)** — Source code and issues
