# Ryo Implementation Roadmap

This roadmap outlines the planned development of the Ryo programming language compiler and runtime. Each milestone focuses on delivering specific, tangible capabilities while building toward a complete language implementation.

**Development Timeline:** Each milestone is designed for approximately 2-4 weeks of development (assuming ~8 hours/week), but timelines should remain flexible to ensure quality over speed.

## Current Status

| Milestone | Status | Notes |
|-----------|--------|-------|
| **Phase 1: Core Foundation** | | |
| Milestone 1: Lexer Basics | ✅ **COMPLETE** | All tokens implemented and tested. `ryo lex` command fully functional. |
| Milestone 2: Parser & AST | ✅ **COMPLETE** | Full AST implemented with variable declarations, type annotations, and expressions. `ryo parse` command functional. 32 unit tests + 5 integration tests. |
| Milestone 3: Cranelift Integration | ✅ **COMPLETE (AOT)** | Full AOT compilation pipeline working. `ryo run` compiles and executes programs. 15 codegen integration tests + `ryo ir` command for IR display. Total: 47 tests passing. |
| **Phase 2: Essential Language Features** | | |
| Milestone 4: Functions & Calls | ⏳ Planned | Define and call functions with arguments and return values. |
| Milestone 5: Module System (Design) | ✅ **COMPLETE** | Directory-based modules with three access levels designed and documented. |
| Milestone 6: Expressions & Operators | ⏳ Planned | Arithmetic, comparison, logical operators. Float type. |
| Milestone 6: Control Flow & Booleans | ⏳ Planned | if/else, while loops, break/continue, boolean logic. |
| Milestone 7: Structs | ⏳ Planned | User-defined composite types with named fields. |
| Milestone 8: Enums (ADTs) | ⏳ Planned | Algebraic data types with variants. |
| Milestone 9: Pattern Matching | ⏳ Planned | Match expressions with exhaustiveness checking. |
| Milestone 10: Tuples | ⏳ Planned | Multiple return values, tuple destructuring. |
| Milestone 11: Error Types & Unions | ⏳ Planned | Error types, error unions, exhaustive error handling. |
| **Phase 3: Type System & Memory Safety** | | |
| Milestone 12: Basic Ownership & String | ⏳ Planned | Move semantics, String type, Copy trait concept. |
| Milestone 13: Optional Types (`?T`) | ⏳ Planned | Null safety with optional types, chaining, orelse. |
| Milestone 14: Traits | ⏳ Planned | Behavior abstraction, trait definitions and implementations. |
| Milestone 15: Method Implementations | ⏳ Planned | Methods on types via impl blocks. |
| Milestone 16: Immutable Borrows | ⏳ Planned | Borrow checking with `&T` for immutable references. |
| Milestone 17: Slices & String Slices | ⏳ Planned | Array slices `&[T]` and string slices `&str`. |
| Milestone 18: Collections | ⏳ Planned | List[T] and Map[K, V] with hardcoded types. |
| Milestone 19: RAII & Drop Trait | ⏳ Planned | Automatic resource cleanup, Drop trait. |
| Milestone 20: Mutable Borrows | ⏳ Planned | Mutable references `&mut T` with aliasing rules. |
| Milestone 21: Try/Catch Operators | ⏳ Planned | Error propagation with try, handling with catch. |
| **Phase 4: Module System & Core Ecosystem** | | |
| Milestone 22: Module System | ⏳ Planned | Modules, imports, visibility control. |
| Milestone 23: Standard Library Core | ⏳ Planned | Essential stdlib: io, string, collections, math, os. |
| Milestone 24: Panic & Debugging | ⏳ Planned | Panic function, stack traces, debugging support. |
| Milestone 25: Testing & Documentation | ⏳ Planned | Test framework, test runner, doc generation. |
| Milestone 26: Core Language Complete | ⏳ Planned | Integration, polish, package manager, v0.1.0 prep. |
| **Phase 5: Post-v0.1.0 Extensions** | | |
| Async/Await, FFI, Generics | 🔮 Future | Advanced features deferred to v0.2+. See proposals.md. |

## Guiding Principles

* **Iterate:** Get something working end-to-end quickly, then refine
* **Test Early, Test Often:** Integrate basic testing from the start
* **Focus:** Each milestone adds a specific, visible capability
* **Simplicity First:** Implement the simplest version that meets the immediate goal
* **Quality of Life:** Include documentation, basic error reporting, and simple tooling

## Phase 1: Core Foundation

### Milestone 1: Setup & Lexer Basics ✅ COMPLETE

**Goal:** Parse basic Ryo syntax into tokens

**Tasks:**
- ✅ Set up Rust project (`cargo new ryo_compiler`)
- ✅ Add dependencies (`logos`, `chumsky`, `clap`)
- ✅ Define core tokens (`Token` enum in `src/lexer.rs`) using `logos`:
  - ✅ Keywords: `fn`, `if`, `else`, `return`, `mut`, `struct`, `enum`, `match`
  - ✅ Identifiers, integer literals, basic operators (`=`, `+`, `-`, `*`, `/`, `:`)
  - ✅ Punctuation: `(`, `)`, `{`, `}` (braces reserved for f-string interpolation in later milestones)
  - ✅ Handle whitespace/comments (Python-style `#` comments)
- ✅ Write comprehensive tests for the lexer (19 unit tests)
- ✅ Create simple CLI harness (`src/main.rs`) using `clap`

**Visible Progress:** `ryo lex <file.ryo>` prints token stream ✅

**Completion Date:** November 9, 2025
**Implementation Details:**
- All Milestone 1 keywords and operators successfully tokenized
- Comments handled correctly (skipped from token stream)
- Comprehensive test suite covers edge cases (keyword keyword-as-part-of-identifier distinction, comment handling, etc.)
- CLI tested with realistic Ryo code samples
- **Design Decision:** Struct literals use parentheses with named arguments `Point(x=1, y=2)`, not braces. Curly braces are reserved exclusively for f-string interpolation (e.g., `f"Hello {name}"`) which will be implemented in later milestones.

### Milestone 2: Parser & AST Basics ✅ COMPLETE

**Goal:** Parse simple variable declarations and integer literals into an Abstract Syntax Tree

**Tasks:**
- ✅ Define basic AST nodes in `src/ast.rs`:
  - ✅ `struct Program`, `struct Statement`, `enum StmtKind::VarDecl`
  - ✅ `struct Expression`, `enum ExprKind::Literal`, `struct Ident`, `struct TypeExpr`
  - ✅ Include spans (`chumsky::SimpleSpan`)
  - ✅ Added `BinaryOperator` and `UnaryOperator` enums for expression support
- ✅ Implement parser using `chumsky` (`src/parser.rs`)
- ✅ Parse variable declarations with pattern: `[mut] ident [: type] = expression`
- ✅ Support full expressions including binary ops (+, -, *, /) and unary ops (-)
- ✅ Integrate parser with lexer output in `main.rs`
- ✅ Update CLI: `ryo parse <file.ryo>` prints generated AST
- ✅ Write comprehensive parser tests (32 unit tests)

**Visible Progress:** `ryo parse <file.ryo>` shows structure of variable declarations ✅

**Completion Date:** November 9, 2025

**Implementation Details:**
- Complete AST refactor with proper span tracking throughout
- Supports multiple variable declarations in a single file
- Expression parser handles operator precedence correctly
- Pretty-print implementation for debugging AST structure
- Full integration test coverage for parse command
- Example files in `examples/milestone2/` directory

**Test Results:**
- 32 parser unit tests (all passing)
- 5 integration tests for parse/lex commands (all passing)
- Total: 37 tests passing

**Design Decisions:**
- Full rewrite approach for cleaner AST foundation
- Struct literals use named arguments: `Point(x=1, y=2)` (not braces)
- Curly braces reserved for f-string interpolation (future milestone)
- Supports both explicit type annotations and implicit type inference
- Expression initializers support full arithmetic expressions

### Milestone 3: "Hello, Exit Code!" (Cranelift Integration) ✅ COMPLETE

**Goal:** Compile minimal Ryo program to native code that returns an exit code

**Status:** ✅ COMPLETE - Full AOT (Ahead-of-Time) compilation pipeline implemented

**Tasks:**
- ✅ Add `cranelift`, `cranelift-module`, `cranelift-jit` dependencies (note: JIT deferred to future milestone)
- ✅ Create basic code generation module (`src/codegen.rs`) - 158 lines, fully functional
- ✅ Implement logic to translate `VarDecl` AST into Cranelift IR
- ✅ Support all expression types: literals, binary ops (+, -, *, /), unary negation
- ✅ Generate code for main function that loads value and returns it
- ✅ Use `cranelift-object` to write object files (.o on Unix, .obj on Windows)
- ✅ Update CLI: `ryo run <file.ryo>` compiles and runs code
- ✅ Add new CLI command: `ryo ir <file.ryo>` displays IR generation info
- ✅ Implement full linking pipeline with multi-linker fallback (zig cc → clang → cc)
- ✅ Comprehensive testing: 15 integration tests for codegen

**Visible Progress:** `ryo run my_program.ryo` executes and exits with specified code ✅ (**Major milestone!**)

**Completion Date:** November 9, 2025

**Implementation Details:**
- Full AOT compilation pipeline: Source → Lex → Parse → Codegen → Link → Execute
- Generates position-independent code (PIC) for portability
- Handles multiple statements (returns last expression value)
- Proper exit code handling (Unix: 0-255, but computed values can be any i64 that gets truncated)
- Example files in `examples/milestone3/` demonstrating all features
- All generated files (object files, executables) cleaned after execution

