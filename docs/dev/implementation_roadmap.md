**Status:** Implementation (v0.0.x–v0.1)

# Ryo Implementation Roadmap

This roadmap outlines the planned development of the Ryo programming language compiler and runtime. Each milestone focuses on delivering specific, tangible capabilities while building toward a complete language implementation.

**Development Timeline:** Each milestone is designed for approximately 2-4 weeks of development (assuming ~8 hours/week), but timelines should remain flexible to ensure quality over speed.

## Guiding Principles

* **Iterate:** Get something working end-to-end quickly, then refine
* **Test Early, Test Often:** Integrate basic testing from the start
* **Focus:** Each milestone adds a specific, visible capability
* **Simplicity First:** Implement the simplest version that meets the immediate goal
* **Quality of Life:** Include documentation, basic error reporting, and simple tooling

## Alpha Scope (v0.0.x)

Alpha is a **delivery slice** within v0.1.0, not a separate language. It targets the smallest subset of features that makes the language usable for external evaluation.

**Acceptance criterion:** the code snippet on `landing/index.html` compiles and runs.

Milestones included in the alpha slice are tagged `[alpha]` in their headings below. Some milestones are **partial** for alpha (only a subset of their tasks ships in `0.0.1-alpha.1`); those are tagged `[alpha: partial]` and the alpha-required subset is listed in [alpha_scope.md](alpha_scope.md).

Full alpha definition, litmus test, non-goals, release tagging, and gating test programs: see [docs/dev/alpha_scope.md](alpha_scope.md).

## Phase 1: Core Foundation

### Milestone 1: Setup & Lexer Basics [alpha] ✅ COMPLETE

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

### Milestone 2: Parser & AST Basics [alpha] ✅ COMPLETE

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

### Milestone 3: "Hello, Exit Code!" (Cranelift Integration) [alpha] ✅ COMPLETE

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
- ✅ Implement full linking pipeline mandating `zig cc` as the driver (for cross-compilation support)
- ✅ Comprehensive testing: 15 integration tests for codegen

**Visible Progress:** `ryo run my_program.ryo` executes and exits with specified code ✅ (**Major milestone!**)

**Completion Date:** November 9, 2025

**Implementation Details:**
- Full AOT compilation pipeline: Source → Lex → Parse → Codegen → Link → Execute
- Generates position-independent code (PIC) for portability
- Handles multiple statements
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
- `ryo ir` command now displays actual Cranelift IR (fixed in M4)


**What's NOT Implemented (Deferred):**
- ❌ JIT compilation (for REPL)
- ✅ Direct IR display → **Implemented in Milestone 4** via `compile_and_dump_ir()`
- ✅ String support → **Implemented in Milestone 3.5**
- ❌ Functions (beyond main)
- ❌ Control flow
- ❌ Error handling

### Milestone 3.5: "Hello, World!" (String Literals & Print) [alpha] ✅ COMPLETE

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

**Completion Date:** November 10, 2025

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
- No ownership semantics (deferred to Milestone 8.1)
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
  - Proper void/unit type will be implemented in Milestone 8 (Control Flow & Booleans)
  - Aligns with Python's `None` convention and uses `void` keyword similar to C/Java/TypeScript
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

### Milestone 4: Functions & Calls [alpha]

**Status:** ✅ COMPLETE - User-defined functions with parameters, return values, and variable references implemented

**Goal:** Define and call simple functions with integer arguments and return values

**What was implemented:**
- CPython-style indent preprocessor (`src/indent.rs`) for tab-based indentation blocks
- Lexer: `Arrow` (`->`), `Newline`, synthetic `Indent`/`Dedent` tokens
- AST: `StmtKind::FunctionDef`, `StmtKind::Return`, `StmtKind::ExprStmt`, `ExprKind::Ident`
- Parser: function definitions (`fn name(params) -> type:`), return statements, expression statements, variable references
- HIR layer (`src/hir.rs`, `src/lower.rs`): post-analysis IR with full type resolution, scope checking, and implicit main wrapping — analogous to Zig's AIR
- Codegen: refactored to consume HIR (not AST), two-pass compilation (declare-then-define) for forward references, `FunctionContext` struct, Cranelift `Variable` storage for locals/params, user function calls
- Builtin function registry (`src/builtins.rs`) for `print()` and future builtins
- `ryo ir` command now displays actual Cranelift IR
- `main.rs` split into focused modules: `errors.rs`, `linker.rs`, `pipeline.rs`
- Backward compatibility: flat programs without `fn main()` are wrapped in a synthetic implicit main returning 0
- 93 tests passing (62 unit + 31 integration)

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
- All functions are module-scoped (no nested functions)
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
	post_ids: list[ids.PostID]  # Store IDs, not Post objects

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

- ❌ Parser support for `module`, `import`, `package` keywords (Milestone 6 implementation)
- ❌ AST nodes for module system
- ❌ Symbol table and name resolution across modules
- ❌ Visibility checking
- ❌ Multi-file project support
- ❌ Module-aware compilation

**Implementation Roadmap:**

The module system will be **implemented** in:
- **Milestone 6 (Implementation)**: Lexer/parser/AST for modules and imports
- **Phase 2**: Full module system integration with codegen and linking

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

Milestone 6 (Implementation) is scheduled at the **start of Phase 4** — after the core language (Milestones 4–23) is complete — so module boundaries are drawn around a fully settled set of language constructs. The design is stable and ready for implementation.

---

> **Note:** Milestone 6 (Module System Implementation) is documented at the **start of Phase 4**, after the core language is complete. The design landed in M5 (above), but implementation waits until all language features are in place so modules don't have to be revised as new constructs arrive.

### Milestone 6.5: Booleans & Equality [alpha]
**Goal:** Add `bool` primitive type and equality operators as a precondition for M7 and M8.

**Status:** ✅ COMPLETE — `bool` type, `true`/`false` literals, and `==`/`!=` operators implemented end-to-end.

**What was implemented:**
- Added `true` and `false` keyword tokens and `==` / `!=` operator tokens to the lexer
- Extended AST with `Literal::Bool` and `BinaryOperator::Eq` / `NotEq`
- Parse boolean literals and equality expressions (non-associative, below additive precedence)
- Extended HIR with `Type::Bool`, `HirExprKind::BoolLiteral`, and equality `BinaryOp` variants
- Type-check equality: same-type operands; supported on `int` and `bool`; `str` and `void` rejected
- Codegen: maps `Type::Bool` to Cranelift `I8`; emits `icmp` for equality; `cranelift_type_for` helper makes variable declaration type-aware
- Unit tests across lexer, parser, lowering, and codegen layers
- Integration test verifying a bool program compiles and exits 0

**Visible Progress:** Programs can declare bool variables and compute equality. Bool values remain unobservable until M8 (no `if` or `print(bool)` yet).

**Example:**
```ryo
fn main() -> int:
	flag = true
	same = 1 == 1
	diff = 1 != 1
	return 0
```

**Implementation Notes:**
- No implicit coercion between `bool` and `int` (Zig-style).
- Equality is non-chaining: `a == b == c` is a parse error.
- String equality is deferred; the error message says `(yet)` to signal future support.
- Dependencies: Milestone 4 (functions).

---
Note: Zig pipeline alignment implemented here.
---

### Milestone 7: Expressions & Operators (Extended) [alpha]
**Goal:** Support float type and extended operators

**Tasks:**
- Add `float` type to lexer/parser/AST
- Extend type system to handle `int` (defaults to `i64`) and `float` separately
- Add float literal parsing: `3.14`, `2.5`
- Add ordering comparison operators: `<`, `>`, `<=`, `>=` (`==` and `!=` added in M6.5)
- Add division operator (`/`) with integer division semantics
- Add modulo operator (`%`)
- Implement basic type checking:
  - Cannot mix `int` and `float` in operations without explicit conversion
  - Ordering comparisons return `bool` (`bool` added in M6.5)
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
- Dependencies: Milestone 4 (functions for testing), Milestone 6.5 (provides bool type and `==`/`!=` semantics)

---
Note: Review [Project Structure](project_structure) and make the first split before M8
---

### Milestone 8: Control Flow & Booleans [alpha]
**Goal:** Implement `if/else` statements, `for` loops, and boolean logic

**Tasks:**
- **From M3.5**: Implement void/unit type (for functions with no return value)
  - Add `void` type to type system as the unit type
  - Update `print()` signature from placeholder `int` to proper `void`
  - Type checker prevents using void values in expressions
  - Enable functions with no return: `fn do_something() -> void:`
- (`bool` type and `true`/`false` literals already exist from M6.5)
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
- func main must have no arguments and no return values (as GO) 

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
	while counter < 10:
		print_if_even(counter)
		counter += 1
	return 0
