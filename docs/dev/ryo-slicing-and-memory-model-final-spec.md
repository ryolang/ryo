# Ryo Slicing, Views & Memory Model — Final Design Specification

> **Document Status:** Design Decision Record + Specification (Pre-Implementation)
> **Last Updated:** 2026-08-02
> **Version:** 1.0.0-draft
> **Supersedes:** `ryo-slice-projections-proposal.md` (adopted), `ryo_binary_pattern_matching_proposal.md` (partially adopted, memory model rejected)
> **Amends:** Ryo Language Specification §1.2 (new principle), §3 (Binding Rule), §4.2, §4.3, §4.4, §4.5, §5.2.1 (concurrency constraint), §5.7, §5.8, §9.1 (ambient runtime), §9.2.1, §17 (unsafe)

---

## 1. Executive Summary

This document records the final decision on how Ryo handles slicing, zero-copy views, and contiguous binary data, resolving two competing community proposals.

**Decisions:**

| # | Decision | Origin |
|---|----------|--------|
| D1 | **Adopt Slice Projections** as Ryo's slice semantics: `strview`, `slice[T]`, `bytesview` — bindable, passable, non-escaping, statically verified, zero runtime cost | Projections proposal (adopted in full) |
| D2 | **Adopt `bytes`** as a new fundamental owned type for contiguous binary data | Binary proposal (salvaged) |
| D3 | **Adopt `sbytes`**, a shared-backed buffer (opt-in ARC + COW with compiler warnings), for views that must escape | New — replaces the binary proposal's universal memory model |
| D4 | **Lift the `unsafe` restriction.** Unsafe blocks move from "system packages only" to a manifest-gated, auditable capability available to all packages | New — amends spec §5.8/§17 |
| D5 | **Adopt scoped task borrows:** closures inside `task.scope` may capture by borrow, enabling fork-join data parallelism | New — amends spec §9.2.1 |
| D6 | **Reject** universal implicit ARC + COW for `str` / `list[T]` | Binary proposal's core memory model |
| D7 | **Defer** binary pattern-matching syntax, mutable view types, and `yield`-style escaping projections | Both proposals |
| D8 | **Reject** Mojo-style origin parameters for escaping views (see §2.6) | Comparison with Mojo |
| D9 | **Adopt two runtime profiles** — `core` (freestanding: no scheduler, `panic` = abort, traces off, allocator-provided) and `hosted` (full ambient runtime, the default); `task.*` is hosted-only (see §8) | New — runtime split proposal |
| D10 | **Adopt bounded operator overloading** — trait-based, fixed operator set, same-type operands, static dispatch; reverses the Zig-inherited rejection (see §9) | New — reverses base-spec §1.2 |
| D11 | **Adopt the anonymous struct as the single grouping type** — tuples become positional sugar; `Name{...}` construction; the Binding Rule (`=` binds names, `:` relates) and the Brace Law (parens positional, braces named); structural identity, exact-match only (see §10) | New — reverses base-spec §4.2/§4.3 pre-implementation |

**New guiding principle (added to spec §1.2):**

> *Ryo must not be limiting where an elegant solution exists.* A solution is **elegant** if and only if: (a) it is statically verified, with no annotations required at the use site; (b) any runtime cost it carries is **opt-in and visible in the type or signature**; (c) code that does not use it pays nothing — no hidden costs; (d) it passes the reviewer test: a human can understand the semantics by reading the code, without memorizing special rules.

D3, D4, and D5 are direct applications of this principle. D6 is its guardrail: elegance is never achieved by making the *default* dynamic.

---

## 2. Learnings (Why This Decision)

### 2.1 The Ownership Lite bargain

Ownership Lite pays expressiveness (Rule 5: no returning borrows; Rule 6: no references in structs) to buy fully static checking (no lifetimes, no GC, no runtime bookkeeping). Every accepted proposal in this document preserves that bargain; every rejection violates it.

The rejected universal-ARC model refunded the expressiveness by moving sharing into runtime bookkeeping (hidden atomic refcounts on every `str` and `list`, data-dependent COW copies on mutation). That is Swift's memory model in Ryo's syntax. It taxes every string in every program to serve the protocol-parsing niche, makes mutation cost data-dependent (O(1) vs O(n) depends on runtime refcount — invisible to the reviewer), and converts "mutate while viewed" from a compile error into a silent stale read with a warning. The spec already scopes Swift-style ARC to `shared[T]` as an explicit opt-in (§5.6); there is no justification for making it the invisible default.

### 2.2 The zero-copy taxonomy

"Zero copy is basic in most languages" is true — and never free. Every language with escaping zero-copy views pays one tax:

| Language | Escaping zero-copy? | Tax |
|----------|--------------------:|-----|
| Rust | Yes | Lifetime annotations |
| Go | Yes | Aliasing hazards (`append` spooky action; substring pins whole buffer) + GC |
| Swift | Yes | Universal hidden ARC + COW; unpredictable copy costs |
| Java / C# / Python | Yes-ish | GC (and Python slicing copies anyway) |

Ryo's tax is **expressiveness at scope boundaries**. This document keeps that tax where it's cheap and removes it where it's expensive, via three opt-in mechanisms (D3, D4, D5) rather than one universal runtime mechanism.

The taxonomy that falls out:

| Direction | Mechanism | Cost |
|-----------|-----------|------|
| Views flow **down** (params, callees) | Projections (`strview`, `slice[T]`, `bytesview`) | Zero, statically verified |
| Views live **within** a scope (locals, re-slices, iterator chains) | Projections + scope-locked views | Zero, statically verified |
| Views flow **up/out** (returns, struct fields, containers) | `sbytes` (opt-in ARC+COW), or owned values, or offset/ID idioms | Opt-in, type-visible |
| Views cross **tasks** | `sbytes` by move; borrows within `task.scope` | Opt-in, statically verified |

### 2.3 Validations

Two architecture walkthroughs confirmed the model fits Ryo's target domains:

- **JSON:** parsers are natural projection sinks — views flow in (`strview` input, zero-copy scanning), owned values flow out (DOM or structs). Output strings are owned; the cost is blunted because escaped strings must allocate in every language (this is why serde_json uses `Cow`), and short keys/values hit the small-string optimization. Serialization writes directly into the response buffer via the `inout` sink pattern — zero intermediate allocations, arguably better than borrowed-view languages.
- **View–Controller–DB–Response:** the request lifecycle is a pipeline of owned values crossing stage boundaries (free in Ryo: move + NRVO) with views living inside each stage. DB results are owned in *every* language (socket buffers are reused), so Ownership Lite costs nothing there. T-strings give injection-safe SQL and XSS-safe HTML by construction.

