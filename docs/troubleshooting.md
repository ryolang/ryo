# Troubleshooting Guide

This guide provides solutions to common issues when compiling and running Ryo programs.

## Table of Contents

1. [Compilation Errors](#compilation-errors)
2. [Linking Errors](#linking-errors)
3. [Runtime Issues](#runtime-issues)
4. [Platform-Specific Issues](#platform-specific-issues)
5. [Getting Help](#getting-help)

---

## Compilation Errors

### "Failed to link with any available linker"

**Symptom:**
```
Error: LinkError("Failed to link with any available linker. Last error: ...")
```

**Cause:** No C linker found on your system. Ryo tries `zig cc`, `clang`, and `cc` in order, but none were found.

**Solution:**

**macOS:**
```bash
xcode-select --install
```
This installs the Xcode Command Line Tools which includes `clang`.

**Linux (Ubuntu/Debian):**
```bash
sudo apt update
sudo apt install build-essential
```
This installs `gcc`, `g++`, and other build tools.

**Linux (Fedora/RHEL):**
```bash
sudo dnf groupinstall "Development Tools"
```

**Universal Solution (Zig):**
```bash
# Download from https://ziglang.org/download/
# Extract and add to PATH
export PATH=$PATH:/path/to/zig
```
Zig includes a C compiler that works on all platforms.

---

### "Failed to create ObjectBuilder: Unsupported target"

**Symptom:**
```
Error: CodegenError("Unsupported target 'unknown-unknown-unknown': ...")
```

**Cause:** Cranelift doesn't support your target architecture.

**Solution:**

1. Check your target:
   ```bash
   cargo run -- ir test.ryo
   ```
   Look at the `Target:` line.

2. Supported targets include:
   - `x86_64-unknown-linux-gnu`
   - `x86_64-apple-darwin`
   - `aarch64-apple-darwin` (Apple Silicon)
   - `x86_64-pc-windows-msvc`

3. If your target is unusual, you may need to:
   - Use a different machine
   - Cross-compile from a supported host
   - File an issue on GitHub for support

---

### Parse Errors: "found 'X' expected 'Y'"

**Symptom:**
```
[03] Error: found '42' expected ':', or '='
   ╭─[cmdline:1:3]
   │
 1 │ x 42
   │   ─┬
   │    ╰──── found '42' expected ':', or '='
   ╰────
```

**Cause:** Syntax error in your Ryo code.

**Solution:**

1. **Check the error message** - It shows exactly what's wrong
2. **Check syntax rules:**
   - Variable declarations need `=`:
     ```ryo
     # Wrong:
     x 42

     # Right:
     x = 42
     ```
   - Type annotations use `:`:
     ```ryo
     # Wrong:
     x int = 42

     # Right:
     x: int = 42
     ```
   - Operators need spaces:
     ```ryo
     # Wrong:
     x=42

     # Right:
     x = 42
     ```

3. **Check examples** in `examples/milestone3/` for correct syntax

---

### "No such file or directory"

**Symptom:**
```
Error: IoError(Os { code: 2, kind: NotFound, message: "No such file or directory" })
```

**Cause:** File path is incorrect or file doesn't exist.

**Solution:**

1. **Check file exists:**
   ```bash
   ls -la your_file.ryo
   ```

2. **Use correct path:**
   ```bash
   # Absolute path (always works)
   cargo run -- run /full/path/to/program.ryo

   # Relative path (from ryox directory)
   cargo run -- run examples/milestone3/simple.ryo
   ```

3. **Check current directory:**
   ```bash
   pwd  # Should be in ryox/
   ```

---

## Linking Errors

### "undefined reference to `main`"

**Symptom:**
```
Error: LinkError("... undefined reference to `main`")
```

**Cause:** Object file doesn't contain a `main` function symbol.

**Solution:**

This shouldn't happen in normal operation. If you see this:

1. Check that codegen succeeded (look for "Generated object file" message)
2. Try inspecting the object file:
   ```bash
   nm program.o | grep main
   # Should show: T _main (or T main on Linux)
   ```
3. If `main` is missing, this is a compiler bug - file an issue on GitHub

---

### "ld: library not found"

**Symptom:**
```
Error: LinkError("... library not found for -lc")
```

**Cause:** System libraries not found by linker.

**Solution:**

1. **Install development libraries:**
   - macOS: Should be included with Xcode Command Line Tools
   - Linux: `sudo apt install libc6-dev` (Ubuntu/Debian)

2. **Try Zig as linker:**
   - Zig bundles all needed libraries
   - Download from https://ziglang.org/
   - Will be used automatically if in PATH

---

## Runtime Issues

### Exit Codes in Milestone 3

**Current Behavior:**

All Milestone 3 programs exit with code 0 (success), regardless of expression values.

**Example:**
```ryo
x = 42       # Evaluates to 42, but exits with 0
result = -1  # Evaluates to -1, but exits with 0
```

**Output:**
```
[Result] => 0
```

**Explanation:**

This is intentional behavior that aligns with industry standards:
- Exit code 0 indicates success (Unix/Linux/macOS convention)
- Non-zero exit codes traditionally indicate errors
- Explicit exit codes will be added in Milestone 4 via return statements

**Future (Milestone 4+):**

Explicit exit code control via return statements:
```ryo
# Planned syntax (NOT YET IMPLEMENTED)
fn main() -> int:
    if error_condition:
        return 1    # Error
    return 0        # Success
```

**If you need specific exit codes now:**

The current implementation always returns 0. To test error conditions or specific exit codes, you'll need to wait for Milestone 4.

---

### "Permission denied" when running executable

**Symptom:**
```
Error: ExecutionError("Permission denied (os error 13)")
```

**Cause:** Executable doesn't have execute permissions.

**Solution:**

**Unix/macOS:**
```bash
chmod +x program
./program
```

**macOS Specific (Gatekeeper):**
If macOS blocks execution:
1. System Preferences → Security & Privacy
2. Click "Allow Anyway" for the blocked program
3. Run again

---

### Executable Not Found

**Symptom:**
```
Error: ExecutionError("No such file or directory (os error 2)")
```

**Cause:** Generated executable not in expected location.

**Solution:**

1. **Check executable was created:**
   ```bash
   ls -la *.o        # Object file
   ls -la program    # Executable (Unix)
   ls -la *.exe      # Executable (Windows)
   ```

2. **Check linking succeeded:**
   Look for "Linked with X: program" message in output

3. **Run from correct directory:**
   Executables are created in the current working directory, not the source file's directory

4. **Platform differences:**
   - Unix: `./program` (needs `./` prefix)
   - Windows: `program.exe` (direct execution)

---

### "Segmentation fault" or "Illegal instruction"

**Symptom:**
Program crashes with segfault or illegal instruction.

**Cause:** This indicates a code generation bug - compiled code is invalid.

**Solution:**

1. **Reduce test case** to minimal example
2. **Check your code** for unsupported features
3. **File a bug report** with:
   - Your Ryo code
   - Platform (OS, architecture)
   - Ryo version: `git rev-parse HEAD`
   - Full error output

This should not happen with current Milestone 3 features - if it does, it's a compiler bug.

---

## Platform-Specific Issues

### macOS Issues

#### "clang: error: unsupported option '-arch'"

**Cause:** Trying to cross-compile or architecture mismatch.

**Solution:**
Ensure you're compiling for the current architecture. Ryo uses `Triple::host()` which should be correct automatically.

#### "xcrun: error: invalid active developer path"

**Cause:** Xcode Command Line Tools not installed or corrupted.

**Solution:**
```bash
xcode-select --install
# If that fails:
sudo xcode-select --reset
xcode-select --install
```

#### "killed: 9" when running executable

**Cause:** macOS Gatekeeper or code signing issue.

**Solution:**
1. System Preferences → Security & Privacy → Allow
2. Or disable Gatekeeper temporarily:
   ```bash
   sudo spctl --master-disable
   # Run program
   sudo spctl --master-enable
   ```

---

### Linux Issues

#### "cannot find -lc"

**Cause:** Standard C library development files not installed.

**Solution:**
```bash
# Ubuntu/Debian
sudo apt install libc6-dev

# Fedora/RHEL
sudo dnf install glibc-devel

# Arch
sudo pacman -S glibc
```

#### "as: unrecognized option '--64'"

**Cause:** Old or incompatible assembler.

**Solution:**
Install newer binutils or use Zig:
```bash
# Update binutils
sudo apt upgrade binutils

# Or use Zig
# Download from https://ziglang.org/
```

---

### Windows Issues

#### "error LNK2001: unresolved external symbol"

**Cause:** MSVC linker can't find symbols.

**Solution:**

1. **Use Visual Studio Developer Command Prompt**
2. **Or ensure MSVC is in PATH:**
   ```cmd
   call "C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Auxiliary\Build\vcvars64.bat"
   ```
3. **Or install Zig** for a standalone C compiler

#### "Access is denied"

**Cause:** Antivirus blocking executable generation or execution.

**Solution:**

1. Add exception in antivirus for:
   - `ryox` directory
   - Generated `.exe` files
2. Or temporarily disable real-time protection

#### ".exe is not a valid Win32 application"

**Cause:** Architecture mismatch (32-bit vs 64-bit).

**Solution:**
- Ryo generates 64-bit executables by default
- Ensure you're running on 64-bit Windows
- Check architecture: `systeminfo | findstr /C:"System Type"`

---

## General Debugging

### Enable Verbose Output

Currently, Ryo shows compilation stages automatically. For more detail:

```bash
# Run with Rust backtrace
RUST_BACKTRACE=1 cargo run -- run program.ryo

# Run with full backtrace
RUST_BACKTRACE=full cargo run -- run program.ryo
```

### Clean Build

If things seem broken, try a clean build:

```bash
cargo clean
cargo build --release
```

### Check Ryo Version

```bash
git rev-parse HEAD  # Current commit
git log -1          # Last commit details
```

### Verify Installation

```bash
# Check Rust
rustc --version

# Check linker
which zig
which clang
which cc

# Test simple program
echo "x = 42" > test.ryo
cargo run -- run test.ryo
# Should output: [Result] => 42
```

---

## Object Files and Cleanup

### "Too many .o files cluttering my directory"

**Issue:** Object files and executables accumulate in your directory.

**Temporary Solution:**

**Unix/macOS:**
```bash
# Remove all object files
rm *.o

# Remove specific executable
rm program_name

# Remove all compiled artifacts
rm *.o program_name
```

**Windows:**
```cmd
del *.obj
del *.exe
```

**Future:** Ryo will use a build directory like Cargo's `target/` in future versions.

---

## Performance Issues

### "Compilation is slow"

**Bottleneck:** Linking is usually the slowest phase (~200-400ms).

**Optimization:**
1. **Use Zig** - Faster than gcc/clang for simple programs
2. **Release builds** compile Ryo itself faster:
   ```bash
   cargo build --release
   ./target/release/ryo run program.ryo
   ```
3. **Reduce program size** - Fewer statements = faster compilation

**Expected Times (small programs):**
- Lexing: <10ms
- Parsing: ~50ms
- Codegen: ~100-200ms
- Linking: ~200-400ms (bottleneck)
- **Total:** ~500ms

### "Generated executable is large"

**Current Size:** ~16KB for minimal programs.

**Explanation:**
- Includes C runtime startup code
- Static linking (currently)
- Will improve with:
  - Dynamic linking option
  - Ryo runtime (future)
  - Strip symbols option

**Reduce Size:**
```bash
# Strip symbols (Unix/macOS)
strip program

# Before: ~16KB
# After:  ~8KB
```

---

## Getting Help

If your issue isn't covered here:

### 1. Check Documentation

- [Quick Start Guide](quickstart.md) - Getting started
- [Getting Started](getting_started.md) - Language introduction
- [Implementation Roadmap](implementation_roadmap.md) - Current status and limitations
- [Compilation Pipeline](dev/compilation_pipeline.md) - How compilation works

### 2. Check Examples

```bash
cd examples/milestone3
ls -la
cat README.md
```

Working examples help identify what's wrong with your code.

### 3. Search Issues

Check if your issue is known:
https://github.com/ryolang/ryox/issues

### 4. File a Bug Report

If you've found a bug, file an issue with:

**Required Information:**
1. **Platform:** macOS 13.1, Ubuntu 22.04, Windows 11, etc.
2. **Architecture:** x86_64, aarch64 (Apple Silicon), etc.
3. **Ryo Version:**
   ```bash
   git rev-parse HEAD
   ```
4. **Rust Version:**
   ```bash
   rustc --version
   ```
5. **Linker Used:** zig cc, clang, cc
6. **Full Error Output:** Copy-paste entire error message
7. **Minimal Example:** Smallest code that reproduces the issue

**Example Bug Report:**

```markdown
## Issue: Compilation fails with linker error

**Platform:** macOS 13.1 (Ventura)
**Architecture:** aarch64 (Apple M1)
**Ryo Version:** abc123def456
**Rust Version:** 1.70.0
**Linker:** zig cc 0.11.0

**Code:**
\`\`\`ryo
x = 42
\`\`\`

**Error:**
\`\`\`
Error: LinkError("ld: library not found for -lc")
\`\`\`

**Steps to Reproduce:**
1. Create test.ryo with code above
2. Run: cargo run -- run test.ryo
3. See error

**Expected:** Should compile and return 42
**Actual:** Linker error
```

### 5. Ask for Help

- **GitHub Discussions:** General questions
- **Issues:** Bugs and feature requests

---

## Known Limitations (Milestone 3)

These are not bugs - they're features not yet implemented:

### Cannot Reference Variables

```ryo
x = 10
y = 20
z = x + y  # Error: Variables not yet implemented
```

**Workaround:** Only use literals and operations in expressions.
**Coming:** Milestone 4 (Functions) will add proper variables.

### Cannot Define Functions

```ryo
fn add(a: int, b: int) -> int:
    return a + b  # Error: Functions not yet supported
```

**Coming:** Milestone 4

### No Strings

```ryo
name = "Alice"  # Error: String literals not yet supported
```

**Coming:** Future milestone

### No Control Flow

```ryo
if x > 0:  # Error: if statements not yet supported
    print("positive")
```

**Coming:** Milestone 6

### No Error Handling

```ryo
x = try divide(10, 0)  # Error: try/errors not yet supported
```

**Coming:** Milestone 7

---

## Quick Reference: Error Codes

| Exit Code | Meaning (Ryo Compiler) |
|-----------|------------------------|
| 0 | Success |
| 1 | IO Error (file not found, permission denied) |
| 101 | General error (fallback) |

| Exit Code | Meaning (Your Program) |
|-----------|------------------------|
| 0 | Success (by convention) |
| 1-255 | User-defined error codes |
| Negative (Windows) | User-defined |
| Negative (Unix) | Wraps to 0-255 |

---

**Can't find your issue?** Check the [documentation index](README.md) or [file an issue](https://github.com/ryolang/ryox/issues/new) on GitHub.
