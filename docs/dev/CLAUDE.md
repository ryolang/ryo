# Developer Documentation Context

Rules for agents editing developer documentation in docs/dev/.

---

## Scope

Files in docs/dev/ are implementation notes, architectural decisions, and design explorations for the Ryo compiler and runtime. These are working documents for contributors — not user-facing docs.

**Not covered here:** Compiler source code and architecture (see root CLAUDE.md), user-facing docs (see docs/CLAUDE.md), or language design (see root CLAUDE.md).

---

## Spec vs Dev Doc

**Test:** "Does this sentence belong in the spec or here?"
- Observable language behavior regardless of implementation → spec
- Compiler internals, algorithms, data structures, Rust code patterns → dev doc

The spec contains zero implementation detail. Your dev doc is the canonical place for how the compiler delivers the spec's promises. See docs/CLAUDE.md for the full three-layer architecture diagram.

---

## Back-Link Convention

Every dev doc MUST include a "References" footer linking to the spec sections it implements.

Example:
```markdown
## References
- Spec: Section 5.9 (copy elision guarantees)
- Dev: pipeline_alignment.md
- Milestone: Copy Elision & NRVO (Phase 5, v0.2+) — see implementation_roadmap.md
```

The spec doesn't know about you; you know about it.

---

## Roadmap Owns Pointers

Dev docs are referenced FROM the roadmap, not FROM the spec (see docs/CLAUDE.md for the full rule). When writing a new dev doc, add a roadmap entry pointing to it.

---

## Milestone Dependencies

Known sequencing constraints:
- M20 (`inout` / mutable borrows) MUST precede M22 (collections) and M23 (Drop)
- Closure capture analysis MUST follow M15 (ownership basics)

When proposing roadmap changes, verify they don't violate these dependencies.

---

## Draft Marking

Speculative or pre-approval content is marked `(Draft — vX)` in the heading. Final content is not. Once approved, remove the draft marker.

---

## Status Headers

Include a status header at the top:

```markdown
**Status:** Design (v0.2+) | Implementation (v0.1) | Complete | Deprecated
```

---

## Lifecycle

Per docs/dev/README.md, every file here should eventually be absorbed into the spec, implemented in code, or deleted. This directory should be empty by v1.0.

---

## File Naming

Dev docs use lowercase with underscores: `copy_elision.md`, `stdlib_optimizations.md`.

---
