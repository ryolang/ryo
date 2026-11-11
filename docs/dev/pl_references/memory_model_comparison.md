**Status:** Reference

# Memory Model Comparison: Rust, Mojo, Hylo, Swift, Ryo

A dimension-by-dimension comparison of the five languages' memory models. Useful as a quick lookup when explaining Ryo's choices to someone familiar with one of the others, or when evaluating whether to adopt a particular pattern from a sibling language.

Companion to [`rust.md`](rust.md), [`mojo.md`](mojo.md), [`hylo.md`](hylo.md), [`swift.md`](swift.md), and [`spec_arc_optimizer.md`](../spec_arc_optimizer.md). Those go deeper on individual axes; this file is the cross-cutting summary. Swift coverage reflects Swift 6.4 (2026-09-15). Hylo coverage uses the current public specification and the 2025 graph-representation paper cited below.

## 1. Default value semantics

| Aspect | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **Default for assignment** | Move (non-Copy) | Move (non-Copyable) | Value semantics; `sink` consumes, `Copyable` types copy | Copy for structs / reference for classes | Move (non-Copy) |
| **What makes something Copy** | `Copy` trait (marker, opt-in) | `Copyable` trait | `Copyable` conformance; `.copy()` is explicit | Value types are copies; classes are refs | Compiler-known (`int`/`float`/`bool`); traits in v0.2 |
| **Source after move** | Uninitialized | Uninitialized | Dead after `sink`; mutable bindings can be reinitialized | Original retains ref (separate handle); `consuming` invalidates | `Moved` (compile error on use) |
| **Implicit copies in user code** | Only for `Copy` types | Only for `Copyable` types | None; `.copy()` is explicit | All value types; never for classes | Only Copy types |

## 2. Parameter conventions

| Convention | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **Default** | Move (consumes) | `borrowed` (read-only) | `let` (read-only) | `borrowing` (read-only, 5.9+) | Implicit immutable borrow |
| **Immutable borrow** | `&T` (explicit) | `borrowed x: T` | `let x: T` | `borrowing x: T` | `x: T` (no annotation) |
| **Mutable borrow** | `&mut T` | `inout x: T` | `inout x: T` | `inout x: T` | `inout x: T` |
| **Take ownership** | `T` (default) | `owned x: T` | `sink x: T` | `consuming x: T` | `move x: T` |
| **Call-site marking** | `&x` / `&mut x` | bare for owned, `^x` for consuming | `&x` for `inout`; bare for `sink` | bare | bare (default) / `&x` for `inout` |
| **Borrow dependency annotations** | Named lifetimes such as `'a` | Origins, inferred | None; subscripts yield dependent projections | `@_lifetime` for nonescapable values; no named lifetime parameters | None (Rules 5/6 forbid escaping borrows) |

## 3. Destruction timing

| Aspect | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **When** | Scope end (lexical) | Last use (ASAP) | Last use or consuming `sink` operation | Last strong ref drops | Last use for memory; scope end for resources (hybrid) |
| **Order in scope** | Reverse declaration | Use-order driven | Use-order and consumption driven | Refcount-driven | Use-order for memory; reverse-decl for resources |
| **User hook** | `impl Drop for T` | `fn __del__(owned self)` | `deinit` and consuming `sink` methods | `deinit { }` | `fn drop(self)` (M26) |
| **Surprises** | `MutexGuard` held until `}` | Eager, predictable | Projection lifetimes protect their owners | Eager, predictable | Eager for memory; lexical for `with`-blocks |
| **NLL / early release** | Borrows release early (NLL); destructor doesn't | N/A (already eager) | Built into binding and projection lifetimes | N/A | N/A (already eager) |

## 4. References and lifetimes

