**Status:** Design (v0.2+) — the `unsafe` operation set has been absorbed into spec §17; this document retains binding-author internals only

# Unsafe Code Architecture

The core design problem: **how to allow dangerous operations (which are necessary) without letting the average user cause memory safety violations.**

Ryo's answer is now owned by the spec. An earlier revision of this document proposed a **Capability-Based System** built on a `kind = "system"` gatekeeper in `ryo.toml`; that design is **superseded**. See **spec §17** for the adopted policy:

- `allow_unsafe = true` in `ryo.toml` — a manifest-declared capability available to any package; without it, `unsafe` blocks are compile errors
- `ryo audit` reports the capability across the dependency tree; consumers can build with `--deny-unsafe=deps`
- Mandatory `#: SAFETY:` doc comments on every `unsafe` block
- Safe-API lint; raw pointers (`*T`) confined to `unsafe` blocks

What remains here is reference detail for binding authors that the spec deliberately does not enumerate.

---

### 1. FFI Type Mapping

**C type mapping.**
- Ryo primitives map directly to their C equivalents.
- Raw pointers: `*const T` / `*mut T`.
- `#[repr(C)]` on a struct guarantees C-compatible layout.
- Complex types cross the boundary via opaque pointers; callbacks via compatible `extern "C"` function pointers.
- String conversion helpers (`&str` ↔ `*const c_char`, with UTF-8 validation returning an error) live in an optional `ffi` stdlib package, not the language.

### 2. The Unsafe Operation Set

The set of operations that require an `unsafe` block is enumerated in **spec §17** (Unsafe operation set) and is owned there; this document no longer duplicates it.

The programmer is responsible for upholding safety invariants inside an `unsafe` block — the type system makes no guarantees there. See spec §4.11 for the full `extern`/bindgen workflow.

## References
- Spec: `docs/specification.md` §17 (FFI & unsafe — now owns the `unsafe` operation set, moved out of this document), §4.11 (FFI & C Interoperability)
- Dev: `docs/dev/built_in.md` (std.sys hidden layer)
- Roadmap: `docs/dev/implementation_roadmap.md` — unsafe policy implementation (manifest gating, `SAFETY:` enforcement, `ryo audit`) targeted at v0.2
