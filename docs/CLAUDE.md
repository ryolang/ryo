# Docs Directory Conventions

Context for agents editing files in `docs/`.

---

## When Writing Specification

* Ryo is a general programming language — no system programming features needed
* Ryo prioritizes DX
* Ryo is not yet implemented; this is the first specification. No versioning concerns — all spec is future work.
* Built-in types are lowercase, user-defined types are PascalCase

## Diagrams

Use mermaid for diagrams.

## Syntax in Code Examples

All code examples follow the rules in root `CLAUDE.md` (Python-style colons + indentation, tabs, no braces except f-strings). See `docs/specification.md` for the full syntax reference.

Key patterns that come up often in docs:

```ryo
# Module-based errors
error DivisionByZero
error InvalidInput(str)

fn divide(a: int, b: int) -> DivisionByZero!int:
    if b == 0:
        return DivisionByZero
    return a / b

# Optional types
user: ?User = none
name = user?.name orelse "Unknown"

# Constrained types (v0.2)
type Port = int(1..65535)

# Contracts (v0.2)
#[pre(x > 0)]
fn double(x: int) -> int:
    return x * 2
```

---

## Directory Layout

- `docs/analysis/` — scratch/working artifacts, not committed
- `docs/assets/` — images and static files
- `docs/dev/` — implementation notes (see `docs/dev/CLAUDE.md`)
- `examples/` — Ryo code examples (working + `future/` for aspirational)
- `docs/extra/` — supplementary documentation

---

## Documentation Conventions

**Three-layer doc split:** Spec says *what*, dev docs say *how*, roadmap says *when* and owns the pointers between layers.

```
specification.md          (what the language does)
         ↑
         │ "implements Section X.Y"
         │
implementation_roadmap.md (when each what gets built — owns pointers to dev docs)
         ↓
docs/dev/*.md             (how the compiler/stdlib delivers — links back to spec sections)
```

**Spec purity:** specification.md contains no implementation details and no path references to docs/dev/ files. Test: "Could this sentence remain true regardless of how the compiler implements it?" If yes → spec. If no → dev doc.

**Roadmap owns pointers:** When a new dev doc is written, link it from the roadmap, not from the spec.

**Spec is source of truth:** When index.md, getting_started.md, or language_comparison.md contradict the spec, update the companion, never the spec.

**Preserve voice; minimal diffs:** Restructuring sections is out of scope. Preserve existing voice and structure. For multi-file changes, show diffs before applying.

**Audit first:** For multi-file documentation changes, grep the tree first and produce an audit.

**Scratch vs committed:** Working artifacts go in docs/analysis/ and are not committed.

---

## Gotchas

**Task closures move implicitly.** In Section 9, closures passed to `task.run`/`task.scope`/`task.spawn_detached` capture by move automatically. Writing `move` on them is accepted but redundant. Elsewhere, `move` is always explicit.

**`&mut` and `move` are the same cost.** Under NRVO, both compile to a pointer pass. See Section 5.2.1.

**Section 5.1 and Rule 2 are both correct.** Section 5.1's "moved by default" applies to assignment and return. Rule 2 says parameters default to immutable borrow. These cover different cases. Do not "fix" the apparent contradiction.

**Roadmap milestone dependencies are real.** See docs/dev/CLAUDE.md for the specific sequencing constraints.

---

## Related Documentation

**For complete syntax:**
- See `docs/specification.md`

**For implementation status:**
- See `docs/dev/implementation_roadmap.md`
