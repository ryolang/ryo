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
*   **Keywords:** `fn`, `struct`, `enum`, `trait`, `impl`, `mut`, `if`, `elif`, `else`, `for`, `in`, `return`, `break`, `continue`, `import`, `match`, `pub`, `true`, `false`, `none`, `async`, `await`, `move`, `error`, `try`, `catch`, `orelse`. (Note: `comptime`, `unsafe` are planned for future implementation. `as`, `default`, `package`, `let` are not keywords).
*   **Operators:** Standard set including arithmetic (`+`, `-`, `*`, `/`, `%`), comparison (`==`, `!=`, `<`, `>`, `<=`, `>=`), logical (`and`, `or`, `not`), assignment (`=`), type annotation (`:`), scope/literal delimiters (`{`, `}`, `[`, `]`, `(` `)`), access (`.`), error union prefix (`!`), optional chaining (`?.`).
    *   **Important Note:** The `!` operator is used exclusively for error union type prefixes (`!T` = error or T, `ErrorType!T` = ErrorType or T). The `!` is NOT used for logical negation—use `not` instead (following Python convention). Similarly, `?` operator in type context (`?T`) denotes optional types, while `?.` is the optional chaining operator.
    *   `_` (Underscore): The underscore `_` is treated as a special identifier. When used in patterns (`match`, destructuring assignment), it signifies a wildcard or an intentionally ignored value; it does not bind to a variable.
*   **Literals:** Integers (decimal `123`, hex `0xFF`, octal `0o77`, binary `0b11`; underscores `1_000`), Floats (`123.45`, `1.23e-10`; underscores `1_000.0`), Strings (`"..."` basic escapes like `\n`, `\t`, `\\`, `\"`, `\xHH`, `\u{HHHH}`). `f"..."` (f-strings with `{expression}` interpolation), Booleans (`true`, `false`), Optional null value (`none`), List (`[...]`), Map (`{key: value, ...}`), Tuple (`(v1, v2, ...)`), Char (`'a'`, `'\u{1F600}'`).
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
*   **Variable Declaration:** Variables are **immutable by default** and do not require a keyword. Use `mut` for mutable variables.
    *   Immutable: `name = value` (type inferred)
    *   Immutable with explicit type: `name: Type = value`
    *   Mutable: `mut name = value` (type inferred)
    *   Mutable with explicit type: `mut name: Type = value`
    *   Examples:
        ```ryo
        pi = 3.14                    # Immutable float (type inferred)
        name = "Alice"               # Immutable string (type inferred)
        count: int = 42              # Immutable int (explicit type)
        mut counter = 0              # Mutable integer (type inferred)
        mut temperature: float = 98.6 # Mutable float (explicit type)
        ```
    *   *(Rationale: Immutable-by-default promotes safer code. No `let` keyword provides Pythonic simplicity. Type inference reduces boilerplate while explicit types remain available for clarity. The `mut` keyword makes mutability explicit and visible).*
    *   **Type Inference:** Ryo uses **bidirectional type checking** (like Rust, TypeScript, and modern statically-typed languages) which provides:
        *   **Function signatures require type annotations** - Good for documentation and API clarity
        *   **Local variables inferred from initialization** - Ergonomic for local code
        *   **Better, localized error messages** - More understandable than full Hindley-Milner
        *   **Simpler implementation** - More practical than complete HM type inference
        *   **Comptime with enhanced inference** - More aggressive type inference in compile-time contexts
        *   Examples:
            ```ryo
            fn add(a: int, b: int) -> int:  # Parameters need types
                result = a + b              # Local variable type inferred: int
                return result               # Return type checked against signature

            # Type errors are localized and clear
            x = 5              # Inferred: int
            y = 3.14           # Inferred: float
            z = x + y          # Error: cannot add int and float (clear, localized)
            ```
        *   *(Rationale: Bidirectional type checking provides the right balance - function signatures serve as documentation and API contracts while local code remains concise. This matches developer expectations from Rust/TypeScript and provides better error messages than fully implicit systems like Hindley-Milner).*
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
    error NetworkError:
        ConnectionFailed
        TimeoutError

    async fn fetch_data() -> NetworkError!Data:
        response = try await http.get("https://api.example.com/data")
        data = try await response.json[Data]()
        return data
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

### 4.8 Optional Types (`?T`)

*   **Syntax:** `?T` represents a value of type `T` that may be absent (represented by `none`). Eliminates null pointer errors through explicit, type-safe handling.
*   **Null literal:** `none` (lowercase keyword, consistent with `true` and `false`). *(Rationale: Python-familiar, semantically clear—"none" means "no value")*
*   **Declaration and Assignment:**
    ```ryo
    user: ?User = none
    user: ?User = User{name: "Alice"}

    config: ?Config = load_config()  # If load_config returns ?Config
    ```
