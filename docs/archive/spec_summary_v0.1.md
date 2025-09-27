**Ryo Programming Language Specification (Draft v0.1)**

**1. Introduction & Vision**

Ryo is a proposed statically-typed, compiled programming language designed to offer a pragmatic balance between performance, safety, and developer ergonomics. It aims to combine the compile-time memory safety guarantees inspired by Rust (simplified, without a garbage collector), the approachable syntax and developer experience reminiscent of Python, and the simple, effective concurrency model pioneered by Go.

**Core Vision:** To be the language of choice for developers seeking performance and reliability without the steep learning curve of Rust or the runtime overhead/safety trade-offs of garbage-collected or dynamically-typed languages, particularly in domains like backend services, CLI tools, and WebAssembly.

**Target Audience:** Developers familiar with languages like Python, Go, TypeScript, or C# who desire better performance, stronger safety guarantees, and no GC pauses, but find the complexity of languages like Rust or C++ prohibitive for their typical tasks.

**2. Guiding Principles**

*   **Safety First (Simplified):** Memory safe by default without a garbage collector. Errors should be explicit (e.g., `Result`, `Optional`). Safety mechanisms should be powerful but strive for less cognitive overhead than alternatives like Rust's full lifetime system.
*   **Pythonic Ergonomics:** Familiar syntax, readability prioritized. Aim for a smooth developer experience in writing, reading, and maintaining code.
*   **Go-like Simplicity:** Minimal keyword set, straightforward core concepts, avoid unnecessary feature creep. Focus on providing essential, orthogonal features.
*   **Performance:** Compiled to efficient native code (or Wasm) via Cranelift. No GC pauses. Aim for performance competitive with Go and close to Rust/C++ for many tasks.
*   **Powerful Compile-Time:** Integrate compile-time execution (`comptime`) deeply for metaprogramming, configuration, optimization, and enhanced safety checks.
*   **Excellent Tooling:** Provide a seamless experience out-of-the-box, including a fast compiler, integrated package manager, REPL, and testing framework.

**3. Syntax**

*   **Blocks:** Indentation-based, like Python.
*   **Keywords (Minimal Set):** `fn`, `struct`, `enum`, `trait`, `impl`, `mut`, `if`, `elif`, `else`, `for`, `in`, `return`, `break`, `continue`, `import`, `package`, `comptime`, `spawn`, `chan`, `select`, `match`, `pub`.
    *   *Rationale:* Keeps the language core small and easy to learn (Go influence). Uses `fn`, `trait` common in modern languages (Rust/Swift). Avoids `while` (use `for condition:`), `let` (use inference), `None` (use `Optional`).
*   **Comments:** `#` for single-line. `"""Docstrings"""` for multi-line documentation associated with functions, structs, etc.
*   **Statements:** Generally one per line; semicolons optional and discouraged.

**4. Types & Variables**

*   **Static Typing:** Types checked at compile time.
*   **Variable Declaration:**
    *   `name = value` (immutable, type inferred)
    *   `mut name = value` (mutable, type inferred)
    *   `name: type = value` (immutable, type explicit)
    *   `mut name: type = value` (mutable, type explicit)
    *   *Rationale:* Pythonic feel for common case (`name = value`), explicitness available when needed or required (signatures, struct fields). `mut` required for mutability (safety).
*   **Basic Types:** `bool`, `str` (UTF-8 owned string - TBD name?), sized integers (`i8`, `u8`, `i16`, `u16`, `i32`, `u32`, `i64`, `u64`), architecture-dependent integers (`isize`, `usize`), floating point (`float32`, `float64`).
    *   *Rationale:* Explicit sized types needed for control and FFI. `usize`/`isize` needed for indexing/pointers.
*   **String & Slice Types (TBD - Needs Refinement):** Requires clear distinction between owned, mutable strings (e.g., `String`) and immutable string slices/views (e.g., `&str`). Also generic slice types (`&[T]`, `&mut [T]`).
    *   *Rationale:* Essential for efficient string/data handling without unnecessary copying.
