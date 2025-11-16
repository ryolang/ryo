# Ryo Programming Language Specification

## 1. Introduction & Vision

*   **Vision:** Ryo is a statically-typed, compiled programming language designed to prioritize developer experience while maintaining memory safety and competitive performance. It aims to combine the compile-time memory safety guarantees inspired by Rust (simplified, without a garbage collector), the approachable syntax and developer experience reminiscent of Python, and familiar async/await concurrency patterns. Where trade-offs exist, Ryo explicitly chooses developer productivity and debugging capability over raw performance optimization.

*   **Target Domains:** Web Backend Development (API Servers, Microservices), CLI Tools, Network Services & Proxies, WebAssembly (Wasm) Applications & Libraries, Game Development (Tooling, Scripting, Core Logic), Data Processing & ETL Pipelines, and Higher-Level Embedded Systems.
*   **Core Goals:**
    *   **Python-like Simplicity:** Clean, readable, minimal syntax. Easy to learn, especially for Python developers. Reduce boilerplate.
    *   **Rust-like Safety (Simplified):** Memory safe by default via ownership and borrowing, without GC. Compile-time checks prevent dangling pointers, data races, use-after-free. Simplified borrowing model compared to Rust (no manual lifetimes).
    *   **Go-like Simplicity:** Minimal keyword set, straightforward core concepts, avoid unnecessary feature creep. Focus on providing essential, orthogonal features.
    *   **Competitive Performance:** Compiled to efficient native code (or Wasm) via **Cranelift**. No GC pauses. Deterministic resource management. Note: Ryo includes automatic debugging features (stack traces, error context) that add ~5-10% runtime overhead but significantly improve developer experience.
    *   **Effective Concurrency:** Simple and safe concurrency using familiar async/await patterns with an async runtime (planned).
    *   **Compile-Time Power:** Integrated compile-time function execution (`comptime`) for metaprogramming, configuration, and optimization (planned for future implementation).
    *.  **Excellent Tooling:** Provide a seamless experience out-of-the-box, including a fast compiler, integrated package manager, REPL, and testing framework.

*   **Target Audience:** Developers familiar with languages like Python, Go, TypeScript, or C# seeking better performance and stronger safety guarantees without the steep learning curve of Rust or the runtime overhead of GC languages, especially for backend services, CLI tools, and scripting.

### Language Inspirations

Ryo synthesizes ideas from several modern programming languages:

*   **Python** - Clean syntax with colons and indentation, f-strings, type inference, async/await, developer-friendly design
*   **Rust** - Ownership model for memory safety, algebraic data types (enums with data), pattern matching, trait system, Result/Option types
*   **Mojo** - Simplified ownership without manual lifetimes, value semantics, progressive complexity model
*   **Go** - Simplicity as a core design principle, fast compilation, built-in concurrency primitives, minimal feature set
*   **Zig** - Explicit error handling with error unions, no hidden control flow, minimal runtime, comptime execution (Ryo plans similar compile-time features for future versions)

**Key Differentiators**: Ryo aims to be easier than Rust (no lifetimes), safer than Python (compile-time memory safety), more expressive than Go (generics, algebraic types), and more familiar than Zig (Python-like syntax).

### 1.1 DX-First Philosophy: Performance vs. Developer Experience Trade-offs

Ryo explicitly prioritizes **developer experience and debugging capability over raw performance** in key areas. This philosophical choice distinguishes Ryo from languages that pursue zero-cost abstractions at the expense of usability.

**Where Ryo Trades Performance for DX:**

| Feature | Runtime Overhead | Binary Size Impact | DX Benefit | Rationale |
|---------|------------------|-------------------|------------|-----------|
| **Automatic error stack traces** | ~5-10% (at error creation) | - | Complete error origin tracking with file/line/function | Eliminates hours of debugging; worth the cost for most applications |
| **Stack frame capture at `try`** | ~5-10% cumulative (at each propagation) | - | Full error propagation chain | Shows exactly how errors bubble through call stack |
| **Panic stack traces** | ~5-10% always-on | - | Post-mortem analysis without debugger | Critical for production debugging |
| **Debug symbols in binaries** | - | +20-30% | Resolve stack traces to source code | Use `--strip` flag for production if needed |

**Total Estimated Overhead:** ~5-10% for error-heavy workloads, negligible for error-free paths.

**When Ryo Is/Isn't Appropriate:**

✅ **Good fit:**
- Web backends, APIs, microservices (I/O-bound)
- CLI tools, build systems, developer tooling
- Applications where debugging time > runtime performance
- Prototyping and rapid development
- Teams prioritizing maintainability

❌ **Not ideal for:**
- Ultra-low-latency systems (HFT, real-time audio/video)
- Bare-metal embedded systems with tight resource constraints
- Applications where every microsecond matters
- Systems that cannot afford 5-10% overhead

**Comparison to Other Languages:**

- **Rust:** Optional stack traces (`RUST_BACKTRACE`), no overhead by default. Harder to debug but faster.
- **Go:** Simple stack traces with lower overhead. Less detailed than Ryo.
- **Zig:** Minimal runtime, opt-in stack traces. More control, less automation.
- **Ryo:** Rich debugging by default, configurable. Best DX out-of-box, with escape hatches.

*Rationale: Most applications spend more engineering time debugging than optimizing. Ryo chooses to save developer time at the cost of runtime performance, making it ideal for the 95% of applications where developer productivity matters more than the last 10% of performance.*

#### Configuration: Choice Without Complexity

True DX means **smart defaults + user choice**, not mandatory overhead. Ryo provides configuration options for performance-critical applications:

**Build-time control (compiler flags):**
```bash
ryo build                        # Default: full traces (~5-10% overhead)
ryo build --error-traces=minimal # Location only (~2-3% overhead)
ryo build --error-traces=off     # No capture (0% overhead)
```

**Profile-based defaults:**
```toml
# ryo.toml
[profile.dev]
error-traces = "full"      # Development wants full DX

[profile.release]
error-traces = "minimal"   # Production balances DX + performance
```

**Key principle:** Most developers never configure this. Defaults prioritize DX. Escape hatches exist for the 5% of applications where performance is critical.

See Section 7.10 for complete configuration reference.

## 2. Lexical Structure

*   **Encoding:** Source files are UTF-8 encoded, allowing for Unicode characters in strings and potentially identifiers (if identifier rules are expanded later).
*   **Identifiers:** `[a-zA-Z_][a-zA-Z0-9_]*`. Case-sensitive.
    *   *Convention:* Follow `snake_case` for variables, functions, and modules. Use `PascalCase` for user-defined types (structs, enums, traits) and enum variants. Built-in fundamental types (primitives and collections) use lowercase (e.g., `int`, `str`, `list`, `map`). *(Rationale: Adopting common conventions enhances readability and aligns with practices in Python and Rust).*
*   **Keywords:** `fn`, `struct`, `enum`, `trait`, `impl`, `mut`, `if`, `elif`, `else`, `for`, `in`, `return`, `break`, `continue`, `import`, `match`, `pub`, `package`, `true`, `false`, `none`, `void`, `async`, `await`, `move`, `error`, `try`, `catch`, `orelse`. (Note: `comptime`, `unsafe` are planned for future implementation. `void` is reserved for the unit type. `as`, `default`, `let` are not keywords. `package` is an access modifier keyword added for package-internal visibility).
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
    - **Note:** Code examples in this documentation may display spaces for markdown rendering compatibility, but actual `.ryo` source files **MUST** use tabs. The compiler will enforce this requirement and reject files with mixed tabs and spaces.
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
*   **Trait Definition:** `trait Name: fn method(...) -> RetType ... (with optional default implementation)`
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

    async fn fetch_data() -> !Data:
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
*   `str`: Owned, heap-allocated, UTF-8 string. Can grow and shrink dynamically when bound to a `mut` variable. *(Rationale: Provides a primary, easy-to-use string type. Mutability controlled by binding aligns with general variable mutability).*
*   `char`: Unicode Scalar Value. Literal: `'a'`.
*   `void`: Unit type. Represents a value with no data. Used for functions that return no meaningful value. *(Rationale: Provides explicit way to represent "no return value" concept, common in many programming languages for side-effecting functions)*.
*   Explicit Sizes: `i8`-`i64`, `u8`-`u64`, `usize`, `float32`. *(Rationale: Necessary for control over representation, performance, and FFI).**

