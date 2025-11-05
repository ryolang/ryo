# Ryo Programming Language Specification

## 1. Introduction & Vision

*   **Vision:** Ryo is a statically-typed, compiled programming language designed to offer a pragmatic balance between performance, memory safety, and developer ergonomics. It aims to combine the compile-time memory safety guarantees inspired by Rust (simplified, without a garbage collector), the approachable syntax and developer experience reminiscent of Python, and familiar async/await concurrency patterns.

*   **Target Domains:** Web Backend Development (API Servers, Microservices), CLI Tools, Network Services & Proxies, WebAssembly (Wasm) Applications & Libraries, Game Development (Tooling, Scripting, Core Logic), Data Processing & ETL Pipelines, and Higher-Level Embedded Systems.
*   **Core Goals:**
    *   **Python-like Simplicity:** Clean, readable, minimal syntax. Easy to learn, especially for Python developers. Reduce boilerplate.
    *   **Rust-like Safety (Simplified):** Memory safe by default via ownership and borrowing, without GC. Compile-time checks prevent dangling pointers, data races, use-after-free. Simplified borrowing model compared to Rust (no manual lifetimes).
    *   **Go-like Simplicity:** Minimal keyword set, straightforward core concepts, avoid unnecessary feature creep. Focus on providing essential, orthogonal features.
    *   **Performance:** Compiled to efficient native code (or Wasm) via **Cranelift**. No GC pauses. Deterministic resource management.
    *   **Effective Concurrency:** Simple and safe concurrency using familiar async/await patterns with a high-performance async runtime.
    *   **Compile-Time Power:** Integrated compile-time function execution (`comptime`) for metaprogramming, configuration, and optimization.
    *.  **Excellent Tooling:** Provide a seamless experience out-of-the-box, including a fast compiler, integrated package manager, REPL, and testing framework.

*   **Target Audience:** Developers familiar with languages like Python, Go, TypeScript, or C# seeking better performance and stronger safety guarantees without the steep learning curve of Rust or the runtime overhead of GC languages, especially for backend services, CLI tools, and scripting.

### Language Inspirations

Ryo synthesizes ideas from several modern programming languages:

*   **Python** - Clean syntax with colons and indentation, f-strings, type inference, async/await, developer-friendly design
*   **Rust** - Ownership model for memory safety, algebraic data types (enums with data), pattern matching, trait system, Result/Option types
*   **Mojo** - Simplified ownership without manual lifetimes, value semantics, progressive complexity model
*   **Go** - Simplicity as a core design principle, fast compilation, built-in concurrency primitives, minimal feature set
*   **Zig** - Comptime execution for zero-cost abstractions, explicit error handling, no hidden control flow, minimal runtime

**Key Differentiators**: Ryo aims to be easier than Rust (no lifetimes), safer than Python (compile-time memory safety), more expressive than Go (generics, algebraic types), and more familiar than Zig (Python-like syntax).

## 2. Lexical Structure

*   **Encoding:** Source files are UTF-8 encoded, allowing for Unicode characters in strings and potentially identifiers (if identifier rules are expanded later).
*   **Identifiers:** `[a-zA-Z_][a-zA-Z0-9_]*`. Case-sensitive.
    *   *Convention:* Follow `snake_case` for variables, functions, and modules. Use `PascalCase` for types (structs, enums, traits) and enum variants. *(Rationale: Adopting common conventions enhances readability and aligns with practices in Python and Rust).*
*   **Keywords:** `fn`, `struct`, `enum`, `trait`, `impl`, `mut`, `if`, `elif`, `else`, `for`, `in`, `return`, `break`, `continue`, `import`, `match`, `pub`, `true`, `false`, `async`, `await`, `move`. (Note: `Result`, `Optional`, `Ok`, `Err`, `Some` are built-in types resolved by the type checker, not keywords. `comptime`, `unsafe` are planned for future implementation. `as`, `default`, `package`, `None`, `let` are not keywords).
*   **Operators:** Standard set including arithmetic (`+`, `-`, `*`, `/`, `%`), comparison (`==`, `!=`, `<`, `>`, `<=`, `>=`), logical (`and`, `or`, `not`), assignment (`=`), type annotation (`:`), scope/literal delimiters (`{`, `}`, `[`, `]`, `(` `)`), access (`.`), error propagation (`?`).
    *   `_` (Underscore): The underscore `_` is treated as a special identifier. When used in patterns (`match`, destructuring assignment), it signifies a wildcard or an intentionally ignored value; it does not bind to a variable.
