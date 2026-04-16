# Standard Library Extensions: Rust Crate Wrapping

A curated list of "Best-in-Class" Rust crates to wrap for a powerful, "Batteries-Included" Standard Library.

Since Ryo targets **General Purpose** use (Web, CLI, Scripts), the standard library needs JSON, HTTP, and Regex support out of the box.

### 1. The Data Serialization Layer (`std.json`, `std.toml`)

**The Challenge:** Serde relies on Rust macros (`#[derive(Serialize)]`). Ryo cannot use Rust macros directly.
**The Strategy:** Use the "DOM" approach (Dynamic Object Model). Map Ryo maps/lists to the crate's `Value` type.

*   **`serde_json`:** The industry standard for JSON.
    *   *Usage:* Implement `json.parse(str) -> !Value` and `json.stringify(Value) -> str`.
    *   *Why:* Fast, correct, and handles edge cases perfectly.
*   **`basic-toml` (or `toml`):**
    *   *Usage:* Essential because Ryo uses `ryo.toml`. Needed to parse config files.
    *   *Why:* `basic-toml` is a lighter dependency than the full `toml` crate, sufficient for 99% of config needs.

### 2. The Networking Layer (`std.net.http`)

**The Challenge:** Ryo v0.1/v0.2 is single-threaded/blocking. v0.4 is Green Threaded.
**The Strategy:** Start with a **Blocking Client** wrapped in `#[blocking]` so it can be offloaded to a thread pool later.

*   **`ureq` (Recommended for v0.1):**
    *   *Why:* Pure Rust, blocking I/O, minimal dependencies (no Tokio). Extremely easy to wrap via FFI.
    *   *Features:* HTTPS (via `rustls`), JSON support, simple API.
*   **`reqwest` (blocking feature):**
    *   *Why:* The heavy hitter. Support it later if `ureq` proves insufficient. It brings in many dependencies.

### 3. Text Processing (`std.regex`)

**The Challenge:** Parsing strings safely and fast.

*   **`regex`:**
    *   *Why:* The gold standard. Guaranteed O(N) time complexity (safe against ReDoS attacks).
    *   *Usage:* `regex.new(pattern).match(text)`.
*   **`unicode-segmentation`:**
    *   *Why:* Since `str` is UTF-8, users will need operations like "reverse a string with Emojis."
    *   *Usage:* `str.graphemes()` implementation. Unicode is too complex to implement from scratch.

### 4. Time & Date (`std.time`)

**The Challenge:** Timezones and leap seconds.

*   **`chrono`:**
    *   *Why:* The most feature-rich library for formatting (`%Y-%m-%d`), parsing, and timezone arithmetic.
    *   *Usage:* `Time.now()`, `Duration.seconds(5)`.

### 5. Randomness & Crypto (`std.rand`, `std.crypto`)

*   **`rand`:**
    *   *Why:* Cryptographically secure random number generation (CSPRNG) by default.
    *   *Usage:* `rand.int()`, `rand.shuffle(list)`.
*   **`sha2` / `blake3`:**
    *   *Why:* Basic hashing is needed for cache keys, signatures, etc.

---

### 6. System & Filesystem (`std.fs`, `std.path`)

*   **`glob`:**
    *   *Why:* Expanding `*.txt` is a common scripting need.
*   **`walkdir`:**
    *   *Why:* Recursive directory traversal is hard to write correctly (symlink loops, permissions). Wrap this crate to provide `fs.walk(path)`.

---

### 7. The Implementation Plan (Architecture)

These crates should be bundled as **Static Rust Libraries** linked into the runtime.

#### Example: `std.json` Wrapper

**1. Rust Shim (`runtime/src/lib.rs`)**
```rust
use serde_json::Value;
use std::ffi::{CStr, CString};

#[no_mangle]
pub extern "C" fn ryo_json_parse(input: *const i8) -> *mut Value {
    let c_str = unsafe { CStr::from_ptr(input) };
    let slice = c_str.to_str().unwrap();
    
    match serde_json::from_str(slice) {
        Ok(v) => Box::into_raw(Box::new(v)),
        Err(_) => std::ptr::null_mut(), // Simplified error handling
    }
}
```

**2. Ryo Standard Library (`std/json.ryo`)**
```ryo
package struct SysValue:
    _ptr: *void

pub fn parse(input: str) -> !JsonValue:
    unsafe:
        ptr = sys_json_parse(input.to_cstring())
        if ptr == null: return error.JsonParseError
        return JsonValue(_handle=ptr)
```

### Summary of Recommendations

| Module | Rust Crate | Rationale |
| :--- | :--- | :--- |
| **`std.json`** | `serde_json` | Industry standard, fast. |
| **`std.http`** | `ureq` | Simple, blocking, pure Rust (good for v0.1). |
| **`std.regex`** | `regex` | Safe O(N) matching. |
| **`std.time`** | `chrono` | Best formatting/parsing DX. |
| **`std.fs`** | `walkdir`, `glob` | Scripting essentials. |
| **`std.rand`** | `rand` | Secure defaults. |
| **`std.simd`** | (Cranelift) | Use compiler intrinsics, not a crate. |

This list provides a feature-rich standard library with lightweight implementation effort (wrapping existing code).
