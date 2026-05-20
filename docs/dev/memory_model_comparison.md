**Status:** Reference

# Memory Model Comparison: Rust, Mojo, Swift, Ryo

A dimension-by-dimension comparison of the four languages' memory models. Useful as a quick lookup when explaining Ryo's choices to someone familiar with one of the others, or when evaluating whether to adopt a particular pattern from a sibling language.

Companion to [`rust_reference.md`](rust_reference.md), [`mojo_reference.md`](mojo_reference.md), and [`arc_optimizer.md`](arc_optimizer.md). Those go deeper on individual axes; this file is the cross-cutting summary.

## 1. Default value semantics

| Aspect | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **Default for assignment** | Move (non-Copy) | Move (non-Copyable) | Copy for structs / reference for classes | Move (non-Copy) |
| **What makes something Copy** | `Copy` trait (marker, opt-in) | `Copyable` trait | Value types are copies; classes are refs | Compiler-known (`int`/`float`/`bool`); traits in v0.2 |
| **Source after move** | Uninitialized | Uninitialized | Original retains ref (separate handle); `consuming` invalidates | `Moved` (compile error on use) |
| **Implicit copies in user code** | Only for `Copy` types | Only for `Copyable` types | All value types; never for classes | Only Copy types |

## 2. Parameter conventions

| Convention | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **Default** | Move (consumes) | `borrowed` (read-only) | `borrowing` (read-only, 5.9+) | Implicit immutable borrow |
| **Immutable borrow** | `&T` (explicit) | `borrowed x: T` | `borrowing x: T` | `x: T` (no annotation) |
| **Mutable borrow** | `&mut T` | `inout x: T` | `inout x: T` | `inout x: T` |
| **Take ownership** | `T` (default) | `owned x: T` | `consuming x: T` | `move x: T` |
| **Call-site marking** | `&x` / `&mut x` | bare for owned, `^x` for consuming | bare | bare (default) / `&x` for `inout` |
| **Generic-over-lifetimes** | Yes (`fn f<'a>(x: &'a T)`) | Yes (origins, inferred) | No | No (Rules 5/6 forbid) |

## 3. Destruction timing

| Aspect | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **When** | Scope end (lexical) | Last use (ASAP) | Last strong ref drops | Last use for memory; scope end for resources (hybrid) |
| **Order in scope** | Reverse declaration | Use-order driven | Refcount-driven | Use-order for memory; reverse-decl for resources |
| **User hook** | `impl Drop for T` | `fn __del__(owned self)` | `deinit { }` | `fn drop(self)` (M23) |
| **Surprises** | `MutexGuard` held until `}` | Eager, predictable | Eager, predictable | Eager for memory; lexical for `with`-blocks |
| **NLL / early release** | Borrows release early (NLL); destructor doesn't | N/A (already eager) | N/A | N/A (already eager) |

## 4. References and lifetimes

| Aspect | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **Reference type** | `&T`, `&mut T` (first-class) | `Reference[T, Origin]` (parametric) | Class instances are refs; `Pointer<T>` for value types | Parameter convention only (no ref type) |
| **Storeable in variable?** | Yes | Yes | Yes (classes) | No |
| **Storeable in struct field?** | Yes (with lifetimes) | Yes (with origins) | Yes (classes) | No (Rule 6) |
| **Returnable from function?** | Yes (with lifetimes) | Yes (with origins) | Yes (classes always) | No (Rule 5) |
| **Lifetime annotations** | `'a` (often inferred, sometimes explicit) | `Origin` (always inferred) | None (refcount manages) | None |
| **Iterator/view escape** | Allowed (`impl Iterator<'_>`) | Allowed (origin-checked) | Allowed (closures capture) | Forbidden — scope-locked (5.7) |

## 5. Shared ownership

| Aspect | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **Primitive** | `Rc<T>` / `Arc<T>` | `ArcPointer[T]` / `OwnedPointer[T]` | Implicit for classes | `shared[T]` |
| **Atomic by default** | `Rc` no, `Arc` yes | Yes | Yes | Yes |
| **Non-atomic variant** | `Rc<T>` available | None | Removed in modern Swift | Possibly future `rc[T]`; deferred |
| **Refcount op visibility** | Explicit `.clone()` | Explicit | **Implicit on assignment** | **Implicit on assignment** (Swift model) |
| **Compiler elision quality** | Limited (LLVM does some) | Limited | **Aggressive (SIL ARC optimizer)** | **Planned (`arc_optimizer.rs`)** |
| **Cycle handling** | `Weak<T>` (explicit) | `WeakPointer[T]` | `weak` / `unowned` qualifiers | `weak[T]` / `unowned[T]` |
| **Cycle collection** | No (leaks) | No (leaks) | No (leaks) | No (leaks; documented as foot-gun) |
| **Stack promotion** | No | Limited | Yes (escape-analyzed) | Planned via `arc_optimizer.rs` |
| **COW collections** | No (`Vec` is owned, slices are borrows) | Limited | Yes (`Array`/`Dictionary`/`String`) | Yes (planned for `list`/`map`/`str`, M22) |