*   **Composite Types:**
    *   `struct Name:` Defines data structures. Fields require type annotations.
    *   `enum Name:` Algebraic data types (sum types), supporting variants with associated data (e.g., `enum Message: Quit, Write(str), Move { x: i32, y: i32 }`).
    *   `trait Name:` Defines a set of method signatures (behavior).
*   **Built-in Collections:** Generic `List[T]` (dynamic array), `Map[K, V]` (hash map). Literals: `[1, 2, 3]`, `{"a": 1, "b": 2}`. Respect ownership/borrowing.
    *   *Rationale:* Essential data structures provided out-of-the-box.
*   **Optionality:** Built-in `enum Optional[T]: Some(T), Empty`. Replaces `null`/`None`. Functions must return `Optional[T]` if they might not return a value. Handled via `match` or optional chaining (`?.`).
    *   *Rationale:* Explicit handling of absence significantly improves safety over nullable types.
*   **User-Defined Generics (TBD - Needs Specification):** Syntax and semantics for generic `fn`, `struct`, `enum`, `trait`. Likely requires trait bounds for constraining type parameters (e.g., `fn process[T: Printer](item: T)`).
    *   *Rationale:* Critical for writing reusable, type-safe code.

**5. Memory Management**

*   **No Garbage Collector:** Eliminates GC pauses and associated runtime overhead.
    *   *Rationale:* Key differentiator from Go/Java/Python, enabling predictable performance for systems/latency-sensitive tasks.
*   **Ownership:** Each value has a single owner. When the owner goes out of scope, the value is dropped.
*   **Move Semantics:** By default, assignment or passing values to functions transfers ownership. The original variable becomes inaccessible.
*   **Borrowing (Simplified):**
    *   `&` creates an immutable borrow (shared reference). Multiple immutable borrows can exist.
    *   `&mut` creates a mutable borrow (exclusive reference). Only one mutable borrow (or any number of immutable borrows) can exist at a time.
    *   **Scope-Based Lifetimes:** The compiler enforces borrow validity based *strictly* on lexical scopes. If a borrow *might* outlive the owner's scope, it's a compile error. **No manual lifetime annotations (`'a`)**.
    *   *Rationale:* Provides significant memory safety guarantees without Rust's full lifetime complexity. This is the core trade-off: simpler than Rust, but potentially rejecting some valid patterns Rust allows. Error messages must be excellent.
*   **RAII (`Drop` Trait):** Types can implement the `Drop` trait (`trait Drop: fn drop(mut self: Self)`). The `drop` method is automatically called when the owner goes out of scope, enabling automatic resource cleanup (files, locks, sockets).
    *   *Rationale:* Provides deterministic, automatic resource management essential in a non-GC language. Simpler and more integrated than `defer`, safer than manual cleanup. Chosen over Linear Types for better ergonomics.
*   **Shared Ownership:** Standard library `Shared[T]` type using atomic reference counting for opt-in shared ownership when needed. Makes the cost explicit.

**6. Control Flow**

*   `if`/`elif`/`else` expressions.
*   `for` loops:
    *   Iterator: `for item in collection:`
    *   Condition: `for condition:` (e.g., `for i < 10:`)
    *   Infinite: `for:`
    *   *Rationale:* `for` covers all looping needs, removing `while` simplifies the keyword set (Go influence).
*   `match` Statement: For powerful pattern matching on `enum` variants (`Optional`, `Result`, custom enums). Needs specification on advanced features (guards, bindings).
*   `break`, `continue`: Standard loop control.

**7. Functions & Closures**

*   **Definition:** `fn name(arg: type) -> return_type:` syntax. Return type required unless implicit unit.
*   **Arguments/Return:** Follow ownership/borrowing rules (pass by value moves, pass by `&` borrows).
*   **Closures (TBD - Needs Specification):** Anonymous functions (`fn() { ... }`). Need to define capture rules (by move, by borrow?), syntax (`move` keyword?), and how they interact with ownership/lifetimes.
    *   *Rationale:* Essential for ergonomic APIs, callbacks, and concurrency.

**8. Error Handling**