*   **Literals:** Integers (decimal `123`, hex `0xFF`, octal `0o77`, binary `0b11`; underscores `1_000`), Floats (`123.45`, `1.23e-10`; underscores `1_000.0`), Strings (`"..."` basic escapes like `\n`, `\t`, `\\`, `\"`, `\xHH`, `\u{HHHH}`). `f"..."` (f-strings with `{expression}` interpolation), Booleans (`true`, `false`), List (`[...]`), Map (`{key: value, ...}`), Tuple (`(v1, v2, ...)`), Char (`'a'`, `'\u{1F600}'`).
*   **Comments:**
    *   **Regular Comment:** Starts with `#` followed by a space or directly by the comment text. Continues to the end of the line. Ignored by the compiler.
        ```ryo
        # This is a comment
        #Another comment
        x = 1 # Comment after code
        ```
    *   **Documentation Comment:** Starts with the specific sequence `#:` (hash symbol immediately followed by a colon). Continues to the end of the line. Processed by documentation tooling (supports Markdown). Ignored otherwise by the compiler. Applies to the item immediately following it. Consecutive `#:` lines form a single documentation block.
        ```ryo
        #: Represents a point in 2D space.
        #: Supports basic arithmetic.
        struct Point:
            x: int #: X coordinate (doc comment for field)
            y: int # Regular comment for field

        #: Calculates the distance from the origin.
        fn distance(p: &Point) -> float:
            ...
        ```
    *   *(Rationale: Uses `#` as the base. The `#:` marker provides an unambiguous distinction for documentation tooling, avoiding whitespace sensitivity and block comment syntax. Attributes `#[...]` remain separate).*
*   **Attributes:** Metadata annotations use the `#[...]` syntax, placed before the documented item. *(Rationale: Distinct syntax using brackets clearly separates attributes from code and comments).*
*   **Indentation:** **Tabs** strictly denote code blocks. One tab per indentation level. Mixing tabs and spaces for indentation is a compile-time error. *(Rationale: Enforces a single, consistent style like Go, avoids common Python indentation issues).*
*   **Statements:** Generally one per line; semicolons are not required or used.

## 3. Syntax & Grammar

*   *(Note: A formal grammar (EBNF) is required for full implementation but omitted here).*
*   **Function Definition:** `fn name(param: Type, ...) -> RetType: ...`
*   **Variable Declaration:** `var = val`, `mut var = val`, `var: Type = val`, `mut var: Type = val`. *(Rationale: Pythonic feel for common `var = val`, explicitness available, `mut` required for mutation).*
*   **Struct Definition:** `struct Name: field: Type ...`
*   **Enum Definition:** `enum Name: Variant1, Variant2(Type), Variant3 { field: Type } ...`
*   **Trait Definition:** `trait Name: fn method(...) -> RetType ... { /* optional default */ }`
*   **Implementation:** `impl Trait for Type: fn method(...) -> RetType: ...`
    ```ryo
    struct Counter: 
        count: int
    trait Resettable:
        fn reset(&mut self)
    impl Resettable for Counter:
        fn reset(&mut self): self.count = 0
    ```
*   **Method Call:** `instance.method(args...)`. Field Access: `instance.field`.
*   **Control Flow:** `if/elif/else`, `for item in iterable:`, `for i in range(start, end):`.
*   **Pattern Matching:** `match expr: Pattern1: ... Pattern2(bind): ... Pattern3 { x, y }: ... _ : ...` (`_` for wildcard/default).

*   **Async/Await:** `async fn name() -> RetType:`, `await expression`,
    ```ryo
    async fn fetch_data() -> Result[Data, Error]:
        response = await http.get("https://api.example.com/data")
        data = await response.json[Data]()
        return Ok(data)
    ```
*   **Closures:** `fn(args): expression`.
*   **Tuple Destructuring:** `(a, b) = my_tuple`.
*   **Type Conversion Syntax:** Uses function-call style `TargetType(value)` for explicit, safe conversions (primarily numeric and compatible types). *(Rationale: Explicit, uses type name directly like Go, avoids `as` keyword ambiguity, separates safe/unsafe casts clearly).*

## 4. Types

### 4.1 Static Typing & Inference

