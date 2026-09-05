**Status:** Design (open questions)

# Ryo Language Design Issues & Recommendations

This document identifies design inconsistencies, open questions, and recommendations for the Ryo language specification and roadmap. Issues are categorized by status. Resolved items are removed (see `git log` for history).

**Last updated:** 2026-09-01 (sweep: removed items resolved into the spec — Circular Dependencies (§11.7), the `!` operator conflict (§2 lexical conventions), Structural Equality (§4.5, `#[derive(Eq)]`), String Indexing (§4.7), Default Arguments (§6.1.1), Variadic Functions (§6.1.2 + §19 rejected list), the Versioned-Iterator runtime piece (§4.7), and Loop-as-an-Expression (§3 control flow) — and their checklist entries, per the removal convention; renumbered sequentially; trimmed Specification Holes to the struct-`impl` gap and the Zig-inspirations item to Safe `continue`. Prior sweeps: 2026-07-19, 2026-07-17.)

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
*   **Status:** Partially settled — the spec commits to static dispatch via monomorphization (§8, Traits) and notes that user-defined generics are future work (§4.7 note, §19). The privileged-stdlib concern above stands while Milestone 22 plans hardcoded types.

### 2. Error Handling Overhead

*   **The Smell:** The spec claims "Native Performance" but admits a "~5-10% overhead" for mandatory stack trace capture on errors.
*   **The Reality:** Capturing stack traces is extremely expensive in high-throughput systems (10x-100x slower than the operation itself).
*   **Proposal: Lazy Symbol Resolution + PC Capture**
    1.  **At Runtime (Fast):** Capture *only* the Program Counter and Stack Pointer (copying a few integers — nanoseconds).
    2.  **At Print Time (Slow):** Resolve pointers to File/Line/Function strings only when `.stack_trace()` is called or the program panics.
    3.  **Production:** Provide `--strip-debug` compiler flag to disable entirely.
*   **Lightweight errors:** Add a `#[no_trace]` attribute for errors used as control flow (e.g., `EndOfFile`).

### 3. Specification Holes

*   **`impl` Blocks for non-Trait methods:** The spec documents inherent `impl` blocks for enums (`impl EnumName:`, §4.6) but not for structs, while the roadmap shows `impl Rectangle: ...`.
    *   *The Fix:* Document inherent struct implementations explicitly in Section 4.5.

### 4. Panic During Drop

*   If a `.drop()` panics while unwinding from another panic, undefined behavior.
*   **Proposal: Immediate Abort.** Document clearly.
*   **Status:** Open — prerequisite is Milestone 23 (RAII & Drop). Partially dissolved: panics abort immediately without unwinding and run no cleanup (§7 Panic Behavior), so a `drop()` never runs *during* a panic — but a `drop()` that itself panics still needs a documented rule.

### 5. Safe `continue` (Zig inspiration)

*   **The Smell:** In `while` loops, using `continue` skips the rest of the block. If manual counter increments are placed there, it results in an accidental infinite loop. Zig solves this with `while (cond) : (continue_expr)`.
*   **Proposal:** Since Ryo uses Pythonic syntax, adding `:(expr)` breaks aesthetic coherence. However, we could introduce a `defer` statement or explore a block-level `continue` hook to guarantee increment execution. For now, rely on `ForRange` loops where `continue` safely jumps to the builtin increment block.

---

## Recommendations

### Spec Completeness

- Document inherent `impl` blocks for structs (Specification Holes, above).
- Add a "post-v0.1" note to Section 9 (Concurrency).

### Open Decisions

- Document drop-panic abort behavior (Panic During Drop — blocked on M23).
- Track Safe `continue` (Zig inspiration).

### Checklist

- [ ] Document inherent `impl` blocks for structs (spec §4.5 gap)
- [ ] Document drop-panic abort behavior (M23)
- [ ] Track Safe `continue` (Zig inspiration)

## References

- Spec: `docs/specification.md` (each resolved issue lands in its relevant section)
- Roadmap: `docs/dev/implementation_roadmap.md`
