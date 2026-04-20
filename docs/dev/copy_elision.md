# Copy Elision and Return Value Optimization

**Status:** Design (v0.2+). Partial implementation starts at milestone
[TBD — see roadmap update in companion task].

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

```ryo
fn consume(move s: str) -> int: ...
s = build_string()
n = consume(move s)    # G3: s's storage moves to consume's parameter
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

### F2: Return of a binding whose address was taken
If the binding's storage address has been shared (e.g., passed as
`&mut` to a function that stores the address), elision would change
observable behavior.

### F3: Conditional return where different paths produce different bindings with incompatible storage
When different branches return different bindings that were allocated
in separate storage locations, the compiler cannot unify them into
a single destination slot.

## Algorithm Sketch

The copy elision pass runs after HIR lowering and before codegen:

1. **Identify return sites.** Walk the HIR to find all `return` statements.
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

This pass integrates with the existing lowering pipeline described in
`compilation_pipeline.md` — it runs between HIR construction and
Cranelift IR generation.

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
- Dev: compilation_pipeline.md
- Milestone: [TBD — pointer added in roadmap update task]
