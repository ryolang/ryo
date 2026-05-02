# Example 01: Simple Module

## Concept

This example demonstrates the basics of Ryo's module system:
- Creating a simple module
- Importing and using module functions
- Public vs module-private visibility

## Structure

```
01-simple-module/
├── src/
│   ├── main.ryo          # Entry point
│   └── math/             # Module "math"
│       └── operations.ryo # Public functions
└── ryo.toml              # Package definition
```

## Key Concepts

1. **Directory = Module**: The `math/` directory defines the "math" module
2. **Public Functions**: Functions marked `pub` can be imported by other modules
3. **Module-Private Functions**: Functions without `pub` are only visible within the module
4. **Import Syntax**: Use `import module_name` to access public items

## How to Run

```bash
ryo run src/main.ryo
```

## Expected Output

```
Addition: 5 + 3 = 8
Subtraction: 10 - 4 = 6
Multiplication: 7 * 6 = 42
```

## What You'll Learn

- How to organize code into modules
- When to use `pub` keyword
- How to import and use module functions
- The difference between public and module-private visibility
