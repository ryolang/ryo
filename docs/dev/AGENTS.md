# Developer Documentation Context

Rules for agents editing developer documentation in docs/dev/.

---

## Scope

Files in docs/dev/ are implementation notes, architectural decisions, and design explorations for the Ryo compiler and runtime. These are working documents for contributors — not user-facing docs.

**Not covered here:** user-facing docs (see docs/AGENTS.md) or language design (see root AGENTS.md). The compiler architecture guide itself lives in [guide_compiler.md](guide_compiler.md).

---

## Spec vs Dev Doc

**Test:** "Does this sentence belong in the spec or here?"
- Observable language behavior regardless of implementation → spec
- Compiler internals, algorithms, data structures, Rust code patterns → dev doc

The spec contains zero implementation detail. Your dev doc is the canonical place for how the compiler delivers the spec's promises. See docs/AGENTS.md for the full three-layer architecture diagram.

---

## Back-Link Convention

Every dev doc MUST include a "References" footer linking to the spec sections it implements.

Example:
```markdown
## References
- Spec: [Section 5.9](../specification.md) (copy elision guarantees)
- Dev: pipeline_alignment.md
- Milestone: Copy Elision & NRVO (Phase 5, v0.2+) — see implementation_roadmap.md
```

The spec doesn't know about you; you know about it.

---

## Roadmap Owns Pointers

Dev docs are referenced FROM the roadmap, not FROM the spec (see docs/AGENTS.md for the full rule). When writing a new dev doc, add a roadmap entry pointing to it.

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

Per docs/dev/README.md, every file here should eventually be absorbed into the spec, implemented in code, or deleted. This directory should be empty by v1.0 — except `NOTES.md`, which is owner-maintained scratch permanently exempt from this lifecycle.

---

## File Naming

Dev docs use lowercase with underscores. The prefix encodes the file's *kind* (its approval/implementation status already lives in the status header, not the name):

- `spec_*` — normative design for one language feature or subsystem (e.g. `spec_copy_elision.md`, `spec_stdlib_optimizations.md`, `spec_slicing_and_memory_model.md`). One topic per file.
- `guide_*` — contributor/agent how-to (`guide_compiler.md`, `guide_llm_instructions.md`, `guide_cranelift_lessons.md`).
- `proposal_*` — pending human approval; not yet normative (`proposal_std_data.md`, `proposal_wasm_target.md`).
- `record_*` — what happened: decision records, spike results, measured evidence (`record_view_materialization.md`, `record_concurrency_poc.md`).
- No prefix — working/index docs (`implementation_roadmap.md`, `pattern_matching.md`, `design_issues.md`, `proposals.md`, `README.md`, `NOTES.md`, `pl_references/`).

New files pick their prefix by kind at creation; renaming an existing file to change kind is fine.

---