```

**Implementation Notes:**
- Short-circuit evaluation for `and`/`or` (don't evaluate right side if not needed)
- **Two `for` loop forms and `while`:**
  - `for item in iterable:` — iteration over collections
  - `for i in range(start, end):` — counted iteration (`range()` is the only mechanism, no `..` for iteration)
  - `while condition:` — condition-based loop
- **Loop variables are block-scoped** — not accessible after the loop ends
- **Loop variables are immutable** — consistent with Ryo's default. While loops use externally declared `mut` variables
- **`range()` is a built-in function** (like `print`). Only mechanism for counted iteration. Exclusive end.
- **Operator separation:** `range()` for iteration, `:` for slicing (`s[1:4]`), `..` for type bounds only (`int(1..65535)`)
- Break/continue affect **innermost loop only**. No labeled breaks in v0.1
- If expressions (returning values) deferred to later milestone
- Dependencies: Milestone 7 (ordering comparisons), Milestone 6.5 (bool type and equality)

### Milestone 8.1: Heap-Allocated `str` Type & Move Semantics [alpha]
**Goal:** Promote string literals from read-only data to a real heap-allocated `str` type, and introduce the ownership-tracking pass that catches use-after-move on named bindings. Borrows (`&T`/`&mut T`) and string slices (`&str`) follow in M8.2–M8.4.

**Status:** ⏳ Planned

**Why this comes before M8.5 (and the rest of Phase 2):**
Every downstream milestone in Phase 2 (structs, tuples, enums, pattern matching, error types) uses `str` in its examples and signatures — `error NotFound(path: str)`, `Result.Error("...")`, struct fields with string names. Landing strings and the borrow checker here makes those milestones implementable as written, instead of forward-referencing features. It also means the move-tracking pass exists before structs and tuples, so their move semantics are checked from day one.

**Tasks:**
- **From M3.5**: Upgrade string literal implementation to a full `str` type
  - String literals from M3.5 currently store in `.rodata` (read-only)
  - Upgrade to heap-allocated `str` (dynamic length, length-prefixed)
  - Enable string variables: `s: str = "hello"`
- Add string concatenation: `s1 + s2` (or `s.concat(other)`)
- Add string methods: `.len()`, `.is_empty()`, `.chars()`, `.substring(start, end)`
- Add stdlib helpers for non-string formatting: `int_to_str(x)`, `float_to_str(x)`, `bool_to_str(b)`
- **F-strings (`f"Value: {x}"`) are deferred to v0.2** — see Phase 5: F-strings & String Interpolation. v0.1 uses `+` concatenation with the `*_to_str` helpers above.
- Implement the **ownership-tracking pass** (`src/sema.rs` extension):
  - Track variable states (uninitialized, valid, moved) for named bindings
  - Implement move semantics for non-Copy types: `s2 = s1` invalidates `s1`
  - Detect and report use-after-move at compile time
- Implement `Copy` semantics as a **compiler-known marker** (the user-facing `trait` keyword is deferred to v0.2/v0.3 — see Phase 5: Traits & Generics):
  - Primitives (`int`, `float`, `bool`) are Copy
  - Heap types (`str`, future `list`/`map`) are Move
  - Aggregates (struct/tuple/enum, once they exist in M9–M11) are Move by default
- Codegen: heap allocation for `str`, free at scope exit, memcpy on move
- Update `print()` to accept owned `str` (still works on literals via the same path)

**Visible Progress:** Strings are real values — assignable, concatenable, returnable. Use-after-move is caught at compile time.

**Example:**
```ryo
fn greet(name: str) -> str:
	return "Hello, " + name + "!"   # name moved into the concatenation chain

fn main() -> int:
	user: str = "Alice"
	msg = greet(user)                # user moved into greet
	# print(user)                    # compile error: use after move
	print(msg)                       # "Hello, Alice!"
	print("len = " + int_to_str(msg.len()))

	x: int = 42
	y = x                            # int is Copy — x still valid
	print("x = " + int_to_str(x))    # ok
	return 0
```

**Implementation Notes:**
- Move tracking covers **named bindings only** in this milestone. `&T` / `&mut T` borrows arrive in M8.2 / M8.3, and field-by-field move tracking (partial moves out of structs/tuples) follows naturally because the same dataflow analysis is reused.
- `str` deallocation is automatic at scope exit (simple RAII; user-extensible cleanup via the `drop` method lands in M23).
- Allocator failure surfaces as a panic in v0.1; allocation-fallible APIs ship alongside error unions (M13).
- Dependencies: Milestone 8 (control flow blocks shape the dataflow regions the move tracker walks).

### Milestone 8.2: Immutable Borrows (`&T`) [alpha]
**Goal:** Add immutable references and the corresponding borrow-checking rules, building on M8.1's move tracker.

**Status:** ⏳ Planned

**Tasks:**
- Add `&` operator to lexer/parser for borrow expressions (and `&T` in type annotations)
- Extend type system: `Type::Ref(Box<Type>)`
- Parse:
  - Type annotations: `&str`, `&User`, `&int`
  - Borrow expressions: `&value`
- Extend the M8.1 ownership pass with borrow tracking:
  - Multiple immutable borrows of the same value are allowed
  - A value cannot be moved while immutable borrows are live
  - Borrows must not outlive the borrowed value (scope-based, no explicit lifetimes)
- Codegen: take address, pass pointer, automatic dereference at use sites
- See [borrow_checker.md](borrow_checker.md) for the algorithm sketch

**Visible Progress:** Functions can accept arguments without moving them. Multiple readers, no mutation.

**Example:**
```ryo
fn length(s: &str) -> int:
	return s.len()

fn main() -> int:
	name: str = "Alice"
	n1 = length(&name)            # name borrowed, not moved
	n2 = length(&name)            # ok — still readable
	print(name)                   # ok — name still owned here
	return 0
```

**Implementation Notes:**
- Borrows are **non-nullable** (always point to valid data)
- Lifetime tracking is **simplified** — no explicit lifetime annotations, scope-based analysis
- Many borrows are **implicit** at call sites once method receivers (M17) ship
- Dependencies: Milestone 8.1 (move tracker provides the dataflow infrastructure)

### Milestone 8.3: Mutable Borrows (`&mut T`) [alpha]
**Goal:** Add mutable references with aliasing exclusion, completing the v0.1 borrow checker.

**Status:** ⏳ Planned

**Tasks:**
- Add `&mut` syntax to lexer/parser
- Extend type system: `Type::MutRef(Box<Type>)`
- Parse:
  - Type annotations: `&mut User`
  - Borrow expressions: `&mut value`
- Extend the borrow checker with exclusion rules:
  - **At most one mutable borrow** at any point
  - **No immutable borrows while a mutable borrow is live**
  - Mutable borrows still cannot outlive the borrowed value
- Codegen: mutable pointers with write access, dereference for assignment

**Visible Progress:** Mutation through references is safe and aliasing-free. Foundations for `&mut self` methods (M17) and in-place collection updates (M22) are in place.

**Example:**
```ryo
fn increment(x: &mut int):
	*x += 1

fn main() -> int:
	mut count = 0
	increment(&mut count)
	print(int_to_str(count))     # 1

	# Aliasing prevented:
	# r1 = &mut count
	# r2 = &mut count            # compile error: cannot borrow as mutable twice
	# r3 = &count                # compile error: shared while mutable borrow live
	return 0
```

**Implementation Notes:**
- Aliasing exclusion is enforced at compile time (no runtime overhead)
- Explicit `*x` dereference for primitive mutation; method calls auto-dereference once M17 lands
- Edge cases (reborrowing, two-phase borrows) documented in [borrow_checker.md](borrow_checker.md) §5
- Dependencies: Milestone 8.2 (immutable borrows establish the reference machinery)

### Milestone 8.4: String Slices (`&str`) [alpha]
**Goal:** Borrowed views into `str` — zero-copy substrings and string parameters.

**Status:** ⏳ Planned

**Tasks:**
- Recognize `&str` as a fat pointer (ptr + len) borrowed from a `str`
- Parse the slice operator on strings: `s[start:end]`
- Codegen:
  - Slice representation (pointer + length)
  - UTF-8 boundary check at slice creation (panic on invalid boundary)
  - Bounds check at runtime (panic on out-of-range)
- Update standard library and method receivers to prefer `&str` for read-only string parameters

**Visible Progress:** Functions like `first_word(text: &str) -> &str` work. String parsing and tokenization no longer require copying.

**Example:**
```ryo
fn first_word(text: &str) -> &str:
	for i in range(text.len()):
		if text[i] == ' ':
			return text[0:i]      # slice into text, no copy
	return text

fn main() -> int:
	s: str = "hello world"
	word = first_word(&s)         # word: &str borrowed from s
	print(word)                   # "hello"
	print(s)                      # ok — s still owned
	return 0
```

**Implementation Notes:**
- `&str` is a **borrowed view** (immutable, fixed-length); the owning `str` remains the source of truth
- UTF-8 validity is checked at slice creation, not on every read — so iteration is allocation-free
- Array slices `&[T]` are **not** included here; they ship in M21 alongside list literal syntax
- Dependencies: Milestone 8.2 (immutable borrows provide the reference machinery)

### Milestone 8.5: Default Parameters & Named Arguments
**Goal:** Support default parameter values and named arguments for all functions (user-defined and builtins), with named-by-default calling convention

**Status:** ⏳ Planned (depends on Milestone 8)

**Calling Convention (Swift-style):**
- All parameters are **keyword-only by default** — callers must use `name=value`
- `_` before a parameter name opts it into **positional** — callers can pass by position
- Named arguments always work, even for `_` params — `_` adds positional as an option, it doesn't remove named
- Positional args must come before named args at the call site
- This replaces the spec's `#[named]` attribute (Section 6.1.1) with a simpler, safer default

**Tasks:**

1. **Parser Extensions:**
   - Parse `name=expr` in call argument lists (inside `()` = named arg, at statement level = assignment)
   - Parse default values in function parameter definitions: `param: Type = default_expr`
   - Parse `_` prefix on parameter names: `_ param: Type`

2. **AST Extensions:**
   - Add `default: Option<Expression>` and `positional: bool` to function parameter nodes
   - Add `name: Option<String>` to call argument nodes to represent `CallArg { name, value }`

