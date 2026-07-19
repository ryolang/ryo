**Status:** Design (open questions)

# Ryo Language Design Issues & Recommendations

This document identifies design inconsistencies, open questions, and recommendations for the Ryo language specification and roadmap. Issues are categorized by status. Resolved items are removed (see `git log` for history).

**Last updated:** 2026-07-19 (sweep: removed resolved items — Variable Initialization, Variable Shadowing, Default Integer Size, Global Mutable State — and their checklist entries, per the removal convention; renumbered sequentially and replaced fragile numbered cross-references with named ones. Prior sweep 2026-07-17: trimmed Iterator Invalidation to its open runtime piece.)

---

## Open Issues

These require resolution before implementation reaches the affected milestone.

### 1. The "Hardcoded Generics" Trap

*   **The Smell:** Milestone 22 implements `list[int]` and `list[str]` as "hardcoded types" while pushing real generics to Phase 5 (post-v1.0).
*   **The Problem:** This creates a **Privileged Standard Library**.
    *   User code cannot define types that look or behave like stdlib types.
    *   When real generics arrive, the entire standard library will need rewriting. Early adopters rewrite their code too.
    *   This mirrors Go's pre-1.18 era where `map` and `slice` were magic generic types but user code was stuck with `interface{}`.
*   **Proposal:** Keep hardcoded generics for v0.1 (pragmatic), but use **Monomorphization** (like Rust/C++) when real generics land — the compiler copies the generic code and replaces `T` with the concrete type.

### 2. Error Handling Overhead

*   **The Smell:** The spec claims "Native Performance" but admits a "~5-10% overhead" for mandatory stack trace capture on errors.
*   **The Reality:** Capturing stack traces is extremely expensive in high-throughput systems (10x-100x slower than the operation itself).
*   **Proposal: Lazy Symbol Resolution + PC Capture**
    1.  **At Runtime (Fast):** Capture *only* the Program Counter and Stack Pointer (copying a few integers — nanoseconds).
    2.  **At Print Time (Slow):** Resolve pointers to File/Line/Function strings only when `.stack_trace()` is called or the program panics.
    3.  **Production:** Provide `--strip-debug` compiler flag to disable entirely.
*   **Lightweight errors:** Add a `#[no_trace]` attribute for errors used as control flow (e.g., `EndOfFile`).

### 3. Circular Dependencies

*   **The Smell:** "Directory = Module" + "No Cycles" mimics Go's structural rigidity.
*   **The Problem:** A `User` struct in `models/` needs to save to `db/`, but `db/` needs to return `User` objects. You're forced to create a third "types" package, breaking encapsulation.
*   **Proposal:** Allow circular dependencies **within the same module (directory)**, ban them between modules. The compiler compiles the directory as a single unit (like a Go package).

### 4. Specification Holes

*   **`impl` Blocks for non-Trait methods:** Roadmap shows `impl Rectangle: ...` but the spec only details `impl Trait for Type`.
    *   *The Fix:* Document inherent implementations explicitly in Section 3.
*   **Generics Strategy:** Roadmap uses "Hardcoded Generics" for v0.1, but the spec implies true generics.
    *   *The Fix:* Explicitly note that user-defined generics are post-v0.1.

### 5. Conflicting Syntax: The `!` Operator

*   `!` is used for **Error Unions** (`!T`) but `not` is used for boolean logic.
*   C/Rust/Java/JS developers have muscle memory for `!x` meaning "not x". Repurposing `!` for types may confuse the target audience. `!?T` looks like "Not Optional T" to a C-family eye but means "Error or Optional T" in Ryo.
*   **Status:** Needs decision — keep `!` for errors (Zig-like) or find alternative syntax.

---

## Grey Areas

These are underspecified aspects that will confuse developers if left undefined.

### 6. Structural Equality

*   When `struct_a == struct_b`, compare fields (Python/Rust) or addresses (Java)?
*   **Proposal: Automatic Structural Equality** for structs containing only comparable types. Pointer equality via `std.mem.same_address(a, b)`.
*   **Status:** Open — prerequisite is Milestone 9 (structs).

### 7. String Indexing — The Unicode Trap

*   `str` is UTF-8. `s[0]` is ambiguous: 0th byte (fast, dangerous) or 0th character (safe, O(N))?
*   **Proposal: Forbid `s[i]`.** Provide `.bytes()[i]` (O(1), returns `u8`) and `.runes()[i]` (O(N), returns `char`).
*   **Status:** Open, but the current direction leans the other way — Milestone 8.4 (String Slices) plans the slice operator `s[start:end]`, i.e. indexing-syntax on strings. The `s[i]` scalar-index question needs an explicit decision alongside M8.4.