## 6. Allocation strategy

| Aspect | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **Stack by default** | Structs, enums, primitives | Structs, primitives | Value types only | Primitives, structs |
| **Heap by default** | Nothing (manual `Box`) | Nothing (manual) | All class instances | Owned heap types (`str`, `list`); structs are stack |
| **Manual heap escape** | `Box<T>` | `OwnedPointer[T]` | N/A (just declare `class`) | `shared[T]` |
| **Fat pointer for owned-heap** | `Vec<T>` = ptr+len+cap | Equivalent | Wrapped in class | `str`/`list` = ptr+len+cap |
| **Slice / borrowed view** | `&[T]`, `&str` | `Span[T]` | `ArraySlice` (refcounted) | Scope-locked view (M8.4) |
| **Small-value optimization** | Stdlib SSO in `compact_str` etc. | TBD | Limited | Planned (small-string opt) |

## 7. Aliasing rules and safety

| Aspect | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **Aliasing model** | Stacked Borrows / Tree Borrows (formal) | Origin + lifetime check (less formal) | Law of Exclusivity for `inout` | OwnershipPass dataflow (intra-procedural) |
| **Mutable XOR shared** | Strictly enforced | Strictly enforced | Enforced for `inout`; classes are co-mutable | Enforced for `inout` (M8.3) |
| **Use after move** | Compile error | Compile error | Compile error for `consuming` | Compile error (M8.1) |
| **Use after free** | Compile error | Compile error | Impossible (ARC always holds) | Compile error for owned; impossible for shared |
| **Iterator invalidation** | Compile-time (borrow conflict) | Compile-time | Runtime trap (COW copy) | Compile-time (view escape rule) |
| **Data race freedom** | `Send`/`Sync` auto traits | Lifetime-based | `Sendable` protocol + actors | Channels + `shared[mutex[T]]` (concurrency.md) |
| **Unsafe escape hatch** | `unsafe { }` (always available) | `__mlir_op[…]` for stdlib only | `withUnsafePointer`, `unowned(unsafe)` | `unsafe` (system packages only) |

## 8. Concurrency model

| Aspect | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **Primary primitive** | `std::thread`, `tokio::spawn` | Tasks, channels | Actors, structured concurrency | Tasks + channels (see concurrency.md) |
| **Auto traits for safety** | `Send` + `Sync` | TBD | `Sendable` (compile-checked) | TBD |
| **Cross-thread move** | Requires `Send` | TBD | `sending` parameters (5.9+) | Channel send |
| **Cross-thread share** | `Arc<T>` (+ `Sync` for refs) | `ArcPointer[T]` | Class refs + `@unchecked Sendable` | `shared[T]` (always atomic) |
| **Actor isolation** | Manual | Manual | First-class `actor` keyword | TBD (Erlang-style proposed) |
| **Async runtime** | Pluggable (tokio, async-std) | First-party | First-party | Go-style (concurrency.md) |

## 9. String and collection representation

| Aspect | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **Owned string** | `String` (`Vec<u8>`) | `String` | `String` (COW class) | `str` (fat ptr, M8.1) |
| **Borrowed string** | `&str` (slice) | `StringSlice` | `Substring` (refcount-shared) | scope-locked `&str` (M8.4) |
| **Literal type** | `&'static str` | `StringLiteral` | `StaticString` | `.rodata` + `ryo_str_alloc` at use |
| **List type** | `Vec<T>` (owned, contig) | `List[T]` | `Array<T>` (COW class) | `list[T]` (planned COW, M22) |
| **Inline storage** | No SSO in std; ecosystem crates | TBD | SSO for `String` | Planned SSO |
| **Slice type** | `&[T]` | `Span[T]` | `ArraySlice` | scope-locked view (M8.4+) |

## 10. Where lifetime / ownership errors come from

| Error class | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **Use after move** | `error[E0382]` (borrowck) | `CheckLifetime` pass | Compile error (`consuming` use) | `DiagCode::UseAfterMove` (ownership.rs) |
| **Borrow outlives owner** | `error[E0597]` | Origin error | N/A (refcount holds) | Forbidden by construction (Rules 5/6) |
| **Conflicting borrows** | `error[E0502]` | Conflict error | Exclusivity violation | `DiagCode::AliasingViolation` (M8.3) |
| **Cycle leaks** | Silent (runtime memory growth) | Silent | Silent | Silent (documented) |
| **Forgotten retain/release** | N/A (manual `clone`) | N/A (compiler manages) | N/A (compiler manages) | N/A (compiler manages) |
| **Lifetime annotation needed** | `error[E0106]` "missing lifetime" | Never (inferred) | Never (no lifetimes) | Never (no lifetimes) |

