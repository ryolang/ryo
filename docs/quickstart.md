# Quick Start: Your First Ryo Program

This tutorial will get you compiling and running Ryo programs in under 5 minutes. By the end, you'll understand how Ryo transforms source code into native executables.

## Prerequisites

Before you begin, make sure you have:

1. **Rust toolchain** installed (1.70 or later)
   ```bash
   rustc --version  # Should show 1.70+
   ```

2. **A C linker** installed (one of: zig cc, clang, or cc)
   ```bash
   # Check if you have a linker:
   which zig    # Preferred (best cross-compilation)
   which clang  # Or this
   which cc     # Or this (system default)
   ```

   If none are installed:
   - **macOS**: `xcode-select --install`
   - **Linux**: `sudo apt install build-essential` (Ubuntu/Debian)
   - **Windows**: Install MSVC or MinGW

3. **Ryo compiler** built
   ```bash
   cd /path/to/ryox
   cargo build --release
   ```

## Your First Program: Hello, Exit Code!

### Step 1: Create a Simple Program

Create a file named `first.ryo` with this content:

```ryo
# My first Ryo program
x = 42
```

That's it! This program declares a variable `x` with the value 42.

### Step 2: Compile and Run

From the ryox directory, run:

```bash
cargo run -- run first.ryo
```

### Step 3: Understand the Output

You'll see output like this:

```
[Input Source]
# My first Ryo program
x = 42

[AST]
Program (24..30)
└── Statement [VarDecl] (24..30)
    VarDecl
      ├── name: x (24..25)
      └── initializer:
          Literal(Int(42)) (28..30)

[Codegen]
Generated object file: first.o
Linked with zig cc: first
[Result] => 42
```

**What just happened?**

1. **[Input Source]** - Shows your source code
2. **[AST]** - Abstract Syntax Tree representation of your program
3. **[Codegen]** - Compilation to native code:
   - Created `first.o` (object file)
   - Linked to create `first` executable
4. **[Result] => 42** - The exit code (explained below)

### Step 4: Verify the Executable

Two new files were created in your directory:

```bash
ls -la first*
# first.o    - Object file (intermediate)
# first      - Executable (final program)
```

You can run the executable directly:

```bash
./first        # Unix/macOS
first.exe      # Windows

echo $?        # Check exit code (Unix/macOS)
echo %ERRORLEVEL%  # Check exit code (Windows)
```

The exit code will be 42!

## Understanding Exit Codes

In Ryo (currently), **programs return the value of the last expression as an exit code**.

```ryo
x = 42    # Program returns 42
```

### Exit Code Conventions

- **0** = Success (by convention)
- **1-255** = Error or custom codes
- Other values wrap on some platforms

### Examples

```ryo
# Returns 0 (success)
result = 0
```

```ryo
# Returns 1 (generic error)
status = 1
```

```ryo
# Returns 14 (arithmetic result)
answer = 2 + 3 * 4
```

### Platform Note: Unix Exit Code Wrapping

On Unix/macOS, exit codes are 8-bit unsigned (0-255). Negative numbers wrap:

```ryo
x = -1     # Returns 255 on Unix (wraps around)
x = -42    # Returns 214 on Unix (256 - 42)
x = 256    # Returns 0 on Unix (wraps to 0)
```

## Try More Examples

Now try the official examples in `examples/milestone3/`:

### Simple Arithmetic

```bash
cargo run -- run examples/milestone3/arithmetic.ryo
# Result: 14  (because 2 + 3 * 4 = 2 + 12 = 14)
```

### Parentheses Change Precedence

```bash
cargo run -- run examples/milestone3/parenthesized.ryo
# Result: 30  (because (10 + 5) * 2 = 15 * 2 = 30)
```

### Multiple Statements

```bash
cargo run -- run examples/milestone3/multiple.ryo
# Result: 30  (returns last value: z = 30)
```

## What's Happening Under the Hood?

When you run `cargo run -- run first.ryo`, this pipeline executes:

