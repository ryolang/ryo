# Proposal Reviews — Issue List & Spec Alignment

> **Document Status:** Review Memo (Action Items)
> **Last Updated:** 2026-07-22
> **Reviewed files:** `std_ext.md` (stdlib via Rust crate wrapping), `tensor.md` (DLPack tensor wrapping), `unsafe.md` (unsafe code architecture), `concurrency.md` + `concurrency_loom_kt.md` (v0.4 concurrency plan and its improved variant)
> **Reviewed against:** `ryo-slicing-and-memory-model-final-spec.md` v1.0.0-draft (decisions D1–D11), Ryo Language Specification v0.1.0-draft, `ryo-missing-features-and-gaps.md` (GAP-1), `ryo-std-data-proposal.md`
> **Scope note:** This memo lists fixes for the *proposal files*. The final spec is unchanged; candidate spec amendments are collected in §7 and require separate approval.

---

## 1. Severity Summary

| File | Must-fix | Should-fix | Nits | Overall verdict |
|------|:--------:|:----------:|:----:|-----------------|
| `std_ext.md` | 3 | 2 | 2 | Sound strategy; correct instincts; FFI details need hardening |
| `tensor.md` | 5 | 3 | 1 | Correct pattern; real safety bugs in the details; views unaddressed |
| `unsafe.md` | 4 | 3 | 0 | Right architecture; ~85% same design as D4 — adopt as D4's implementation companion with amendments |
| `concurrency.md` (base plan) | 3 | 1 | 1 | Solid phased plan; Phase 6 effect analysis is the crown jewel; one design gap (FFI stacks) resolved by the loom variant |
| `concurrency_loom_kt.md` (improved variant) | 3 | 2 | 1 | **Recommended as the plan** — surgical, honest, additive improvements; three must-fixes before adoption |
| **Cross-cutting** | 2 | 2 | — | See §6 |

---

## 2. `std_ext.md` — Issues

| ID | Sev | Issue | Required fix | Spec relationship |
|----|-----|-------|--------------|-------------------|
| SE-1 | **Must** | The wrapped `serde_json` DOM is an opaque Rust-heap object behind accessors — it cannot participate in exhaustive `match`, method views, or projection-based navigation, contradicting the validated JSON design | Keep the public API Ryo-shaped (`parse(str) -> !JsonValue` with native navigation); declare the Rust backend **transitional**; schedule a native Ryo parser (best available stress test for projections) | Final spec §2.3 (JSON validation); D1 projections; Rule 5 method views |
| SE-2 | **Must** | Shim safety: `CStr` assumes no interior NUL (UTF-8 permits U+0000; Ryo strings are ptr+len); `Err(_) => null` discards all error information | Pass `(ptr, len)` across FFI, never null-terminated alone; return a structured error the Ryo side maps into a proper `error` type with location | Base spec §4.10 (rich errors); interacts with `unsafe.md`'s `ffi` package (adopt U-good-2) |
| SE-3 | **Must** | No `Drop` half for the opaque handle — every `parse` leaks a Rust-side `Box<Value>` | Add `ryo_json_free` and `impl Drop for JsonValue` (`move self`, see X-1) | Base spec §5.4 (RAII/Drop) |
| SE-4 | Should | `chrono` recommendation is dated | Evaluate `jiff` (same author as `regex`; `chrono` effectively maintenance-mode by 2026) | — (ecosystem currency) |
| SE-5 | Should | No mapping of modules to runtime profiles | State per module: `core`-compatible (json, regex, time — allocator only) vs `hosted`-only (http, fs, net) | D9 (§8.2 profile table) |
| SE-6 | Nit | `std.simd` example (`a + b` on `f32x4`) silently assumes operator overloading exists | Note the dependency explicitly; specify vector types as Copy value types with fixed layout | D10 (adopted v0.2–v0.3 — timeline compatible) |
| SE-7 | Nit | Two-allocator seam (Rust heap + Ryo runtime) for every wrapped crate | Acceptable if transitional; record that wrapped crates appear in `ryo audit` output | D4 audit tooling |

---

## 3. `tensor.md` — Issues