## 11. Pipeline position of the ownership check

| Stage | Rust | Mojo | Swift | Ryo |
|---|---|---|---|---|
| **High-level IR** | HIR | (direct to MLIR) | AST + SIL | UIR |
| **Typecheck on** | HIR | MLIR (`kgen` + `pop`) | SIL passes | UIR → TIR |
| **Mid-level IR** | THIR → MIR | MLIR | SIL | TIR |
| **Ownership / borrow check** | `rustc_borrowck` (NLL + Polonius) on MIR | `CheckLifetime` on MLIR | Exclusivity + ARC check on SIL | `OwnershipPass` on TIR |
| **ARC elision** | LLVM `ObjCArcOpts` (limited) | (limited) | SIL `ARCOptimizer` (aggressive) | `arc_optimizer.rs` (planned) |
| **Native backend** | LLVM | LLVM | LLVM | Cranelift |
| **WASM backend** | LLVM (`wasm32-*`) | Not yet supported | LLVM (`swiftwasm` fork) | None today — Cranelift does not emit WASM. Future work would build a parallel `wasm-encoder` backend — see [proposals/wasm_target.md](proposals/wasm_target.md). |
| **WASM runtime model** | Rust std on `wasm32-wasi` uses wasi-libc | N/A | Swift runtime ported (large) | TBD — earlier "Go-style bundled `dlmalloc-rs` + direct WASI imports" plan was tied to the dropped Cranelift-emits-WASM design; the runtime model will be revisited if/when the second backend is funded. |

## Philosophical positioning

| | What it optimizes for | What it gives up |
|---|---|---|
| **Rust** | Predictable zero-cost performance, no GC, fearless concurrency | Annotation overhead, expert mental model, smaller audience |
| **Mojo** | Python ergonomics + AI/GPU performance, gradual systems escape | Closed-source compiler, narrow audience (AI/HPC) |
| **Swift** | App-developer ergonomics with predictable performance via implicit ARC | Some performance variance vs Rust; mutable shared state via class refs is easy to misuse |
| **Ryo** | Python syntax + memory safety + no lifetime annotations | Cannot express some Rust patterns (refs in structs, returning slices); refcount cost when elision fails |

## TL;DR axes

- **Move-by-default for assignment:** Rust, Mojo, Ryo. Swift uses copy for value types and reference for classes.
- **Borrow-by-default for parameters:** Mojo, Swift (5.9+), Ryo. Rust requires explicit `&`.
- **ASAP destruction:** Mojo, Ryo (memory types), Swift (effectively, via last-ref-drop). Rust uses lexical scope.
- **Implicit refcount ops:** Swift, Ryo (planned). Rust and Mojo make `.clone()` explicit.
- **No lifetime annotations:** Swift, Ryo. Rust requires them sometimes. Mojo always infers.
- **First-class reference types:** Rust, Mojo, Swift. Ryo has no reference *type* — only conventions.
- **First-class actor isolation:** Swift only. Others have manual or proposed concurrency models.

## Clustering

The four languages cluster like this:

- **Rust** is the outlier: most expressive references, most explicit lifetime story, manual refcount.
- **Mojo and Ryo** sit in the middle: borrow-by-default parameters, move-by-default assignment, post-typecheck ownership pass, ASAP destruction.
- **Swift** is the other outlier: implicit refcount everywhere (classes), ASAP-ish via refcount, no first-class lifetimes, actors for concurrency.

Ryo deliberately picks: Mojo's argument conventions, Mojo's ASAP destruction, Swift's implicit refcount and elision, and (uniquely among the four) the Rules 5/6 stance that refs cannot escape function scope — which is what eliminates lifetime annotations entirely. The cost is reduced expressiveness vs Rust; the win is the smallest possible mental model for memory safety.

## References

- Spec: `docs/specification.md` Section 5 (Memory Management)
- Dev: `docs/dev/rust_reference.md` (deeper Rust comparison)
- Dev: `docs/dev/mojo_reference.md` (Mojo as ownership-pass inspiration)
- Dev: `docs/dev/arc_optimizer.md` (Swift-style refcount elision design)
- Dev: `docs/dev/borrow_checker.md` (Ryo's algorithm sketch)
- Dev: `docs/dev/concurrency.md` (Ryo's concurrency model)
- Dev: `docs/dev/proposals/wasm_target.md` (WASM target — deferred proposal)
- Milestone: `docs/dev/implementation_roadmap.md`
- Upstream: <https://github.com/rust-lang/rust>, <https://github.com/modular/modular/tree/main/mojo>, <https://github.com/swiftlang/swift>
