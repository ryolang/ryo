# Example 06: Circular Dependencies

## Concept

This example demonstrates:
- What circular dependencies are
- Why they're forbidden between modules
- Common circular dependency patterns
- Solutions to break circular dependencies

## Structure

```
06-circular-deps/
├── src/
│   ├── main.ryo              # Entry point
│   ├── broken/               # ❌ Circular dependency (for demonstration)
│   │   ├── user.ryo          # Imports post
│   │   └── post.ryo          # Imports user (circular!)
│   └── fixed/                # ✓ Solutions to circular dependencies
│       ├── models/           # Solution 1: Common types
│       │   └── ids.ryo
│       ├── user/
│       │   └── user.ryo
│       └── post/
│           └── post.ryo
└── ryo.toml                  # Package definition
```

## Key Concepts

1. **Circular Dependencies**: When module A imports B, and B imports A
2. **Forbidden Between Modules**: Ryo forbids circular dependencies between modules
3. **Allowed Within Modules**: Files in the same module CAN reference each other
4. **Solutions**: Extract common types, use IDs instead of direct references, or restructure

## How to Run

```bash
# This example contains compile errors in the broken/ directory
# The fixed/ directory shows working solutions

ryo run src/main.ryo
```

## Expected Output

```
[FIXED] User created: Alice
[FIXED] Post created by user ID: 1
[FIXED] Post: "Hello World" by user ID 1
```

## What You'll Learn

- How to recognize circular dependencies
- Why they cause problems
- Multiple strategies to break circular dependencies
- When to use ID references vs direct object references
