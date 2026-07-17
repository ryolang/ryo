**Status:** Design (Draft — v0.1)

# ARC Optimizer Pass (`ryo-driver/src/arc_optimizer.rs` or `ryo-frontend/src/arc_optimizer.rs`)

> Draft. The pass does not exist yet; this doc is the design sketch that will be referenced when the pass lands. Milestone TBD — likely sequenced with M11 (structs) or M15 (`shared[T]` stdlib type), whichever introduces refcounted values first.

This pass is the compiler-engineering investment that makes `shared[T]`'s performance story (spec 5.6) credible. Without it, every implicit retain/release on assignment fires an atomic op, and `shared[T]` overhead dominates application code. With it, retain/release pairs that bracket regions where the value cannot be deallocated are elided before codegen, matching the Swift model the spec commits to.

The pass is conceptually distinct from the ownership pass (`ryo-frontend/src/ownership.rs`):

| Pass | Concern | Soundness vs perf |
|---|---|---|
| `ryo-frontend/src/ownership.rs` (M8.1) | Move semantics, use-after-move, `Free` insertion for owned types like `str` | Soundness: rejects programs |
| `ryo-driver/src/arc_optimizer.rs` or `ryo-frontend/src/arc_optimizer.rs` (this doc) | Elide redundant retain/release on `shared[T]` values | Perf only: never changes program meaning |

Both passes run on TIR (post-sema, pre-codegen). The ownership pass must run first because its inserted `Free` instructions are inputs to the ARC pass's "what's the last use of this value?" analysis.

## Why this pass exists

The naive lowering of `shared[T]` semantics produces a retain on every assignment and a release on every drop:

```ryo
a: shared[Config] = load()    # retain (refcount = 1)
b = a                         # retain (refcount = 2)
read_from(b)
read_from(a)
                              # release b at end of scope (refcount = 1)
                              # release a at end of scope (refcount = 0, free)
```

Two retains, two releases — four atomic ops to use a value twice. For a hot loop this is wasteful. The retain/release pair on `b` is **redundant**: `a` is alive throughout the region where `b` exists, so the storage is already protected. Elide the inner pair and the program is identical:

```ryo
a: shared[Config] = load()    # retain (refcount = 1)
b = a                         # (elided)
read_from(b)
read_from(a)
                              # (elided) release b
                              # release a (refcount = 0, free)
```

Swift's SIL ARC optimizer does exactly this. Without it, Swift would be unusable for application code. Ryo inherits the same constraint.

## When elision is sound

A retain/release pair on a value `v` can be elided if and only if **between the retain and the release, `v` is guaranteed to remain alive by some other live reference**. The "alive by some other reference" condition is the load-bearing part — that's what makes the pair redundant.

In TIR terms, this becomes a dataflow query: for each pair (retain at TirRef `r`, release at TirRef `q`) on a value `v`, find at least one *witness* — another reference to `v` whose own (retain, release) interval covers `[r, q]`. If a witness exists, the inner pair is redundant.

Several patterns produce witnesses naturally:

1. **Aliased local assignment** (`b = a`): `a`'s retain covers `b`'s entire lifetime, so the retain on `b` (and the symmetric release) elides.
2. **Pass to borrowing function** (`read_from(s)` where `s: shared[T]` is borrowed per Rule 2): no retain happens at the call boundary in the first place — borrow semantics suppress the op.
3. **Pass to `move` parameter** (`consume(s)` where `s: move shared[T]`): the callee takes ownership without an extra retain — semantically, the retain is "moved" rather than duplicated.
4. **Loop-invariant retain** (`for x in xs: read(shared_thing)`): the retain on `shared_thing` outside the loop covers all iterations; redundant per-iteration retains elide.

Cases where elision is *not* sound:

