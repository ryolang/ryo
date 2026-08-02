# View Materialization in Ryo — Decision Record, Milestone Plan, and Use-Case Examples

> **Status: decision record — DECIDED (`str(view)`, user decision 2026-08-01);
> IMPLEMENTED in M8.4.1.2 (2026-08-02).** Captures the design discussion of
> 2026-07 on copying views: what operation is needed, what it is called, when
> it ships. The 2026-07 discussion concluded `str.from(view)`; the decision
> flipped to the plain call form `str(view)` on 2026-08-01 — §2.1 records the
> evidence, §6 keeps `str.from` in rejected alternatives with the real reasons.
> Cross-references: slicing/memory final spec (view rules P1–P6, E0034
> ViewEscape, §3.4.1 materialization), `ryo-missing-features-and-gaps.md`
> (trait extensions), `ryo-agent-interface-proposal.md` (machine-applicable
> suggestions), `ryo-c-binding-guide.md` (`cstr.from` precedent).

---

## 1. The question

Views (`strview`, `bytesview`, `slice[T]`) are Copy values — 16 bytes, ptr +
len. Do we need a `view.copy()` method or `copy(view)` function?

**Two different operations hide inside the word "copy":**

| | Operation | Cost | Needed? |
|---|---|---|---|
| Copy the **view value** (ptr+len) | `b = a` | Free | Already exists via assignment — no API |
| Copy the **viewed data** into an owned value | `strview → str` | Allocates + memcpy | **Yes — this document** |

