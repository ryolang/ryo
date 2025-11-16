# Compilation Pipeline Architecture

**Target Audience:** Contributors, maintainers, compiler developers

This document provides a detailed technical explanation of how Ryo compiles source code into native executables. Understanding this pipeline is essential for contributing to the compiler.

## Table of Contents

1. [Overview](#overview)
2. [Phase 1: Lexical Analysis](#phase-1-lexical-analysis)
3. [Phase 2: Syntax Analysis](#phase-2-syntax-analysis)
4. [Phase 3: Code Generation](#phase-3-code-generation)
5. [Phase 4: Linking](#phase-4-linking)
6. [Phase 5: Execution](#phase-5-execution)
7. [Design Decisions](#design-decisions)
8. [Future Evolution](#future-evolution)

---

## Overview

The Ryo compilation pipeline transforms source code through five distinct phases:

```
┌─────────────┐    ┌────────┐    ┌────────┐    ┌─────────┐    ┌────────┐    ┌────────────┐
│ Source Code │ -> │ Lexer  │ -> │ Parser │ -> │ Codegen │ -> │ Linker │ -> │ Executable │
│  (.ryo)     │    │(Tokens)│    │ (AST)  │    │  (.o)   │    │        │    │  (native)  │
└─────────────┘    └────────┘    └────────┘    └─────────┘    └────────┘    └────────────┘
```

### High-Level Flow

1. **Source** → **Tokens** (`src/lexer.rs`)
2. **Tokens** → **AST** (`src/parser.rs`)
3. **AST** → **Cranelift IR** → **Object File** (`src/codegen.rs`)
4. **Object File** + **C Runtime** → **Executable** (`src/main.rs::link_executable`)
5. **Execute** and capture exit code (`src/main.rs::execute_program`)

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

- **Token definition:** `src/lexer.rs:8-45`
- **Tests:** `src/lexer.rs:114-400`
- **CLI integration:** `src/main.rs:119-133`

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
2. **Statement** = VarDecl
3. **VarDecl** = `[mut] ident [: type] = expression`
4. **Expression** = Binary | Unary | Literal | `( Expression )`

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
    BinaryOp(Box<Expression>, BinaryOperator, Box<Expression>),
    UnaryOp(UnaryOperator, Box<Expression>),
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

- **Parser definition:** `src/parser.rs:11-160`
- **AST types:** `src/ast.rs:1-243`
- **Tests:** `src/parser.rs:161-400`
- **Error display:** `src/main.rs:175-192`

---

## Phase 3: Code Generation

**Module:** `src/codegen.rs`
**Library:** [Cranelift](https://docs.rs/cranelift/) v0.125
**Function:** Translate AST into native object files

This is the most complex phase. Let's break it down in detail.

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
    int_type: Type,
}
```

**Fields:**
- `builder_context`: Reusable context for FunctionBuilder
- `ctx`: Function compilation context (holds IR)
- `module`: Object file builder
- `int_type`: Target's pointer-sized integer (i32/i64)

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

### Step 2: Compile Program

```rust
pub fn compile(&mut self, program: Program) -> Result<FuncId, String> {
    // 1. Create function signature: () -> i64
    let sig = {
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(self.int_type));  // Return exit code
        sig
    };

    // 2. Declare 'main' function with export linkage
    let func_id = self.module.declare_function("main", Linkage::Export, &sig)?;

    // 3. Set function signature
    self.ctx.func.signature = sig;

    // 4. Generate IR for function body
    {
        let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
        let int_type = self.int_type;

        Self::translate(&mut builder, program, int_type)?;

        builder.finalize();  // Validate and finalize function
    }

    // 5. Define function in module
    self.module.define_function(func_id, &mut self.ctx)?;

    // 6. Clear context for next function
    self.ctx.clear();

    Ok(func_id)
}
```

### Step 3: Translate AST to IR

```rust
fn translate(builder: &mut FunctionBuilder, program: Program, int_type: Type) -> Result<(), String> {
    // 1. Create entry block
    let entry_block = builder.create_block();
    builder.switch_to_block(entry_block);
    builder.seal_block(entry_block);  // No more predecessors

    // 2. Handle empty program
    if program.statements.is_empty() {
        let zero = builder.ins().iconst(int_type, 0);
        builder.ins().return_(&[zero]);
        return Ok(());
    }

    // 3. Evaluate each statement, keep last value
    let mut result_val = None;
    for stmt in program.statements {
        let val = Self::eval_expr(builder, &stmt.kind.as_var_decl().initializer, int_type)?;
        result_val = Some(val);
    }

    // 4. Return last value as exit code
    let return_val = result_val.unwrap();
    builder.ins().return_(&[return_val]);

    Ok(())
}
```

### Step 4: Evaluate Expressions

```rust
fn eval_expr(builder: &mut FunctionBuilder, expr: &Expression, int_type: Type) -> Result<Value, String> {
    match &expr.kind {
        // Integer literal → iconst instruction
        ExprKind::Literal(Literal::Int(val)) => {
            Ok(builder.ins().iconst(int_type, *val as i64))
        }

        // Unary negation → ineg instruction
        ExprKind::UnaryOp(UnaryOperator::Neg, sub_expr) => {
            let sub_val = Self::eval_expr(builder, sub_expr, int_type)?;
            Ok(builder.ins().ineg(sub_val))
        }

        // Binary operations
        ExprKind::BinaryOp(lhs, op, rhs) => {
            let lhs_val = Self::eval_expr(builder, lhs, int_type)?;
            let rhs_val = Self::eval_expr(builder, rhs, int_type)?;

            let result = match op {
                BinaryOperator::Add => builder.ins().iadd(lhs_val, rhs_val),
                BinaryOperator::Sub => builder.ins().isub(lhs_val, rhs_val),
                BinaryOperator::Mul => builder.ins().imul(lhs_val, rhs_val),
                BinaryOperator::Div => builder.ins().sdiv(lhs_val, rhs_val),  // Signed division
            };

            Ok(result)
        }
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

### Code Reference

- **Codegen struct:** `src/codegen.rs:11-16`
- **Initialization:** `src/codegen.rs:19-50`
- **Compilation:** `src/codegen.rs:52-84`
- **Translation:** `src/codegen.rs:86-114`
- **Expression eval:** `src/codegen.rs:116-140`
- **CLI integration:** `src/main.rs:215-240`

---

## Phase 4: Linking

**Module:** `src/main.rs::link_executable`
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

- **Linking function:** `src/main.rs:242-281`
- **CLI integration:** `src/main.rs:215-240`

---

## Phase 5: Execution

**Module:** `src/main.rs::execute_program`
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

### No Variable Storage (Yet)

**Decision:** Variables are evaluated but not stored in memory

**Rationale:**
- Milestone 3 goal: Demonstrate compilation, not variables
- Simplifies codegen significantly
- Variables don't need to interact yet
- Will be addressed with functions (Milestone 4)

**Current Limitation:**
```ryo
x = 10
y = 20
z = x + y  # Error: Can't reference x or y yet
```

Only the last expression value matters.


## Future Evolution

### Milestone 4: Functions

**Additions:**
- Function definitions and calls
- Local variables (stack allocation)
- Parameters and return values
- Multiple functions per program

**Codegen Changes:**
- Multiple function compilation
- Call instruction generation
- Stack frame management
- Proper `return` statements

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

### View IR Info

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