| ID | Sev | Issue | Required fix | Spec relationship |
|----|-----|-------|--------------|-------------------|
| T-1 | **Must** | `fn drop(inout self)` contradicts the base spec | `drop` takes **`move self`** ("the value is being destroyed — no reason to borrow something that ceases to exist") | Base spec §5.4 (explicit rationale); see X-1 — recurring bug |
| T-2 | **Must** | Bounds check has a hole: `index >= self._shape[0]` never rejects **negative** indices; `ptr.offset(-1)` is instant UB — in a document whose purpose is safety containment | Check `index < 0 or index >= len` | Base spec §4.7 (strict non-negative bounds) |
| T-3 | **Must** | `_shape` stored but `_strides` not; `get` assumes contiguous row-major — non-contiguous tensors (transposed views, strided slices, both expressible via DLPack `strides`/`byte_offset`) read garbage | Store and honor strides, or verify contiguity and refuse otherwise | DLPack struct (their own §1 lists strides as danger #2) |
| T-4 | **Must (design)** | **Tensor views are unaddressed** — slicing/reshape/transpose create aliasing tensors, but DLPack gives one `deleter` on one `DLManagedTensor`; two `Tensor[T]`s sharing it double-free | Apply the two-tier rule: same-scope views = projections (non-escaping, zero-cost); escaping/shared tensors = refcounted ownership of the managed tensor | Final spec §5.1.1 (two-tier rule); D1 projections; D3 shared-buffer precedent |
| T-5 | **Must** | GPU race named (§1 danger #4) but never solved: no stream/event synchronization; host-side `get` on GPU memory is a race or fault | Document a sync contract: per-device ordered stream; host accessors (or explicit `.sync()`/`.cpu()`) synchronize before reading | Complements D5 (concurrency correctness story) |
| T-6 | Should | Timing: `Tensor[T]` needs user-defined generics — **v0.3** per the base rollout — but the doc says v0.2+ | Restatus to v0.3+ | Base spec Feature Availability table |
| T-7 | Should | `panic("Shape mismatch")` for runtime-influenced shapes | Error union (`ShapeError!Tensor`); panics are for programmer errors, not user data | Base spec §7 (error philosophy) |
| T-8 | Should | CPU execution leans on an auto-vectorization pass as the primary mechanism | Prefer explicit `simd.f32x4` kernels (D10) + `par_*` multicore (D5); auto-vectorization as a bonus for user loops | D5, D10; see X-3 |
| T-9 | Nit | `unsafe struct DLTensor` syntax not aligned with D4 forms | Extern declarations inside `unsafe` blocks, `package` visibility, mandatory `#: SAFETY:` comments | D4 rule 3 |

**Noted synergy (not an issue):** the doc's own §4 shows `a + b` on tensors — legal under D10's same-type rule (`impl Add for Tensor[f32]`). Record D10 as a dependency.

---

## 4. `unsafe.md` — Issues

| ID | Sev | Issue | Required fix | Spec relationship |
|----|-----|-------|--------------|-------------------|
| U-1 | **Must** | Flag collision: `kind = "system"` (package *identity*) vs. D4's `allow_unsafe = true` (*capability*). `kind` mislabels non-FFI unsafe packages (`fastcodec`, lock-free queues) and forces whole-package reclassification when an app needs one unsafe block | Gate on `allow_unsafe = true`; `kind`, if kept, is descriptive metadata — not the switch | D4 §6.2 |
| U-2 | **Must** | D4 machinery missing: mandatory `#: SAFETY:` comment (compile error if absent), `ryo audit`, `--deny-unsafe=deps`, safe-API lint | Adopt unsafe.md as D4's **implementation**, not its replacement — import D4 policy wholesale | D4 rules 2–4 |
| U-3 | **Must** | `fn drop(inout self)` — same §5.4 violation as T-1 | `move self`; sweep all proposal docs (see X-1) | Base spec §5.4 |
| U-4 | **Must** | `pub struct Buffer: ptr: *void` — the "safe wrapper" exposes its raw handle publicly | Requires a base-spec decision: field-level visibility (undefined today — three-tier visibility covers items, not struct fields), or a formalized `_`-prefix convention | **Spec gap G-1** (§5.2); affects every safe-wrapper pattern incl. tensor.md's `_handle` |
| U-5 | Should | Bare `extern "C":` vs. base spec's `unsafe extern "C":` (§6.1.2); declaration-vs-call gating muddled | Pick one form; recommended: extern *declarations* gated by the package flag, extern *calls* require `unsafe` blocks (what their own §7 implies) | Base spec §6.1.2, §17 |
| U-6 | Should | §7 lists `static mut` access — but base spec §1.2 **forbids global mutable state** as an AI-era principle | Explicit carve-out scoped to FFI (errno-style C globals), not a quiet list entry | Base spec §1.2 |
| U-7 | Should | Framing is FFI-only ("Binding Maintainers (1%)", "use a pre-existing library") — undercuts D4's broader motivation: third-party *native* safe abstractions (containers, concurrency primitives) | Reword to "systems-capability packages"; keep the mechanism | D4 §6.1 (rationale) |

### 4.1 Parts of `unsafe.md` to adopt *into* the spec (not bugs — strengths)

| ID | Item | Why |
|----|------|-----|
| U-good-1 | **Unsafe operation set** (§7): pointer deref/arithmetic, extern calls, `unsafe fn` calls, `static mut`, unsafe trait impls | Rust-proven enumeration; D4 didn't spell it out |
| U-good-2 | **`unsafe fn` propagation** — calling an unsafe fn requires an `unsafe` block; unsafety is viral-explicit in signatures | Keeps unsafe from leaking through safe call sites |
| U-good-3 | **`ffi` stdlib package** — `&str ↔ *const c_char` with UTF-8 validation *returning an error*, in a library not the language | Preempts the SE-2 bug class |
| U-good-4 | **std as "Root System Package"** — formal proof the containment model works | Consistent with D4; worth recording |
| U-good-5 | **Gatekeeper simplicity** — a ~10-line checker pass | Capability systems fail by being clever; this one isn't |

---

## 5. `concurrency.md` + `concurrency_loom_kt.md` — Issues

> **Recommendation:** promote `concurrency_loom_kt.md` from "structured alternative" to **the plan** before Phase 1 begins (its own trigger condition), with amendments L-1…L-3 plus integration sections C-2/C-3. The base plan's Phase 6 and the loom doc's FFI router are both keepers — they compose without conflict.

> **Status (m8.4.2):** Promotion executed — the merged plan now lives at `concurrency.md`. Resolved en route: L-1 (`Dispatcher`/`with_dispatcher` rename), L-2 (overflow system coroutines, `RYO_FFI_OVERFLOW_DEPTH`, fail-fast `FfiReentryLimit`), L-3 (`with_dispatcher` is `[yields]`; `task.pin()` interaction in §6.2.6), L-6 (APIs marked proposal-only, spec edits still owed). Still open: L-4, L-5, C-2…C-5.

### 5.1 `concurrency.md` (base plan)

| ID | Sev | Issue | Required fix | Spec relationship |
|----|-----|-------|--------------|-------------------|
| C-1 | **Must (design)** | Plain FFI runs on the green thread's 32–128 KB stack: C libraries exceeding the cap (codecs, ML inference, recursive C) kill the task with `StackOverflow`; the only escape is `#[blocking]` at ~10 µs/call | **Resolved by adopting the loom variant** (L-good-2: per-worker 2 MB system coroutine at ~200 ns/call) | §17 FFI; D9 hosted runtime |
| C-2 | **Must** | Predates D5: no mention of scoped task borrows — `task.scope` children may capture by **borrow**, and stdlib `par_*` builds on it | Add a D5 section: borrow captures verified against the scope join; place `par_*` relative to `pool::compute` | Final spec §7 (D5) |
| C-3 | **Must** | Predates GAP-1: no ambient-context integration — yet §3.5's task-locals with scope inheritance are exactly the mechanism GAP-1 needs | Add integration section: context frames as inherited task-locals; deadlines as timer-wheel cancellation sources; `ctx.done()` as a `select` arm | `ryo-context-and-otel-proposal.md` §2 |
| C-4 | Should | `task_local! { static REQUEST_ID: Cell<u64> }` is Rust macro syntax with Rust types — violates Ryo's no-macros principle; pseudocode must not leak into user-facing docs | Replace with a Ryo-idiomatic API/builtin (e.g. `task.local[T](init)`); sweep examples for Rust-isms | Base spec §1.2 (no macros) |
| C-5 | Nit | No D9 note: the entire scheduler is hosted-only; on `core`, FFI runs on the real OS stack — no 128 KB cap, no router needed | State it; the system coroutine is a hosted-runtime component | D9 §8.2 |

### 5.2 `concurrency_loom_kt.md` (improved variant)

| ID | Sev | Issue | Required fix | Spec relationship |
|----|-----|-------|--------------|-------------------|
| L-1 | **Must** | `Pool` type / `with_pool` **collides with `std.pool`** (data-layer proposal): the loom `Pool` is a *scheduler execution context* (bounds concurrency); `std.pool` is a *resource manager* (bounds connections). The loom doc's own DB example is already served by `std.pool`'s 16 connections | Rename the scheduler concept — `Dispatcher` / `Executor` family (Kotlin's own term is `CoroutineDispatcher`); keep `std.pool` for resources | `ryo-std-data-proposal.md` §4 |
| L-2 | **Must** | **Nested FFI reentrancy unspecified:** C calls back into Ryo (now on the system coroutine), that Ryo code calls FFI again — the worker's single system coroutine is occupied; neither open-question option (one-per-worker / shared pool) covers nesting. Real-world cases: SQLite authorizer callbacks, libpng error handlers | Spec: dedicated system coroutine per worker **plus on-demand overflow coroutines** for nested FFI | §17 FFI callbacks |
| L-3 | **Must** | `with_pool` migrates the task across OS threads but is not integrated into Phase 6.2: it must be tagged `[yields]` (a `with_pool` inside `with lock()` = same hard error as `recv`), and its interaction with the `task.pin()` hook (thread-affine C libraries) is unwritten | Add both rules to the effect-analysis spec | concurrency.md §6.2, §3.3 |
| L-4 | Should | `#[blocking]` pool default shrunk 10K→1K — risky for DB-heavy apps where sqlite/libpq route there; "tuned later" is optimistic for a hard-coded default | Ship configurable from day one (`RYOBLOCKINGPOOL` or ryo.toml); default 1K acceptable | Go M-cap alignment |
| L-5 | Should | `conflated` channel × `select` waker semantics unwritten: what happens to a pending waker when an unreceived value is overwritten | Spec text before implementation (~1 paragraph) | concurrency.md §4.3 |
| L-6 | Nit | Claims "zero spec changes" — but `task.supervise`, `with_pool`, and two channel modes are user-facing additions requiring spec §9 text | Amend the claim; schedule the spec edits with adoption | Base spec §9 |

### 5.3 Strengths to record (both documents)

| ID | Item | Why |
|----|------|-----|
| C-good-1 | **Phase 6 inferred `[yields]` effect analysis** — zero annotations, leaf-tagged propagation, effect-polymorphic generics, whole-program `dyn` devirtualization with impl-trace hard errors, channel API split, flow-sensitive ARC lint | The purest "compiler strict, user simple" feature in either document; Go can't do it, Rust can't without coloring. Make-or-break = error-trace quality (correctly risk-registered) |
| C-good-2 | **Task-local storage with scope inheritance** (§3.5) | GAP-1's propagation mechanism, built before GAP-1 was named |
| C-good-3 | **Async-drop shielding** (§4.6): destructors may yield, cancellation deferred during unwind, soft 5 s deadline | Answers the OTel proposal's Q2 (context shields) preemptively; Trio/Kotlin-proven |
| C-good-4 | **Adaptive stacks from Phase 1** with quantified rationale (128 KB × 1 M = 128 GB ceiling) | Forward-thinking without over-engineering |
| C-good-5 | **WASM/WasmFX deferral with explicit prerequisites** | Same measured-re-entry discipline as the final spec's deferred tier |
| L-good-1 | **Honest abandonment of the true-Loom framing** ("no Rust crate implements stack-capture in production quality") | Projections-v2-grade epistemics; no phantom capabilities |
| L-good-2 | **System coroutine** (Go g0 pattern via corosensei): 2 MB FFI stack at ~200 ns vs ~10 µs; makes C→Ryo callbacks safe | Solves C-1 with proven engineering, ~250–400 LOC, fully additive |
| L-good-3 | **Delta discipline**: explicit "what stays" list, LOC cost accounting, "when to revisit" trigger conditions | Template for all future alternative proposals |
| L-good-4 | **`task.supervise` + rendezvous/conflated channels** (~300 LOC total) | `conflated` (latest-value) feeds §11 GUI event loops; supervise fits fan-out servers |

---

## 6. Cross-Cutting Issues

| ID | Sev | Issue | Required fix |
|----|-----|-------|--------------|
| X-1 | **Must** | **`drop(inout self)` appears in two independent proposals** (T-1, U-3) — evidence the proposal docs were written against an older spec revision | Sweep *all* proposal/dev docs for §5.4 drift (`drop` takes `move self`); implementers will copy the examples |
| X-2 | **Must (spec gap G-1)** | **Field-level visibility is undefined in the base spec** — every safe-wrapper pattern (Buffer, Tensor `_handle`, `SysValue._ptr`) depends on it | Base-spec amendment: extend three-tier visibility to struct fields (or formalize the `_` convention). Small change, unblocks all FFI wrappers |
| X-3 | Should | CPU compute story is fragmented across three docs: auto-vectorization pass (`tensor.md`), explicit `std.simd` (`std_ext.md`), `par_*` (final spec D5) | One narrative: explicit SIMD types for library kernels + `par_*` for multicore + auto-vectorization as best-effort bonus |
| X-4 | Should | D9 profile mapping not applied to std-module proposals | Each std proposal states `core` vs `hosted` availability (see SE-5) |
| X-5 | Should | **Spec-decision drift:** the concurrency docs (C-2, C-3) and earlier proposals (X-1) were written against older decision sets — no process propagates new decisions (D5, GAP-1, D10) into existing dev plans | Institute an alignment sweep: each accepted decision lists affected docs in its record; sweep on every decision merge |

---

## 7. Dependency & Alignment Map (proposals ↔ final-spec decisions)

| Proposal | Depends on | Benefits from | Conflicts (after fixes) |
|----------|-----------|---------------|------------------------|
| `std_ext.md` | §4.11 FFI workflow; D4 (unsafe in wrappers); D10 (simd operators); D9 (profile gating) | D9 pay-per-import alignment; U-good-3 `ffi` package | None — compatible |
| `tensor.md` | D4 (unsafe containment); user generics (v0.3); D10 (`a + b`, `matmul` notation) | D1/D3 two-tier rule solves its unasked views question (T-4); D5 CPU parallelism | None after T-1…T-5 — compatible |
| `unsafe.md` | §17 FFI; base spec §1.2 | **Is** the D4 gatekeeper implementation; U-good-1…5 strengthen D4 | U-1 flag naming; U-2 policy muscle — resolved by adopting as companion |
| `concurrency.md` | D9 (hosted runtime); §9 spec | C-good-2 task-locals are GAP-1's foundation; C-good-3 answers OTel Q2 | C-1 resolved by loom adoption; C-2/C-3 integration sections owed |
| `concurrency_loom_kt.md` | `concurrency.md` phases; D9; §17 FFI | L-good-2 solves C-1; L-good-4 feeds GUI §11 | L-1 naming collision with `std.pool`; L-2/L-3 amendments owed |

**Direction of influence:** all reviewed proposals fit *under* D1–D11; none requires changing the final spec. The only reverse-flow items are the **G-1 field-visibility gap** (base-spec amendment), the **U-good-1…5 adoptions** (candidate D4 enrichment), and the **C-2/C-3 integration text** (concurrency plan owes alignment to D5 and GAP-1, not vice versa) — all listed here for approval, none applied.

---

## 8. Recommended Action Order

1. **Concurrency decision (time-boxed):** promote `concurrency_loom_kt.md` to the plan with amendments L-1…L-3 and integration sections C-2 (D5) / C-3 (GAP-1) — *before Phase 1 begins*, per its own trigger condition.
2. **X-1 + T-1 + U-3** — global `drop(move self)` sweep (cheap, prevents copied bugs).
3. **G-1 (X-2 / U-4)** — decide field-level visibility; everything FFI-shaped blocks on it.
4. **U-1 + U-2** — unify the unsafe gate on `allow_unsafe = true` and merge unsafe.md's operation set + `unsafe fn` + `ffi` package into D4's record.
5. **T-2, T-3, T-5** — the three *actual memory-safety* holes in tensor.md (negative index, strides, GPU sync).
6. **T-4** — write the tensor-views section using the two-tier rule (the highest-value design addition).
7. **SE-1…SE-3** — harden the JSON shim (API shape, error mapping, Drop).
8. Everything else (SE-4/5, T-6/7/8, U-5/6/7, C-4/5, L-4/5/6, X-3/4/5) — normal review cycle.

---

*End of Review Memo*