| Aspect | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **Reference type** | `&T`, `&mut T` (first-class) | `Reference[T, Origin]` (parametric) | Projections, including stored `[let T]` / `[inout T]` forms | Class instances are shared refs; `Ref`/`MutableRef` are nonescapable borrows | Parameter convention only; views are a narrow exception |
| **Storeable in variable?** | Yes | Yes | Yes | Yes | Borrows no; scope-locked views yes |
| **Storeable in struct field?** | Yes (with lifetimes) | Yes (with origins) | Yes, as captured projections with owner dependencies | Class refs yes; `Ref`/`MutableRef` only in `~Escapable` types | No (Rule 6), except retaining views |
| **Returnable from function?** | Yes (with lifetimes) | Yes (with origins) | Subscripts yield projections; functions return independent values | Class refs yes; dependent borrows with `@_lifetime` | No (Rule 5), except method and retaining views |
| **Lifetime annotations** | `'a` (often inferred, sometimes explicit) | `Origin` (always inferred) | None; projection dependencies are inferred | `@_lifetime` dependencies for nonescapable values; ARC classes need none | None |
| **Iterator/view escape** | Allowed (`impl Iterator<'_>`) | Allowed (origin-checked) | Bounded by the projected owner; stored captures are supported | Class-backed values escape; nonescapable views follow dependencies | Forbidden — scope-locked (5.7), except retaining views |

Swift 6.4's `Ref` and `MutableRef` do not make general self-referential owning structs safe. They refer to a separately borrowed target. A type that stores these references must itself be nonescapable. This differs from an aggregate that owns a field and stores a reference into that same field.

Hylo's captured projections also describe dependencies on existing external objects. They do not provide a documented `pin_init`-style mechanism for an aggregate to own an inline field and a projection into that same field. Hylo has limited support for independent lifetimes within aggregates. Its graph guidance instead uses whole-value ownership and IDs or offsets for peer relationships.

## 5. Shared ownership

| Aspect | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **Primitive** | `Rc<T>` / `Arc<T>` | `ArcPointer[T]` / `OwnedPointer[T]` | Unique whole/part ownership; no core ARC | Implicit for classes | `shared[T]` |
| **Atomic by default** | `Rc` no, `Arc` yes | Yes | N/A | Yes | Yes |
| **Non-atomic variant** | `Rc<T>` available | None | N/A | Removed in modern Swift | Possibly future `rc[T]`; deferred |
| **Refcount op visibility** | Explicit `.clone()` | Explicit | N/A | **Implicit on assignment** | **Implicit on assignment** (Swift model) |
| **Compiler elision quality** | Limited (LLVM does some) | Limited | N/A | **Aggressive (SIL ARC optimizer)** | **Planned (`arc_optimizer.rs`)** |
| **Cycle handling** | `Weak<T>` (explicit) | `WeakPointer[T]` | IDs or offsets for peer relationships | `weak` / `unowned` qualifiers | `weak[T]` / `unowned[T]` |
| **Cycle collection** | No (leaks) | No (leaks) | Not needed for ID/offset graphs; no core ARC cycles | No (leaks) | No (leaks; documented as foot-gun) |
| **Stack promotion** | No | Limited | N/A | Yes (escape-analyzed) | Planned via `arc_optimizer.rs` |
| **COW collections** | No (`Vec` is owned, slices are borrows) | Limited | Not the core model; copies are explicit | Yes (`Array`/`Dictionary`/`String`) | Yes (planned for `list`/`map`/`str`; deferred from M25 to v0.2/Phase 5 — v0.1 ships eager `clone()`) |

## 6. Allocation strategy