3. **HIR/Lowering:**
   - Validate: defaults must be trailing (no `fn f(a: int = 1, b: int)` — compile error)
   - Validate: named args match parameter names
   - Validate: positional args can only target `_`-marked params
   - Validate: no positional args after named args at call site
   - Insert default values for omitted arguments during lowering
   - All calls arrive at codegen fully resolved

4. **Codegen:**
   - No structural changes needed if lowering inserts defaults
   - All calls arrive at codegen fully resolved with all arguments filled in

5. **Builtin Updates:**
   - Update `BuiltinFunction` struct to include parameter metadata (names, types, defaults, positional flag)
   - Builtins like `print` participate in the named/positional system

6. **Testing:**
   - Default values, named args, `_` positional params
   - Mixing positional + named args
   - Error cases: wrong name, positional for non-`_` param, positional after named, missing required arg, duplicate arg

**Visible Progress:** Functions with defaults and named arguments work. Clear compile errors for argument misuse.

**Example:**
```ryo
# All params keyword-only by default
fn create_user(name: str, age: int, role: str = "user"):
	...

create_user(name="Alice", age=30)              # ok
create_user(name="Alice", age=30, role="admin") # ok
create_user("Alice", 30)                        # compile error

# _ opts into positional
fn add(_ a: int, _ b: int) -> int:
	return a + b

add(1, 2)          # ok
add(a=1, b=2)      # also ok

# Mix: first param positional, rest keyword-only
fn print(_ text: str, end: str = "\n"):
	...

print("hello")              # ok — text positional, end defaults to "\n"
print("hello", end="")      # ok — explicit end
print("hello", "")          # compile error — end is keyword-only
```

**Design Decisions:**
- **Named by default, `_` opts into positional** (Swift model) — replaces the spec's `#[named]` attribute with a simpler, safer default. Proven at scale by Swift for 10+ years
- **Defaults evaluated at each call site** (Kotlin/Swift model), not at definition time — avoids Python's mutable default gotcha where `def f(x=[])` shares the list across calls
- **Defaults must be compile-time evaluable expressions** (literals, constants, future `comptime` calls) — simplifies codegen, aligns with Zig philosophy
- **Trailing position required** for params with defaults (like Python, Kotlin, C++)
- **AI-era rationale:** Named arguments cost the AI nothing — it types for free. But the human reviewer sees exactly what each argument means without cross-referencing the function signature

**Implementation Notes:**
- Struct literal syntax `Point(x=1, y=2)` and function named args `f(x=1)` use identical `name=value` grammar — the parser doesn't need to distinguish them at parse time, resolution happens during lowering
- No function overloading in Ryo, so defaults don't create ambiguity
- Languages analyzed: Go (no defaults — too limiting), Rust (no defaults — relies on builders/traits Ryo lacks), Python (`*` separator — good but `_` is cleaner), Swift (named by default — best fit for Ryo)
- Dependencies: Milestone 8 (control flow for conditional default handling), Milestone 7 (comparison operators)

**Unlocks:** Milestone 9 struct literals share `name=value` parsing infrastructure. Future `print(_ text: str, end: str = "\n")` API.

> **Note:** Closures and lambda expressions were originally planned as Milestone 8.6 but have been **deferred to v0.2** (see Phase 5: Closures & Lambda Expressions). Closures are not strictly required for the v0.1.0 core language; named functions plus the standard library cover every v0.1 use case, and deferring capture analysis (originally M15.5) lets the v0.1 borrow checker stay focused on let/struct/method bindings.

### Milestone 9: Structs
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
- No methods yet (added in Milestone 17)
- Parentheses with named arguments used for struct literals: `Point(x=1, y=2)`, reuses `name=value` parsing infrastructure from Milestone 8.5
- Braces reserved exclusively for f-string interpolation
- Dependencies: Milestone 4 (functions for passing structs), Milestone 8.5 (named argument parsing)

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
- Dependencies: Milestone 9 (structs provide foundation)

### Milestone 11: Enums (Algebraic Data Types) [alpha]
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
- String type used for the example above is available since Milestone 8.1
- Dependencies: Milestone 9 (structs provide foundation for variant data), Milestone 10 (tuples share the variant-payload codegen path)

### Milestone 12: Pattern Matching [alpha]
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
- Dependencies: Milestone 11 (enums to match on)

### Milestone 13: Error Types & Unions [alpha]
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
- Implement explicit error propagation via pattern matching on the union (the `try` sugar lands in v0.2)
- Implement `.message() -> str` method for all errors (compiler-known interface, not a user trait yet)
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

# v0.1 propagates errors explicitly via match. The `try`/`catch` sugar
# (originally Milestone 14) is deferred to v0.2 — see Phase 5.
fn fetch_and_parse() -> (http.ConnectionFailed | http.RequestTimeout | parse.InvalidJson)!Data:
	response = match http_get("https://api.example.com"):
		Ok(r): r
		Err(e): return e          # propagate http errors
	data = match parse_json(response.body()):
		Ok(d): d
		Err(e): return e          # propagate parse errors
	return data

fn main() -> int:
	return 0
```

**Implementation Notes:**
- Errors are **single-variant only** (no multi-variant enums for errors)
- Error unions use `|` syntax for composition
- `.message() -> str` is exposed via a compiler-known interface (the user-facing `trait` keyword is v0.2/v0.3 — see Phase 5: Traits & Generics)
- Ergonomic `try`/`catch` operators are deferred to v0.2 (see Phase 5: Try/Catch Operators); v0.1 uses `match` for propagation and handling
- Dependencies: Milestone 11 (enums), Milestone 12 (pattern matching for handling)

> **Note:** Milestone 14 (`try`/`catch` operators) has been **deferred to v0.2** — see Phase 5: Try/Catch Operators. The error-union *types* and `.message()` accessor remain in v0.1; only the propagation/handling sugar is deferred. Programs use `match` on the error union, which is verbose but fully expressive.

## Phase 3: Type System & Memory Safety

> **Phase 3 is now smaller than originally planned.** The string type, ownership-tracking pass, immutable borrows, mutable borrows, and string slices were all moved forward into Phase 2 as Milestones 8.1–8.4 so that downstream Phase 2 milestones (structs, enums, error types) can use them in their examples and signatures. What remains here is everything that builds *on top of* the borrow checker: optional types, methods, array slices, collections, and RAII.

> **Note:** Milestone 15 (originally "Basic Ownership & String Type") has been **split and moved earlier in the timeline**. See:
> - **Milestone 8.1** — heap-allocated `str` type and move tracking
> - **Milestone 8.2** — immutable borrows `&T`
> - **Milestone 8.3** — mutable borrows `&mut T`
> - **Milestone 8.4** — string slices `&str`
>
> Closure capture analysis (originally Milestone 15.5) is **deferred to v0.2** — see Phase 5: Closures & Lambda Expressions.

### Milestone 16: Optional Types (`?T`) [alpha]
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
- Dependencies: Milestone 11 (enums provide foundation for tagged unions)

### Milestone 17: Method Implementations
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
  - `&self` for immutable borrow (available since M8.2)
  - `&mut self` for mutable borrow (available since M8.3)
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
		return Circle(radius=self.radius * factor)

fn main() -> int:
	c = Circle(radius=5.0)
	a = c.area()              # Consumes c (moved)
	# c.area()                # Error: c was moved
	return 0
```

**Implementation Notes:**
- `self` **moves by default** (ownership)
- Method call syntax: `obj.method()` desugars to `Type::method(obj)`
- No method overloading (one method per name per type)
- Dependencies: Milestone 8.1 (ownership/move tracking for `self`), Milestone 8.2/8.3 (borrows for `&self` / `&mut self`)

> **Note:** Milestone 18 (Traits) has been **deferred to v0.2/v0.3** and folded into the Phase 5 "Traits & Generics" track. Without generics, traits are largely cosmetic — you cannot write `fn max[T: Comparable](...)` — so shipping them together preserves design coherence. A handful of trait-shaped concepts the v0.1 language depends on (`.message()` on errors, `Copy` marker, `Drop` cleanup) are implemented as **compiler-known interfaces** rather than user-extensible traits in v0.1. Methods on user types still work via `impl` blocks (Milestone 17); only the `trait` keyword and trait bounds are deferred.

> **Note:** Milestone 19 (Immutable Borrows) has been **moved to Milestone 8.2** — see Phase 2.

> **Note:** Milestone 20 (Mutable Borrows) has been **moved to Milestone 8.3** — see Phase 2.

### Milestone 21: Array Slices (`&[T]`)
**Goal:** Borrowed views into arrays — zero-copy iteration over sub-ranges. (String slices `&str` already shipped in **M8.4**.)

**Tasks:**
- Add `[T]` array literal syntax to lexer/parser: `[1, 2, 3]`
- Extend type system: `Type::Slice(Box<Type>)` for array slices `&[T]` and `&mut [T]`
- Parse slice operations on arrays:
  - `array[start:end]` — partial slice
  - `array[:]` — full slice
  - `&array[1:4]`, `&mut array[1:4]` — slice borrows
- Codegen:
  - Slice representation (pointer + length, fat pointer)
  - Bounds checking at runtime (panic on out-of-range)
- Write tests for array slices

**Visible Progress:** Efficient array sub-range iteration without copying.

**Example:**
```ryo
fn sum_slice(numbers: &[int]) -> int:
	mut total = 0
	for n in numbers:
		total += n
	return total

fn main() -> int:
	nums = [1, 2, 3, 4, 5]
	total = sum_slice(&nums[1:4])    # pass slice [2, 3, 4]
	print(int_to_str(total))         # 9
	return 0
```