*   **Optional Chaining (`?.`):** Access nested optional fields without explicit unwrapping. Returns an optional type if any step is `none`:
    ```ryo
    city = user?.profile?.address?.city  # Returns ?str

    # Equivalent to (conceptually):
    city = if user != none and user.profile != none and user.profile.address != none:
        user.profile.address.city
    else:
        none
    ```
*   **Default Values with `orelse`:** Provide defaults or early return:
    ```ryo
    name = user?.name orelse "Unknown"
    port = config?.port orelse 8080

    # Early return pattern (with smart casting)
    user = optional_user orelse return error.NoUser
    # 'user' is now User (not ?User)
    ```
*   **Smart Casting after Null Checks:** After a null check, the type is automatically narrowed:
    ```ryo
    if user != none:
        print(user.name)  # user is User here, not ?User (smart cast)

    if let user = optional_user:  # Future: if-let syntax
        print(user.name)
    ```
*   **Pattern Matching:**
    ```ryo
    match optional_user:
        none: print("No user")
        else: print(f"User: {optional_user.name}")
    ```
*   *(Rationale: Zig-inspired `?T` syntax is concise and compositional. The `none` keyword aligns with Python's `None`. Optional chaining (`?.`) and `orelse` provide ergonomic handling. Smart casting reduces boilerplate after validation)*

### 4.9 Error Types (`ErrorType!SuccessType`)

*   **Purpose:** Error types are algebraic data types specifically designed for error handling. Use the `error` keyword to define error types with associated data.
*   **Definition Syntax:**
    ```ryo
    error IoError:
        FileNotFound(path: str)
        PermissionDenied(path: str)
        DiskFull

    error ParseError:
        InvalidSyntax(line: int, column: int)
        UnexpectedToken(expected: str, got: str)
        UnexpectedEof
    ```
*   **Variants with Associated Data:** Error variants can carry data (tuple or struct variants):
    ```ryo
    error DatabaseError:
        ConnectionFailed(reason: str)
        QueryTimeout(Duration)
        InvalidQuery(sql: str, position: int)
    ```
*   **Error Union Type (`ErrorType!T`):** Represents a value that can be the error type or the success type:
    ```ryo
    fn read_file(path: str) -> IoError!str:
        if not exists(path):
            return IoError.FileNotFound(path)
        return os.read(path)
    ```
*   **Generic Error Type (`!T`):** When error type is not specified, `!T` means "any error or T":
    ```ryo
    fn flexible_operation() -> !Data:
        # Can return any error type
        ...
    ```
*   **Combined Error and Optional (`!?T`):** For operations that can fail (error), return no value (`none`), or return a value:
    ```ryo
    fn find_user(db: Database, id: int) -> IoError!?User:
        # Can return: IoError, none (not found), or User
        rows = try db.query("SELECT * FROM users WHERE id = ?", id)
        if rows.is_empty():
            return none
        return User.from_row(rows[0])
    ```
*   **Pattern Matching on Errors:** Use full ADT pattern matching to handle different error variants:
    ```ryo
    result = load_config() catch |e|:
        match e:
            ParseError.InvalidSyntax(line, col):
                print(f"Syntax error at {line}:{col}")
            ParseError.UnexpectedToken(exp, got):
                print(f"Expected {exp}, got {got}")
            ParseError.UnexpectedEof:
                print("Unexpected end of file")
    ```
*   *(Rationale: `error` keyword signals error-handling intent. Associated data enables rich error information. Zig-style `E!T` syntax is concise. Full pattern matching provides type-safe error handling)*

### 4.11 FFI Types

*   **Note:** FFI types are planned for future implementation. See [Language Proposals](proposals.md#foreign-function-interface-ffi--unsafe-code) for detailed design.

### 4.12 Type Conversion Syntax

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

Error handling in Ryo uses algebraic error types (defined with the `error` keyword) combined with the `try` and `catch` operators for type-safe, explicit error management.

### 7.1 Error Types and Definitions

Error types are defined with the `error` keyword and support variants with associated data:

```ryo
error NetworkError:
    ConnectionTimeout
    DnsResolutionFailed(domain: str)
    HttpError(status: int, message: str)

error FileError:
    NotFound(path: str)
    PermissionDenied(path: str)
    ReadFailed(reason: str)
```

*   *(Rationale: `error` keyword signals error-handling intent. Associated data enables rich error information.)*

### 7.2 Error Union Types (`ErrorType!T`)

Function return types specify both the error type and success type using the `ErrorType!SuccessType` syntax:

```ryo
fn read_file(path: str) -> FileError!str:
    if not exists(path):
        return FileError.NotFound(path)
    return os.read(path)

# Flexible error type - can return any error
fn flexible_operation() -> !Data:
    ...
```

*   *(Rationale: Zig-style `E!T` syntax is concise and reads naturally. Error type is explicitly documented in function signature.)*

### 7.3 Error Propagation (`try`)

The `try` keyword unwraps success or propagates the error early:

```ryo
fn load_and_parse(path: str) -> !Config:
    # Both try expressions propagate errors
    content = try read_file(path)      # FileError propagates
    config = try parse_config(content) # ParseError propagates
    return config
```

*   **Semantic:** `try expr` evaluates `expr`. If it succeeds, the value is returned. If it fails (error), the error is propagated to the caller.
*   *(Rationale: `try` clearly signals error propagation. Familiar to async/await users.)*

### 7.4 Error Handling (`catch`)

The `catch` operator handles errors with pattern matching:

```ryo
config = load_and_parse("app.toml") catch |e|:
    match e:
        FileError.NotFound(path):
            print(f"Creating default config at {path}")
            return default_config()

        ParseError.InvalidSyntax(line, col):
            print(f"Syntax error at {line}:{col}")
            exit(1)
```

*   **Syntax:** `expr catch |e|: handle_error(e)`
*   **Pattern Matching:** Full ADT pattern matching enables type-safe error handling.
*   *(Rationale: `catch` follows familiar error-handling conventions. Full pattern matching prevents missed error cases.)*

### 7.5 Error Conversion (`From` Trait)

The `From` trait enables automatic error conversion with the `try` operator:

```ryo
trait From[T]:
    fn from(value: T) -> Self

impl From[FileError] for AppError:
    fn from(err: FileError) -> AppError:
        return AppError.File(err)

impl From[ParseError] for AppError:
    fn from(err: ParseError) -> AppError:
        return AppError.Parse(err)

# With From trait implementations, try automatically converts errors
fn high_level() -> AppError!Data:
    content = try read_file("data.txt")
    # FileError automatically converted to AppError via From trait

    parsed = try parse_data(content)
    # ParseError automatically converted to AppError via From trait

    return process(parsed)
```

*   **Automatic Conversion:** When `try` encounters an error, it attempts to convert it using the `From` trait.
*   *(Rationale: Eliminates boilerplate error conversion. Standard pattern from Rust.)*

### 7.6 Combined Error + Optional (`!?T`)

For operations that can fail (error), return no value (`none`), or succeed:

```ryo
fn find_user(db: Database, id: int) -> DatabaseError!?User:
    # Can return: DatabaseError, none (not found), or User
    rows = try db.query("SELECT * FROM users WHERE id = ?", id)
    if rows.is_empty():
        return none
    return User.from_row(rows[0])

# Sequential unwrapping pattern
fn authenticate(db: Database, token: ?str) -> !User:
    t = token orelse return error.MissingToken
    # t is now str (smart cast from ?str)

    user = try find_user(db, 42) orelse return error.UserNotFound
    # First try: handle error (!?User -> ?User)
    # Then orelse: handle optional (?User -> User)
    # user is now User (fully unwrapped)

    return user
```

*   **Sequential Unwrapping:** `try` handles errors, `orelse` handles optionals.
*   **Smart Casting:** Values are automatically narrowed after unwrapping.
*   *(Rationale: Handles real-world patterns where operations can both error and return optional data.)*

### 7.7 Unrecoverable Errors (`panic`)

For unrecoverable errors, use `panic("message")`:

```ryo
fn critical_operation():
    if not initialized:
        panic("System not initialized!")  # Aborts immediately
```

*   **Behavior:** Aborts the process. **Does not unwind** (simplifies implementation and predictability).
*   *(Rationale: Simplest, safest default for truly unrecoverable situations.)*

### 7.8 Error Handling Best Practices

1. **Use `try` for propagating errors** in functions that return error unions
2. **Use `catch` for handling errors** at boundaries (main functions, API handlers)
3. **Define specific error types** that capture all failure modes
4. **Implement `From` trait** for error composition across layers
5. **Use `!?T` carefully** to distinguish between errors and legitimate absence
6. **Pattern match exhaustively** to handle all error variants

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
    error NetworkError:
        ConnectionFailed
        RequestTimeout

    async fn process_request(req: Request) -> NetworkError!Response:
        data = try await database.query("SELECT * FROM users")
        result = try await external_api.call(data)
        return Response.json(result)

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
*   **`fn main()`:** Required in entry point. Takes no parameters, returns nothing. Use `try/catch` for error handling within main.
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
    *   `core`/`builtin` (Implicit): Core traits (`Drop`, `From`, `Length` for `.len(self)`), built-in functions (`print`, `println`, `panic`), error and optional type support.
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