### 4.3 Tuple Type

*   **Tuple Type:** `(T1, T2, ...)`. Literal `(v1, v2, ...)`. Access `.0`, `.1`, etc. Destructuring. *(Rationale: High Pythonic familiarity. Ergonomic for returning multiple values and simple ad-hoc grouping without needing named structs. Note: The unit type is represented by the `void` keyword, not an empty tuple, to avoid syntax ambiguity)*.*

### 4.4 Slice Types (Borrowed Views)


*   `&str`: Borrowed, immutable UTF-8 view (pointer + byte length). Created via `my_str[start_byte..end_byte]`, `my_str.as_slice()`, or from literals. Lifetime tied to borrowed data.
*   `&[T]`: Borrowed, immutable slice of `T` elements (pointer + element length). Created via `my_list[start..end]`, `my_list.as_slice()`.
*   `&mut [T]`: Borrowed, *mutable* slice of `T` elements. Created via `my_mut_list.as_mut_slice()`. Requires `mut` borrow of source.
*   *(Rationale: `&` syntax leverages borrow concept. No `&mut str` initially simplifies UTF-8 safety. Slices provide efficient read-only/mutable views without copying).*

**Function Parameter Note:** When using slice types like `&str` or `&[T]` as function parameters, the `&` is *required* because these are slice types, not owned types. Example:
```ryo
fn process_string(s: &str):      # Explicit & required for string slices
    # ... read s ...

fn process_list(items: &[int]):  # Explicit & required for list slices
    # ... read items ...

fn process_owned(data: MyStruct): # No & needed - implicit immutable borrow
    # ... read data ...
```

### 4.5 Struct Type (Product Type)

*   User-defined data aggregation: `struct Name: field: Type ...`.
*   Instances created via struct literals with named arguments: `Name(field=value, ...)`.
*   Access via dot notation: `instance.field`. Mutable if instance bound `mut`.

### 4.6 Enum Type (Sum Type / Algebraic Data Type - ADT)

*   **Concept:** Defines a type that can be exactly *one* of several named **variants**. Each variant can optionally hold associated data. Enums are fundamental for representing alternatives, states, and structured data safely.
*   **Syntax:**
    ```ryo
    enum EnumName[T]: # Optional type parameters for generics
        UnitVariant             # Variant with no data
        TupleVariant(Type1, Type2) # Variant holding ordered data
        StructVariant(name1: TypeA, name2: TypeB) # Variant holding named fields
    ```
*   **Instantiation:** Use `EnumName.VariantName`. Provide data for tuple/struct variants.
    ```ryo
    msg1 = Message.Quit
    msg2 = Message.Write("hello")
    msg3 = Message.Coords(x=10, y=-5)
    ```
