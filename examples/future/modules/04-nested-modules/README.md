# Example 04: Nested Modules

## Concept

This example demonstrates hierarchical module organization:
- Parent modules containing child submodules
- Importing nested modules
- Parent modules with both files AND subdirectories
- Clear hierarchical structure

## Structure

```
04-nested-modules/
├── src/
│   ├── main.ryo              # Entry point
│   └── utils/                # Parent module "utils"
│       ├── core.ryo          # Part of "utils" module
│       ├── strings/          # Child module "utils.strings"
│       │   └── formatting.ryo
│       └── math/             # Child module "utils.math"
│           ├── basic.ryo     # Part of "utils.math" module
│           └── advanced.ryo  # Part of "utils.math" module
└── ryo.toml                  # Package definition
```

## Key Concepts

1. **Hierarchical Structure**: Modules can contain submodules
2. **Parent + Child**: `utils/` has both files (`core.ryo`) and subdirectories (`strings/`, `math/`)
3. **Import Paths**: Use dot notation: `utils.math`, `utils.strings`
4. **Module Boundaries**: Each directory is a separate module with its own private scope

## How to Run

```bash
ryo run src/main.ryo
```

## Expected Output

```
[UTILS] Core utility initialized
[STRINGS] Formatting text: "Hello, World!"
[MATH.BASIC] Adding 5 + 3 = 8
[MATH.ADVANCED] Computing power: 2^8 = 256
```

## What You'll Learn

- How to organize code hierarchically
- How to import nested modules
- When to use parent modules vs flat structure
- How parent and child modules interact
