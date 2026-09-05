**Status:** Design (v0.2+) — partially absorbed into the spec (§14, §17)

# First Standard Library Module: `std.io`

The first standard library implementation should be **`std.io`** (Console Input/Output), backed by a hidden **`std.sys`** (C Bindings). This choice validates the manifest-declared `allow_unsafe` capability (spec §17) and the "Ownership Lite" model.

### 1. Why `std.io` First

Implementing `std.io` first forces resolution of the **three hardest infrastructure problems** immediately, before the language grows complex:

1.  **The Unsafe Boundary:** Calling C functions (`write`, `read`) safely.
2.  **String Layout:** Deciding how `str` (Ryo) maps to `char*` (C).
3.  **Error Handling:** I/O is the first place where `!T` (Error Unions) matters.

### 2. The Architecture (Layered Implementation)

Two modules are needed. This validates the `allow_unsafe` capability model (spec §17).

#### Layer 1: `std.sys` (The Hidden Foundation)
*   **Location:** `src/std/sys.ryo`
*   **Config:** `allow_unsafe = true` in `ryo.toml` (spec §17).
*   **Role:** Raw C bindings. No safety guarantees.

```ryo
# std/sys.ryo
package fn libc_write(fd: i32, buf: *u8, count: usize) -> isize:
    extern "C":
        fn write(fd: i32, buf: *u8, count: usize) -> isize

package fn libc_malloc(size: usize) -> *void:
    extern "C":
        fn malloc(size: usize) -> *void

package fn libc_free(ptr: *void):
    extern "C":
        fn free(ptr: *void)
```
*Note: `package` visibility restricts access to the Standard Library only.*

#### Layer 2: `std.io` (The Safe Facade)
*   **Location:** `src/std/io.ryo`
*   **Config:** Standard safety rules; the one internal `unsafe` block still requires `allow_unsafe = true` in the manifest (spec §17).
*   **Role:** The user-facing API.

```ryo
import std.sys

# Define the Error Type (Milestone 9)
pub error WriteError(code: int)

# Safe Wrapper
pub fn write(s: &str) -> WriteError!void:
    # 1. Access the raw parts of the string slice (Milestone 10)
    ptr = s.ptr()
    len = s.len()

    # 2. Call the system layer
    #: SAFETY: fd 1 (stdout) is a valid descriptor; `ptr`/`len` come from a live `&str`.
    unsafe:
        res = sys.libc_write(1, ptr, len) # 1 = stdout

    if res < 0:
        return WriteError(res)

    return void

pub fn println(s: &str) -> WriteError!void:
    try write(s)
    try write("\n")
    return void
```
*Note: the fallible API is named `write` — `print` is the infallible builtin (spec §14) and `std.io` does not redefine it.*

### 3. Validation

Implementing just `write` and `println` proves that:

1.  **Milestone 3 (CodeGen):** The compiler can link against `libc`.
2.  **Milestone 10 (Strings):** The memory layout of `&str` (Fat Pointer) is correctly defined so that `s.ptr()` and `s.len()` work.
3.  **Milestone 21 (FFI Gatekeeper):** The compiler correctly allows `extern "C"` in `std.sys` but forbids it in packages without `allow_unsafe = true` (spec §17).

### 4. The Next Step: `std.mem` (The Allocator)

Once `std.io` works for literals (`write("hello")`), a wall is immediately hit: **dynamic text cannot be written** (`write("Hello " + name)`).

This dictates the **second** stdlib module: **`std.mem`**.

*   **Role:** Wraps `malloc` and `free`.
*   **Enables:** The owned `str` type (concatenation) and `list` (arrays).
*   **Safety:** This is where the **Concurrent Allocator** requirement (Milestone 30 prep) is addressed; the thread-safe allocator choice itself is owned by the spec (§14.5.6).

### Summary
1.  **First:** `std.sys` (Private, Unsafe C bindings).
2.  **Second:** `std.io` (Public, Safe Console Output).
3.  **Third:** `std.mem` (Public/Private, Allocator for Strings/Lists).

This order delivers "Hello World" first (high morale), then immediately forces resolution of memory management (the core architecture).

## References
- Spec: `docs/specification.md` §14 (Standard Library — hybrid runtime architecture, core packages), §17 (FFI & `unsafe` gatekeeping)
- Dev: `docs/dev/built_in.md` (built-in vs stdlib boundary), `docs/dev/std_ext.md`
- Roadmap: `docs/dev/implementation_roadmap.md`