*   **Pattern Matching (`match`):** The primary way to use enum values. `match` destructures variants and allows executing code based on the current variant.
    ```ryo
    match my_enum_value:
        MyEnum.Variant1:
            # Code for Variant1
        MyEnum.TupleVariant(data1, data2): # Bind tuple data
            # Code using data1, data2
        MyEnum.StructVariant(field_a, count): # Bind struct fields
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

*   `list[T]`: Dynamic array. Homogeneous. *(Built-in fundamental type)*
*   `map[K, V]`: Hash map. Homogeneous keys/values. `K` must be hashable/comparable. *(Built-in fundamental type)*

*Note: User-defined generics are planned for future implementation. See [Language Proposals](proposals.md#advanced-generics) for detailed generic type system design.*

### 4.8 Optional Types (`?T`)

*   **Syntax:** `?T` represents a value of type `T` that may be absent (represented by `none`). Eliminates null pointer errors through explicit, type-safe handling.
*   **Null literal:** `none` (lowercase keyword, consistent with `true` and `false`). *(Rationale: Python-familiar, semantically clear—"none" means "no value")*
*   **Declaration and Assignment:**
    ```ryo
    user: ?User = none
    user: ?User = User(name="Alice")

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
*   *(Rationale: Zig-inspired `?T` syntax is concise and compositional. The `none` keyword aligns with Python's `None`. Optional chaining (`?.`) and `orelse` provide ergonomic handling. Smart casting reduces boilerplate after validation)*

### 4.9 Error Types (`ErrorType!SuccessType`)

*   **Purpose:** Error types are algebraic data types specifically designed for error handling. Use the `error` keyword to define error types with associated data.

#### **Single-Variant Errors** (Simple Case)

*   **Unit Error (No Data):** A simple error marker:
    ```ryo
    error Timeout
    error Unauthorized
    ```
    **Usage:**
    ```ryo
    fn operation() -> Timeout!Data:
        if elapsed > limit:
            return Timeout  # Direct return
        return data
    ```

*   **Message-Only Error (Most Common):** Single unnamed string field becomes the message:
    ```ryo
    error NotFound(str)
    error ValidationFailed(str)
    ```
    **Usage:**
    ```ryo
    fn find_user(id: int) -> NotFound!User:
        if not exists(id):
            return NotFound("User not found")
        return user
    ```
    **Automatic message:** The string is accessible via `.message()` method on error trait.

*   **Structured Single-Variant Error:** Multiple named fields:
    ```ryo
    error HttpError(status: int, message: str)
    error ValidationError(field: str, constraint: str)
    ```
    **Usage:**
    ```ryo
    fn fetch(url: str) -> HttpError!Data:
        response = await http.get(url)
        if response.status != 200:
            return HttpError(status: response.status, message: response.body)
        return parse(response.body)
    ```

#### **Grouping Related Errors with Modules**

For organizing related errors, use directory-based module organization:

> **Note:** Ryo does NOT have a `module` keyword for inline definitions. Modules are defined by directory structure, and all `.ryo` files in a directory automatically form one module. Use `import` statements to reference errors from other modules. See Section 11 (Module System) for complete details.

```ryo
# File: io/errors.ryo
error FileNotFound(path: str)
error PermissionDenied(path: str)
error DiskFull

# File: parse/errors.ryo
error InvalidSyntax(line: int, column: int)
error UnexpectedToken(expected: str, got: str)
error UnexpectedEof
```

Usage:
```ryo
# File: main.ryo
import io
import parse

fn read_file(path: str) -> io.FileNotFound!str:
    if not exists(path):
        return io.FileNotFound(path)
    return os.read(path)

fn parse_json(text: str) -> parse.InvalidSyntax!Data:
    if invalid_json(text):
        return parse.InvalidSyntax(line=0, column=0)
    return Data(...)
```

#### **Error Union Types** (Composition)

*   **Explicit Error Unions** - Compose multiple error types:
    ```ryo
    # Can return either FileError or ParseError
    fn process(path: str) -> (FileError | ParseError)!Data:
        file = try read_file(path)      # FileError
        data = try parse_json(file)     # ParseError
        return data
    ```

*   **Inferred Error Unions** - Compiler infers error set from `try` expressions:
    ```ryo
    # Just use ! and compiler infers: (FileError | ParseError)!Data
    fn process(path: str) -> !Data:
        file = try read_file(path)      # FileError
        data = try parse_json(file)     # ParseError
        return data

    # Use --show-inferred-errors flag to see inferred type:
    # process() -> (FileError | ParseError)!Data
    ```
    **Benefits:** No wrapper types needed, composition is automatic, refactoring-friendly.

*   **Explicit Single Error Type** (`ErrorType!T`):
    ```ryo
    fn read_file(path: str) -> io.FileNotFound!str:
        if not exists(path):
            return io.FileNotFound(path)
        return os.read(path)
    ```

*   **Generic Error Type** (`!T`): Accept any error:
    ```ryo
    fn flexible_operation() -> !Data:
        # Can return any error type
        ...
    ```

*   **Combined Error and Optional** (`!?T`):
    ```ryo
    fn find_user(db: Database, id: int) -> DatabaseError!?User:
        # Can return: DatabaseError, none (not found), or User
        rows = try db.query("SELECT * FROM users WHERE id = ?", id)
        if rows.is_empty():
            return none
        return User.from_row(rows[0])
    ```

#### **Pattern Matching on Errors**

*   **Single Error Type** (Exhaustive matching required):
    ```ryo
    result = read_file(path) catch |e|:
        match e:
            io.FileNotFound(p):
                print(f"File not found: {p}")
            io.PermissionDenied(p):
                print(f"Permission denied: {p}")
            io.DiskFull:
                print("Disk full")
            # MUST handle all variants - compiler error if missing
    ```

*   **Error Union** (Exhaustive matching required):
    ```ryo
    result = process(path) catch |e|:
        match e:
            io.FileNotFound(p):
                return create_default(p)
            io.PermissionDenied(p):
                return request_permissions()
            parse.InvalidSyntax(line, col):
                log_error(f"Syntax error at {line}:{col}")
                return default_config()
            parse.UnexpectedToken(exp, got):
                log_error(f"Expected {exp}, got {got}")
                return default_config()
            parse.UnexpectedEof:
                log_error("Unexpected end of file")
                return default_config()
            # MUST handle all variants in union - compiler enforces this
    ```

    **Using catch-all when needed:**
    ```ryo
    result = process(path) catch |e|:
        match e:
            io.FileNotFound(p):
                return create_default(p)
            _:  # Explicit catch-all: "handle everything else the same way"
                log_error(e.message())
                return default_config()
    ```

*   *(Rationale: Single-variant errors provide simplicity (one syntax). Error unions enable automatic composition without wrapper types (Zig-inspired). Exhaustive matching by default ensures all error cases are explicitly handled. The `_` catch-all provides an escape hatch for truly generic handling.)*

### 4.10 Error Trait and Message Handling


* **Error Creation:** When an error value is created (`return MyError(...)`), the compiler automatically captures the full call stack at that moment, storing it as the initial stack trace. **Performance Note:** Stack capture incurs ~5-10% overhead at error creation, but only when errors actually occur (error-free code paths have no overhead). See Section 1.1 for DX vs. performance trade-off rationale.
* **Error Propagation (`try`):** When an error is propagated via try, the compiler appends a new frame to the error's stack trace. This new frame contains the location (file, line, function) of the try expression itself. **Performance Note:** Each propagation adds ~5-10% overhead at that specific `try` site when an error is being propagated (no overhead on success path).
* **Result:** The final `.stack_trace()` provides a complete, easy-to-read "story" of the failure, starting with the original error and showing every function that propagated it. This rich debugging information is a core part of Ryo's DX-first philosophy.


*   **Error Trait:** All error types automatically implement the `Error` trait:
    ```ryo
    trait Error:
        fn message(self) -> str           # Human-readable message
        fn location(self) -> ?Location    # Where error was created
        fn stack_trace(self) -> ?StackTrace  # Call stack when created

    struct Location:
        file: str          # File path (absolute or relative)
        line: int          # Line number (1-indexed)
        column: int        # Column number (1-indexed)
        function: str      # Function name with module path

    struct StackTrace:
        frames: list[StackFrame]

    struct StackFrame:
        function: str      # Function name with module path
        file: str          # File path
        line: int          # Line number
        column: int        # Column number
    ```

*   **Automatic Location Tracking:** All error values automatically capture the location where they are created:
    *   **`.location()`** returns `Location` with file, line, column, and function name
    *   **`.stack_trace()`** returns the full call stack (list of frames) at error creation
    *   Useful for debugging: find exactly where an error originated
    *   Works across error propagation with `try` - stack grows as error bubbles up

    Example:
    ```ryo
    # File: file/errors.ryo
    error NotFound(path: str)

    # File: main.ryo
    import file

    fn find_config() -> file.NotFound!Config:
        # Error created here captures: line 5, column 8, file "src/main.ryo"
        return file.NotFound("/etc/config.toml")

    fn main():
        config = find_config() catch |e|:
            # Access location information
            loc = e.location()  # Returns Location{file: "src/main.ryo", line: 5, ...}
            print(f"Error at {loc.file}:{loc.line}:{loc.column} in {loc.function}")

            # Get full stack trace
            trace = e.stack_trace()
            for frame in trace.frames:
                print(f"  {frame.function} at {frame.file}:{frame.line}")
    ```

*   **Automatic Message Generation:**
    *   **Single string field:** The string is used as the message.
        ```ryo
        error NotFound(str)
        # .message() returns the string directly
        ```
    *   **Named message field:** The `message` field is used.
        ```ryo
        error HttpError(status: int, message: str)
        # .message() returns the message field
        ```
    *   **Unit variant:** Variant name is used.
        ```ryo
        error Timeout
        # .message() returns "Timeout"
        ```
    *   **Multiple fields (no message field):** Generated from Debug representation.
        ```ryo
        error FileNotFound(path: str, permission_level: int)
        # .message() returns "FileNotFound(path=/var/log, permission_level=0700)"
        ```

*   **Custom Message Implementation:** Override automatic message generation:
    ```ryo
    # Single-variant errors with custom messages
    error TooShort(field: str, min_length: int)
    error TooLong(field: str, max_length: int)

    impl Error for TooShort:
        fn message(self) -> str:
            return f"{self.field} must be at least {self.min_length} characters"

    impl Error for TooLong:
        fn message(self) -> str:
            return f"{self.field} cannot exceed {self.max_length} characters"
    ```

*   **Accessing Error Messages and Location:**
    ```ryo
    result = operation() catch |e|:
        # Access message directly
        print(e.message())

        # Access location information for debugging
        if loc = e.location():
            print(f"Error at {loc.file}:{loc.line} in {loc.function}")

        # Or use in catch handlers
        match e:
            NotFound(msg):
                print(f"Not found: {msg}")
            _:
                print(f"Error: {e.message()}")
                if trace = e.stack_trace():
                    print("Stack trace:")
                    for frame in trace.frames:
                        print(f"  {frame.function} at {frame.file}:{frame.line}")
    ```

*   *(Rationale: Error messages are essential for debugging and user feedback. Automatic generation from data reduces boilerplate. Custom implementations enable domain-specific messages. Location tracking and stack traces enable efficient debugging without requiring external tools or logging.)*

### 4.11 FFI Types

*   **Note:** FFI types are planned for future implementation. See [Language Proposals](proposals.md#foreign-function-interface-ffi--unsafe-code) for detailed design.

### 4.12 Type Conversion Syntax

*   Uses function-call style `TargetType(value)` for explicit, safe conversions (primarily numeric and compatible types). *(Rationale: Explicit, uses type name directly like Go, avoids `as` keyword ambiguity, separates safe/unsafe casts clearly).*


### 5. Memory Management: "Ownership Lite"

The core principle of Ryo's "Ownership Lite" will be: **Borrow-by-Default for Functions, Move-by-Default for Assignment.** This is the crucial ergonomic trade-off that simplifies the model compared to Rust while maintaining safety.

*   **Core Principle:** Simplified Ownership & Borrowing, inspired by Rust but using Mojo's access-mode mental model.
*   **No Garbage Collector.** Provides deterministic performance and resource management.

#### 5.1 Value Semantics (Copy) vs. Ownership Semantics (Move)

*   **Value Types (Copy):** Primitive types (`int`, `float`, `bool`, `char`) and small, user-defined structs (that contain only Copy types) are **copied** on assignment, function call, and return. Ownership is trivial.
*   **Ownership Types (Move):** Types that manage external resources (e.g., `str`, `list[T]`, `map[K, V]`, and most user-defined structs/enums) are **moved** by default.

#### 5.2 The Three Modes of Data Access (Mojo-Inspired)

Ryo defines three explicit ways to pass or assign data, which the compiler uses to enforce safety without manual lifetime annotations.

| Mode | Syntax/Keyword | Semantics | Variable State After Operation |
| :--- | :--- | :--- | :--- |
| **1. Ownership Transfer** | `move` keyword on parameter, or implicit on assignment/return | Transfers ownership. The resource is now managed by the new owner. | **Invalidated** (Use-After-Move is a compile error) |
| **2. Exclusive Mutable Borrow** | `&mut` prefix on type (e.g., `&mut T`, `&mut self`) | Grants a temporary, **exclusive** mutable reference. Prevents all other access until the borrow ends. | **Valid** (Temporarily frozen from move/borrow) |
| **3. Shared Immutable Borrow** | `&` prefix on type (e.g., `&T`, `&self`) or **Implicit Default** for function parameters | Grants a temporary, **shared** immutable reference. Read-only access. | **Valid** (Temporarily frozen from mutable borrow/move) |

#### 5.3 Formalized Rules

1.  **Assignment & Return (Default: MOVE):**
    *   For Ownership Types, assignment (`new = old`) and return statements **move** the value, invalidating the original variable (`old`).
    *   Example: `new_str = old_str` (moves `old_str`)

2.  **Function Parameters (Default: IMMUTABLE BORROW):**
    *   Function parameters are **implicitly treated as Shared Immutable Borrows (`&Type`)** unless explicitly marked with `&mut` or `move`.
    *   This is the core ergonomic trade-off: `fn read(data: MyStruct)` is equivalent to `fn read(data: &MyStruct)`.
    *   The compiler enforces the borrow rule: the argument passed *cannot* be mutated by the function, and the caller's variable remains valid after the call.

3.  **Explicit Mutability:**
    *   **Mutable Variable:** Use `mut` on declaration: `mut my_data = ...`
    *   **Mutable Parameter:** Use the `&mut` symbol: `fn mutate(data: &mut MyType):` (This replaces the confusing `mut data: Type` syntax from the original spec).
    *   **Explicit Move Parameter:** Use the `move` keyword: `fn consume(move data: MyType):` (Overrides the implicit borrow default).

4.  **Borrowing Rules (Compile-Time Enforced):**
    *   **One Writer OR Many Readers:** At any point, a variable can have *either* one or more Shared Immutable Borrows (`&`) *OR* exactly one Exclusive Mutable Borrow (`&mut`).
    *   **Lexical Scopes:** Lifetimes are inferred by the compiler based on **lexical scopes**. A borrow cannot outlive the scope of its owner. **No manual lifetime annotations (`'a`) are required.**

### 5.4 RAII (`Drop` Trait) - The Core of Non-GC Safety

The `Drop` trait is the fundamental mechanism that allows Ryo to manage resources deterministically and avoid a GC.

*   **Function:** It guarantees that a resource (like a file handle, network socket, or heap memory) is cleaned up *exactly* when its owning variable goes out of scope.
*   **Safety & Performance:** Without `Drop`, Ryo would have to rely on manual cleanup calls, which are error-prone, or implement a full GC, which violates the "no GC pauses" performance goal.
*   **Relation to Ownership:** The Move/Borrow model (Ownership Lite) dictates *who* owns the value and *when* that ownership ends (e.g., on scope exit or after a move). The `Drop` trait dictates *what happens* when ownership ends. They work together.

The provided definition is sound:

> `impl Drop for Type: fn drop(self): ...`. Automatic cleanup on scope exit for owned values. Drop order is reverse declaration order within scope.

### 5.5 Shared Ownership (`Shared[T]` / ARC) - The Single-Owner Escape Hatch

The Move/Borrow model is excellent for hierarchical, tree-like data structures where a single owner is clear. However, it cannot safely handle two common scenarios:

1.  **Graph/Cyclic Data:** Structures like doubly linked lists or general graph data where nodes must reference each other, creating cycles that violate the single-owner rule.
2.  **Shared State:** Intentional sharing of a resource among multiple, independent entities (e.g., a configuration object accessed by multiple worker threads).

*   **Function:** `Shared[T]` (Atomic Reference Counted pointer) allows multiple "owners" to safely access the same data. The data is only dropped when the last `Shared[T]` reference is released. `Weak[T]` breaks cycles.
*   **Safety:** By making this mechanism explicit, Ryo retains its safety guarantee. The developer must opt-in to the overhead and understand the shared nature of the data, rather than having it happen implicitly (like in a GC language).
*   **Relation to Ownership:** When a value is wrapped in `Shared[T]`, the `Shared[T]` container becomes the new single owner, and the value inside is governed by the container's reference count.

The provided definition is sound:

> `Shared[T]` (ARC) / `Weak[T]` provided in stdlib... for opt-in shared ownership and cycle breaking. API uses dot notation... *(Rationale: Provides necessary mechanism... while making the associated overhead (ARC) explicit).*

### Summary

The Ryo Ownership Model is a three-layered system:

1.  **Layer 1 (The Core):** **Move/Borrow/Exclusive Access** (The Mojo-inspired rules) - *Governs how data can be accessed and transferred.*
2.  **Layer 2 (Resource Cleanup):** **RAII/Drop** - *Governs what happens when data ownership ends.*
3.  **Layer 3 (The Escape Hatch):** **Shared[T]/Weak[T]** - *Governs how to handle multi-owner scenarios safely.*

All three layers are required for Ryo to successfully deliver on its vision.

## 6. Functions & Closures

*   **Functions/Methods:** Standard definition/call. Return single value (can be tuple). Methods use `&self` (immutable borrow), `&mut self` (mutable borrow), or `self` (take ownership).
*   **Closures:** Anonymous functions `fn(args): expression`.
    *   **Capture:** Default immutable borrow. `move fn` captures by move. Mutable borrow inferred on mutation (requires original `mut`). Compiler checks rules. *(Rationale: Provides explicit control over captures, crucial for safety with `spawn` and non-escaping closures).*
    *   **Conceptual Types:** `Fn`, `FnMut`, `FnMove` describe capabilities, guiding type checking for functions accepting closures. *(Rationale: Defines closure behavior without full initial trait complexity).*

## 7. Error Handling

Error handling in Ryo uses algebraic error types (defined with the `error` keyword) combined with the `try` and `catch` operators for type-safe, explicit error management.

### 7.1 Error Types and Definitions

Error types are defined with the `error` keyword, using directory-based modules to organize related errors:

```ryo
# File: network/errors.ryo
error ConnectionTimeout
error DnsResolutionFailed(domain: str)
error HttpError(status: int, message: str)

# File: io/errors.ryo
error NotFound(path: str)
error PermissionDenied(path: str)
error ReadFailed(reason: str)
```

*   *(Rationale: `error` keyword signals error-handling intent. Single-variant errors with module organization provide clear composition without wrapper types. Associated data enables rich error information.)*

### 7.2 Error Union Types

Function return types specify both the error type and success type. Ryo provides three ways to express error types, each with different use cases:

#### **Choosing Your Error Union Syntax**

| Syntax | Use Case | Example |
|--------|----------|---------|
| `ErrorType!T` | Single, specific error type | `fn read(path) -> FileNotFound!str` |
| `(E1\|E2\|E3)!T` | Multiple known error types | `fn load() -> (FileNotFound\|ParseError)!Data` |
| `!T` | Any/inferred error types | `fn process() -> !Result` (compiler infers all errors from `try`) |

**Decision Guide:**
- **Use `ErrorType!T`** when your function can only fail in one specific way
- **Use `(E1|E2)!T`** when you know exactly which errors can occur and want to document them
- **Use `!T`** when composing multiple functions with different errors - the compiler automatically infers the error union from `try` expressions

#### **Single Error Type**

The `ErrorType!SuccessType` syntax indicates a function can return one specific error or a value:

```ryo
fn read_file(path: str) -> FileError!str:
    if not exists(path):
        return FileError.NotFound(path)
    return os.read(path)
```

#### **Multiple Error Types (Error Unions)**

**Explicit error union** - List all possible error types:

```ryo
# Can return FileError OR ParseError OR Data
fn process(path: str) -> (FileError | ParseError)!Data:
    file = try read_file(path)      # FileError
    data = try parse_json(file)     # ParseError
    return data
```

**Inferred error union** - Compiler infers from `try` expressions:

```ryo
# Compiler infers: (FileError | ParseError)!Data
fn process(path: str) -> !Data:
    file = try read_file(path)      # FileError
    data = try parse_json(file)     # ParseError
    return data
```

**Generic error type** - Accept any error:

```ryo
fn flexible_operation() -> !Data:
    # Can return any error type
    ...
```

*   **Error Union Semantics:**
    *   Error types are composed with `|` operator (unordered, not a sequence)
    *   Inferred unions automatically track all possible errors from `try` expressions
    *   Use `--show-inferred-errors` compiler flag to see inferred error set
    *   Single error type is a special case of error union with one member

*   *(Rationale: Zig-style `E!T` syntax is concise. Error unions eliminate wrapper types through automatic composition. Explicit unions document API contracts. Inferred unions reduce boilerplate.)*

### 7.3 Error Propagation (`try`)

**Error Context Preservation (DX Priority):** When `try` propagates an error, it captures the current execution context (file, line, function name) and appends this frame to the error's internal stack trace. **Performance Impact:** This process incurs approximately **5-10% runtime overhead** (due to memory allocation and stack frame capture) at every propagation boundary where an error is actually being propagated. The success path (no error) has no overhead. Ryo prioritizes complete debugging information over raw performance; see Section 1.1 for trade-off rationale. The final stack trace shows the complete chain of propagation.

The `try` keyword unwraps success or propagates the error early:

```ryo
fn load_and_parse(path: str) -> !Config:
    # Both try expressions propagate errors
    content = try read_file(path)      # FileError propagates
    config = try parse_config(content) # ParseError propagates
    return config
```

*   **Semantic:** `try expr` evaluates `expr`:
    *   If success: returns the value
    *   If error: propagates error to caller

*   **Error Composition with `try`:**
    *   In functions with inferred error unions, `try` automatically collects all error types
    *   In explicit error unions, error must be in the union
    *   In single error type, error must match exactly

*   **Example - Inferred Union:**
    ```ryo
    fn process() -> !Data:
        a = try func_a()  # FileError
        b = try func_b()  # ParseError
        c = try func_c()  # NetworkError
    # Inferred as: (FileError | ParseError | NetworkError)!Data
    ```

*   **Example - Explicit Union with Conversion:**
    ```ryo
    # Example using separate error types with error unions
    fn process() -> (FileError | ParseError)!Data:
        a = try read_file(path)  # Can return FileError
        b = try parse_json(a)    # Can return ParseError
        return b
    ```

*   **Error Context Preservation:** When `try` propagates an error, the original error's location and stack trace are preserved intact. No context is lost as the error bubbles up through the call stack. Each level can inspect `.location()` and `.stack_trace()` to see where the error originated.

    Example:
    ```ryo
    fn level3() -> db.QueryFailed!Result:
        # Error created here with location information
        return db.QueryFailed("Invalid query")

    fn level2() -> db.QueryFailed!Result:
        result = try level3()  # Error propagates, context preserved
        return result

    fn level1() -> !Result:
        result = try level2()  # Error propagates, context preserved
        return result

    fn main():
        data = level1() catch |e|:
            # Can still access original location from level3
            loc = e.location()
            print(f"Original error at {loc.file}:{loc.line}")
    ```

*   *(Rationale: `try` clearly signals error propagation. Familiar to async/await users. Automatic composition via inferred unions eliminates wrapper types (Zig-inspired). Error context preservation ensures debugging information is never lost during propagation.)*

### 7.4 Error Handling (`catch`)

**IMPORTANT:** Error handling with `catch` requires **exhaustive pattern matching**. All error types and variants must be explicitly handled. If you don't want to handle specific error cases explicitly, use the `_` wildcard pattern to match remaining cases.

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

*   **Pattern Matching Differences:**
    *   **Single Error Type** (exhaustive): Must handle all variants
        ```ryo
        result = read_file(path) catch |e|:
            match e:
                FileError.NotFound(p):
                    # ...
                FileError.PermissionDenied(p):
                    # ...
                FileError.ReadError(r):
                    # ...
                # MUST handle all variants
        ```
    *   **Error Union** (Exhaustive matching required): Must handle all error types in union:
        ```ryo
        result = process(path) catch |e|:
            match e:
                io.FileNotFound(p):
                    return create_default(p)
                parse.InvalidFormat(reason):
                    log_error(f"Parse error: {reason}")
                    return default_config()
                network.ConnectionFailed(reason):
                    return retry_later()
                # MUST handle all variants in union
        ```
    *   **With Catch-All**: When you want generic handling for some errors:
        ```ryo
        result = process(path) catch |e|:
            match e:
                io.FileNotFound(p):
                    return create_default(p)
                _:  # Explicit catch-all for all other error types
                    log_error(e.message())
                    return default_config()
        ```

*   *(Rationale: `catch` follows familiar error-handling conventions. Exhaustive matching for all error types (single or union) ensures all error cases are explicitly handled, improving code reliability and preventing silent failures.)*

### 7.5 Combined Error + Optional (`!?T`)

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

### 7.6 Unrecoverable Errors (`panic`)

For unrecoverable errors, use `panic("message")`. When a panic occurs, the program immediately terminates after printing diagnostic information.

```ryo
fn critical_operation():
    if not initialized:
        panic("System not initialized!")  # Aborts immediately
```

#### **Panic Behavior**

*   **Aborts the process immediately** with exit code `101`
*   **Does not unwind** - no cleanup code runs (simplifies implementation and predictability)
*   **Captures and prints full stack trace** - shows complete call chain leading to panic
*   **Includes location information** - file, line, column, and function name of panic call

#### **Panic Output Format**

When `panic("message")` executes, output appears on stderr:

```
thread 'main' panicked at src/database.ryo:42:13 in function 'connect':
  Database connection failed: timeout after 30s

Stack trace:
  0: database::connect (src/database.ryo:42:13)
  1: app::initialize (src/app.ryo:18:25)
  2: main (src/main.ryo:10:5)

note: Set RYOLANG_BACKTRACE=full for more verbose output
```

**Stack trace details:**
- Each frame shows: frame number, function path, file:line:column location
- Frame 0 is the panic call (most recent)
- Frame N is the entry point (oldest)
- Includes inlined functions and async boundaries

#### **Debug Symbols and Stack Traces**

**Default behavior (DX-optimized):**
- Stack traces automatically captured for all panics
- Debug symbols included (DWARF format via Cranelift)
- Binary size impact: +20-30%

**Configuration options:**

*Build-time (compiler flags):*
```bash
ryo build                        # Default: full traces
ryo build --error-traces=minimal # Location only (~2-3% overhead)
ryo build --error-traces=off     # No automatic capture (0% overhead)
ryo build --strip                # Remove debug symbols (production)
```

*Runtime (environment variables):*
```bash
RYOLANG_ERROR_TRACES=full    # Show all frames
RYOLANG_ERROR_TRACES=short   # Show 3-5 frames (default)
RYOLANG_ERROR_TRACES=off     # Only error message
```

**Recommended approach:**
- Development: Use defaults (`--error-traces=full`)
- Production: Use `--error-traces=minimal` or profile-based config
- HFT/embedded: Use `--error-traces=off` for zero overhead

#### **Performance Implications**

Panic stack traces incur runtime overhead even when no panic occurs (in `full` mode):

*   **Runtime overhead** - ~5-10% estimated (varies by workload) for stack frame maintenance
*   **Memory overhead** - Maintaining stack frame information uses additional memory
*   **Configurable** - Use `--error-traces=minimal` or `=off` to reduce/eliminate overhead (see Section 7.10)

**When to configure:**
- Ultra-low-latency systems → Use `--error-traces=off`
- Performance-sensitive services → Use `--error-traces=minimal`
- Most applications → Use defaults (debugging capability > 5-10% overhead)

**Mitigation strategies:**
- Use build profiles (dev: full, release: minimal)
- Structure code to avoid panic in hot paths
- Use error types (`!T`) for recoverable errors instead of panics
- See Section 7.10 for complete configuration guide

#### **When to Use `panic`**

Use `panic()` **only** for:
- Truly unrecoverable conditions that indicate a bug in your program
- Invalid program state that cannot be recovered
- Internal consistency violations

Do **not** use `panic()` for:
- User input errors (use error types instead)
- Expected failure modes (use error types instead)
- Control flow (use error types instead)

#### **Example: Understanding a Panic**

```ryo
# File: database/errors.ryo
error ConnectionFailed(reason: str)

# File: main.ryo
import database

fn connect(host: str, port: int) -> database.ConnectionFailed!Connection:
    if port < 1 or port > 65535:
        # BUG: Invalid port should never reach here if caller validates
        panic(f"Invalid port {port}: must be 1-65535")

    # ... actual connection code ...
    Connection{...}

fn main():
    # If this panics with invalid port, stack trace shows:
    # 1. panic location (in connect function)
    # 2. call to connect (in main)
    # 3. where to fix the bug
```

*   *(Rationale: Immediate abort without unwinding simplifies runtime and guarantees clean termination. Comprehensive stack traces provide essential debugging information for post-mortem analysis.)*

### 7.7 Error Handling Best Practices

1. **Use `try` for propagating errors** in functions that return error unions
2. **Use `catch` for handling errors** at boundaries (main functions, API handlers)
3. **Define specific error types** that capture all failure modes
4. **Use modules to organize related errors** ensuring clear composition
5. **Use `!?T` carefully** to distinguish between errors and legitimate absence
6. **Pattern match exhaustively** to handle all error variants

### 7.8 Forbidden: Direct Unwrap

**Direct unwrap is NOT allowed.** Attempting to access error or optional values without using `try`, `catch`, or `orelse` is a **compile-time error**:

```ryo
# ❌ COMPILE ERROR: Cannot access error union value directly
result: ParseError!int = parse_int("42")
value = result  # ERROR: Cannot use value of type ParseError!int directly

# ❌ COMPILE ERROR: Cannot access optional value directly
maybe_user: ?User = get_user(id)
name = maybe_user.name  # ERROR: Cannot access fields on optional type

# ✅ CORRECT: Use try to unwrap errors
result: ParseError!int = parse_int("42")
value = try result catch |e|:
    handle_error(e)
    return

# ✅ CORRECT: Use try to unwrap and propagate
fn load_data() -> ParseError!int:
    result = try parse_int("42")  # Propagates error on failure
    return result

# ✅ CORRECT: Use orelse for optionals
maybe_user: ?User = get_user(id)
name = maybe_user?.name orelse "Unknown"

# ✅ CORRECT: Use smart casting after null check
if maybe_user != none:
    name = maybe_user.name  # Type narrowed to User
```

*   **Rationale:** Direct unwrap removes type safety. By requiring explicit `try`/`catch`/`orelse`, Ryo ensures all error and null cases are handled, preventing silent failures and unexpected panics. This design choice makes error handling visible and intentional.

### 7.9 Stack Traces and Debugging

Ryo provides comprehensive stack trace and debugging information to help diagnose runtime errors efficiently.

#### **Automatic Stack Trace Capture**

**Default behavior (DX-first):**
- Stack traces automatically captured for all panics and errors
- ~5-10% runtime overhead in default mode
- Can be configured at build-time or runtime

**Configuration tiers:**

1. **Build-time** (most common): `--error-traces=full|minimal|off`
2. **Runtime**: `RYOLANG_ERROR_TRACES` env var (controls display only)

See Section 7.10 for complete configuration options and trade-offs.

**Full call chain** - Shows complete function call path from entry point to error location
**Accessible at runtime** - Use `.stack_trace()` method on errors to access frame information

#### **Using Stack Traces for Debugging**

**From Panics:**
```ryo
fn dangerous_operation() -> int:
    panic("Something went very wrong!")

fn main():
    # When panic occurs, stderr shows:
    # thread 'main' panicked at src/main.ryo:10:5 in function 'dangerous_operation':
    # Something went very wrong!
    #
    # Stack trace:
    #   0: main::dangerous_operation (src/main.ryo:10:5)
    #   1: main (src/main.ryo:5:5)
    dangerous_operation()
```

**From Errors:**
```ryo
# File: db/errors.ryo
error QueryFailed(sql: str)

# File: main.ryo
import db

fn query_user(id: int) -> db.QueryFailed!User:
    # Error automatically captures file/line/function at creation
    return db.QueryFailed(f"SELECT * FROM users WHERE id = {id}")

fn main():
    user = query_user(42) catch |e|:
        # Access location where error was created
        if loc = e.location():
            print(f"Error created at {loc.file}:{loc.line} in {loc.function}")

        # Access full stack at time of error creation
        if trace = e.stack_trace():
            for frame in trace.frames:
                print(f"  Frame: {frame.function} at {frame.file}:{frame.line}")
        return
```

#### **Debug Symbols and Build Information**

*   **Debug symbols always included by default** - DWARF format generated via Cranelift
*   **Binary size impact** - Approximately 20-30% larger due to debug information
*   **`--strip` compiler flag** - Remove debug symbols from production binaries if size is critical
*   **Trade-off confirmed** - Size cost justified by debugging capability

#### **Environment Variables**

Control stack trace verbosity:

*   **`RYOLANG_BACKTRACE=1`** (default) - Standard stack trace with file, line, column, function name
*   **`RYOLANG_BACKTRACE=full`** (future) - Verbose output with additional context and local values
*   **`RYOLANG_BACKTRACE=0`** (not recommended) - Minimal output, disables stack trace display

Example:
```bash
# Show standard stack trace (default)
./my_program

# Show verbose stack trace
RYOLANG_BACKTRACE=full ./my_program

# Suppress stack trace (not recommended)
RYOLANG_BACKTRACE=0 ./my_program
```

#### **Comparison: Ryo vs Other Languages**

Understanding how Ryo's debugging approach compares to alternatives:

| Language | Stack Trace Approach | Overhead | DX Rating | When to Choose |
|----------|---------------------|----------|-----------|----------------|
| **Ryo** | Always-on, automatic, rich context | ~5-10% always | ⭐⭐⭐⭐⭐ Excellent | When debugging ease > raw performance |
| **Rust** | Optional (`RUST_BACKTRACE=1`), opt-in | ~0% default, ~3-5% when enabled | ⭐⭐⭐ Good (requires env var) | When performance > debugging ease |
| **Go** | Always-on, simpler traces | ~1-3% | ⭐⭐⭐⭐ Very good (less detail) | Balanced, but less detail than Ryo |
| **Zig** | Optional, manual stack walking | ~0% default | ⭐⭐ Fair (manual effort) | Maximum control, minimal overhead |
| **Python** | Always-on, interpreter traces | High (GC+interpreter) | ⭐⭐⭐⭐⭐ Excellent | Prototyping, development |
| **C/C++** | Debugger-only, no built-in traces | ~0% | ⭐ Poor (debugger required) | Maximum performance, embedded |

**Key Takeaway:** Ryo sits between Python (maximum DX, high overhead) and Rust (maximum performance, manual DX). Ryo chooses mandatory rich debugging at a measurable but acceptable cost for most applications.

For complete performance implications and mitigation strategies, see Section 7.6.

#### **Best Practices for Debugging**

1. **Use error location for quick diagnosis:**
   ```ryo
   result = risky_operation() catch |e|:
       loc = e.location()
       print(f"Quick fix: Check {loc.file}:{loc.line}")
   ```

2. **Print full stack trace for complex error chains:**
   ```ryo
   result = risky_operation() catch |e|:
       print(f"Error: {e.message()}")
       if trace = e.stack_trace():
           print("Debug stack trace:")
           for frame in trace.frames:
               print(f"  {frame.function}")
   ```

3. **Use messages with context:**
   ```ryo
   error DatabaseError(sql: str, reason: str)
   # When error occurs, message includes both query and reason
   ```

4. **Avoid panics in production paths:**
   - Use error types for expected failures
   - Reserve `panic()` for bugs and internal inconsistencies
   - Structured error handling is better for debugging than post-mortem panic analysis

### 7.10 Error Trace Configuration

Ryo provides flexible configuration for error stack traces, balancing DX with performance needs.

#### **Configuration Levels**

**1. Build-Time (Recommended)**

Compiler flag: `--error-traces=LEVEL`

| Level | Creation Overhead | Propagation Overhead | Total | Use Case |
|-------|-------------------|----------------------|-------|----------|
| `full` (default) | ~5-10% | ~5-10% cumulative | ~5-10% | Development, most production |
| `minimal` | ~2-3% | 0% | ~2-3% | Performance-sensitive services |
| `off` | 0% | 0% | 0% | HFT, real-time, embedded |

**Examples:**
```bash
# Default build with full traces
ryo build

# Minimal traces for production
ryo build --release --error-traces=minimal

# Zero overhead for performance-critical applications
ryo build --release --error-traces=off --strip
```

**2. Profile-Based Configuration**

Configure per build profile in `ryo.toml`:

```toml
[profile.dev]
error-traces = "full"      # Full DX during development

[profile.release]
error-traces = "minimal"   # Balanced for production

[profile.production]
error-traces = "off"       # Maximum performance if needed
```

**3. Runtime Configuration (Display Control)**

Environment variables control **display**, not capture:

```bash
RYOLANG_ERROR_TRACES=full    # Show all frames (verbose)
RYOLANG_ERROR_TRACES=short   # Show 3-5 frames (default)
RYOLANG_ERROR_TRACES=off     # Only error message
```

**Note:** Runtime variables only affect what's printed, not what's captured. Use build-time flags to eliminate capture overhead.

#### **Recommended Configurations by Use Case**

**Web Backend / API Server:**
```toml
[profile.dev]
error-traces = "full"

[profile.release]
error-traces = "full"  # Still useful for debugging production issues
```

**High-Performance Service:**
```toml
[profile.dev]
error-traces = "full"

[profile.release]
error-traces = "minimal"  # Location only, minimal overhead
```

**Real-Time / Embedded Systems:**
```toml
[profile.release]
error-traces = "off"  # Zero overhead, manual logging required
```

**CLI Tools / Developer Tooling:**
```toml
# Use defaults - full traces everywhere
```

#### **How Configuration Works**

**Build-time (`--error-traces`):**
- Compiler generates different IR based on flag
- `full`: Captures stack at error creation + every `try`
- `minimal`: Only captures location at error creation
- `off`: No automatic capture, errors only have `.message()`

**Runtime (`RYOLANG_ERROR_TRACES`):**
- Controls output format when panic/error occurs
- Does NOT affect capture (that's build-time decision)
- Useful for CI/CD where you want concise logs

*Rationale: Configuration respects DX-first philosophy while providing escape hatches for performance-critical code. Smart defaults mean most developers never need to configure anything.*

## 8. Traits (Behavior)

*   **Definition:** `trait Name: fn method(...) ...` (with optional default implementations). Default methods allowed. *(Rationale: Default methods reduce boilerplate).*
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
    async fn process_request(req: Request) -> !Response:
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

### 11.1 Package Definition

**Package** = The entire project defined by `ryo.toml`. This is the unit of compilation, versioning, and distribution.

*   **Properties:**
    *   One `ryo.toml` file defines one package
    *   Contains one or more modules
    *   Compiled as a single unit
    *   Published to package registry as one unit
    *   Has a unique name (e.g., "mywebapp")
    *   Defines the boundary for `package` visibility

*   **Equivalent to:** Rust's crate, Go's module (go.mod), Swift's package (Package.swift), Python's distribution package

**Example Structure:**
```
mywebapp/              # Package "mywebapp"
├── ryo.toml           # Package manifest
└── src/
    ├── main.ryo       # Entry point
    ├── server/        # Module "server"
    └── database/      # Module "database"
```

**ryo.toml:**
```toml
[package]
name = "mywebapp"
version = "1.0.0"
authors = ["Your Name <you@example.com>"]

[dependencies]
http = "1.0"
json = "0.5"
```

### 11.2 Module Definition

**Module** = A directory containing `.ryo` files. All `.ryo` files in the same directory are part of one module and share a namespace.

*   **Properties:**
    *   One directory = one module
    *   All `.ryo` files in directory share the same namespace
    *   Can contain both `.ryo` files AND subdirectories (submodules)
    *   Hierarchical: parent modules can contain child modules
    *   Module-private items are shared among all files in the directory
    *   Imported by full path (e.g., `import utils.math`)
    *   Implicit discovery - no `mod` keyword needed

*   **Equivalent to:** Go's package (directory concept), Rust's module (organizational concept), Python's package (directory)

**Example:**
```
src/
  utils/               # Module "utils"
    core.ryo           # Part of "utils" module
    helpers.ryo        # Part of "utils" module
```

All functions and types in `core.ryo` and `helpers.ryo` are part of the `utils` module namespace.

### 11.3 Hierarchical Module Structure

Modules can contain submodules, creating a hierarchical organization.

**Example:**
```
src/
  utils/               # Module "utils" (parent)
    core.ryo           # Part of "utils" module
    helpers.ryo        # Part of "utils" module
    math/              # Module "utils.math" (child of utils)
      basic.ryo        # Part of "utils.math" module
      geometry/        # Module "utils.math.geometry" (child of utils.math)
        shapes.ryo     # Part of "utils.math.geometry" module
```

**Module Hierarchy:**
*   `utils` module contains:
    *   Files: `core.ryo`, `helpers.ryo`
    *   Submodule: `utils.math`
*   `utils.math` module contains:
    *   Files: `basic.ryo`
    *   Submodule: `utils.math.geometry`
*   `utils.math.geometry` module contains:
    *   Files: `shapes.ryo`

**Key Properties:**
*   Parent directories can have BOTH `.ryo` files AND subdirectories (submodules)
*   Each directory level is a separate module with its own namespace
*   Child modules do NOT automatically see parent module's private items
*   Must import parent explicitly to use its items

### 11.4 Access Control Levels

Ryo provides three access control levels for fine-grained visibility control:

#### **1. `pub` - Public (Exported)**

Visible everywhere, including external packages that import your library.

```ryo
# server/http.ryo
pub fn start():        # Public API - anyone can use
    _bind_port()
```

**Use when:**
*   It's your library's public API
*   External projects will import it
*   You promise stability (semver applies)

#### **2. `package` - Package-Internal**

Visible to all modules within the same package (defined by `ryo.toml`), but NOT to external packages.

```ryo
# internal/config.ryo
package fn load_config():  # Shared across modules in project
    pass

# server/http.ryo (different module, same package)
import internal.config

fn start():
    config.load_config()   # ✅ OK - package visibility
```

**Use when:**
*   Multiple modules in your project need it
*   It's an implementation detail, not public API
*   You want to share code without exposing it externally

#### **3. No Keyword - Module-Private**

Visible only within the same module (directory). All files in the directory can access module-private items.

```ryo
# server/http.ryo
fn _bind_port():       # Module-private
    pass

# server/routes.ryo (same directory = same module)
fn register():
    http._bind_port()  # ✅ OK - same module

# database/connection.ryo (different directory = different module)
fn connect():
    server.http._bind_port()  # ❌ ERROR - module-private
```

**Use when:**
*   Only one module needs it
*   It's an implementation detail within that module
*   You want to hide complexity from other modules

#### **Comparison to Other Languages**

| Language | Public | Package/Crate | Module/Internal | File |
|----------|--------|---------------|-----------------|------|
| **Ryo** | `pub` | `package` | *(no keyword)* | - |
| **Rust** | `pub` | `pub(crate)` | `pub(super)` / *(no keyword)* | - |
| **Go** | Capitalized | `internal/` | *(lowercase)* | - |
| **Swift 6** | `public`/`open` | `package` | `internal` | `fileprivate` |
| **Zig** | `pub` | - | *(no keyword)* | - |
| **Python** | *(default)* | - | *(by convention)* | - |

*Rationale: Three levels provide the right balance - simpler than Rust/Swift (4-6 levels), more expressive than Go/Zig (2 levels). Swift 6 added `package` in 2025, validating this need.*

### 11.5 Import System

#### **Import Syntax**

```ryo
import utils                         # Import module
import utils.math                    # Import submodule (full path required)
import utils.math.geometry           # Import nested submodule
import server.middleware as mw       # Aliased import
import utils.{add, subtract}         # Import specific items (future)
import pkg:external_dep              # External dependency from ryo.toml
```

#### **Import Rules**

1.  **Full Path Required:** Must specify complete module path
    ```ryo
    import utils.math.geometry  # ✓ Full path
    import geometry             # ✗ Missing parent path
    ```

2.  **No Implicit Parent Access:** Child modules must import parent explicitly
    ```ryo
    # utils/math/basic.ryo
    import utils  # Must import parent to use it

    fn example():
        utils.helper()  # ✓ After importing
    ```

3.  **Paths Relative to `src/`:** Import paths resolve from `src/` directory
    ```ryo
    import server      # → src/server/
    import utils.math  # → src/utils/math/
    ```

4.  **External Dependencies:** Use `pkg:` prefix for dependencies in `ryo.toml`
    ```ryo
    import pkg:http    # From [dependencies]
    import pkg:json    # From [dependencies]
    ```

### 11.6 Module Visibility Rules

#### **Within Same Module (Directory)**

All files in the same directory share namespace and can access each other's module-private items.

```ryo
# server/http.ryo
fn _helper():  # Module-private
    pass

pub fn start():
    _helper()  # ✓ Same module

# server/routes.ryo (same directory)
fn register():
    http._helper()  # ✓ Same module (server)
```

#### **Between Different Modules**

Only `pub` and `package` items are visible between modules.

```ryo
# utils/math.ryo
pub fn add():        # Public
    pass

package fn internal():  # Package-visible
    pass

fn _private():       # Module-private
    pass

# server/http.ryo (different module)
import utils.math

fn example():
    math.add()       # ✓ Public
    math.internal()  # ✓ Package visibility
    math._private()  # ✗ ERROR - module-private
```

#### **Between Parent and Child Modules**

Child modules are separate namespaces from parent. Must import parent explicitly.

```ryo
# utils/core.ryo (parent module)
pub fn parent_pub():
    pass

package fn parent_package():
    pass

fn parent_private():
    pass

# utils/math/basic.ryo (child module)
import utils  # Must import parent!

fn example():
    utils.parent_pub()      # ✓ Public
    utils.parent_package()  # ✓ Package visibility
    utils.parent_private()  # ✗ ERROR - module-private to utils
```

### 11.7 Circular Dependencies

#### **Forbidden Between Modules**

Circular dependencies between modules are **compile-time errors**.

```ryo
# server/http.ryo
import database  # Server imports database

# database/connection.ryo
import server    # ✗ ERROR - Circular dependency!
```

**Error Message:**
```
Error: Circular dependency detected
  server → database → server
```

*Rationale: Prevents spaghetti code, forces clearer architecture, enables deterministic compilation order.*

#### **Allowed Within Module**

Files in the same module (directory) can freely reference each other.

```ryo
# server/http.ryo
fn start():
    routes.register()  # ✓ OK - same module

# server/routes.ryo
fn register():
    http.start()       # ✓ OK - same module
```

#### **Common Workarounds for Cross-Module Dependencies**

**Problem:** User ↔ Post circular dependency

**Solution 1: Extract Common Types**
```ryo
# types/core.ryo
pub struct UserID(int)
pub struct PostID(int)

# user/user.ryo
import types.core

pub struct User:
    id: core.UserID
    posts: list[core.PostID]  # Reference by ID

# post/post.ryo
import types.core

pub struct Post:
    id: core.PostID
    author_id: core.UserID    # Reference by ID
```

**Solution 2: Merge Modules**
```ryo
# domain/models.ryo - Combined module
pub struct User: ...
pub struct Post:
    author: User  # ✓ Same module
```

### 11.8 Complete Examples

#### **Example 1: Simple Package**

```
myapp/
├── ryo.toml
└── src/
    ├── main.ryo
    └── utils/
        └── math.ryo
```

**src/utils/math.ryo:**
```ryo
pub fn add(a: int, b: int) -> int:
    return a + b
```

**src/main.ryo:**
```ryo
import utils.math

fn main():
    result = math.add(2, 3)
    print(result)  # 5
```

#### **Example 2: Multi-File Module**

```
myapp/
└── src/
    └── server/
        ├── http.ryo    # Part of "server" module
        └── routes.ryo  # Part of "server" module
```

**src/server/http.ryo:**
```ryo
pub fn start():              # Public
    _bind_port()             # Module-private

fn _bind_port():             # Module-private
    routes.register()        # ✓ Same module
```

**src/server/routes.ryo:**
```ryo
pub fn register():
    http._bind_port()        # ✓ Same module
```

#### **Example 3: Hierarchical Modules**

```
myapp/
└── src/
    └── utils/
        ├── core.ryo       # Part of "utils"
        └── math/
            └── basic.ryo  # Part of "utils.math"
```

**src/utils/core.ryo:**
```ryo
pub fn helper():
    pass

package fn internal():
    pass
```

**src/utils/math/basic.ryo:**
```ryo
import utils  # Import parent

pub fn calculate():
    utils.helper()      # ✓ Public
    utils.internal()    # ✓ Package visibility
```

**src/main.ryo:**
```ryo
import utils
import utils.math

fn main():
    utils.helper()         # ✓ Public
    utils.math.calculate() # ✓ Public
```

#### **Example 4: Access Levels**

```ryo
# lib/api.ryo

pub fn public_api():              # External API
    package_helper()

package fn package_helper():      # Internal API for project
    _module_helper()

fn _module_helper():             # Implementation detail
    pass
```

**Usage from same package:**
```ryo
# server/main.ryo
import lib.api

fn main():
    api.public_api()       # ✓ Public
    api.package_helper()   # ✓ Package visibility
    api._module_helper()   # ✗ ERROR - module-private
```

**Usage from external package:**
```ryo
# external-project/main.ryo
import myapp.lib.api

fn main():
    api.public_api()       # ✓ Public
    api.package_helper()   # ✗ ERROR - package-private
```

#### **Example 5: Package Boundary**

**Package 1 (mylib):**
```
mylib/
├── ryo.toml  # name = "mylib"
└── src/
    └── utils/
        └── helper.ryo
```

**mylib/src/utils/helper.ryo:**
```ryo
pub fn public_helper():
    pass

package fn internal_helper():  # Only within mylib package
    pass
```

**Package 2 (myapp):**
```
myapp/
├── ryo.toml  # name = "myapp", depends on mylib
└── src/
    └── main.ryo
```

**myapp/src/main.ryo:**
```ryo
import pkg:mylib.utils.helper

fn main():
    helper.public_helper()    # ✓ Public
    helper.internal_helper()  # ✗ ERROR - Different package!
```

*Rationale: Implicit module discovery reduces boilerplate (no `mod` keyword), hierarchical paths enable clear organization, three access levels balance simplicity and expressiveness, forbidden circular dependencies enforce good architecture.*

## 12. Application Entry Point

*   **Convention:** Default entry point file is `src/main.ryo`.
*   **`fn main()`:** Required in entry point. Takes no parameters, returns the unit type `void`. Use `try/catch` for error handling within main.
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
    *   `io`: Console (`readln`), Files (`File`), Buffering (functions return `IoError!T`), implements `Drop`.
    *   `string`: `&str` manipulation, parsing (functions return `ParseError!T`).
    *   `collections`: `list[T]`, `map[K, V]` types and methods.
    *   `math`: Functions, constants, explicit overflow methods.
    *   `time`: `Instant`, `SystemTime`, `Duration`.
    *   `encoding.json`: `encode -> JsonError!str`, `decode -> JsonError!JsonValue`, `decode_into[T] -> JsonError!T`.
    *   `net.http`: Async Client/Server primitives (`Request`, `Response`, async handlers, functions return `HttpError!T`).
    *   `os`: Env, args, basic filesystem ops (functions return `OsError!T`).
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
