# Ryo Example Code Context

---

## When writing specification

* Ryo is a general programming language, do not need system programming features
* Ryo Prioritizes DX
* Ryo is not yet implemented, this is the first specification, we don't need to take care about versions, all specification is future work.
* Ryo built in types are lowercase, user-defined types are PascalCase

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

# Loops - two for-loop forms and while
for i in range(10):       # counted (range built-in, exclusive end)
    print(i)
for item in items:        # iteration over collections
    print(item)
mut n = 0
while n < 10:             # condition-based loop
    n += 1
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

# Constrained types (v0.2)
type Port = int(1..65535)
type Percentage = float(0.0..100.0)
p = Port(8080)

# Distinct types (v0.2)
type Meters = distinct float
type Seconds = distinct float
d = Meters(100.0)

# Contracts (v0.2)
#[pre(x > 0)]
#[post(result >= x)]
fn double(x: int) -> int:
    return x * 2
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