**Test Results:**
- 15 new integration tests for `ryo run` command (all passing)
- Tests cover: simple literals, arithmetic, parentheses, multiple statements, type annotations, mutable variables, negation
- Total test count: 32 lexer tests + 32 parser tests + 15 codegen tests = 79 tests (all passing)

**Features Implemented:**
- ✅ Variable declarations with optional type annotations
- ✅ Mutable variable declarations (`mut` keyword)
- ✅ Arithmetic operators: +, -, *, / with correct precedence
- ✅ Unary negation operator (-)
- ✅ Parenthesized expressions
- ✅ Multiple statements per program
- ✅ Proper exit code return
- ✅ Cross-platform support (Unix/Windows/macOS)

**Design Decisions:**
- AOT only (JIT not implemented, deferred to future milestone for REPL)
- **Exit codes:** All programs exit with 0 (success) by default - explicit returns coming in Milestone 4
- Object file and executable remain in current directory after execution
- `ryo ir` command provides IR generation confirmation (full IR display requires deeper Cranelift integration)

**Design Change (2025-01-10):**
Exit code behavior changed from "last expression value" to "implicit 0 (success)". This aligns with industry standards (Rust, Go, Python, C) and prepares for Milestone 4's return statements. See `docs/design_issues.md` for detailed rationale. Previous behavior was confusing and incompatible with future features.

**What's NOT Implemented (Deferred):**
- ❌ JIT compilation (for REPL)
- ❌ Direct IR display (technical limitation: would require pub access to Cranelift Context display)
- ✅ String support → **Implemented in Milestone 3.5**
- ❌ Functions (beyond main)
- ❌ Control flow
- ❌ Error handling

### Milestone 3.5: "Hello, World!" (String Literals & Print) ✅ COMPLETE

**Goal:** Add string literals and print() function for debugging and visible output

**Status:** ✅ COMPLETE - String literals and print syscall implemented

**Tasks:**
- ✅ Extend lexer to tokenize string literals with escape sequences
- ✅ Update AST to support `Literal::Str` and `ExprKind::Call`
- ✅ Implement parser support for string literals and function call expressions
- ✅ Add data section support in codegen for storing string constants
- ✅ Implement print() as libc write() call (fd=1, stdout)
- ✅ Add platform detection (macOS/Linux support)
- ✅ Create hello world example program
- ✅ Add integration tests for print functionality

**Visible Progress:** `print("Hello, World!")` actually works! Real visible output! ✅

**Completion Date:** January 10, 2025

**Implementation Details:**
- String literals stored in `.rodata` section with deduplication
- Escape sequence support: `\n`, `\t`, `\r`, `\\`, `\"`, `\0`
- Print implemented via external libc `write()` function call
- Platform support: macOS (Darwin), Linux
- Simple approach: string literals only (no variables yet), no heap allocation

**Test Results:**
- 4 new integration tests for print functionality (all passing)
- Tests cover: hello world, newlines, multiple prints, empty strings
- Total test count: 38 unit tests + 19 integration tests = 57 tests (all passing)

**Features Implemented:**
- ✅ String literal parsing with escape sequences
- ✅ Call expression syntax: `print("message")`
- ✅ Data section management for strings
- ✅ libc write() syscall integration
- ✅ Platform-specific support (Unix-like systems)

**Design Decisions:**
- String literals as compile-time constants only (no runtime heap allocation)
- print() accepts only string literals (not variables)
- No ownership semantics (deferred to Milestone 12)
- Simple 3-5 day implementation vs full ownership (2-3 weeks)

**Example:**
```ryo
# print() returns void (nothing) - use _ to indicate value is ignored
_ = print("Hello, World!")
_ = print("First\n")
_ = print("Second\n")
```

**Known Limitations:**
- **Return Type**: print() currently returns int(0) as a placeholder for the future void/unit type
  - This value is semantically meaningless and should be ignored
  - Proper void/unit type will be implemented in Milestone 6 (Type System)
  - Aligns with Python's `None` and Rust's `()` conventions
- **Parser Limitation**: Bare expression statements not supported yet
  - Must use assignment syntax: `_ = print("...")`
  - Expression statements will be added in Milestone 4 (Functions & Calls)
- **Usage Pattern**: Use `_` as variable name to indicate the value is intentionally unused (Rust convention)

**What's NOT Implemented (Deferred):**
- ❌ String variables (requires ownership model)
- ❌ String concatenation or manipulation
- ❌ Formatted output (f-strings)
- ❌ Other I/O functions (file operations)
- ❌ Windows support (needs different syscall approach)

## Phase 2: Essential Language Features

### Milestone 4: Functions & Calls
**Goal:** Define and call simple functions with integer arguments and return values

**Tasks:**
- **From M3.5**: Add expression statement support (bare expressions without assignment)
  - Extend Parser: `StmtKind::ExprStmt` to allow `print("...")` without `_ =`
  - Enable side-effect-only function calls as statements
- Extend AST: `StmtKind::FunctionDef`, `ExprKind::Call` (already have Call from M3.5)
- Add `fn` keyword parsing for function definitions
- Parse function signatures: `fn func_name(arg: type) -> type:`
- Parse function bodies with `return` statements
- Parse function calls: `func_name(arg)` (already have syntax from M3.5)
- Implement basic symbol table for function name resolution
- Extend Codegen: Generate Cranelift IR for:
  - Function definitions with parameters
  - Function calls with argument passing (already support external calls from M3.5)
  - Return statements
- Add `fn main() -> int` support for explicit exit codes
- Write tests for function definition and calls

**Visible Progress:** Can compile and run programs with multiple functions. Programs can return explicit exit codes via `fn main() -> int`.

**Example:**
```ryo
fn add(a: int, b: int) -> int:
    return a + b

fn main() -> int:
    result = add(2, 3)
    print("Result is: ")  # Expression statement (no assignment needed)
    return 0  # Success
```

**Implementation Notes:**
- Start with single-argument functions, expand to multiple arguments
- All functions are module-scoped (no nested functions yet)
- No function overloading (one function per name)
- Dependencies: Milestone 3 (codegen foundation)

### Milestone 5: Module System (Design Phase) ✅ COMPLETE

**Goal:** Design and document the module system for code organization and visibility control

**Status:** ✅ COMPLETE - Module system fully designed and documented (2025-11-11)

**What Was Completed:**

1. **Formal Definitions**:
   - **Package**: Entire project defined by `ryo.toml` (compilation/distribution unit)
   - **Module**: Directory containing `.ryo` files (organizational unit)
   - **Directory = Module**: All `.ryo` files in a directory form a single module
   - **Hierarchical Structure**: Parent modules can contain both files and child submodules

2. **Access Level Design** (3 levels for simplicity):
   - **`pub`**: Public - accessible from any module
   - **`package`**: Package-internal - accessible within same `ryo.toml` package
   - **No keyword**: Module-private - accessible only within same module

3. **Key Design Decisions**:
   - Implicit discovery (no `mod` keyword needed, filesystem structure defines modules)
   - Hierarchical modules (Rust-style structure with Go-style directory=package)
   - Circular dependencies forbidden between modules, allowed within modules
   - Three access levels validated by Swift 6 (added `package` keyword in March 2025)

4. **Documentation Created**:
   - `docs/specification.md` Section 11: Complete module system specification (270+ lines)
   - `docs/specification.md` Section 2: Added `package` keyword to language keywords
   - `docs/proposals.md`: 8 future enhancement proposals (re-exports, workspaces, etc.)
   - `docs/design_issues.md`: Comprehensive design rationale and trade-off analysis
   - `docs/getting_started.md` Section 3: Complete module tutorial with examples
   - `docs/examples/modules/`: 6 practical examples demonstrating all features
   - `CLAUDE.md`: Module system design added to Key Design Decisions

5. **Practical Examples** (6 comprehensive examples):
   - `01-simple-module/`: Basic module creation and imports
   - `02-multi-file-module/`: Multiple files sharing namespace
   - `03-access-levels/`: pub, package, and module-private demonstration
   - `04-nested-modules/`: Hierarchical module structure
   - `05-package-visibility/`: Package boundary demonstration
   - `06-circular-deps/`: Circular dependency errors and solutions

**Design Validation:**

Comparison with other languages informed the design:

| Language | Module Unit | Access Levels | Discovery | Circular Deps | Ryo's Choice |
|----------|-------------|---------------|-----------|---------------|--------------|
| **Ryo** | Directory | 3 (pub, package, private) | Implicit | Forbidden | ✅ Sweet spot |
| Rust | File | 4 (pub, pub(crate), pub(super), private) | Explicit (`mod`) | Forbidden | Too complex |
| Go | Directory | 2 (Exported, unexported) | Implicit | Forbidden | Too limiting |
| Python | Directory | 1 (convention-based) | Implicit | Allowed | Too loose |
| Zig | Build-defined | 2 (pub, private) | Explicit | Forbidden | Too low-level |
| Swift 6 | Target | 6 levels | Explicit | Allowed | Too complex |

**Key Insights:**
- Swift 6 added `package` keyword in March 2025, validating Ryo's three-level design
- Go's 15+ years prove directory-based structure works at scale
- Rust's 2018 edition deprecated `mod.rs` for simpler structure (Ryo adopts this)

**Examples:**

