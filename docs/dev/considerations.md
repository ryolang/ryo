# Production Considerations

To transition from a prototype to a production-ready language (especially one targeting Web Services and Data Science), the following **operational** and **ecosystem** realities must be addressed.

---

### 1. Supply Chain Security (The "NPM/Pip" Problem)
Software supply chain attacks (typosquatting, malicious build scripts) are a significant concern. Since Ryo uses a central registry (Phase 5), security must be designed **into the client**.

*   **The Risk:** A user adds `pkg:left-pad`. That package contains a build script that steals SSH keys during installation.
*   **Ryo Design:**
    *   **No "Install Scripts":** By default, installing a package should **never** execute code. It should only download sources.
    *   **Sandboxed Builds:** If a "System" package needs to compile C code (Milestone 21), it must happen in a restricted environment or explicitly request permission: *"Package 'sqlite-sys' wants to run a build script. Allow? [y/N]"*.
    *   **Lockfile Hashing:** `ryo.lock` must store cryptographic hashes of tarballs, not just versions.

### 2. Observability Hooks (For the "Ambient Runtime")
The Green Thread runtime for Network Services (Phase 5) requires production observability.

*   **The Problem:** In a "Colorless" async world (Green Threads), traditional profilers often get confused by stack swapping. They see the Scheduler running, not the Request logic.
*   **Ryo Design:**
    *   **Runtime Events:** `libryo_runtime` (Rust) must expose an event stream (e.g., `on_thread_park`, `on_thread_start`).
    *   **Context Propagation:** The Thread-Local Runtime Context needs a slot for **Trace IDs** (OpenTelemetry). This allows a Request ID to survive a stack swap automatically, enabling distributed tracing without user code changes.

### 3. Cross-Compilation (The "Mac vs. Linux" Reality)
The target audience uses macOS (Apple Silicon) for development but deploys to Linux (AMD64/ARM64) containers.

*   **The Problem:** If Ryo relies heavily on the system C compiler (`cc`) for linking (Milestone 3), cross-compiling becomes a nightmare of installing GCC toolchains.
*   **Ryo Design:**
    *   **Zig-style Linking:** Eventually, bundling `lld` (LLVM Linker) or using `zig cc` as a backend would allow `ryo build --target x86_64-unknown-linux-gnu` to work out of the box from a Mac.
    *   **Pure Ryo/Rust Deps:** Libraries should be pure Ryo/Rust where possible to avoid libc dependency issues.

### 4. Source Maps / Debug Info (DWARF)
Panic stack traces exist (Milestone 25), but full debugger support (GDB/LLDB) requires more.

*   **The Problem:** Cranelift generates machine code. Without DWARF debug data, GDB only sees assembly, not Ryo source lines.
*   **Ryo Design:**
    *   **Line Number Mapping:** The Codegen phase must strictly map Cranelift instructions back to `Span` (File/Line/Col) in the AST.
    *   **Struct Layouts:** DWARF descriptions of Ryo `structs` are needed so GDB knows that "Offset 0 is `id`" and "Offset 8 is `name`".
    *   **Action:** Add a "Debug Info Emission" task to Phase 3 or 4. Without this, Data Science users will struggle to debug logic errors.

### 5. The "Windows Path" Trap (OS Reality)
Ryo defines `str` as UTF-8.
*   **The Reality:**
    *   **Linux/macOS:** Paths are arbitrary bytes (usually UTF-8).
    *   **Windows:** Paths are UTF-16 (UCS-2).
*   **The Conflict:** If `std.fs.open(path: str)` expects UTF-8, what happens when a Windows file has invalid UTF-8 characters?
*   **Ryo Design:**
    *   **The "Go" Approach:** Treat paths as strings, but handle conversion errors at the syscall boundary. This is DX-First.
    *   **The "Rust" Approach:** Create `OsString`. Correct but annoying.
    *   **Decision:** Stick to `str` (UTF-8) for DX. If a file path is not valid UTF-8, Ryo simply **cannot open it**. This is an acceptable trade-off for a general-purpose language (most paths are UTF-8 now), but it must be documented.

---

### Summary of Additions to Roadmap/Spec

1.  **Security:** Spec must define "Safe Package Installation" (No arbitrary code exec).
2.  **Observability:** Spec the `Runtime` to support OTel Context Propagation.
3.  **Debugging:** Compiler must eventually emit **DWARF** data.
4.  **Unicode:** Explicitly state that Ryo is **UTF-8 Only**. Non-UTF-8 file paths are unsupported (or require raw byte access via unsafe `std.sys`).
