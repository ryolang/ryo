# `with` Enforcement — The `Scoped` Marker (Draft — M23)

**Status:** Design (Draft — M23, pending approval)

Proposal to generalize the spec's mandatory-`with` rule (§14.5.4, currently written only
for `mutex[T].lock()` / `rwlock` guards) into a user-accessible core trait, so any type
whose *acquisition* is the hazard can require consumption by a `with` block.

---

## Motivation

The spec already defines the behavior this proposal generalizes (§14.5.4):

> Synchronization guards ... cannot be bound by plain assignment; they must be
> consumed by a `with` block, so the critical section is always lexically explicit.

Today that rule is an ad-hoc property of three stdlib types. It cannot be expressed
by users, and it cannot be applied to other acquisition-shaped APIs. Four goals
motivate generalizing it:

1. **Explicitness.** Acquiring a resource should always be lexically visible
   (`with cache.lock() as m:`), never hidden in `m = cache.lock()`.
2. **Misuse prevention.** Some return values are meaningless or dangerous outside a
   `with` body. The type should be inert until consumed by `with`.
3. **Uniform must-consume pattern.** Ryo already has typed must-consume shapes:
   `Future` is consumed by `.await`; a `Scoped` value is consumed by `with`. One
   rule per statement form, enforced by the type rather than convention.
4. **Zero loss from the ownership model.** Rules 5/6 (no borrowed fields, no escape)
   already make a guard token non-escaping: it cannot be stored in a struct field,
   returned, or sent anywhere. Forbidding plain binding therefore removes no
   legitimate use — it only removes the misleading one.

Note what this proposal does *not* claim: in Ryo, `with` is not a safety requirement
the way Python's `with` is. Python needs context managers because cleanup is
GC-dependent; Ryo destroys `Drop` types lexically at scope end (§5.4), so
`f = File.open(...)` still closes reliably. The gap `with` closes is *when* (early,
at a chosen boundary) and *how visible* the acquisition is — not *whether* cleanup
happens. Enforcement is therefore about explicitness and hazard shapes, not leak
prevention.

## Proposal

A marker trait in core, beside `Drop`:

```ryo
# Core trait (compiler-known, like Drop in v0.1; promoted to a real trait with
# the v0.2/v0.3 trait system without breaking source compatibility).
impl Scoped for MutexGuard[T]
```

A value whose type implements `Scoped`:

- **Cannot be bound by plain assignment** — `g = cache.lock()` is a compile error.
  The binding restriction is the same enforcement shape already used for `never`
  (E0017, `VoidValueInExpression`): a small set of types the compiler refuses to
  bind.
- **Can only be consumed as a `with` target** — `with cache.lock() as g:` binds it
  inside the block, where it behaves as an ordinary in-scope value.
- **Implies `Drop`.** `Scoped` without `Drop` is a compile error; the cleanup the
  `with` block guarantees must exist.

### What stays normal

`Scoped` is opt-in per type. Ownable, storable resources do **not** implement it:

```ryo
# File is a normal ownable value: storable in a struct, closed by Drop at scope end.
struct Logger:
	f: File

impl Drop for Logger:
	fn drop(move self):
		self.f.close()     # field drop runs first; this is belt-and-braces
```

Making `File`/`Connection` `Scoped` would forbid `struct Logger: f: File`, which is a
real pattern with no hazard. `with File.open(...) as f:` remains the idiomatic
early-close form (§5.5) without being mandatory. Hazard comes from acquisition, not
ownership: a held file is inert, a held lock blocks everyone.

### Who implements `Scoped`

| Type | Rationale |
|---|---|
| `MutexGuard[T]` (§14.5.4) | Already spec'd as `with`-only; gains the trait as its formal mechanism |
| `RwLockReadGuard[T]`, `RwLockWriteGuard[T]` (§14.5.4) | Same |
| `PoolCheckout[T]` (optional, per pool type) | A forgotten checkout stalls the pool; acquisition is the hazard |
| `File`/`Connection`/transactions | **Not** `Scoped` — see "What stays normal" |

### Diagnostics

New error code, mirroring E0017's shape:

```text
error[E0XXX]: cannot bind 'Scoped' value to 'g'
  --> example.ryo:3:5
  |
3 |     g = cache.lock()
  |     ^ `MutexGuard[int]` implements `Scoped` and must be consumed by a `with` block
  |
help: wrap the acquisition in `with`
  |
3 |     with cache.lock() as g:
4 |         ...
  |
  = note: see §5.5 — `with` guarantees cleanup on every exit path
```

Machine-applicable suggestion payload rides the deferred suggestion machinery noted in
the roadmap Phase 5 table (same dependency as E0034's fix).

### Compiler impact

- **Sema, not the ownership pass.** The check is purely local: a variable-declaration
  binding whose initializer's resolved `TypeId` carries the `Scoped` facet. No flow
  analysis, no liveness interaction. `Scoped` values that pass the binding check are
  ordinary in-scope `Drop` values from the ownership pass's perspective (lexical
  destruction at block exit, per §5.4 rule 2).
- **Facet, not a parallel hierarchy.** `Scoped` is a boolean facet on `TypeId`
  metadata, like the `Drop` facet M23 already tracks; no new IR instructions, no
  codegen surface.
- **Stdlib surface.** The three guard types in §14.5.4 implement it the moment the
  facet exists; §14.5.4's prose becomes a reference to the trait rather than a
  special case.

## Alternatives considered

- **First-class must-consume value (Future analogy).** A bindable, movable
  `acquired[T]` value that must eventually reach a `with`, enforced by flow analysis
  in the ownership pass. More flexible (can route through a helper), but pays real
  compiler complexity for expressiveness Rules 5/6 mostly forbid — guards cannot
  escape their scope anyway. Deferred unless a demonstrated need appears.
- **Warning-only (W-series lint).** Keep binding legal; warn when a `Scoped`-capable
  value is bound and used far from its binding. Warnings drift into noise and prevent
  nothing; rejected for the guard case §14.5.4 already made an error.

## Open questions

1. **Name.** `Scoped` (reads naturally: "a scoped value"), `WithOnly` (mechanical,
   matches the rule), or `Guard` (evokes the dominant use). Recommendation: `Scoped`.
2. **Explicit close inside the block?** May a `Scoped` value be consumed early by an
   explicit `close(g)` / `drop(g)` call inside the `with` body, or is block exit the
   only legal destruction point? Recommendation: allow it — the value is an ordinary
   value once bound; `Scoped` restricts *where binding may occur*, not what the body
   may do.
3. **`with` on `Scoped`-typed expressions in non-binding position** — e.g. bare
   `with cache.lock():` (guard unused inside). Legal and useful (lock a region,
   touch nothing); no special rule needed.

## References

- Spec: [Section 5.4](../specification.md) (Hybrid Eager Destruction & RAII — lexical destruction of `Drop` types)
- Spec: [Section 5.5](../specification.md) (`with` — Resource Lifetime Blocks)
- Spec: [Section 14.5.4](../specification.md) (Synchronization Primitives — the guard rule this proposal generalizes)
- Spec: [Section 6.1.3 / E0017](../specification.md) (`never` binding restriction — the enforcement precedent)
- Dev: `concurrency.md` milestone summary ("`with` enforcement", Phase 6)
- Milestone: RAII & Drop (M23) — see implementation_roadmap.md
