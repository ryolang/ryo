# Standard Library Allocation Optimizations

**Status:** Design (v0.2+). Drives stdlib implementation starting at
milestone Stdlib Allocation Optimizations (Phase 5, v0.2+).

## Purpose
Documents the allocation-avoidance optimizations the standard
library MUST implement to deliver Section 5.9's user-facing promises
about copy-free return paths.

## Small-String Optimization (SSO)

### Guarantee
`str` values with payload below the SSO threshold are stored inline
in the fat pointer. No heap allocation.

### Threshold
Target: 23 bytes on 64-bit platforms, matching C++ `std::string`'s
typical SSO size. Confirm after benchmarking on Ryo's target
platforms.

### Representation
The `str` type uses a tagged union layout:

- **Inline case (SSO):** A discriminator bit (e.g., lowest bit of
  the capacity field) signals inline storage. The remaining bytes
  of the fat pointer hold the string data and its length.
- **Heap case:** Standard fat pointer — `pointer + length + capacity`.

The exact bit layout is not fixed until implementation. The key
constraint: switching between inline and heap must be transparent
to user code — `str` behaves identically in both representations.

### Observable behavior
- Construction of short strings: no allocation
- Move of short strings: single register move
- Mutation exceeding threshold: one-time promotion to heap
- Capacity introspection: inline and heap cases report uniformly

### Non-goals
- Small-vector optimization for `list[T]`: deferred to a later phase.
  Most list growth patterns defeat SSO-style optimization. Revisit
  after stdlib benchmarks.

## Copy-on-Write for Strings

### Guarantee
When a copy is required for an immutable `str` (e.g., an explicit
`clone()` or a compiler-inserted copy on a path where elision does
not apply), the backing buffer is not duplicated. Instead, the new
value shares the existing buffer via refcount, deferring allocation
until mutation.

### Applies to
- `str` values exceeding the SSO threshold
- `str` values produced by known-immutable constructors (literal,
  `str.from_static`, etc.)

### Does not apply to
- `mut str` — mutation is the point, COW is overhead
- SSO-inline strings — already copy-free

### Thread safety
COW refcounts on `str` MUST be atomic, matching `shared[T]` semantics.
The cost of the atomic operation is accepted in exchange for
thread-safe cheap clones.

### Interaction with `shared[T]`
`shared[str]` is a different thing: an explicit multi-owner handle.
COW on `str` is transparent — the user does not see refcounts.
Both can coexist.

## Sink-Parameter Pattern

### Convention
Functions that build incrementally on caller-provided storage take
`move T` and return `T`:

```ryo
fn write_header(move buf: str, name: str, value: str) -> str:
	buf.push_str(name)
	buf.push_str(": ")
	buf.push_str(value)
	buf.push('\n')
	return buf
```

### When to use it
- Buffer-building APIs (HTTP headers, log formatting, serialization)
- Any API where the caller wants to reuse capacity across calls

### When not to use it
- APIs where `&mut` is cleaner (no return, simpler chaining)
- One-shot construction (just return a fresh owned value)

### Stdlib APIs adopting this pattern
Planned adoption in:
- `str` builder methods (e.g., `str.with_capacity` → incremental append)
- HTTP header construction in `std.net.http`
- Log message formatting in `std.log`
- Serialization buffer reuse in `std.json`

Specific function signatures will be finalized during stdlib design.

## References
- Spec: Section 5.9
- Dev: std.md, std_ext.md, copy_elision.md
- Milestone: Stdlib Allocation Optimizations (Phase 5, v0.2+) — see implementation_roadmap.md
