# Milestone 3 Examples

This directory contains example Ryo programs that demonstrate the code generation and execution capabilities of Milestone 3.

## Overview

Milestone 3 completes the compilation pipeline, enabling:
- **Parsing** Ryo source code to AST
- **Generating** Cranelift IR from the AST
- **Compiling** IR to native object files
- **Linking** object files to create executables
- **Executing** compiled programs and returning exit codes

## Example Files

### simple.ryo
**Purpose:** Basic example showing the simplest possible program
**Content:** `x = 42`
**Exit Code:** 0 (success)
**Demonstrates:** Literal integer compilation

```bash
cargo run -- run examples/milestone3/simple.ryo
# [Result] => 0
```

**Note:** The expression evaluates to 42, but all Milestone 3 programs exit with 0 (success). Explicit exit codes will be added in Milestone 4.

### exit_zero.ryo
**Purpose:** Example demonstrating successful program execution
**Content:** `x = 0`
**Exit Code:** 0 (success)
**Demonstrates:** Success exit code (all programs exit with 0)

```bash
cargo run -- run examples/milestone3/exit_zero.ryo
# [Result] => 0
```

### arithmetic.ryo
**Purpose:** Arithmetic expression with multiple operators
**Content:** `result = 2 + 3 * 4`
**Exit Code:** 0 (success)
**Demonstrates:**
- Operator precedence (* before +)
- Binary operations compilation
- Expression evaluation (result is 14, but exit code is 0)

```bash
cargo run -- run examples/milestone3/arithmetic.ryo
# [Result] => 0
```

**Calculation:** 2 + (3 * 4) = 2 + 12 = 14 (computed correctly, exits with 0)

### parenthesized.ryo
**Purpose:** Override default precedence with parentheses
**Content:** `result = (10 + 5) * 2`
**Exit Code:** 0 (success)
**Demonstrates:**
- Parenthesized expressions
- Precedence control
- Expression evaluation (result is 30, but exit code is 0)

```bash
cargo run -- run examples/milestone3/parenthesized.ryo
# [Result] => 0
```

**Calculation:** (10 + 5) * 2 = 15 * 2 = 30 (computed correctly, exits with 0)

### multiple.ryo
**Purpose:** Program with multiple variable declarations
**Content:**
```ryo
x = 10
y = 20
z = 30
```
**Exit Code:** 0 (success)
**Demonstrates:**
- Multiple statements in one program
- All statements evaluated, program exits with 0

```bash
cargo run -- run examples/milestone3/multiple.ryo
# [Result] => 0
```

### exit_code_future.ryo
**Purpose:** Documentation of planned future exit code syntax
**Content:** Comments showing Milestone 4+ syntax, with working Milestone 3 code
**Exit Code:** 0 (success)
**Demonstrates:**
- Future syntax for explicit exit codes (planned for Milestone 4)
- How current behavior differs from future plans
- Integration with error handling (Milestone 7+)

```bash
cargo run -- run examples/milestone3/exit_code_future.ryo
# [Result] => 0
```

**Note:** This file is primarily documentation. Read the comments to see how explicit exit codes will work in future milestones.

## Compilation Pipeline

Each example goes through the following pipeline:

1. **Lexical Analysis** (Lexer) - Tokenization
2. **Syntax Analysis** (Parser) - AST generation
3. **Code Generation** (Codegen) - Cranelift IR → Object file
4. **Linking** - Object file → Native executable
5. **Execution** - Run executable and capture exit code

## Running Examples

### Run a Program
```bash
cargo run -- run examples/milestone3/simple.ryo
```

**Output:**
```
[Input Source]
x = 42

[AST]
Program (0..6)
└── Statement [VarDecl] (0..6)
    VarDecl
      ├── name: x (0..1)
      └── initializer:
          Literal(Int(42)) (4..6)

[Codegen]
Generated object file: simple.o
Linked with zig cc: simple
[Result] => 0
```

**Note:** The expression `x = 42` evaluates to 42 (stored in Cranelift SSA register), but the program exits with 0 (success convention).

### View AST Only
```bash
cargo run -- parse examples/milestone3/simple.ryo
```

### View Cranelift IR Info
```bash
cargo run -- ir examples/milestone3/simple.ryo
```

## Features Tested

### Data Types
- ✅ Integer literals

### Operators
- ✅ Addition (+)
- ✅ Subtraction (-)
- ✅ Multiplication (*)
- ✅ Division (/)
- ✅ Unary negation (-)

### Syntax
- ✅ Variable declarations
- ✅ Type annotations (optional)
- ✅ Mutable variables (mut keyword)
- ✅ Parenthesized expressions
- ✅ Multiple statements

### Code Generation
- ✅ Literal compilation
- ✅ Expression evaluation
- ✅ Operator precedence
- ✅ Object file generation
- ✅ Executable linking
- ✅ Exit code return

## Notes

- Generated object files (`.o` or `.obj`) are written to the current directory
- Generated executables are also in the current directory
- **All programs exit with code 0 (success)** - this is the Unix convention for successful program execution
- Explicit exit codes will be added in Milestone 4 via return statements: `fn main() -> int: return 1`
- Expressions are evaluated correctly (e.g., `2 + 3 * 4 = 14`) but the result doesn't affect the exit code

## What's Not Implemented Yet

These examples do NOT demonstrate (coming in future milestones):
- ❌ String literals
- ❌ Functions
- ❌ Control flow (if, for)
- ❌ Pattern matching
- ❌ Error handling
- ❌ Structs and enums
- ❌ Traits
- ❌ Async/await
- ❌ Generic types

## Testing

All examples are tested automatically:

```bash
# Run all tests (including codegen tests)
cargo test

# Run only codegen integration tests
cargo test test_run
```

Current test count: 15 codegen-specific tests + 32 parser tests + 5 other integration tests = 52 total tests
