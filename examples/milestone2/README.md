# Milestone 2 Examples

This directory contains example Ryo programs to test the parsing and error reporting capabilities of the compiler.

## Valid Examples

- **simple.ryo** - Basic variable declaration with type inference
- **typed.ryo** - Variable declarations with explicit type annotations
- **mutable.ryo** - Mutable variable declarations using `mut` keyword
- **expressions.ryo** - Variable initializers with complex expressions
- **complete.ryo** - Comprehensive example showcasing all supported features

### Running Valid Examples

```bash
# Tokenize the source
cargo run -- lex examples/milestone2/simple.ryo

# Parse and display AST
cargo run -- parse examples/milestone2/complete.ryo
```

## Error Examples

These files intentionally contain parse errors to demonstrate Ariadne error reporting:

- **error_missing_assign.ryo** - Missing assignment operator `=`
  - Expected: `x = 42` but got `x 42`

- **error_missing_initializer.ryo** - Missing expression after `=`
  - Expected: `x = <expression>` but got `x =`

- **error_unexpected_token.ryo** - Unexpected token in expression
  - The `@` symbol is not a valid token and causes parse error

- **error_invalid_syntax.ryo** - Completely invalid syntax
  - Multiple assignment operators with no valid pattern: `= = = x y z`

- **error_multiple.ryo** - Multiple errors in one file
  - Contains missing operator, unexpected token, and missing initializer
  - Tests Ariadne's ability to report multiple errors

- **error_type_annotation.ryo** - Malformed type annotation
  - Missing type name after colon: `x: = 42`

### Running Error Examples

```bash
# Try parsing an error example (will fail with Ariadne error message)
cargo run -- parse examples/milestone2/error_missing_assign.ryo

# Expected output: Pretty-formatted error with source highlighting
```

## Error Output Format

When a parse error occurs, Ariadne displays:
- Error code (e.g., [03])
- Clear message describing what was found vs. expected
- Source location with line:column numbers
- Pretty-formatted code context with error highlighting

Example:
```
Error: found '42' expected ':', or '='
   ╭─[cmdline:3:3]
   │
 3 │ x 42
   │   ─┬
   │    ╰──── found '42' expected ':', or '='
   ╰────
```

## Feature Coverage

These examples test:
- ✅ Variable declarations (`x = 42`)
- ✅ Type annotations (`x: int = 42`)
- ✅ Mutable variables (`mut x = 42`)
- ✅ Complex expressions (`x = 2 + 3 * 4`)
- ✅ Operator precedence (`x = (10 + 5) * 2`)
- ✅ Error reporting with Ariadne
- ✅ Multiple statements per file
