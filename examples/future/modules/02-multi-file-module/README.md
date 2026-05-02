# Example 02: Multi-File Module

## Concept

This example demonstrates how multiple `.ryo` files in the same directory form a single module:
- Multiple files sharing the same namespace
- Files can call each other's module-private functions
- Public API can be spread across multiple files

## Structure

```
02-multi-file-module/
├── src/
│   ├── main.ryo          # Entry point
│   └── server/           # Module "server" (multiple files)
│       ├── http.ryo      # HTTP server logic
│       ├── routes.ryo    # Route handlers
│       └── middleware.ryo # Request middleware
└── ryo.toml              # Package definition
```

## Key Concepts

1. **Multi-File Modules**: All `.ryo` files in `server/` are part of the "server" module
2. **Shared Namespace**: Functions from `http.ryo`, `routes.ryo`, and `middleware.ryo` all belong to the `server` module
3. **Internal Communication**: Files in the same module can call each other's module-private functions
4. **Unified Import**: Other modules import `server` once and get access to all public items

## How to Run

```bash
ryo run src/main.ryo
```

## Expected Output

```
[SERVER] Starting HTTP server...
[SERVER] Binding to port 8080
[MIDDLEWARE] Logging request: GET /api/users
[ROUTES] Handling GET /api/users
[SERVER] Server running!
```

## What You'll Learn

- How to split a module across multiple files
- How files in the same module can access each other's private functions
- How to organize related functionality
- The difference between module boundaries and file boundaries
