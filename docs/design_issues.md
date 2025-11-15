# Ryo Language Design Issues & Recommendations

This document identifies critical design inconsistencies in the Ryo language specification that must be resolved before implementation.

## Resolved Design Decisions

### Exit Code Handling ✅ RESOLVED (2025-01-10)

**Issue:** How should Ryo programs communicate exit codes to the operating system?

**Decision:** Programs exit with 0 (success) by default. Explicit return statements will control exit codes in Milestone 4+.

**Previous Behavior (Milestone 3.0):**
The last expression's value was used as the exit code:
```ryo
x = 42  # Program exited with code 42
```

**Why Changed:**
1. **No precedent:** No mainstream language uses this pattern (checked C, Rust, Go, Python, Ruby, Node.js, Zig, Swift)
2. **Confusing:** Assignments look like exit codes (`x = 42` suggests assignment, not "exit with 42")
3. **Future incompatible:** Conflicts with functions, types, error handling
   - What if last expression is a string? A function return? An error type?
   - `x = calculate()` - does this exit with calculate's return value?
4. **Violates principles:** "Explicit is better than implicit" (Python Zen)
5. **Type safety:** Can't enforce exit code types with implicit conversion

**Current Behavior (Milestone 3.1+):**
All programs exit with 0 (success):
```ryo
x = 42  # Evaluates to 42, program exits with code 0
y = 100  # Evaluates to 100, program exits with code 0
```

**Future Behavior (Milestone 4+):**
Rust-style return type annotations for explicit exit codes:
```ryo
# Implicit success (default)
fn main():
    x = 42  # Exits with 0

# Explicit exit code
fn main() -> int:
    if error_condition:
        return 1  # Error
    return 0      # Success
```

**Future Behavior (Milestone 7+):**
Integration with error handling:
```ryo
module app:
    error ConfigError
    error ConnectionFailed

fn main() -> (app.ConfigError | app.ConnectionFailed)!():
    config = try load_config()
    try connect_to_server(config)
    # Success: exit 0
    # Error: exit 1 + error message
```

**Rationale:**
- **Industry standard:** Aligns with Rust, Go, Python, C, Zig (all default to 0)
- **Type-safe:** Return type annotation enforces exit code types
- **Explicit:** Clear intent when non-zero exit codes are needed
- **Flexible:** Simple programs stay simple, complex programs have options
- **Future-proof:** Works with functions, types, and error handling
- **Developer expectations:** 0 = success is universal convention

**Implementation:**
- Changed in `src/codegen.rs` (lines 350-369)
- Always returns 0 by default
- Explicit returns deferred to Milestone 4 with function implementation

**References:**
- See research in plan mode analysis (2025-01-10)
- Industry survey: Rust (Termination trait), Go (implicit 0), Python (sys.exit), C (return 0)

---

### Module System Design ✅ RESOLVED (2025-01-11)

**Issue:** How should Ryo organize code into modules and packages? What visibility levels are needed?

**Decision:** Directory = Module, Three Access Levels, Hierarchical Structure, Package = ryo.toml project

#### **1. Directory = Module (vs File = Module)**

**Choice:** One directory = one module (all .ryo files in directory share namespace)

**Rationale:**
- **Go's Proven Model:** Go has used directory = package successfully for 15+ years
- **Simplicity:** No `mod` keyword declarations needed (implicit discovery)
- **Multi-File Modules:** Large modules can span multiple files naturally
- **Python Familiarity:** Similar to Python's package directories
- **Avoids Rust Confusion:** No `mod.rs` vs `file.rs` ambiguity (deprecated in Rust 2018)

**Comparison:**
| Approach | Languages | Pros | Cons | Ryo Choice |
|----------|-----------|------|------|------------|
| File = Module | Rust | Fine-grained control | Requires `mod` declarations, boilerplate | ❌ |
| Directory = Module | Go, Python | Simple, implicit | Less granular | ✅ |
| Build-defined | Zig | Explicit, no magic | Verbose, two-step process | ❌ |

**Trade-off Accepted:**
- Less granular than Rust's file = module
- Cannot have multiple small modules in same directory
- Accepted for simplicity and Python/Go developer familiarity

---

#### **2. Three Access Levels (vs 2, 4, or 6)**

**Choice:** `pub`, `package`, module-private (no keyword)

**Rationale:**

