# Ryo Programming Language - Project Context

**Ryo** is an early-stage (pre-alpha) statically-typed, compiled aot/jit programming language that draws inspiration from several modern programming languages, taking the best ideas from each:

### 🐍 Python - Syntax & Developer Experience
- **Clean, readable syntax** with colons and indentation, IMPORTANT!
- **Type inference** for reduced boilerplate
- **F-strings** for intuitive string formatting
- **Tab-based indentation** (enforced at compile-time)

**Rationale**: Python's syntax makes code accessible to developers of all skill levels. By adopting its readability, Ryo lowers the barrier to entry while maintaining compile-time safety.

### 🦀 Rust - Ownership & Type Safety
- **Ownership model** for memory safety without garbage collection
- **Algebraic data types** (enums with associated data)
- **Pattern matching** with exhaustive checks
- **Trait system** for polymorphism

**Rationale**: Rust's ownership system eliminates entire classes of bugs (use-after-free, data races) while maintaining performance. Ryo simplifies this concept with "Ownership Lite" - removing lifetime annotations while keeping the core safety guarantees.

### 🔥 Mojo - Ownership Simplified
- **Ownership without lifetimes** - simpler mental model
- **Value semantics** with clear ownership transfer
- **Progressive complexity** - start simple, add complexity when needed

**Rationale**: Mojo demonstrates that ownership doesn't require Rust's complexity. Ryo follows this philosophy, making memory safety accessible to Python developers.

### 🔷 Go - Simplicity & Concurrency
- **Simplicity as a core value** - fewer language features, done well
- **Fast compilation** times for rapid development
- **Built-in concurrency primitives** future: CSP-style channels with a twist
- **Single, standard toolchain** no build configuration hell
- **Pragmatic approach** to language design

**Rationale**: Go proves that simplicity and performance aren't mutually exclusive. Ryo adopts this pragmatic philosophy while adding modern type safety.

### ⚡ Zig - Simplicity, Error Handling & Compile-Time
- **No hidden control flow** - explicit is better than implicit
- **Comptime execution** for zero-cost abstractions without macros
- **Simple error handling** with explicit error sets, IMPORTANT!
- **No operator overloading** - predictable code behavior
- **Minimal runtime** requirements

**Rationale**: Zig's `comptime` provides powerful metaprogramming without complex macro systems. Ryo adopts this for configuration, code generation, and type introspection.

**Design Philosophy:**
1. **Simplicity** - Fewer language features, done well
2. **Ownership Lite** - Simplified memory management without manual lifetimes
3. **Python-Inspired Syntax** - Colons and indentation instead of braces
4. **Compile-Time Safety** - Static typing with inference, exhaustive pattern matching
5. **No Garbage Collection** - Predictable performance through ownership
6. **Built in Concurrency** - Task/Future/Channel runtime for I/O-bound work
7. **AI-Era Design** - Optimized for the AI-writes, human-reviews workflow

**AI-Era Language Design:** As of 2026, most code is written by AI agents and reviewed by humans. Ryo optimizes for this: strict rules and verbose safety patterns (the AI handles ceremony), explicit naming and predictable semantics (the human benefits from clarity). Compiler strictness catches errors before production. Python-style syntax and readable error messages serve the human side of the workflow.

**Target Audience:** Python developers who need better performance, memory safety, and static type checking with an easier learning curve than Rust.

---

## File Naming Conventions

### Ryo Source Files
- Use lowercase with underscores: `error_handling.ryo`, `hello_world.ryo`
- Example files: `docs/examples/temperature_converter.ryo`

### Documentation Files
- Use lowercase with underscores: `getting_started.md`, `design_issues.md`
- Special files use uppercase: `README.md`, `CLAUDE.md`, `TODO.md`

### Rust Source Files
- Use lowercase with underscores: `main.rs`, `ast.rs`, `codegen.rs`
- Follow Rust conventions

---

## Critical Syntax Rules

### ⚠️ CRITICAL: Python-Style Syntax is MANDATORY

All Ryo code examples **must** use Python-style colons and indentation, **NOT** curly braces.

### No Braces for Code Blocks

### Braces Are Only for F-Strings

### Tab Indentation

- **Use TABS** (not spaces)
- Mixing tabs and spaces is a **compile-time error**
- One tab = one indentation level

---

## Documentation Standards

### Code Examples

Always use fenced code blocks with language tag:

````markdown
```ryo
fn main():
    print("Hello, World!")
```
````

### Cross-References

Use relative paths for internal links:
```markdown
See [specification](docs/specification.md) for details.
See [examples](docs/examples/README.md) for code samples.
```

---

## Quick Command Reference

### Build & Run
```bash
cargo check                  # Check local pkg for errors
cargo build                  # Build debug
cargo build --release        # Build release
cargo run -- run <file>      # Compile and execute
cargo test                   # Run tests
```

### File Extensions
- `.ryo` - Ryo source files
- `.md` - Markdown documentation
- `.rs` - Rust source files
- `.o` / `.obj` - Object files (generated)

---
