# Ryo — Missing Features & Gap Analysis

> **Document Status:** Design Gap Register (Pre-Implementation)
> **Last Updated:** 2026-07-22
> **Version:** 1.0.0-draft
> **Method:** Every declared target domain (base spec §1.1) reviewed against the base specification, the final slicing/memory-model spec (decisions D1–D11), the GUI strategy (§11), and the reviewed ecosystem proposals (`std_ext`, `tensor`, `unsafe`)
> **Companion document:** `ryo-context-and-otel-proposal.md` (full design for GAP-1)

---

## 1. Summary

The slicing/ownership/memory-model work is **converged** — no remaining gaps are data-model features. What remains are **runtime semantics** (context propagation, integer overflow, volatile access) and **layout control** (packed representation) — all in service of domains the design already opened. Four genuine gaps were found; none is structural.

| ID | Gap | Severity for target domains | Fix class | Target milestone |
|----|-----|------------------------------|-----------|------------------|
| GAP-1 | Context propagation & cancellation deadlines | **High** — primary domain (web services) | Language + runtime design | v0.4 |
| GAP-2 | Integer overflow semantics unspecified | **High** — numeric, embedded, protocol domains | One-paragraph semantics + stdlib ops | v0.1 (must freeze early) |
| GAP-3 | Volatile memory access (MMIO) | Medium — embedded path (D9 `core` profile) | Unsafe operation-set addition | v0.2 |
| GAP-4 | Packed layout control | Medium — protocol parsing, hardware registers | One attribute | v0.2 |

---

## 2. GAP-1 — Context Propagation & Cancellation Deadlines

**The gap.** Every production web service needs request-scoped context: deadlines, cancellation, trace IDs, auth identity. Go's `context.Context` — bolted on years after launch, threaded manually through every signature — remains the top ergonomics complaint in Go services. Tokio reinvented it as task-locals; Python's asyncio as contextvars. It is reinvented everywhere because it is mandatory. **Neither Ryo spec mentions it.**

**Why Ryo can do better than all of them.** The ambient runtime already lives in TLS (§9.1), and `task.scope` already defines a lexical tree of work (§9.2.1, extended by D5). The context tree and the task tree are *the same tree*: propagation and cancellation can flow down the structure the scheduler already maintains — no parameter threading, no function coloring, with `with` blocks giving deadlines a lexical, reviewer-visible extent.

**Disposition:** full design in the companion document `ryo-context-and-otel-proposal.md` (context propagation is the mechanism; OpenTelemetry is its flagship consumer). Must land **with the v0.4 concurrency runtime** — retrofitting after the scheduler freezes would repeat Go's mistake.

---

## 3. GAP-2 — Integer Overflow Semantics

**The gap.** The base specification never defines what happens when `int` arithmetic overflows. If left implicit, the default industry outcome is trap-in-debug / wrap-in-release — behavior *drift* between development and production, a direct violation of the spec's "predictable patterns over clever shortcuts" and "no surprises" principles.

**Why it matters now.** The domains D2/D9/D10 opened are exactly where overflow is a correctness property, not a crash: protocol length fields (`bytes`), hardware register math (D9 `core`), numeric kernels (D10, `std.simd`).

**Proposed semantics (one paragraph):**

> Default integer arithmetic **traps (panics) on overflow in all build modes**, debug and release alike (Swift-proven; consistent with DX-first). Code that intends wrapping, saturation, or testability spells it: `wrapping_add` / `saturating_add` / `checked_add` (the latter returning `?int`) as stdlib operations or D10-overloadable explicit operators. The reviewer always knows which regime applies by reading the operation — the default is the safe one, the exceptions are visible.

**Cost:** one overflow-check per integer op, predictable and branch-predictable; domains that measure it can opt into `wrapping_*`. Target: **freeze in v0.1** — semantics like this cannot be changed after code exists.

---

## 4. GAP-3 — Volatile Memory Access (MMIO)

**The gap.** D9's `core` profile targets freestanding environments, but bare-metal code requires **volatile** reads/writes to memory-mapped registers (the compiler must not elide, coalesce, or reorder them). Neither the base spec nor the unsafe operation set (`unsafe.md` §7) includes volatile access. Without it, the LLVM backend (Q6) would arrive at a language that still cannot toggle a GPIO pin.

**Proposed fix (minimal):** add volatile load/store to the unsafe operation set — e.g. `volatile_read(ptr: *const T) -> T` / `volatile_write(ptr: *mut T, value: T)` as `unsafe` intrinsics in the `core` profile's intrinsic package, subject to all D4 rules (SAFETY comments, gating, audit). No new syntax, no safe-code exposure. Target: **v0.2**, with the unsafe-policy implementation.

---

## 5. GAP-4 — Packed Layout Control

**The gap.** `#[repr(C)]` exists (§4.11) but `#[repr(packed)]` does not. Two *stated* target domains need unaligned, padding-free layouts: **wire-protocol parsing** (the entire `bytes`/D2 motivation — headers are packed by definition) and **hardware register maps**.

**Proposed fix (minimal):** `#[repr(packed)]` on structs, with the standard rule that taking a reference/projection to a field of a packed struct is a compile error (unaligned references are UB-adjacent; access by value copy instead — matching Rust's proven rule). Rides with the FFI work in **v0.2**. Bitfield-level control remains deferred to `comptime` (v0.3) as a library-generation concern.

---

## 6. Tier 2 — Not Language Gaps; Validate Early

| Item | Why it matters | Status / disposition |
|------|----------------|----------------------|
| **Atomics in stdlib `sync`** | D4 lets third parties build concurrent structures, but they need `AtomicInt` & friends to do it | Presumably v0.4 with the scheduler — should be **stated** in the stdlib roadmap, not assumed |
| **Arena allocation as a library pattern** | Per-request arenas are a top web-performance idiom; game tooling depends on them | Now expressible (arena owns a block; hands out non-escaping projections — D1 + D4). **Validate with a stdlib PoC** (`std.mem.arena`) before v0.4 |
| **Allocator swap for `hosted`** | Perf-sensitive applications want jemalloc-class allocators without Zig-style explicit allocator plumbing | Keep allocators hidden (philosophy), allow a **build-level swap** (`--allocator=`); unspecified today |
| **Signals / graceful shutdown / process management** | Every production service and CLI needs it | Stdlib `os`; add to the `std_ext` v2 checklist |
| **Structured logging facade** | Services live or die on observability; trace IDs must correlate with logs | Folded into the OTel proposal (`std.log` + OTel bridge) — see companion document §6 |

---

## 7. Tier 3 — Checked and Reaffirmed as Non-Gaps

Macros, `eval`, runtime reflection, variadics, exceptions, GC — exclusions with recorded rationale (final spec §12). ETL-style lazy pipelines are covered by lazy scope-locked iterator chains (§5.7 + D1) rather than Python-style generators; Hylo `yield` remains deferred with its measured re-entry condition (final spec §11.4). Serialization derives are the known `comptime` gap, already roadmapped (v0.3).

---

## 8. Relationship to the Final Spec

| Gap | Amends (when approved) |
|-----|------------------------|
| GAP-1 | New section with companion proposal; interacts with D5 (task tree), D9 (profiles), `with` statement |
| GAP-2 | Base spec §4.1/§4.13 (integer semantics); one paragraph + stdlib naming |
| GAP-3 | Final spec §6 (D4 operation set) or `unsafe.md` §7 — one bullet |
| GAP-4 | Base spec §4.11 (representation attributes) — one attribute |

None of these touches decisions D1–D11; all are additive. Approvals are required before any text moves into the main specification documents.

---

*End of Gap Register*