**Why NOT 2 levels (like Go/Zig)?**
- Go's limitation: Only Exported (capitalized) vs unexported (lowercase)
- Go developers create awkward `internal/` directory workarounds
- Cannot share code between modules without making it public
- Real pain point: 50+ upvoted Stack Overflow questions about this

**Why NOT 4+ levels (like Rust/Swift)?**
- Rust has 4: `pub`, `pub(crate)`, `pub(super)`, private
- Swift 6 has 6: `open`, `public`, `package`, `internal`, `fileprivate`, `private`
- Too complex for target audience (Python/Go developers)
- Diminishing returns: `pub(super)` and `fileprivate` rarely used

**Why 3 is the Sweet Spot:**
- **Historical Validation:** Java has 4 levels (public, protected, package, private)
  - But no inheritance in Ryo, so 3 levels sufficient
- **Swift 6 Validation:** Apple added `package` keyword in March 2025
  - Proves industry need for package-internal visibility
  - Validates our design before implementation!
- **Real-World Use Cases:**
  1. `pub` - Library public API (web framework, database driver)
  2. `package` - Internal shared code (config loader, logger, utils)
  3. Module-private - Implementation details (helpers, validation)

**Comparison Table:**

| Language | Levels | Public | Package/Crate | Module | File | Ryo Verdict |
|----------|--------|--------|---------------|--------|------|-------------|
| **Go** | 2 | Capitalized | `internal/` | lowercase | - | Too limiting |
| **Zig** | 2 | `pub` | - | private | - | Too limiting |
| **Rust** | 4 | `pub` | `pub(crate)` | `pub(super)` + private | - | Too complex |
| **Swift 6** | 6 | `public`/`open` | `package` | `internal` | `fileprivate` + `private` | Too complex |
| **Ryo** | 3 | `pub` | `package` | private | - | ✅ Just right |

**Why `package` instead of `pub(crate)`:**
- Ryo doesn't have "crates" (that's Rust terminology)
- Ryo has "packages" (defined by ryo.toml)
- `package` more intuitive for non-Rust developers
- Aligns with Swift 6's 2025 addition

**Trade-off Accepted:**
- No `pub(super)` for parent-only visibility (deferred to proposals)
- No file-level privacy (deferred, may never implement)
- Simpler mental model prioritized over maximum flexibility

---

#### **3. Hierarchical Modules (vs Flat Packages)**

**Choice:** Modules can contain submodules (`utils.math.geometry`)

**Rationale:**

**Go's Limitation: Flat Packages**
```go
// These are completely unrelated packages:
import "net"
import "net/http"  // No relationship to "net"!
```

Problems:
- No true namespaces
- Verbose import paths (`github.com/org/project/internal/database/postgres/connection`)
- Refactoring pain (moving packages breaks all imports)
- Forces awkward organization at scale

**Rust's Solution: Hierarchical Modules**
```rust
mod utils {
    pub mod math {
        pub mod geometry { }
    }
}
// utils contains math, math contains geometry
```

Benefits:
- Clear parent-child relationships
- Natural organization
- Can re-export from children

**Ryo's Approach: Directory-Based Hierarchy**
```
src/
  utils/           # Module "utils"
    core.ryo       # Part of utils
    math/          # Module "utils.math" (child of utils)
      basic.ryo    # Part of utils.math
```