| Aspect | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **Stack by default** | Structs, enums, primitives | Structs, primitives | Values and product types | Value types only | Primitives, structs |
| **Heap by default** | Nothing (manual `Box`) | Nothing (manual) | Nothing implicit in the core model | All class instances | Owned heap types (`str`, `list`); structs are stack |
| **Manual heap escape** | `Box<T>` | `OwnedPointer[T]` | `indirect` types; low-level dynamic allocation builtins | `UniqueBox` (unique, 6.4) or a class (ARC) | `shared[T]` |
| **Fat pointer for owned-heap** | `Vec<T>` = ptr+len+cap | Equivalent | Not part of the language contract | Wrapped in class | `str`/`list` = ptr+len+cap |
| **Slice / borrowed view** | `&[T]`, `&str` | `Span[T]` | Projection of part of a collection | Nonescapable `Span`; `ArraySlice` retains shared storage | Scope-locked view (M8.4) |
| **Small-value optimization** | Stdlib SSO in `compact_str` etc. | TBD | Implementation-specific | Limited | Planned (small-string opt) |

## 7. Aliasing rules and safety

| Aspect | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **Aliasing model** | Stacked Borrows / Tree Borrows (formal) | Origin + lifetime check (less formal) | Mutable value semantics and projections | Law of Exclusivity for `inout` | OwnershipPass dataflow (intra-procedural) |
| **Mutable XOR shared** | Strictly enforced | Strictly enforced | Strictly enforced; mutable projections are exclusive | Enforced for `inout`; classes are co-mutable | Enforced for `inout` (M8.3) |
| **Use after move** | Compile error | Compile error | Compile error after `sink` | Compile error for `consuming` | Compile error (M8.1) |
| **Use after free** | Compile error | Compile error | Compile error through value and projection lifetimes | Impossible (ARC always holds) | Compile error for owned; impossible for shared |
| **Iterator invalidation** | Compile-time (borrow conflict) | Compile-time | Owner frozen or inaccessible during projection | Runtime trap (COW copy) | Compile-time (view escape rule) |
| **Data race freedom** | `Send`/`Sync` auto traits | Lifetime-based | Value independence and exclusive mutation | `Sendable` protocol + actors | Channels + `shared[mutex[T]]` (spec_concurrency.md) |
| **Unsafe escape hatch** | `unsafe { }` (always available) | `__mlir_op[…]` for stdlib only | Explicit, auditable opt-in | `withUnsafePointer`, `unowned(unsafe)` | `unsafe` (system packages only) |

## 8. Concurrency model

| Aspect | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **Primary primitive** | `std::thread`, `tokio::spawn` | Tasks, channels | Independent values and `async` expressions | Actors, structured concurrency | Tasks + channels (see spec_concurrency.md) |
| **Auto traits for safety** | `Send` + `Sync` | TBD | Value independence rather than shared-alias analysis | `Sendable` (compile-checked) | TBD |
| **Cross-thread move** | Requires `Send` | TBD | `sink` transfers independent values | `sending` parameters (5.9+) | Channel send |
| **Cross-thread share** | `Arc<T>` (+ `Sync` for refs) | `ArcPointer[T]` | Immutable independent values; no shared mutable aliases | Class refs + `@unchecked Sendable` | `shared[T]` (always atomic) |
| **Actor isolation** | Manual | Manual | Not central to the model | First-class `actor` keyword | TBD (Erlang-style proposed) |
| **Async runtime** | Pluggable (tokio, async-std) | First-party | Language-level `async`; implementation evolving | First-party | Go-style (spec_concurrency.md) |

## 9. String and collection representation

| Aspect | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **Owned string** | `String` (`Vec<u8>`) | `String` | `String` value type | `String` (value type with COW storage) | `str` (fat ptr, M8.1) |
| **Borrowed string** | `&str` (slice) | `StringSlice` | Projection of string storage | `Substring` (refcount-shared) | scope-locked `&str` (M8.4) |
| **Literal type** | `&'static str` | `StringLiteral` | Value literal; no ARC requirement | `StaticString` | `.rodata` + `ryo_str_alloc` at use |
| **List type** | `Vec<T>` (owned, contig) | `List[T]` | Array value type with unique ownership | `Array<T>` (COW); `UniqueArray` (no COW, 6.4) | `list[T]` (eager `clone()` in M25; COW deferred to v0.2) |
| **Inline storage** | No SSO in std; ecosystem crates | TBD | Value types and inline product storage | `InlineArray`; SSO for `String` | Planned SSO |
| **Slice type** | `&[T]` | `Span[T]` | Collection projection | `ArraySlice`; nonescapable `Span` | scope-locked view (M8.4+) |

