# Ryo Language Design Issues & Recommendations

This document identifies critical design inconsistencies in the Ryo language specification that must be resolved before implementation.

## Critical Issues Requiring Immediate Resolution

### 1. Tuple Syntax Ambiguity 🔴

**Problem**: Identical syntax `(...)` used for multiple contexts causes parser ambiguity.

**Examples**:
```ryo
# Type annotation
fn foo() -> (int, str):
    ...

# Value literal
x = (42, "hello")

# Function parameter grouping
fn bar(param: (int, str)):
    ...
bar((42, "hello"))  # One tuple arg or two separate args?

# Expression grouping
result = (a + b) * c

# Empty tuple vs unit
empty = ()
fn unit_func() -> ():
    ...
```

**Recommendation**:
- Use different syntax for unit type: `fn unit_func() -> unit:`
- Or use explicit tuple constructors: `tuple(42, "hello")`
- Or require trailing comma for single-element tuples: `(42,)`

### 2. Implicit Borrow vs Move Inconsistency 🔴

**Problem**: Function parameters default to borrowing while assignments default to moving.

**Examples**:
```ryo
# Assignment: MOVES
new_var = old_var  # old_var invalid

# Function call: BORROWS
fn process(data: SomeType):
    ...
process(my_data)  # my_data still valid

# This creates confusion:
data = create_data()
process(data)      # Borrows - OK
other = data       # Moves - data invalid!
process(data)      # ERROR: use of moved value
```

**Recommendation**:
- **Option A**: Make everything explicit - remove implicit borrowing
  ```ryo
  fn process(data: &SomeType):  # Explicit borrow
      ...
  fn consume(data: SomeType):   # Explicit move
      ...
  ```
- **Option B**: Make assignment borrowing more explicit
  ```ryo
  other = move data  # Explicit move
  other = data       # Implicit borrow (like function params)
  ```

### 3. Keywords vs Types Conflict 🔴

**Problem**: `Result`, `Optional`, `Ok`, `Err`, `Some` are listed as keywords but used as types.

**Examples**:
```ryo
# Cannot create identifiers with these names
struct MyResult: ...  # ERROR: 'Result' is keyword

# But used as types everywhere
fn parse() -> ParseError!int:
    ...
```

**Recommendation**: 
- Remove `Result`, `Optional`, `Ok`, `Err`, `Some` from keywords list
- Treat them as built-in types resolved by type checker
- Allow users to shadow these names if needed

### 4. Generic Syntax Undefined 🔴

**Problem**: Generics used throughout spec (`List[T]`, `Map[K,V]`) but syntax never defined.

**Examples**:
```ryo
# Used in spec but undefined:
List[T], Map[K,V]

# How to define?
struct MyStruct[T]: ...?     # Unclear syntax
fn generic_fn[T](...): ...?  # Unclear syntax
```

**Recommendation**: Define complete generic syntax:
```ryo
# Generic struct
struct Container[T]:
    value: T

# Generic function
fn identity[T](x: T) -> T:
    return x

# Generic enum
error ApiResponse[T]:
    Success(T)
    Failure(str)

# With trait bounds (future)
fn sort[T: Comparable](list: List[T]):
    ...
```

### 5. Method Self Parameter Confusion 🔴

**Problem**: `mut self` meaning unclear - mutable borrow or owned value?

**Examples**:
```ryo
impl Counter:
    fn increment(mut self):  # Borrow or move?
        self.count += 1

    fn drop(mut self): ...   # Drop must take ownership

# Usage unclear:
counter.increment()  # Does counter still exist?
```

**Recommendation**: Use Rust-like explicit syntax:
```ryo
impl Counter:
    fn increment(&mut self):     # Mutable borrow - clear
        self.count += 1

    fn consume(self):            # Take ownership - clear
        # ...

    fn drop(self): ...           # Drop takes ownership
```

### 6. Error Handling with Automatic Composition ✅ RESOLVED

**Previously**: Developers had to create wrapper error types when composing functions with different error types, creating boilerplate (the "wrapper problem").

**Solution Implemented**: Comprehensive error union system inspired by Zig with improvements from Swift and Rust.

#### Key Features:

1. **Single-Variant Errors Only** (simplified design):
   ```ryo
   error Timeout                          # Unit error
   error NotFound(str)                    # Message-only error
   error HttpError(status: int, message: str)  # Structured error
   ```

2. **Module-Based Grouping** (organize related errors):
   ```ryo
   module math:
       error DivisionByZero
       error InvalidInput(message: str)
       error OverflowError
   ```

3. **Error Union Types** (automatic composition from `try`):
   ```ryo
   # Explicit union - manually specified
   fn process() -> (FileError | ParseError)!Data:
       file = try read_file(path)
       data = try parse(file)
       return data

   # Inferred union - compiler automatically infers from try expressions
   fn process() -> !Data:
       file = try read_file(path)      # FileError
       data = try parse(file)          # ParseError
       return data
   # Compiler infers: (FileError | ParseError)!Data
   ```

4. **Error Trait** (automatic message generation):
   ```ryo
   # All errors implement Error trait with .message() method
   result = fetch_resource(url) catch |e|:
       print(e.message())  # Automatic or custom message
       return
   ```

5. **Error Propagation** (no wrapper boilerplate):
   ```ryo
   # Before (wrapper boilerplate):
   error AppError:
       Http(HttpError)
       Io(IoError)

   # After (automatic composition):
   fn fetch_and_save() -> !():
       data = try http.get("...")     # Different errors
       try files.write(data)           # Automatically composed
       return
   # Compiler infers: (HttpError | IoError)!()
   ```