```ryo
# Project structure
myproject/
├── ryo.toml              # Package boundary
└── src/
    ├── main.ryo          # Module "main"
    ├── utils/            # Module "utils" (parent)
    │   ├── core.ryo
    │   └── math/         # Module "utils.math" (child)
    │       └── ops.ryo
    └── server/           # Module "server" (multi-file)
        ├── http.ryo
        └── routes.ryo

# Access levels in utils/core.ryo
pub fn public_api():           # External users can call
    package_configure()

package fn package_configure(): # Only this package can call
    _internal_setup()

fn _internal_setup():          # Only utils module can call
    pass

# Importing modules
import utils              # Parent module
import utils.math         # Child module
import server             # Multi-file module
```

**Circular Dependency Solution Pattern:**

```ryo
# Problem: user imports post, post imports user (circular!)

# Solution: Extract common types
# models/ids.ryo
pub struct UserID(int)
pub struct PostID(int)

# user/user.ryo
import models.ids
struct User:
    id: ids.UserID
    post_ids: List[ids.PostID]  # Store IDs, not Post objects

# post/post.ryo
import models.ids
struct Post:
    id: ids.PostID
    author_id: ids.UserID  # Store ID, not User object
```

**Design Rationale:**

- **Simpler than Rust**: No `mod` declarations, no file vs module confusion
- **More powerful than Go**: Three access levels vs Go's two, explicit package boundaries
- **Familiar to Python/Go developers**: Directory-based structure is intuitive
- **Validated by industry**: Swift 6's addition of `package` proves the three-level model

**What's NOT Implemented (Design Complete, Implementation Deferred):**

- ❌ Parser support for `module`, `import`, `package` keywords (Milestone 22 implementation)
- ❌ AST nodes for module system
- ❌ Symbol table and name resolution across modules
- ❌ Visibility checking
- ❌ Multi-file project support
- ❌ Module-aware compilation

**Implementation Roadmap:**

The module system will be **implemented** in:
- **Milestone 22 (Implementation)**: Lexer/parser/AST for modules and imports
- **Phase 4**: Full module system integration with codegen and linking

**Future Enhancements** (documented in proposals.md):

1. Re-exports with `pub use` (High Priority)
2. File-level privacy `file fn` (Deferred/Maybe Never)
3. Parent-only visibility `pub(super)` (v2.0+)
4. Conditional compilation `#[cfg(...)]` (Post-v1.0 Essential)
5. Workspace support for multi-package projects (Post-v1.0 Important)
6. Module control files (optional `mod.ryo`) (Post-v1.0)
7. Glob imports `import utils.*` (Low Priority)
8. Visibility aliases `pub(friend)` (Future Research)

**Completion Date:** November 11, 2025

**Completion Criteria Met:**

- ✅ Formal package/module terminology defined
- ✅ Three access levels designed and justified
- ✅ Implicit discovery specified
- ✅ Circular dependency rules established
- ✅ Comprehensive documentation created
- ✅ Practical examples provided
- ✅ Comparison with other languages completed
- ✅ Future enhancements documented

**References:**

- `docs/specification.md` Section 11 - Complete specification
- `docs/design_issues.md` - Design rationale and trade-offs
- `docs/proposals.md` - Future enhancements
- `docs/examples/modules/` - Practical examples
- `docs/getting_started.md` Section 3 - Tutorial
- `CLAUDE.md` - Architecture guidelines

**Next Steps:**

Proceed with Milestone 22 (Implementation) after Milestone 4 (Functions & Calls) is complete. The design is stable and ready for implementation.

---

### Milestone 6: Expressions & Operators (Extended)
**Goal:** Support float type and extended operators

**Tasks:**
- Add `float` type to lexer/parser/AST
- Extend type system to handle `int` and `float` separately
- Add float literal parsing: `3.14`, `2.5`
- Add comparison operators: `==`, `!=`, `<`, `>`, `<=`, `>=`
- Add division operator (`/`) with integer division semantics
- Add modulo operator (`%`)
- Implement basic type checking:
  - Cannot mix `int` and `float` in operations without explicit conversion
  - Comparison operators return `bool` (added in M6)
- Extend Codegen: Generate IR for:
  - Float arithmetic operations
  - Comparison operations
  - Type conversions (if needed)
- Write tests for float operations and comparisons

**Visible Progress:** Can use floats and compare values. Clear type error messages when mixing types.

**Example:**
```ryo
x: float = 3.14
y: float = 2.71
pi_approx = x + y / 2.0

a = 10
b = 3
quotient = a / b      # 3 (integer division)
remainder = a % b     # 1
```

**Implementation Notes:**
- Float arithmetic uses IEEE 754 semantics
- Integer division truncates toward zero
- Type errors are clear and localized (bidirectional type checking)
- Dependencies: Milestone 4 (functions for testing)

### Milestone 6: Control Flow & Booleans
**Goal:** Implement `if/else` statements, `for` loops, and boolean logic

**Tasks:**
- **From M3.5**: Implement void/unit type (for functions with no return value)
  - Add `void` type to type system (or use `()` unit type)
  - Update `print()` signature from placeholder `int` to proper `void`
  - Type checker prevents using void values in expressions
  - Enable functions with no return: `fn do_something() -> void:`
- Add `bool` type to type system
- Add boolean literals: `true`, `false`
- Add logical operators: `and`, `or`, `not`
- Extend Parser/AST:
  - `StmtKind::IfStmt` with optional `else` branch
  - `StmtKind::ForLoop`
  - `StmtKind::Break`, `StmtKind::Continue`
  - Boolean expressions in conditions
- Extend Codegen: Generate Cranelift IR for:
  - Conditional branching (if/else)
  - Loop constructs (for)
  - Break/continue statements
  - Boolean operations (and/or/not with short-circuiting)
- Write tests for control flow and boolean logic

**Visible Progress:** Can write programs with conditionals and loops

**Example:**
```ryo
fn is_positive(x: int) -> bool:
    if x > 0:
        return true
    else:
        return false

fn print_if_even(n: int) -> void:  # void return type
    if n % 2 == 0:
        print("Even number")
    # No return statement needed for void

fn main() -> int:
    mut counter = 0
    for counter < 10:
        print_if_even(counter)
        counter += 1
    return 0
```

