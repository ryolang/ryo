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
**Exit Code:** 42
**Demonstrates:** Literal integer compilation

```bash
cargo run -- run examples/milestone3/simple.ryo
# [Result] => 42
```

### exit_zero.ryo
**Purpose:** Example returning a zero exit code (success)
**Content:** `x = 0`
**Exit Code:** 0
**Demonstrates:** Success exit code

```bash
cargo run -- run examples/milestone3/exit_zero.ryo
# [Result] => 0
```

### arithmetic.ryo
**Purpose:** Arithmetic expression with multiple operators
**Content:** `result = 2 + 3 * 4`
**Exit Code:** 14
**Demonstrates:**
- Operator precedence (* before +)
- Binary operations compilation

```bash
cargo run -- run examples/milestone3/arithmetic.ryo
# [Result] => 14
```

Calculation: 2 + (3 * 4) = 2 + 12 = 14

### parenthesized.ryo
**Purpose:** Override default precedence with parentheses
**Content:** `result = (10 + 5) * 2`
**Exit Code:** 30
**Demonstrates:**
- Parenthesized expressions
- Precedence control

```bash
cargo run -- run examples/milestone3/parenthesized.ryo
# [Result] => 30
```

Calculation: (10 + 5) * 2 = 15 * 2 = 30

### multiple.ryo
**Purpose:** Program with multiple variable declarations
**Content:**
```ryo
x = 10
y = 20
z = 30
```
**Exit Code:** 30
**Demonstrates:**
- Multiple statements in one program
- Program returns the last statement's value

```bash
cargo run -- run examples/milestone3/multiple.ryo
# [Result] => 30
```

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
[Result] => 42
```

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
- Exit codes are in the range 0-255 (Unix) or 0-2147483647 (some systems)
- Negative numbers wrap to unsigned range (e.g., -1 becomes 255 on Unix)

## What's Not Implemented Yet

These examples do NOT demonstrate (coming in future milestones):
- ❌ String literals
- ❌ Functions
- ❌ Control flow (if, for, while)
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