**Implementation Notes:**
- Array slices are **fat pointers** (pointer + length); the same representation `&str` already uses
- Bounds checking at runtime; out-of-range slicing panics
- `[T]` array literals create stack-allocated fixed-size arrays in v0.1; growable `list[T]` lands in M22
- Dependencies: Milestone 8.2 (immutable borrows), Milestone 8.3 (`&mut [T]` slices)

### Milestone 22: Collections (List, Map)
**Goal:** Implement `list[T]` and `map[K, V]` with hardcoded types

**Tasks:**
- Implement `list[int]` and `list[str]` as built-in types:
  - Dynamic array with growth
  - Methods: `append`, `len`, `get`, `remove`
- Implement `map[str, int]` as built-in type:
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
	mut numbers = list[int]()
	numbers.append(1)
	numbers.append(2)
	numbers.append(3)

	mut sum = 0
	for n in numbers:
		sum += n
	print(sum)  # 6

	mut scores = map[str, int]()
	scores.insert("Alice", 100)
	scores.insert("Bob", 85)

	alice_score = scores.get("Alice") orelse 0
	print(alice_score)  # 100

	return 0
```

**Implementation Notes:**
- **Hardcoded types** initially: `list[int]`, `list[str]`, `map[str, int]`
- Generics deferred to Phase 5 (post-v0.1.0)
- Collections own their data (RAII cleanup in M23)
- Iteration uses immutable borrows
- Dependencies: Milestone 8.3 (`&mut` for append/remove), Milestone 21 (array slices for iteration)

### Milestone 23: RAII & Drop (Compiler Intrinsic)
**Goal:** Implement automatic resource cleanup via a compiler-known `drop` method

**Tasks:**
- Recognize `fn drop(&mut self)` as a **compiler-known method name** on any type. No `trait Drop` keyword in v0.1 — the user-facing `trait` system is deferred to v0.2/v0.3 (see Phase 5: Traits & Generics). Once traits land, `Drop` will be promoted to a real trait without breaking source compatibility (the method signature is identical).
- Implement automatic drop calls:
  - At end of scope
  - On early returns
  - On move (drop old value if reassigning)
- Implement Drop for built-in types:
  - `str`: Free heap memory
  - `list[T]`: Free array and drop elements
  - `map[K, V]`: Free table and drop entries
- Extend Codegen: Generate IR for:
  - Drop calls at scope exit
  - Drop calls on early returns
- Write tests for RAII and Drop

**Visible Progress:** No memory leaks! Resources cleaned up automatically.

**Example:**
```ryo
struct File:
	handle: int  # File descriptor

# v0.1: a method literally named `drop` is recognized by the compiler
# as the cleanup hook. v0.2+ will allow `impl Drop for File:` once the
# trait keyword exists; the method body is unchanged.
impl File:
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
- User-defined cleanup via the special `drop` method on `impl` blocks (no `trait Drop` keyword needed yet)
- Prevents resource leaks (files, sockets, memory)
- **Forward compatibility:** when the user-facing trait system ships in v0.2/v0.3, `Drop` becomes a real trait and existing `fn drop(&mut self)` methods continue to work unchanged
- Dependencies: Milestone 17 (methods — `impl` blocks), Milestone 8.3 (mutable borrows for `&mut self`)

## Phase 4: Module System & Core Ecosystem

### Milestone 6: Module System (Implementation)
**Goal:** Implement the module system designed in Milestone 5 — multi-file projects, three-level visibility, name resolution, and circular-dependency detection.

**Status:** ⏳ Planned (Design completed in Milestone 5; implementation lives at the start of Phase 4 so all language constructs are settled before module boundaries are drawn around them.)

**Prerequisites:**
- ✅ Module system design complete (Milestone 5)
- ✅ All v0.1 language features (Milestones 4 through 23) — functions, types, errors, ownership, RAII

**Design Reference:**
Directory-based modules, three access levels (`pub` / `package` / module-private), implicit discovery, hierarchical structure, no circular dependencies between modules. See Milestone 5 (above) and `docs/specification.md` Section 11 for the full design.

**Implementation Tasks:**

1. **Lexer/Parser Extensions:**
   - Add `import`, `package` keywords (note: `pub` already exists from M5 design landing)
   - Extend AST: `StmtKind::ImportStmt`
   - Parse `import math`, `import utils.strings`, and the `package` visibility modifier

2. **Multi-File Project Support:**
   - `src/` as root directory; directories as modules; multiple `.ryo` files per module
   - File system traversal for module discovery; build the module dependency graph

3. **Symbol Table & Name Resolution:**
   - Per-module symbol tables and cross-module name resolution
   - Resolve qualified names: `math.add()`, `utils.strings.format()`
   - Track visibility modifiers: `pub`, `package`, module-private

4. **Visibility Checking:**
   - Three-level access control enforced at use sites with clear diagnostics

5. **Circular Dependency Detection:**
   - Cycles between modules are errors; cycles within a module (same directory) are allowed

6. **Compilation Pipeline Updates:**
   - Dependency-first compilation order
   - Generate separate object files per module (or whole-program), then link

7. **Testing:** imports, visibility, multi-file modules, nested modules, circular detection, real multi-module projects.

**Visible Progress:** Real multi-file projects with proper encapsulation — the v0.1 standard library (M24) lives in modules from day one.

**Example:**
```ryo
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
	return x * 2

fn _validate(x: int) -> int:
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
- Re-exports via `pub use` deferred to a future enhancement (see proposals.md)

**Dependencies:**
- Milestone 5 (design)
- Milestone 23 (everything else — modules wrap a complete language)

**Future Enhancements** (see proposals.md): re-exports `pub use`, glob imports `import utils.*`, conditional compilation `#[cfg(...)]`, workspace support.

### Milestone 24: Standard Library Core [alpha: partial]
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
  - `write_file(path: &str, content: &str) -> io.Error!void`: Write to file
  - `append_file(path: &str, content: &str) -> io.Error!void`: Append to file
- Implement `string` module:
  - `split(s: &str, delimiter: &str) -> list[str]`
  - `join(parts: &[str], separator: &str) -> str`
  - `trim(s: &str) -> &str`
  - `to_upper(s: &str) -> str`, `to_lower(s: &str) -> str`
- Implement `collections` module:
  - `list[T]` methods: `push`, `pop`, `len`, `get`, `clear`
  - `map[K, V]` methods: `insert`, `remove`, `get`, `keys`, `values`
  - Iterator support for `for` loops
- Implement `math` module:
  - `abs(x: float) -> float`
  - `sqrt(x: float) -> float`
  - `pow(base: float, exp: float) -> float`
  - Constants: `PI`, `E`
- Implement `os` module:
  - `args() -> list[str]`: Command-line arguments
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
	# v0.1: explicit error propagation via match (try/catch is v0.2)
	content = match io.read_file(filename):
		Ok(c): c
		Err(e):
			io.println("Error reading file: " + e.message())
			return 1

	words = string.split(content, " ")
	io.println("Word count: " + int_to_str(words.len()))  # v0.1: concat (f-strings v0.2)

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
- Dependencies: Milestone 6 (modules for stdlib organization)

### Milestone 25: Panic & Debugging Support [alpha: partial]
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
  - Print error message to stderr with `file:line:column` (using span info already threaded through `diag.rs`)
  - Exit with non-zero code (typically 101)
- Add `assert` function:
  ```ryo
  fn assert(condition: bool, message: str)
  ```
- Improve compiler error messages:
  - Include source context in compiler errors
  - Better span highlighting for common mistakes
- Write tests for panic and assertions

> **Deferred to v0.2:** full stack-trace generation (DWARF integration, multi-frame backtraces, `RYOLANG_BACKTRACE` env-var control, "did you mean?" compiler suggestions). v0.1 ships single-frame panic with file/line/column — enough to debug, not enough to be polished. See Phase 5: Stack Traces & Compiler Diagnostics.

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

**Output on panic (v0.1):**
```
thread 'main' panicked at 'Division by zero', src/main.ryo:4:9
```

**Output on panic (v0.2+, with DWARF stack traces):**
```
thread 'main' panicked at 'Division by zero', src/main.ryo:4:9
stack backtrace:
   0: divide (src/main.ryo:4)
   1: main (src/main.ryo:11)
note: run with `RYOLANG_BACKTRACE=1` for full backtrace
```

**Implementation Notes:**
- Panic **unwinds the stack** (calls `drop` on all values)
- Panic is **not recoverable** (use error handling for that)
- v0.1 panic is single-frame; full DWARF stack traces ship in v0.2
- Dependencies: Milestone 23 (RAII for unwinding)

### Milestone 26: Testing Framework & Documentation
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
- Add test timeouts:
  - `#[test(timeout=5s)]` attribute parameter for per-test time limits
  - Global `[testing] default-timeout` in `ryo.toml`
  - Timed-out tests trigger `panic("test timed out after {duration}")` with clear diagnostics
- Implement documentation comments:
  - Triple-slash `///` for doc comments
  - Preserved in AST and source for future tooling
- Write tests for test framework itself (meta!)