## 10. Where lifetime / ownership errors come from

| Error class | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **Use after move** | `error[E0382]` (borrowck) | `CheckLifetime` pass | Compile error after `sink` | Compile error (`consuming` use) | `DiagCode::UseAfterMove` (ownership.rs) |
| **Borrow outlives owner** | `error[E0597]` | Origin error | Projection escape error | Lifetime-dependency error for nonescapable values; ARC handles class refs | Forbidden by construction (Rules 5/6) |
| **Conflicting borrows** | `error[E0502]` | Conflict error | Overlapping `inout` or projection error | Exclusivity violation | `DiagCode::MutableAliasingViolation` (E0032, M8.3) |
| **Cycle leaks** | Silent (runtime memory growth) | Silent | Not applicable to ID/offset graphs | Silent | Silent (documented) |
| **Forgotten retain/release** | N/A (manual `clone`) | N/A (compiler manages) | N/A (no core ARC) | N/A (compiler manages) | N/A (compiler manages) |
| **Lifetime annotation needed** | `error[E0106]` "missing lifetime" | Never (inferred) | Never; projections replace named lifetimes | Required for some returned nonescapable dependencies | Never (no escaping borrows) |

## 11. Pipeline position of the ownership check

| Stage | Rust | Mojo | Hylo | Swift | Ryo |
|---|---|---|---|---|---|
| **High-level IR** | HIR | (direct to MLIR) | Typed AST and Hylo IR | AST + SIL | UIR |
| **Typecheck on** | HIR | MLIR (`kgen` + `pop`) | Hylo's generic type system | SIL passes | UIR → TIR |
| **Mid-level IR** | THIR → MIR | MLIR | Hylo IR | SIL | TIR |
| **Ownership / borrow check** | `rustc_borrowck` (NLL + Polonius) on MIR | `CheckLifetime` on MLIR | Mandatory value-semantics and projection analysis on Hylo IR | Exclusivity + ARC check on SIL | `OwnershipPass` on TIR |
| **ARC elision** | LLVM `ObjCArcOpts` (limited) | (limited) | N/A | SIL `ARCOptimizer` (aggressive) | `arc_optimizer.rs` (planned) |
| **Native backend** | LLVM | LLVM | LLVM | LLVM | Cranelift |
| **WASM backend** | LLVM (`wasm32-*`) | Not yet supported | Not established in the cited sources | LLVM; official Wasm SDK in 6.4 | None today — Cranelift does not emit WASM. Future work would build a parallel `wasm-encoder` backend — see [proposal_wasm_target.md](../proposal_wasm_target.md). |
| **WASM runtime model** | Rust std on `wasm32-wasi` uses wasi-libc | N/A | Not established in the cited sources | Swift runtime ported (large) | TBD — earlier "Go-style bundled `dlmalloc-rs` + direct WASI imports" plan was tied to the dropped Cranelift-emits-WASM design; the runtime model will be revisited if/when the second backend is funded. |

## Philosophical positioning

| | What it optimizes for | What it gives up |
|---|---|---|
| **Rust** | Predictable zero-cost performance, no GC, fearless concurrency | Annotation overhead, expert mental model, smaller audience |
| **Mojo** | Python ergonomics + AI/GPU performance, gradual systems escape | Closed-source compiler, narrow audience (AI/HPC) |
| **Hylo** | Generic systems code with mutable value semantics | General stored references and independent aggregate lifetimes |
| **Swift** | App-developer ergonomics, ARC, and expressive value ownership | ARC costs plus noncopyable/nonescapable type-system complexity |
| **Ryo** | Python syntax + memory safety + no lifetime annotations | Cannot express some Rust/Hylo/Swift patterns (stored dependent refs, returned nonescapable views); refcount cost when elision fails |