*   **Static Typing:** Checked at compile time. Enhances safety and enables performance optimizations.
*   **Type Inference:** Limited to variable declarations (`var = val`). Explicit type annotations are required for function signatures, struct fields, enum variant data, and potentially complex literals to maintain clarity. *(Rationale: Balances Pythonic convenience for local variables with the clarity and safety benefits of explicit types in definitions and interfaces).*

### 4.2 Primitive Types

*   `int`: Defaults to `isize` (signed pointer-sized integer).
*   `float`: Defaults to `float64` (64-bit IEEE 754 float).
*   `bool`: `true`, `false`.
*   `str`: Owned, growable, heap-allocated, UTF-8 string. Mutable if bound `mut`. *(Rationale: Provides a primary, easy-to-use string type. Mutability controlled by binding aligns with general variable mutability).*
*   `char`: Unicode Scalar Value. Literal: `'a'`.
*   Explicit Sizes: `i8`-`i64`, `u8`-`u64`, `usize`, `float32`. *(Rationale: Necessary for control over representation, performance, and FFI).*

### 4.3 Tuple Type

*   **Tuple Type:** `(T1, T2, ...)`. Literal `(v1, v2, ...)`. Access `.0`, `.1`, etc. Destructuring. *(Rationale: High Pythonic familiarity. Ergonomic for returning multiple values and simple ad-hoc grouping without needing named structs).*

### 4.4 Slice Types (Borrowed Views)


*   `&str`: Borrowed, immutable UTF-8 view (pointer + byte length). Created via `my_str[start_byte..end_byte]`, `my_str.as_slice()`, or from literals. Lifetime tied to borrowed data.
*   `&[T]`: Borrowed, immutable slice of `T` elements (pointer + element length). Created via `my_list[start..end]`, `my_list.as_slice()`.
*   `&mut [T]`: Borrowed, *mutable* slice of `T` elements. Created via `my_mut_list.as_mut_slice()`. Requires `mut` borrow of source.
*   *(Rationale: `&` syntax leverages borrow concept. No `&mut str` initially simplifies UTF-8 safety. Slices provide efficient read-only/mutable views without copying).*

### 4.5 Struct Type (Product Type)

*   User-defined data aggregation: `struct Name: field: Type ...`.
*   Instances created via struct literals: `Name { field: value, ... }`.
*   Access via dot notation: `instance.field`. Mutable if instance bound `mut`.

### 4.6 Enum Type (Sum Type / Algebraic Data Type - ADT)

*   **Concept:** Defines a type that can be exactly *one* of several named **variants**. Each variant can optionally hold associated data. Enums are fundamental for representing alternatives, states, and structured data safely.
*   **Syntax:**
    ```ryo
    enum EnumName[T]: # Optional type parameters for generics
        UnitVariant             # Variant with no data
        TupleVariant(Type1, Type2) # Variant holding ordered data
        StructVariant { name1: TypeA, name2: TypeB } # Variant holding named fields
    ```
*   **Instantiation:** Use `EnumName.VariantName`. Provide data for tuple/struct variants.
    ```ryo
    msg1 = Message.Quit
    msg2 = Message.Write("hello")
    msg3 = Message.Coords { x: 10, y: -5 }
    ```
*   **Pattern Matching (`match`):** The primary way to use enum values. `match` destructures variants and allows executing code based on the current variant.
    ```ryo
    match my_enum_value:
        MyEnum.Variant1:
            # Code for Variant1
        MyEnum.TupleVariant(data1, data2): # Bind tuple data
            # Code using data1, data2
        MyEnum.StructVariant { field_a, count }: # Bind struct fields
            # Code using field_a, count
        _ : # Wildcard for unlisted variants (required if not exhaustive)
            # Default code
    ```
*   **Exhaustiveness:** The compiler **enforces** that `match` expressions handle *all* possible variants of an enum, preventing runtime errors from unhandled cases. A wildcard `_` can be used to satisfy exhaustiveness if not all variants are explicitly matched. *(Rationale: Core safety feature, eliminates bugs from missed cases).*
*   **Ownership:** Enum values follow standard ownership rules. An enum value owns any data contained within its current variant. Moving the enum moves the contained data. Destructuring in `match` can move or borrow contained data based on the pattern.
*   **Methods:** Methods can be defined on enums using `impl EnumName: ...`, often using `match self:` internally.
    ```ryo
    impl MyEnum:
        fn process(self):
            match self:
                MyEnum.Variant1: io.println("Processing V1")
                # ... other variants ...
    ```
