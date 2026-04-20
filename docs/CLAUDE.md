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

## Documentation Conventions

**Three-layer doc split:** Spec says *what*, dev docs say *how*, roadmap says *when* and owns the pointers between layers.

```
specification.md          (what the language does)
         ↑
         │ "implements Section X.Y"
         │
implementation_roadmap.md (when each what gets built — owns pointers to dev docs)
         ↓
docs/dev/*.md             (how the compiler/stdlib delivers — links back to spec sections)
```

**Spec purity:** specification.md contains no implementation details and no path references to docs/dev/ files. Test: "Could this sentence remain true regardless of how the compiler implements it?" If yes → spec. If no → dev doc.

**Roadmap owns pointers:** When a new dev doc is written, link it from the roadmap, not from the spec.

**Spec is source of truth:** When index.md, language_comparison.md, or quickstart.md contradict the spec, update the companion, never the spec.

**Preserve voice; minimal diffs:** Restructuring sections is out of scope. Preserve existing voice and structure. For multi-file changes, show diffs before applying.

**Audit first:** For multi-file documentation changes, grep the tree first and produce an audit.

**Scratch vs committed:** Working artifacts go in docs/analysis/ and are not committed.

---

## Gotchas

**Task closures move implicitly.** In Section 9, closures passed to `task.run`/`task.scope`/`task.spawn_detached` capture by move automatically. Writing `move` on them is accepted but redundant. Elsewhere, `move` is always explicit.

**`&mut` and `move` are the same cost.** Under NRVO, both compile to a pointer pass. See Section 5.2.1.

**Section 5.1 and Rule 2 are both correct.** Section 5.1's "moved by default" applies to assignment and return. Rule 2 says parameters default to immutable borrow. These cover different cases. Do not "fix" the apparent contradiction.

**Spec sections never link to `docs/dev/` files by path.** If you're about to write "see `docs/dev/X.md`" in the spec, stop — the reference belongs in the roadmap.

**Roadmap milestone dependencies are real.** M20 (`&mut`) must precede M22 (collections) and M23 (Drop). Closure capture analysis must follow M15. Reorderings that ignore these recreate known paradoxes.

---

## Related Documentation

**For complete syntax:**
- See `docs/specification.md`

**For implementation status:**
- See `docs/dev/implementation_roadmap.md`
