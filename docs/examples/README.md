# Ryo Code Examples

This directory contains practical code examples demonstrating various Ryo language features and programming patterns.

## Examples Overview

### [hello_world.ryo](hello_world.ryo)
**Concepts:** Basic syntax, functions, string interpolation
- Simple "Hello World" program
- Function definitions and calls
- String formatting with f-strings

### [temperature_converter.ryo](temperature_converter.ryo)
**Concepts:** Enums, structs, pattern matching, methods
- Enum definitions with variants
- Struct definitions with fields
- Pattern matching with `match` expressions
- Function organization and code structure

### [error_handling.ryo](error_handling.ryo)
**Concepts:** Result types, error propagation, `?` operator
- `Result[T, E]` type for error handling
- Pattern matching for error cases
- The `?` operator for error propagation
- Handling empty collections and edge cases

### [ownership_borrowing.ryo](ownership_borrowing.ryo)
**Concepts:** Memory management, ownership, borrowing, references
- Ownership transfer and move semantics
- Immutable borrows (`&T`)
- Mutable borrows (`&mut T`)
- Scope-based lifetime management
- Safe memory access patterns

### [async_example.ryo](async_example.ryo)
**Concepts:** Asynchronous programming, concurrency, futures
- `async fn` definitions and `await` expressions
- Sequential vs. concurrent execution
- Error handling in async contexts
- Timeout and cancellation patterns

### [data_structures.ryo](data_structures.ryo)
**Concepts:** Collections, custom types, methods, iteration
- Built-in collections (`List`, `Map`)
- Custom struct and enum definitions
- Method implementations with `impl` blocks
- Complex data structures and nested types
- Iteration patterns and data processing

## Running the Examples

Once the Ryo compiler is available, you can run these examples using:

```bash
# Compile and run an example
ryo run hello_world.ryo

# Or compile to a binary
ryo build temperature_converter.ryo -o temp_converter
./temp_converter
```

## Learning Path

For beginners, we recommend following this order:

1. **hello_world.ryo** - Basic syntax and structure
2. **temperature_converter.ryo** - Data types and pattern matching  
3. **error_handling.ryo** - Error handling patterns
4. **ownership_borrowing.ryo** - Memory management concepts
5. **data_structures.ryo** - Working with collections and custom types
6. **async_example.ryo** - Concurrent programming

## Key Concepts Demonstrated

### Memory Safety
- Ownership and borrowing prevent memory leaks and use-after-free bugs
- Compile-time checking ensures memory safety without runtime overhead
- Clear rules about when data can be accessed and modified

### Error Handling
- Explicit error handling with `Result` types
- No hidden exceptions or null pointer errors
- Ergonomic error propagation with the `?` operator

### Performance
- Zero-cost abstractions - high-level code compiles to efficient machine code
- No garbage collector overhead
- Predictable memory usage and performance characteristics

### Developer Experience
- Python-like syntax that's easy to read and write
- Strong type system that catches errors at compile time
- Clear error messages that help you fix problems quickly

## Additional Resources

- [Getting Started Guide](../getting_started.md) - Comprehensive introduction
- [Language Specification](../specification.md) - Detailed language reference
- [Language Comparison](../language_comparison.md) - How Ryo compares to other languages
- [Implementation Roadmap](../implementation_roadmap.md) - Development progress and plans

## Contributing Examples

If you have ideas for additional examples that would help other developers learn Ryo, please consider contributing! Good examples should:

- Demonstrate specific language features clearly
- Include helpful comments explaining the concepts
- Be practical and realistic (not just toy examples)
- Follow Ryo best practices and idioms
- Include error handling where appropriate