> **Deferred to v0.2:** `#[bench]` attribute + `ryo bench` runner, and `ryo doc` HTML generator. Benchmarking pre-generics has limited utility (you can't write a generic harness), and HTML doc generation is mostly a polish task that doesn't gate any v0.1 use case. Triple-slash doc comments are *parsed and preserved* in v0.1 so v0.2 can ship the generator without source migration. See Phase 5: Benchmarking & Doc Generation.

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
- Dependencies: Milestone 25 (assert functions)

### Milestone 26.5: Distribution & Installer [alpha: partial]
**Goal:** Zero-friction installation and distribution for v0.1.0 release

**Tasks:**

1. **CI/CD Pipeline:**
   - Set up GitHub Actions workflow for multi-platform builds
   - Build static binaries for:
     - `x86_64-unknown-linux-musl` (Static Linux)
     - `aarch64-unknown-linux-musl` (Static Linux ARM)
     - `aarch64-apple-darwin` (macOS Apple Silicon)
     - `x86_64-pc-windows-msvc` (Windows)
   - Automated release artifact creation
   - Binary signing and checksums

2. **Installation Scripts:**
   - Write `install.sh` for Unix-like systems:
     - OS/Architecture detection (Linux/Darwin, AMD64/ARM64)
     - Download latest `ryo` binary to `~/.ryo/bin/`
     - PATH setup (append to `.zshrc` or `.bashrc`)
   - Write `install.ps1` for Windows:
     - Same logic as shell script
     - Modify User PATH in Registry
     - Install to `%USERPROFILE%\.ryo\`

3. **Zig Dependency Management:** ✅ Implemented in `src/toolchain.rs`
   - Auto-downloads pinned Zig version on first use to `~/.ryo/toolchain/zig-{version}/`
   - No system Zig dependency — fully managed by the compiler

4. **Self-Update Command:**
   - Implement `ryo upgrade` command
   - Check latest release from GitHub/CDN
   - Download and replace binary in `~/.ryo/bin/`
   - Version pinning support (future): `ryo upgrade v0.2.0`

5. **Landing Page:**
   - Simple static page at `ryolang.org`
   - Prominent install command: `curl -fsSL https://ryolang.org/install.sh | sh`
   - Platform-specific instructions
   - Quick start guide

6. **Testing:**
   - Test installation on all target platforms
   - Test upgrade mechanism
   - Test Zig auto-download
   - Test PATH setup

**Visible Progress:** Users can install Ryo with a single command on any platform

**Example:**
```bash
# Install Ryo
curl -fsSL https://ryolang.org/install.sh | sh

# Verify installation
ryo --version

# Update Ryo
ryo upgrade
```

**Implementation Notes:**
- Installation must be **instant, dependency-free, and isolated**
- Zig dependency managed automatically (users don't need to install it)
- All files in `~/.ryo/` directory for clean uninstall
- Windows is a first-class citizen (PowerShell script works seamlessly)
- Dependencies: Milestone 27 prep work (this enables distribution)

### Milestone 27: Core Language Complete & v0.1.0 Prep
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

Acceptance criteria, this code must run well with version v0.1
```ryo
# Errors as values · optionals · safe by default

error NotFound

fn find_user(id: int) -> NotFound!str:
	if id == 0:
		return NotFound
	return f"user_{id}"

fn main():
	# Immutable by default — no `let` keyword
	greeting = "Welcome to Ryo"

	# Optionals — no null pointer exceptions
	maybe: ?str = "Alice"
	name = maybe orelse "guest"

	# Explicit, type-safe error handling
	user = find_user(42) catch |err|:
		match err:
			NotFound: "unknown"

	print(f"{greeting}, {name} → {user}")
```

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

### Task/Future/Channel Runtime (Green Threads & Ambient Runtime)
**Goal:** Implement concurrent programming for I/O-bound applications with "colorless" functions

**Why Post-v0.1.0:**
- Requires mature ownership and type system (Milestones 12-21)
- Complex runtime implementation (executor, scheduler, reactor, stack swapping)
- Not essential for initial adoption (synchronous code works fine)
- Allows more design iteration based on community feedback

**Design Decision: Green Threads (Stack Swapping) vs async/await**

Ryo deliberately **does NOT use `async`/`await` keywords**. Instead, it uses **Green Threads (M:N Threading)** with **Stack Swapping** for the following reasons:

*Rationale:*
- **Avoids function coloring problem:** No distinction between async and sync functions
- **Pythonic simplicity:** Functions look normal, concurrency is transparent
- **Proven approach:** Go has used this successfully for 15+ years
- **Better DX:** No need to mark everything with `async`, no `.await` everywhere

**Implementation: Ambient Runtime Pattern**

Instead of passing runtime context as function parameters (Zig approach), Ryo uses **Thread-Local Storage (TLS)** to provide an "ambient" runtime:

```ryo
import std.task
import std.net

# No runtime parameter needed - looks like regular code!
fn fetch_data(url: str) -> !Data:
	task.sleep(100ms)  # Accesses TLS runtime
	response = try net.get(url)
	return parse(response.body)
```

**How it works:**
1. `task.sleep()` accesses a Thread-Local Variable pointing to the current scheduler
2. If in async runtime: Swaps stack to another task
3. If in blocking runtime: Blocks OS thread
4. If in test: Uses mock runtime

**Testing Pattern:**
```ryo
#[test]
fn test_fetch():
	mock = MockRuntime.create()
	task.with_runtime(mock, fn():
		data = fetch_data("http://example.com")  # Runs instantly
		assert_eq(data.status, 200)
	)
```

**Features Implemented:**

**1. Structured Concurrency (Primary Pattern)**
- `task.scope` - Primary concurrency pattern (not `task.spawn`)
- Prevents resource leaks and zombie tasks
- All tasks in scope must complete before scope exits
- **Fire-and-forget is opt-in:** `task.spawn_detached()` for rare cases

```ryo
import std.task

fn process_all(urls: list[str]) -> !list[Data]:
	task.scope |s|:
		for url in urls:
			s.spawn(fn(): fetch_data(url))
	# Implicit join - all tasks finished or cancelled
	return results
```

**2. Sync Primitives (Not Just Channels)**
- `Mutex[T]` - Mutual exclusion lock
- `RwLock[T]` - Reader-writer lock
- `Atomic[T]` - Lock-free atomic operations

```ryo
import std.sync

cache = Shared(Mutex(map[str, int]()))

fn worker(cache: Shared[Mutex[map[str, int]]]):
	mut m = cache.lock()  # RAII - unlock on scope exit
	m.insert("key", 100)
```

**3. Select Statement & Cancel Safety**
- Non-deterministic operation selection (first to complete wins)
- **Cancel safety:** Unselected operations don't transfer ownership

```ryo
select:
	case data = rx.recv():
		print(f"Received: {data}")
	case tx.send(my_value):
		print("Sent")
	case task.timeout(1s):
		print("Timed out")  # my_value remains valid!
```

**4. Parallelism Spec Updates (Breaking Changes from Single-Threaded)**

Adding M:N threading has **specification impacts** that require changes to earlier milestones:

**A. `Shared[T]` Must Be Atomic Reference Counted (ARC)**
- **Change in Milestone 23 (RAII & Drop)**: `Shared[T]` uses atomic CPU instructions
- **Performance cost:** ~5-10 CPU cycles per clone/drop for thread safety
- **Rationale:** Prevents data races when multiple threads share ownership

**B. Global Mutable State Rules**
- **New Rule:** Global `mut` variables are **forbidden** (compile error) or require `unsafe`
- **Pattern:** Use `static CACHE: Shared[Mutex[Map]]` instead
- **Rationale:** Prevents data races on global state

**C. FFI `#[blocking]` Annotation**
- **New Attribute:** Mark C functions that block OS threads
- **Runtime behavior:** Spawn new OS thread to prevent scheduler starvation
- **Example:**
  ```ryo
  #[blocking]
  extern "C" fn sqlite_exec(db: *void, sql: *c_char) -> int
  ```

**D. Panic Isolation (Task-Level Boundaries)**
- **Behavior:** Panics inside `task.spawn()` kill only that task, not the process
- **Exception:** Panic in `main()` or outside task context crashes process
- **Rationale:** Server with 10,000 requests shouldn't crash if one request panics

**E. Thread-Safe Allocator**
- **Requirement:** Use **mimalloc** or **jemalloc** instead of system malloc
- **Rationale:** System malloc is often slow/contended for multi-threaded workloads

**F. Reserved Keywords**
- `async` and `await` are **reserved** (unused) to prevent breaking changes if design evolves

**Runtime Architecture:**
- **M:N Threading:** M green threads on N OS threads (N = CPU cores)
- **Work-Stealing Scheduler:** Threads steal tasks from each other
- **Stack Swapping:** Save/restore stack pointers when tasks block
- **Default Runtime:** Single-threaded blocking (initialized on first `task` call)
- **Production Runtime:** Multi-threaded, explicit initialization

**Standard Library Modules:**
- `std.task` - Task spawning, scheduling, scopes, timeouts
- `std.channel` - Channel creation, sender/receiver types
- `std.sync` - Mutex, RwLock, Atomic primitives
- `std.net` - Async network I/O (TCP, UDP, HTTP)

**Example (Full Workflow):**
```ryo
import std.task
import std.channel

fn worker(rx: receiver[int], tx: sender[str]):
	for num in rx:
		result = process(num)
		tx.send(result)

fn main():
	rt = MultiThreadedRuntime.new(threads=4)
	rt.run(fn():
		(tx_in, rx_in) = channel.create[int]()
		(tx_out, rx_out) = channel.create[str]()
		
		task.scope |s|:
			# Spawn workers
			for _ in range(4):
				s.spawn(fn(): worker(rx_in.clone(), tx_out.clone()))
			
			# Send work
			for i in range(100):
				tx_in.send(i)
			
			# Collect results
			for _ in range(100):
				result = rx_out.recv()
				print(result)
	)
```

**Implementation Phases:**
1. **Milestone 32:** Green threads runtime, ambient context, basic task spawning
2. **Milestone 33:** Cancellation model (`Canceled`/`Timeout` errors, cooperative cancellation, RAII cleanup on cancel)
3. **Milestone 34:** Parallelism, sync primitives (Mutex/RwLock), spec updates, work-stealing
4. **Milestone 35:** Data parallelism (par_iter, fork-join)
5. **Milestone 36 (optional):** Select statement and advanced cancellation patterns

**Timeline:** v1.5-1.6 (6-12 months after v0.1.0)

### Closures & Lambda Expressions
**Goal:** Implement anonymous functions with capture analysis and ownership-aware semantics

**Why Post-v0.1.0:**
- Named functions plus standard-library iteration (`for ... in`, `range()`) cover every v0.1 use case
- Capture analysis significantly expands the borrow checker's scope; deferring keeps the v0.1 checker focused on let bindings, struct/tuple moves, and method receivers
- Closure ABI (environment layout, fat function pointers, move-vs-borrow capture mode) benefits from a settled borrow-checker and trait system before being committed to
- Higher-order standard-library APIs (`filter`, `map`, `fold`) can wait for the generics system in v0.3+; in the meantime, hardcoded iteration suffices

**Features:**

**1. Closure Syntax (originally M8.6):**

```ryo
# Single-line closure
square = fn(x: int): x * x
print(square(5))  # 25

# Multi-line closure with complex logic
validator = fn(x: int) -> bool:
	if x < 0:
		return false
	return x % 2 == 0

# Closure as parameter (higher-order function)
fn apply(x: int, f: fn(int) -> int) -> int:
	return f(x)

result = apply(5, square)
```

- Single-line: `fn(args): expression`
- Multi-line: `fn(args):` followed by tab-indented block
- First-class values: closures can be passed, returned, stored
- Function pointer optimization for non-capturing closures (zero overhead)

**2. Capture Analysis (originally M15.5):**

```ryo
# Move capture (explicit)
name = "Alice"
greeter = move fn(): f"Hello, {name}"
# name is moved, cannot be used here

# Mutable capture (inferred)
mut counter = 0
increment = fn():
	counter += 1
	return counter

# Borrow capture (default)
data = [1, 2, 3]
printer = fn(): print(data.len())  # immutable borrow of data
```

- Capture modes: immutable borrow (default), mutable borrow (inferred from mutation), move (`move fn(...)` opt-in)
- Borrow checker enforces capture rules at compile time (no runtime overhead)
- Closure types (`Fn`, `FnMut`, `FnMove`) start as compiler concepts; promoted to real traits once the trait system supports it
- See [closure_representation.md](closure_representation.md) for memory layout and ABI details

**Dependencies:**
- Milestone 8 (Control Flow & Booleans) — required for closure bodies
- Milestone 8.2 (Immutable Borrows) and Milestone 8.3 (Mutable Borrows) — required for capture-mode inference
- Milestone 8.1 (Heap-Allocated `str` & Move Semantics) — required for `move` captures

**Effort:** ~3 weeks (closure syntax: 1 week, capture analysis & borrow integration: 2 weeks)
**Timeline:** v0.2 (early post-v0.1.0)

**Future Enhancements** (post-v0.2):
- Closure traits (`Fn`, `FnMut`, `FnOnce`) as real traits in the standard library
- Generic closures: `fn[T](x: T) -> T` (requires generics from v0.3+)
- Devirtualization / inlining optimizations
- Integration with concurrent runtime (v0.4+)

### Foreign Function Interface (FFI)
**Goal:** Comprehensive C interoperability for integrating with existing libraries

**Why Post-v0.1.0:**
- Safety model must be fully tested and stable
- `unsafe` blocks require careful design and auditing
- Not required for pure-Ryo applications
- Community will identify which C libraries are most needed

**Features:**
- `extern "C"` function declarations
- `unsafe` blocks for FFI calls (Requires `kind = "system"` in `ryo.toml`)
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

### Traits & Generics System
**Goal:** User-facing `trait` keyword, trait bounds, generic types and functions — shipped together

**Why Post-v0.1.0:**
- Hardcoded collections (Milestone 22) sufficient for v0.1.0
- Generic implementation is complex (monomorphization, specialization)
- **Traits without generics are cosmetic** — you cannot write `fn max[T: Comparable](...)`, so shipping the `trait` keyword separately from generics produces an awkward intermediate state. Folding them together preserves design coherence.
- Community feedback will inform design (variance, associated types, etc.)
- v0.1 already exposes a few trait-shaped concepts as **compiler-known interfaces** (`.message()` on errors, `Copy` marker, `drop` method for RAII); promoting them to real traits in v0.2/v0.3 is source-compatible.

**Migration from v0.1 compiler intrinsics to real traits:**
- `fn drop(&mut self)` (compiler-known method) → `impl Drop for T` (no source change to method body)
- `Copy` (compiler marker) → user-derivable `#[derive(Copy)]`
- `.message()` (compiler-known error accessor) → `impl Error for T` with explicit `fn message(&self) -> str`

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
	items: list[T]

impl[T] Stack[T]:
	fn push(&mut self, item: T):
		self.items.append(item)

	fn pop(&mut self) -> ?T:
		return self.items.pop()
```

**Phasing:**
- **v0.2:** `trait` keyword, trait definitions, `impl Trait for T`, basic trait bounds. Standalone traits without generics enable user-defined `Drop`, `Copy` derivations, and `Display` for f-strings (below). Promoted from v0.1 compiler intrinsics.
- **v0.3:** Generic functions and types with trait bounds (`fn max[T: Comparable](...)`, `struct Stack[T]`), generic standard library replacing hardcoded `list[int]` / `map[str, int]`.
- **v0.4+:** Associated types, default trait methods, dynamic dispatch (`dyn Trait`).

**Timeline:** v0.2 (traits, ~3 weeks), v0.3 (generics, 18-24 months after v0.1.0)

### Try/Catch Operators
**Goal:** Ergonomic error propagation and handling sugar over error unions

**Why Post-v0.1.0:**
- v0.1 ships error union *types* (Milestone 13) and pattern matching (Milestone 12), so error handling is fully expressible — just verbose
- `try`/`catch` is pure sugar; deferring it lets v0.2 design the operator alongside the trait system and `?` shorthand as a coherent ergonomics package

**Features:**
- `try expr` — propagates errors to the caller, unwraps the success value
- `expr catch |e|: handler` — expression-based error handling with pattern matching on `e`
- Automatic error-union composition (no manual enum construction at propagation sites)

**Example:**
```ryo
# v0.2 sugar (this section)
fn load_config(path: &str) -> (file.NotFound | parse.InvalidFormat)!Config:
	content = try read_file(path)
	config = try parse_config(content)
	return config

result = load_config("config.toml") catch |e|:
	match e:
		file.NotFound(p): print("missing: " + p)
		parse.InvalidFormat(m): print("bad format: " + m)
	return 1
```

```ryo
# v0.1 equivalent (verbose but expressive)
fn load_config(path: &str) -> (file.NotFound | parse.InvalidFormat)!Config:
	content = match read_file(path):
		Ok(c): c
		Err(e): return e
	config = match parse_config(content):
		Ok(c): c
		Err(e): return e
	return config
```

**Effort:** ~2-3 weeks
**Dependencies:** v0.1 error unions (M13), pattern matching (M12)
**Timeline:** v0.2 (early post-v0.1.0)

### F-strings & String Interpolation
**Goal:** Inline expression interpolation in string literals: `f"Hello, {name}! Score: {score}"`

**Why Post-v0.1.0:**
- v0.1 ships string concatenation (`+`) and stdlib `int_to_str` / `float_to_str` / `bool_to_str` helpers — every f-string is mechanically rewritable to concat form
- F-strings need an interpolation parser, type-directed `to_string()` resolution (cleanest with the `Display` trait from v0.2 traits), and codegen for arbitrary expression splicing — better designed alongside the trait system
- Curly braces are already reserved by the lexer, so the v0.2 addition is non-breaking

**Features:**
- `f"..."` literal prefix triggers interpolation
- `{expr}` splices any expression whose type implements `Display` (from v0.2 trait system)
- Format specifiers: `{x:.2}` for floats, `{n:04}` for zero-padded ints (Python/Rust-inspired)
- Compile-time parsed: invalid expressions or missing `Display` impls produce errors at the interpolation site

**Example:**
```ryo
# v0.2
name = "Alice"
score = 95.5
msg = f"Hello, {name}! Score: {score:.1}"

# v0.1 equivalent
msg = "Hello, " + name + "! Score: " + float_to_str(score)
```

**Effort:** ~1-2 weeks (after v0.2 trait system lands)
**Dependencies:** v0.2 traits (for `Display`), v0.1 strings (M15)
**Timeline:** v0.2

### Stack Traces & Compiler Diagnostics Polish
**Goal:** Multi-frame DWARF stack traces on panic, plus "did you mean?" suggestions in compiler errors

**Why Post-v0.1.0:**
- v0.1 panic prints a single frame (`file:line:column`) which is sufficient to debug; multi-frame traces are polish, not correctness
- DWARF integration is platform-sensitive (Linux/macOS/Windows differ) and benefits from settling after distribution lands (M26.5)
- Suggestion engine (`Levenshtein` over identifier scopes, structured fix-it hints) is independent of language semantics — pure tooling work

**Features:**
- Multi-frame stack traces using DWARF debug info
- `RYOLANG_BACKTRACE=1` env var for full traces (default: short)
- `RYOLANG_BACKTRACE=0` to disable entirely
- Stripped release builds with optional symbol files
- "did you mean?" suggestions for typos in identifiers, type names, method calls
- Source context with caret highlighting in compiler errors (improvement over v0.1)

**Effort:** ~1.5-2 weeks
**Dependencies:** v0.1 panic (M25), Drop/RAII (M23) for unwinding
**Timeline:** v0.2

### Benchmarking & Doc Generation
**Goal:** `#[bench]` benchmark harness and `ryo doc` HTML generator

**Why Post-v0.1.0:**
- Benchmarking pre-generics has limited utility — you can't write a generic harness, so most v0.1 benchmarks would be tightly coupled to specific types
- HTML doc generation is a polish task; v0.1 *parses and preserves* `///` doc comments so v0.2 can ship the generator without source migration
- Both features are independent of language semantics — pure tooling work that benefits from a stabilized core

**Features:**
- `#[bench]` attribute + `ryo bench` runner with statistical sampling (warmup, iterations, mean/stddev)
- `ryo doc` HTML generator: cross-linked module/type/method pages, embedded examples, search index
- Markdown rendering in doc comments
- Doctest extraction: examples in `///` comments runnable via `ryo test`

**Effort:** ~2 weeks (bench: ~3 days, doc generator: ~7 days)
**Dependencies:** v0.1 testing framework (M26), attribute system (M26)
**Timeline:** v0.2

### Constrained Types & Distinct Types (Ada-Inspired Type Safety)
**Goal:** Add range-bounded types and strong typedefs for compile-time constraint enforcement

**Why Post-v0.1.0:**
- Requires mature attribute system and type checker (Milestones 17, 26)
- Builds on type conversion syntax (`TargetType(value)`) already in v0.1
- Not essential for initial adoption (manual validation works)
- Ada-inspired features benefit from community feedback on syntax

**Features:**

**1. Constrained Types (Range Types)**

Define numeric types with compile-time and runtime bounds:

```ryo
type Port = int(1..65535)
type Percentage = float(0.0..100.0)
type HttpStatus = int(100..599)

fn serve(port: Port):
    bind(port)  # guaranteed valid

fn main():
    serve(Port(8080))              # compile-time check: ok
    serve(Port(70000))             # compile-time error: out of range
    p = Port(user_input)           # runtime check: panics if out of range
    p = try Port.checked(input)    # safe: returns RangeError!Port
```

**Implementation Tasks:**
1. **Type System:** Add `ConstrainedType` variant to type representation (base type + min + max)
2. **Parser:** Parse `type Name = BaseType(min..max)` syntax (reuses range `..` operator)
3. **Compile-Time Check:** When constructing from a literal, verify bounds during type checking
4. **Runtime Check:** When constructing from a dynamic value, emit bounds check + panic/error
5. **`.checked()` Method:** Generate a function that returns `RangeError!T` instead of panicking
6. **Introspection:** Expose `.min` and `.max` as compile-time constants
7. **Arithmetic:** Operations on constrained types produce the base type (explicit re-constraining required)

**2. Distinct Types (Strong Typedefs)**

Create new nominal types that share representation but prevent accidental mixing:

```ryo
type Meters = distinct float
type Seconds = distinct float

fn speed(distance: Meters, time: Seconds) -> float:
    return float(distance) / float(time)

d = Meters(100.0)
t = Seconds(9.58)
v = speed(d, t)        # ok
v = speed(t, d)        # compile error: expected Meters, got Seconds
```

**Implementation Tasks:**
1. **Type System:** Add `DistinctType` variant (wraps base type with new nominal identity)
2. **Parser:** Parse `type Name = distinct BaseType` syntax
3. **Type Checker:** Distinct types are incompatible with their base type and each other
4. **Conversion:** Explicit `BaseType(distinct_val)` and `DistinctType(base_val)` required
5. **Composition:** Allow `type Port = distinct int(1..65535)` (distinct + constrained)
6. **Codegen:** Zero overhead — same representation as base type, all checks are compile-time

**Effort:** ~3-4 weeks total (constrained: 2-3 weeks, distinct: 1 week)
**Dependencies:** Type checker (Phase 3), type conversion syntax (v0.1)
**Timeline:** v0.2 (early post-v0.1.0)

### Contracts (`#[pre]`/`#[post]`)
**Goal:** Add function precondition and postcondition attributes for enforced documentation

**Why Post-v0.1.0:**
- Requires attribute system (`#[...]` parsing and AST representation)
- Requires functions, boolean expressions, `panic()` — all v0.1 features
- Contracts are syntactic sugar over `if not: panic()`, so once prerequisites exist the implementation is small

**Features:**

```ryo
#[pre(amount > 0)]
#[pre(balance >= amount)]
#[post(result.balance == balance - amount)]
fn withdraw(balance: int, amount: int) -> BankError!Account:
    if amount > balance:
        return BankError("insufficient funds")
    return Account(balance=balance - amount)

#[pre(items.len() > 0)]
fn average(items: list[float]) -> float:
    return sum(items) / float(items.len())
```

**Implementation Tasks:**

1. **Attribute Parsing (shared cost):**
   - Extend lexer to distinguish `#[` from `#` comments
   - Parse `#[name(expression)]` into attribute AST nodes
   - Store `Vec<Attribute>` on function definitions
   - *This infrastructure is shared with `#[test]`, `#[blocking]`, `#[named]`*

2. **Precondition Injection:**
   - For each `#[pre(expr)]`, insert `if not (expr): panic("precondition failed: {expr_text} at {function} ({file}:{line})")` at function entry
   - Multiple `#[pre]` attributes checked in order

3. **Postcondition Injection:**
   - For each `#[post(expr)]`, rewrite every `return value` to:
     - Store value in `__result` temporary
     - Check `if not (expr_with_result_substituted): panic(...)`
     - Return `__result`
   - The identifier `result` in `#[post]` expressions refers to the return value
   - For error union returns, postconditions only apply to the success path

4. **Configuration:**
   - `--contracts=enforce` (default): Emit all checks
   - `--contracts=off`: Strip all contract checks (zero overhead)
   - Profile-based configuration in `ryo.toml`

5. **Testing:**
   - Tests for precondition violations (should panic with clear message)
   - Tests for postcondition violations
   - Tests for multiple return points with postconditions
   - Tests for error union functions (postcondition on success path only)
   - Tests for `--contracts=off` (should not emit checks)

**Visible Progress:** Functions declare their invariants as enforced documentation. Violations produce clear diagnostics.

**Violation Output:**
```
ContractViolation: precondition failed: amount > 0
  in function 'withdraw' at src/bank.ryo:3
  contract defined at src/bank.ryo:1
```

**Effort:** ~1-2 weeks (once attribute system exists from `#[test]` milestone)
**Dependencies:** Attribute system (Milestone 26), functions (M4), boolean expressions (M8), panic (M25)
**Timeline:** v0.2 (ships alongside attribute system)

**Hardest Part:** Handling `#[post]` with multiple return points — each `return` must be rewritten to check the postcondition before returning. This is a well-understood AST transformation but requires care.

### Copy Elision & Return Value Optimization
**Goal:** Implement guaranteed copy elision (NRVO) for return values and move parameters

**Why Post-v0.1.0:**
- v0.1.0 can use naive copies for returns — correctness first, optimization second
- Requires mature ownership system (Milestones 15-23) to know what's safe to elide
- NRVO is an optimization pass, not a semantic feature — adding it later doesn't break existing code

**Features:**
- Guaranteed elision (G1-G4): local returns, literal construction, last-use moves, tail chains
- Permitted elision (P1-P4): branch returns, match arms, loop exits, struct field moves
- Hidden output pointer calling convention for eligible return sites
- Integration with HIR lowering pipeline

**Implementation Details:** See [copy_elision.md](copy_elision.md) for the full G/P/F classification and algorithm sketch.

**Effort:** ~2-3 weeks
**Dependencies:** Ownership (M15), Borrows (M19-M20), RAII (M23)
**Timeline:** v0.2 (early post-v0.1.0)

### Standard Library Allocation Optimizations (SSO, COW)
**Goal:** Implement small-string optimization and copy-on-write for the `str` type

**Why Post-v0.1.0:**
- Requires stable `str` type representation (Milestone 8.1)
- SSO changes the internal layout — must be decided before ABI stabilization
- COW requires atomic refcounting infrastructure (shared with `shared[T]`)
- Performance optimization, not correctness — v0.1.0 works without it

**Features:**
- Small-string optimization: inline storage for strings ≤23 bytes (zero allocation)
- Copy-on-write: immutable strings share backing buffers, allocate on mutation
- Sink-parameter convention: `move T -> T` pattern for buffer-building APIs
- Atomic COW refcounts for thread safety

**Implementation Details:** See [stdlib_optimizations.md](stdlib_optimizations.md) for SSO threshold, COW semantics, and sink-parameter guidelines.

**Effort:** ~3-4 weeks
**Dependencies:** String type (M15), Copy elision (above), Concurrency runtime (for atomic refcounts)
**Timeline:** v0.2-v0.3

### Cancellation Model (`Canceled`/`Timeout` Errors)
**Goal:** Define clear cancellation semantics for the concurrency runtime with built-in error types

**Why Post-v0.1.0:**
- Requires concurrency runtime (Milestone 32) to be functional
- Requires error unions and error types (v0.1 features)
- Cancellation semantics must be designed alongside green threads, not after

**Features:**

```ryo
import std.task

# Built-in error types
error Canceled
error Timeout

# Cooperative cancellation — delivered at suspension points
worker = task.run:
    result = expensive_calculation(data)
    try save_to_db(result)  # Canceled delivered here if task cancelled
    return result

# Timeout integration
result = try task.timeout(5s, worker).await catch |e|:
    match e:
        task.Canceled:
            log("Task cancelled")
            return fallback
        task.Timeout:
            log("Task timed out")
            return default
```

**Implementation Tasks:**

1. **Built-in Error Types:**
   - Define `Canceled` and `Timeout` as unit errors in `std.task`
   - Ensure they compose with error unions (`(Canceled | Timeout | HttpError)!Data`)

2. **Cooperative Cancellation Delivery:**
   - At each suspension point (I/O, channel ops, `.await`, `task.delay`), check cancellation flag
   - If cancelled, return `Canceled` error instead of performing the operation
   - Pure computation is never interrupted — only suspension points check

3. **RAII Cleanup on Cancellation:**
   - When `Canceled` propagates up the stack, `Drop` implementations and `with` blocks execute normally
   - Cancellation unwinds in reverse declaration order, same as normal scope exit
   - Test: verify file handles, connections, and locks are released on cancel

4. **Cancellation Sources:**
   - Dropping a `future[T]` sets the cancellation flag on its associated task
   - `task.scope` exit cancels all remaining child tasks
   - `select` losing branches receive cancellation
   - `task.timeout` sets cancellation flag when duration expires
   - `fut.cancel()` explicit method sets flag immediately

5. **Testing:**
   - Test cancellation at various suspension points (I/O, channel, delay)
   - Test RAII cleanup during cancellation (Drop runs, with blocks clean up)
   - Test cancellation propagation through `try`
   - Test `task.scope` cancels children when one panics
   - Test `select` cancel safety (losing operations clean up)

**Visible Progress:** Tasks cancel cooperatively with clear error types. Resources are always cleaned up. No leaked file handles, connections, or locks on cancellation.

**Effort:** ~2-3 weeks (integrated with Milestone 32-33 runtime work)
**Dependencies:** Concurrency runtime (Milestone 32), error unions (v0.1), Drop trait (Milestone 23)
**Timeline:** v0.4+ (ships with concurrency runtime, Milestone 33)

### Named Parameters (`#[named]`)
**Goal:** Allow functions to require callers to use named arguments

**Why Post-v0.1.0:**
- Requires attribute system (shared with `#[test]`, `#[pre]`, `#[post]`)
- Low priority — optional call-site ergonomics, not a safety feature

**Features:**

```ryo
#[named]
fn create_user(name: str, age: int, role: str):
    # ...

create_user(name="Alice", age=30, role="admin")   # ok
create_user("Alice", 30, "admin")                   # compile error
```

**Implementation Tasks:**
1. Parse `#[named]` attribute (reuses attribute system)
2. At call sites for `#[named]` functions, verify all arguments are named
3. Clear error message: "function 'create_user' requires named arguments"

**Effort:** ~2-3 days (trivial once attribute system exists)
**Dependencies:** Attribute system (Milestone 26)
**Timeline:** v0.3

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
- **HTTP Client/Server:** HTTP/2 and HTTP/3 support with concurrent handlers
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
- **Concurrent Runtime:** Runtime library for Task/Future support (to be determined)

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
- ❌ Closures & lambda expressions (v0.2 — incl. capture analysis)
- ❌ User-facing `trait` keyword & trait bounds (v0.2 — v0.1 has compiler-known interfaces for `drop`/`Copy`/`.message()`)
- ❌ `try`/`catch` operators (v0.2 — v0.1 uses `match` for error propagation)
- ❌ F-strings / string interpolation (v0.2 — v0.1 uses `+` concatenation with `*_to_str` helpers)
- ❌ Multi-frame stack traces & "did you mean?" suggestions (v0.2 — v0.1 has single-frame panic)
- ❌ `#[bench]` benchmark harness & `ryo doc` HTML generator (v0.2 — doc comments are still parsed)
- ❌ Constrained types / range types (v0.2)
- ❌ Distinct types / strong typedefs (v0.2)
- ❌ Contracts — `#[pre]`/`#[post]` (v0.2)
- ❌ Task/Future runtime (v0.4+)
- ❌ FFI/unsafe blocks (v0.2+)
- ❌ Full generics system (v0.3+)
- ✅ Named parameters & default values (v0.1 — Milestone 8.5)
- ❌ LSP/advanced tooling (v0.2+)

This foundation enables building **synchronous applications** including CLI tools, build systems, compilers, data processing pipelines, and game engines. Concurrency/FFI features will follow based on community needs.

## Development Timeline

### Realistic Estimates (2-4 weeks per milestone)

**Phase 1 (M1-M3.5):** ✅ COMPLETE (~2 months)
**Phase 2 (M4-M13):** 14 milestones — incl. M8.1 (str+heap), M8.2 (&T), M8.3 (&mut T), M8.4 (&str); excl. closures and try/catch (v0.2) and M6 (now early-Phase-4) × 3 weeks avg = ~42 weeks (~10 months)
**Phase 3 (M16, M17, M21, M22, M23):** 5 milestones — strings/borrows pulled forward to Phase 2; traits and closure capture deferred to v0.2 × 3 weeks avg = ~15 weeks (~4 months)
**Phase 4 (M24-M27):** 5 milestones (includes M26.5 Distribution & Installer) × 4 weeks avg = ~20 weeks (~5 months)

**Total Estimated Time:** 89 weeks (~22 months) from Phase 2 start to v0.1.0

### Development Approach

- **Incremental:** Working software at every milestone
- **Flexible:** Adjust timeline for quality (testing, bug fixes, polish)
- **Parallel Work:** Documentation, testing, examples can overlap with implementation
- **Community-Driven:** Beta testing and feedback incorporated before v0.1.0 release

### Milestones by Complexity

**Simple (2 weeks):** M4, M5, M7, M10, M8.4
**Medium (3 weeks):** M6, M8, M8.1, M8.2, M8.3, M8.5, M9, M11, M12, M13, M16, M17, M21, M22, M24, M25, M26, M26.5
**Complex (4-5 weeks):** M20, M23, M27

This timeline is **realistic** based on compiler development best practices. Each milestone includes implementation, testing, documentation, and examples.

## Known Limitations & Design Trade-offs

This section documents intentional limitations and pragmatic trade-offs in the roadmap.

### v0.1.0 Intentional Omissions

**No Generics in v0.1.0:**
- **Why:** Generic implementation is complex (monomorphization, specialization, error messages)
- **Workaround:** Hardcoded collection types (`list[int]`, `list[str]`, `map[str, int]`)
- **Impact:** Some code duplication, but v0.1.0 remains usable for most applications
- **Timeline:** Full generics in v0.4+ (Phase 5)

**No Concurrency Runtime in v0.1.0:**
- **Why:** Requires mature runtime, complex implementation, not essential for initial adoption
- **Workaround:** Use synchronous I/O (works fine for many applications)
- **Impact:** Higher latency for I/O-bound applications, but predictable performance
- **Timeline:** Task/Future runtime in v0.2+ (Phase 5)

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

**No User-Facing Traits (v0.1.0):**
- The `trait` keyword and trait bounds are deferred to v0.2/v0.3 (shipped with generics)
- v0.1 exposes a small fixed set of compiler-known interfaces: `drop` (RAII), `Copy` (marker), `.message()` (errors)
- Trade-off: less abstraction power, but avoids "traits without generics" awkwardness

**No Dynamic Dispatch (v0.1.0):**
- `dyn Trait` deferred until traits + generics ship
- Trade-off: simpler v0.1, fewer cross-cutting design decisions

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
- Timeline: Threading in Phase 5 (alongside Task runtime)

### Rationale for Trade-offs

These limitations are **intentional** to:
1. **Reach v0.1.0 faster** - Avoid scope creep, ship working language
2. **Validate core design** - Get community feedback before advanced features
3. **Maintain quality** - Better to ship complete simple features than half-baked complex ones
4. **Iterate based on usage** - Real-world usage will inform priorities for Phase 5

The goal is a **production-ready core language** that can evolve based on actual user needs rather than speculation.

## Conclusion

This roadmap represents an **honest, achievable plan** for building Ryo v0.1.0 over approximately 18-26 months. By deferring advanced features (concurrency runtime, FFI, generics) to Phase 5, we can deliver a solid, usable language faster while maintaining room for future growth.

**Next steps:**
1. Complete Phase 2 (Functions, Control Flow, Core Types)
2. Implement Phase 3 (Ownership, Type System, Memory Safety)
3. Build Phase 4 (Modules, Stdlib, Tooling)
4. Release v0.1.0 and gather community feedback
5. Iterate on Phase 5 features based on real-world needs

**Join us in building Ryo!** See [CONTRIBUTING.md](../CONTRIBUTING.md) for how to get involved.

## References

- Spec: `docs/specification.md` — canonical language specification; this roadmap schedules its delivery
- Dev: [alpha_scope.md](alpha_scope.md), [pipeline_alignment.md](pipeline_alignment.md), [middle_end_refactor.md](middle_end_refactor.md) — implementation plans linked from milestones
- Milestone: alpha milestones tagged `[alpha]` inline; see [alpha_scope.md](alpha_scope.md) for the alpha delivery slice
