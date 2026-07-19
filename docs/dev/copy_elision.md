# Copy Elision and Return Value Optimization

**Status:** Design (v0.2+). Partial implementation starts at milestone
Copy Elision & NRVO (Phase 5, v0.2+).

**Current status (2026-07):** parts of the contract this document
defines are already true in the compiler, and verified by the M8.1–M8.3
work:

- **The destination-slot mechanism exists.** `str` returns already use
  the hidden sret output pointer described in the Algorithm Sketch —
  the same calling-convention shape the G1/G2 guarantees will build on.
- **G3 behavior is implemented.** Move parameters receive the original
  buffer (a fat-pointer move, no data copy), and reusing a moved
  binding is a compile error (E0020). Forwarding through a `move`
  chain requires `move` in each signature, as G3 states.
- **F2's note is verified.** `inout` parameters cannot be returned
  (E0022) or stored, so a mutable borrow provably never survives the
  call — the F2 alias is unconstructible.
- **The `inout` write-back ABI (M8.3)** passes a pointer to the
  caller's slot for mutable borrows — the second place the
  destination-slot discipline already exists.

Still future: the dedicated elision pass itself (G1/G2 guarantees,
the P1–P4 permitted cases, and the safety verification of step 4).

## Purpose
Defines the compiler's contract for eliding copies on function return
and parameter passing. Section 5.9 of the specification promises
certain elision guarantees; this document defines exactly which.

## Guaranteed Elision

The compiler MUST elide the copy in these cases. These are observable
contracts — code relies on them for correctness, not just performance.

### G1: Direct return of a local binding
When a function returns a binding that is not used after the return
statement, the binding's storage is the caller's destination slot.

```ryo
fn make() -> str:
	s = build_string()
	return s     # G1: s is written directly to caller's slot
```

### G2: Return of a constructed literal
When a function returns a struct or collection literal directly, the
literal is constructed in the caller's slot.

```ryo
fn origin() -> Point:
	return Point(x=0, y=0)    # G2: no temporary
```

### G3: Last-use move into `move` parameter
When a caller passes a binding as the last use to a `move` parameter,
the binding is not copied; the callee receives the original storage.

The `move` annotation lives in the function **signature**, not at the
call site — the caller writes a plain call and the compiler enforces
the move (see spec Rule 4). Inside a function body, forwarding an
already-moved parameter to another `move` parameter requires explicit
`move` to signal the re-transfer.

```ryo
fn consume(move s: str) -> int: ...
s = build_string()
n = consume(s)    # G3: s's storage moves to consume's parameter
```

### G4: Tail move-return chain
`fn f(move x: T) -> T: return x` compiles to a direct pass-through,
no intermediate storage.

## Permitted Elision

The compiler MAY elide copies in these cases. Code must not rely on
elision here for correctness.

### P1: Return across branches with single live binding
When all branches of an `if`/`else` or `match` return the same
binding, the compiler may use a single destination slot.

### P2: Return from inside a `match` arm when all arms return the same binding name
A specialization of P1 for pattern matching.

### P3: Return from inside a loop when the binding dominates the exit
When a loop always returns the same binding and the loop exit
dominates the function return, the compiler may elide.

### P4: Struct field moves when the surrounding struct is being constructed in the caller's slot
When a struct is being constructed in the caller's slot (G2), moving
a field from another binding into the struct may avoid an intermediate
copy.

## Forbidden Cases (Copies Required)

### F1: Return when the binding is used after the return point
Not directly possible in Ryo's current design, but can occur via
`shared[T]` references in future designs.

### F2: Return of a binding whose address is aliased
If the binding's storage is aliased so that another live handle can
observe it (e.g., wrapped in `shared[T]`, whose refcounted cell may
outlive the binding), elision would change observable behavior.
(Note: `inout` cannot create such an alias — a mutable borrow cannot
be stored or returned under Rules 5 and 6, so it never survives the
call.)

### F3: Conditional return where different paths produce different bindings with incompatible storage
When different branches return different bindings that were allocated
in separate storage locations, the compiler cannot unify them into
a single destination slot.

## Algorithm Sketch

The copy elision pass runs on TIR (after Sema and the ownership pass,
before codegen):

1. **Identify return sites.** Walk the TIR to find all `return` statements.
2. **Classify each return site.** For each return:
   a. If the returned value is a local binding with no post-return uses → G1.
   b. If the returned value is a literal/constructor → G2.
   c. If the returned value is a `move` parameter that flows directly to return → G4.
   d. If the returned value is a `move` parameter's last use → G3 (at call site).
   e. Otherwise, check for P1–P4 applicability.
   f. If none apply → F-class, emit copy.
3. **Rewrite storage.** For G-class sites, allocate the local binding
   directly in the caller's destination slot (passed as a hidden
   output pointer in the calling convention).
4. **Verify safety.** Confirm that no alias to the binding's original
   storage survives the rewrite.

This pass integrates with the compilation pipeline described in the
root `CLAUDE.md` — it runs between TIR construction (Sema) and
Cranelift IR generation (codegen).

## Interaction with Ownership Lite

Elision does not weaken Rule 5 (no returned borrows) or Rule 6 (no
references in struct fields). Elision operates on owned values; it
substitutes storage locations, not ownership semantics.

## Edge Cases for Future Design

1. **Elision across `try` boundaries** — does error-propagation stack
   frame capture affect elision of the success path?
2. **Elision through generic functions before monomorphization** — can
   the compiler guarantee elision before it knows the concrete type?
3. **Elision interaction with RAII drop order (Section 5.4)** — does
   changing storage location affect when destructors run?
4. **Elision visibility across package boundaries** — is elision a
   guaranteed contract or an implementation detail when calling into
   another package?

## References
- Spec: Section 5.9 (user-facing guarantees)
- Spec: Section 5.4 (Drop / RAII)
- Dev: compilation pipeline — see root `CLAUDE.md`
- Milestone: Copy Elision & NRVO (Phase 5, v0.2+) — see implementation_roadmap.md
