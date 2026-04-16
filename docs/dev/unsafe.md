# Unsafe Code Architecture

The core design problem: **how to allow dangerous operations (which are necessary) without letting the average user cause memory safety violations.**

Ryo solves this with a **Capability-Based System**. The `unsafe` keyword exists in the language syntax, but the compiler **bans it by default** unless the project explicitly declares itself as a "System Binding Package."

---

### 1. The Architecture: "App" vs. "Binding" Packages

The ecosystem is split into two tiers:

1.  **Application Developers (99%):** Write web servers, CLI tools, scripts.
    *   **Rule:** If `unsafe` appears, the compiler throws a **Hard Error**: *"Unsafe operations are forbidden in application code."*
2.  **Binding Maintainers (1%):** Write wrappers for C libraries (SQLite, TileLang, GTK).
    *   **Rule:** Explicit opt-in via configuration is required.

### 2. Implementation: The "Gatekeeper"

#### Step A: The Configuration (`ryo.toml`)
The package manifest contains a flag that changes the compiler mode for that specific package.

**For a Normal App (User):**
```toml
[package]
name = "my_web_server"
version = "0.1.0"
# 'kind' defaults to "application"
```

**For a Binding Library (Maintainer):**
```toml
[package]
name = "sqlite-sys"
version = "1.0.0"
kind = "system"  # <--- THE MAGIC SWITCH
```

#### Step B: The Compiler Check (In Rust)
In the `SemanticAnalyzer` or `Checker` pass, a check is added when traversing the AST.

```rust
// src/checker.rs

fn check_unsafe_block(&self, block: &UnsafeBlock, config: &PackageConfig) {
    if config.kind != PackageKind::System {
        self.errors.push(Error::new(
            block.span,
            "Forbidden Unsafe: 'unsafe' blocks are not allowed in Application packages.",
            "Hint: Use a pre-existing library or set 'kind = \"system\"' in ryo.toml for C bindings."
        ));
    }
    // If kind == System, allow it.
}
```

### 3. The `unsafe` Syntax

Even though normal users cannot access it, the syntax must be defined for library authors. It follows a familiar style (Rust/Go) but is distinct enough to signal danger.

```ryo
# src/lib.ryo in a 'system' package

# 1. Define the C function
extern "C":
    fn malloc(size: usize) -> *void
    fn free(ptr: *void)

# 2. The Wrapper (Standard Ryo)
pub struct Buffer:
    ptr: *void

# 3. The Unsafe Implementation
impl Buffer:
    fn new(size: int) -> Buffer:
        unsafe:  # Allowed ONLY because ryo.toml says kind="system"
            p = malloc(size)
            return Buffer(ptr=p)

    fn drop(&mut self):
        unsafe:
            free(self.ptr)
```

### 4. Why This Works

1.  **DX Protection:** A new user cannot accidentally write memory-unsafe code. The compiler stops them immediately.
2.  **Ecosystem Clarity:** A package with `kind = "system"` signals: *"This package contains dangerous code and interacts with C."*
3.  **No "Super-User" Syntax:** No separate file extensions (`.ryo` vs `.ffi`) are needed. It is standard language syntax, gated by project configuration.
4.  **Reviewability:** When auditing dependencies, only packages marked `kind = "system"` need review. Everything else is guaranteed safe by the compiler.

### 5. Implementation Roadmap Update

This requires adjustments to **Milestone 21** in the roadmap.

**Milestone 21: C ABI & The "System" Capability**
*   **Task 1:** Add `kind` field to `ryo.toml` parser.
*   **Task 2:** Implement `unsafe` block parsing.
*   **Task 3:** Implement the "Gatekeeper" pass in the compiler (Block `unsafe` usage if `kind != system`).
*   **Task 4:** Implement `extern "C"` parsing (allowed only in system packages).

### 6. The Standard Library

The Standard Library (`std`) is effectively the "Root System Package."
*   It is written in Ryo.
*   It is compiled with the "System" capability enabled.
*   It internally uses `unsafe` to implement `File.open`, `Socket.connect`, etc.
*   It exposes **Safe** structs (`File`, `Socket`) to the user.

This proves the model works: the user interacts with `std` safely, never knowing it contains `unsafe` code internally.
