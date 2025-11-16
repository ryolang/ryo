# Example 05: Package Visibility

## Concept

This example demonstrates package boundaries and the `package` keyword:
- Package-internal APIs shared across modules
- Public APIs for external users
- Package boundary defined by `ryo.toml`
- Real-world scenario: web framework with internal helpers

## Structure

```
05-package-visibility/
├── src/
│   ├── main.ryo          # Entry point (uses public API)
│   ├── router/           # Module "router"
│   │   └── handler.ryo
│   ├── middleware/       # Module "middleware"
│   │   └── auth.ryo
│   └── internal/         # Module "internal"
│       └── helpers.ryo   # Package-internal utilities
└── ryo.toml              # Package boundary
```

## Key Concepts

1. **Package Boundary**: Defined by `ryo.toml` - everything in `src/` is one package
2. **Public API**: `pub` functions are the external-facing API
3. **Package API**: `package` functions are shared utilities for internal use
4. **Clean Separation**: Public API is minimal, internal utilities stay hidden

## How to Run

```bash
ryo run src/main.ryo
```

## Expected Output

```
[INTERNAL] Validating request (package-internal helper)
[AUTH] Authenticating request...
[INTERNAL] Logging event (package-internal helper)
[ROUTER] Handling request: GET /api/users
Request handled successfully!
```

## What You'll Learn

- When to use `package` instead of `pub`
- How to design clean public APIs
- How to share utilities across modules without polluting the public API
- The difference between package boundaries and module boundaries
