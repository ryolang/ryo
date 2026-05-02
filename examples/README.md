# Ryo Examples

## Working Examples (Milestone 8b)

These compile and run with the current compiler.

| File | Features |
|------|----------|
| [hello.ryo](hello.ryo) | `print()` |
| [multiline.ryo](multiline.ryo) | Multiple statements |
| [calculator.ryo](calculator.ryo) | Arithmetic, operator precedence |
| [square.ryo](square.ryo) | Function definition, return types |
| [fizzbuzz.ryo](fizzbuzz.ryo) | `if`/`elif`/`else`, modulo |
| [classify.ryo](classify.ryo) | Conditionals, logical operators, `and`/`not` |
| [booleans.ryo](booleans.ryo) | `bool` type, equality operators |
| [floats_and_ordering.ryo](floats_and_ordering.ryo) | `float` type, comparison, integer division |

### Running

```bash
cargo run -- run examples/hello.ryo        # JIT
cargo run -- build examples/hello.ryo      # AOT → ./hello
```

## Future Examples

`future/` contains aspirational examples showing planned features that **do not compile yet**. They serve as design references for upcoming milestones.

Includes: error handling, ownership/borrowing, structs, enums, pattern matching, collections, modules, concurrency (tasks, channels, select), closures, and more.

## Resources

- [Getting Started](../docs/getting_started.md)
- [Language Specification](../docs/specification.md)
- [Implementation Roadmap](../docs/dev/implementation_roadmap.md)