A `view.copy()` method was rejected: views being Copy types makes the name read as
"duplicate the pointer," and the operation it would name already exists for free.
`.to_owned()` (Rust's word) was rejected on audience grounds: Ryo's ergonomics
target developers coming from Python/Go/TS, and `to_owned` is Rust-community
vocabulary none of them bring.

## 2. The decision

**Two operations, two names, each matching what the target audience already knows.**

### 2.1 Type-changing materialization — plain constructor call

```ryo
owned = str(view)          # strview   → str    (shipped, M8.4.1.2)
data  = bytes(bview)       # bytesview → bytes  (ships with M8.4.2)
# slice[T] materialization defers to M21 (bit-copy restriction, §2.4)
```

**Decision history.** The 2026-07 discussion concluded the
associated-constructor form `str.from(view)`. On 2026-08-01 the decision
**flipped to the plain call form `str(view)`**, on cross-language evidence:
Swift `String(substring)`, Mojo `str(slice)`, and Go `string(b)` all spell
exactly this operation as a plain type-name call — and `int_to_str` already
holds the stringify niche in Ryo, so `str(...)` collides with nothing. The
trade-off is recorded honestly: the `*.from` family uniformity (`bytes.from`,
`[T].from`, `cstr.from`) is lost; precedent and readability won. `str.from`
now lives in §6 with the full reasons.

Rationale (carried over from the 2026-07 analysis, still valid):

- **Audience convergence**: `str(x)` (Python), `string(b)` (Go), `String(x)`
  (TS) — all three source ecosystems use constructor/conversion call syntax
  for exactly this operation.
- **Trait-forward** (corrects an earlier assumption that only methods are): the
  call can later resolve through a converting-initializer protocol (Swift
  `init(_:)`-style, or a `From`/`Materialize` trait with call syntax — name
  TBD at the trait milestone) without changing call sites. Methods on views
  stay at zero, keeping views "dumb" values (provenance in ownership-pass side
  tables, never in the type).
- **Zero method surface** on views means no invitation to treat a `strview` as
  an object with behavior.

`cstr.from(str, buf)` keeps constructor form regardless — it carries an extra
contract (null termination) plus an explicit buffer argument that plain call
syntax cannot express (§5.6).

### 2.2 Same-type duplication — future `Clone` trait (deferred)

```ryo
trait Clone:                       # ships with the trait system, not before
	fn clone(self) -> Self

impl Clone for Node:               # user types duplicate themselves
	fn clone(self) -> Node: ...
```

- Go's word (`slices.Clone`, `bytes.Clone`); `T → T` semantics; plain trait
  dispatch — Ownership Lite never special-cases it.
- No collision: `Clone` = same type, `str(view)` = type-changing, "Copy types"
  stays compiler-internal vocabulary.

### 2.3 The no-alloc path — `copy_into`

```ryo
buf: [64]u8
n = bytes.copy_into(view, &buf)   # explicit buffer, visible bounds, no allocator
```

Required for the `core` runtime profile (no allocator) and embedded; composes
with fixed-capacity containers (Odin `[dynamic; N]T` evidence).

### 2.4 Hard rules

1. **Never implicit.** The compiler never auto-materializes to silence an escape
   diagnostic. Allocation is always a visible, greppable call in source.
2. **Bit-copy restriction.** `slice[T]` materialization requires `T` to be
   trivially copyable. Views of owning values (`slice[str]`, `slice[Node]`) are
   materialized by explicit iteration or, later, user `Clone` impls — never by
   memcpy.
3. **Allocation source is the task-context allocator** (per the runtime-context
   design); the `copy_into` variant takes no allocator.
4. **No `Borrow` equivalent.** Rust's `ToOwned` is entangled with `Borrow`
   (hashmap lookup by borrowed key); Ryo takes only the standalone materialize
   protocol. Map-lookup-by-view, if ever wanted, is a separate RFC.

### 2.5 Division of labor with the M8.4.1 re-borrow

M8.4.1 gave views a free path into `str` parameters: the `cap=0` re-borrow
(final spec P6') manufactures a call-scoped `str` header over the view's
bytes — no allocation, valid for the duration of the call only.
Materialization covers what the re-borrow cannot: **escapes** — returns,
stores into longer-lived structures, moves into spawned tasks — and
**defensive copies** taken before the source is mutated.

The two mechanisms overlap at call sites, where `str(view)` would pay an
allocation the re-borrow avoids. Warning **W0003 `RedundantMaterialize`**
(shipped in M8.4.1.2) guards the overlap, in two shapes:

- **(a) Call-site redundancy:** a materialize call in an argument position the
  re-borrow already serves (borrowed `str` parameter, view-accepting builtin).
  Not fired for `move`/`inout` parameters — the re-borrow cannot feed those,
  so the copy is legitimate.
- **(b) Never-escapes binding:** a bound materialize result that never escapes
  while the view's root owner is never moved, mutated, or `inout`-passed
  afterward. Mutating the source later is a legitimate defensive copy (§5.4)
  and is not flagged.

W0003 is warnings-only, heuristic, and conservative: when unsure, no warning.

## 3. Diagnostic synergy — E0034 becomes machine-applicable (deferred)

The primary consumer of this feature is the compiler itself. Today, E0034
(ViewEscape) can only say "no." With materialization it can say "here is the
fix" — once the diagnostic machinery can carry suggestion payloads:

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

Per `ryo-agent-interface-proposal.md`, `machine_applicable` means the agent may
apply the edit without human confirmation. The memory model and the agent
interface reinforce each other: strict escape rules are acceptable *because* the
fix is one mechanical edit.

**Status: deferred.** M8.4.1.2 shipped `str(view)` without the suggestion —
the Diag suggestion-payload machinery does not exist yet. This belongs with the
agent-interface milestone.

## 4. Milestone plan

| Milestone | Deliverable | Rationale |
|---|---|---|
| **M8.4.1.2 ✅ COMPLETE (2026-08-02)** | `str(view)` + W0003 RedundantMaterialize | Shipped. The materialize API is the escape hatch that makes the escape rules livable; W0003 guards the overlap with the M8.4.1 re-borrow (§2.5). The E0034 suggestion stayed out (§3). |
| M8.4.2 (bytes) | `bytes(bview)` + `bytes.copy_into(bview, &buf)` | Ships with the `bytes` type itself; `bytes(bview)` mirrors `str(view)`. `copy_into` here because `bytes` is the core-profile string surrogate. |
| M21 (slice views `slice[T]`) | `slice[T]` materialization with the bit-copy check in sema | Lands with the slice-view machinery; sema enforces trivially-copyable `T` (needs the Copy-type classification from the ownership pass side tables). |
| Trait milestone (post-D10, v0.3 line) | `From`/`Materialize` + `Clone` traits; `str(view)` resolves through the converting-initializer protocol | Call sites unchanged. GAP entry already records the trait pair; remove it from open questions when scheduled. |
| Agent-interface milestone | E0034 machine-applicable suggestion (§3) | Needs Diag suggestion-payload machinery that doesn't exist yet. |
| Core-profile milestone | `copy_into` generalized to fixed-capacity containers | Blocked on fixed-capacity container types (Odin `[dynamic; N]T` candidate). |

**History note.** This section previously drafted a "Task 10.2" for insertion
into the M8.4 milestone file (`str.from(&str)` + E0034 suggestion wiring, with
file paths from an older tree layout). That draft was never inserted and is
removed: the feature shipped instead as **Milestone 8.4.1.2** — see
`implementation_roadmap.md` — with the call-form spelling `str(view)` and
W0003 in place of the E0034 suggestion wiring.

## 5. Use-case examples

### 5.1 The E0034 fix path — keeping parsed data past its buffer

The canonical case: a view into a reusable buffer must outlive the buffer.

```ryo
fn first_token(src: strview) -> str:
	tok = lex_next(src)         # tok: strview — a view INTO src
	return tok                  # E0034: view escapes its root owner
```

Fix — materialize (this is what the machine-applicable suggestion will produce):

```ryo
fn first_token(src: strview) -> str:
	tok = lex_next(src)
	return str(tok)             # owned copy; src may be freed after return
```

### 5.2 Lexer/parser — views by default, owned for the survivors

Tokens are views into the source (fast, allocation-free). Only the identifiers
that enter the symbol table are copied:

```ryo
while tok = lex_next(src):
	match tok.kind:
		Ident:
			if is_declaration(tok):
				symbols[tok.text] = ...   # E0034 — stored into a long-lived table
				# fix:
				symbols[str(tok.text)] = ...
		_:
			pass                      # 99% of tokens: zero allocation
```

The asymmetry is the point: views for the hot path, materialization for the
cold one, and the compiler tells you exactly where the boundary is.

### 5.3 Crossing a task boundary

Views cannot flow into spawned tasks except through scoped borrows (final spec D5). An owned
copy can be *moved*:

```ryo
fn handle(req: &Request):
	body = req.body             # strview — borrowed from the request buffer
	spawn log_async(str(body))  # moved: the task owns its copy
	# scoped-borrow alternative exists for short-lived tasks;
	# str(view) is for fire-and-forget
```

### 5.4 Defensive copy before buffer reuse

```ryo
ring = RingBuffer(4096)
while pkt = ring.read():
	header = pkt[..16]          # bytesview into the ring
	if is_interesting(header):
		queue.push(bytes(header))    # copy BEFORE the ring overwrites it (M8.4.2)
```

### 5.5 The no-alloc path (core profile / embedded)

```ryo
fn tag_frame(view: bytesview):
	buf: [32]u8
	n = bytes.copy_into(view, &buf)     # explicit buffer, no allocator
	if n == 0:
		return                          # didn't fit — visible, handled
	transmit(&buf[..n])
```

### 5.6 FFI adjacency (same idiom, extra argument)

```ryo
raylib.set_window_title(str)        # needs cstr
buf: [256]u8
raylib.SetWindowTitle(cstr.from(title, &buf))   # from + explicit buffer
```

`cstr.from` is the same operation with an extra contract (null termination) and an
explicit buffer — which is why it keeps constructor form even after traits exist:
trait methods cannot take the buffer argument.

### 5.7 (Future) Same-type duplication — where `Clone` will live

```ryo
# trait milestone, not M8.x
fn snapshot(n: Node) -> Node:
	return n.clone()            # T → T; user-written impl; plain dispatch
```

Shown here to mark the boundary: `clone()` is never how you escape a view —
`str(view)` is. If you find yourself wanting `view.clone()`, the operation you
mean is materialization.

## 6. Rejected alternatives (for the record)

| Candidate | Why rejected |
|---|---|
| `str.from(view)` — associated-constructor form (the 2026-07 decision) | Flipped 2026-08-01 on cross-language evidence: Swift `String(substring)`, Mojo `str(slice)`, and Go `string(b)` all spell this operation as a plain type-name call, and `int_to_str` already holds the stringify niche so `str(...)` collides with nothing. Trade-off accepted: the `*.from` family uniformity (`bytes.from`, `[T].from`, `cstr.from`) is lost — precedent and readability won. `cstr.from` keeps constructor form regardless (extra contract + buffer argument, §5.6) |
| `view.copy()` | Views are Copy values; name reads as pointer duplication; the operation it would name is free via assignment |
| `copy(view)` builtin | Same naming trap in function form |
| `.to_owned()` | Rust-community vocabulary; alien to the Python/Go/TS audience Ryo targets |
| `.clone()` on views | Clone semantics are `T → T`; materialization is `strview → str` — the name would lie about the signature |
| Implicit materialization on escape | Hidden allocation, unreviewable — violates the no-hidden-cost rule |
| `.dup()`, `.snapshot()`, `.owned()` | No audience brings intuition for them |
| `Borrow` companion trait | Rust's hashmap-lookup entanglement; out of scope, separate RFC if ever wanted |

## 7. Cross-document impacts

1. Slicing/memory final spec: **done (M8.4.1.2)** — materialization subsection
   §3.4.1: `str(view)`, never-implicit, re-borrow division of labor, W0003,
   trait-forward hook.
2. Diagnostic catalog: **W0003 RedundantMaterialize added** (M8.4.1.2). The
   E0034 machine-applicable suggestion remains deferred to the agent-interface
   milestone (§3).
3. `ryo-missing-features-and-gaps.md`: still open — record `From`/`Materialize`
   + `Clone` as a scheduled trait-milestone extension (not an open question).
4. Roadmap: **done** — the Milestone 8.4.1.2 entry supersedes the Task 10.2
   draft (§4); the M8.4.2 entry carries `bytes(bview)` as the mirror of
   `str(view)`.
