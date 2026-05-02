# Ryo Language Design Issues & Recommendations

This document identifies design inconsistencies, open questions, and recommendations for the Ryo language specification and roadmap. Issues are categorized by status.

**Last updated:** 2026-04-20 (full-tree consistency sweep after spec coherence plan).

---

## Open Issues

These require resolution before implementation reaches the affected milestone.

### 2. The "Hardcoded Generics" Trap

*   **The Smell:** Milestone 22 implements `list[int]` and `list[str]` as "hardcoded types" while pushing real generics to Phase 5 (post-v1.0).
*   **The Problem:** This creates a **Privileged Standard Library**.
    *   User code cannot define types that look or behave like stdlib types.
    *   When real generics arrive, the entire standard library will need rewriting. Early adopters rewrite their code too.
    *   This mirrors Go's pre-1.18 era where `map` and `slice` were magic generic types but user code was stuck with `interface{}`.
*   **Proposal:** Keep hardcoded generics for v0.1 (pragmatic), but use **Monomorphization** (like Rust/C++) when real generics land — the compiler copies the generic code and replaces `T` with the concrete type.

### 3. Error Handling Overhead

*   **The Smell:** The spec claims "Native Performance" but admits a "~5-10% overhead" for mandatory stack trace capture on errors.
*   **The Reality:** Capturing stack traces is extremely expensive in high-throughput systems (10x-100x slower than the operation itself).
*   **Proposal: Lazy Symbol Resolution + PC Capture**
    1.  **At Runtime (Fast):** Capture *only* the Program Counter and Stack Pointer (copying a few integers — nanoseconds).
    2.  **At Print Time (Slow):** Resolve pointers to File/Line/Function strings only when `.stack_trace()` is called or the program panics.
    3.  **Production:** Provide `--strip-debug` compiler flag to disable entirely.
*   **Lightweight errors:** Add a `#[no_trace]` attribute for errors used as control flow (e.g., `EndOfFile`).

### 4. Circular Dependencies

*   **The Smell:** "Directory = Module" + "No Cycles" mimics Go's structural rigidity.
*   **The Problem:** A `User` struct in `models/` needs to save to `db/`, but `db/` needs to return `User` objects. You're forced to create a third "types" package, breaking encapsulation.
*   **Proposal:** Allow circular dependencies **within the same module (directory)**, ban them between modules. The compiler compiles the directory as a single unit (like a Go package).

### 5. Specification Holes

*   **`main` Return:** Spec says `void`, Roadmap says `int`.
    *   *The Fix:* Standardize on `void` (implicitly returns exit code 0).
*   **`never` Type:** Used in panic definitions but not defined in the Type section.
    *   *The Fix:* Add the Bottom Type (`!`) to the spec.
*   **`impl` Blocks for non-Trait methods:** Roadmap shows `impl Rectangle: ...` but the spec only details `impl Trait for Type`.
    *   *The Fix:* Document inherent implementations explicitly in Section 3.
*   **Generics Strategy:** Roadmap uses "Hardcoded Generics" for v0.1, but the spec implies true generics.
    *   *The Fix:* Explicitly note that user-defined generics are post-v0.1.

### 6. Conflicting Syntax: The `!` Operator

*   `!` is used for **Error Unions** (`!T`) but `not` is used for boolean logic.
*   C/Rust/Java/JS developers have muscle memory for `!x` meaning "not x". Repurposing `!` for types may confuse the target audience. `!?T` looks like "Not Optional T" to a C-family eye but means "Error or Optional T" in Ryo.
*   **Status:** Needs decision — keep `!` for errors (Zig-like) or find alternative syntax.

---

## Grey Areas

These are underspecified aspects that will confuse developers if left undefined.

### 7. Variable Initialization

*   **The Risk:** If `mut x: int` is allowed without initialization, you need complex flow analysis to prevent reading before writing.
*   **Proposal: Mandatory Initialization.** Reject `mut x: int`. Require `mut x: int = 0`.

### 8. Variable Shadowing

*   **Proposal: Allow Shadowing (Rust Style).** Useful for transformations where the old binding is no longer needed.

### 9. Structural Equality

*   When `struct_a == struct_b`, compare fields (Python/Rust) or addresses (Java)?
*   **Proposal: Automatic Structural Equality** for structs containing only comparable types. Pointer equality via `std.mem.same_address(a, b)`.

### 10. String Indexing — The Unicode Trap

*   `str` is UTF-8. `s[0]` is ambiguous: 0th byte (fast, dangerous) or 0th character (safe, O(N))?
*   **Proposal: Forbid `s[i]`.** Provide `.bytes()[i]` (O(1), returns `u8`) and `.runes()[i]` (O(N), returns `char`).

### 11. Default Integer Size

*   `int` defaults to `isize` (varies by platform — 32-bit vs 64-bit).
*   **Proposal: Default to `i64`.** Consistent across platforms. Use `isize` only for indexing.

### 12. Default Arguments

*   No overloading (correct), but default arguments are missing.
*   **Proposal:** Allow `fn connect(url: str, retries: int = 3)`.

### 13. Panic During Drop

*   If a `.drop()` panics while unwinding from another panic, undefined behavior.
*   **Proposal: Immediate Abort.** Document clearly.

### 14. Variadic Functions

*   `print` is variadic. Can users define them?
*   **Proposal: Reserve for built-ins only (v0.1).** Users accept lists: `fn log(msgs: list[str])`.

### 15. Global Mutable State

*   No mechanism defined for application-wide state (e.g., DB pool).
*   **Proposal:** Allow `const` at module level (compile-time constant). For mutable state, use `Shared[T]` as defined in spec section 5.6. No module-level `mut` variables — shared state must be explicit.

---

### 16. Iterator Invalidation — PARTIALLY RESOLVED

*   **Was:** Modifying a list while iterating causes use-after-free without Rust's strict borrow checker.
*   **Resolution (compile-time):** Scope-locked views (spec 5.7) prevent iterators from escaping their block. Rule 7 (one writer OR many readers) prevents simultaneous mutation and iteration in concurrent contexts.
*   **Remaining (runtime):** For sequential code where mutation happens *inside* the loop body (e.g., `list.append()` during `for n in list`), **Versioned Iterators** are still needed as a runtime safety net:
    *   Every collection has an internal `mod_count` that increments on modification.
    *   Iterators capture `expected_mod` at creation and check on every `next()`.
    *   Mismatch triggers a panic with a clear message: `"collection modified during iteration"`.
    *   Cost: one integer comparison per iteration — negligible.



### Priority 2 (Spec Completeness)
3.  Add `never` type (`!`) to Section 4.
5.  Document inherent `impl` blocks.
6.  Add "post-v0.1" note to Sections 9 (Concurrency) and generics usage.

### Priority 3 (Grey Area Decisions)
7.  Mandate variable initialization.
8.  Decide on shadowing.
9.  Forbid `s[i]` string indexing.
10. Default `int` to `i64`.
11. Allow default function arguments.
12. Document double-panic behavior.

### Checklist

- [ ] Add `never` type to spec
- [ ] Define `mut x: T = val` as mandatory
- [ ] Define shadowing rules
- [ ] Define `==` as structural equality
- [ ] Forbid `str` indexing, add `.bytes()` and `.runes()`
- [ ] Default `int` to `i64`
- [ ] Allow default function arguments
- [ ] Document double-panic abort behavior
- [ ] Reserve variadic functions for built-ins only
- [ ] Resolve `!` operator conflict