*   **Recoverable Errors:** Built-in `enum Result[OkType, ErrType]`. Functions return `Result` for operations that can fail predictably.
*   **`?` Operator:** Postfix `?` unwraps `Ok(value)` to `value` or propagates `Err(error)` early from the current function (must return compatible `Result`).
    *   *Rationale:* Provides structured error handling without exceptions. `?` offers significant ergonomic improvement over manual `match` or `if err != nil`.
*   **Unrecoverable Errors (Panics) (TBD - Needs Specification):** Mechanism for handling programming errors/broken invariants (`panic!`?). Define behavior (unwinding? abort?), possibility of recovery (`catch_unwind`?).
    *   *Rationale:* Necessary for dealing with unexpected, catastrophic failures.

**9. Traits & Implementation**

*   **Traits:** `trait Name: ...` defines method signatures. Similar to Go interfaces or Rust traits.
*   **Implementation:** `impl Trait for Type: ...` provides concrete implementations of trait methods for a specific type. Methods are called using dot syntax (`value.method()`).

**10. Concurrency**

*   **Model:** Go-inspired Communicating Sequential Processes (CSP).
*   **Primitives:**
    *   `spawn`: Keyword to run a function concurrently in a lightweight task (similar to goroutine).
    *   `chan[DataType]`: Typed channels for communication and synchronization between tasks. Ownership rules apply to data sent.
    *   `select`: Statement to wait on multiple channel operations (send or receive) concurrently.
    *   *Rationale:* Chosen over `async`/`await` due to the complexity of making `async`/`await` memory-safe without GC and without Rust's lifetime complexity. CSP aligns well with ownership (moving data between tasks via channels) and Go-like simplicity.
*   **Ergonomics:** Standard library should provide wrappers (`Future` types with `.wait()`, `select`-integration, helpers like `wait_all`) to simplify common async patterns (e.g., HTTP requests) built on top of the core primitives.

**11. Compile-Time Execution (`comptime`)**

*   **Mechanism:** Code within `comptime { ... }` blocks or marked as `comptime fn` runs at compile time.
*   **Capabilities:** Can evaluate functions, manipulate types, read files (relative to build), generate data, enforce constraints. Results can initialize constants or influence code generation. Operates in a restricted environment.
*   *Rationale:* Inspired by Zig/Odin. Enables powerful metaprogramming, configuration loading, pre-computation, and enhanced safety checks without runtime overhead or macros.

**12. Modules & Packages**

*   **Organization:** Code organized into packages (`package name` declaration). A directory is a package.
*   **Imports:** `import path.to.module`. Use module name for qualification (`module.function()`). Details TBD (aliases? selective imports?).
*   **Visibility:** `pub` keyword marks items (structs, fns, fields, enum variants) as usable outside the current module. Default is private.
*   **Package Manager (`ryopkg` - TBD Details):** Needs specification for manifest file format (`Ryo.toml`?), dependency resolution, versioning (semver?), build commands, test integration, publishing.
    *   *Rationale:* A robust, integrated package manager is critical for ecosystem growth.

**13. FFI & `unsafe`**

*   **FFI (TBD - Needs Specification):** Mechanism to call functions from external (typically C) libraries and expose Ryo functions to C. Syntax (`extern "C" fn ...`?), type mapping rules needed.
    *   *Rationale:* Essential for interoperability with existing code, OS APIs.
*   **`unsafe` Blocks/Functions (TBD - Needs Specification):** `unsafe { ... }` or `unsafe fn`. Required to perform operations the compiler cannot guarantee memory safety for (e.g., dereferencing raw pointers, calling FFI functions, certain low-level type manipulations).
    *   *Rationale:* Necessary escape hatch for low-level programming and FFI. Use must be clearly demarcated and minimized.

**14. Standard Library Philosophy (TBD - Needs Scope Definition)**

*   Provide a practical, "batteries-included" (within reason) standard library covering common needs.
*   Core Modules Needed: `io` (console, files), `os` (env, fs ops), `net` (tcp/udp, http?), `time`, `math`, `collections` (List, Map, Set?), `str` (string ops), `fmt` (formatting), `sync` (mutex, atomics, needed alongside channels), `testing`, `encoding` (json, utf8, etc.).
*   Leverage Ryo's features (Ownership, `Result`, `Optional`) for safe and robust APIs.