6. **Pattern Matching (Exhaustive by Default)**:
   - **Single error types**: Exhaustive matching required (all variants must be handled)
   - **Error unions**: Exhaustive matching required (compiler enforces handling all types in union)
   - **Catch-all pattern**: Use `_` when you want generic handling for unspecified errors

#### Examples:

**Single error type (exhaustive):**
```ryo
result = divide(10.0, 0.0) catch |e|:
    match e:
        math.DivisionByZero:
            print("Cannot divide by zero")
    return
# MUST handle the single error type
```

**Error union (exhaustive matching):**
```ryo
result = complex_operation() catch |e|:
    match e:
        math.DivisionByZero:
            print("Cannot divide by zero")
        math.InvalidInput(msg):
            print(f"Invalid: {msg}")
        io.FileNotFound(path):
            print(f"File not found: {path}")
        parse.InvalidJson(reason):
            print(f"Parse error: {reason}")
    return
# MUST handle all error types in the union (unless using catch-all)
```

**Using catch-all for generic handling:**
```ryo
result = complex_operation() catch |e|:
    match e:
        math.DivisionByZero:
            print("Math error!")
        _:  # Explicit catch-all: handle all other errors the same way
            log_error(e.message())
            print("Generic error occurred")
    return
```

#### Benefits:
- ✅ **Maximum simplicity**: Single-variant only (one syntax to learn)
- ✅ **Zero boilerplate**: No wrapper types, no multi-variant boilerplate
- ✅ **Type safety**: All errors explicitly tracked by type system
- ✅ **Composability**: Functions naturally compose without explicit error mapping
- ✅ **Safety**: Exhaustive matching by default ensures all error paths are handled
- ✅ **Ergonomic**: `try` keyword for propagation, `catch` for handling
- ✅ **Explicit handling**: `try`/`catch` makes all error paths visible in code
- ✅ **Zig-inspired**: Simple error sets (like Zig) with payload support (unlike Zig)

#### Key Safety Features:
- **No direct unwrap**: Error values cannot be used without `try`/`catch` or propagation
- **Exhaustive matching**: Compiler requires handling all error types in a union (or explicit catch-all)
- **Automatic inference**: Compiler tracks error types and infers unions automatically
- **Module namespacing**: Related errors organized in modules (not multi-variant syntax)
- **Message support**: All errors automatically implement `.message()` method
- **From trait**: Allows explicit cross-layer error conversion when needed

This design achieves **maximum simplicity** (single-variant errors only with module grouping) while maintaining **strong safety guarantees** (exhaustive matching by default). It eliminates the "wrapper problem" without requiring multi-variant syntax, inspired by Zig's philosophy of simplicity.

## Moderate Issues

### 7. Async Main Function Undefined ⚠️

**Problem**: Examples show `async fn main()` but spec only mentions sync main.

**Recommendation**: Define async main semantics:
```ryo
# Option A: Explicit runtime
fn main():
    async_runtime.run(async_main())

async fn async_main() -> AppError!():
    ...

# Option B: Compiler magic
async fn main() -> AppError!():  # Compiler starts runtime
    # ...
```

### 8. Channel Operators Listed as Current but in Future ⚠️

**Problem**: `<-` operator listed in current spec but channels are in proposals.md.

**Recommendation**: Remove `<-` from current operator list, add back when CSP is implemented.

### 9. Static Dispatch Only Limitation ⚠️

**Problem**: No dynamic dispatch limits Python-like polymorphism patterns.

**Recommendation**: Consider trait objects for future:
```ryo
# Future syntax for dynamic dispatch
trait Drawable:
    fn draw(self)

fn process_shapes(shapes: List[&dyn Drawable]):
    for shape in shapes:
        shape.draw()  # Dynamic dispatch
```

### 10. Array vs Slice Type Ambiguity ⚠️

**Problem**: `[T]` syntax meaning unclear - array or slice?

**Recommendation**: Define clear syntax:
```ryo
[T; N]    # Fixed-size array of N elements
[T]       # Dynamic array (List[T])  
&[T]      # Slice (borrowed view)
```

## Resolution Status

**✅ Resolved:**
3. ~~Remove type names from keywords~~ - Removed `Result`, `Optional`, `Ok`, `Err`, `Some` from keywords
4. ~~Define generic syntax completely~~ - Moved detailed syntax to proposals.md
5. ~~Clarify method self parameters~~ - Applied explicit `&self`, `&mut self`, `self` syntax
6. ~~Design error trait system~~ - Implemented `error` keyword, `try`/`catch` operators, `From` trait
7. ~~Define async main function~~ - Specified sync main only with explicit runtime calls
8. ~~Resolve operator inconsistencies~~ - Removed channel operators from current spec
9. ~~Consider dynamic dispatch options~~ - Added future trait objects plan

**🔄 Deferred for Review:**
1. Fix tuple syntax ambiguity - Keep in file for later review
2. Resolve borrow/move inconsistency - Keep in file for later review
10. Clarify array/slice syntax - Keep in file for later review

## Next Steps (Remaining Issues)

**Before Implementation Begins:**
1. Review tuple syntax ambiguity (kept for future consideration)
2. Review borrow/move inconsistency (kept for future consideration)
6. Review error trait system design (kept for future consideration)
10. Review array/slice syntax (kept for future consideration)

## Next Steps

1. Create detailed syntax specification resolving these ambiguities
2. Update all documentation to use consistent syntax
3. Create grammar specification (EBNF) to catch remaining conflicts
4. Implement parser with clear error messages for edge cases