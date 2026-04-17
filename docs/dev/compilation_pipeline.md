# Compilation Pipeline Architecture

**Target Audience:** Contributors, maintainers, compiler developers

This document provides a detailed technical explanation of how Ryo compiles source code into native executables. Understanding this pipeline is essential for contributing to the compiler.

## Table of Contents

1. [Overview](#overview)
2. [Phase 1: Lexical Analysis](#phase-1-lexical-analysis)
3. [Phase 2: Syntax Analysis](#phase-2-syntax-analysis)
4. [Phase 3: Lowering (AST → HIR)](#phase-3-lowering-ast--hir)
5. [Phase 4: Code Generation](#phase-4-code-generation)
6. [Phase 5: Linking](#phase-5-linking)
7. [Phase 6: Execution](#phase-6-execution)
8. [Design Decisions](#design-decisions)
9. [Future Evolution](#future-evolution)

---

## Overview

The Ryo compilation pipeline transforms source code through six distinct phases:

```
┌─────────────┐    ┌────────┐    ┌────────┐    ┌─────────┐    ┌─────────┐    ┌────────┐    ┌────────────┐
│ Source Code │ -> │ Lexer  │ -> │ Parser │ -> │ Lower   │ -> │ Codegen │ -> │ Linker │ -> │ Executable │
│  (.ryo)     │    │(Tokens)│    │ (AST)  │    │ (HIR)   │    │  (.o)   │    │        │    │  (native)  │
└─────────────┘    └────────┘    └────────┘    └─────────┘    └─────────┘    └────────┘    └────────────┘
```

### High-Level Flow

1. **Source** → **Tokens** (`src/lexer.rs` + `src/indent.rs`)
2. **Tokens** → **AST** (`src/parser.rs`)
3. **AST** → **HIR** (`src/lower.rs`) — scope resolution, type checking, implicit main wrapping
4. **HIR** → **Cranelift IR** → **Object File** (`src/codegen.rs`)
5. **Object File** + **C Runtime** → **Executable** (`src/linker.rs`)
6. **Execute** and capture exit code (`src/pipeline.rs`)

---

## Phase 1: Lexical Analysis

**Module:** `src/lexer.rs`
**Library:** [Logos](https://docs.rs/logos/) v0.15
**Function:** Tokenize source code into a stream of tokens

### Implementation

The lexer uses the Logos procedural macro to define tokens:

```rust
#[derive(Logos, Debug, PartialEq, Eq, Hash, Clone)]
pub enum Token<'a> {
    #[token("fn")]
    Fn,

    #[token("mut")]
    Mut,

    #[regex("[a-zA-Z_][a-zA-Z0-9_]*")]
    Ident(&'a str),

    #[regex("[0-9]+")]
    Int(&'a str),

    #[token("+")]
    Add,

    // ... more tokens
}
```

### Process

1. Input: Raw source code as `&str`
2. Logos scans character by character
3. Matches patterns to token types
4. Returns iterator of `Result<Token, ()>`
5. Comments and whitespace are skipped automatically

### Example

**Input:**
```ryo
x = 42
```

**Token Stream:**
```
Ident("x"), Assign, Int("42")
```

### Span Tracking

Logos provides `Span` (byte offsets) for each token:
```rust
let mut lex = Token::lexer(input).spanned();
for (result, span) in lex {
    // span: Range<usize> (e.g., 0..1, 2..3, 4..6)
}
```

These spans are crucial for error reporting in later phases.

### Code Reference

- **Token definition:** `src/lexer.rs`
- **Indent preprocessor:** `src/indent.rs` — inserts synthetic `Indent`/`Dedent` tokens
- **Tests:** `src/lexer.rs` (unit tests), `src/indent.rs` (unit tests)
- **CLI integration:** `src/pipeline.rs::lex_command`

---

## Phase 2: Syntax Analysis

**Module:** `src/parser.rs`
**Library:** [Chumsky](https://docs.rs/chumsky/) v0.11
**Function:** Parse token stream into Abstract Syntax Tree (AST)

### Architecture

The parser uses combinators to build a recursive descent parser:

```rust
pub fn program_parser<'a, I>() -> impl Parser<'a, I, Program, extra::Err<Rich<'a, Token<'a>>>> + 'a
where
    I: ValueInput<'a, Token = Token<'a>, Span = SimpleSpan>,
{
    statement_parser()
        .repeated()
        .collect::<Vec<_>>()
        .map_with(|statements, e| {
            // Build Program node with span
        })
        .then_ignore(end())
}
```

### Grammar Structure

The parser handles:

1. **Program** = Statement*
2. **Statement** = VarDecl | FunctionDef | Return | ExprStmt
3. **FunctionDef** = `fn name(params) [-> type]: body`
4. **VarDecl** = `[mut] ident [: type] = expression`
5. **Expression** = Binary | Unary | Literal | Ident | Call | `( Expression )`

### Operator Precedence

Implemented using separate parser levels:

```rust
// Highest precedence: atoms (literals, parenthesized)
let atom = int_literal | parenthesized_expr;

// Unary operators: -expr
let unary = op('-').ignore_then(atom).or(atom);

// Multiplication/Division (higher than +/-)
let term = unary.foldl(...);  // * and /

// Addition/Subtraction (lower precedence)
let expr = term.foldl(...);   // + and -
```

**Example:**
- `2 + 3 * 4` parses as `Add(2, Mul(3, 4))` ✅
- NOT as `Mul(Add(2, 3), 4)` ❌

### AST Structure

The parser produces a structured AST (defined in `src/ast.rs`):

```rust
pub struct Program {
    pub statements: Vec<Statement>,
    pub span: SimpleSpan,
}

pub struct Statement {
    pub kind: StmtKind,
    pub span: SimpleSpan,
}

pub enum StmtKind {
    VarDecl(VarDecl),
    FunctionDef(FunctionDef),
    Return(Option<Expression>),
    ExprStmt(Expression),
}

pub struct VarDecl {
    pub mutable: bool,
    pub name: Ident,
    pub type_annotation: Option<TypeExpr>,
    pub initializer: Expression,
}

pub struct Expression {
    pub kind: ExprKind,
    pub span: SimpleSpan,
}

pub enum ExprKind {
    Literal(Literal),
    Ident(String),
    BinaryOp(Box<Expression>, BinaryOperator, Box<Expression>),
    UnaryOp(UnaryOperator, Box<Expression>),
    Call(String, Vec<Expression>),
}
```

### Error Handling

Chumsky provides rich error messages using Ariadne:

```rust
fn display_parse_errors(errs: &[Rich<'_, Token<'_>>], input: &str) {
    let source = Source::from(input);
    for err in errs {
        Report::build(ReportKind::Error, (SOURCE_ID, err.span().start..err.span.end))
            .with_message(err.to_string())
            .with_label(Label::new((SOURCE_ID, err.span().into_range()))
                .with_message(err.reason().to_string())
                .with_color(Color::Red))
            .finish()
            .eprint((SOURCE_ID, &source))
            .unwrap();
    }
}
```

### Code Reference

- **Parser definition:** `src/parser.rs`
- **AST types:** `src/ast.rs`
- **Tests:** `src/parser.rs` (unit tests)
- **Error display:** `src/pipeline.rs::display_parse_errors`

---

## Phase 3: Lowering (AST → HIR)

**Module:** `src/lower.rs`, `src/hir.rs`
**Function:** Transform the AST into a typed, analyzed High-level IR (HIR)

This phase performs semantic analysis comparable to Zig's Sema pass. The HIR is a post-analysis IR where all types are resolved, scopes are checked, and the program structure is normalized.

### HIR Data Structures

Defined in `src/hir.rs`:

```rust
pub enum Type { Int, Str, Void }

pub struct HirProgram { pub functions: Vec<HirFunction> }

pub struct HirFunction {
    pub name: String,
    pub params: Vec<HirParam>,
    pub return_type: Type,
    pub body: Vec<HirStmt>,
}

pub enum HirStmt {
    VarDecl { name: String, mutable: bool, ty: Type, initializer: HirExpr, span: Span },
    Return(Option<HirExpr>, Span),
    Expr(HirExpr, Span),
}

pub enum HirExprKind {
    IntLiteral(isize),
    StrLiteral(String),
    Var(String),
    BinaryOp(Box<HirExpr>, BinaryOp, Box<HirExpr>),
    UnaryOp(UnaryOp, Box<HirExpr>),
    Call(String, Vec<HirExpr>),
}
```

### What Lowering Does

1. **Scope resolution:** Uses a `Scope` struct with parent pointers for nested lookup
2. **Type inference:** Resolves variable types from initializers and annotations
3. **Implicit main wrapping:** Flat programs (no `fn main()`) get wrapped in a synthetic main returning 0
4. **Function signature collection:** Two-pass approach — collect all signatures first, then lower bodies (enables forward references)
5. **Builtin resolution:** Resolves builtin functions (e.g., `print`) via `src/builtins.rs` registry
6. **Validation:** Rejects nested function definitions, top-level statements when explicit main exists

### Example

**Input AST** (from `x = 42`):
```
Program → [VarDecl { name: "x", initializer: Literal(42) }]
```

**Output HIR:**
```
HirProgram {
    functions: [HirFunction {
        name: "main",
        params: [],
        return_type: Int,
        body: [
            VarDecl { name: "x", ty: Int, initializer: IntLiteral(42) },
            Return(Some(IntLiteral(0)))
        ]
    }]
}
```

### Code Reference

- **HIR types:** `src/hir.rs`
- **Lowering pass:** `src/lower.rs`
- **Builtin registry:** `src/builtins.rs`
- **Tests:** `src/lower.rs` (13 unit tests)

---

## Phase 4: Code Generation

**Module:** `src/codegen.rs`
**Library:** [Cranelift](https://docs.rs/cranelift/) v0.125
**Function:** Translate HIR into native object files via Cranelift IR

This phase takes the fully typed HIR and generates Cranelift IR instructions.

### Cranelift Overview

Cranelift is a code generation library that:
- Produces native machine code
- Supports multiple targets (x86-64, aarch64, etc.)
- Provides both JIT and AOT compilation
- Offers a simple IR (Intermediate Representation)

**Why Cranelift?**
- Faster compilation than LLVM
- Simpler API than LLVM
- Good enough optimizations for Ryo v1.0
- Built-in JIT support for future REPL

### Code Generation Structure

```rust
pub struct Codegen {
    builder_context: FunctionBuilderContext,
    ctx: codegen::Context,
    module: ObjectModule,
    int_type: types::Type,
    data_ctx: DataDescription,
    string_data: HashMap<String, DataId>,
    triple: Triple,
}

struct FunctionContext<'a> {
    module: &'a mut ObjectModule,
    data_ctx: &'a mut DataDescription,
    string_data: &'a mut HashMap<String, DataId>,
    int_type: types::Type,
    triple: &'a Triple,
    locals: &'a HashMap<String, Variable>,
    func_ids: &'a HashMap<String, FuncId>,
}
```

**Codegen fields:**
- `builder_context`: Reusable context for FunctionBuilder
- `ctx`: Function compilation context (holds IR)
- `module`: Object file builder
- `int_type`: Target's pointer-sized integer (i32/i64)
- `data_ctx` / `string_data`: String literal storage and deduplication
- `triple`: Target triple for platform-specific codegen

**FunctionContext** bundles per-function compilation state, passed to `eval_expr` to avoid excessive parameter counts.

### Step 1: Initialize Codegen

```rust
pub fn new(target_triple: Triple) -> Result<Self, String> {
    // 1. Configure settings (enable PIC)
    let mut shared_builder = settings::builder();
    shared_builder.enable("is_pic")?;  // Position Independent Code

    // 2. Look up ISA for target
    let isa = isa::lookup(target_triple.clone())?
        .finish(shared_flags)?;

    // 3. Create ObjectModule for AOT compilation
    let obj_builder = ObjectBuilder::new(
        isa,
        "ryo_module",  // Module name
        cranelift_module::default_libcall_names(),
    )?;

    let module = ObjectModule::new(obj_builder);
    let int_type = module.target_config().pointer_type();  // i64 on 64-bit, i32 on 32-bit

    Ok(Self { builder_context, ctx, module, int_type })
}
```

**Key Decisions:**
- **PIC enabled:** Allows code to be loaded at any address (required for shared libraries, good practice)
- **ObjectModule:** AOT compilation (vs JITModule for REPL)
- **Int type:** Uses pointer size for integers (i64 on 64-bit systems)

### Step 2: Compile Program (Two-Pass)

The compiler uses a two-pass approach to support forward references between functions:

```rust
pub fn compile(&mut self, program: &HirProgram) -> Result<FuncId, String> {
    // Pass 1: Declare all functions (signatures only)
    let mut func_ids: HashMap<String, FuncId> = HashMap::new();
    for func in &program.functions {
        let sig = self.build_signature(func);
        let linkage = if func.name == "main" { Linkage::Export } else { Linkage::Local };
        let func_id = self.module.declare_function(&func.name, linkage, &sig)?;
        func_ids.insert(func.name.clone(), func_id);
    }

    // Pass 2: Compile function bodies
    for func in &program.functions {
        self.compile_function(func, &func_ids)?;
    }

    func_ids.get("main").copied().ok_or_else(|| "No main function defined".into())
}
```

### Step 3: Compile Function Bodies

Each function gets its own entry block, parameters are mapped to Cranelift `Variable`s, and statements are translated to IR:

```rust
fn compile_function(&mut self, func: &HirFunction, func_ids: &HashMap<String, FuncId>) -> Result<Option<String>, String> {
    // Create entry block and map parameters to variables
    let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
    let entry_block = builder.create_block();
    builder.append_block_params_for_function_params(entry_block);

    // Map each parameter to a Cranelift Variable
    let mut locals: HashMap<String, Variable> = HashMap::new();
    for (i, param) in func.params.iter().enumerate() {
        let var = builder.declare_var(int_type);
        builder.def_var(var, builder.block_params(entry_block)[i]);
        locals.insert(param.name.clone(), var);
    }

    // Translate each HirStmt
    for stmt in &func.body {
        match stmt {
            HirStmt::VarDecl { name, initializer, .. } => { /* eval + store */ }
            HirStmt::Return(Some(expr), _) => { /* eval + return */ }
            HirStmt::Return(None, _) => { /* return void */ }
            HirStmt::Expr(expr, _) => { /* eval for side effects */ }
        }
    }
    builder.finalize();
}
```

### Step 4: Evaluate Expressions

`eval_expr` takes an HIR expression and a `FunctionContext` bundle:

```rust
fn eval_expr(
    builder: &mut FunctionBuilder,
    expr: &HirExpr,
    ctx: &mut FunctionContext,
) -> Result<Value, String> {
    match &expr.kind {
        HirExprKind::IntLiteral(val) => Ok(builder.ins().iconst(ctx.int_type, *val as i64)),
        HirExprKind::StrLiteral(content) => { /* store string data, return pointer */ }
        HirExprKind::Var(name) => Ok(builder.use_var(*ctx.locals.get(name)?)),
        HirExprKind::UnaryOp(UnaryOp::Neg, sub) => { /* eval + ineg */ }
        HirExprKind::BinaryOp(lhs, op, rhs) => { /* eval both + iadd/isub/imul/sdiv */ }
        HirExprKind::Call(name, args) => { /* builtin or user function call */ }
    }
}
```

### Cranelift Instructions Generated

**For `x = 42`:**
```
block0:
    v0 = iconst.i64 42
    return v0
```

**For `result = 2 + 3 * 4`:**
```
block0:
    v0 = iconst.i64 2
    v1 = iconst.i64 3
    v2 = iconst.i64 4
    v3 = imul v1, v2      ; 3 * 4 = 12
    v4 = iadd v0, v3      ; 2 + 12 = 14
    return v4
```

**For `x = -42`:**
```
block0:
    v0 = iconst.i64 42
    v1 = ineg v0          ; -42
    return v1
```

### Step 5: Generate Object File

```rust
pub fn finish(self) -> Result<Vec<u8>, String> {
    self.module
        .finish()           // Finalize module
        .emit()             // Generate object file bytes
        .map_err(|e| format!("Failed to emit object file: {}", e))
}
```

The returned bytes are a valid object file that can be linked.

### Object File Format

- **Unix/Linux:** ELF format (`.o`)
- **macOS:** Mach-O format (`.o`)
- **Windows:** COFF format (`.obj`)

Contains:
- Machine code for `main` function
- Symbol table (exports `main`)
- Relocation information
- Metadata for linker

### IR Inspection

The `ryo ir` command displays the actual Cranelift IR generated for a program:

```bash
$ cargo run -- ir program.ryo
```

This is powered by `compile_and_dump_ir()`, which captures `format!("{}", self.ctx.func)` after `builder.finalize()`.

### Code Reference

- **Codegen struct:** `src/codegen.rs`
- **FunctionContext:** `src/codegen.rs`
- **Two-pass compile:** `src/codegen.rs::compile`
- **IR dump:** `src/codegen.rs::compile_and_dump_ir`
- **Expression eval:** `src/codegen.rs::eval_expr`
- **Print builtin:** `src/codegen.rs::generate_print_call`
- **CLI integration:** `src/pipeline.rs::compile_program`

---

## Phase 5: Linking

**Module:** `src/linker.rs`
**Function:** Combine object file with C runtime to create executable

### Why Linking is Needed

Object files contain compiled code but:
- Don't have an entry point (need C runtime `_start`)
- Missing standard library functions
- Relocations not resolved
- Not executable format

The linker:
1. Adds C runtime startup code
2. Resolves symbol references
3. Combines sections (.text, .data, .rodata)
4. Creates executable file format
5. Sets proper permissions

### Multi-Linker Fallback Strategy

Ryo tries linkers in this order:

```rust
fn link_executable(obj_file: &str, exe_file: &str) -> Result<(), CompilerError> {
    let linkers = vec!["zig cc", "clang", "cc"];

    for linker in linkers {
        let parts: Vec<&str> = linker.split_whitespace().collect();
        let output = if parts.len() > 1 {
            Command::new(parts[0])
                .arg(parts[1])
                .arg("-o").arg(exe_file)
                .arg(obj_file)
                .output()
        } else {
            Command::new(linker)
                .arg("-o").arg(exe_file)
                .arg(obj_file)
                .output()
        };

        match output {
            Ok(output) if output.status.success() => {
                println!("Linked with {}: {}", linker, exe_file);
                return Ok(());
            }
            _ => continue,  // Try next linker
        }
    }

    Err(CompilerError::LinkError("No linker found".into()))
}
```

**Linker Priority Rationale:**

1. **zig cc** (preferred)
   - Best cross-compilation support
   - Ships with libc for all targets
   - Handles platform differences automatically

2. **clang** (good alternative)
   - Modern, widely available
   - Good error messages
   - LLVM-based toolchain

3. **cc** (system default)
   - Always present on Unix systems
   - May be gcc, clang, or other
   - Fallback for maximum compatibility

### Linking Process

```
┌──────────────┐
│ first.o      │  Object file
│ (machine     │
│  code)       │
└──────┬───────┘
       │
       ├─> Linker  <─── C Runtime (_start, exit, etc.)
       │   (zig cc)
       │
       ▼
┌──────────────┐
│ first        │  Executable
│ (can run)    │
└──────────────┘
```

### What the Linker Does

1. **Adds Entry Point:**
   - C runtime's `_start` function
   - Calls `main()`
   - Handles program initialization

2. **Resolves Symbols:**
   - Our `main` function export
   - System calls (if any)

3. **Creates Executable:**
   - Proper file format (ELF/Mach-O/PE)
   - Executable permissions
   - Load address information

### Platform-Specific Behavior

**Unix/Linux:**
```bash
zig cc -o first first.o
# Creates: first (ELF executable, mode 755)
```

**macOS:**
```bash
zig cc -o first first.o
# Creates: first (Mach-O executable)
```

**Windows:**
```bash
zig cc -o first.exe first.obj
# Creates: first.exe (PE executable)
```

### Code Reference

- **Linking function:** `src/linker.rs::link_executable`
- **CLI integration:** `src/pipeline.rs::compile_program`

---

## Phase 6: Execution

**Module:** `src/pipeline.rs::execute_program`
**Function:** Run the compiled executable and capture exit code

### Execution Process

```rust
fn execute_program(exe_file: &str) -> Result<i32, CompilerError> {
    // Platform-specific path handling
    let exe_path = if cfg!(windows) {
        exe_file.to_string()
    } else {
        format!("./{}", exe_file)  // Unix needs ./ prefix
    };

    // Run executable
    let output = Command::new(&exe_path)
        .output()
        .map_err(|e| CompilerError::ExecutionError(e.to_string()))?;

    // Get exit code
    match output.status.code() {
        Some(code) => Ok(code),
        None => Err(CompilerError::ExecutionError("No exit code".into())),
    }
}
```

### Platform Differences

**Unix/macOS:**
- Need `./` prefix (security: explicit path required)
- Exit codes: 0-255 (8-bit unsigned)
- Negative values wrap: -1 → 255

**Windows:**
- Run directly by name
- Exit codes: Full 32-bit signed range
- No wrapping for negative values

### Exit Code Capture

The compiled program's `main` function returns zero, the OS captures this return value as the process exit code.

---

## Design Decisions

### AOT Over JIT (Currently)

**Decision:** Implement AOT-only compilation first

**Rationale:**
- Most users want real executables, not REPL (yet)
- AOT is simpler to implement and test
- JIT requires different Cranelift module type
- REPL can wait until language is more mature

**Future:** JIT will be added for REPL (Milestone 4+)

### Cranelift Over LLVM

**Decision:** Use Cranelift as code generation backend

**Rationale:**
- **Compilation Speed:** Cranelift is 10-50x faster than LLVM
- **Simplicity:** Simpler API, easier to learn
- **Size:** Smaller dependency, faster builds
- **JIT Support:** Built-in JIT for future REPL
- **Good Enough:** Optimizations sufficient for v1.0

**Tradeoffs:**
- Less mature than LLVM (but improving)
- Fewer optimization passes (acceptable for Ryo)
- Smaller ecosystem (not critical for our use case)

### Object Files in Current Directory

**Decision:** Write `.o`/`.obj` files to current working directory

**Rationale:**
- **Transparency:** Users see what compiler generates
- **Debugging:** Can inspect object files with `objdump`/`otool`
- **Simplicity:** No need to manage build directory yet

**Future:** Will move to `target/` directory like Cargo

### HIR as Intermediate Representation

**Decision:** Add an HIR layer between parsing and codegen (Zig-inspired)

**Rationale:**
- Separates analysis (type resolution, scope checking) from code generation
- Codegen only sees fully typed, validated IR — no AST imports needed
- Scope struct with parent pointers is ready for nested scopes (if/else, loops)
- Spans preserved from AST for future error reporting in later passes

**Comparison to Zig:** Ryo's HIR corresponds to Zig's AIR (post-analysis IR), and `lower.rs` corresponds to Zig's Sema pass.


## Completed Evolution

### Milestone 4: Functions & HIR (✅ Complete)

**Implemented:**
- Indent preprocessor (`src/indent.rs`): CPython-style `Indent`/`Dedent` token insertion
- HIR layer (`src/hir.rs`, `src/lower.rs`): post-analysis IR with full type resolution, scope checking, and implicit main wrapping — analogous to Zig's AIR (the lowering pass is analogous to Zig's Sema)
- Two-pass compilation: declare all functions first, then compile bodies (enables forward references)
- Cranelift `Variable` abstraction for local variables and function parameters
- User-defined function calls via `declare_func_in_func` + `call` instruction
- `return` statements with expression values
- Expression statements (bare function calls without assignment)
- Backward compatibility: flat programs without `fn main()` wrapped in implicit main
- Builtin function registry (`src/builtins.rs`) replacing scattered string matching
- `FunctionContext` struct in codegen bundling per-function state
- `ryo ir` command now displays actual Cranelift IR (not stub text)
- `main.rs` split into focused modules: `errors.rs`, `linker.rs`, `pipeline.rs`

**Pipeline change:** Source → Lexer → **Indent Preprocessor** → Parser → **Lower (HIR)** → Codegen → Object File → Linker → Executable

## Future Evolution

### Milestone 5+: More Expressions

**Additions:**
- Boolean operations
- Comparison operators
- String literals
- More numeric types (f64, i32, etc.)

**Codegen Changes:**
- New Cranelift instruction types
- Type-specific operations
- String memory management

### Future: Optimizations

**Additions:**
- Constant folding
- Dead code elimination
- Cranelift optimization passes
- Inlining

**Codegen Changes:**
- Enable Cranelift optimizations
- Optimization level flags (`-O0`, `-O2`, `-O3`)

### Future: JIT for REPL

**Additions:**
- Interactive Read-Eval-Print Loop
- Instant execution without linking

**Codegen Changes:**
- `JITModule` instead of `ObjectModule`
- In-memory execution
- State management between REPL lines

---

## Debugging the Pipeline

### View Tokens

```bash
cargo run -- lex program.ryo
```

### View AST

```bash
cargo run -- parse program.ryo
```

### View Cranelift IR

```bash
cargo run -- ir program.ryo
```

### Inspect Object File

**macOS:**
```bash
otool -tV program.o  # Disassemble
otool -h program.o   # Headers
nm program.o         # Symbols
```

**Linux:**
```bash
objdump -d program.o  # Disassemble
objdump -h program.o  # Headers
nm program.o          # Symbols
```

**Cross-platform:**
```bash
xxd program.o | less  # Hex dump
```

### Debug Linking

```bash
# See what linker is doing
zig cc -v -o program program.o
```

### Debug Execution

```bash
# Unix: Check exit code
./program
echo $?

# Unix: Run with debugger
lldb ./program

# Linux: Run with debugger
gdb ./program
```

---

## Performance Characteristics

### Compilation Speed

**Current (Milestone 3):**
- Lexing: <10ms
- Parsing: ~50ms
- Codegen: ~100-200ms
- Linking: ~200-400ms (bottleneck)
- **Total:** ~500ms for small programs

**Bottleneck:** Linking (external process, C runtime)

### Runtime Performance

- Native code (near-C performance)
- No garbage collection
- Integer arithmetic: optimal
- No allocations (yet)

### Binary Size

- Minimal programs: ~16KB
- Includes C runtime startup code
- Static linking (currently)

---

## Further Reading

- **Cranelift Documentation:** https://docs.rs/cranelift/
- **Cranelift Book:** https://cranelift.readthedocs.io/
- **LLVM Comparison:** https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/docs/compare-llvm.md
- **Object File Formats:**
  - ELF: https://en.wikipedia.org/wiki/Executable_and_Linkable_Format
  - Mach-O: https://en.wikipedia.org/wiki/Mach-O
  - PE/COFF: https://en.wikipedia.org/wiki/Portable_Executable

---

**Next Steps:**
- Read `src/codegen.rs` with this document as reference
- Experiment with modifying expression evaluation
- Try adding a new operator (%, etc.)
- Explore Cranelift IR with debugging tools