### 8. Default Arguments

*   No overloading (correct), but default arguments are missing.
*   **Proposal:** Allow `fn connect(url: str, retries: int = 3)`.
*   **Status:** Decided — scheduled as **Milestone 8.5** (Default Parameters & Named Arguments). The remaining work is the spec text when M8.5 lands.

### 9. Panic During Drop

*   If a `.drop()` panics while unwinding from another panic, undefined behavior.
*   **Proposal: Immediate Abort.** Document clearly.
*   **Status:** Open — prerequisite is Milestone 23 (RAII & Drop).

### 10. Variadic Functions

*   `print` is variadic. Can users define them?
*   **Proposal: Reserve for built-ins only (v0.1).** Users accept lists: `fn log(msgs: list[str])`.
*   **Status:** Matches the current implementation (`print` is special-cased in codegen — I-006 — and user-defined variadics don't exist), but the reservation is not yet documented in the spec.

### 11. Iterator Invalidation — OPEN (runtime piece only)

*   **Compile-time half resolved:** Scope-locked views (spec §5.7) prevent iterators from escaping their block, and Rule 7 (one writer OR many readers) prevents simultaneous mutation and iteration in concurrent contexts. The ownership framework shipped with M8.1/M8.2; the intra-call aliasing exclusion (Rule 7) was completed in M8.3.
*   **Open (runtime):** For sequential code where mutation happens *inside* the loop body (e.g., `list.append()` during `for n in list`), **Versioned Iterators** are still needed as a runtime safety net:
    *   Every collection has an internal `mod_count` that increments on modification.
    *   Iterators capture `expected_mod` at creation and check on every `next()`.
    *   Mismatch triggers a panic with a clear message: `"collection modified during iteration"`.
    *   Cost: one integer comparison per iteration — negligible.

### 12. Loop-as-an-Expression and Safe `continue` (Zig inspirations)

*   **The Smell:**
    1. Loop-as-an-expression (`break <value>`) is highly useful for searching and initializing immutable variables, but is currently deferred.
    2. In `while` loops, using `continue` skips the rest of the block. If manual counter increments are placed there, it results in an accidental infinite loop. Zig solves this with `while (cond) : (continue_expr)`.
*   **Proposal for Loop Expressions:** Consider implementing `break <value>` in a later milestone (requires `Phi` node insertion or stack allocs at the exit block).
*   **Proposal for Safe `while`:** Since Ryo uses Pythonic syntax, adding `:(expr)` breaks aesthetic coherence. However, we could introduce a `defer` statement or explore a block-level `continue` hook to guarantee increment execution. For now, rely on `ForRange` loops where `continue` safely jumps to the builtin increment block.

---

## Recommendations

### Spec Completeness

- Document inherent `impl` blocks (Issue 4 — Specification Holes).
- Add a "post-v0.1" note to Sections 9 (Concurrency) and generics usage.

### Grey Area Decisions

- Structural equality semantics (Issue 6 — blocked on M9).
- `s[i]` scalar indexing vs. slices (Issue 7 — decide alongside M8.4, which currently plans `s[start:end]`).
- Default function arguments (Issue 8 — decided, lands with M8.5; spec text then).
- Document double-panic abort behavior (Issue 9 — blocked on M23).
- Document the variadic-as-builtin-only reservation (Issue 10 — already true in the compiler).
- Resolve the `!` operator conflict (Issue 5).
- Track Loop-as-an-Expression and Safe `continue` (Issue 12).

### Checklist

- [ ] Define `==` as structural equality (M9 prerequisite)
- [ ] Forbid `str` indexing, add `.bytes()` and `.runes()` (revisit against M8.4 slices)
- [ ] Allow default function arguments (M8.5)
- [ ] Document double-panic abort behavior (M23)
- [ ] Reserve variadic functions for built-ins only (already true in the compiler; needs spec text)
- [ ] Resolve `!` operator conflict (Issue 5)
- [ ] Track Loop-as-an-Expression and Safe `continue` (Issue 12)

## References

- Spec: `docs/specification.md` (each resolved issue lands in its relevant section)
- Roadmap: `docs/dev/implementation_roadmap.md`
