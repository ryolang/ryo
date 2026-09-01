**Status:** Design (v0.2+) — shipped portions complete; this document tracks only the remaining work.

# View Materialization in Ryo — Remaining Work

> **Shipped:** `str(view)` (M8.4.1.2, 2026-08-02) and `bytes(bview)` (M8.4.2,
> 2026-09-01) — explicit, greppable, allocating copies out of `strview` /
> `bytesview` — plus warning **W0003 `RedundantMaterialize`** guarding the
> overlap with the M8.4.1 `cap=0` re-borrow. The plain-call spelling `str(view)`
> was decided 2026-08-01 on cross-language evidence (Swift `String(substring)`,
> Mojo `str(slice)`, Go `string(b)`), superseding the 2026-07 `str.from(view)`
> conclusion. Design and completion notes: `implementation_roadmap.md`
> (M8.4.1.2 / M8.4.2) and final spec §3.4.1. Everything below is still pending.

---

## 1. `slice[T]` materialization — Milestone 21

`slice[T]` materialization lands with the slice-view machinery. The bit-copy
restriction is enforced in sema: `T` must be trivially copyable (needs the
Copy-type classification from the ownership-pass side tables). Views of owning
values (`slice[str]`, `slice[Node]`) are materialized by explicit iteration or,
later, user `Clone` impls — never by memcpy.

## 2. `From`/`Materialize` + `Clone` traits — trait milestone (v0.2/v0.3)

Two pending trait extensions:

- **Trait-forward resolution.** `str(view)` / `bytes(bview)` later resolve
  through a converting-initializer protocol (Swift `init(_:)`-style, or a
  `From`/`Materialize` trait with call syntax — name TBD at the trait
  milestone) without changing call sites. Methods on views stay at zero,
  keeping views "dumb" values (provenance in ownership-pass side tables, never
  in the type).
- **Same-type duplication** — a user-facing `Clone` trait:

```ryo
trait Clone:
	fn clone(self) -> Self

impl Clone for Node:               # user types duplicate themselves
	fn clone(self) -> Node: ...
```

Go's word (`slices.Clone`, `bytes.Clone`); `T → T` semantics; plain trait
dispatch — Ownership Lite never special-cases it. No collision: `Clone` = same
type, `str(view)` = type-changing. The boundary holds after traits land:
`clone()` is never how you escape a view — `str(view)` is. If you find
yourself wanting `view.clone()`, the operation you mean is materialization.

## 3. `bytes.copy_into` — the no-alloc path (core profile, v0.2)

```ryo
buf: [64]u8
n = bytes.copy_into(view, &buf)   # explicit buffer, visible bounds, no allocator
```

Required for the `core` runtime profile (no allocator) and embedded; composes
with fixed-capacity containers (Odin `[dynamic; N]T` evidence). Blocked on
fixed-capacity container types; scheduled in the roadmap's Phase 5 deferred
table. Later generalizes `copy_into` to fixed-capacity containers.

Example — bounded frame tagging without an allocator:

```ryo
fn tag_frame(view: bytesview):
	buf: [32]u8
	n = bytes.copy_into(view, &buf)     # explicit buffer, no allocator
	if n == 0:
		return                          # didn't fit — visible, handled
	transmit(&buf[:n])
```

FFI adjacency (same idiom, extra argument):

```ryo
title: str = "Ryo"                  # raylib wants a cstr
buf: [256]u8
raylib.SetWindowTitle(cstr.from(title, &buf))   # from + explicit buffer
```

`cstr.from` is the same operation with an extra contract (null termination)
and an explicit buffer — which is why it keeps constructor form even after
traits exist: trait methods cannot take the buffer argument.

## 4. E0034 machine-applicable suggestion — agent-interface milestone

Today E0034 (ViewEscape) can only say "no." With materialization it can say
"here is the fix" — once the diagnostic machinery can carry suggestion
payloads:

```json
{
  "code": "E0034",
  "message": "view 'tok' escapes the scope of its root owner 'src'",
  "suggestions": [
    {
      "message": "materialize the data into an owned value",
      "replacement": "str(tok)",
      "applicability": "machine_applicable"
    }
  ]
}
```

Per the agent-interface proposal
(`docs/experimental/ryo-agent-interface-proposal.md`), `machine_applicable`
means the agent may apply the edit without human confirmation. The memory
model and the agent interface reinforce each other: strict escape rules are
acceptable *because* the fix is one mechanical edit. Blocked on the Diag
suggestion-payload machinery, which does not exist yet.

## 5. Hard rules (apply to all pending materialization work)

1. **Never implicit.** The compiler never auto-materializes to silence an
   escape diagnostic. Allocation is always a visible, greppable call in source.
2. **Bit-copy restriction.** `slice[T]` materialization requires `T` to be
   trivially copyable (§1).
3. **Allocation source is the task-context allocator** (per the
   runtime-context design); the `copy_into` variant takes no allocator.
4. **No `Borrow` equivalent.** Rust's `ToOwned` is entangled with `Borrow`
   (hashmap lookup by borrowed key); Ryo takes only the standalone materialize
   protocol. Map-lookup-by-view, if ever wanted, is a separate RFC.

## References

- Spec: final slicing & memory spec §3.4.1 (materialization), view rules P1–P6, E0034 ViewEscape (`ryo-slicing-and-memory-model-final-spec.md`)
- Dev: `docs/experimental/ryo-agent-interface-proposal.md` (machine-applicable suggestions)
- Milestone: M8.4.1.2 and M8.4.2 (shipped); M21, Traits & Generics, and the Phase 5 deferred table (pending) — see `implementation_roadmap.md`