## TL;DR axes

- **Move-by-default for assignment:** Rust, Mojo, Hylo (through `sink`), Ryo. Swift uses copy for value types and reference for classes.
- **Borrow-by-default for parameters:** Mojo, Hylo, Swift (5.9+), Ryo. Rust requires explicit `&`.
- **ASAP destruction:** Mojo, Hylo, Ryo (memory types), Swift (effectively, via last-ref-drop). Rust uses lexical scope.
- **Implicit refcount ops:** Swift, Ryo (planned). Rust and Mojo make `.clone()` explicit. Hylo has no core ARC model.
- **Lifetime dependency surface:** Rust has named lifetimes, Mojo has inferred origins, Hylo has inferred projection dependencies, and Swift has `@_lifetime` dependencies for nonescapable values. Ryo forbids escaping borrows instead.
- **First-class reference types:** Rust and Mojo. Hylo uses projections. Swift has class references and nonescapable `Ref`/`MutableRef`. Ryo has parameter conventions and restricted views.
- **Unique heap ownership:** Rust `Box`, Mojo `OwnedPointer`, Hylo `indirect` and dynamic allocation, and Swift 6.4 `UniqueBox`. Ryo has no general unique-box type.
- **First-class actor isolation:** Swift only. Others have manual, proposed, or non-actor concurrency models.

## Clustering

The five languages cluster like this:

- **Rust** is the outlier: most expressive references, most explicit lifetime story, manual refcount.
- **Mojo, Hylo, and Ryo** share value independence, borrow-by-default parameters, consuming ownership transfers, and eager destruction. Hylo adds yielding projections rather than general stored references.
- **Swift** is the other outlier: implicit ARC for classes, unique and noncopyable ownership, nonescapable lifetime-dependent references, and actors for concurrency.

Ryo deliberately picks: Mojo's argument conventions and ASAP destruction, Hylo's owner-protected projection model, and Swift's implicit refcount and elision. Its Rules 5/6 stance is narrower than Hylo's stored captures: general Ryo refs cannot escape function scope, while persistent views need a `shared[T]` retain. The cost is reduced expressiveness; the win is a small mental model for memory safety.

## References

- Spec: `docs/specification.md` Section 5 (Memory Management)
- Dev: `docs/dev/pl_references/rust.md` (deeper Rust comparison)
- Dev: `docs/dev/pl_references/mojo.md` (Mojo as ownership-pass inspiration)
- Dev: `docs/dev/pl_references/hylo.md` (Hylo projections and value semantics)
- Dev: `docs/dev/spec_arc_optimizer.md` (Swift-style refcount elision design)
- Dev: `ryo-frontend/src/ownership/mod.rs` (the shipped ownership pass); Spec: `docs/specification.md` §5 (Memory Management)
- Dev: `docs/dev/spec_concurrency.md` (Ryo's concurrency model)
- Dev: `docs/dev/proposal_wasm_target.md` (WASM target — deferred proposal)
- Milestone: `docs/dev/implementation_roadmap.md`
- Upstream: <https://github.com/rust-lang/rust>, <https://github.com/modular/modular/tree/main/mojo>, <https://github.com/hylo-lang/hylo>, <https://github.com/swiftlang/swift>
- Hylo: [Introduction](https://hylo-lang.org/introduction/), [subscripts](https://hylo-lang.org/docs/user/language-tour/subscripts/), [specification](https://github.com/hylo-lang/specification/blob/main/spec.md), [Who Owns the Contents of a Doubly-Linked List?](https://drops.dagstuhl.de/entities/document/10.4230/OASIcs.Programming.2025.25)
- Swift: [Swift 6.4 release](https://www.swift.org/blog/swift-6.4-released/), [SE-0517 `UniqueBox`](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0517-uniquebox.md), [SE-0519 `Ref` and `MutableRef`](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0519-ref-mutableref-types.md)