### 2.4 Where the walls were

Honest gaps identified (and their dispositions):

1. **Fork-join parallelism on local data** — was inexpressible (tasks capture by move; no borrows cross tasks; views don't escape). **Fixed by D5.**
2. **Persistent zero-copy views** (parsed packet structs, caches, columnar analytics) — required offset/ID bookkeeping or copies. **Fixed by D3.**
3. **Third-party systems libraries** — `unsafe` restricted to system packages meant the set of expressible safe abstractions was fixed forever by the language team; the ecosystem could not build what the stdlib didn't anticipate. **Fixed by D4.**
4. **Mutable window algorithms** (codecs, compression, in-place sub-ranges) — mutable view types remain rejected (they reopen the M8.3 `MutRef` question); mitigated by range-based `inout` stdlib APIs. **Open question Q4.**
5. **Excluded by design, unchanged:** HFT, real-time A/V, bare-metal embedded (spec §1.1).

### 2.5 Secondary learnings

- **Warnings expose costs; errors enforce semantics.** The COW-warning design (adopted in D3) is the right tool for *cost visibility*. It is the wrong tool for *semantic hazards* — those stay compile errors (P2 freeze) or panics (versioned iterators).
- **The spec had already relaxed Ownership Lite twice**, in bounded verifiable ways: method views (Rule 5 exception) and scope-locked iterators (§5.7). D3–D5 follow that template: bounded, opt-in, statically verifiable, annotation-free.
- **The Erlang-style parse-into-struct-of-views use case is real** for Ryo's networking domain. It is now served by `sbytes` struct fields instead of a universal memory-model change.

### 2.6 Mojo, origins, and the escaping-view path not taken

Mojo is a declared Ryo inspiration (spec §1), and Ryo already imports its best ideas: `inout` conventions, value semantics, move-by-default, and ASAP destruction (§5.4 is explicitly Mojo-inspired; projection rules P4/P5 are exactly ASAP destruction extended with a projection-safety constraint). The one Mojo idea Ryo must *not* import is the one Mojo invented for the problem this document solves: **origins**.

**How Mojo handles escapes.** Mojo's view types (`StringSlice`, `Span[T]`) carry an **origin parameter** — a compile-time token identifying the value a view borrows from. Because origins travel in the type, Mojo views may escape: `fn first_word(s: String) -> StringSlice[origin_of(s)]` is legal, giving zero-cost returning views with no ARC. Simple cases are inferred; ambiguous or complex ones require explicit origin annotations in signatures, and origins are parametric over mutability (`ImmutableOrigin` / `MutableOrigin`).

**Honest evaluation.** Mojo's model is strictly more expressive than Ryo's final design — it is the only system in the comparison that achieves escaping zero-copy views with neither lifetimes-as-syntax nor runtime refcounting. That credit is real. The costs, equally real:

1. **Origins are lifetimes under another name.** They appear in type signatures (`Span[UInt8, origin_of(buffer)]`), they are viral through API boundaries exactly like Rust's `'a`, and inference failure modes produce the same class of diagnostics. Ryo's founding promise (§5: *"Rust-level safety without lifetime annotations"*) is repealed in substance if origins are admitted — the annotation count is lower, but the *concept* returns.
2. **Evidence of instability.** Mojo itself has iterated its ownership surface repeatedly (`^`, `owned` → `consuming` → `var`, `ref`, then origins) — the strongest available evidence that the "progressive lifetime" middle path is hard to stabilize and teach.
3. **The reviewer test fails.** Ryo's AI-era design rule (§1.2) is that a human reviewer understands semantics by reading code without memorizing special rules. A signature carrying origin parametrics requires the reader to reason about an invisible provenance graph; a parameter typed `sbytes` states its semantics in its name.
4. **Cost accounting.** Mojo pays *signature and compiler complexity everywhere origins leak*; Ryo pays *a bounded runtime cost (atomic refcount, COW-with-warning) exactly and only where the user opted into `sbytes`*. For a language whose stated performance goal is "comparable to Go" and whose philosophy trades runtime for DX — not semantic complexity for DX — Ryo's side of this trade is the consistent one.

**Where the rejection could hurt, and the mitigation.** Tight loops that manufacture millions of *escaping* view values would feel `sbytes` refcount traffic where Mojo pays nothing. The mitigation is architectural, not linguistic: hot loops use projections (free, non-escaping); `sbytes` is used at boundaries where values are stored or returned. If profiling ever shows this gap dominating real workloads, the documented evolution path is Hylo-style `yield` subroutines (§12) — which recovers most of Mojo's expressiveness for the producer/consumer pattern *without* general origin parametrics.

**Note on the rejected binary proposal.** That proposal cited Mojo's origin tracking as inspiration but discarded it in favor of universal runtime ARC — accepting Mojo's *goal* (escaping views) while rejecting Mojo's *mechanism* (compile-time provenance) and Ryo's mechanism (opt-in sharing) alike, landing on hidden runtime cost instead. The final spec keeps the Mojo ideas that fit (eager destruction, `inout`, value semantics) and rejects the one that contradicts Ryo's founding promise (origins, D8).

---

## 3. Final Specification: Slice Projections (D1)

*This section is normative. It adopts the projections proposal (v2) in full; only the `bytesview` extension is new.*

### 3.1 Types

| Type | Meaning | Representation |
|------|---------|----------------|
| `strview` | Read-only UTF-8 view into a `str` buffer | `{ ptr, len }` — 16 bytes |
| `slice[T]` | Read-only view into contiguous element storage (fixed arrays `[T]`, later `list[T]`) | `{ ptr, len }` — 16 bytes |
| `bytesview` | Read-only view into a `bytes` buffer | `{ ptr, len }` — 16 bytes |

Slice expressions: `s[start:end]`, `s[start:]`, `s[:end]`, `s[:]` — half-open `[start, end)`. Indices are non-negative `int`; out-of-range and reversed ranges (`start > end`) panic. For `str`, indices are byte offsets and must lie on UTF-8 boundaries, otherwise panic. No negative indexing (consistent with §4.7).

### 3.2 Projection rules

- **P1.** A slice is a projection: it exposes a sub-region of its owner's storage, owns nothing, allocates nothing.
- **P2.** While any projection of owner `o` is live, `o` is frozen: mutation, `inout` passing, and `move` of `o` are compile errors. Reading `o` (including more slices) is legal.
- **P3.** Projection is transitive: re-slicing a slice projects the original owner; P2 still applies to that owner.
- **P4.** A slice's lifetime ends at its last use; P2's restriction lifts at that point. Across branches the last use is per-path: a read in a sibling arm does not keep the slice live on this arm's path, while a read after the join keeps it live on every arm's path.
- **P5.** The owner's destruction is deferred to the later of its own last use and the last use of any live projection. This overrides ASAP destruction (§5.4) wherever they disagree; when no projection is live, behavior is unchanged (zero cost).
- **P6.** Producing an owned copy of viewed contents creates a new object, not a projection. Views never implicitly copy: passing a view to a `str` parameter re-borrows it (`cap=0`, no allocation), exactly like a string literal.

### 3.3 Escape rules

- **E1.** Slices cannot be returned from functions. *(Exception per Rule 5: method views tied to `self`'s scope.)*
- **E2.** Slices cannot be passed to `move` parameters.
- **E3.** Slices cannot be stored in aggregates: no struct fields, no `list[strview]`.
- **E4.** Slices may be passed to default borrow parameters — the callee's borrow is bounded by the call.

For views that must escape: use `sbytes` (§5).

### 3.4 Interaction with borrow modes

`strview` / `slice[T]` / `bytesview` become the preferred parameter types for read-only access. Passing an owned value to a view parameter triggers an implicit view conversion (`str → strview` drops the `cap` word; representation coercion, not just a borrow). The reverse direction also works: passing a view to an ordinary `str` parameter re-borrows it (`cap=0`, no allocation) — call-scoped only; binding `x: str = view` or passing to a `move str` parameter remains an error.

| Parameter type | Use when |
|----------------|----------|
| `strview` / `slice[T]` / `bytesview` | Reading contents (replaces most `s: str` parameters) |
| `str` / `bytes` | Keeping or extending the value |
| `inout str` / `inout bytes` | Mutating the caller's value in place |
| `move str` / `move bytes` | Taking ownership |

### 3.4.1 Materialization — `str(view)` (M8.4.1.2)

The re-borrow (P6') covers call sites for free; it cannot cover escapes, because the manufactured `str` is call-scoped. The owned-copy operation P6 alludes to is spelled **`str(view)`** (decision record: `ryo-view-materialization.md`):

- **Type rule.** The argument must be a `strview`; anything else — including an owned `str` — is E0012 TypeMismatch. There is no `str(owned)` clone form: same-type duplication is the future `Clone` trait's story (trait milestone).
- **Result.** A fresh owned `str` (`{ptr, len, cap=len}`): allocates, copies the viewed bytes, and is independent of the source buffer.
- **Binding.** `x: str = str(view)` is legal (fresh owner); `x: str = view` remains E0012 — materialization is never implicit, here or anywhere.
- **Division of labor.** Re-borrow (P6') for call sites: zero cost, call-scoped. Materialization for escapes: returns, stores into longer-lived structures, moves into spawned tasks, and defensive copies taken before the source is mutated. Reaching for `str(view)` at a call site pays an allocation the re-borrow avoids.
- **W0003 RedundantMaterialize.** A heuristic, warnings-only lint flags the two bad shapes: (a) a materialize call in an argument position the re-borrow already serves, and (b) a bound materialize result that never escapes while its source is never mutated afterward (mutating the source later is a legitimate defensive copy and is not flagged). Conservative by design: when unsure, no warning.
- **Trait-forward hook.** When traits land, `str(view)` resolves through a converting-initializer protocol (Swift `init(_:)`-style, or a `From`/`Materialize` trait with call syntax — name TBD at the trait milestone). Call sites unchanged; recorded as an open hook, not built now.

`bytes(bview)` mirrors this with M8.4.2; `slice[T]` materialization defers to M21 with its bit-copy restriction (decision record §2.4).

### 3.5 Conformance edits to the base spec

1. **§4.4 wording:** replace "Slices cannot be stored in variables" with: *"A slice may be bound to a local variable whose uses remain within the current function; a slice cannot be stored in a variable, field, or container that outlives that function (see §5.7 and Rule 5)."*
2. **§4.4 rationale:** replace "borrows are parameter-passing conventions, not general-purpose types" with: *"Mutable borrows remain a parameter-passing convention, not a type (M8.3). Immutable views (`strview`, `slice[T]`, `bytesview`) are a narrow exception: they are first-class types that may be bound and passed, but they are non-escaping — they cannot be returned, moved, or stored in aggregates — so they cannot play the role of general-purpose reference types."*
3. **Implementation:** the ownership pass gains projection-origin tracking (root-owner side table), freeze ranges (live-projection set per owner), and P5-deferred destruction. No new pass; the existing forward walk is extended. Diagnostics are emitted post-liveness so "last use" spans are accurate.

---

## 4. New Fundamental Type: `bytes` (D2)

An owned, heap-allocated, contiguous byte buffer — the binary sibling of `str`. Fat pointer `{ ptr, len, cap }`, move semantics, mutability by binding. Fills the gap where `list[u8]` was the only (awkward) option.

```ryo
raw = bytes.from_list([0x01, 0x02, 0x03])
lit = b"\x00\x01"              # bytes literal

# Slicing yields a projection (D1 rules apply: non-escaping, zero-copy)
header = raw[0:2]              # bytesview

# Bridging
text = try raw.to_str()        # UTF-8 validated, Utf8Error!str
raw2 = text.to_bytes()         # owned copy
```

- `str` indexing remains forbidden (§4.7); `bytes` supports `b[i]` (bytes are not text — no UTF-8 hazard).
- `bytes` is an ownership type: assignment/return move; parameters borrow by default; `inout`/`move` as usual.
- Buffer building uses the builder idiom (`bytes.builder().u8(v).u16_be(n).bytes(b).build()`), consistent with `move self -> Self` chaining (§5.2.1).

---

## 5. New Fundamental Type: `sbytes` — the Shared-Backed Buffer (D3)

For views that must **escape** — struct fields, return values, caches, task boundaries — Ryo provides an explicitly shared buffer. This is the mechanism of the rejected binary proposal (ARC + COW + warnings), rescoped from *universal default* to *opt-in type*.

### 5.1 Semantics

```ryo
struct Packet:
	header: sbytes           # ESCAPING view — legal: sbytes is an owned type
	payload: sbytes

fn parse_packet(move buf: bytes) -> ParseError!Packet:
	sb = buf.share()                  # move: buf's allocation rewrapped as ARC (refcount = 1, no copy)
	if sb.len() < 4:
		return ParseError("too short")
	return Packet(
		header  = sb[0:4],            # sbytes slice → sbytes (refcount +1, zero-copy)
		payload = sb[4:],             # zero-copy, may outlive this function
	)
```

- **`share()` is consuming and zero-copy:** `fn share(move self) -> sbytes` rewraps the existing heap allocation as shared — no copy, no hidden cost. The reverse direction (`sb.clone_bytes() -> bytes`) is an explicit copy, spelled as one.
- **Slicing returns `sbytes`** (same type): offset + length into the shared buffer, refcount incremented. Storable, returnable, movable across tasks (atomic refcount).
- **Value semantics via COW:** mutating an `sbytes` with refcount > 1 clones the buffer first. The compiler emits a **warning** at mutation sites where static analysis detects the buffer may be shared — showing the aliasing slice, buffer size, and suggested fixes (consume before mutate; drop slices first; explicit `.clone()`). **Honesty clause:** aliasing created across function or task boundaries is not always statically visible; those COW events happen without a warning. The copy still preserves value semantics — the warning is a best-effort cost signal, not a guarantee.
- **Warnings, not errors:** COW is a *cost* event, not a semantic hazard — value semantics are preserved either way. `#[allow(cow)]` suppresses per expression/function; `[warnings] cow = "allow"` per package.
- **Explicit opt-in:** the type name is visible in every signature and struct field. `str`, `bytes`, and `list[T]` are completely unaffected — code that never touches `sbytes` pays nothing (no refcounts, no COW, no warnings).
- **Concurrency:** `sbytes` may be moved into task closures (D5's move-capture rule unchanged for it). Mutable sharing across tasks still requires `shared[mutex[sbytes]]`.

### 5.1.1 Interplay with projections: the two-tier rule

`sbytes` participates in *both* aliasing regimes, and the boundary is exact:

1. **Slicing an `sbytes` with `sb[a:b]` yields an owned `sbytes`, not a projection.** The P/E rules do not apply: the result does not borrow, so the source is *not* frozen, and later mutation falls under the COW rule above (runtime aliasing).
2. **An `sbytes` may be viewed as `bytesview`** (implicit view conversion at a `bytesview` parameter, or `sb.view()` for a local binding). That *is* a projection: P1–P6 apply in full, so the `sbytes` is frozen for the view's lifetime — mutation is a **compile error**, and no COW event can occur while a projection is live.

The rule of thumb: **compile-time-visible aliasing (projections) is enforced by freeze; runtime-visible aliasing (`sbytes`↔`sbytes`) is handled by COW with a warning.** The two never overlap, because a frozen value cannot reach a mutation site.

### 5.2 What it unlocks

| Use case | Before | With `sbytes` |
|----------|--------|---------------|
| Zero-copy parsed packet structs | Offsets/IDs or copies | `sbytes` fields |
| Caches of parsed artifacts over one loaded buffer | Manual arena | `sbytes` values in `map` |
| Columnar/analytics buffers (Arrow-style) | Inexpressible | `sbytes` columns + slices |
| Zero-copy payloads crossing tasks | Copy or `shared[bytes]` + manual ranges | Move `sbytes` slices |

### 5.3 Explicitly not provided

- No `sstr`: shared *text* is served by `shared[str]` (immutable strings already get COW buffer-sharing as a stdlib optimization, §5.9 #8). If demand proves otherwise, `sstr` is a additive follow-up, not a redesign.
- No implicit conversions between `bytes` and `sbytes`. `buf.share()` and `sb.clone_bytes()` are explicit, so the ownership model is always visible at the boundary.

---

## 6. `unsafe` Policy Revision (D4)

### 6.1 The problem with the old rule

Spec §5.8 restricted `unsafe` to system packages. Intent: protect the ecosystem from unsafe sprawl. Effect: the set of expressible safe abstractions was permanently fixed by the language team. In Rust, `unsafe` is how the *ecosystem* builds new safe abstractions (`Vec`, `crossbeam`, `tokio`); under the old rule, Ryo third parties could not build a custom container, an FFI wrapper with non-trivial ownership, or a `par_chunks`-style primitive. Prohibition did not eliminate risk — it centralized capability and blocked the elegant solutions the principle in §1 requires.

### 6.2 The new rule: gated, auditable, documented

`unsafe` becomes a **manifest-declared capability** for any package:

```toml
# ryo.toml
[package]
name = "fastcodec"
allow_unsafe = true        # required, or unsafe blocks are compile errors
```

Rules:

1. **Default unchanged:** packages without the flag cannot contain `unsafe` blocks. Most of the ecosystem never opts in.
2. **Visibility:** the capability is stated in the manifest, printed by `ryo audit` (dependency tree report), and surfaced by tooling. Consumers can build with `--deny-unsafe=deps` to reject any dependency that uses it.
3. **Mandatory safety documentation:** every `unsafe` block must be preceded by a `#: SAFETY:` doc comment explaining why its invariants hold. A missing or empty `SAFETY:` comment is a compile error. The reviewer test applies to unsafe code most of all.
4. **Safe-API norm (lint):** `pub` functions whose bodies contain `unsafe` trigger a pedantic lint unless documented as safe abstractions. Raw pointers (`*T`) remain confined to `unsafe` blocks; they never appear in safe signatures.

This keeps Ryo's promise ("memory safe by default") while making the boundary *auditable* rather than *prohibitive*: the language team no longer decides which abstractions may exist; the manifest and the review process decide which get used.

---

## 7. Scoped Task Borrows (D5)

### 7.1 The hole

Tasks captured by move only (§9.2.1) and borrows never cross task boundaries (Rule 7). Correct for `task.run` / `task.spawn_detached` — the task may outlive the spawner. But `task.scope` is **structured concurrency**: the scope joins all children before it exits. A child that borrows the spawner's stack data *cannot* outlive it — this is the one case where borrowing into tasks is provably safe, and it is exactly the fork-join pattern (parallel map over a local array) that was otherwise inexpressible.

### 7.2 The rule

Inside a `task.scope` body:

1. Child closures **may capture by immutable borrow**. The compiler verifies the captured data is not mutated for the scope's duration (same freeze machinery as P2) and that no capture escapes the scope (children cannot be detached; the scope joins before any captured binding dies).
2. **Projections may be captured too.** A `strview` / `slice[T]` / `bytesview` captured by a scope child does not violate E1–E4: the scope join is lexically inside the defining function, so the view still cannot escape it. The effect is that the owner's freeze (P2) extends to the end of the `task.scope` block rather than the view's last use.
3. `task.run` and `task.spawn_detached` are **unchanged**: implicit move capture, enforced.
4. Mutable parallel access is provided by **stdlib APIs built on D4**, not by language-level mutable captures:

```ryo
mut data = load_pixels()              # list[u32], 100M elements

task.scope:
	# stdlib: splits into disjoint inout ranges, one child task each.
	# Safe API; unsafe internals permitted by D4; borrows permitted by D5.
	data.par_for_each_chunk(fn(chunk):    # chunk: inout range of the same list
		apply_filter(&chunk))
# scope joined — data fully owned by caller again, no copies
```

The language change is minimal (a capture-rule exemption bounded by the scope join); the parallelism primitives live in the library where they belong.

---

## 8. Runtime Profiles: `core` and `hosted` (D9)

### 8.1 Motivation

The ambient runtime — TLS runtime context, M:N green-thread scheduler with stack swapping, error/panic stack-trace capture — is a *deployment assumption*, not a language property. Ownership, borrowing, Drop, eager destruction, bounds checks, and versioned iterators are all compile-time or trivially local machinery; none of them need an OS. Splitting the runtime into two profiles pays off immediately, independent of any embedded ambition:

- **Wasm** (a stated target domain): TLS-based ambient context and stack-swapping green threads are exactly the components that sit awkwardly on Wasm. A `core` profile makes Wasm builds small and idiomatic today.
- **CLI tools and small binaries:** less runtime, faster builds, no scheduler code in a 200-line utility.
- **Bare metal (future):** removes the *runtime* blocker. The *backend* blocker remains — Cranelift cannot emit AVR, Xtensa, or Thumb code, so Arduino-class MCUs stay gated on a future LLVM backend (Q6). This profile split is necessary but not sufficient, and the document says so rather than implying otherwise.

### 8.2 The two profiles

| Capability | `core` | `hosted` (default) |
|------------|:------:|:------------------:|
| Move / borrow / Drop / eager destruction | ✅ (compile-time) | ✅ |
| Bounds checks, versioned iterators | ✅ | ✅ |
| `panic` | abort / trap (handler overridable) | full stack trace |
| Error unions, `try` / `catch` | ✅ types; `.location()` / `.stack_trace()` return `none` | ✅ full capture |
| `str`, `bytes`, `list`, `map` | ✅ (requires a linked allocator) | ✅ |
| `sbytes`, `shared[T]` (atomic ARC) | ✅ (atomics only) | ✅ |
| `task.run` / `task.scope` / `task.spawn_detached`, `future`, channels, `select` | ❌ compile error with targeted message | ✅ |
| `with` blocks | ✅ (pure Drop) | ✅ |
| REPL (Cranelift JIT, v0.2) | ❌ (no runtime to host the session) | ✅ |

Rules:

1. **Default is `hosted`.** `ryo build --profile=core` or `runtime = "core"` in `ryo.toml` opts out. The existing `--error-traces=off` flag is subsumed: `core` implies traces off, no capture machinery is compiled in.
2. **One language, no dialect.** Source that uses only `core` capabilities compiles identically under both profiles. Using `task.*` under `core` is a compile error: *"`task.run` requires the hosted runtime (ambient scheduler). Build with `--profile=hosted`, or use a third-party executor crate."*
3. **Third-party executors are a library concern.** An Embassy-style cooperative executor (proven pattern on Cortex-M) can be built as an ordinary package against `core`, using D4's gated `unsafe` internally. Concurrency on `core` is not forbidden — it is unbundled.
4. **Stdlib layering:** the `core` module is always available; `std` re-exports `core` and adds hosted facilities (mirroring the proven Rust `core`/`std` precedent, adapted: Ryo's `str` is a built-in heap type, so `core` assumes an allocator rather than Rust's three-way `core`/`alloc`/`std` split).

### 8.3 Naming

`core` / `hosted`. **Rejected:** `full` (vague — full of what?), `std` (collides with the standard library itself), `min` / `micro` (value judgment, and wrong once big freestanding programs exist). `hosted` is the C standard's term for "an OS is present" — precise and self-explanatory. The concurrency API needs no profile-specific naming: `task.*` means the same thing everywhere; only availability differs.

---

## 9. Operator Overloading (D10)

### 9.1 Reversing the Zig-inherited rejection

Base spec §1.2 lists "no operator overloading" under readable-by-default, inherited from Zig's philosophy rather than derived from Ryo's own goals. Following the stated-principle-reversal protocol established by the projections proposal (§2.1 of that document), the reversal is recorded with its defense:

1. **The reviewer test cuts the other way for arithmetic.** The rejection targets *abuse* — C++'s `cout <<`, hidden semantics behind innocuous symbols. But `total = price * qty + tax` is *more* reviewable than `total = price.mul(qty).add(tax)`: notation is not magic when semantics stay arithmetic. The thing a reviewer must be able to see is *which* operation runs — and in a statically-typed language with no implicit conversions, the operand types say so.
2. **Ryo's own roadmap feels the cap.** Distinct types (§4.14) force `Velocity(float(distance) / float(time))`; constrained types decay to their base type on any arithmetic; a financial `Decimal`, embedded register math, and the analytics/numeric domains opened by D3/D5/D9 are all ergonomically blocked. The Python-domain analysis identified this as a *permanent language-level* cap — D10 removes the cap without claiming the ecosystem.
3. **Precedent says the bounded form is safe.** Rust's trait-based overloading (`Add`, `Sub`, …) has years of evidence: abuse is rare, semantics stay statically dispatched and visible, and the failure modes the Zig rejection fears come from features Ryo never had (user-defined operator symbols, cross-type surprise overloads, implicit conversions feeding operators).

### 9.2 The bounded design

1. **Fixed operator set only.** The existing arithmetic and comparison operators (`+ - * / %`, `== != < > <= >=`), unary negation, and indexing `[]`. No user-defined symbols, no new operators, no precedence or associativity changes. The grammar is frozen.
2. **Trait-based, static dispatch.** `trait Add: fn add(self, rhs: Self) -> Self` and siblings; operators are sugar for trait calls, monomorphized — zero runtime cost, no dynamic dispatch. `Eq` is already a compiler-known trait (§4.5); the arithmetic traits follow the same path and promote identically.
3. **Same-type operands.** `a + b` requires `typeof(a) == typeof(b)`. No cross-type overloads — this removes the structural ambiguity vector (and the iostream failure mode). Mixed forms like `matrix * scalar` are **deferred** (Q8): today, spell the conversion explicitly.
4. **Built-ins unchanged.** `str + str` remains excluded — f-strings and `push_str` are the idiom, and the common `+` path keeps its no-hidden-allocation property. Machine `int`/`float` semantics are untouched; division by zero still panics.
5. **Value semantics.** Operator methods borrow their operands and return a new owned value — trivially compatible with the projection rules (no view returns involved).
6. **No implicit conversions introduced** — reaffirmed verbatim; overload resolution never triggers coercion.

Residual risk, stated honestly: semantic abuse (`+` that saves to a database) remains *possible*, exactly as in Rust. The fixed set, same-type rule, and review visibility make it self-limiting; Ryo accepts the residue rather than punishing every numeric type for it.

### 9.3 What it unlocks

- **Distinct types with notation:** `impl Add for Meters` — zero-cost units that read like physics, the §4.14 value proposition completed.
- **Stdlib numeric types:** `Decimal`, `Complex`, `BigInt` as ordinary libraries (with D4 where needed).
- **Third-party linear algebra / fixed-point / register-level types:** the language no longer forbids the idiom; ecosystem maturity remains the real gate.
- **Constrained types** may define arithmetic that revalidates bounds on construction, per §4.13's existing rule.

---

## 10. Anonymous Structs, the Binding Rule & Brace-Law Construction (D11)

*Adopted pre-implementation (M9/M10 not started; zero user breakage). Recorded as a formal reversal of base-spec §4.2/§4.3 syntax, with defense and bounds, per the escalation protocol established for D1 and D10.*

### 10.1 The two laws

> **The Binding Rule:** `=` binds a name to a value. `:` never binds — it *relates* (name↔type, header↔block, key↔value, position↔position).
>
> **The Brace Law:** parens group positionally; braces group by name.

The Binding Rule already holds across the entire base spec — assignment, named arguments, defaults, destructuring, type aliases all bind with `=`; type ascription, block colons, match arms, slices, format specs all relate with `:` and bind nothing. The biconditional is the law's strength: *everywhere a name gets a value, the mark is `=`; everywhere the mark is `:`, no name gets a value.*

**Corollary (the field/key rule):** struct fields are **names** (accessed as identifiers, compiler-known) → `=`; map keys are **data** (accessed as runtime values) → `:`. Scope: the law governs bindings, types, scopes, and pairings; bracket/string micro-syntax (slice ranges, format specs) satisfies "never binds" without being governed by it.

### 10.2 One grouping type: the anonymous struct

Ryo has exactly **one** ad-hoc grouping type — the anonymous struct:

```ryo
# value literal: = (fields are names — the Binding Rule)
point  = {x=1, y=2}
packet = {header=sb[0:4], payload=sb[4:]}

# type literal: : (field declarations, mirroring struct bodies)
fn divmod(a: int, b: int) -> {q: int, r: int}

# tuple sugar: positional syntax for fields named "0", "1" — UNCHANGED surface syntax
pair = (17, "alice")              # ≡ {0=17, 1="alice"}
(q, r) = divmod(17, 5)            # positional destructure (paren-less allowed)
print(f"{pair.0}")                # positional access per base-spec §4.3

# named destructuring: punning and renaming
{q, r}       = divmod(17, 5)
{x = quot}   = divmod(17, 5)

# match patterns
match point:
	{x=0, y=_}: "on y-axis"
	(0, _):     "tuple form"
```

Rules:

1. **Identity is structural:** same field names, same types, same order. **Exact match only** — no subtyping, no width rules. Structural typing applies *only* to anonymous structs; named structs remain nominal forever.
2. **Construction of named types uses the same literal with the nominal tag:** `Point{x=1, y=2}`. Named enum payloads follow: `Shape.Circle{radius=5}`; positional enum payloads keep parens (`Color.RGB(255, 0, 0)`).
3. **No implicit coercion** between anonymous and named structs. Graduating a value from anonymous to named is an explicit, greppable act.
4. **Usage rule:** crossing a boundary (public API, package, long-term storage) or carrying domain meaning → named; local/transient (multi-return, unpacking, throwaway shapes) → anonymous. A lint flags anonymous structs returned from public functions of other packages ("consider naming this shape") — style by tooling, semantics by the compiler.
5. **`{}` is reserved** for the future empty map literal (Python intuition). There is no empty anonymous struct — use `none`.
6. Single-element tuple keeps Python's trailing comma: `(x,)`. Trailing commas allowed in all literals.

### 10.3 Rationale

- **Fewer concepts, kept familiarity.** The tuple type constructor is removed; the Python-familiar surface (`(q, r) = divmod(...)`, `t.0`) survives as sugar. Named groupings gain self-documenting returns (`result.q` over `result.0`).
- **Construction is now visibly distinct from calls** (reviewer test): `Point{x=1, y=2}` cannot be mistaken for `fetch(x=1, y=2)`. Construction allocates, establishes invariants, and creates ownership — explicit where intent matters (§1.2).
- **Synergies already recorded:** DB rows as `.{id: int, name: str}` (data-layer proposal, `comptime`-checked later); `list[{id: int, score: float}]` with v0.3 generics; field-name serialization (JSON objects, not positional arrays).

### 10.4 Rejected alternatives

| Alternative | Why rejected |
|-------------|--------------|
| **P1** — `{x=1}` anon + `Point(x=1)` call-style named | Construction hides among function calls; two syntaxes for one intent |
| **P3** — colon everywhere (`{x: 1}` struct, `{"a": 1}` map, `map{}` empty) | Inverts Python's `{key: 1}` (variable-key map) into a struct — a **silent semantic flip** for the target audience; requires an escape hatch for variable-key maps; `map{}` wart. Violates the Binding Rule's corollary (fields are names, keys are data) |
| **P4** — parens anon structs `(x=1, y=2)` | **Argument-position collision:** `g(x=1, y=2)` is indistinguishable from named call arguments — the exact pressure that forced Python's dict braces. Braces are load-bearing, not aesthetic; and braces enter the language regardless (map literal) |

### 10.5 Conformance edits to the base spec

1. **§3 (lexical/structure):** record the Binding Rule as a named principle.
2. **§4.2:** struct literal syntax → `Name{field=value}`.
3. **§4.3:** rewrite the tuple section — tuples as positional sugar for anonymous structs; destructuring forms per §10.2.
4. **§4.5:** named enum payloads → `Variant{field=value}`.
5. **§3.1 (destructuring):** add `{q, r}` punning and `{x = quot}` rename forms; allow paren-less positional unpacking.
6. **`alpha_scope.md`:** M10 (tuples) folds into M9 (structs — shared construction/layout machinery; anonymous structs need no declarations or defaults).

---

## 11. GUI Strategy (Deferred-Feature Governance)

*This section is strategy, not a language decision: it commits to no new syntax. It exists so that GUI support is pursued through framework architecture on already-decided mechanisms, and so that `yield` keeps a measured re-entry condition rather than being promoted by anticipation.*

### 11.1 Why Swift wins at GUI — precisely

"Swift is used for GUI because of ARC" is half the story. The full mechanism has three layers, none of which requires *user code* to hold borrows:

1. **The retained graph is framework-internal.** The real widget tree (AppKit/UIKit objects; SwiftUI's shadow render tree) lives inside the framework, managed by ARC with `weak` back-references to break cycles.
2. **Declarative views are value types.** The user-written view tree is cheap structs, rebuilt on state change and diffed by the framework; identity and lifetime belong to the framework.
3. **Observation is library + macro sugar** (`@State`, `@Observable`): reference cells that invalidate the view on write.

### 11.2 The Ryo mapping — no language changes required

| GUI need | Swift | Ryo (already decided) |
|----------|-------|----------------------|
| Framework-internal retained graph | ARC classes + `weak` | `shared[T]` / `weak[T]` / `unowned[T]` (§5.6 — explicitly Swift-inspired) |
| Platform bindings | Obj-C interop | `ryo-bindgen` + D4 gated `unsafe` (v0.2) |
| Event loop / async UI work | GCD, async/await | UI green thread `select`ing on event + state-change channels (§9.2) |
| Graph cycles | `weak` | `weak[T]` |

Two user-side paradigms fit Ownership Lite **today**, with zero extensions:

- **Immediate mode (egui-style).** Per-frame `ui.window("Tools", fn(ui): ...)` closures are invoked synchronously within the call — ordinary Rule 7 borrows, nothing stored. Interaction state lives in framework-owned storage keyed by **IDs** — Rule 6's own stated idiom. Proven at scale by Rust's egui, notably in **game tooling, a stated Ryo target domain**.
- **Model-View-Update (Elm/Iced-style).** `update(inout model, msg)` is the `inout` convention as designed; messages are enums — ADTs plus **exhaustive `match`** make Ryo arguably the best-fit mainstream language for MVU; `view(model) -> WidgetNode` returns an **owned description tree** diffed by the framework, so escape rules E1–E3 never engage; reactivity flows through channels the UI task `select`s on.

Retained-mode OOP widget trees (user-owned widgets, stored callbacks, parent/child cycles) are deliberately *not* the target: that paradigm fights every ownership language, including Swift — which is why the industry is leaving it.

### 11.3 Honest residual pains

1. **Stored handler closures** capture `shared[AppState]` clones — ceremony parity with Swift's `[weak self]`, more explicit, same shape.
2. **View-function copying:** owned `str` per text widget per rebuild. Noise for forms; measurable for 60fps, 10k-row tables.
3. **Binding/observation sugar** needs `comptime` attribute machinery (roadmap), not memory-model work.
4. **No result-builder DSL** (no macros, by design): closure-children or `move self -> Self` builders (§5.2.1) instead — more verbose, no magic.

### 11.4 The position of `yield`

Hylo-style `yield` addresses exactly one pain: **11.3.2**. A view function could *lend* a widget-description tree that borrows the model (`Text(&model.name)`) for the render pass, with the framework consuming (diffing) it before the yield region ends — zero-copy declarative views without violating non-escape. It does nothing for 11.3.1/3/4. **`yield` therefore stays in Deferred (§12) with a measured re-entry condition:** demonstrated view-copy cost in the Phase-2 framework, not architectural anticipation.

### 11.5 Sequencing

1. **Phase 1 — Immediate-mode toolkit PoC** (post-v0.2: D4 `unsafe` internally, `ryo-bindgen` for platform APIs). Validates ID-keyed state and the green-thread event loop; independently justified by the game-tooling domain.
2. **Phase 2 — MVU framework** with owned description trees and exhaustive-match message handling. Instruments view-function copy cost.
3. **Phase 3 — only on measurement:** `yield` RFC (bounded lending to a consumer region) + `comptime` observation sugar.

Mobile note: iOS and Android are **`hosted` environments** under D9 — the full runtime applies; the gate is bindings and framework work, not the runtime.

---

## 12. Deferred and Rejected (For the Record)

| Item | Disposition | Rationale |
|------|-------------|-----------|
| Universal ARC + COW for `str`/`list[T]` | **Rejected** | Hidden per-operation cost on all code; data-dependent mutation cost fails the reviewer test; COW stale reads convert semantic hazards into warnings. Superseded by `sbytes` (opt-in). |
| Binary pattern-matching syntax (`match bytes[...]`) | **Deferred** | Real value for protocol parsing, but "no magic syntax when functions suffice." Prototype as a stdlib parser facility first; revisit syntax only on demonstrated demand. If adopted, bindings must be projections (`bytesview`) or `sbytes` — never a third sharing model. |
| Mutable view types (`inout` slices as bindable types) | **Rejected (reaffirm M8.3)** | Reopens the `MutRef` question M8.3 settled; aliasing mutable views are Go's hazard, not Ryo's model. Mitigation: range-based `inout` stdlib APIs (Q4). |
| `yield`-style subroutines for escaping projections | **Deferred** | The elegant endgame for returning views without annotations or ARC. Significant language machinery; revisit only if `sbytes` proves insufficient in practice. |
| Mojo-style origin parameters (`Span[T, origin]`) for escaping views | **Rejected (D8, §2.6)** | Origins are lifetimes under another name: viral through signatures, inference failures reproduce Rust's diagnostic class, and the reviewer test fails. Ryo pays a bounded opt-in runtime cost (`sbytes`) instead of unbounded signature complexity. |
| Negative indexing | **Rejected (reaffirm §4.7)** | Footgun avoidance; `.last()` etc. cover the use cases. |
| `eval` / runtime code evaluation | **Rejected (identity, permanent)** | A runtime facility that compiles/interprets arbitrary strings inside a running program dissolves every static guarantee Ryo sells (ownership, exhaustiveness, error unions, the reviewer test) and ships the compiler in every binary. **Not to be confused with the REPL** (v0.2, Cranelift JIT, hosted profile): the REPL is toolchain-side incremental compilation of *fully checked* code — every line passes the complete front-end before execution. Precedents for compiled-language REPLs without `eval`: Swift, Mojo, GHCi, evcxr. Eval-shaped needs are served by: expression-language crates (config/rules), t-strings (SQL/HTML/templates — the injection-safe anti-eval), embedded interpreter crates (plugins, via D4), and toolchain hot-reload. |

---

## 13. Use-Case Matrix (Final)

| Use case | Status | Mechanism |
|----------|--------|-----------|
| Read-only params (`fn f(s: strview)`) | ✅ | Projections, implicit view conversion |
| Local slicing / scanning / lexing / parse loops | ✅ | Projections (P1–P6) |
| Iterator & transformation chains | ✅ | Scope-locked views (§5.7) |
| JSON parse → structs / DOM; JSON serialize | ✅ | Views in, owned out; `inout` buffer sink |
| View–Controller–DB–response pipelines | ✅ | Owned across boundaries, views within stages; t-string SQL/HTML |
| Web backends, CLI, network services, Wasm, batch ETL | ✅ | Target domains; model validated |
| Zero-copy packet structs / protocol parsing | ✅ (D2+D3) | `bytes` + `sbytes` fields |
| Caches of views over shared buffers; columnar analytics | ✅ (D3) | `sbytes` |
| Numeric / financial / scientific notation (`Decimal`, units, matrices) | ✅ (D10) | Bounded operator overloading on library types; ecosystem maturity is the real gate |
| Fork-join data parallelism | ✅ (D5) | `task.scope` borrows + stdlib `par_*` |
| Third-party systems libraries (containers, FFI wrappers, concurrency primitives) | ✅ (D4) | Gated, documented `unsafe` |
| `first_word(s: strview) -> strview` | ⚠️ Idiom | Return `(offset, len)`, owned value, or invert control (closure) |
| String interning / symbol tables | ⚠️ Idiom | Arena + ID indices |
| Mutable sub-range algorithms | ⚠️ Idiom | `inout` + range APIs (Q4) |
| GUI applications (desktop / mobile) | ⚠️ Framework-dependent (§11) | Phase 1: immediate-mode, zero language changes; Phase 2: MVU with enum messages; Phase 3: `yield` only if measured. Mobile runtimes are `hosted` — gate is bindings, not runtime |
| WebAssembly applications | ✅ improved (D9) | `core` profile: no TLS runtime, no scheduler in the binary |
| Linux-class embedded (Raspberry Pi, RISC-V SBCs, Cortex-A gateways) | ✅ | `hosted` profile; Cranelift AArch64/RISC-V targets |
| Bare-metal MCUs (AVR, Xtensa, Cortex-M) — Arduino-class | ❌ today | Runtime blocker removed by D9 (`core` profile); **still gated on an LLVM backend** (Q6). Not promised; revisitable when the backend exists |
| HFT, real-time A/V | ❌ | Excluded by design (§1.1), unchanged |

---

## 14. Milestone Mapping

| Item | Milestone | Dependencies |
|------|-----------|--------------|
| `strview`, slice expressions, P1–P6, E1–E4, implicit view conversion | 8.4 | Ownership-pass extensions (§3.5.3); spec wording edits (§3.5) |
| `str(view)` materialization, W0003 RedundantMaterialize (§3.4.1) | 8.4.1.2 | M8.4.1 re-borrow |
| Anonymous structs, tuple sugar, `Name{...}` construction (D11) | M9–M10 | Parser grammar (§10.2); base-spec edits (§10.5); zero implementation cost pre-alpha |
| `bytes` type, `bytesview`, builder API | 8.4.2 | D1 |
| Conformance edits to spec §4.4/§5.7 | 8.4 | — (ship with the feature) |
| `slice[T]` over fixed arrays `[T]` | 21 | `[T]` array type |
| `for x in slice` iteration | 22 | for-over-iterable |
| `unsafe` policy revision (manifest gating, `SAFETY:` enforcement, `ryo audit`) | v0.2 | Lands with FFI/unsafe work |
| `sbytes` (ARC buffer, slicing, COW warnings) | v0.2–v0.3 | `shared[T]` atomic-refcount runtime (§5.6, already specified); slicing + COW machinery |
| Scoped task borrows + stdlib `par_*` | v0.4 | Concurrency runtime |
| Runtime profile split: stdlib layering, `--profile=core`, panic/trace gating | v0.2 | Stdlib architecture work; **no backend changes required** |
| LLVM backend (bare-metal enabler) | Post-v0.4 / community-driven | Q6; not a commitment |
| Operator overloading: arithmetic traits (`Add`…), same-type rule | v0.2–v0.3 | v0.2 for concrete types; generic operator traits with user generics (v0.3) |
| GUI Phase 1: immediate-mode toolkit PoC (ecosystem, not language) | Post-v0.2 | D4 gated `unsafe`; `ryo-bindgen` platform bindings (§11.5) |
| Binary pattern matching (if adopted) | Post-v0.4 | Evidence from stdlib parser usage |
| `yield` subroutines (if ever) | Separate RFC | — |

---

## 15. Open Questions

- **Q1 — Naming:** `sbytes` vs. making `shared[bytes]` sliceable. `sbytes` is terser and reads as one concept; `shared[bytes]` reuses existing vocabulary. Non-blocking; decide at implementation.
- **Q2 — COW warning default level:** `warn` for all sizes, or `info` below a small-buffer threshold (the binary proposal suggested 64 bytes)? Decide with real telemetry.
- **Q3 — `sstr`:** defer until `shared[str]` pain is demonstrated (§5.3).
- **Q4 — Mutable sub-ranges:** is `inout` + explicit range parameters (`f(&buf, start, end)`) sufficient for codec-style code, or does the stdlib need a dedicated `RangeMut[T]` opaque token (definably safe, implemented via D4)? Prototype in stdlib first.
- **Q5 — `&` in type position:** **RESOLVED — `strview` / `bytesview` / `slice[T]`** (word family; `&` remains exclusively the `inout` call-site marker). The collision that forced the rename was flipped mutability: `&` at a call site (`f(&x)`) marks a *mutable* borrow, so spelling the *read-only* view type `&str` gave one sigil opposite mutability meanings in type position versus call position — a permanent teaching and review hazard. Shipped in M8.4.1: legacy `&str` in type position is a targeted migration error ("`&str` was renamed to `strview` (final spec Q5)").
- **Q6 — LLVM backend:** the sole remaining gate for Arduino-class bare metal (D9 removed the runtime gate). Commitment, timing, and ownership (core team vs. community) are deliberately undecided — the `core` profile is justified by Wasm and small binaries on its own merits, so this question is free to wait for real demand.
- **Q7 — Profile naming ratification:** `core` / `hosted` is the recommendation (§8.3); confirm before the v0.2 stdlib layering lands, since the names will appear in manifests and error messages.
- **Q8 — Mixed-type operator forms:** `matrix * scalar`, `Decimal * int` — deferred by D10's same-type rule (§9.2 rule 3). Revisit only with a concrete proposal (broadcast traits? explicit widening?) after the same-type core ships and real friction is measured.
- **Q9 — Field punning in anonymous-struct construction:** `{x, y}` meaning `{x=x, y=y}` — deferred (§10.2 allows punning in *destructuring* only). The shape collides with the reserved set-literal space `{1, 2}`; revisit if sets take a different syntax or demand proves out.

---

*End of Document*