**Implementation Notes:**
- Short-circuit evaluation for `and`/`or` (don't evaluate right side if not needed)
- Break/continue only valid inside loops (checked at compile time)
- If expressions (returning values) deferred to later milestone
- Dependencies: Milestone 5 (comparison operators)

### Milestone 7: Structs
**Goal:** Implement user-defined composite types with named fields

**Tasks:**
- Add `struct` keyword to lexer/parser
- Extend AST: `StmtKind::StructDef`
- Parse struct definitions:
  ```ryo
  struct Point:
      x: float
      y: float
  ```
- Parse struct literals with parentheses: `Point(x=1.0, y=2.0)`
- Parse field access: `point.x`, `point.y`
- Extend type system:
  - Track struct definitions in symbol table
  - Type-check struct literals (all fields present, correct types)
  - Type-check field access (field exists, correct type)
- Extend Codegen: Generate IR for:
  - Stack allocation of structs
  - Field access (offset calculations)
  - Struct initialization
- Write tests for struct definition, initialization, and field access

**Visible Progress:** Can define and use custom types with multiple fields

**Example:**
```ryo
struct Rectangle:
    width: float
    height: float

fn area(rect: Rectangle) -> float:
    return rect.width * rect.height

fn main() -> int:
    r = Rectangle(width=10.0, height=5.0)
    a = area(r)
    return 0
```

**Implementation Notes:**
- Structs are **moved by default** (ownership semantics)
- Field order matters (affects memory layout)
- No default values for fields (all must be initialized)
- No methods yet (added in Milestone 15)
- Parentheses with named arguments used for struct literals: `Point(x=1, y=2)`, consistent with language design
- Braces reserved exclusively for f-string interpolation
- Dependencies: Milestone 4 (functions for passing structs)

### Milestone 8: Enums (Algebraic Data Types)
**Goal:** Implement enums with variants (sum types / tagged unions)

**Tasks:**
- Add `enum` keyword to lexer/parser
- Extend AST: `StmtKind::EnumDef`
- Parse enum definitions with variants:
  ```ryo
  enum Color:
      Red
      Green
      Blue

  enum Shape:
      Circle(radius: float)
      Rectangle(width: float, height: float)
  ```
- Parse enum variant construction: `Color.Red`, `Shape.Circle(5.0)`
- Extend type system:
  - Track enum definitions in symbol table
  - Type-check variant construction
- Extend Codegen: Generate IR for:
  - Enum representation (tag + data)
  - Variant construction
  - Tag checking (for pattern matching in M9)
- Write tests for enum definition and variant construction

**Visible Progress:** Can define sum types and construct variants

**Example:**
```ryo
enum Result:
    Success(value: int)
    Error(message: str)

fn divide(a: int, b: int) -> Result:
    if b == 0:
        return Result.Error("Division by zero")
    return Result.Success(a / b)
```

**Implementation Notes:**
- Enums are tagged unions (tag indicates which variant is active)
- Memory layout: tag (int) + max variant size
- Cannot access variant data without pattern matching (safety)
- String type needed for example above (added in Milestone 12)
- Dependencies: Milestone 7 (structs provide foundation for variant data)

### Milestone 9: Pattern Matching
**Goal:** Implement exhaustive pattern matching on enums and literals

**Tasks:**
- Add `match` keyword to lexer/parser
- Extend AST: `ExprKind::Match` with arms
- Parse match expressions:
  ```ryo
  match value:
      Pattern1: expression1
      Pattern2: expression2
      _: default_expression
  ```
- Parse patterns:
  - Literal patterns: `42`, `true`, `Color.Red`
  - Enum variant patterns: `Shape.Circle(radius)` (destructuring)
  - Wildcard pattern: `_`
- Implement exhaustiveness checking:
  - All enum variants must be covered
  - Or use wildcard `_` to catch remaining cases
- Extend Codegen: Generate IR for:
  - Match expressions (tag checking, jumps to arms)
  - Variable binding from patterns
- Write tests for pattern matching and exhaustiveness

**Visible Progress:** Can safely destructure enums and handle all cases

**Example:**
```ryo
enum Option:
    Some(value: int)
    None

fn unwrap_or(opt: Option, default: int) -> int:
    match opt:
        Option.Some(v): return v
        Option.None: return default

fn describe_color(color: Color) -> str:
    match color:
        Color.Red: return "red"
        Color.Green: return "green"
        Color.Blue: return "blue"
```

**Implementation Notes:**
- Match is an **expression** (returns a value)
- All arms must have same return type
- Exhaustiveness checking at compile time (prevents missing cases)
- Nested patterns deferred to later milestone
- Dependencies: Milestone 8 (enums to match on)

### Milestone 10: Tuples
**Goal:** Implement tuple types for multiple return values and grouping

**Tasks:**
- Add tuple syntax to lexer/parser
- Extend type system: `Type::Tuple(Vec<Type>)`
- Parse tuple type annotations: `(int, str)`
- Parse tuple literals: `(42, "hello")`
- Parse tuple destructuring:
  ```ryo
  (x, y) = get_point()
  ```
- Extend Codegen: Generate IR for:
  - Tuple construction (stack allocation)
  - Tuple field access by index
  - Tuple destructuring in assignments
- Write tests for tuple types, literals, and destructuring

**Visible Progress:** Can return multiple values from functions and destructure them

**Example:**
```ryo
fn divmod(a: int, b: int) -> (int, int):
    quotient = a / b
    remainder = a % b
    return (quotient, remainder)

fn main() -> int:
    (q, r) = divmod(10, 3)
    # q = 3, r = 1
    return 0
```

**Implementation Notes:**
- Tuples are **anonymous structs** (no named fields)
- Fixed size (known at compile time)
- Can be nested: `((int, int), str)`
- Tuple indexing syntax deferred (use destructuring for now)
- Tuples are **moved** like structs (ownership)
- Dependencies: Milestone 7 (structs provide foundation)

### Milestone 11: Error Types & Unions
**Goal:** Implement error types, error unions, and the error trait

**Tasks:**
- Add `error` keyword to lexer/parser
- Extend AST: `StmtKind::ErrorDef`
- Parse error definitions:
  ```ryo
  # File: file/errors.ryo
  error NotFound(path: str)
  error PermissionDenied(path: str)

  # File: main.ryo
  import file
  ```
- Parse error union syntax: `(ErrorA | ErrorB)!SuccessType`
- Parse function signatures with error returns: `fn foo() -> FileError!Data`
- Implement automatic error union inference from `try` expressions
- Implement `.message()` method for all errors (Error trait)
- Extend Codegen: Generate IR for:
  - Error variant construction
  - Error unions (tagged union of errors + success value)
- Write tests for error definitions and error unions

**Visible Progress:** Can define domain-specific errors and compose them in error unions

**Example:**
```ryo
# File: http/errors.ryo
error ConnectionFailed(reason: str)
error RequestTimeout

# File: parse/errors.ryo
error InvalidJson(message: str)

# File: main.ryo
import http
import parse

fn fetch_and_parse() -> (http.ConnectionFailed | http.RequestTimeout | parse.InvalidJson)!Data:
    response = try http_get("https://api.example.com")  # Returns http errors
    data = try parse_json(response.body())              # Returns parse errors
    return data

fn main() -> int:
    # Error handling with try/catch added in Milestone 21
    return 0
```

**Implementation Notes:**
- Errors are **single-variant only** (no multi-variant enums for errors)
- Error unions use `|` syntax for composition
- All errors implement `.message() -> str` automatically
- `try`/`catch` operators added in Milestone 21 for ergonomics
- Dependencies: Milestone 8 (enums), Milestone 9 (pattern matching for handling)

## Phase 3: Type System & Memory Safety

### Milestone 12: Basic Ownership & String Type
**Goal:** Implement move semantics for heap-allocated `str` type and introduce `Copy` trait

**Tasks:**
- **From M3.5**: Upgrade string literal implementation to full str type
  - String literals from M3.5 currently store in `.rodata` (read-only)
  - Upgrade to heap-allocated str type (dynamic length)
  - Enable string variables: `s: str = "hello"`
  - Add string concatenation: `s1 + s2` or `s.concat(other)`
  - Add string methods: `.len()`, `.is_empty()`, `.chars()`, `.substring()`, etc.
  - Add formatted string output (f-strings): `f"Value: {x}"`
- Implement semantic analysis pass (`src/checker.rs`):
  - Track variable states (uninitialized, valid, moved)
  - Implement move semantics for non-Copy types
  - Detect use-after-move errors
- Implement `Copy` trait concept:
  - Primitives (`int`, `float`, `bool`) are Copy
  - Structs/enums are Move by default
  - str is Move (heap-allocated)
- Extend Codegen: Generate IR for:
  - str allocation/deallocation
  - Move operations (memcpy)
  - Copy operations (simple assignment)
  - str concatenation operations
- Write tests for ownership violations and string operations

**Visible Progress:** Compiler catches use-after-move errors at compile time

**Example:**
```ryo
fn main() -> int:
    # String operations (from M3.5 deferred features)
    s1: str = "hello"
    s2: str = " world"
    greeting = s1 + s2    # Concatenation
    print(greeting)       # "hello world"

    name = "Alice"
    msg = f"Hello, {name}!"  # F-string formatting
    print(msg)

    # Ownership example
    s3 = "test"       # str allocated
    s4 = s3           # s3 moved to s4
    # print(s3)      # Error: s3 was moved

    x: int = 42
    y = x             # x copied to y (int is Copy)
    print(f"y = {y}") # OK: x still valid
    return 0
```

**Implementation Notes:**
- Move semantics are **implicit** (no explicit `move` keyword)
- Copy trait is **marker-only** (no custom implementation yet)
- str deallocation handled automatically at end of scope (RAII, refined in M19)
- Simple garbage-free memory management
- Dependencies: Milestone 11 (error types for allocation failures)

**References:**
- https://www.modular.com/blog/mojo-vs-rust
- https://docs.modular.com/mojo/manual/values/

### Milestone 13: Optional Types (`?T`)
**Goal:** Implement null-safe optional types with `?T`, `none`, and `orelse`

**Tasks:**
- Add `none` keyword to lexer/parser
- Extend type system: `Type::Optional(Box<Type>)`
- Parse optional type annotations: `?User`, `?str`
- Parse `none` literal
- Parse optional chaining: `user?.name?.len()`
- Parse `orelse` operator: `value orelse default`
- Implement smart casting:
  - `if x != none:` narrows type from `?T` to `T` in true branch
  - `x orelse return err` narrows type after statement
- Extend Codegen: Generate IR for:
  - Optional representation (tag + value)
  - None checks
  - Optional chaining short-circuiting
- Write tests for optional types and null safety

**Visible Progress:** No more null pointer exceptions! Type-safe optional handling.

**Example:**
```ryo
fn find_user(id: int) -> ?User:
    if id < 0:
        return none
    return User(name="Alice", id=id)

fn main() -> int:
    user = find_user(42)

    # Optional chaining
    name_len = user?.name?.len()  # Returns ?int

    # Providing defaults
    display_name = user?.name orelse "Unknown"

    # Early return with smart casting
    u = user orelse return 1
    # u is now User (not ?User) after this line
    print(u.name)

    return 0
```

**Implementation Notes:**
- Optional types use **tagged union** (tag + value)
- `none` is **not null** (different representation, type-safe)
- Smart casting narrows types in control flow
- Chaining returns `?T` (must handle with `orelse` or check)
- Dependencies: Milestone 8 (enums provide foundation for tagged unions)

### Milestone 14: Traits
**Goal:** Implement trait system for behavior abstraction

**Tasks:**
- Add `trait` keyword to lexer/parser
- Extend AST: `StmtKind::TraitDef`
- Parse trait definitions:
  ```ryo
  trait Drawable:
      fn draw(self)
      fn area(self) -> float
  ```
- Parse trait bounds in function signatures: `fn process[T: Drawable](obj: T)`
- Extend type system:
  - Track trait definitions
  - Track trait implementations
  - Check trait bounds
- **Static dispatch only** (monomorphization, no dynamic dispatch yet)
- Write tests for trait definition and bounds

**Visible Progress:** Can define shared behavior across types

**Example:**
```ryo
trait Printable:
    fn to_string(self) -> str

impl Printable for int:
    fn to_string(self) -> str:
        # Convert int to string
        return int_to_str(self)

impl Printable for User:
    fn to_string(self) -> str:
        return f"User({self.name})"
```

**Implementation Notes:**
- Traits define **required methods** only
- No associated types or constants yet (future milestone)
- No default implementations yet (future milestone)
- Static dispatch via monomorphization (like Rust)
- Dependencies: Milestone 15 (methods needed for trait impl)

### Milestone 15: Method Implementations
**Goal:** Implement methods on types via `impl` blocks

**Tasks:**
- Add `impl` keyword to lexer/parser
- Extend AST: `StmtKind::ImplBlock`
- Parse impl blocks:
  ```ryo
  impl Rectangle:
      fn area(self) -> float:
          return self.width * self.height
  ```
- Parse method calls: `rect.area()`
- Handle `self` parameter:
  - `self` for consuming methods (move)
  - `&self` for immutable borrow (added in M16)
  - `&mut self` for mutable borrow (added in M20)
- Extend type system:
  - Associate methods with types
  - Type-check method calls
- Extend Codegen: Generate IR for:
  - Method calls (pass self as first argument)
  - Method definitions
- Write tests for methods

**Visible Progress:** Can call methods on custom types with dot syntax

**Example:**
```ryo
struct Circle:
    radius: float

impl Circle:
    fn area(self) -> float:
        return 3.14159 * self.radius * self.radius

    fn scale(self, factor: float) -> Circle:
        return Circle{radius: self.radius * factor}

fn main() -> int:
    c = Circle{radius: 5.0}
    a = c.area()              # Consumes c (moved)
    # c.area()                # Error: c was moved
    return 0
```

**Implementation Notes:**
- `self` **moves by default** (ownership)
- Method call syntax: `obj.method()` desugars to `Type::method(obj)`
- No method overloading (one method per name per type)
- Dependencies: Milestone 12 (ownership for self parameter)

### Milestone 16: Immutable Borrows (`&T`)
**Goal:** Implement immutable references to avoid unnecessary moves

**Tasks:**
- Add `&` syntax to lexer/parser for borrow operator
- Extend type system: `Type::Ref(Box<Type>)`
- Parse immutable borrow syntax:
  - Type annotations: `&str`, `&User`
  - Borrow expressions: `&value`
- Implement borrow checking rules:
  - Can have multiple immutable borrows
  - Cannot mutate through immutable borrow
  - Borrows must not outlive the value
- Update method signatures to use `&self`:
  ```ryo
  fn area(&self) -> float
  ```
- Extend Codegen: Generate IR for:
  - Taking references (get address)
  - Dereferencing (automatic for method calls)
  - Passing pointers
- Write tests for borrow checking

**Visible Progress:** Can share data without moving ownership

**Example:**
```ryo
struct Point:
    x: float
    y: float

impl Point:
    fn distance(&self, other: &Point) -> float:
        dx = self.x - other.x
        dy = self.y - other.y
        return sqrt(dx * dx + dy * dy)

fn main() -> int:
    p1 = Point(x=0.0, y=0.0)
    p2 = Point(x=3.0, y=4.0)

    d = p1.distance(&p2)   # Borrow p2, don't move it
    print(d)               # Can still use p1 and p2

    # p1.x = 10.0          # Error: p1 is immutably borrowed (if concurrent borrow)
    return 0
```

**Implementation Notes:**
- Borrows are **implicit** in many cases (function parameters)
- References are **non-nullable** (always point to valid data)
- Lifetime tracking is **simplified** (no explicit lifetimes like Rust)
- Borrow checker uses basic scope-based analysis
- Dependencies: Milestone 15 (methods with &self)

### Milestone 17: Slices & String Slices
**Goal:** Implement borrowed views into arrays and strings

**Tasks:**
- Add slice syntax to lexer/parser
- Extend type system:
  - `Type::Slice(Box<Type>)` for array slices `&[T]`
  - `&str` as string slice (borrowed view)
- Parse slice operations:
  - Array slicing: `array[start:end]`
  - Full slice: `array[:]`
- Distinguish `str` (owned) from `&str` (borrowed):
  - `str` is heap-allocated, owned, mutable
  - `&str` is borrowed view, immutable
- Extend Codegen: Generate IR for:
  - Slice representation (pointer + length)
  - Slice bounds checking
  - String slice operations
- Write tests for slices and string slices

**Visible Progress:** Efficient string/array operations without copying

**Example:**
```ryo
fn first_word(text: &str) -> &str:
    for i in range(text.len()):
        if text[i] == ' ':
            return text[0:i]  # Return slice (no copy)
    return text

fn sum_slice(numbers: &[int]) -> int:
    mut total = 0
    for n in numbers:
        total += n
    return total

fn main() -> int:
    s = "hello world"
    word = first_word(s)  # word is &str (view into s)
    print(word)           # "hello"

    nums = [1, 2, 3, 4, 5]
    total = sum_slice(&nums[1:4])  # Pass slice [2, 3, 4]
    print(total)  # 9
    return 0
```

**Implementation Notes:**
- Slices are **fat pointers** (pointer + length)
- Bounds checking at runtime (panic on out-of-bounds)
- String slices must be **UTF-8 valid** (checked at slice boundaries)
- No mutable slices yet (added in M20)
- Dependencies: Milestone 16 (borrows for slice references)

### Milestone 18: Collections (List, Map)
**Goal:** Implement `List[T]` and `Map[K, V]` with hardcoded types

**Tasks:**
- Implement `List[int]` and `List[str]` as built-in types:
  - Dynamic array with growth
  - Methods: `append`, `len`, `get`, `remove`
- Implement `Map[str, int]` as built-in type:
  - Hash table implementation
  - Methods: `insert`, `get`, `remove`, `contains`
- Add `for` loop support for collections:
  ```ryo
  for item in list:
      process(item)
  ```
- Extend Codegen: Generate IR for:
  - Collection allocation/deallocation
  - Dynamic resizing
  - Iteration
- Write tests for collections

**Visible Progress:** Can use dynamic collections for real programs

**Example:**
```ryo
fn main() -> int:
    mut numbers = List[int]()
    numbers.append(1)
    numbers.append(2)
    numbers.append(3)

    mut sum = 0
    for n in numbers:
        sum += n
    print(sum)  # 6

    mut scores = Map[str, int]()
    scores.insert("Alice", 100)
    scores.insert("Bob", 85)

    alice_score = scores.get("Alice") orelse 0
    print(alice_score)  # 100

    return 0
```

**Implementation Notes:**
- **Hardcoded types** initially: `List[int]`, `List[str]`, `Map[str, int]`
- Generics deferred to Phase 5 (post-v0.1.0)
- Collections own their data (RAII cleanup in M19)
- Iteration uses immutable borrows
- Dependencies: Milestone 17 (slices for iteration)

### Milestone 19: RAII & Drop Trait
**Goal:** Implement automatic resource cleanup with Drop trait

**Tasks:**
- Add `Drop` trait to standard library:
  ```ryo
  trait Drop:
      fn drop(&mut self)
  ```
- Implement automatic drop calls:
  - At end of scope
  - On early returns
  - On move (drop old value if reassigning)
- Implement Drop for built-in types:
  - `str`: Free heap memory
  - `List[T]`: Free array and drop elements
  - `Map[K, V]`: Free table and drop entries
- Extend Codegen: Generate IR for:
  - Drop calls at scope exit
  - Drop calls on early returns
- Write tests for RAII and Drop

**Visible Progress:** No memory leaks! Resources cleaned up automatically.

**Example:**
```ryo
struct File:
    handle: int  # File descriptor

impl Drop for File:
    fn drop(&mut self):
        close_file(self.handle)  # FFI call

fn process_file(path: &str):
    file = open_file(path)  # File opened
    # ... use file ...
    # File automatically closed at end of scope (drop called)

fn early_return():
    file = open_file("data.txt")
    if file.is_empty():
        return  # File dropped here (drop called on early return)
    # ... use file ...
    # File dropped here (drop called at end of scope)
```

**Implementation Notes:**
- Drop is **automatic** (compiler inserts calls)
- Drop order: **reverse of declaration order** (like Rust)
- User-defined Drop for custom resources
- Prevents resource leaks (files, sockets, memory)
- Dependencies: Milestone 14 (traits), Milestone 20 (mutable borrows for &mut self)

### Milestone 20: Mutable Borrows (`&mut T`)
**Goal:** Implement mutable references with aliasing restrictions

**Tasks:**
- Add `&mut` syntax to lexer/parser
- Extend type system: `Type::MutRef(Box<Type>)`
- Parse mutable borrow syntax:
  - Type annotations: `&mut User`, `&mut [int]`
  - Borrow expressions: `&mut value`
- Implement borrow checking rules:
  - **At most one mutable borrow** at a time
  - **No immutable borrows while mutable borrow exists**
  - Borrows must not outlive the value
- Update method signatures to use `&mut self`:
  ```ryo
  fn set_x(&mut self, x: float)
  ```
- Extend Codegen: Generate IR for:
  - Mutable references (pointers with write access)
  - Dereferencing for mutation
- Write tests for mutable borrow checking

**Visible Progress:** Can mutate through references safely (no data races)

**Example:**
```ryo
fn increment(x: &mut int):
    *x += 1  # Dereference and mutate

impl Point:
    fn translate(&mut self, dx: float, dy: float):
        self.x += dx
        self.y += dy

fn main() -> int:
    mut count = 0
    increment(&mut count)
    print(count)  # 1

    mut p = Point(x=0.0, y=0.0)
    p.translate(5.0, 10.0)
    print(p.x)  # 5.0

    # Aliasing prevented:
    # r1 = &mut p
    # r2 = &mut p      # Error: cannot borrow as mutable twice
    # r3 = &p          # Error: cannot borrow as immutable while mutable borrow exists

    return 0
```

**Implementation Notes:**
- Mutable borrows are **exclusive** (no other borrows allowed)
- Prevents data races at compile time
- Explicit dereference `*x` for mutation in some cases (automatic for method calls)
- Simplified borrow checker (no lifetimes like Rust)
- Dependencies: Milestone 16 (immutable borrows provide foundation)

### Milestone 21: Try/Catch Operators
**Goal:** Implement ergonomic error propagation and handling

**Tasks:**
- Add `try` and `catch` keywords to lexer/parser
- Extend AST: `ExprKind::Try`, `ExprKind::Catch`
- Parse try expressions:
  ```ryo
  result = try fallible_operation()
  ```
- Parse catch expressions:
  ```ryo
  value = operation() catch |e|:
      handle_error(e)
      return default_value
  ```
- Implement try semantics:
  - If operation returns error, propagate to caller
  - Automatically composes error unions
- Implement catch semantics:
  - If operation returns error, execute handler block
  - Handler has access to error value
  - Can return, re-throw, or provide default
- Extend Codegen: Generate IR for:
  - Error checking and propagation
  - Catch handler jumps
- Write tests for try/catch

**Visible Progress:** Clean error handling without verbose match statements

**Example:**
```ryo
# File: file/errors.ryo
error NotFound(path: str)
error PermissionDenied(path: str)

# File: parse/errors.ryo
error InvalidFormat(message: str)

# File: main.ryo
import file
import parse

fn load_config(path: &str) -> (file.NotFound | file.PermissionDenied | parse.InvalidFormat)!Config:
    content = try read_file(path)       # Propagates file errors
    config = try parse_config(content)  # Propagates parse errors
    return config

fn main() -> int:
    config = load_config("config.toml") catch |e|:
        match e:
            file.NotFound(path):
                print(f"File not found: {path}")
            file.PermissionDenied(path):
                print(f"Permission denied: {path}")
            parse.InvalidFormat(msg):
                print(f"Invalid config: {msg}")
        return 1  # Error exit code

    print("Config loaded successfully")
    return 0
```

**Implementation Notes:**
- Try is **expression-based** (returns success value or propagates error)
- Catch is **expression-based** (can return value from handler)
- Error unions composed automatically (no manual enum definition)
- Pattern matching in catch for specific error handling
- Dependencies: Milestone 11 (error types), Milestone 9 (pattern matching)

## Phase 4: Module System & Core Ecosystem

### Milestone 22: Module System (Implementation)
**Goal:** Implement module system for code organization and visibility control

**Status:** ⏳ Planned (Design completed in Milestone 5)

**Prerequisites:**
- ✅ Module system design complete (Milestone 5)
- ⏳ All core language features (Milestones 4-21)

**Design Reference:**
The module system design was completed in **Milestone 5** with full documentation:
- Directory-based modules (directory = module)
- Three access levels: `pub`, `package`, module-private
- Implicit discovery (no `mod` keyword)
- Hierarchical structure
- Circular dependency prevention

See Milestone 5 (above) and `docs/specification.md` Section 11 for complete design details.

**Implementation Tasks:**

1. **Lexer/Parser Extensions:**
   - Add `import`, `package` keywords to lexer (note: `pub` already exists)
   - Extend AST: `StmtKind::ImportStmt` for import declarations
   - Parse import statements:
     ```ryo
     import math                    # Simple import
     import utils.strings           # Nested module import
     ```
   - Parse `package` visibility modifier:
     ```ryo
     package fn helper()            # Package-internal function
     ```

2. **Multi-File Project Support:**
   - Update project structure handling:
     - `src/` as root directory
     - Directories as modules
     - Multiple `.ryo` files per module
   - File system traversal for module discovery
   - Build module dependency graph

3. **Symbol Table & Name Resolution:**
   - Implement per-module symbol tables
   - Implement cross-module name resolution
   - Resolve qualified names: `math.add()`, `utils.strings.format()`
   - Track visibility modifiers: `pub`, `package`, module-private

4. **Visibility Checking:**
   - Implement three-level access control:
     - `pub`: accessible from any module
     - `package`: accessible within same `ryo.toml` package
     - (no keyword): accessible only within same module
   - Check visibility at use sites
   - Provide clear error messages for visibility violations

5. **Circular Dependency Detection:**
   - Build module dependency graph during compilation
   - Detect cycles between modules (error)
   - Allow cycles within modules (same directory)

6. **Compilation Pipeline Updates:**
   - Update compilation order (dependency-first)
   - Generate separate object files per module (or whole-program)
   - Link modules together

7. **Testing:**
   - Tests for import statements
   - Tests for visibility checking (pub, package, private)
   - Tests for multi-file modules
   - Tests for nested modules
   - Tests for circular dependency detection
   - Integration tests with real multi-module projects

**Visible Progress:** Can organize code into multiple files with proper encapsulation

**Example:**
```ryo
# Project structure:
# myproject/
# ├── ryo.toml
# └── src/
#     ├── main.ryo
#     └── math/
#         └── operations.ryo

# src/math/operations.ryo
pub fn add(a: int, b: int) -> int:
    return _validate(a) + _validate(b)

package fn internal_helper(x: int) -> int:
    # Package-internal, can be used by other modules in this package
    return x * 2

fn _validate(x: int) -> int:
    # Module-private, only math module can use
    if x < 0:
        panic("Negative values not allowed")
    return x

# src/main.ryo
import math

fn main() -> int:
    result = math.add(2, 3)              # ✓ OK: add is pub
    # math.internal_helper(5)            # ✓ OK: same package
    # math._validate(10)                 # ❌ Error: module-private
    return 0
```

**Implementation Notes:**
- Directory = module (not file = module like Rust)
- All `.ryo` files in a directory share the module namespace
- Modules are **namespaces** (not values or types)
- Filesystem structure defines module hierarchy
- Circular imports between modules detected at compile time (error)
- Re-exports via `pub use` deferred to future enhancement (see proposals.md)

**Dependencies:**
- Milestone 21 (all core language features needed for module contents)
- Milestone 5 (design complete, ready for implementation)

**Future Enhancements** (see proposals.md):
- Re-exports: `pub use math.geometry.{Point, Line}`
- Glob imports: `import utils.*` (low priority)
- Conditional compilation: `#[cfg(test)]`
- Workspace support for multi-package projects

### Milestone 23: Standard Library Core
**Goal:** Implement essential standard library modules

**Tasks:**
- **From M3.5**: Expand I/O beyond basic print()
  - M3.5 provides basic `print()` for stdout
  - This milestone adds full I/O operations
- Implement `io` module:
  - `print(str) -> void`: Print to stdout (already in M3.5 as builtin)
  - `println(str) -> void`: Print with newline
  - `eprint(str) -> void`, `eprintln(str) -> void`: Print to stderr
  - `input() -> io.Error!str`: Read from stdin
  - `read_file(path: &str) -> io.Error!str`: Read file contents
  - `write_file(path: &str, content: &str) -> io.Error!()`: Write to file
  - `append_file(path: &str, content: &str) -> io.Error!()`: Append to file
- Implement `string` module:
  - `split(s: &str, delimiter: &str) -> List[str]`
  - `join(parts: &[str], separator: &str) -> str`
  - `trim(s: &str) -> &str`
  - `to_upper(s: &str) -> str`, `to_lower(s: &str) -> str`
- Implement `collections` module:
  - `List[T]` methods: `push`, `pop`, `len`, `get`, `clear`
  - `Map[K, V]` methods: `insert`, `remove`, `get`, `keys`, `values`
  - Iterator support for `for` loops
- Implement `math` module:
  - `abs(x: float) -> float`
  - `sqrt(x: float) -> float`
  - `pow(base: float, exp: float) -> float`
  - Constants: `PI`, `E`
- Implement `os` module:
  - `args() -> List[str]`: Command-line arguments
  - `env(key: &str) -> ?str`: Environment variables
  - `exit(code: int)`: Exit program
- Write comprehensive tests for stdlib

**Visible Progress:** Can write real programs with I/O, string processing, and file operations

**Example:**
```ryo
import io
import string
import collections
import os

fn main() -> int:
    args = os.args()
    if args.len() < 2:
        io.println("Usage: program <file>")
        return 1

    filename = args[1]
    content = io.read_file(filename) catch |e|:
        io.println(f"Error reading file: {e.message()}")
        return 1

    words = string.split(content, " ")
    io.println(f"Word count: {words.len()}")

    return 0
```

**Implementation Notes:**
- **From M3.5**: Expand platform support beyond macOS/Linux
  - M3.5 currently supports macOS (Darwin) and Linux only
  - Add Windows support (using `WriteFile` instead of `write` syscall)
  - Add comprehensive platform detection and conditional compilation
  - Abstract platform differences in standard library
- Standard library is **written in Ryo** (using FFI for OS calls)
- Error types defined in respective modules (e.g., `io.Error`)
- All I/O operations return error unions (explicit error handling)
- UTF-8 string support throughout
- Platform-specific code isolated to `os` module
- Dependencies: Milestone 22 (modules for stdlib organization)

### Milestone 24: Panic & Debugging Support
**Goal:** Implement panic mechanism and debugging features

**Tasks:**
- **From M3**: Add direct IR display capability
  - M3 deferred this due to Cranelift API limitations
  - Add `ryo ir <file>` command to display Cranelift IR
  - Show optimized IR for debugging codegen issues
  - Include IR visualization options (control flow graph)
- Add `panic` function to stdlib:
  ```ryo
  fn panic(message: str) -> never
  ```
- Implement panic handling:
  - Print error message to stderr
  - Print stack trace (file, line, function)
  - Exit with non-zero code (typically 101)
- Add `assert` function:
  ```ryo
  fn assert(condition: bool, message: str)
  ```
- Implement stack trace generation:
  - Use DWARF debug info (in debug builds)
  - Include file names, line numbers, function names
- Add environment variable control:
  - `RYOLANG_BACKTRACE=1`: Enable full backtraces
  - `RYOLANG_BACKTRACE=0`: Disable backtraces (default: short)
- Improve error messages:
  - Include source context in compiler errors
  - Suggest fixes for common mistakes
- Write tests for panic and assertions

**Visible Progress:** Clear crash reports with stack traces for debugging

**Example:**
```ryo
import io

fn divide(a: int, b: int) -> int:
    if b == 0:
        panic("Division by zero")
    return a / b

fn main() -> int:
    assert(1 + 1 == 2, "Math is broken!")

    result = divide(10, 0)  # Panics with stack trace
    io.println(f"Result: {result}")
    return 0
```

**Output on panic:**
```
thread 'main' panicked at 'Division by zero', src/main.ryo:4:9
stack backtrace:
   0: divide (src/main.ryo:4)
   1: main (src/main.ryo:11)
note: run with `RYOLANG_BACKTRACE=1` for full backtrace
```

**Implementation Notes:**
- Panic **unwinds the stack** (calls Drop on all values)
- Panic is **not recoverable** (use error handling for that)
- Stack traces use DWARF debug info (compiled in debug mode)
- Release builds can strip debug info for smaller binaries
- Dependencies: Milestone 19 (Drop trait for unwinding)

### Milestone 25: Testing Framework & Documentation
**Goal:** Implement built-in testing and documentation generation

**Tasks:**
- Add `test` attribute for test functions:
  ```ryo
  #[test]
  fn test_addition():
      assert(1 + 1 == 2, "Addition works")
  ```
- Implement test runner:
  - `ryo test` command discovers and runs all tests
  - Reports pass/fail statistics
  - Captures test output
  - Parallel test execution (optional)
- Add assertion helpers:
  - `assert_eq(a, b, message)`: Assert equality
  - `assert_ne(a, b, message)`: Assert inequality
  - `assert_error(result, message)`: Assert error returned
- Implement documentation comments:
  - Triple-slash `///` for doc comments
  - Markdown formatting support
- Implement `ryo doc` command:
  - Generate HTML documentation from source
  - Include examples from doc comments
  - Link to module/type definitions
- Write tests for test framework itself (meta!)

**Visible Progress:** Professional testing and documentation workflow

**Example:**
```ryo
/// Calculates the factorial of a number.
///
/// # Examples
///
/// ```ryo
/// result = factorial(5)
/// assert(result == 120)
/// ```
fn factorial(n: int) -> int:
    if n <= 1:
        return 1
    return n * factorial(n - 1)

#[test]
fn test_factorial():
    assert_eq(factorial(0), 1, "0! = 1")
    assert_eq(factorial(1), 1, "1! = 1")
    assert_eq(factorial(5), 120, "5! = 120")

#[test]
fn test_factorial_large():
    result = factorial(10)
    assert(result == 3628800, "10! calculation")
```

**Running tests:**
```bash
$ ryo test
Running 2 tests...
test test_factorial ... ok
test test_factorial_large ... ok

Test result: ok. 2 passed; 0 failed
```

**Implementation Notes:**
- Tests run in **isolated processes** (failure doesn't crash runner)
- Test discovery scans all files in project
- Doc comments use **Markdown** (like Rust)
- Generated docs include trait implementations, method signatures
- Dependencies: Milestone 24 (assert functions)

### Milestone 26: Core Language Complete & v0.1.0 Prep
**Goal:** Finalize core language, polish, and prepare for v0.1.0 release

**Tasks:**
- **Integration & Polish:**
  - Comprehensive end-to-end testing of all features
  - Fix remaining bugs from GitHub issues
  - Performance optimization passes
  - Memory leak detection and fixes
- **Package Manager:**
  - Implement `ryo new <project>`: Create new project
  - Implement `ryo build`: Build project
  - Implement `ryo run`: Build and run project
  - Implement `ryo test`: Run project tests
  - Implement `ryo doc`: Generate documentation
  - Basic dependency management (local path dependencies)
- **Error Messages:**
  - Review and improve all compiler error messages
  - Add "did you mean?" suggestions
  - Include code snippets with error highlighting
- **Documentation:**
  - Complete language reference documentation
  - Write "Ryo by Example" tutorial series
  - Write migration guides (from Python, Rust, Go)
  - API documentation for all stdlib modules
- **Quality Assurance:**
  - Set up CI/CD pipeline (GitHub Actions)
  - Code coverage tracking (aim for >80%)
  - Fuzzing for parser and compiler
  - Security audit for memory safety
- **Release Preparation:**
  - Version numbering (semantic versioning)
  - Release notes and changelog
  - Binary distributions (Linux, macOS, Windows)
  - Installation script (`curl ... | sh`)

**Visible Progress:** Ryo v0.1.0 is production-ready!

**Implementation Notes:**
- This milestone is about **polish and integration**, not new features
- All previous milestones must be complete and stable
- Community feedback incorporated during beta period
- Long-term support (LTS) considerations
- Dependencies: Milestones 1-25 (everything!)

## Phase 5: Post-v0.1.0 Extensions (v0.2+)

**Note:** These features are deferred to post-v0.1.0 releases. They're important for advanced use cases but not required for a production-ready core language.

### REPL & JIT Compilation (Interactive Mode)
**Goal:** Implement interactive REPL with JIT compilation using Cranelift

**Why Post-v0.1.0:**
- **From M3**: JIT compilation deferred to avoid delaying core features
- AOT (ahead-of-time) compilation is sufficient for production use
- REPL requires significant additional work (state management, incremental compilation)
- Not essential for initial adoption (compile-run workflow works fine)
- Community feedback will inform REPL design (IPython-style vs basic)

**Features:**
- Interactive Read-Eval-Print Loop (REPL)
- JIT compilation using Cranelift (already a dependency)
- Multi-line input support (functions, structs, etc.)
- History and tab completion
- Variable inspection and debugging commands
- Hot code reloading (redefine functions on the fly)
- Integration with debugger

**Example REPL Session:**
```
$ ryo repl
Welcome to Ryo v1.5 REPL
>>> x = 42
>>> y = x * 2
>>> y
84
>>> fn double(n: int) -> int:
...     return n * 2
...
>>> double(21)
42
>>> :type double
fn double(n: int) -> int
>>> :help
Available commands:
  :quit - Exit REPL
  :type <expr> - Show type of expression
  :clear - Clear all bindings
  :help - Show this message
```

**Technical Notes:**
- Use Cranelift's JIT mode (already available, unused in M3)
- State management for incremental definitions
- Error recovery (syntax errors don't crash REPL)
- Integration with readline/rustyline for input editing

**Timeline:** v1.4 (3-6 months after v0.1.0)
**Effort:** 2-3 weeks
**Dependencies:** Core language complete (M1-M26)

### Async/Await Runtime
**Goal:** Implement asynchronous programming for I/O-bound applications

**Why Post-v0.1.0:**
- Requires mature ownership and type system (Milestones 12-21)
- Complex runtime implementation (executor, scheduler, reactor)
- Not essential for initial adoption (synchronous code works fine)
- Allows more design iteration based on community feedback

**Features:**
- `async fn` and `await` keywords
- Future trait and async runtime
- async-aware standard library (async I/O, async HTTP)
- Single-threaded and multi-threaded executors
- Cancellation and timeouts

**Example:**
```ryo
async fn fetch_url(url: &str) -> http.Error!str:
    response = await http.get(url)
    body = await response.text()
    return body

async fn main():
    data = await fetch_url("https://api.example.com")
    print(data)
```

**Timeline:** v1.5 (6-12 months after v0.1.0)

### Foreign Function Interface (FFI)
**Goal:** Comprehensive C interoperability for integrating with existing libraries

**Why Post-v0.1.0:**
- Safety model must be fully tested and stable
- `unsafe` blocks require careful design and auditing
- Not required for pure-Ryo applications
- Community will identify which C libraries are most needed

**Features:**
- `extern "C"` function declarations
- `unsafe` blocks for FFI calls
- Automatic binding generation (bindgen-like tool)
- C struct layout compatibility
- Callback support (C calling Ryo functions)

**Example:**
```ryo
extern "C":
    fn strlen(s: *const char) -> int
    fn printf(format: *const char, ...) -> int

fn main():
    unsafe:
        len = strlen(c"Hello")
        printf(c"Length: %d\n", len)
```

**Timeline:** v1.6 (12-18 months after v0.1.0)

### Full Generics System
**Goal:** Generic types and functions with trait bounds

**Why Post-v0.1.0:**
- Hardcoded collections (Milestone 18) sufficient for v0.1.0
- Generic implementation is complex (monomorphization, specialization)
- Trait system must be mature and stable (Milestone 14)
- Community feedback will inform design (variance, associated types, etc.)

**Features:**
- Generic functions: `fn max[T: Comparable](a: T, b: T) -> T`
- Generic types: `struct Box[T]`, `enum Option[T]`
- Trait bounds: `fn process[T: Printable + Cloneable](value: T)`
- Associated types in traits
- Generic standard library (replace hardcoded collections)

**Example:**
```ryo
trait Comparable:
    fn compare(&self, other: &Self) -> int

fn max[T: Comparable](a: T, b: T) -> T:
    if a.compare(b) > 0:
        return a
    return b

struct Stack[T]:
    items: List[T]

impl[T] Stack[T]:
    fn push(&mut self, item: T):
        self.items.append(item)

    fn pop(&mut self) -> ?T:
        return self.items.pop()
```

**Timeline:** v1.7 (18-24 months after v0.1.0)

### Additional Post-v0.1.0 Features

**Tooling & Developer Experience:**
- **Language Server Protocol (LSP):** IDE integration (autocompletion, go-to-definition, diagnostics)
- **Debugger Integration:** GDB/LLDB support with Ryo syntax awareness
- **Package Registry:** Central repository (crates.io-like) with version resolution
- **Workspaces:** Multi-package projects with shared dependencies
- **Build Caching:** Incremental compilation and artifact caching

**Advanced Language Features:**
- **Compile-time Execution (comptime):** Metaprogramming and zero-cost abstractions
- **CSP-Style Channels:** Optional concurrency model (`chan`, `select`) for specialized use cases
- **Inline Assembly:** For performance-critical code and kernel development
- **Cross-Compilation:** Easy targeting of different platforms
- **Profile-Guided Optimization (PGO):** Runtime profiling for better optimization

**Standard Library Expansion:**
- **HTTP Client/Server:** Async HTTP/2 and HTTP/3 support
- **JSON/YAML/TOML:** Serialization and deserialization
- **Regular Expressions:** Fast regex engine
- **Cryptography:** Hashing, encryption, TLS support
- **Compression:** gzip, zlib, brotli support
- **Database Drivers:** PostgreSQL, MySQL, SQLite connectors

See [proposals.md](proposals.md) for detailed designs of these features.

## Implementation Notes

### Key Dependencies
- **Rust Toolchain:** Latest stable Rust
- **Parsing:** `logos` for lexing, `chumsky` for parsing
- **Code Generation:** `cranelift` family of crates
- **Error Reporting:** `ariadne` for beautiful error messages
- **CLI:** `clap` for command-line interface
- **Async Runtime:** `tokio` or `futures` for async/await support

### Testing Strategy
- Unit tests for each compiler phase
- Integration tests for end-to-end compilation
- Golden file tests for error messages
- Performance benchmarks for compilation speed
- Memory safety tests for ownership system

### Quality Assurance
- Continuous Integration with multiple platforms
- Code coverage tracking
- Fuzzing for parser robustness
- Memory leak detection
- Security audit for FFI boundaries

## Core Language Goals (v0.1.0)

The 26 milestones in Phases 1-4 represent the **core language** needed for Ryo v0.1.0. Upon completion, developers will have:

✅ **Memory Safety:** Ownership and borrowing prevent use-after-free, double-free, and data races
✅ **Null Safety:** Optional types (`?T`) eliminate null pointer exceptions
✅ **Type Safety:** Static typing with bidirectional inference catches errors at compile time
✅ **Error Handling:** Error types, error unions, and `try`/`catch` for explicit, composable error management
✅ **Modern Type System:** Structs, enums (ADTs), traits, pattern matching, tuples
✅ **Performance:** AOT compilation to native code with zero-cost abstractions
✅ **Resource Management:** RAII with Drop trait for automatic cleanup
✅ **Standard Library:** Core I/O, strings, collections, math, OS integration
✅ **Tooling:** Compiler, test framework, documentation generator, package manager
✅ **Developer Experience:** Clear error messages with suggestions, comprehensive documentation

**What v0.1.0 does NOT include** (deferred to Phase 5):
- ❌ Async/await runtime (v0.2+)
- ❌ FFI/unsafe blocks (v0.3+)
- ❌ Full generics system (v0.4+)
- ❌ LSP/advanced tooling (v0.2+)

This foundation enables building **synchronous applications** including CLI tools, build systems, compilers, data processing pipelines, and game engines. Async/FFI features will follow based on community needs.

## Development Timeline

### Realistic Estimates (2-4 weeks per milestone)

**Phase 1 (M1-M3):** ✅ COMPLETE (~2 months)
**Phase 2 (M4-M11):** 8 milestones × 3 weeks avg = ~24 weeks (~6 months)
**Phase 3 (M12-M21):** 10 milestones × 3 weeks avg = ~30 weeks (~7.5 months)
**Phase 4 (M22-M26):** 5 milestones × 4 weeks avg = ~20 weeks (~5 months)

**Total Estimated Time:** 74-104 weeks (18-26 months) from Phase 2 start to v0.1.0

### Development Approach

- **Incremental:** Working software at every milestone
- **Flexible:** Adjust timeline for quality (testing, bug fixes, polish)
- **Parallel Work:** Documentation, testing, examples can overlap with implementation
- **Community-Driven:** Beta testing and feedback incorporated before v0.1.0 release

### Milestones by Complexity

**Simple (2 weeks):** M4, M5, M6, M10, M22
**Medium (3 weeks):** M7, M8, M9, M11, M12, M13, M14, M15, M16, M17, M18, M23, M24, M25
**Complex (4-5 weeks):** M19, M20, M21, M26

This timeline is **realistic** based on compiler development best practices. Each milestone includes implementation, testing, documentation, and examples.

## Known Limitations & Design Trade-offs

This section documents intentional limitations and pragmatic trade-offs in the roadmap.

### v0.1.0 Intentional Omissions

**No Generics in v0.1.0:**
- **Why:** Generic implementation is complex (monomorphization, specialization, error messages)
- **Workaround:** Hardcoded collection types (`List[int]`, `List[str]`, `Map[str, int]`)
- **Impact:** Some code duplication, but v0.1.0 remains usable for most applications
- **Timeline:** Full generics in v0.4+ (Phase 5)

**No Async/Await in v0.1.0:**
- **Why:** Requires mature runtime, complex implementation, not essential for initial adoption
- **Workaround:** Use synchronous I/O (works fine for many applications)
- **Impact:** Higher latency for I/O-bound applications, but predictable performance
- **Timeline:** Async runtime in v0.2+ (Phase 5)

**No FFI in v0.1.0:**
- **Why:** Safety model must be stable, `unsafe` requires careful audit
- **Workaround:** Write pure Ryo code or wait for FFI support
- **Impact:** Cannot integrate with existing C libraries initially
- **Timeline:** FFI in v0.3+ (Phase 5)

**No LSP in v0.1.0:**
- **Why:** Core language must be stable before tooling investment
- **Workaround:** Use basic text editor with syntax highlighting
- **Impact:** No IDE autocompletion or diagnostics initially
- **Timeline:** LSP in v0.2+ (Phase 5)

### Simplified Features (vs. Rust)

**No Explicit Lifetimes:**
- Ryo uses simplified "Ownership Lite" without lifetime annotations
- Most borrow checking is scope-based
- Trade-off: Simpler mental model but less flexibility than Rust
- Some advanced patterns may not be expressible

**No Trait Associated Types (v0.1.0):**
- Deferred to post-v0.1.0 generics work
- Traits can only define methods initially
- Trade-off: Simpler trait system but less powerful

**No Default Trait Methods (v0.1.0):**
- All trait methods must be implemented
- Trade-off: More explicit but more boilerplate

**No Dynamic Dispatch (v0.1.0):**
- Only static dispatch via monomorphization
- Trade-off: Better performance but larger binaries
- Dynamic dispatch (`dyn Trait`) in future milestone

### Performance Trade-offs

**Bounds Checking:**
- Array/slice access includes runtime bounds checks
- Trade-off: Safety over raw performance
- Future: Compiler may optimize away redundant checks

**No Inline Assembly (v0.1.0):**
- Cannot write performance-critical assembly code
- Trade-off: Portability and safety over peak performance
- Timeline: Inline assembly in Phase 5

**Debug Symbols in Binaries:**
- Stack traces require DWARF debug info (larger binaries)
- Trade-off: Better debugging over minimal binary size
- Workaround: Strip symbols in release builds

### Ecosystem Limitations

**No Package Registry (v0.1.0):**
- Only local path dependencies initially
- Trade-off: Simpler implementation to reach v0.1.0 faster
- Timeline: Central registry in Phase 5

**Limited Standard Library (v0.1.0):**
- Core I/O, strings, collections only
- No HTTP, JSON, regex, crypto in stdlib initially
- Trade-off: Smaller maintenance burden for v0.1.0
- Timeline: Stdlib expansion in v0.2+

**Single-Threaded (v0.1.0):**
- No multi-threading or parallelism support
- Trade-off: Simpler concurrency model (no data races by design)
- Timeline: Threading in Phase 5 (alongside async)

### Rationale for Trade-offs

These limitations are **intentional** to:
1. **Reach v0.1.0 faster** - Avoid scope creep, ship working language
2. **Validate core design** - Get community feedback before advanced features
3. **Maintain quality** - Better to ship complete simple features than half-baked complex ones
4. **Iterate based on usage** - Real-world usage will inform priorities for Phase 5

The goal is a **production-ready core language** that can evolve based on actual user needs rather than speculation.

## Conclusion

This roadmap represents an **honest, achievable plan** for building Ryo v0.1.0 over approximately 18-26 months. By deferring advanced features (async, FFI, generics) to Phase 5, we can deliver a solid, usable language faster while maintaining room for future growth.

**Next steps:**
1. Complete Phase 2 (Functions, Control Flow, Core Types)
2. Implement Phase 3 (Ownership, Type System, Memory Safety)
3. Build Phase 4 (Modules, Stdlib, Tooling)
4. Release v0.1.0 and gather community feedback
5. Iterate on Phase 5 features based on real-world needs

**Join us in building Ryo!** See [CONTRIBUTING.md](../CONTRIBUTING.md) for how to get involved.
