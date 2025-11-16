# Ryo Example Code Context

---

## Diagrams

Use mermaid for diagrams.

## Syntax Essentials

### ALWAYS Use Python-Style Syntax

```ryo
# Functions - colon and indentation (NO braces)
fn main():
    print("Hello, World!")

# Control flow - colon and indentation
if x > 0:
    print("positive")

# Loops - colon and indentation
for i in range(10):
    print(i)
```

### Indentation

- **Use TABS** (not spaces)
- One tab = one indentation level
- Mixing tabs/spaces is a compile error

### Module-Based Errors

```ryo
# File: math/errors.ryo
error DivisionByZero
error InvalidInput(str)

# File: main.ryo
import math

fn divide(a: int, b: int) -> math.DivisionByZero!int:
    if b == 0:
        return math.DivisionByZero
    return a / b
```

### F-Strings

```ryo
name = "Alice"
age = 30
print(f"Hello, {name}! You are {age} years old.")
print(f"Result: {2 + 2}")  # Expressions in braces
```

---

## Quick Syntax Reference

```ryo
# Variables
x = 42              # Immutable
mut y = 0           # Mutable
name: str = "Alice" # Explicit type

# Functions
fn add(a: int, b: int) -> int:
    return a + b

# Structs
struct Point:
    x: float
    y: float

p = Point(x=1.0, y=2.0)

# Enums
enum Color:
    Red
    Green
    Blue

c = Color.Red

# Error handling
result = divide(10, 0) catch |e|:
    print(f"Error: {e}")
    return

# Optional types
user: ?User = none
name = user?.name orelse "Unknown"

# Match expressions
match value:
    Color.Red: print("red")
    Color.Green: print("green")
    Color.Blue: print("blue")
```

---

## Example File Template

```ryo
# filename.ryo
# Brief description of what this example demonstrates

# File: errors.ryo (if using errors)
error ExampleError(reason: str)

# File: main.ryo
import errors

fn main():
    # Example code here
    print("Hello from Ryo!")
```

---

## Related Documentation

**For complete syntax:**
- See `docs/specification.md`

**For implementation status:**
- See `docs/implementation_roadmap.md`

---
