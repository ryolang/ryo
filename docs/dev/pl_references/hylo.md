**Status:** Reference

# Hylo Projections and Value Semantics

Sources checked on 2026-10-15. This is a focused comparison for Ryo views,
not a full compiler snapshot or a proposal to change Ryo semantics.

[Hylo repository](https://github.com/hylo-lang/hylo)

## Why Hylo belongs in Ryo's references

Hylo is a design reference for Ryo's projection model. Both models lend
access to storage while the compiler protects that access. A view does
not need to become a general reference that can escape its owner.

Ryo's concrete rules remain its own. Use the [specification](../../specification.md#44-slice-types-scope-locked-views)
and the [projection rules P1–P6](../ryo-slicing-and-memory-model-final-spec.md#32-projection-rules)
for freeze, last-use, transitive-owner, and destruction requirements.

Hylo complements the other references: Zig informs UIR/TIR arenas, Mojo
informs ownership and eager destruction, and Swift informs shared-storage ARC.

## Hylo's access model

Hylo distinguishes immutable access (`let`), exclusive mutable access
(`inout`), and ownership transfer (`sink`). The compiler rejects overlapping
mutable access. Borrowed parameters cannot escape without an owned copy.
See [Functions and methods](https://hylo-lang.org/docs/user/language-tour/functions-and-methods/).

A subscript lends a value to its caller through `yield`. The producer
resumes after the projection ends, so work after `yield` belongs to the
access lifetime. Variants include immutable access, mutable access, and a
consuming `sink` form that returns ownership. Computed properties can also
lend temporary storage and write a result back after access ends.
See [Subscripts](https://hylo-lang.org/docs/user/language-tour/subscripts/).

Hylo describes a collection slice as a projection of part of the collection,
with read or write access. See [Basic types](https://hylo-lang.org/docs/user/language-tour/basic-types/).

## Comparison with Ryo

| Concern | Hylo reference | Ryo contract |
|---|---|---|
| Storage access | Subscripts lend access to values or parts of values | `strview`, `bytesview`, and `slice[T]` expose borrowed regions |
| Mutability | Subscripts support immutable and mutable access variants | These Ryo views are read-only; P2 freezes their owner while live |
| Ownership | `sink` transfers ownership | `move` parameters transfer ownership; view materialization creates an owned value |
| Access lifetime | A yielded projection ends before the producer resumes | P4 ends a projection at its last use, with documented conservative cases |
| Producer lifetime | Yielded access keeps its producer region active | P5 delays owner destruction until all live projections permit it |
| Escapes | A yielded projection is distinct from a consuming result | Owned-backed views are scope-locked; shared-backed retaining views have a separate storage-lifetime contract |

These are conceptual mappings, not identical type systems or lowering rules.
In particular, Hylo's mutable projections do not authorize mutable Ryo views.
Hylo's computed projections also show why a projection need not always name
a direct sub-region of one original allocation.

Ryo does not adopt Hylo's subscript declarations or `yield` syntax here.
The [existing lending-subroutine discussion](../ryo-slicing-and-memory-model-final-spec.md#114-the-position-of-yield)
requires evidence of a use case and a separate design decision.

## Compiler lessons for Ryo

The [Hylo compiler architecture note](https://github.com/hylo-lang/hylo/blob/main/Docs/Compiler/CompilerArchitecture.md)
describes a Swift compiler with Hylo IR and an LLVM backend. Mandatory
analysis passes establish valid IR before backend translation.
The public documentation is evolving: its architecture page points to this
repository note, and its IR reference is marked outdated. Check current
source before copying instruction names or pass order.

For Ryo, the useful lessons are responsibilities and validation contracts:

- Record the protected owner and storage identity for each view. Re-slicing
  must preserve the original owner relationship.
- Compute view liveness over Flow branches, joins, and loop backedges.
  Unknown analysis results cannot justify release or mutation.
- Keep projection release separate from storage destruction. An owner must
  survive every access through its live views.
- Preserve those facts through body duplication and optimization. Check
  transformed Flow and cleanup before backend lowering.
- If Ryo later adopts lending subroutines, model producer suspension,
  resumption, and cleanup on every exit explicitly. A pointer and length
  alone cannot describe that execution contract.

These checks follow Ryo's existing rules. Hylo is supporting design evidence,
not a reason to add a pipeline stage or change the Cranelift backend.

## References

- Ryo spec: [§4.4 — Slice types](../../specification.md#44-slice-types-scope-locked-views), [§5.7 — Iterators and views](../../specification.md#57-iterators-and-views).
- Ryo rules: [Projection rules P1–P6](../ryo-slicing-and-memory-model-final-spec.md#32-projection-rules).
- Ryo architecture: [Pipeline migration](../pipeline_migration.md), [compiler guide](../compiler_guide.md).
- Ryo sequencing: [Implementation roadmap](../implementation_roadmap.md).
- Hylo primary sources: [Subscripts](https://hylo-lang.org/docs/user/language-tour/subscripts/), [Functions and methods](https://hylo-lang.org/docs/user/language-tour/functions-and-methods/), [Basic types](https://hylo-lang.org/docs/user/language-tour/basic-types/), [Compiler architecture note](https://github.com/hylo-lang/hylo/blob/main/Docs/Compiler/CompilerArchitecture.md).
