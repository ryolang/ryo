# The Zen of Ryo

Mon 17 Nov 2025

## I. Clarity and Simplicity

1.  **Clarity is better than cleverness.** Code should be easy to read, even if it is not the most compact.
2.  **Explicit is better than implicit, but simple is better than verbose.**
3.  **Consistency trumps convenience.** A single, predictable way to do things is a simple language rule.
4.  **Clarity of intent is paramount.** If the code mutates, borrows, or transfers ownership, the keywords must show it.
5.  **Complexity should not be hidden.** When complexity is necessary (e.g., FFI, shared state), it must be explicitly marked.

## II. Safety and Correctness

6.  **Safety is not optional.** Memory safety and null safety must be enforced at compile time, not runtime.
7.  **Errors are values, not exceptions.** Failures are explicit branches of code flow, not hidden control flow.
8.  **The default is the safe path.** Immutability is the default; mutability requires intent.
9.  **Avoid hidden state.** The flow of data and resources must be visible and predictable.

## III. Developer Experience

10. **Productivity over perfection.** Ryo is built for the 95% of applications where developer time is more costly than micro-optimizations.
11. **Debugging should be effortless.** The compiler should generate rich, automatic debugging information (stack traces, error chains) at a small, acceptable runtime cost.
12. **The dot operator is for behavior.** Use methods for type behavior, but use functions for universal utilities (e.g., `len(list)`).
13. **Tools should enable, not hinder.** The compiler and tooling should be fast, clean, and helpful, not a burden.
14. **Concurrency is not a color.** The synchronous/asynchronous divide must be managed by the runtime, not by the function signature.

---
