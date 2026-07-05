**Status:** Reference

# Rust Borrow Checker Reference (Comparison)

A focused comparison of Ryo's M8.1 ownership pass against Rust's MIR borrowck. Rust is the obvious reference point — its borrow checker is the most influential one in production — so understanding where Ryo deliberately echoes it and where it diverges is a design-record worth keeping.

This file mirrors the role of [`mojo_reference.md`](mojo_reference.md): part external-ecosystem overview, part Ryo-specific decision log. Mojo is the primary algorithmic inspiration for `ryo-frontend/src/ownership.rs` and `ryo-core/src/ownership.rs`; Rust is the bar for diagnostic UX and the well-trodden reference for "what a borrow checker does."

[View rustc on GitHub](https://github.com/rust-lang/rust) — Rust's compiler is fully open source; the borrow-check sources live in `compiler/rustc_borrowck/` and `compiler/rustc_mir_dataflow/`.

## Pipeline position (where Rust and Ryo align)

| Stage | Rust | Ryo (M8.1+) |
|---|---|---|
| High-level IR | HIR | (none — Ryo skips this) |
| Typecheck | typeck on HIR | sema on UIR → TIR |
| Mid-level IR | THIR → MIR (CFG with basic blocks) | TIR (flat-arena, per-function) |
| Ownership/borrow check | **borrowck on MIR (NLL)** | **OwnershipPass on TIR** |
| Backend | LLVM | Cranelift |

Architecturally, both run the ownership check as a **post-typecheck pass on a typed mid-level IR**. Same idea as Mojo's `CheckLifetime`. This is the most important structural inheritance.

The IR shapes differ — Rust MIR is an explicit CFG with basic blocks and terminators; Ryo TIR is a flat instruction stream with refs into a per-function arena (more like Zig's AIR). The dataflow analysis itself is largely IR-shape-independent, so both work.

## The big differences

### 1. Destruction timing

| | Drop point |
|---|---|
| **Rust** | Lexical scope end, in reverse declaration order. NLL (Non-Lexical Lifetimes) released *borrows* earlier but the actual `Drop::drop` call still fires at scope close. |
| **Ryo (M8.1)** | ASAP — `Free` inserted at last use of the value, not at scope end. |
| **Mojo** | Same as Ryo (ASAP). |

This is the biggest semantic divergence from Rust. Rust's lexical drop is famously surprising for `MutexGuard` and `RefCell::borrow_mut()` — `let _ = m.lock();` doesn't release the guard until scope end. Ryo (and Mojo) sidestep this by destroying eagerly. Spec Section 5.4 calls it "Hybrid Eager Destruction" because resource types (`File`, `Mutex`) can opt back into scope-end drop via the future `Drop` trait (M23), while pure-memory types like `str` always get ASAP destruction.

### 2. Lifetime / origin handling

| | Reference lifetime |
|---|---|
| **Rust** | Explicit `<'a>` parameters when ambiguous; inferred when single. Functions can return `&'a T`. Structs can hold `&'a T` fields. |
| **Ryo** | **Doesn't exist**. Rule 5 (no returning borrows) and Rule 6 (no struct ref fields) eliminate the need. Inside a function, borrows are just parameter conventions that release on return. |
| **Mojo** | Origins (formerly lifetimes), always inferred, only required when references escape — same use-cases Ryo's Rules 5/6 forbid. |

This is the design choice Ryo's spec leans on hardest. The cost is reduced expressiveness (you cannot write Rust's `fn longest<'a>(a: &'a str, b: &'a str) -> &'a str`); the win is no annotations, ever. M8.1's ownership pass therefore needs **zero region inference** machinery — which is most of what makes Rust's borrowck (and Polonius) complex.

### 3. Parameter conventions

| | Default parameter |
|---|---|
| **Rust** | **Moves** (for non-`Copy`). `fn f(s: String)` consumes `s`. To borrow, you write `&s` at the call site and `&String` in the signature. |
| **Ryo** | **Borrows** (spec Rule 2). `fn f(s: str)` is implicit immutable borrow. To consume, the signature uses `move s: str`. |
| **Mojo** | Same as Ryo — `borrowed` default, `owned` for transfer. |

This inverts Rust's default. The rationale (spec 5.1 "Borrow-by-Default for Functions, Move-by-Default for Assignment") is that ~90% of function calls want a read-only borrow, so making borrow the default removes most of the `&` noise Rust requires.

### 4. Borrows as types vs conventions

| | Borrow expressiveness |
|---|---|
| **Rust** | `&T` is a full type. Variables can hold borrows; structs can have borrow fields; functions can return borrows; iterators yield borrows. |
| **Ryo** | Borrows are **call-site conventions only**. They cannot be stored, returned, or placed in fields. Views/slices in M8.4 are scope-locked. |
| **Mojo** | `Reference[T, Origin]` is a parametric type — closer to Rust than Ryo, but origins are inferred. |

This is where Ryo trades expressiveness for simplicity most aggressively. It's *less* powerful than Rust, deliberately. Rust patterns that need long-lived borrows (e.g., self-referential iterators, parent-child references in trees) become `shared[T]` or owned-with-an-ID in Ryo (spec 5.6).

Ryo's `shared[T]` is modeled on **Swift's class reference semantics**, not Rust's manual `Arc<T>`: refcount ops are implicit on assignment (no `.clone()` in user code), the compiler elides redundant retain/release pairs aggressively, and stdlib collections implement copy-on-write. Reach-for-it cost is closer to Swift than to Rust-with-`Arc`. See [arc_optimizer.md](arc_optimizer.md) for the elision-pass design that makes this performance model real.

### 5. Analysis algorithm

| | Algorithm |
|---|---|
| **Rust** | Originally lexical (AST), then HIR-based, then **MIR + NLL** (Non-Lexical Lifetimes, dataflow). **Polonius** (Datalog-based, experimental) handles more edge cases. Region inference is a constraint-solving problem. |
| **Ryo (M8.1)** | Standard forward dataflow over TIR with lattice `NotTracked ⊑ Valid`, `Valid ⊓ Moved = Moved`. Loop fixed-point typically converges in ≤ 2 iterations. Backward liveness for ASAP `Free` insertion. **No region inference needed** (no lifetimes). |

Because Ryo has no regions/origins, the M8.1 pass is dramatically simpler than `rustc_borrowck`. There's no constraint solver, no region graph, no "this borrow outlives that borrow" analysis. Just per-value Valid/Moved state and liveness. This is the practical payoff of spec Rules 5/6.

When M8.4 adds `&str` slices (scope-locked views), the pass gains a small "view escape" check — but still no inter-procedural region inference, because views can't escape blocks.

### 6. Copy vs Move classification

| | Mechanism |
|---|---|
| **Rust** | `Copy` is a trait. `T: Copy` is a structural property; deriveable. Move semantics applies when `!Copy`. |
| **Ryo (M8.1)** | Compiler-known marker. `is_copy(TypeId)` returns true for `int`/`float`/`bool`. No user-extensibility yet — traits land in v0.2 Phase 5. |
| **Mojo** | Trait-based (`Copyable`, `Movable`, `Defaultable`) — closer to Rust. |

Ryo's choice here is pragmatic — implementing traits before structs is out of order, so M8.1 hardcodes Copy. Once traits land, this becomes a trait.

### 7. Diagnostics

| | DX quality |
|---|---|
| **Rust** | Industry-leading. Multi-label spans, "consider borrowing instead" suggestions, error codes (`E0382`), `rustc --explain` long-form articles, suggestion-driven autofixes. |
| **Ryo (M8.1)** | Target is Rust-parity for the M8.1 surface area — 6 DX criteria (where / what / where-invalidated / which-value / origin / how-to-fix) are spec'd as the acceptance bar. Multi-label via Ariadne. Move-kind-specific help text. No equivalent of `--explain` yet. |
| **Mojo** | Less mature than Rust; we don't have a great public record. |

The M8.1 design explicitly chose Rust's diagnostic surface as the bar, even while taking Mojo's algorithm. That's a deliberate combination — Mojo's algorithm without Rust's error UX would be a regression for users coming from either ecosystem.

## What Ryo deliberately does NOT take from Rust

- **Lifetime annotations** (`<'a>`) — eliminated by Rules 5/6
- **Lexical drop** — replaced by ASAP destruction
- **Move-by-default for parameters** — replaced by borrow-by-default
- **Borrow types as first-class** — only conventions and scope-locked views
- **Two-phase borrows** — not needed without borrow types as variables
- **Polonius / Datalog** — overkill without regions
- **Stacked Borrows / Tree Borrows** (memory model) — Ryo doesn't have `unsafe` for everyday code, so the operational-semantics complexity is mostly avoided
- **`Pin` / self-referential structs** — not addressed; no plans
- **Auto traits (`Send` / `Sync`)** — Ryo's concurrency model is different (see [concurrency.md](concurrency.md))

## What Ryo does take from Rust

- **Use-after-move = compile error** with the same diagnostic shape ("moved here" + "used here")
- **`Copy` as a property of the type** (even if marker-based vs trait-based for now)
- **Post-typecheck IR pass for the borrow check** — same architectural choice as MIR borrowck
- **Multi-label diagnostic rendering** — Ariadne plays the role of Rust's diagnostic framework

## TL;DR positioning

Ryo's ownership story is essentially **Rust's safety story with Mojo's ergonomics** — move semantics and use-after-move enforcement (Rust), without lifetime annotations or borrow types (Mojo's avoidance of explicit regions, taken further by Ryo's Rules 5/6). The M8.1 pass implementation is closer to Mojo's `CheckLifetime` in shape (single pass, ASAP destruction, no region inference) than to Rust's MIR borrowck (multi-phase, lexical drop, region constraint solving) — but both are post-typecheck passes on a typed mid-level IR, which is the architectural piece that matters most for organizing the work.

The cost vs Rust is honest: Ryo is **less expressive**. You can't write some Rust patterns (self-referential structures with borrows, returning slices from functions). The win is no annotations and a much simpler checker — about 1/5 the conceptual surface area of `rustc_borrowck`. For Ryo's target audience (web backends, CLI tools, scripts), the trade is favorable; for systems code Rust covers natively, Ryo deliberately sends you to `shared[T]` or `unsafe` instead.

## Quick lookup table

| Topic | Rust | Mojo | Ryo (M8.1) |
|---|---|---|---|
| Pass position | post-typecheck on MIR | post-typecheck on MLIR | post-typecheck on TIR |
| Pass name | `rustc_borrowck` (NLL/Polonius) | `CheckLifetime` | `OwnershipPass` |
| Drop timing | lexical scope end | ASAP (last use) | ASAP (last use) |
| Lifetime annotations | explicit when ambiguous | inferred origins | none (Rules 5/6 forbid escape) |
| Default parameter | move (if non-Copy) | `borrowed` | implicit immutable borrow |
| Move syntax | implicit on assignment/pass | `owned x: T` | `move x: T` |
| Borrow expressiveness | full first-class types | parametric refs | call-site convention only |
| Copy / Move classification | trait (`Copy`) | trait (`Copyable`) | compiler marker (traits v0.2) |
| Region inference | constraint solver | inference engine | none |
| Diagnostic style | multi-label + suggestions + `--explain` | improving | multi-label via Ariadne, 6 DX criteria |

## References

- Spec: `docs/specification.md` Section 5 (Memory Management)
- Dev: `docs/dev/mojo_reference.md` (primary algorithmic inspiration for the ownership pass)
- Dev: `docs/dev/borrow_checker.md` (Ryo-specific algorithm sketch)
- Dev: `docs/dev/zig_reference.md` (still applies for everything except the ownership pass)
- Milestone: `docs/dev/implementation_roadmap.md` Milestone 8.1
- Design doc: `docs/superpowers/specs/2026-05-11-milestone-8.1-heap-str-and-move-semantics-design.md`
- Upstream: <https://github.com/rust-lang/rust>, `compiler/rustc_borrowck/`, `compiler/rustc_mir_dataflow/`