1. **Cross-thread escape**: passing a `shared[T]` to a task or async closure. The witness can't be guaranteed alive on the other thread without the retain.
2. **Storage in a long-lived container**: pushing a `shared[T]` into a `list[shared[T]]` or struct field that outlives the source. The retain is real.
3. **Conditional ownership across CFG joins** where the lattice can't prove the witness exists on every path.
4. **`weak[T]` upgrade**: when `weak[T]::upgrade() -> ?shared[T]` succeeds, the new `shared[T]` is a real retain (the weak ref didn't hold one).

## Pipeline position

```text
… Sema → TIR → OwnershipPass → TIR' → ArcOptimizer → TIR'' → Codegen …
```

The ARC optimizer runs after the ownership pass for two reasons:

1. The ownership pass inserts `TirTag::Free` for owned types (`str`, etc.). The ARC optimizer's "alive at this point?" query depends on knowing where pure-memory frees happen, so the data has to be there first.
2. The ownership pass's per-`TirRef` `ValueState` lattice is a near-exact prerequisite for the ARC pass's witness analysis. Same dataflow infrastructure, extended.

A reasonable implementation strategy is to **share the dataflow framework** between the two passes — same lattice scaffolding, different transfer functions. The ARC optimizer is then "compose witness intervals over the same TIR" rather than a separate pass with its own dataflow library.

**Target-agnostic.** The pass operates on TIR before backend lowering, so it is unaffected by the choice of native (Cranelift → object) vs any future WASM backend. If a WASM emission path is ever built (see [proposals/wasm_target.md](proposals/wasm_target.md) — currently deferred), the elided retains/releases would be WASM atomic ops when the `+atomics` feature is enabled and ordinary loads/stores otherwise; the pass would not need to care which.

## The pass, sketched

```rust
// src/arc_optimizer.rs (sketch)

pub struct ArcOptimizer<'a> {
    pool: &'a InternPool,
    tir: &'a mut Tir,

    /// Per-TirRef refcount class. Only meaningful for refs whose type
    /// is shared-counted (`shared[T]`, future `rc[T]`).
    refcount_class: Vec<RefcountClass>,

    /// Map from each shared value to the set of TirRefs that alias it.
    /// Used to build witness intervals.
    aliases: HashMap<SharedValueId, Vec<TirRef>>,
}

#[derive(Clone, Copy, Debug)]
enum RefcountClass {
    /// Not a refcounted type — leave alone.
    NotShared,
    /// `shared[T]` — atomic refcount on retain/release.
    SharedAtomic,
    /// Future: `rc[T]` — non-atomic single-threaded variant.
    SharedNonAtomic,
}

#[derive(Clone, Copy, Debug)]
struct SharedValueId(u32);  // canonical id for an aliasing equivalence class

impl<'a> ArcOptimizer<'a> {
    pub fn run(&mut self) {
        self.classify_values();        // fill `refcount_class`
        self.compute_alias_classes();  // build `aliases`
        self.elide_redundant_pairs();  // the actual optimization
        self.promote_to_stack();       // optional: escape-analyse for stack alloc
    }

    fn elide_redundant_pairs(&mut self) {
        for class in self.aliases.values() {
            // For each (retain, release) pair on a TirRef in `class`,
            // check whether any *other* member of the class has an active
            // (retain, release) interval covering this one. If so, this
            // pair is redundant: rewrite to no-ops (or remove instructions
            // entirely — TIR builder supports both).
        }
    }
}
```

The interesting work is `compute_alias_classes` and the dataflow that backs `elide_redundant_pairs`. Both are standard SSA-style aliasing analysis — the kind of code that fits in a single file once the algorithms are settled.

## Algorithm in detail

### 1. Classify refcounted values

Walk the TIR once. For each TirRef whose `ty` is `shared[_]` (or a future `rc[_]`), set its `RefcountClass` accordingly. All other refs are `NotShared` and ignored by subsequent steps.

### 2. Build alias equivalence classes

Two TirRefs `r`, `q` are aliases of the same shared value if:

- `q` is the target of an `Assign { name, value: r }`, or
- `q` is the result of an alias-preserving operation on `r` (e.g., passing `r` as a borrow parameter and reading the resulting ref inside the callee — though for M8.1's intra-procedural pass, callees are opaque, so this is conservative).

Build equivalence classes via union-find over the assignment graph. Each class gets a `SharedValueId`.

### 3. Find retain/release pairs and witnesses

For each TirRef in each class, identify the implicit retain (at the producer site — e.g., `Assign`, `VarDecl`) and the implicit release (at the last use — analogous to the ownership pass's `Free` insertion logic, but for refcount instead of pure-memory).

For each pair `(retain_at, release_at)` on TirRef `r`, query: is there another TirRef `r'` in the same class with `retain_at(r') < retain_at(r)` and `release_at(r') > release_at(r)`? If so, `r`'s pair is **redundant**.

Standard interval-tree or sweep-line algorithms make this O(n log n) over the pairs in a class. In practice classes are small (most shared values have 2–5 aliases), so naive O(n²) per class is fine.

### 4. Rewrite

Replace redundant retains and their paired releases with no-ops. The TIR builder needs a `Nop` tag (or the pass can compact in place). The pass emits no diagnostics — elision is invisible to the user.

### 5. Optional: stack promotion

After elision, walk the remaining refcount ops. For values whose **all** retains and releases are now elided AND whose lifetime is bounded within the function (no escape into closures, returns, struct fields, task spawns), the heap allocation itself can be replaced with a stack slot. This mirrors Swift's stack-promotion pass and is the biggest single win for short-lived shared values.

Stack promotion is sound only when escape analysis is conclusive. Be conservative — false positives are a correctness bug (use-after-free at the stack frame's end). The conservative default is "don't promote unless every aliasing TirRef is observably local."

## What this means for `weak[T]` and `unowned[T]`

The pass treats `weak[T]` and `unowned[T]` as **non-counting references**: they don't fire retains, so there's nothing to elide. The only refcount op they generate is the eventual increment when `weak[T]::upgrade()` returns a `?shared[T]` — that retain is real and never elidable (the weak ref didn't keep the storage alive).

This means the pass's existence simplifies the `weak[T]` cost story: weak references are essentially free, and the only cost is on the upgrade path. Document this in spec section 5.6's cost table.

## What this means for COW collections

`list[T]`, `map[K, V]`, and `str` are class-backed but value-typed at the API. Internally they hold a `shared[Buffer<T>]` (or equivalent). Mutating methods check the refcount; if > 1, they clone the buffer first.

The ARC optimizer interacts with COW like this: most reads of a `list[T]` never trigger a buffer clone because the elision pass keeps the alias count visible. A function that takes a `list[T]` parameter doesn't trigger a clone because it's a borrow; only a mutation through a second handle would. The COW protocol and the ARC optimizer reinforce each other — neither would suffice alone.

## What this pass does NOT do

- **It is not a soundness check.** Use-after-free of a `shared[T]` is impossible by construction (refcount holds the storage); the pass can never introduce a use-after-free, only remove redundant safety.
- **It does not handle cross-function elision.** All analysis is intra-procedural. Inlining (whether done by Cranelift or by a future Ryo-side inliner) is the prerequisite for cross-function ARC elision. Pre-monomorphization (M11+) most inlining will be limited.
- **It does not insert retain/release ops.** Those come from the lowering of `shared[T]` semantics in earlier passes (sema or a dedicated shared-lowering step). The optimizer only removes redundant ops.
- **It does not handle cycle collection.** Cycles between `shared[T]` values that don't include a `weak[T]` link leak. Spec section 5.6 acknowledges this; the optimizer doesn't change it.
- **It does not optimize `mutex[T]` or `rwlock[T]` lock acquisition.** Those are independent primitives with their own runtime. Lock elision is a separate project (and a much harder one).

## Sequencing on the roadmap

The pass is not needed until refcounted values exist as language features. Likely sequencing:

1. **M11 (structs)** — first real candidate for `shared[T]` because real-world shared-struct patterns appear once structs land.
2. **M15 (concurrency / tasks)** — `shared[T]` becomes essential when multiple tasks need access to the same value. Spec section 5.6 implicitly assumes this is available.
3. **ARC optimizer pass** — should land alongside or shortly after the first stdlib introduction of `shared[T]`, before any benchmark commitments. Without it, microbenchmarks will look bad and erode trust.

A plausible milestone slot is **M11.5** (after structs, before tasks) so the pass is in place when concurrency code starts using `shared[T]` in earnest.

## Testing strategy

- **Unit tests** in `ryo-frontend/src/arc_optimizer.rs` or `ryo-driver/src/arc_optimizer.rs`: hand-built TIRs with known retain/release patterns, assert the pass elides the expected pairs and leaves the rest alone.
- **Differential tests** against a "no optimization" baseline: same program, compare both behaviour (must be identical) and refcount op counts (must decrease).
- **Microbenchmarks** for representative patterns: tight loops over `shared[T]`, builder chains over COW collections, observer-pattern fan-out. Each should land within a small multiple of an equivalent Rust `&T` baseline; if not, the pass is missing cases.
- **Soundness fuzzing**: random TIRs with synthetic retain/release pairs; assert elision never removes a pair whose dataflow analysis says "non-redundant." This catches algorithmic bugs that unit tests miss.

## Open questions

- **Do we need `rc[T]` (non-atomic) as a separate type?** Swift removed its non-atomic option after its ARC optimizer matured enough that the distinction wasn't worth the API complexity. Ryo can defer the decision — if `shared[T]` benchmarks acceptably with the optimizer, no `rc[T]` is needed. If not, add it later.
- **How much of the pass should run on JIT?** Full optimization wins on AOT; for JIT (REPL / `cargo run --`), elision still helps but stack promotion may be too expensive to compute. Pass should be tunable.
- **Interaction with comptime (v0.2+)?** Comptime-known refcounts could in principle be folded to compile-time constants. Speculative — defer until comptime is a real feature.

## References

- Spec: `docs/specification.md` Section 5.6 (Shared Ownership)
- Dev: `ryo-frontend/src/ownership.rs` (ownership-pass infrastructure this pass extends)
- Dev: `docs/dev/mojo_reference.md` (ownership-pass inspiration)
- Dev: `docs/dev/rust_reference.md` (comparison against Rust's `Arc<T>` model)
- Upstream prior art: Swift's SIL ARC optimizer (`swift/lib/SILOptimizer/ARC/`), Apple's WWDC talks on Swift performance.
- Milestone: TBD — see *Sequencing on the roadmap* above.
