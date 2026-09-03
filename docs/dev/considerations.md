**Status:** Design (future concerns; remaining items not yet in the spec — absorbed items swept out, see References)

# Production Considerations

To transition from a prototype to a production-ready language (especially one targeting Web Services and Data Science), the following **operational** and **ecosystem** realities must be addressed. Items already absorbed into the spec (cross-compilation, DWARF debug info, UTF-8 strings, UTF-8 filesystem paths, context propagation) have been swept out — see References.

---

### 1. Supply Chain Security (The "NPM/Pip" Problem)
Software supply chain attacks (typosquatting, malicious build scripts) are a significant concern. Since Ryo uses a central registry (Phase 5), security must be designed **into the client**.

*   **The Risk:** A user adds `pkg:left-pad`. That package contains a build script that steals SSH keys during installation.
*   **Ryo Design:**
    *   **No "Install Scripts":** By default, installing a package should **never** execute code. It should only download sources.
    *   **Sandboxed Builds:** If a "System" package needs to compile C code (Milestone 21), it must happen in a restricted environment or explicitly request permission: *"Package 'sqlite-sys' wants to run a build script. Allow? [y/N]"*.
    *   **Lockfile Hashing:** `ryo.lock` must store cryptographic hashes of tarballs, not just versions. The spec currently guarantees only reproducible builds via `ryo.lock` (§13); the hashing rule is not yet specified.

### 2. Observability Hooks (For the "Ambient Runtime")
The Green Thread runtime for Network Services (Phase 5) requires production observability.

*   **The Problem:** In a "Colorless" async world (Green Threads), traditional profilers often get confused by stack swapping. They see the Scheduler running, not the Request logic.
*   **Ryo Design:**
    *   **Runtime Events:** `libryo_runtime` (Rust) must expose an event stream (e.g., `on_thread_park`, `on_thread_start`).
    *   **Trace-ID Slot:** Context propagation itself is now a spec-level planned item (§19, "Context Propagation & Cancellation Deadlines"). What remains here is the runtime-side detail the spec does not cover: the Thread-Local Runtime Context needs a slot for **Trace IDs** (OpenTelemetry). This allows a Request ID to survive a stack swap automatically, enabling distributed tracing without user code changes.

---

### Summary of Additions to Roadmap/Spec

1.  **Security:** Spec must define "Safe Package Installation" (No arbitrary code exec) on top of the §13 lockfile guarantee.
2.  **Observability:** Spec the `Runtime` to support OTel Trace-ID propagation (the language-level concept is already planned in §19).

## References
- Spec: `docs/specification.md` — §13 (package manager / `ryo.lock`), §19 (context propagation). Absorbed and removed: §4.2 (`str` is UTF-8), §7.6 & §7.9 (DWARF debug symbols via Cranelift, always-on), §14 (`os` package: filesystem paths are plain `str`/UTF-8, non-UTF-8 paths fail with `OsError`), §16 (cross-compilation via Zig linker).
- Roadmap: `docs/dev/implementation_roadmap.md`