**15. Tooling**

*   **Compiler:** Based on Cranelift backend. Supports **AOT compilation** for deployment (`ryoc`?) and **JIT compilation** capability for interactive tools. Aims for fast compilation speeds.
*   **Package Manager:** `ryopkg` (TBD) - Integrated build, dependency management, testing.
*   **REPL:** Interactive `ryo` command using JIT capability for learning and experimentation.
*   **Testing Framework:** Built-in support (e.g., `test fn ...` annotation, `ryopkg test` command).
*   **LSP (Language Server Protocol):** Future goal for IDE integration (autocompletion, diagnostics, etc.).

**16. Target Domains**

*   Web Backend Development (API Servers, Microservices)
*   CLI Tools
*   Network Services & Proxies
*   WebAssembly (Wasm) Applications & Libraries
*   Game Development (Tooling, Scripting, Core Logic)
*   Data Processing & ETL Pipelines (Performance-sensitive parts)
*   Higher-Level Embedded Systems

**17. Development Milestones (Proposed)**

*   **M0: Foundation (The "Walking Skeleton")**
    *   Core syntax parsing (AST).
    *   Basic type system & type checking.
    *   Simplified ownership/borrow checking (scope-based).
    *   Basic AOT compilation via Cranelift (simple code generation for core types/ops).
    *   Primitive types (`i32`, `bool`, etc.).
    *   Basic `fn`, `struct`, `if`, assignment.
    *   Minimal runtime.
    *   *Goal: Compile and run "hello world" and basic functions.*

*   **M1: Core Usability**
    *   `enum` and `match` statements.
    *   `Drop` trait and RAII implementation.
    *   Basic `comptime` support.
    *   `Result` and `Optional` enums, `?` operator.
    *   Basic built-in collections (`List`, `Map`).
    *   Initial standard library modules (`io`, basic `fmt`).
    *   Basic package manager (`ryopkg` for local builds).
    *   Basic user generics (functions/structs, no bounds yet).
    *   Concrete integer types, basic string type (owned).
    *   *Goal: Write simple, useful programs managing resources and handling errors.*

*   **M2: Concurrency & Tooling**
    *   `spawn`, `chan`, `select` implementation with runtime scheduler.
    *   REPL implementation using JIT.
    *   Built-in testing framework (`ryopkg test`).
    *   Closures (define capture rules and syntax).
    *   String/Slice distinction (`&str`, `&[T]`).
    *   More standard library modules (`net`, `time`, `os` basics).
    *   Improved package manager (dependency fetching).
    *   *Goal: Build concurrent applications and have a solid developer workflow.*

*   **M3: Interoperability & Maturity**
    *   FFI definition and implementation.
    *   `unsafe` keyword and semantics.
    *   Panic mechanism definition and implementation.
    *   Initial Wasm target support.
    *   Traits and `impl Trait for Type`.
    *   Advanced generics (trait bounds?).
    *   Package manager features (publishing, profiles).
    *   More standard library modules (`sync`, `encoding`).
    *   *Goal: Interoperate with C, target Wasm, build more complex abstractions.*

*   **M4+: Ecosystem & Refinement**
    *   Performance optimizations (compiler, runtime).
    *   Language Server Protocol (LSP) implementation for IDEs.
    *   Stabilize standard library APIs.
    *   Expand standard library based on user needs.
    *   Community building, documentation improvements.
    *   Consider advanced features (more `comptime` power, custom allocators?).
    *   *Goal: Foster a healthy ecosystem and mature the language.*

**18. Future Considerations / Open Questions**

*   Precise rules for closure captures and lifetimes.
*   Exact syntax and power of user-defined generics and trait bounds.
*   Detailed design of FFI and `unsafe` semantics.
*   Panic unwinding vs. abort strategy.
*   Full specification of the standard library modules.
*   Package manager details (dependency resolution, locking, etc.).
*   Advanced `match` pattern features.
*   Metaprogramming capabilities beyond `comptime`.

**Disclaimer:** This document describes a hypothetical programming language based on conversational design. Many details require significant further thought, design work, and practical implementation experience to validate and refine.

--- 