*   *(Rationale: Enums provide type-safe ways to represent alternatives (like `Result`/`Optional`), states, and structured messages, crucial for robust software and eliminating `null` errors. Exhaustive matching is a key safety feature derived from functional programming and Rust).*

### 4.7 Built-in Collections

*   `List[T]`: Dynamic array. Homogeneous. *(Built-in generic type)*
*   `Map[K, V]`: Hash map. Homogeneous keys/values. `K` must be hashable/comparable. *(Built-in generic type)*

*Note: User-defined generics are planned for future implementation. See [Language Proposals](proposals.md#advanced-generics) for detailed generic type system design.*

### 4.8 Standard Library Types (`Result`, `Optional`)

*   `Result[T, E]`: Built using `enum { Ok(T), Err(E) }`.
*   `Optional[T]`: Built using `enum { Some(T), None }`. Replaces `null`.
    *   Uses the variant None (accessed as `Optional.None`). Note that `None` itself is not a global keyword, but the specific identifier for this variant within the `Optional` enum, aligning with common practice in languages like Rust for null safety.
    *   *(Rationale: Explicit handling of absence/errors via ADTs is safer than nullable types or exceptions).*

### 4.9 FFI Types

*   **Note:** FFI types are planned for future implementation. See [Language Proposals](proposals.md#foreign-function-interface-ffi--unsafe-code) for detailed design.

### 4.10 Type Conversion Syntax

*   Uses function-call style `TargetType(value)` for explicit, safe conversions (primarily numeric and compatible types). *(Rationale: Explicit, uses type name directly like Go, avoids `as` keyword ambiguity, separates safe/unsafe casts clearly).*


## 5. Memory Management: "Ownership Lite"

*   **No Garbage Collector.** Provides deterministic performance and resource management.
*   **Core Principle:** Simplified Ownership & Borrowing, inspired by Rust but aiming for lower complexity.
    *   **Ownership:** Single owner responsible for deallocation.
    *   **Move Semantics (Default):** Assignment (`new = old`), return, and passing owned arguments to functions (that don't declare borrows) *moves* ownership. The original variable (`old`) becomes invalid after the move (compile-time check). *(Rationale: Prevents accidental aliasing of owned mutable data. Provides clear resource responsibility transfer. This is the foundational rule).*
    *   **Borrowing:** Grants temporary access without transferring ownership.
        *   **Implicit Immutable Borrow (Default Function Params):**
            *   Function parameters are *implicitly* treated as immutable borrows (`&Type`) unless marked `mut` or `move`. *(Wording slightly adjusted for clarity)*
            *   **Important Distinction:** This default implicit borrow for function arguments *contrasts* with the default *move* semantics for assignment and return values. This choice prioritizes ergonomics for the common case of read-only function access (enhancing Pythonic feel) over strict uniformity. Developers must be aware that `my_func(my_var)` typically borrows `my_var` immutably (leaving `my_var` valid), while `let new_var = my_var` moves `my_var` (invalidating `my_var`). The compiler must provide clear error messages when ownership rules are violated due to this distinction.
            *   *(Example Added)*
                ```ryo
                fn process_data(data: &SomeType) { # Explicit borrow, same effect as implicit
                    # ... read data ...
                }
                fn read_data(data: SomeType) { # Implicit immutable borrow
                    # ... read data ...
                }
                fn consume_data(move data: SomeType) { # Explicit move (alternative to default borrow)
                    # ... takes ownership ...
                }
                fn main():
                    my_data = SomeType { ... }
                    read_data(my_data) # Implicitly borrows my_data, my_data still valid here
                    # process_data(&my_data) # Explicit borrow, my_data still valid

                    moved_data = my_data # MOVES my_data, my_data is now INVALID
                    # read_data(my_data) # Compile Error: Use of moved value 'my_data'

                    another_data = SomeType { ... }
                    consume_data(move another_data) # MOVES another_data, another_data is now INVALID
                ```
        *   **Explicit Mutable Borrow (`mut` Keyword):** `mut param: Type` requires a mutable borrow (`&mut Type`). The variable passed *must* be declared `mut`. *(Rationale: Makes mutation intent explicit. Tying it to variable declaration simplifies reasoning compared to call-site mutability markers).*
        *   **Explicit Move (`move` Keyword on Param):** `move param: Type` explicitly enforces move semantics for a function parameter, overriding the implicit borrow default. *(Added for completeness)*
        *   **Lifetime Inference:** Lifetimes are inferred by the compiler based primarily on **lexical scopes**. Borrows are valid only within the scope where they are created and cannot outlive the owner. **No manual lifetime annotations (`'a`)**. *(Rationale: Core simplification vs Rust, crucial for approachability).*
            *   This simplification means some complex borrowing patterns possible in languages with manual lifetime annotations may not be directly expressible or may require different structuring (e.g., returning owned data instead of borrows, using reference counting). It prioritizes approachability over maximum flexibility.
        *   **Borrowing Rules (Compile-Time Enforced):**
            1.  At any point in time, a variable can have *either*: One or more **immutable borrows** OR Exactly **one mutable borrow**.
            2.  Borrows must not outlive the owner's scope.
            3.  A mutable borrow cannot exist simultaneously with any other borrow (mutable or immutable) to the same variable within the same or overlapping scopes.
        *   **Collection Borrowing Rules:** Mutable borrow of collection prevents *any* other borrows to the collection *or its elements*. Reallocation may invalidate element borrows (compiler tracked). *(Rationale: Prioritizes safety and implementation simplicity over fine-grained element borrowing initially, preventing iterator invalidation and element dangling pointer issues, This rule simplifies safety by preventing issues like iterator invalidation, potentially being refined in future versions if safe fine-grained borrowing patterns are identified without significantly increasing complexity).*
*   **RAII (`Drop` Trait):**
    *   `impl Drop for Type: fn drop(self): ...`. Automatic cleanup on scope exit for owned values. Drop order is reverse declaration order within scope.
    *   Errors in `drop` cannot propagate; must not panic. Use explicit methods for fallible cleanup. *(Rationale: Ensures deterministic, non-failing scope exit critical for resource safety).*
*   **Shared Ownership:** `Shared[T]` (ARC) / `Weak[T]` provided in stdlib (e.g., `sync` module) for opt-in shared ownership and cycle breaking. API uses dot notation (`Shared.new`, `ref.clone`, `ref.downgrade`, `weak_ref.upgrade`). *(Rationale: Provides necessary mechanism for shared state and cyclic data when single ownership is insufficient, while making the associated overhead (ARC) explicit).*

## 6. Functions & Closures

*   **Functions/Methods:** Standard definition/call. Return single value (can be tuple). Methods use `&self` (immutable borrow), `&mut self` (mutable borrow), or `self` (take ownership).
*   **Closures:** Anonymous functions `fn(args): expression`.
    *   **Capture:** Default immutable borrow. `move fn` captures by move. Mutable borrow inferred on mutation (requires original `mut`). Compiler checks rules. *(Rationale: Provides explicit control over captures, crucial for safety with `spawn` and non-escaping closures).*
    *   **Conceptual Types:** `Fn`, `FnMut`, `FnMove` describe capabilities, guiding type checking for functions accepting closures. *(Rationale: Defines closure behavior without full initial trait complexity).*

## 7. Error Handling

*   **Recoverable:** `Result[T, E]` (`Ok`, `Err`). Mandatory handling (via `match`, `?`, methods like `.unwrap_or()`). *(Rationale: Explicit error handling prevents ignored errors).*
*   **Propagation:** `?` operator unwraps `Ok` or returns `Err` early. *(Rationale: Highly ergonomic, standard pattern in Rust/Swift).* Error type compatibility for `?` across different error types relies on a conversion mechanism (like a standard `From` trait or similar), which is planned for detailed specification.
*   **Unrecoverable:** `panic("message")`. **Aborts** process by default. *(Rationale: Simplest, safest default. Avoids unwind complexity).*

## 8. Traits (Behavior)

*   **Definition:** `trait Name: fn method(...) ... { /* optional default impl */ }`. Default methods allowed. *(Rationale: Default methods reduce boilerplate).*
*   **Implementation:** `impl Trait for Type: fn method(...) ...`. Can override defaults.
*   **Dispatch:** **Static Dispatch** via monomorphization only (initially). *(Rationale: Prioritizes runtime performance and implementation simplicity).* No dynamic dispatch (`dyn Trait`).
    *   This means polymorphism is primarily achieved through generics (compile-time polymorphism). Patterns requiring runtime dynamic dispatch (common in some OOP/dynamic languages) will need alternative approaches in Ryo, such as using enums with associated data to represent variants or passing function pointers/closures.
    *   **Future Extension:** Dynamic dispatch via trait objects (e.g., `&dyn Trait`) is planned for future versions to enable more flexible polymorphism patterns familiar to Python developers. See [Language Proposals](proposals.md#dynamic-dispatch-trait-objects) for details.
*   **Associated Types:** Not supported initially. *(Rationale: Significant type system complexity).*

## 9. Concurrency Model: Async/Await

*   **Model:** Cooperative concurrency using async/await with a high-performance runtime. Familiar to Python developers while maintaining memory safety.
*   **Primitives:**
    *   `async fn`: Declares an asynchronous function that returns a future.
    *   `await`: Suspends execution until the awaited future completes.
    *   **Async Runtime:** Built-in runtime handles task scheduling, I/O operations, and timers.
    *   **Ownership Integration:** Async functions work seamlessly with Ryo's ownership model - values can be moved into async contexts safely.
*   **Examples:**
    ```ryo
    async fn process_request(req: Request) -> Result[Response, Error]:
        data = await database.query("SELECT * FROM users")?
        result = await external_api.call(data)?
        return Ok(Response.json(result))

    async fn process_all_requests():
        tasks = [
            process_request(req1),
            process_request(req2),
            process_request(req3)
        ]
        results = await async.gather(tasks)
        print(f"Processed {results.len()} requests")

    fn main():
        # Start async runtime and run async code
        async_runtime.run(process_all_requests())
    ```
*   *(Rationale: Async/await is familiar to Python developers, provides excellent ergonomics for I/O-bound applications, and integrates well with Ryo's ownership model. The cooperative nature prevents many concurrency bugs while maintaining high performance).*
*   **Future Extensions:** CSP-style channels (`chan[T]`, `select`) planned as optional additions for specialized use cases. See [Language Proposals](proposals.md#concurrency-extensions-csp-communicating-sequential-processes) for detailed CSP design.

## 10. Compile-Time Execution (`comptime`)

*   **Note:** Compile-time execution is planned for future implementation. See [Language Proposals](proposals.md#compile-time-execution-comptime) for detailed design.

## 11. Modules & Packages

*   **Implicit Packaging:** Directory structure under `src/` defines hierarchy. No `package` keyword. *(Rationale: Pythonic simplicity, reduces boilerplate).*
*   **Imports:** `import path.to.module`, `import path.{item}`, `import pkg:dep_name`, `import path as alias`. Paths relative to `src/`.
*   **Visibility:** Default private, `pub` for public.
*   **Cycles:** Disallowed.

## 12. Application Entry Point

*   **Convention:** Default entry point file is `src/main.ryo`.
*   **`fn main()`:** Required in entry point (`fn main()` or `fn main() -> Result[(), ErrType]`).
*   **Compiler Enforcement:** `fn main()` only allowed in the designated entry point file for executable compilation. *(Rationale: Clear convention without needing `package main` keyword).*

## 13. Package Manager (`ryopkg`)

*   **Manifest:** `ryo.toml`. Defines metadata, dependencies.
*   **Registry:** `ryopkgs.io` (hypothetical).
*   **Versioning:** SemVer enforced.
*   **Locking:** `ryo.lock` for reproducible builds.
*   **CLI Tool:** Cargo-inspired commands (`new`, `add`, `install`, `build`, `run`, `test`, `publish`, `update`, `lock`). *(Rationale: Proven, robust model for ecosystem management).*

## 14. Standard Library

*   **Philosophy:** Modular packages, practical, ergonomic, safe, reasonably "batteries-included" for web/scripting.
*   **Structure:** Composed of distinct packages (e.g., `io`, `string`, `collections`, `net.http`, `ffi`). Users import only needed packages. *(Rationale: Reduces binary size, improves compile times, makes dependencies explicit).*
*   **Core Packages (Initial):**
    *   `core`/`builtin` (Implicit): Fundamental types (`Result`, `Optional`), core traits (`Drop`, `Length` for `.len(self)`), built-in functions (`print`, `println`, `panic`).
    *   `io`: Console (`readln`), Files (`File`), Buffering (using `Result`, implements `Drop`).
    *   `string`: `&str` manipulation, parsing (`parse_* -> Result`).
    *   `collections`: `List[T]`, `Map[K, V]` types and methods.
    *   `math`: Functions, constants, explicit overflow methods.
    *   `time`: `Instant`, `SystemTime`, `Duration`.
    *   `encoding.json`: `encode -> Result[str]`, `decode -> Result[JsonValue]`, `decode_into[T] -> Result[T]`.
    *   `net.http`: Async Client/Server primitives (`Request`, `Response`, async handlers).
    *   `os`: Env, args, basic filesystem ops (using `Result`).
    *   `testing`: `#[test]` attribute, `assert()`, `assert_eq()`. *(Planned)*
    *   `sync`: `Shared[T]`/`Weak[T]` types for optional shared ownership.
    *   `mem`: Basic memory utilities, `Drop` trait definition.
    *   `utf8`: Utilities for `str`/`&str` validation, char iteration.

## 15. Tooling

*   **Compiler Backend:** **Cranelift**. Supports AOT, JIT, WebAssembly. *(Rationale: Good balance of performance, compile speed, JIT/Wasm support).*
*   **Tools:** `ryo` package manager integrated, `ryo` REPL (using JIT), Integrated Testing (`ryo test`). LSP future goal.

## 16. FFI & `unsafe`

*   **Note:** Foreign Function Interface and unsafe operations are planned for future implementation. See [Language Proposals](proposals.md#foreign-function-interface-ffi--unsafe-code) for detailed design.

## 17. Integer Overflow Behavior

*   **Default:** Panic (debug), Wrap (release). *(Rationale: Balance safety during dev with performance in release).*
*   **Explicit Methods:** `checked_* -> Optional`, `wrapping_*`, `saturating_*` (on types or in `math`).
*   **Division by Zero:** Always panics.
*   **Numeric Conversions (`TargetType(value)`):** Safe, explicitly defined behavior (widening ok, float->int truncates towards zero, narrowing int wraps/truncates). Does *not* require `unsafe`. This defined behavior ensures portability and avoids undefined behavior common in some other languages for certain conversions.

## 18. Missing Elements / Future Work

For detailed future features and extensions, see the dedicated [Language Proposals](proposals.md) document.

**Current Specification Gaps:**
*   **Formal Grammar (EBNF/BNF).**
*   **Detailed Standard Library API Specification** (All function signatures, struct fields, detailed semantics).
*   **Precise Borrow Checker Algorithm Specification** (Formal lifetime inference/validation rules, edge cases).
*   **Precise Closure Representation/ABI** (Memory layout, FFI compatibility).
*   **Error Handling Details** (Standard `Error` trait? `From` trait for `?` conversions?).
*   **Module System Edge Cases** (Detailed resolution rules, visibility across modules/packages).
*   **Attributes:** Formal system for attributes like `#[test]`, `#[no_mangle]`, `#[repr(C)]`.
*   **Memory Layout Guarantees** (Beyond `#[repr(C)]`).
*   **WebAssembly Target Details** (ABI, JS interop bindings, WASI support).

**Planned Future Extensions (see [proposals.md](proposals.md)):**
*   **Compile-Time Execution** (`comptime` blocks and functions)
*   **Foreign Function Interface & Unsafe Operations** (C FFI, raw pointers, unsafe blocks)
*   **CSP Concurrency Extensions** (channels, select, spawn - optional)
*   **Advanced Generics** (User-defined generics with trait bounds)
*   **Iterator System** (Standard iterator traits and lazy evaluation)
*   **Standard Error Trait** (Unified error handling with conversions)
*   **Attribute System** (Formal `#[attribute]` syntax)
*   **Advanced String Formatting** (Display/Debug traits)
*   **Dynamic Dispatch** (Trait objects for runtime polymorphism)
*   **Advanced Pattern Matching** (Guards, OR patterns, advanced destructuring)
*   **Advanced Compile-Time Reflection** (Type introspection and code generation)
*   **SIMD Support** (Vector operations)
*   **Module System Extensions** (Conditional compilation)

## See Also

- **[Code Examples](examples/)** - Practical examples demonstrating the features described in this specification
- **[Getting Started Guide](getting_started.md)** - Step-by-step tutorial for learning Ryo
- **[Standard Library](std.md)** - Built-in functions and modules available in Ryo programs

---