```
Source Code  →  Lexer  →  Parser  →  Codegen  →  Linker  →  Executable
  (first.ryo)   (tokens)  (AST)      (first.o)  (first)     (runs)
```

### Phase 1: Lexing
Breaks code into tokens:
```
"x" (identifier), "=" (assign), "42" (integer)
```

### Phase 2: Parsing
Builds an Abstract Syntax Tree (AST):
```
VarDecl { name: "x", initializer: Int(42) }
```

### Phase 3: Code Generation
Translates AST to Cranelift IR, then to native machine code:
```
iconst v0, 42
return v0
```

### Phase 4: Linking
Combines object file with C runtime to create executable.

### Phase 5: Execution
Runs the executable and captures the exit code.

## Exploring Other Commands

### View Tokens Only

```bash
cargo run -- lex first.ryo
```

Shows the token stream (useful for debugging syntax).

### View AST Only

```bash
cargo run -- parse first.ryo
```

Shows the Abstract Syntax Tree without compiling.

### View IR Info

```bash
cargo run -- ir first.ryo
```

Shows information about Cranelift IR generation (confirms compilation succeeded).

## Common First-Time Issues

### "Failed to link with any available linker"

**Problem:** No linker found on your system.

**Solution:**
- macOS: `xcode-select --install`
- Linux: `sudo apt install build-essential`
- Or install Zig: https://ziglang.org/

### "No such file or directory"

**Problem:** File path is wrong or you're in the wrong directory.

**Solution:**
```bash
# Make sure you're in the ryox directory
pwd

# Use full path to file
cargo run -- run /full/path/to/first.ryo
```

### Exit Code Doesn't Match Expected Value

**Problem:** On Unix, negative numbers wrap to 0-255.

**Solution:** Design your program to return values in the 0-255 range, or understand that -1 becomes 255.

### Object Files Piling Up

**Problem:** `.o` files and executables accumulate in your directory.

**Solution:** Clean up manually for now:
```bash
rm *.o        # Remove object files
rm first      # Remove executable
```

Future versions will use a build directory like Cargo's `target/`.

## What Can You Do Right Now?

✅ **You can:**
- Declare variables with integers
- Use arithmetic operators: `+`, `-`, `*`, `/`
- Use parentheses for precedence
- Write programs with multiple statements
- Use type annotations: `x: int = 42`
- Declare mutable variables: `mut x = 42`

❌ **You cannot (yet):**
- Define functions (Milestone 4)
- Use strings (future milestone)
- Use if/else statements (Milestone 6)
- Use loops (Milestone 10)
- Handle errors (Milestone 7)

See `docs/implementation_roadmap.md` for what's coming next!

## Next Steps

1. **Explore Examples:**
   ```bash
   cd examples/milestone3
   ls -la
   cat README.md  # Read about each example
   ```

2. **Write Your Own Programs:**
   Try variations:
   ```ryo
   # Calculate something
   total = (100 + 50) * 2

   # Use multiple lines
   x = 10
   y = 20
   sum = x + y
   ```

3. **Learn More:**
   - [Getting Started Guide](getting_started.md) - Full language introduction
   - [Troubleshooting](troubleshooting.md) - Solutions to common problems
   - [Implementation Roadmap](implementation_roadmap.md) - See what's completed and what's next
   - [Examples README](../examples/milestone3/README.md) - Detailed example explanations

4. **Contribute:**
   - Read [CLAUDE.md](../CLAUDE.md) for project context
   - Check [docs/dev/](dev/) for architecture documentation
   - See [TODO.md](../TODO.md) for areas needing work

## Questions or Problems?

- Check [docs/troubleshooting.md](troubleshooting.md) for solutions
- Read [docs/implementation_roadmap.md](implementation_roadmap.md) for status
- File an issue on GitHub with:
  - Your platform (macOS/Linux/Windows)
  - Ryo version: `git rev-parse HEAD`
  - Full error output

---

**Congratulations!** You've successfully compiled and run your first Ryo program. Welcome to the Ryo community! 🎉