**Key Design Decisions:**
1. **Parent can have files AND subdirectories** (unlike Go's file-only)
2. **Child is separate namespace** (not automatic visibility of parent private items)
3. **Must import parent explicitly** (no implicit `super::` like Rust)

**Why this design:**
- **Flexibility:** Like Rust's hierarchy
- **Simplicity:** Like Go's implicit discovery
- **Clean Separation:** Child doesn't pollute parent namespace
- **Explicit Imports:** No magic, clear dependencies

**Comparison:**
| Feature | Go | Rust | Python | Ryo |
|---------|----|----|--------|-----|
| Hierarchical? | ❌ Flat | ✅ Tree | ✅ Tree | ✅ Tree |
| Files + subdirs? | ❌ No | ✅ Yes | ✅ Yes | ✅ Yes |
| Child sees parent? | N/A | ✅ Implicit | ✅ Implicit | ❌ Must import |
| Discovery | Implicit | Explicit (`mod`) | Implicit (`__init__.py`) | Implicit (directory) |

**Trade-off Accepted:**
- Child must import parent explicitly (more verbose than Rust/Python)
- But clearer dependencies and no implicit access

---

#### **4. No Circular Dependencies Between Modules**

**Choice:** Forbid circular dependencies between modules (compile error)

**Rationale:**

**Go's Experience: Forbidden and Beneficial**
- Circular deps **always** indicate poor architecture
- Forces clearer design and better abstraction
- Makes compilation deterministic
- 10+ years of Go development proves this works

**The Classic Problem: User ↔ Post**
```ryo
# user/user.ryo
import post
struct User:
    posts: List[post.Post]

# post/post.ryo
import user
struct Post:
    author: user.User

# ✗ ERROR: Circular dependency!
```

**Standard Solutions (all acceptable):**

**Solution 1: Extract Common Types**
```ryo
# types/ids.ryo
struct UserID(int)
struct PostID(int)

# user/user.ryo
import types.ids
struct User:
    posts: List[ids.PostID]  # Just IDs

# post/post.ryo
import types.ids
struct Post:
    author_id: ids.UserID  # Just ID
```

**Solution 2: Merge Modules**
```ryo
# domain/models.ryo
struct User: ...
struct Post:
    author: User  # ✓ Same module
```

**Solution 3: Interface Abstraction**
```ryo
# interfaces/author.ryo
trait Author:
    fn get_id() -> int

# post/post.ryo
import interfaces
struct Post:
    author: interfaces.Author  # Interface, not concrete type
```

**Why Allow Within Module:**
```ryo
# server/http.ryo
fn start():
    routes.register()  # ✓ OK

# server/routes.ryo
fn register():
    http.start()  # ✓ OK - same module (server)
```

Files in same directory collaborate freely.

**Comparison:**
| Language | Between Packages/Crates | Within Module/Package | Ryo Choice |
|----------|------------------------|----------------------|------------|
| **Go** | ❌ Forbidden | ✓ Allowed | ✅ Same |
| **Rust** | ❌ Between crates | ✓ Within crate | ✅ Similar |
| **Python** | ✓ Allowed (runtime error) | ✓ Allowed | ❌ Too permissive |
| **Zig** | ✓ Allowed | ✓ Allowed | ❌ Too permissive |

**Rationale:**
- **Architecture Quality:** Forces good design (proven by Go)
- **Compilation Speed:** Deterministic order enables parallelization
- **Clear Dependencies:** No spaghetti code
- **Within Module Flexibility:** Files collaborate without barriers

**Trade-off Accepted:**
- Sometimes requires refactoring (extract types, merge modules)
- But results in better architecture

---

#### **5. Implicit Module Discovery (vs Explicit Declaration)**

**Choice:** No `mod` keyword needed, directories auto-discovered

**Rationale:**

**Rust's Verbosity:**
```rust
// Must explicitly declare every submodule
mod server;   // Loads server.rs or server/mod.rs
mod database; // Loads database.rs or database/mod.rs
```

**Zig's Build System Requirement:**
```zig
// build.zig
const server = b.addModule("server", .{
    .root_source_file = .{ .path = "src/server.zig" }
});
```

**Ryo/Go/Python Simplicity:**
```
src/
  server/     # Automatically a module, no declaration needed
  database/   # Automatically a module, no declaration needed
```

**Benefits:**
- **Less Boilerplate:** No per-module declarations
- **Obvious from File Structure:** Directory layout is module layout
- **Faster Iteration:** Just create directory and start coding
- **Familiar:** Python and Go developers expect this

**Trade-offs:**
- **Less Explicit:** Can't selectively expose/hide submodules (deferred to `pub use` proposal)
- **No Renaming:** Module name = directory name (but can alias on import)
- **All-or-Nothing:** All .ryo files in directory are part of module

**Comparison:**
| Language | Discovery | Flexibility | Boilerplate | Ryo Choice |
|----------|-----------|-------------|-------------|------------|
| **Rust** | Explicit `mod` | High (can hide) | High | ❌ |
| **Go** | Implicit (dir) | Low (all visible) | None | ✅ |
| **Zig** | Build system | High (can rename) | Very high | ❌ |
| **Python** | Implicit (`__init__.py`) | Medium | Low | ✅ Similar |

**Rationale:** Python/Go developers are the target audience. They expect implicit discovery.

---

#### **Summary of Decisions**

| Aspect | Choice | Alternative Considered | Why Rejected |
|--------|--------|----------------------|--------------|
| **Module Unit** | Directory | File (Rust) | Too much boilerplate |
| **Access Levels** | 3 (pub, package, private) | 2 (Go/Zig) | Too limiting |
| | | 4+ (Rust/Swift) | Too complex |
| **Hierarchy** | Tree (parent contains child) | Flat (Go) | Poor organization at scale |
| **Circular Deps** | Forbidden between modules | Allowed (Python) | Spaghetti code |
| **Discovery** | Implicit (directory) | Explicit (Rust/Zig) | Too verbose |
| **Package Term** | `package` keyword | `pub(crate)` (Rust) | Ryo doesn't have crates |

**Validation:**
- **Swift 6 (March 2025):** Added `package` keyword - validates our design!
- **Go (15+ years):** Directory = package works at massive scale
- **Rust (2018 edition):** Deprecated `mod.rs` for simpler file structure

**Implementation Timeline:**
- Specification: ✅ Complete (2025-01-11)
- Implementation: Milestone 5 (Basic imports), Milestone 6 (Access checking)

**References:**
- Research: Go, Rust, Zig, Swift 6, Python module systems (2025-01-11)
- Swift 6 package keyword: https://github.com/swiftlang/swift-evolution/blob/main/proposals/0386-package-access-modifier.md
- Go package system analysis: Stack Overflow top issues, HN discussions
- Rust module system evolution: RFC 2126, 2018 edition changes

---

### Struct Literal Syntax ✅ RESOLVED (2025-11-15)

**Issue:** Should struct literals use braces `Point{x: 1, y: 2}` (like Rust/Go) or parentheses with equals `Point(x=1, y=2)` (like Python/Swift)?

**Decision:** Parentheses with named arguments: `Point(x=1, y=2)`

**Rationale:**

#### **1. Consistency with "No Braces" Philosophy**

Ryo's core design principle: **braces are reserved exclusively for f-string interpolation**

```ryo
# Braces ONLY for f-strings
name = "Alice"
print(f"Hello {name}!")

# Struct literals use parentheses
point = Point(x=1.0, y=2.0)
user = User(name="Alice", age=30)
```

**Why this matters:**
- Maintains visual consistency across the language
- Aligns with Python-style syntax (colons, indentation, no braces for code blocks)
- Prevents "slippery slope" - if structs use braces, why not functions?

#### **2. Target Audience: Python Developers**

Python developers expect parentheses for object construction:

```python
# Python dataclass
from dataclasses import dataclass

@dataclass
class Point:
    x: float
    y: float

point = Point(x=1.0, y=2.0)  # Parentheses with named args
```

Ryo mirrors this familiar pattern:
```ryo
struct Point:
    x: float
    y: float

point = Point(x=1.0, y=2.0)  # Same syntax!
```

#### **3. Modern Language Precedent**

Languages with **clean, modern syntax** use parentheses:

| Language | Struct Literal Syntax | Philosophy Match |
|----------|----------------------|------------------|
| **Swift** | `Point(x: 1, y: 2)` | ✅ Clean syntax, parentheses |
| **Kotlin** | `Point(x = 1, y = 2)` | ✅ Clean syntax, parentheses |
| **Python** | `Point(x=1, y=2)` | ✅ Clean syntax, parentheses |
| **Rust** | `Point { x: 1, y: 2 }` | ❌ C-like syntax, braces |
| **Go** | `Point{X: 1, Y: 2}` | ❌ C-like syntax, braces |
| **Zig** | `Point{ .x = 1, .y = 2 }` | ❌ C-like syntax, braces |

**Observation**: Languages with C-like syntax use braces. Languages with cleaner, more expressive syntax use parentheses.

#### **4. Consistency with Type Conversion**

Specification (Section 3) defines type conversion using parentheses:

```ryo
# Type conversion uses parentheses
x = int(3.14)      # Convert float to int
y = float(42)      # Convert int to float

# Struct "construction" should match
point = Point(x=1.0, y=2.0)  # Construct a Point type
```

**Same pattern**: Calling a type name with arguments.

#### **5. Unified Enum Syntax**

Initially, enum struct variants used braces, creating inconsistency:

```ryo
# BEFORE (inconsistent)
struct Point:
    x: int
    y: int

enum Message:
    Coords { x: int, y: int }  # Braces!

point = Point{x: 1, y: 2}       # Braces
msg = Message.Coords { x: 1, y: 2 }  # Braces
```

**Resolution**: Unify both to use parentheses:

```ryo
# AFTER (consistent)
struct Point:
    x: int
    y: int

enum Message:
    Coords(x: int, y: int)  # Parentheses!

point = Point(x=1, y=2)       # Parentheses
msg = Message.Coords(x=1, y=2)  # Parentheses

# Pattern matching also uses parentheses
match msg:
    Message.Coords(x, y):  # Consistent!
        print(f"x={x}, y={y}")
```

**Benefits**:
- Single syntax for all product types (structs, enum variants)
- Construction and destruction use same syntax
- Simpler mental model for developers

#### **6. Parser Simplicity**

**Parentheses approach:**
- Parentheses already used for: tuples, function calls, grouping, type conversion
- No context switching between `()` and `{}`
- Fewer lexical tokens to track
- Named arguments (`=`) clearly distinguish from function calls

**Braces approach would require:**
- Context-dependent parsing (f-string braces vs struct braces vs enum braces)
- More complex error messages
- Special cases in the parser

#### **Trade-offs Accepted**

**Against braces:**
- Different from Rust/Go (but Ryo targets Python devs, not systems programmers)
- Slightly less common in systems languages (but more common in modern high-level languages)

**For parentheses:**
- Philosophically consistent
- Target-audience familiar
- Simpler implementation
- Unified across language features

**Implementation Changes (2025-11-15):**

Fixed 17 documentation inconsistencies:
- `docs/getting_started.md` - 4 instances
- `docs/implementation_roadmap.md` - 6 instances
- `docs/specification.md` - 4 instances (struct + enum syntax)
- `CLAUDE.md` - 3 instances (error example + quick ref + version)

**References:**
- Language comparison research (Swift, Kotlin, Python, Rust, Go, Zig)
- CLAUDE.md Section 5 (Syntax Conventions)
- docs/specification.md Section 4.5 (Struct Type) and 4.6 (Enum Type)

---

## Critical Issues Requiring Immediate Resolution

### 1. Tuple Syntax Ambiguity 🔴

**Problem**: Identical syntax `(...)` used for multiple contexts causes parser ambiguity.

**Examples**:
```ryo
# Type annotation
fn foo() -> (int, str):
    ...

# Value literal
x = (42, "hello")

# Function parameter grouping
fn bar(param: (int, str)):
    ...
bar((42, "hello"))  # One tuple arg or two separate args?

# Expression grouping
result = (a + b) * c

# Empty tuple vs unit
empty = ()
fn unit_func() -> ():
    ...
```

**Recommendation**:
- Use different syntax for unit type: `fn unit_func() -> unit:`
- Or use explicit tuple constructors: `tuple(42, "hello")`
- Or require trailing comma for single-element tuples: `(42,)`

### 2. Implicit Borrow vs Move Inconsistency 🔴

**Problem**: Function parameters default to borrowing while assignments default to moving.

**Examples**:
```ryo
# Assignment: MOVES
new_var = old_var  # old_var invalid

# Function call: BORROWS
fn process(data: SomeType):
    ...
process(my_data)  # my_data still valid

# This creates confusion:
data = create_data()
process(data)      # Borrows - OK
other = data       # Moves - data invalid!
process(data)      # ERROR: use of moved value
```

**Recommendation**:
- **Option A**: Make everything explicit - remove implicit borrowing
  ```ryo
  fn process(data: &SomeType):  # Explicit borrow
      ...
  fn consume(data: SomeType):   # Explicit move
      ...
  ```
- **Option B**: Make assignment borrowing more explicit
  ```ryo
  other = move data  # Explicit move
  other = data       # Implicit borrow (like function params)
  ```

### 3. Keywords vs Types Conflict 🔴

**Problem**: `Result`, `Optional`, `Ok`, `Err`, `Some` are listed as keywords but used as types.

**Examples**:
```ryo
# Cannot create identifiers with these names
struct MyResult: ...  # ERROR: 'Result' is keyword

# But used as types everywhere
fn parse() -> ParseError!int:
    ...
```

**Recommendation**: 
- Remove `Result`, `Optional`, `Ok`, `Err`, `Some` from keywords list
- Treat them as built-in types resolved by type checker
- Allow users to shadow these names if needed

### 4. Generic Syntax Undefined 🔴

**Problem**: Generics used throughout spec (`List[T]`, `Map[K,V]`) but syntax never defined.

**Examples**:
```ryo
# Used in spec but undefined:
List[T], Map[K,V]

# How to define?
struct MyStruct[T]: ...?     # Unclear syntax
fn generic_fn[T](...): ...?  # Unclear syntax
```

**Recommendation**: Define complete generic syntax:
```ryo
# Generic struct
struct Container[T]:
    value: T

# Generic function
fn identity[T](x: T) -> T:
    return x

# Generic enum
error ApiResponse[T]:
    Success(T)
    Failure(str)

# With trait bounds (future)
fn sort[T: Comparable](list: List[T]):
    ...
```

### 5. Method Self Parameter Confusion 🔴

**Problem**: `mut self` meaning unclear - mutable borrow or owned value?

**Examples**:
```ryo
impl Counter:
    fn increment(mut self):  # Borrow or move?
        self.count += 1

    fn drop(mut self): ...   # Drop must take ownership

# Usage unclear:
counter.increment()  # Does counter still exist?
```

**Recommendation**: Use Rust-like explicit syntax:
```ryo
impl Counter:
    fn increment(&mut self):     # Mutable borrow - clear
        self.count += 1

    fn consume(self):            # Take ownership - clear
        # ...

    fn drop(self): ...           # Drop takes ownership
```

### 6. Error Handling with Automatic Composition ✅ RESOLVED

**Previously**: Developers had to create wrapper error types when composing functions with different error types, creating boilerplate (the "wrapper problem").

**Solution Implemented**: Comprehensive error union system inspired by Zig with improvements from Swift and Rust.

#### Key Features:

1. **Single-Variant Errors Only** (simplified design):
   ```ryo
   error Timeout                          # Unit error
   error NotFound(str)                    # Message-only error
   error HttpError(status: int, message: str)  # Structured error
   ```

2. **Module-Based Grouping** (organize related errors):
   ```ryo
   module math:
       error DivisionByZero
       error InvalidInput(message: str)
       error OverflowError
   ```

3. **Error Union Types** (automatic composition from `try`):
   ```ryo
   # Explicit union - manually specified
   fn process() -> (FileError | ParseError)!Data:
       file = try read_file(path)
       data = try parse(file)
       return data

   # Inferred union - compiler automatically infers from try expressions
   fn process() -> !Data:
       file = try read_file(path)      # FileError
       data = try parse(file)          # ParseError
       return data
   # Compiler infers: (FileError | ParseError)!Data
   ```

4. **Error Trait** (automatic message generation):
   ```ryo
   # All errors implement Error trait with .message() method
   result = fetch_resource(url) catch |e|:
       print(e.message())  # Automatic or custom message
       return
   ```

5. **Error Propagation** (no wrapper boilerplate):
   ```ryo
   # Before (wrapper boilerplate):
   error AppError:
       Http(HttpError)
       Io(IoError)

   # After (automatic composition):
   fn fetch_and_save() -> !():
       data = try http.get("...")     # Different errors
       try files.write(data)           # Automatically composed
       return
   # Compiler infers: (HttpError | IoError)!()
   ```

6. **Pattern Matching (Exhaustive by Default)**:
   - **Single error types**: Exhaustive matching required (all variants must be handled)
   - **Error unions**: Exhaustive matching required (compiler enforces handling all types in union)
   - **Catch-all pattern**: Use `_` when you want generic handling for unspecified errors

#### Examples:

**Single error type (exhaustive):**
```ryo
result = divide(10.0, 0.0) catch |e|:
    match e:
        math.DivisionByZero:
            print("Cannot divide by zero")
    return
# MUST handle the single error type
```

**Error union (exhaustive matching):**
```ryo
result = complex_operation() catch |e|:
    match e:
        math.DivisionByZero:
            print("Cannot divide by zero")
        math.InvalidInput(msg):
            print(f"Invalid: {msg}")
        io.FileNotFound(path):
            print(f"File not found: {path}")
        parse.InvalidJson(reason):
            print(f"Parse error: {reason}")
    return
# MUST handle all error types in the union (unless using catch-all)
```

**Using catch-all for generic handling:**
```ryo
result = complex_operation() catch |e|:
    match e:
        math.DivisionByZero:
            print("Math error!")
        _:  # Explicit catch-all: handle all other errors the same way
            log_error(e.message())
            print("Generic error occurred")
    return
```

#### Benefits:
- ✅ **Maximum simplicity**: Single-variant only (one syntax to learn)
- ✅ **Zero boilerplate**: No wrapper types, no multi-variant boilerplate
- ✅ **Type safety**: All errors explicitly tracked by type system
- ✅ **Composability**: Functions naturally compose without explicit error mapping
- ✅ **Safety**: Exhaustive matching by default ensures all error paths are handled
- ✅ **Ergonomic**: `try` keyword for propagation, `catch` for handling
- ✅ **Explicit handling**: `try`/`catch` makes all error paths visible in code
- ✅ **Zig-inspired**: Simple error sets (like Zig) with payload support (unlike Zig)

#### Key Safety Features:
- **No direct unwrap**: Error values cannot be used without `try`/`catch` or propagation
- **Exhaustive matching**: Compiler requires handling all error types in a union (or explicit catch-all)
- **Automatic inference**: Compiler tracks error types and infers unions automatically
- **Module namespacing**: Related errors organized in modules (not multi-variant syntax)
- **Message support**: All errors automatically implement `.message()` method
- **From trait**: Allows explicit cross-layer error conversion when needed

This design achieves **maximum simplicity** (single-variant errors only with module grouping) while maintaining **strong safety guarantees** (exhaustive matching by default). It eliminates the "wrapper problem" without requiring multi-variant syntax, inspired by Zig's philosophy of simplicity.

## Moderate Issues

### 7. Async Main Function Undefined ⚠️

**Problem**: Examples show `async fn main()` but spec only mentions sync main.

**Recommendation**: Define async main semantics:
```ryo
# Option A: Explicit runtime
fn main():
    async_runtime.run(async_main())

async fn async_main() -> AppError!():
    ...

# Option B: Compiler magic
async fn main() -> AppError!():  # Compiler starts runtime
    # ...
```

### 8. Channel Operators Listed as Current but in Future ⚠️

**Problem**: `<-` operator listed in current spec but channels are in proposals.md.

**Recommendation**: Remove `<-` from current operator list, add back when CSP is implemented.

### 9. Static Dispatch Only Limitation ⚠️

**Problem**: No dynamic dispatch limits Python-like polymorphism patterns.

**Recommendation**: Consider trait objects for future:
```ryo
# Future syntax for dynamic dispatch
trait Drawable:
    fn draw(self)

fn process_shapes(shapes: List[&dyn Drawable]):
    for shape in shapes:
        shape.draw()  # Dynamic dispatch
```

### 10. Array vs Slice Type Ambiguity ⚠️

**Problem**: `[T]` syntax meaning unclear - array or slice?

**Recommendation**: Define clear syntax:
```ryo
[T; N]    # Fixed-size array of N elements
[T]       # Dynamic array (List[T])  
&[T]      # Slice (borrowed view)
```

## Resolution Status

**✅ Resolved:**
3. ~~Remove type names from keywords~~ - Removed `Result`, `Optional`, `Ok`, `Err`, `Some` from keywords
4. ~~Define generic syntax completely~~ - Moved detailed syntax to proposals.md
5. ~~Clarify method self parameters~~ - Applied explicit `&self`, `&mut self`, `self` syntax
6. ~~Design error trait system~~ - Implemented `error` keyword, `try`/`catch` operators, `From` trait
7. ~~Define async main function~~ - Specified sync main only with explicit runtime calls
8. ~~Resolve operator inconsistencies~~ - Removed channel operators from current spec
9. ~~Consider dynamic dispatch options~~ - Added future trait objects plan

**🔄 Deferred for Review:**
1. Fix tuple syntax ambiguity - Keep in file for later review
2. Resolve borrow/move inconsistency - Keep in file for later review
10. Clarify array/slice syntax - Keep in file for later review

## Design Decision: Stack Traces vs Performance Trade-Off ✅ RESOLVED

**Decision**: Ryo prioritizes **debugging capability over raw performance**. Stack traces are **always captured and included by default**.

### The Trade-Off

**Option A: Always Capture Stack Traces** ✅ **CHOSEN**
- **Pros:**
  - Best debugging experience - developers can always see where errors occurred
  - Transparent - no configuration needed to get debugging info
  - Consistent behavior across all deployments
  - Python-like developer experience (always have traceback)
  - Fault analysis is immediate without reproducing issues
- **Cons:**
  - ~5-10% runtime overhead (estimate, varies by workload)
  - ~20-30% larger binary size (due to DWARF debug symbols)
  - Memory overhead for maintaining stack frame information

**Option B: Debug-Only Stack Traces**
- **Pros:**
  - Zero overhead in release builds
  - Better performance in production
  - Smaller release binaries (no debug symbols)
- **Cons:**
  - Different behavior between debug and release builds (confusing)
  - Production crashes lack debugging info (must reproduce with debug build)
  - More complex tooling (need separate debug and release artifacts)
  - Common source of "works in debug, fails in production" issues

**Option C: Opt-In Stack Traces**
- **Pros:**
  - Zero cost when disabled
  - Developers choose trade-off per application
- **Cons:**
  - Easy to forget to enable for troubleshooting
  - Inconsistent debugging across projects
  - Defeats purpose if not enabled when needed
  - Complexity - must manage feature flag

**Option D: No Stack Traces**
- **Pros:**
  - Zero overhead, maximum performance
  - Minimal complexity in runtime
- **Cons:**
  - Worst debugging experience
  - Requires external tools (debuggers, logging)
  - Poor for production incident response
  - Contradicts Ryo's goal of being "easy to debug"

### Rationale for Chosen Approach

Ryo's design philosophy emphasizes **developer productivity and debugging** over micro-optimization. The reasoning:

1. **Developer Time is Expensive**: The 5-10% performance cost is often worth 10x faster debugging
2. **Production Incidents**: When systems fail in production, having stack traces is invaluable
3. **Zero Configuration**: No environment variables or flags needed for normal debugging
4. **Consistency**: Same behavior in all build modes - no surprises
5. **Python Heritage**: Ryo targets developers from Python who expect always-available tracebacks
6. **Real-World Data**: Profile before optimizing - most applications won't be bottlenecked by stack trace overhead

### Implementation Details

- **DWARF Format**: Debug symbols generated via Cranelift backend
- **Always Included**: No compiler flag needed
- **Strippable**: `--strip` flag can remove debug symbols for production if needed (not recommended)
- **Controllable Output**: `RYOLANG_BACKTRACE` environment variable controls verbosity
- **Accessible at Runtime**: `.location()` and `.stack_trace()` methods available on errors

### When This Trade-Off Makes Sense

This trade-off is appropriate for Ryo because:

1. **Target Use Cases**: Web services, CLI tools, data processing - rarely performance-limited by stack traces
2. **Development Speed**: Most projects prioritize fast debugging over 5% performance gain
3. **Production Operations**: Stack traces from production errors are worth the cost
4. **Ecosystem Compatibility**: Aligns with Python/JavaScript/Go patterns (always have stack traces)

### When This Might NOT Make Sense

For applications where this trade-off is problematic:
- Extreme real-time systems (sub-millisecond latencies)
- Embedded systems with very tight memory constraints
- Performance-critical inner loops running billions of times

**Mitigation**: Use `--strip` to remove debug symbols, or profile to verify stack traces aren't actually the bottleneck.

### Comparison with Other Languages

| Language | Stack Traces | Default | Trade-off |
|----------|--------------|---------|-----------|
| **Ryo** | ✅ Always | Always captured | Performance < Debugging |
| Python | ✅ Always | Always captured | Performance < Debugging |
| Go | ✅ Always | Always captured | Performance < Debugging |
| Rust | ✅ With RUST_BACKTRACE | Opt-in | Performance > Debugging |
| C/C++ | ❌ Manual setup | Manual logging | Performance > Debugging |
| Java | ✅ Always | Always captured | Performance < Debugging |

Ryo aligns with Python/Go (developer-friendly) rather than Rust (performance-optimized).

### Future Flexibility

While the v1.0 decision is firm, future versions can provide:
- Lazy stack trace materialization (only capture on error)
- Sampling profilers for production monitoring
- Conditional compilation to disable in extremely hot code paths
- Zero-cost abstractions for performance-critical sections

The decision is not irrevocable, but the default remains: **debugging capability first**.

## Next Steps (Remaining Issues)

**Before Implementation Begins:**
1. Review tuple syntax ambiguity (kept for future consideration)
2. Review borrow/move inconsistency (kept for future consideration)
6. Review error trait system design (kept for future consideration)
10. Review array/slice syntax (kept for future consideration)

## Next Steps

1. Create detailed syntax specification resolving these ambiguities
2. Update all documentation to use consistent syntax
3. Create grammar specification (EBNF) to catch remaining conflicts
4. Implement parser with clear error messages for edge cases