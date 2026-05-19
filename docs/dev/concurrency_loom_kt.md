**Status:** Design proposal (Draft — alternative to concurrency.md)

# Concurrency Plan: Loom-Inspired FFI + Kotlin-Borrowed Patterns

This document proposes a refined concurrency runtime for Ryo: keep [`concurrency.md`](concurrency.md)'s `corosensei`-based green-thread foundation, **borrow Loom's FFI ergonomics** via a per-OS-thread "system coroutine" routing pattern, and **borrow Kotlin's user-facing primitives** (explicit dispatchers, supervisor scopes, channel-mode taxonomy).

The result is the same colorless-concurrency promise as the current plan, with two material improvements:

1. **FFI safety** — compute C calls run on an MB-sized stack, never the green-thread stack. C libraries that recurse or allocate large stack arrays no longer risk `StackOverflow` on Ryo tasks.
2. **Runtime pool flexibility** — users can declare custom thread pools for app-level isolation (e.g., bounded DB-query concurrency) without changing the colorless model.

This doc is structured as a **delta against [`concurrency.md`](concurrency.md)** — what stays, what changes, what's new, and what it costs.

This proposal does **not** change the user-facing spec. Ryo remains colorless. No `suspend` keyword. One new user-facing primitive (`with_pool`, borrowed from Kotlin's `withContext`).

> **Sibling reference docs:** [`memory_model_comparison.md`](memory_model_comparison.md), [`rust_reference.md`](rust_reference.md), [`mojo_reference.md`](mojo_reference.md), [`arc_optimizer.md`](arc_optimizer.md), [`wasm_target.md`](wasm_target.md).

---

## A note on "Loom-inspired"

The original framing of this proposal was "Loom-style virtual threads" — meaning replace corosensei with stack-walking + heap-capture continuations, the way Java Loom does. After investigation:

- **No Rust crate implements Loom's stack-capture primitive in production-quality form.** The Rust ecosystem has stack-switching crates (`corosensei`, `may`, `generator-rs`, `fringe`) and stackless async runtimes (`tokio`, `monoio`, `smol`). Loom's specific "freeze frames to heap, thaw on different carrier" mechanism would require per-ISA assembly and DWARF integration, building from scratch.
- **The WebAssembly stack-switching proposal** is the closest production-bound implementation of Loom-style semantics for any target. When it stabilizes, Ryo's WASM target may get true Loom semantics natively. Native targets remain a separate question.

So this proposal **borrows Loom's FFI ergonomics goal** without inheriting Loom's implementation. The mechanism is corosensei's stack-switching (already in the plan) plus a per-OS-thread "system coroutine" with a large stack that hosts FFI calls. This is Go's g0 mechanism applied via corosensei — proven engineering, no new low-level primitives required.

True Loom-style stack-capture remains a possible **future optimization** if/when Ryo needs to support 1M+ concurrent suspended tasks per process. The proposal here is sized for Ryo's actual target audience: 10K–100K concurrent tasks per process, with FFI-heavy workloads.

---

## Two motivations for the proposal

1. **FFI ergonomics.** The current plan caps per-task stacks at 128 KB. C libraries that use more than that — image codecs, ML inference, some crypto, anything that recurses — risk `StackOverflow` on a Ryo task. Routing every FFI call through `#[blocking]` works but pays ~10 µs of thread-pool dispatch per call. Neither is acceptable for compute-heavy FFI. The Loom-borrowed answer: run FFI on a per-OS-thread coroutine that has a big stack.
2. **Runtime pool flexibility.** The current plan has two implicit pools (carrier workers and `#[blocking]`). Kotlin's explicit dispatcher abstraction generalizes this: users can declare custom pools and switch into them via `with_pool`. Useful for app-level isolation (bound DB queries to 16 concurrent, run image processing on a separate compute pool, etc.) without changing the colorless model.

The proposal preserves the rest of [`concurrency.md`](concurrency.md) — phases, primitives, spec semantics, `corosensei` and `mio` dependencies. Most of the doc still applies. The changes are surgical.

---

## What stays the same

The current plan and this proposal agree on:

- **Colorless concurrency.** No `async`/`await`, no `suspend`. Tasks look like synchronous code.
- **Ambient runtime via TLS.** No runtime handle passed by caller.
- **`task.run` / `future[T]` / channel primitives.** User-facing API unchanged.
- **Dropping a future cancels its task.** RAII-based cancellation.
- **Cooperative cancellation at suspension points only.**
- **RAII cleanup on cancellation (stack unwind).**
- **`mio` as the cross-platform I/O reactor.** epoll / kqueue / IOCP today; io_uring as a Linux opt-in feature later.
- **`corosensei` as the stack-switching primitive.** No replacement; the system-coroutine pattern is built *on top of* corosensei.
- **Per-task growable stacks** (32 KB initial, 128 KB max in v0.4). The cap is now safe for Ryo-only code because compute FFI runs elsewhere; Ryo code rarely needs deep stack.
- **Phase shape** — Phase 1 single-threaded foundation, Phase 2 I/O, Phase 3 multi-threading, Phase 4 channels + structured concurrency, Phase 5 hardening, Phase 6 compiler enforcements. Names and ordering hold.
- **`crossbeam-deque` work-stealing queues.**
- **Happens-before memory model.** Same Go-aligned guarantees.
- **Spec section 9.** Zero language-level changes.

Anyone reading the current `concurrency.md` and knowing about this proposal should mentally add three pieces (the system-coroutine FFI router, explicit `with_pool` API, `task.supervise` variant) and treat the rest as-is. About 90% of the current plan transfers directly.

---

## What changes

### Change 1 — FFI routing via per-OS-thread system coroutine

**Current plan (concurrency.md §3.4):**
- Plain FFI runs on the green-thread's 32–128 KB stack. **Risky** — if C uses more than 128 KB, the task hits the hard cap and gets a `StackOverflow` error.
- `#[blocking]` routes to a dedicated 10K-thread pool. ~10 µs dispatch overhead. Overkill for compute FFI that doesn't actually block.

**This proposal:**
- Each OS worker thread maintains a **system coroutine** at startup — a long-lived `corosensei::Coroutine` with a 2 MB stack, sized for any reasonable C library.
- Plain FFI calls are routed through the system coroutine:
  1. Task coroutine yields to scheduler with `FfiCallRequest`.
  2. Scheduler resumes the system coroutine on the same OS worker, passing the FFI closure.
  3. System coroutine runs the C call on its 2 MB stack.
  4. System coroutine yields back with the result.
  5. Scheduler resumes the original task coroutine.
- Cost: two extra stack-switches per FFI call (~200 ns total on x86_64). Compare to `#[blocking]` dispatch (~10 µs) — 50× cheaper.
- `#[blocking]` is still required for C that does its own blocking I/O (sqlite, libpq sync mode, blocking sockets opened outside Ryo). It continues to route to a dedicated thread pool.

This is **Go's cgo model** implemented via corosensei. Mature engineering, no new low-level primitives.

**Impact on the plan:**
- §3.4 (`#[blocking]` Thread Pool) is kept but narrowed: it's only for blocking-by-design FFI, not a catch-all. Default pool size can be smaller (1K instead of 10K — possibly tuned later).
- New §3.4a: System coroutine setup, FFI routing path.
- The 128 KB green-thread stack cap is no longer a stack-safety risk for FFI users. It remains a safety net for runaway Ryo recursion.

### Change 2 — Explicit dispatcher API (`with_pool`)

**Current plan:**
- Two implicit pools: the carrier pool (`RYOMAXPROCS` workers) and the `#[blocking]` thread pool.
- User cannot create app-specific pools.

**This proposal (Kotlin-borrowed):**
- First-class `Pool` type (Kotlin's `CoroutineDispatcher`).
- Built-in pools:
  - `pool::default` — the carrier pool (`RYOMAXPROCS` workers).
  - `pool::blocking` — overflow pool for blocking work (`#[blocking]` routes here).
  - `pool::compute` — CPU-bound pool, sized to CPU cores; optional.
- User-defined pools: `pool::custom(workers = 8, name = "db-pool")` — useful for app-level resource isolation (e.g., a fixed-size pool to bound DB concurrency).
- Explicit pool switch via `with_pool`:

  ```ryo
  fn handle_request(req: Request, db_pool: Pool) -> Response:
  	bytes = with_pool(db_pool):
  		sqlite::query(req.sql)            # runs on db_pool
  	return Response(parse(bytes))         # back on default pool
  ```

- `with_pool` is **not a coloring marker.** Calling functions don't need to know which pool the callee uses. The pool is a runtime concept, not a type-system one.

**Impact on the plan:**
- §3.4 gains a generalization. `#[blocking]` is now sugar for "auto-route this FFI to `pool::blocking`"; `with_pool` lets users pick a different pool explicitly.
- Phase 4 gains a §4.X (`Pool` and `with_pool`) — small additional API surface.
- Phase 6 may add a linter: "long-running CPU work on `pool::blocking` is wasteful; consider `pool::compute`."

### Change 3 — `task.supervise` scope variant (Kotlin's `supervisorScope`)

**Current plan (concurrency.md §4.2):**
- `task.scope: ...` cancels all siblings if any child panics or the scope exits early.

**This proposal (Kotlin-borrowed):**
- Add `task.supervise: ...` as a sibling primitive. Inside a supervisor scope, a child task failing does **not** cancel its siblings. The scope still awaits all children before returning. Failures are surfaced as part of each child's `Future<T>` result.
- Use case: a long-running parent task that spawns many independent workers, where one worker's failure shouldn't kill the others. Common in HTTP server request handlers, batch processors, monitoring agents.

**Impact on the plan:**
- §4.2 gains a sibling subsection: §4.2.1 `task.supervise`. Small implementation delta — same `JoinHandle` list, different failure-propagation policy.

### Change 4 — Channel mode taxonomy (Kotlin-borrowed)

**Current plan (concurrency.md §4.1):**
- Two variants: `std.channel.create[T]()` (unbounded) and `std.channel.bounded[T](capacity)` (bounded).

**This proposal (Kotlin-borrowed):**
- Four explicit modes:
  - `Channel.unbounded[T]()` — current `create[T]()`. Sender never blocks; memory unbounded.
  - `Channel.bounded[T](capacity)` — current `bounded[T]()`. Sender blocks at capacity.
  - `Channel.rendezvous[T]()` — capacity 0. Sender blocks until receiver picks up. Synchronous handoff. (Equivalent to `bounded[T](0)` but named for clarity.)
  - `Channel.conflated[T]()` — buffer of 1; new sends overwrite the unreceived value. Useful for "latest state" patterns (UI updates, sensor readings).
- The first two are exactly what the current plan provides. The latter two are net-new and trivial on the same `VecDeque` infrastructure.

**Impact on the plan:**
- §4.1 gains two channel modes. Each ~20 LOC additional. API surface widens slightly but discoverably (single `Channel` namespace).

---

## What's new (no current-plan counterpart)

### New 1 — Per-OS-thread system coroutine + FFI router

A long-lived `corosensei::Coroutine` per OS worker thread, owned by the scheduler. Setup at worker start; reused across many FFI calls. Each system coroutine has:

- A 2 MB stack (configurable via env var `RYO_FFI_STACK_SIZE`).
- A read-loop that accepts FFI closures from the scheduler and runs them.

The FFI router code lives in the scheduler. A plain `extern "C"` call site lowers to:

```rust
// pseudocode in codegen
let result = scheduler::call_ffi(|| unsafe {
    extern_function(args)
});
```

`call_ffi` performs the yield-to-system-coroutine handoff. ~150–250 LOC depending on detail.

### New 2 — Optional carrier-pinning telemetry

For the `#[blocking]` pool specifically, the runtime emits events when the pool is sustained-saturated:

- `pool_drained(pool_name, queue_depth)` — published when a pool has more pending tasks than active workers for more than a threshold (default 100 ms).

Hooks into the Phase 5.5 observability story. Cheap (small atomic counters checked at scheduler boundaries). Helps users tune custom pool sizes.

### New 3 — `with_pool` block syntax

Borrows from Kotlin's `withContext` but uses Ryo's `with`-block style (consistent with the spec's `with` semantics for resource lifetime). Implemented as a scheduler operation: the task's *home pool* is changed for the duration of the block, then restored.

---

## Comparison tables

### User-facing semantics

| Dimension | concurrency.md (current) | This proposal |
|---|---|---|
| Function coloring | None ✓ | None ✓ |
| `task.run` / `future[T]` API | Same | Same |
| `task.scope` | Same | Same |
| `task.supervise` | Not present | **Added** (Kotlin pattern) |
| Channels | unbounded, bounded | unbounded, bounded, rendezvous, conflated |
| Pool selection | Implicit (carrier + blocking) | Explicit via `with_pool(p)` (Kotlin pattern) |
| `#[blocking]` annotation | Required for any blocking C | Required only for C that blocks on its own I/O |
| Plain FFI safety | Risky on 128 KB stack | Safe on 2 MB system coroutine |
| Cancellation primitive | Drop on `Future<T>` | Same |
| Structured concurrency | `task.scope` | `task.scope` + `task.supervise` |
| Memory model | Happens-before, Go-aligned | Same |
| Spec impact | None (matches spec) | None (matches spec) |

### Runtime architecture

| Component | concurrency.md (current) | This proposal |
|---|---|---|
| Task stack | Per-task growable (32 KB → 128 KB), `corosensei` | **Same** — kept as-is |
| Stack-switching primitive | `corosensei::Coroutine` | **Same** |
| Worker / carrier OS threads | `RYOMAXPROCS` (= `NumCPU`) | Same |
| Per-OS-thread system coroutine | None | **New** — 2 MB stack, long-lived, hosts FFI |
| Work-stealing queues | `crossbeam-deque` | Same |
| I/O reactor | `mio` (epoll / kqueue / IOCP) | Same; io_uring as Linux opt-in feature (future) |
| Blocking-FFI pool | 10K threads cap, mandatory for blocking C | 1K threads cap (smaller default); only for `#[blocking]` |
| Plain FFI overhead | None — runs on green stack (risky) | ~200 ns (two stack switches via system coroutine) |
| Blocking FFI overhead | ~10 µs (thread-pool dispatch) | Same — ~10 µs |
| Pool API | Implicit | Explicit `Pool` type + `with_pool` |
| Stack-walking / heap-capture (Loom-proper) | Not present | **Not present in this proposal either** — deferred as future optimization |

### Implementation cost delta

| Component | Current plan effort | Proposal delta | Net |
|---|---|---|---|
| Stack abstraction (`corosensei` wrapper) | ~400 LOC | Unchanged | **0** |
| Per-OS-thread system coroutine + FFI router | None | ~250–400 LOC | **+250–400** |
| `Pool` type + `with_pool` | None | ~300–500 LOC | **+300–500** |
| `task.supervise` | None | ~150 LOC | **+150** |
| Channel rendezvous + conflated modes | None | ~150 LOC | **+150** |
| Pool-drained telemetry | None | ~100 LOC | **+100** |
| Smaller `#[blocking]` pool default | Implicit | Trivial config change | **~0** |
| **Net delta vs current plan** | baseline | **~+950–1300 LOC** | All additive — no replacement of existing primitives |

Compared to the earlier "Loom-proper with hand-rolled continuation capture" framing of this doc (~+500–1300 LOC of which 800–1500 was per-ISA assembly), this approach is **strictly less work**: no new low-level primitives, no per-ISA assembly, no new Rust dependencies beyond what's already in the plan.

### FFI scenarios

| Scenario | Current plan | Proposal |
|---|---|---|
| `libjpeg::decode(bytes)` (compute, no I/O) | Risky if libjpeg uses >128 KB stack | Safe — runs on 2 MB system coroutine. ~200 ns overhead. |
| `simdjson::parse(bytes)` (compute, no I/O) | OK on 128 KB | Same — also safe via system coroutine. |
| `sqlite3::query(db, sql)` (blocking I/O) | `#[blocking]`; ~10 µs dispatch | `#[blocking]`; ~10 µs dispatch. Same. |
| `openssl::aes_encrypt(key, data)` (compute) | OK on 128 KB | Safe on 2 MB. ~200 ns overhead. |
| `libpng::write(file, data)` (compute + libc write) | `#[blocking]` recommended; libc write may block | `#[blocking]` required. Same. |
| `libfoo::compute()` recursing 4 MB deep | Stack overflow at 128 KB → task fails | Stack overflow at 2 MB → task fails. Configurable via env var. |
| Callback from C into Ryo | Callback runs on green-thread stack (risky) | Callback runs on system-coroutine stack — safe to do work, can `task.spawn` cleanly |

### Memory profile at scale

| Concurrent task count | Current plan (32 KB+ per task) | Proposal (same as current — no change to stack model) |
|---|---|---|
| 10 K tasks | ~320 MB stack | Same |
| 100 K tasks | ~3.2 GB stack | Same |
| 1 M tasks | ~32 GB stack | Same |

This is the honest trade-off vs. true Loom: the per-task memory floor is corosensei's, not Loom's. For 1M+ concurrent tasks, true Loom-style would be ~8 GB instead of ~32 GB. For Ryo's target audience (10K–100K concurrent tasks per process), this is rounding error.

The proposal **explicitly defers** true Loom-style heap-capture as a future optimization. The path is:

- If/when 1M+ concurrent tasks become a real Ryo use case: revisit.
- If/when a `loom-rs` Rust crate matures (or someone contributes upstream to corosensei adding `freeze`/`thaw` modes): adopt.
- On the WASM target: when the [WebAssembly stack-switching proposal](https://github.com/WebAssembly/stack-switching) stabilizes, Ryo's WASM build can get true Loom semantics natively (Wasm engine handles the capture). Native and WASM may diverge in the runtime; that's fine.

---

## Code example: the same workload

Same workload — HTTP server with config, SQLite (blocking C), libjpeg (compute C):

### Current concurrency.md plan

```ryo
fn handle_request(req: Request, db: shared[SqliteDb]) -> Response:
	user = db.query(req.sql)              # #[blocking] inside SqliteDb.query
	avatar = libjpeg::decode(user.bytes)  # runs on 32-128 KB green-thread stack
	                                      # may overflow if libjpeg recurses
	return Response(user.name, avatar)

fn main():
	db = shared[SqliteDb](SqliteDb::open(cfg.db_path))
	http::serve(cfg.port, fn(req): handle_request(req, db))
```

### Proposal

```ryo
fn handle_request(req: Request, db: shared[SqliteDb]) -> Response:
	user = db.query(req.sql)              # #[blocking] — same ~10 µs dispatch
	avatar = libjpeg::decode(user.bytes)  # runs on 2 MB system coroutine — safe
	                                      # ~200 ns overhead vs current
	return Response(user.name, avatar)

fn main():
	db = shared[SqliteDb](SqliteDb::open(cfg.db_path))
	http::serve(cfg.port, fn(req): handle_request(req, db))
```

User-visible code is **identical**. The difference is entirely under the runtime:

- `libjpeg::decode` is safe regardless of internal stack usage.
- `db.query` cost is unchanged.

Adding an explicit DB pool (for connection limiting at the app level):

```ryo
fn main():
	db_pool = pool::custom(workers = 16, name = "db")
	db = shared[SqliteDb](SqliteDb::open(cfg.db_path))
	http::serve(cfg.port, fn(req):
		with_pool(db_pool):
			handle_request(req, db)         # SQLite queries bounded to 16 concurrent
	)
```

This is genuinely new capability — the current plan doesn't have a way to bound DB concurrency from Ryo code; you'd need a separate semaphore.

---

## What this proposal does not change

To be exhaustive about preservation from [concurrency.md](concurrency.md):

- All of Phase 1.1 (Stack Abstraction with `corosensei`) — kept entirely. Per-task growable stacks, 32 KB → 128 KB, guard pages, stack caching from day one. Unchanged.
- All of Phase 1.2 (Task and Future Primitives) — same API.
- All of Phase 1.4 (TLS Runtime Handle) — same caveat, same handling.
- All of Phase 1.5 (Cancellation Delivery) — same mechanism.
- All of Phase 1.6 (Panic Semantics) — same.
- All of Phase 2 (I/O Integration via `mio`) — same; same option to add io_uring later.
- All of Phase 3.1, 3.2, 3.3, 3.5, 3.6 (multi-threaded scheduler, work stealing, task pinning hooks, task-locals, memory model) — same.
- All of Phase 4.3, 4.4, 4.5, 4.6 (`select`, `task.gather/join/any`, cancellation sources, async drop) — same.
- All of Phase 5.1 (Stack Size Tuning) — kept. Still tunable, just less load-bearing now.
- All of Phase 6 (compiler enforcements: `with` blocks, yield-while-locked, ARC linter) — same.
- The risk register, milestone summary, dependency table — same with `#[blocking]` pool sizing adjusted downward.

This is a meaningful detail. The proposal is **not a rewrite of the concurrency story** — it's a targeted refinement of one area (FFI safety via system coroutine) and a handful of small additions (`with_pool`, `task.supervise`, channel modes).

---

## Open questions before committing

1. **System-coroutine stack size: 2 MB default, configurable?**
   - 2 MB matches Linux's typical `pthread_create` default; covers virtually all real C libraries.
   - Configurable via env var `RYO_FFI_STACK_SIZE` for unusual workloads (deep recursion, very large stack arrays).
   - **Recommendation:** 2 MB default, `RYO_FFI_STACK_SIZE` for tuning.

2. **System coroutine: one per OS worker thread, or shared pool?**
   - One per worker: simplest, no cross-worker synchronization needed for FFI calls. Each worker pays ~2 MB stack memory.
   - Shared pool: smaller memory footprint at low worker count, but adds synchronization to the FFI hot path.
   - **Recommendation:** one per worker. The memory cost (~16 MB for 8 workers) is negligible compared to per-task stack memory.

3. **`#[blocking]` annotation: keep or remove?**
   - Keep: explicit hint that this FFI is blocking-by-design; routes to `pool::blocking` automatically.
   - Remove: rely on runtime detection.
   - **Recommendation:** keep for v0.4 — better signal-to-noise from explicit annotation.

4. **`with_pool` syntax: block form vs function form?**
   - Block: `with_pool(p):\n\t...` — matches Ryo's `with` semantics for resource lifetime.
   - Function: `pool::run(p, fn(): ...)`.
   - **Recommendation:** block form.

5. **Channel mode default.**
   - Kotlin default is `RENDEZVOUS` (size 0).
   - Go default is unbounded.
   - Current Ryo plan defaults to unbounded.
   - **Recommendation:** keep unbounded as default; `rendezvous` and `conflated` are opt-in.

6. **Defer true Loom-style entirely, or carve out a future Phase 7?**
   - This proposal explicitly defers. No Phase 7 yet.
   - **Recommendation:** revisit only if/when memory profiling shows the 32 KB-per-task floor is a real constraint, or when a Rust `loom-rs` crate matures.

7. **WASM target: corosensei or stack-switching proposal?**
   - corosensei supports WASM (via Emscripten / wasm32 targets, with limitations) but the story is rougher than native.
   - The WebAssembly stack-switching proposal, when stable, gives true Loom semantics on WASM at no implementation cost to Ryo.
   - **Recommendation:** ship WASM concurrency on corosensei initially (matches native), then migrate to stack-switching proposal when it stabilizes. Wrapped in [wasm_target.md](wasm_target.md)'s feature-flagged WASM path.

---

## When to revisit

Trigger conditions for promoting this proposal from "alternative draft" to "the plan":

- **Before Phase 1 of concurrency implementation begins.** The system-coroutine pattern is small but non-trivial; deciding before commit saves work.
- **When concrete FFI workloads land.** If the early stdlib uses C libraries that exceed 128 KB stacks (image codecs, ML inference, some crypto), this proposal moves from "nicer to have" to "necessary."
- **When the WebAssembly stack-switching proposal reaches Phase 4** (W3C standardization). At that point, WASM target gets true Loom semantics; native may diverge or follow.
- **When a Rust crate matures that implements Loom's stack-capture primitive.** Doesn't exist today; if it appears with production-quality maintenance, revisit the "defer true Loom" decision.

Until then, [concurrency.md](concurrency.md) stays as the committed plan. This document is the structured alternative kept warm for the comparison.

---

## References

- Plan being compared against: [concurrency.md](concurrency.md) (Go-style M:N green threads via `corosensei`).
- Spec: [`docs/specification.md`](../specification.md) Section 9 (Concurrency).
- Sibling design docs: [`memory_model_comparison.md`](memory_model_comparison.md), [`rust_reference.md`](rust_reference.md), [`mojo_reference.md`](mojo_reference.md), [`arc_optimizer.md`](arc_optimizer.md), [`wasm_target.md`](wasm_target.md).
- Upstream prior art:
  - [JEP 444: Virtual Threads (Java 21 GA)](https://openjdk.org/jeps/444) — Loom (inspiration for the FFI ergonomics goal).
  - [Loom OpenJDK wiki](https://wiki.openjdk.org/display/loom/Main).
  - [Kotlin Coroutines Guide](https://kotlinlang.org/docs/coroutines-guide.html) — source of `with_pool`, `task.supervise`, channel mode patterns.
  - [`kotlinx.coroutines` source](https://github.com/Kotlin/kotlinx.coroutines).
  - [`corosensei`](https://github.com/Amanieu/corosensei) — Rust stack-switching primitive; the foundation of this proposal.
  - [`may`](https://github.com/Xudong-Huang/may) — production Rust green-thread runtime (alternative reference).
  - Go's cgo internals — origin of the system-stack switching pattern this proposal mirrors via corosensei.
  - [WebAssembly stack-switching proposal](https://github.com/WebAssembly/stack-switching) — future path to true Loom semantics on WASM.
