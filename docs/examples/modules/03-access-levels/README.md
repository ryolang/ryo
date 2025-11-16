# Example 03: Access Levels

## Concept

This example demonstrates Ryo's three access levels:
- **`pub`** - Public (accessible from any module)
- **`package`** - Package-internal (accessible within the same `ryo.toml` package)
- **No keyword** - Module-private (accessible only within the same module)

## Structure

```
03-access-levels/
├── src/
│   ├── main.ryo          # Entry point
│   ├── database/         # Module "database"
│   │   └── connection.ryo
│   └── config/           # Module "config"
│       └── loader.ryo
└── ryo.toml              # Package definition
```

## Key Concepts

1. **Public (`pub`)**: API for external users - any module can import and use
2. **Package (`package`)**: Internal API for this package - other modules in same `ryo.toml` can use
3. **Module-private (no keyword)**: Implementation details - only this module can use
4. **Progressive Disclosure**: Show minimal API publicly, share more within package, hide internals

## How to Run

```bash
ryo run src/main.ryo
```

## Expected Output

```
[CONFIG] Loading configuration...
[CONFIG] Using package-internal loader
[DATABASE] Connecting to database...
[DATABASE] Using config from package
Connected successfully!
```

## What You'll Learn

- When to use `pub` vs `package` vs module-private
- How `package` enables internal APIs
- How to design clean public APIs
- How to share utilities across modules without exposing them publicly
