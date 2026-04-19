# Quick Start: Your First Ryo Program

This tutorial will get you compiling and running Ryo programs in under 5 minutes. By the end, you'll understand how Ryo transforms source code into native executables.

## Prerequisites

Before you begin, make sure you have:

1. **Rust toolchain** installed (1.70 or later)
   ```bash
   rustc --version  # Should show 1.70+
   ```

2. **Ryo compiler** built
   ```bash
   cd /path/to/ryo
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

From the ryo directory, run:

```bash
cargo run -- run first.ryo
```

> **Note:** `cargo run -- run` compiles the Ryo compiler from source, then runs your file. This is for working within the `ryo` repository. After installing Ryo (via `cargo install` or a package manager), you would use `ryo run first.ryo` instead — both do the same thing.

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
[Result] => 0
```

**What just happened?**

1. **[Input Source]** - Shows your source code
2. **[AST]** - Abstract Syntax Tree representation of your program
3. **[Codegen]** - JIT compilation to native code and execution
4. **[Result] => 0** - The exit code (all Milestone 3 programs exit with 0)

> **Tip:** `ryo run` uses JIT (in-memory) execution. To produce a standalone binary, use `ryo build first.ryo` instead — this creates a `first` executable via AOT compilation.

## Understanding Exit Codes

In Ryo Milestone 3, **all programs exit with code 0 (success)**. This follows the Unix convention where 0 indicates successful execution.

```ryo
x = 42    # Evaluates to 42, but program exits with 0
```

### Current Behavior (Milestone 3)

All programs exit with code 0, regardless of expression values. This is the Unix convention for successful program execution.

**Why 0?**
- By convention, exit code 0 means "success" on Unix/Linux/macOS
- Non-zero exit codes indicate errors or specific conditions
- This aligns with how other languages (Rust, Python, Go) handle default exit codes

### Examples

```ryo
# All of these exit with 0 (success)
x = 42           # Exits with 0
result = 0       # Exits with 0
answer = 2 + 3 * 4  # Computes 14, exits with 0
```

### Future: Explicit Exit Codes (Milestone 4+)

In future milestones, you'll be able to explicitly control exit codes:

```ryo
# Planned for Milestone 4 (NOT YET IMPLEMENTED)
fn main() -> int:
	if check_error():
		return 1    # Error
	return 0        # Success
```

## Try More Examples

Now try the official examples in `examples/milestone3/`:

### Simple Arithmetic

```bash
cargo run -- run examples/milestone3/arithmetic.ryo
# Result: 0  (computes 2 + 3 * 4 = 14, but exits with 0)
```

### Parentheses Change Precedence

```bash
cargo run -- run examples/milestone3/parenthesized.ryo
# Result: 0  (computes (10 + 5) * 2 = 30, but exits with 0)
```

### Multiple Statements

```bash
cargo run -- run examples/milestone3/multiple.ryo
# Result: 0  (evaluates all statements, exits with 0)
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
Combines object file with C runtime to create executable using Ryo's managed Zig toolchain (downloaded automatically on first use).

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

### "Toolchain error: Zig binary not found"

**Problem:** The managed Zig toolchain failed to download or install.

**Solution:**
```bash
ryo toolchain install    # Or: cargo run -- toolchain install
ryo toolchain status     # Verify installation
```

### "No such file or directory"

**Problem:** File path is wrong or you're in the wrong directory.

**Solution:**
```bash
# Make sure you're in the ryo directory
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

See `docs/dev/implementation_roadmap.md` for what's coming next!

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
   - [Implementation Roadmap](dev/implementation_roadmap.md) - See what's completed and what's next
   - [Examples README](../examples/milestone3/README.md) - Detailed example explanations

4. **Contribute:**
   - Read [CLAUDE.md](../CLAUDE.md) for project context
   - Check [docs/dev/](dev/) for architecture documentation
   - See [TODO.md](../TODO.md) for areas needing work

## Questions or Problems?

- Check [docs/troubleshooting.md](troubleshooting.md) for solutions
- Read [docs/implementation_roadmap.md](dev/implementation_roadmap.md) for status
- File an issue on GitHub with:
  - Your platform (macOS/Linux/Windows)
  - Ryo version: `git rev-parse HEAD`
  - Full error output

---

**Congratulations!** You've successfully compiled and run your first Ryo program. Welcome to the Ryo community! 🎉
