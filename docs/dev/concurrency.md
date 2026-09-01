**Status:** Design (v0.4)

# Ryo v0.4 Concurrency Implementation Plan
> Task / Future / Channel / Dispatcher — Green Thread M:N Runtime with System-Coroutine FFI

---

## Overview

This document describes the phased implementation plan for Ryo's concurrency
model. The goal is a working, cross-platform (Linux, macOS, Windows) M:N green
thread runtime backing `task.run`, `future[T]`, and channels by the v0.4
release.

**Lineage.** This plan supersedes the earlier draft of this document and
absorbs the adopted Loom/Kotlin alternative proposal
(`concurrency_loom_kt.md`). Two ideas from that proposal are now part of the
core design:

- **Loom-inspired FFI ergonomics.** Every compute FFI call runs on a
  per-OS-thread *system coroutine* with a megabyte-sized stack — Go's g0/cgo
  pattern implemented via `corosensei` — never on the green-thread stack
  (§3.5). True Loom-style stack capture (freeze frames to heap, thaw on
  another carrier) was investigated and rejected for v0.4: no Rust crate
  implements it in production-quality form, and hand-rolling it means
  per-ISA assembly plus DWARF integration. It remains a deferred future
  optimization (see *Memory profile at scale*); the WebAssembly
  stack-switching proposal is the closest production-bound realization (see
  the WasmFX section).
- **Kotlin-borrowed user-facing primitives.** Explicit dispatchers
  (`Dispatcher` + `with_dispatcher`, from `CoroutineDispatcher` /
  `withContext`), a supervisor scope (`task.supervise`, from
  `supervisorScope`), and a four-mode channel taxonomy.

`Dispatcher`, `with_dispatcher`, and `task.supervise` are **proposal-only** —
not stable user-facing APIs — unless/until their definitions are added to the
normative specification. Ryo remains colorless: no `async`/`await`, no
`suspend` keyword.

**Core stack:**
- `corosensei` — cross-platform stack switching
- `mio` — cross-platform I/O polling (epoll / kqueue / IOCP)
- `crossbeam-deque` — work-stealing task queues
- Custom scheduler — M:N green thread dispatch

**Design constraints inherited from spec:**
- No function coloring — tasks look like synchronous code to the user
- Ambient runtime via TLS — no runtime handle passed by caller
- Dropping a future cancels its task
- Cooperative cancellation at suspension points only
- RAII cleanup guaranteed on cancellation (stack unwind)

**Tunables and their defaults (Go-aligned):**

| Setting | Default | Notes |
|---|---|---|
| `RYOMAXPROCS` | `NumCPU` (logical cores) | Number of OS worker threads in the M:N scheduler. Same shape as Go's `GOMAXPROCS`. |
| Blocking thread pool cap | `1000` | Hard ceiling on concurrent `#[blocking]` calls. Smaller than Go's 10 000 M cap: only blocking-by-design FFI routes here (§3.4); compute FFI runs on the system coroutine (§3.5). Possibly tuned later. |
| Initial task stack | `32 KB` | Grows on demand. See §1.1. |
| Maximum task stack | `128 KB` (v0.4) | Hard cap; overflow delivers `StackOverflow` to the task. |
| `RYO_FFI_STACK_SIZE` | `2 MB` | System-coroutine stack size (§3.5). Matches Linux's typical `pthread_create` default; covers virtually all real C libraries. |
| `RYO_FFI_OVERFLOW_DEPTH` | `8` | Max nested FFI re-entry coroutines per worker (§3.5). |
| `RYO_MAX_DISPATCHERS` | `64` | Process-wide cap on live custom dispatchers (§4.5). |
| Dispatcher worker budget | `4 × RYOMAXPROCS` | Total `workers` across all custom dispatchers (§4.5). |
| Timer wheel resolution | `1 ms` | Sufficient for scripting workloads. |

> **Sibling reference docs:** [`memory_model_comparison.md`](pl_references/memory_model_comparison.md), [`rust.md`](pl_references/rust.md), [`mojo.md`](pl_references/mojo.md), [`arc_optimizer.md`](arc_optimizer.md), [`proposals/wasm_target.md`](proposals/wasm_target.md).

---

## Phase 1 — Foundation (Single-Threaded)

**Goal:** Get a single green thread switching correctly on all three platforms.
No scheduler beyond a FIFO queue, no real I/O, no channels. Just prove the
stack model works.

### 1.1 Stack Abstraction (Adaptive From Day One)

- Wrap `corosensei` in a `RyoStack` type owned by the runtime
- **Start at 32 KB, grow up to 128 KB on guard-page hit, hard-fail beyond**
- Guard page at the bottom of every stack
- On final guard-page hit (above 128 KB): deliver `StackOverflow` error to the
  task, not a process crash
- Stack caching from Phase 1 — keep a small free list of recently-released
  stacks at each size class (cheap, prevents allocator pressure later)
- Stack size configurable via spawn options for the future

```
RyoStack {
    coroutine: corosensei::Coroutine,
    initial_size: usize,    // default 32KB
    current_size: usize,    // current allocation
    max_size: usize,        // default 128KB in v0.4
    guard_page: bool,       // always true in v0.4
}
```

> **Why adaptive from Phase 1, not Phase 5:** A fixed 128 KB × 1 M tasks =
> 128 GB. That's a hard concurrency ceiling baked into the address-space
> layout. Retrofitting growth later means rewriting the stack allocator and
> auditing every assumption that depended on a fixed slot size. Doing it now
> costs little and keeps the door open.

The 128 KB cap is safe for Ryo-only code because compute FFI runs on the
system coroutine (§3.5), not on the green-thread stack. It remains a safety
net for runaway Ryo recursion.

### 1.2 Task and Future Primitives

- Define `Task<T>` as an owned handle to a green thread
- Define `future[T]` as the user-facing return value of `task.run`
- Implement `Drop` on `future[T]` to request cancellation
- States: `Pending | Running | Completed(T) | Cancelled | Panicked(Reason)`

```
future[T]:
    Drop  → sends CancelRequest to task
    .await → suspends caller until task completes, returns T
```

### 1.3 Minimal Cooperative Scheduler (Single OS Thread)

- A simple FIFO run queue of ready tasks
- `task.run` pushes a new task onto the queue
- `.await` on a future suspends the current task, yields to scheduler
- Scheduler loops: pop next ready task, resume it via `corosensei`
- No I/O yet — only `task.delay` (busy-wait stub for now)

### 1.4 TLS Runtime Handle (and the OS-TLS Caveat)

- Store the scheduler pointer in thread-local storage
- `task.run`, `.await`, channels all access it implicitly
- Establish the ambient runtime pattern that v0.4 will build on

> **Important distinction (forward-looking):** OS thread-local storage is
> *not* task-local. In Phase 3 a task can migrate between worker OS threads
> across yield points, and any value read from native TLS will change
> identity. Library authors must not store task-state in OS TLS.
> §3.6 introduces a `task_local!` macro that survives migration.

### 1.5 Cancellation Delivery (Basic)

- When a future is dropped, set a `cancel_requested` flag on the task
- At every suspension point (`.await`, channel ops), check the flag
- If set, unwind the task's stack via normal Ryo drop semantics
- Deliver `task.Canceled` as an error into the task's error union

### 1.6 Panic Semantics

A task that panics does not take down the runtime. Concretely:

- Panic unwinds the task's stack normally (running drops in reverse order)
- The task transitions to `Panicked(Reason)`
- `.await` on a panicked task's future propagates the panic to the awaiter
- A detached panicked task's panic is logged to stderr in v0.4 (a hook for
  user-defined panic handlers is a v0.5 concern)
- Inside `task.scope` (§4.2), a panicking child cancels its siblings and
  propagates the panic out of the scope
- Inside `task.supervise` (§4.2.1), a panicking child does **not** cancel its
  siblings; the failure surfaces in the child's `future[T]`

### 1.7 Test Strategy

- Per-platform smoke test: spawn 1 000 tasks, each does simple compute, all
  complete on Linux + macOS + Windows
- Cancellation correctness: drop a future mid-execution, assert all
  destructors ran in declaration-reverse order
- Panic correctness: panicking task delivers panic to awaiter, runtime
  remains usable
- Stack growth: synthetic test that recurses past 32 KB but under 128 KB
- Stack overflow: synthetic test that exceeds 128 KB — verify `StackOverflow`
  is delivered to the task, not a SIGSEGV

**Exit criteria:** Smoke test passes on all three platforms. Panic and
cancellation tests pass. Stack growth works.

---

## Phase 2 — I/O Integration

**Goal:** Real non-blocking I/O via `mio`. Tasks suspend on I/O, not busy-wait.

### 2.1 Integrate `mio` Event Loop

- Create a `mio::Poll` instance owned by the runtime
- Scheduler loop becomes: run all ready tasks → poll I/O events → wake waiting
  tasks → repeat
- `mio` handles epoll (Linux), kqueue (macOS), IOCP (Windows) transparently
- io_uring may be added later as a Linux opt-in feature; `mio` remains the
  cross-platform baseline

```
Scheduler loop:
    while true:
        drain ready_queue          // run all runnable tasks
        poll = mio.poll(timeout)   // block until I/O event or timeout
        for event in poll:
            wake_task(event.token) // move waiting task to ready_queue
```

### 2.2 Async File and Network I/O

- Wrap `mio`-backed TCP/UDP sockets as Ryo's standard `net` types
- `.send()` / `.recv()` on sockets suspend the task, register with `mio`,
  resume when ready
- File I/O on Linux via thread pool (files are not pollable on most OSes)
- **Define the `#[blocking]` FFI attribute now**, but dispatch is via a
  minimal single-thread fallback pool until §3.4 builds the real one.
  This avoids the previous draft's chicken-and-egg between 2.2 and 3.4.

### 2.3 Real `task.delay`

- Replace the busy-wait stub with a timer wheel
- Design reference: Tokio's `HashedWheelTimer` (MIT licensed — study freely)
- Resolution: 1 ms minimum, sufficient for scripting use cases
- `task.delay(duration)` suspends the task, wakes it after the timer fires

### 2.4 `task.timeout` Implementation

- Wraps any `future[!T]` with a timer
- On expiry: deliver `task.Timeout` to the waiting task
- Uses the same timer wheel as `task.delay`

### 2.5 Test Strategy

- HTTP echo server stress test: ≥10 K concurrent connections, no thread
  blockage
- Timer accuracy test: `task.delay(100ms)` accurate within 5 ms p99
- Timeout test: `task.timeout` delivers `Timeout` exactly once, never races
  with successful completion
- File I/O correctness: large-file read/write on all three platforms

**Exit criteria:** A Ryo HTTP server handles concurrent connections.
`task.delay(100ms)` accurate to ~5 ms p99. Timeout has no double-delivery
races.

---

## Phase 3 — Multi-Threading and Work Stealing

**Goal:** Spread tasks across multiple OS threads (M:N scheduling). Full
work-stealing. Route FFI safely.

### 3.1 Multi-Threaded Scheduler

- Spawn `RYOMAXPROCS` OS worker threads (default: `NumCPU`)
- Each worker thread has its own local run queue and its own TLS runtime
  handle
- Each worker thread runs its own `mio` poll loop
- Override via `RYOMAXPROCS` environment variable, matching Go's
  `GOMAXPROCS` convention

### 3.2 Work Stealing via `crossbeam-deque`

- Each worker owns a `Worker<Task>` deque (push/pop local end)
- Each worker holds `Stealer<Task>` handles to all other workers
- When a worker's local queue is empty: steal from a random other worker
- Stealing is the standard Chase-Lev algorithm — `crossbeam-deque` implements
  this correctly

```
Worker loop:
    task = local_queue.pop()
         ?? steal_from_others()
         ?? park_until_woken()
    execute(task)
```

### 3.3 Task Affinity and Pinning (Hook Only)

- By default tasks migrate freely between workers
- Reserve `task.pin()` API for tasks holding OS-thread-local resources (some
  C FFI libraries require this)
- Do not implement for v0.4 — design the hook, implement later
- Design constraint for the hook: a pinned task is non-migrating, so
  `with_dispatcher` (§4.5) inside a pinned region is a compile error (§6.2.6)

### 3.4 `#[blocking]` Thread Pool

- Separate thread pool for C that blocks on its own I/O (sqlite, libpq sync
  mode, blocking sockets opened outside Ryo)
- When a `#[blocking]` function is called from a green thread:
  1. Move the blocking call to the thread pool
  2. Suspend the green thread
  3. Resume when the thread pool call completes
- **Pool cap: 1 000 threads by default.** Starts small, grows on demand,
  shrinks idle threads after a timeout. The cap is smaller than Go's 10 000 M
  cap because the pool is narrowed to blocking-by-design FFI — compute FFI
  runs on the system coroutine (§3.5) instead.
- `#[blocking]` is sugar for "auto-route this FFI to `dispatcher.blocking`"
  (§4.5); an enclosing `with_dispatcher` block overrides that routing for the
  duration of the block (precedence rule, §4.5).
- This replaces the minimal fallback pool from §2.2

### 3.5 Per-OS-Thread System Coroutine and FFI Router

**Rationale.** Plain FFI on the green-thread stack is unsafe: a C library
that uses more than the 128 KB task-stack cap — image codecs, ML inference,
some crypto, anything that recurses — would hit `StackOverflow` on a Ryo
task. Routing every FFI call through the `#[blocking]` pool works but pays
~10 µs of thread-pool dispatch per call, unacceptable for compute-heavy FFI.
The fix borrows Loom's FFI ergonomics goal via Go's g0/cgo pattern: run FFI
on a dedicated big-stack coroutine.

**Mechanism.** Each OS worker thread maintains a **system coroutine** at
startup — a long-lived `corosensei.Coroutine`, owned by the scheduler, with
a 2 MB stack (configurable via `RYO_FFI_STACK_SIZE`; covers virtually all
real C libraries). One per worker, not a shared pool: no cross-worker
synchronization on the FFI hot path, and the memory cost (~16 MB for 8
workers) is negligible next to per-task stacks. The system coroutine runs a
read loop that accepts FFI closures from the scheduler and executes them.

A plain `extern "C"` call site lowers to:

```ryo
# pseudocode for the lowering of an extern call site
result = scheduler.call_ffi(fn(): extern_function(args))
```

`call_ffi` performs the yield-to-system-coroutine handoff (~150–250 LOC):

1. Task coroutine yields to scheduler with `FfiCallRequest`.
2. Scheduler resumes the system coroutine on the same OS worker, passing the
   FFI closure.
3. System coroutine runs the C call on its 2 MB stack.
4. System coroutine yields back with the result.
5. Scheduler resumes the original task coroutine.

Cost: two extra stack switches per FFI call (~200 ns total on x86_64),
versus ~10 µs for `#[blocking]` dispatch — roughly 50× cheaper. With compute
FFI on the system coroutine, the 128 KB green-stack cap is no longer a
stack-safety risk for FFI users; it remains a safety net for runaway Ryo
recursion.

**Nested FFI re-entry from C callbacks.** A C library may call back into Ryo
code (qsort comparators, libjpeg error handlers, visitor APIs), and that
callback may itself make an FFI call while the worker's primary system
coroutine is still occupied by the outer call. Policy: the inner FFI call is
routed to an **overflow system coroutine** — an extra large-stack coroutine
created on demand on the same worker, cached for reuse, and bounded per
worker by `RYO_FFI_OVERFLOW_DEPTH` (default 8). When the bound is reached,
the inner FFI call **fails immediately** with an explicit re-entry-limit
error (`FfiReentryLimit`) — it must not queue and suspend the current task,
because the suspended task may be exactly what the occupied system coroutines
are waiting on, so queueing risks deadlock. (A non-blocking
`try_call_ffi`-style fallback that reports busy instead of failing is a
possible later addition.) Below the limit, overflow coroutines are reused
across nested calls.

### 3.6 Task-Local Storage

The OS-TLS caveat from §1.4 becomes load-bearing once tasks migrate. Provide:

```
task_local! { static REQUEST_ID: Cell<u64> = Cell::new(0); }
```

Implementation: each task owns a small map of task-local slots. Reads and
writes go through the current task's map, not OS TLS. Survives migration.
Inherited by child tasks spawned within the same `task.scope` (§4.2)
unless explicitly overridden.

### 3.7 Memory Model

Once tasks share state across worker OS threads, users need visibility
guarantees. Ryo adopts a **happens-before model close to Go's**:

- A successful `tx.send(v)` happens-before the matching `rx.recv()` returning
  `v`
- `mutex.lock()` acquires happen-before subsequent acquires of the same mutex
- A task spawn happens-before the first instruction of the spawned task
- A task's last instruction happens-before its future's `.await` returning

Plain reads/writes of `shared[T]` without synchronization are a data race
and undefined behavior. The Phase 6 enforcements catch the most common
locking mistakes; the memory model defines what "correct" code is allowed
to assume.

A dedicated concurrency memory-model document will spell this out
precisely when the concurrency milestone is designed. This phase only
commits to the high-level shape.

### 3.8 Test Strategy

- CPU-bound benchmark: 10 000 tasks doing pure computation, scaling near
  linearly with `RYOMAXPROCS`
- Steal stress test: deliberately imbalanced producer/consumer pattern,
  verify all cores stay busy
- Migration correctness: task that yields and resumes verifies its
  task-locals survived
- `#[blocking]` saturation: 1 000 simultaneous blocking calls do not stall
  the scheduler
- FFI router correctness: a C library recursing past 128 KB (but under
  `RYO_FFI_STACK_SIZE`) completes via plain FFI; one recursing past 2 MB
  delivers `StackOverflow` to the task
- FFI re-entry: a C callback that itself calls FFI works up to
  `RYO_FFI_OVERFLOW_DEPTH` nested calls; one level deeper fails immediately
  with `FfiReentryLimit` and no task is left suspended
- FFI overhead benchmark: plain FFI round-trip ≈200 ns on x86_64

**Exit criteria:** Linear scaling for embarrassingly parallel workloads.
`task_local!` works across migration. Blocking pool does not stall green
threads. Compute FFI is stack-safe via the system coroutine, and re-entry
overflow fails fast without deadlock.

---

## Phase 4 — Channels and High-Level Primitives

**Goal:** Implement all concurrency primitives described in the spec, plus
the proposal-only additions (dispatchers, `task.supervise`).

### 4.1 Channels

Four modes on the same infrastructure:

```
std.channel.create[T]() -> (sender[T], receiver[T])      # unbounded (default)
std.channel.bounded[T](capacity) -> (sender[T], receiver[T])
std.channel.rendezvous[T]() -> (sender[T], receiver[T])
std.channel.conflated[T]() -> (sender[T], receiver[T])
```

- **MPMC by default.** `sender[T]` and `receiver[T]` are reference-counted
  handles; `clone()` produces another handle to the same end. The channel
  closes on that side when the **last** clone is dropped.
- `channel.unbounded[T]()` (= `create[T]()`) — sender never blocks; memory
  unbounded. This is the default mode.
- `channel.bounded[T](capacity)` — sender blocks at capacity.
- `channel.rendezvous[T]()` — capacity 0. Sender blocks until the receiver
  picks up. Synchronous handoff. (Equivalent to `bounded[T](0)` but named
  for clarity.)
- `channel.conflated[T]()` — buffer of 1; new sends overwrite the unreceived
  value. Useful for "latest state" patterns (UI updates, sensor readings).
  Opt-in. The overwrite-vs-receive happens-before rule is **draft** — see
  the data-plane section.
- `tx.send(value)` — moves value into channel, suspends if buffer full
- `tx.try_send(value)` — non-yielding; returns `Full` error if buffer full
  (lock-safe; see Phase 6)
- `rx.recv()` — suspends until message available, returns value
- `rx.try_recv()` — non-yielding; returns `Empty` error if no message
  (lock-safe)
- Closing a channel delivers `Closed` error to all subsequent receivers
  *after* the buffer drains; closing the receive side delivers `Closed` to
  subsequent senders immediately

Internal implementation:
- `VecDeque<T>` as the ring buffer (the two new modes are ~20 LOC each on
  the same code)
- Intrusive wait lists for suspended senders and receivers
- No `Mutex` held during task resumption — only during queue manipulation

### 4.2 `task.scope` (Structured Concurrency)

- All tasks spawned inside a scope must complete before the scope exits
- If any task panics or the scope exits early: cancel all remaining tasks
  and await their unwind (the scope blocks until all children are settled)
- Scope holds a `JoinHandle` list; on exit, awaits all of them
- **This is the recommended default over `task.spawn_detached`**

### 4.2.1 `task.supervise` (Supervisor Scope)

> **Status:** proposal-only API (Kotlin's `supervisorScope`) — not yet in the
> normative specification.

- Sibling primitive to `task.scope`. Inside a supervisor scope, a child task
  failing does **not** cancel its siblings. The scope still awaits all
  children before returning. Failures are surfaced as part of each child's
  `future[T]` result.
- Children yield **both** handles to the supervisor scope: the identity-only
  `handle[T]` (spec §9.2.1 — sendable, comparable, no dereference; Pony's
  `tag`) for supervision — the supervisor can hold, registry-store, and
  compare child handles, and signal them through channels — and an awaitable
  `future[T]` join/result handle so the supervisor can await each child and
  observe its result or failure. The two are not interchangeable: `handle[T]`
  has no dereference and cannot be joined; `future[T]` joins but is not a
  stable identity for registry use.
- Use case: a long-running parent task that spawns many independent workers,
  where one worker's failure shouldn't kill the others. Common in HTTP server
  request handlers, batch processors, monitoring agents.
- Implementation: same `JoinHandle` list as `task.scope`, different
  failure-propagation policy (~150 LOC).

### 4.3 `select`

- Waits on multiple concurrency primitives simultaneously
- First ready case wins; all others are cancelled atomically (waker
  deregistration is part of the win, not best-effort)
- `default` branch makes the select non-blocking
- Implementation: register all cases as wakers, first waker to fire wins,
  deregister the rest

```
select:
    case msg = rx.recv():     // channel receive
        handle(msg)
    case res = fut.await:     // future completion
        handle(res)
    case task.delay(1s).await: // timer
        print("timed out")
    default:                   // non-blocking
        print("nothing ready")
```

### 4.4 `task.gather` / `task.join` / `task.any`

- `task.join([futures])` — homogeneous, waits for all, returns `list[T]`
- `task.gather([futures])` — heterogeneous, waits for all, returns tuple
- `task.any([futures])` — returns first to complete, cancels the rest
- All three implemented on top of `select` internally

### 4.5 `Dispatcher` and `with_dispatcher`

> **Status:** proposal-only API (Kotlin's `CoroutineDispatcher` /
> `withContext`) — not yet in the normative specification.

Kotlin's explicit dispatcher abstraction generalizes the two implicit pools
(carrier workers and `#[blocking]`) into user-declarable scheduler execution
contexts. Useful for app-level isolation — bound DB queries to 16 concurrent,
run image processing on a separate compute dispatcher — without changing the
colorless model.

- First-class `Dispatcher` type (Kotlin's `CoroutineDispatcher`). The name is
  deliberate: a `Dispatcher` is a scheduler execution context, and is
  unrelated to the `std.pool` resource-pool namespace (connection pools,
  object pools). The two never interact; examples keep them terminologically
  separate.
- Built-in dispatchers:
  - `dispatcher.default` — the carrier pool (`RYOMAXPROCS` workers).
  - `dispatcher.blocking` — overflow pool for blocking work (`#[blocking]`
    routes here).
  - `dispatcher.compute` — CPU-bound pool, sized to CPU cores; optional.
- User-defined dispatchers: `dispatcher.custom(workers = 8, name = "db")` —
  app-level resource isolation (e.g., a fixed-size dispatcher to bound DB
  concurrency).
- Explicit dispatcher switch via `with_dispatcher` — block form only,
  matching Ryo's `with` semantics for resource lifetime. Implemented as a
  scheduler operation: the task's *home dispatcher* changes for the duration
  of the block, then is restored:

  ```ryo
  fn handle_request(req: Request, db_dispatcher: Dispatcher) -> Response:
  	bytes = with_dispatcher(db_dispatcher):
  		sqlite.query(req.sql)            # runs on db_dispatcher
  	return Response(parse(bytes))         # back on default dispatcher
  ```

- `with_dispatcher` is **not a coloring marker.** Calling functions don't
  need to know which dispatcher the callee uses. The dispatcher is a runtime
  concept, not a type-system one.
- `with_dispatcher` is a **yielding operation.** Entering and leaving the
  block are suspension points, and the task may migrate across OS worker
  threads (to a worker of the target dispatcher, and back on exit). Two
  consequences:
  - A task holding a lock guard (`shared[mutex[T]]` / `shared[rwlock[T]]`)
    must not cross a `with_dispatcher` boundary — the compiler diagnoses it
    under the yield-while-locked enforcement (§6.2).
  - `task.pin()` (§3.3) marks a non-migrating critical section: the compiler
    diagnoses `with_dispatcher` used inside a `task.pin()` region, since a
    dispatcher switch may migrate the task. Code that must stay on one
    carrier switches dispatchers outside the pinned region.
- **Precedence rule:** an enclosing `with_dispatcher` block wins — for the
  duration of the block, calls inside it run on that dispatcher, overriding
  `#[blocking]` auto-routing to `dispatcher.blocking`. (If `#[blocking]` won
  instead, the bounded-concurrency example in the appendix would be void.)
- **Capability-gated creation (hard contract, not convention).**
  `dispatcher.custom` from arbitrary code can starve the runtime. The runtime
  enforces a process-wide limit of live custom dispatchers
  (`RYO_MAX_DISPATCHERS`, default 64), and their `workers` threads count
  against a total extra-worker budget of 4 × `RYOMAXPROCS`. Creation that
  would exceed either limit fails immediately with a `ResourceExhausted`
  error at the `dispatcher.custom` call — it does not queue and does not
  silently degrade. Authority: creation requires the runtime-context
  capability, which is granted at startup/main scope and is not obtainable by
  library code — matching Pony's authority-enters-at-one-point principle.
  Until the planned capability injection lands, the numeric limits are
  enforced globally at every call site — but they are only
  resource-exhaustion protection, not authority: they do not distinguish
  callers, so `dispatcher.custom` must not be exposed to untrusted library
  code before the capability check lands.

See the appendix for a full workload example (bounded DB concurrency).

### 4.6 Cancellation Sources

| Source | Mechanism | Sync or async? |
|---|---|---|
| `drop(future)` | Sets cancel flag, wakes task | Sync request, async observation |
| `task.scope` exit | Cancels all child futures, awaits unwind | Async — scope blocks until done |
| `select` losing case | Cancels non-winning operations | Async — `select` blocks until cancelled siblings settle |
| `task.timeout` expiry | Delivers `Timeout` at next suspension | Async |
| `fut.cancel()` | Explicit cancel call | **Async** — returns a future that resolves when the cancelled task has fully unwound. `fut.cancel_now()` is the fire-and-forget variant for cases where the caller does not need to await unwind. |

### 4.7 Async Drop / Destructor-Yield Semantics

This is the design call most likely to bite later if left ambiguous. Ryo's
position for v0.4:

**Destructors may yield.** They run on the task's stack like any other code,
and may call `.await`, `recv`, or `send`. This is necessary for clean
network teardown, buffered-writer flushing, etc.

**A destructor running because of cancellation cannot itself be cancelled.**
Once unwind starts, further cancel requests are deferred until unwind
completes. This prevents "cancellation of cancellation" pathologies.

**Destructors have a soft deadline.** A destructor that yields for longer
than `unwind_deadline` (default 5 s, configurable per scope) is logged and
the task is force-terminated, leaking that destructor's resources. This is
the lesser evil compared to wedging a `task.scope` indefinitely.

This is the same shape as Trio's "shielded cleanup" and Kotlin's
`NonCancellable` context. We borrow the semantics, not the names.

### 4.8 Test Strategy

- All §9.4 spec examples pass
- `task.scope` leak test: child task that holds a file handle, parent panics,
  assert handle is closed before scope exits
- `select` race test: 1 000 iterations of contended select, no zombie wakers
- Channel ownership test: clone senders, drop them in arbitrary order, verify
  close happens exactly when last clone drops
- Channel modes: rendezvous handoff synchronizes sender and receiver;
  conflated overwrite keeps exactly the latest value and never blocks the
  sender
- `task.supervise`: failing child leaves siblings running; scope still awaits
  all children; each child's `future[T]` surfaces its own failure; the
  supervisor receives both `handle[T]` and `future[T]` per child
- Dispatcher switch: work inside `with_dispatcher(d)` runs on `d`'s workers;
  a 16-worker custom dispatcher bounds concurrent DB queries to 16; creation
  beyond `RYO_MAX_DISPATCHERS` or the worker budget fails with
  `ResourceExhausted`
- Async drop test: destructor that yields completes before scope exit;
  destructor that exceeds `unwind_deadline` logs and force-terminates

**Exit criteria:** All spec examples in §9.4 run correctly. `task.scope`
prevents resource leaks under cancellation. `task.supervise` isolates child
failures without losing results. `select` with `default` is non-blocking.
Channel ownership is correct under reference counting. Dispatchers bound
concurrency as declared, and creation limits are enforced.

---

## Phase 5 — Hardening and Performance

**Goal:** Production-ready runtime. Correct under adversarial conditions,
tuned for Ryo's scripting workloads.

### 5.1 Stack Size Tuning

The adaptive stacks from §1.1 are working but not yet tuned:

- Profile real Ryo programs to find p99 stack depth
- Tune the size-class progression (32 KB → 64 KB → 128 KB? other shape?)
- Tune the stack cache size and eviction policy (stacks are per-size-class,
  cache by class)
- Validate that stack growth on guard-page hit is cheap enough to not be a
  performance cliff in real workloads
- Tune `RYO_FFI_STACK_SIZE` if real C libraries approach the 2 MB default
  (deep recursion, very large stack arrays)

This tuning is less load-bearing than it would be without §3.5: compute FFI
no longer pressures the 128 KB task-stack cap.

### 5.2 Deadlock Detection

- In debug mode: detect when all tasks are suspended and no I/O is pending
- Report the cycle with task IDs and suspension points
- Matches the `mutex` deadlock detection described in §9.2.4

### 5.3 Scheduler Fairness

- Add a task age counter — tasks that have been in the queue longest get
  priority
- Prevents starvation in high-throughput workloads
- Provide `task.yield_now()` for CPU-bound tasks (equivalent to Go's
  `runtime.Gosched()`)

### 5.4 Windows IOCP Hardening

- IOCP is completion-based, not readiness-based — different from epoll/kqueue
- `mio` abstracts this, but test explicitly:
  - Stack unwinding through IOCP callbacks
  - `#[blocking]` thread pool interaction with IOCP
  - SEH (Structured Exception Handling) compatibility via `corosensei`
- **Windows CI runs from Phase 1 onward** (per risk register) — Phase 5
  hardens, it does not introduce, Windows support

### 5.5 Observability Hooks

- Task IDs visible in debug output and panic messages
- `task.current_id()` for user-facing introspection
- Runtime stats in debug mode: active tasks, queue depth, steal count,
  blocking-pool size, stack-class distribution
- `pool_drained(pool_name, queue_depth)` event — published when a pool has
  more pending tasks than active workers for more than a threshold (default
  100 ms). Cheap: small atomic counters checked at scheduler boundaries.
  Helps users tune custom dispatcher sizes.
- Foundation for a future `ryo profile` tool

### 5.6 Test Strategy

- Loom-style permutation testing of the scheduler core (channels, select,
  cancellation) — catches scheduler-order-dependent races
- 24-hour soak test on each platform: random workload, no leaks, no panics,
  no deadlocks
- FFI router stress: system coroutines saturated with nested C callbacks —
  verify fail-fast at `RYO_FFI_OVERFLOW_DEPTH`, no deadlock, no coroutine
  leak
- Adversarial cancellation: deeply nested `task.scope`s with random
  cancellation injection
- Deadlock detector: synthetic deadlock test, verify the report names the
  right tasks and locks

**Exit criteria:** Soak test passes on all three platforms. Loom tests pass.
Deadlock detector reports actionable cycles.

---

## Phase 6 — Compiler Enforcements for Concurrency Safety

**Goal:** Provide Rust-like safety for global mutable state and locks
without lifetime annotations and without function coloring, leveraging
Ryo's whole-program AOT compilation.

### 6.1 Mandating `with` Blocks for Synchronization Guards

- **Problem:** Because Ryo drops variables at scope exit in reverse-declaration
  order, assigning lock guards to variables (`mut m = cache.lock()`) can
  lead to holding locks longer than intended or risking read-write deadlocks.
- **Implementation:** Types returning synchronization guards (e.g.,
  `mutex[T].lock()`, `rwlock[T].read_lock()`) **cannot be bound using
  standard variable assignment**. They must be consumed by a `with` block.
- **Outcome:** Creates a strict, visually obvious critical section.
  ```ryo
  # ❌ Compile Error: Guard must be scoped using a 'with' block
  mut m = CACHE.lock()

  # ✅ Allowed
  with CACHE.lock() as m:
      m.insert(key, val)
  ```

### 6.2 Yield-While-Locked Static Analysis

The goal: **physically prevent the #1 cause of green-thread deadlocks
without forcing function coloring on the developer.**

#### 6.2.1 Inferred effect, not declared effect

Yielding is an **inferred property** of a function body, not a declaration
the user writes. Think of it as Ryo's analog of Rust's `Send`/`Sync` auto
traits.

1. **Leaf primitives are tagged by the compiler:** `recv`, `await`, `delay`,
   timer ops, bounded-channel `send` when buffer is full, `with_dispatcher`
   entry/exit (§4.5).
2. **Propagation is inferred upward** through the call graph during
   whole-program AOT compilation. A function is `[yields]` iff its body
   transitively calls a `[yields]` operation. Recursion is handled by
   fixed-point iteration.
3. **No annotation is required on traits, impls, or function signatures.**
   The compiler derives the property from bodies.
4. **Generic functions are effect-polymorphic** in their type parameters,
   the same way `Vec<T>: Send iff T: Send` works in Rust.

#### 6.2.2 The dyn problem, solved by whole-program devirtualization

The only thing the compiler cannot infer from a body is `dyn Trait`,
function pointers, and other erased-implementation values. Ryo handles
this in three layers:

| Case | Compiler does | User does |
|---|---|---|
| Static dispatch inside lock | Inferred-effect check | Nothing |
| `dyn` with bounded reachable impls, all non-yielding | Devirtualizes against the reachable impl set, proves safe | Nothing |
| `dyn` with bounded reachable impls, ≥1 yielding | **Hard error** with concrete trace to the yielding impl | Move call outside the `with` block |
| `dyn` with unbounded impls (FFI callbacks, plugins) | Hard error by default | Use `dyn Trait + no_yield` to assert non-yielding, or move the call out |

Because Ryo is whole-program AOT, the reachable impl set for a `dyn Trait`
in a given program is **statically known** for the vast majority of cases.
The compiler enumerates implementations, checks each for the `[yields]`
property, and either proves the call safe or produces a hard error naming
the offending impl.

#### 6.2.3 Why hard error, not warning

A warning the user can `#[allow]` away under deadline pressure converts a
static guarantee into a lint. The whole point of Phase 6.2 is the
guarantee. We follow Rust's `Send`/`Sync` precedent: hard errors at the
boundary, never warnings.

The cost is acceptable because the **intersection** of (uses `dyn Trait`)
and (calls it inside `with lock()`) is genuinely rare. Most lock bodies
operate on concrete types — `cache.insert(k, v)`, `counter += 1`,
`state.transition()`. Calling unknown code inside a critical section is
exactly the deadlock anti-pattern we want to reject.

#### 6.2.4 The `+ no_yield` escape hatch

Reserved for the **unbounded-impl case only** — FFI callbacks, runtime-loaded
plugins, anything the linker does not see at compile time. Not a default,
not something users hit in normal code:

```ryo
# I'm constructing a dyn from an unbounded source and I assert it cannot yield.
# The runtime traps if a yielding implementation is ever passed in.
fn install_callback(cb: dyn Logger + no_yield):
    ...
```

The 99% of code never writes this annotation.

#### 6.2.5 Channels: split the API

Inside a `with lock()`, blocking channel ops are rejected; non-blocking
variants are fine. The split is in the API itself:

```ryo
# Inside a lock — these are statically rejected:
tx.send(x)        # may yield (bounded, buffer full)
rx.recv()         # always yields

# Inside a lock — these are fine:
tx.try_send(x)    # non-yielding; returns Full
rx.try_recv()     # non-yielding; returns Empty
unbounded_tx.send(x)  # statically non-yielding (if the type is unbounded)
```

Unbounded `sender[T]` carries enough type information for `send` to be
statically non-yielding. Bounded `sender[T].send` is `[yields]`. Rendezvous
`send` always yields (blocks until pickup); conflated `send` never yields
(overwrites the unreceived slot). The user chooses the right tool for the
location.

#### 6.2.6 `with_dispatcher` and pinned regions

`with_dispatcher` (§4.5) is a yielding operation that may migrate the task
across OS worker threads. Two diagnoses follow:

- Holding a lock guard across a `with_dispatcher` boundary is
  yield-while-locked and is rejected by this analysis like any other yield.
- Once the `task.pin()` hook (§3.3) is implemented, `with_dispatcher` inside
  a pinned region is a compile error: a dispatcher switch may migrate the
  task, violating the pin. Code that must stay on one carrier switches
  dispatchers outside the pinned region.

#### 6.2.7 Error message requirements

This is the make-or-break implementation detail. The error must show:

1. The call site (inside the lock)
2. The active lock guard, with its `with` line
3. **At least one concrete impl from the reachable set that yields, with
   the path to the yield point**

Example:

```
error: cannot call possibly-yielding `dyn Logger.log()` while holding lock `cache`
   ┌─ src/handler.ryo:42:9
   │
40 │     with cache.lock() as c:
   │          ─────────────  lock acquired here
41 │         c.insert(k, v)
42 │         logger.log("done")
   │         ^^^^^^^^^^^^^^^^^^ may yield
   │
   = note: `RemoteLogger` implements `Logger.log` and yields here:
           src/logging/remote.ryo:18 → tcp.send() at line 24
   = help: move this call outside the `with` block, or use a non-yielding logger
```

Without the impl trace, the error is unactionable. Implementing this trace
is non-trivial but is the single biggest determinant of whether users
accept Phase 6.2 or fight it.

### 6.3 Linter: ARC Overhead in Loops (`shared[T].clone()`)

- **Problem:** Cloning a `shared[T]` pointer inside a tight loop causes
  cache contention from atomic refcount instructions (cache-line bouncing).
- **Implementation:** **Flow-sensitive**, not pure AST. The linter detects:
  - A `shared[T]` value originating outside a loop
  - Cloned inside the loop body
  - Where the clone is **not** moved into a spawned task or sent over a
    channel (those clones are intentional, not redundant)
- **Outcome:** Catches accidental refcount churn without false-positiving
  on the legitimate "clone-then-move-into-task" pattern.
- A further candidate lint once dispatchers (§4.5) land: "long-running CPU
  work on `dispatcher.blocking` is wasteful; consider `dispatcher.compute`."

### 6.4 Test Strategy

- Snapshot-test the inferred-effect propagation on a curated corpus of
  real Ryo programs (catches regressions in the analysis)
- Snapshot-test error messages (especially the dyn impl trace — small
  output changes are easy to break)
- Compile-time benchmark: measure the cost of effect propagation on a
  representative large program; track over time
- Negative tests: every documented "this should be rejected" case has a
  test that confirms rejection with the right error — including
  `with_dispatcher` inside a `with lock()` critical section

**Exit criteria:** All §9.x spec examples involving locks pass. Compile-time
overhead from the analysis is under 10% on the benchmark corpus. Error
messages include the impl trace.

---

## Data plane: Pony cross-reference (added 2026-08)

The runtime mechanics above schedule closures and say nothing about what
data may flow across them. The base data-plane rules are settled at the spec
level (2026-08 amendments, informed by the Pony comparison in
[`experimental/ryo-vs-pony.md`](../experimental/ryo-vs-pony.md)). The
plan-specific rules further below — conflated-channel ordering, FFI callback
captures, dispatcher-local state — are **draft/open**, pending formalization,
and nothing in this section should be read as committing them:

- **Sharing freezes (spec §5.6).** Access through `shared[T]` is read-only;
  shared mutation requires `shared[mutex[T]]` / `shared[rwlock[T]]`. The
  `shared[SqliteDb]` examples in this doc assume `SqliteDb` is internally
  synchronized; a plain mutable `SqliteDb` would need the `mutex` wrapper.
- **`handle[T]` (spec §9.2.1).** `task.spawn_detached` returns an
  identity-only, sendable, comparable handle with no dereference — Pony's
  `tag`. This is what `task.supervise` needs to become supervision rather
  than just failure isolation: the supervisor can hold, registry-store, and
  compare child handles, and signal them through channels. Hence §4.2.1:
  `task.supervise` children yield **both** handles to the supervisor scope —
  the identity-only `handle[T]` for supervision, and an awaitable `future[T]`
  join/result handle so the supervisor can await each child and observe its
  result or failure. The two are not interchangeable: `handle[T]` has no
  dereference and cannot be joined; `future[T]` joins but is not a stable
  identity for registry use.
- **Send predicate (spec §14.5.6 #6).** A value crosses a task boundary in
  exactly three ways: owned move (Pony's `iso`), `shared[T]` handle (`val`),
  `handle[T]` (`tag`). Views cross only inside a `task.scope` (D5). Every
  primitive in this plan must be checkable against that predicate.

New sendability edges this plan introduces — to be folded into the pending
Ownership-Lite formalization (Q10 / Polonius fragment):

1. **Conflated-channel overwrite.** `channel.conflated` destroys an
   unreceived owned value on the *sender's* context while a receiver may
   concurrently observe the slot. Happens-before edges must be specified per
   channel mode: rendezvous is a synchronization point and gets a clean HB
   edge for free; conflated needs an explicit overwrite-vs-receive rule (the
   Go-aligned HB model has no conflated precedent).
2. **FFI-originated tasks.** A C callback running on the system coroutine
   can `task.spawn` (see the FFI scenarios table). The send predicate must
   state what a callback-spawned closure may capture — FFI pointers are
   `handle[T]`-shaped, but Ryo values captured across the C boundary need a
   rule.
3. **Dispatcher migration.** `with_dispatcher` changes a task's home
   dispatcher mid-execution; captures travel with the task (fine), but
   dispatcher-local and task-local state must not leak across the switch.

Two Pony imports adopted as runtime design principles:

- **Per-carrier reclamation locality (ORCA instinct, no GC needed).** This is
  a **goal, not a guarantee**: channel ownership transfers and
  `with_dispatcher` migration can free memory on a different thread than the
  one that allocated it. Those cross-thread frees go through mimalloc's
  remote-free path (deferred to the owning heap's thread), so correctness
  holds even when locality does not. Stating it matters because
  conflated-channel drops and dispatcher migrations would otherwise silently
  shift reclamation across threads.
- **Capability-gated dispatcher creation (AmbientAuth instinct).** The hard
  contract lives in §4.5: `RYO_MAX_DISPATCHERS`, the extra-worker budget,
  `ResourceExhausted` on overflow, and capability-based authority at
  startup/main scope. The global limits are resource-exhaustion protection,
  not authority — Pony's authority-enters-at-one-point principle is only
  fully realized once the planned runtime-context capability injection lands.

---

## Gleam/BEAM cross-reference (added 2026-08)

Gleam is a statically-typed language whose **compiler** is written in Rust,
but it does not implement lightweight processes itself — it compiles to
BEAM bytecode, and processes are ERTS primitives (C). Gleam's contribution
is a *type-safe API layer* over the BEAM's existing machinery: typed
`Subject(message)` addresses into a process mailbox, a composable
`Selector(payload)` for multi-subject receive, `process.call` with a
**mandatory timeout**, actors (typed `gen_server`), supervisors, and
`spawn` that **links by default** (`spawn_unlinked` is the explicit escape).
BEAM internals worth benchmarking against: preemptive scheduling via
reduction counting (~4000 reductions between preemptions), one scheduler
thread per core with per-scheduler run queues, per-process stack/heap/
mailbox isolation with per-process copying GC, and **dirty schedulers** —
separate pools (dirty-CPU / dirty-IO) for long or blocking NIFs, which is
the same split as §3.4 `#[blocking]` + §3.5 system coroutine, ~15 years
earlier.

Lessons, mapped onto this plan:

1. **Colorlessness validated.** Gleam's docs stress exactly Ryo's core
   constraint: no `async`/`await`, async code reads as synchronous code.
   BEAM and Go both converged here independently.
2. **Preemption is the biggest divergence — and a known risk.** The plan
   rejects preemptive scheduling (cooperative + fairness counter +
   `task.yield_now`, §5.3). BEAM shows preemption's payoff: no task —
   including an accidental infinite loop — can starve a worker. Reduction
   counting also shows preemption need not be signal-based: it is a cheap
   counter check at call boundaries, which Cranelift could insert. Not a
   v0.4 change, but the rejection deserves a risk-register entry.
3. **Linked-by-default ≈ `task.scope` as recommended default (§4.2).**
   Both make the safe thing the easy thing; keep it that way in API naming.
4. **`Selector` vs `select`.** Gleam's selector is a first-class,
   composable value (mergeable, mappable, monitor-aware); Ryo's §4.3
   `select` is a one-shot syntax form. The Gleam shape is strictly more
   expressive for library authors — consider whether `select` should lower
   to a selector-like runtime value. The waker-deregistration machinery is
   identical either way.
5. **Isolation is what makes BEAM simple.** BEAM has no yield-while-locked
   problem because there are no locks. Ryo's `shared[mutex[T]]` choice buys
   Go-style sharing at the cost of the entire Phase 6.2 inferred-effect
   analysis. Deliberate trade — but Gleam/BEAM is the reference for the
   other branch (unbounded mailboxes, message copying, no shared reads).
6. **Backpressure: Ryo is ahead.** BEAM mailboxes are unbounded; `send`
   never blocks, and slow-consumer mailbox growth is a famous production
   failure mode with no built-in answer. Ryo's four channel modes (§4.1)
   are explicit backpressure tools Gleam can only imitate with
   protocol-level acks.
7. **Dirty schedulers validate §3.4/§3.5.** Ryo's ~200 ns coroutine
   handoff is the cheaper mechanism; BEAM pays a full scheduler handoff
   for dirty NIFs.
8. **Mandatory call timeout.** `process.call` forces a timeout and crashes
   the caller on expiry. Ryo's `.await` has no such forcing function
   (`task.timeout` is opt-in, §2.4). For channel-based request/reply,
   consider a loudly-defaulted timeout.
9. **Monitors as messages compose with `select` for free.** A BEAM `Down`
   notification is a mailbox message, so "data arrived OR worker died" is
   one select. Ryo's `select` can await a future (§4.3 example), but a
   *panicked detached* task's only surface is stderr logging (§1.6) — a
   monitor-like signal would close that gap.
10. **The JS-target cautionary tale.** Gleam's JavaScript target has no
    processes at all — the platform doesn't provide them. Evidence for
    Ryo owning its runtime rather than assuming a host supplies one.

One-line summary: Gleam shows the ceiling of the borrowed-runtime path;
Ryo cannot borrow ERTS, so the real takeaways are the API-layer lessons
(typed subjects, composable selectors, linked-by-default spawns, mandatory
call timeouts) and the scheduling warning that cooperative-only dispatch
is the one place Ryo chooses to be weaker than BEAM.

---

## Dependency Summary

| Crate | Purpose | Alternatives considered |
|---|---|---|
| `corosensei` | Stack switching, all platforms; also the system-coroutine primitive | `context`, raw `ucontext` |
| `mio` | I/O polling abstraction | `tokio` (rejected — wrong model) |
| `crossbeam-deque` | Work-stealing queues | Manual Chase-Lev |
| `crossbeam-channel` | Internal runtime messaging | `std::sync::mpsc` |

No dependencies beyond these four: the system coroutine, FFI router,
dispatchers, supervisor scope, and new channel modes are all built on the
existing stack.

---

## What Is Explicitly Out of Scope

| Feature | Reason |
|---|---|
| Generator-style `yield` | Channels cover all use cases more idiomatically |
| WASM target | Stack swapping not available in standard WASM — see WasmFX section |
| Stackless coroutines | Reintroduces function coloring, wrong for Ryo |
| Tokio as scheduler | Stackless-first, conflicts with stack-swapping model |
| `io_uring` direct | Linux only, use `mio` for cross-platform (may become a Linux opt-in feature later) |
| Preemptive scheduling | Cooperative is sufficient for scripting; adds significant complexity |
| User-defined panic handlers | v0.5 concern — log-to-stderr is the v0.4 default |
| True Loom-style stack capture | No production-quality Rust crate; per-ISA assembly + DWARF integration to hand-roll. Deferred — see *Memory profile at scale* |

---

## FFI scenarios

How common C-library workloads behave under this plan:

| Scenario | Behavior |
|---|---|
| `libjpeg.decode(bytes)` (compute, no I/O) | Safe for any internal stack usage up to the configured `RYO_FFI_STACK_SIZE` (2 MB by default) — runs on the system coroutine, ~200 ns overhead. This removes the previous 128 KB stack limit for typical libraries. |
| `simdjson.parse(bytes)` (compute, no I/O) | Same system-coroutine path. |
| `sqlite3.query(db, sql)` (blocking I/O) | `#[blocking]`; ~10 µs dispatch. |
| `openssl.aes_encrypt(key, data)` (compute) | System coroutine; ~200 ns overhead. |
| `libpng.write(file, data)` (compute + libc write) | `#[blocking]` required — the libc write may block. |
| `libfoo.compute()` recursing 4 MB deep | `StackOverflow` at 2 MB → task fails. Configurable via `RYO_FFI_STACK_SIZE`. |
| Callback from C into Ryo | Runs on system-coroutine stack — safe to do work, can `task.spawn` cleanly; nested FFI from the callback routes to an overflow system coroutine, and past `RYO_FFI_OVERFLOW_DEPTH` the inner call fails immediately with an explicit re-entry-limit error (`FfiReentryLimit`, no queueing — see §3.5). |

---

## Memory profile at scale

| Concurrent task count | Stack memory (32 KB+ per task) |
|---|---|
| 10 K tasks | ~320 MB |
| 100 K tasks | ~3.2 GB |
| 1 M tasks | ~32 GB |

The per-task memory floor is corosensei's, not Loom's: true Loom-style heap
capture would bring 1 M concurrent tasks to ~8 GB instead of ~32 GB. For
Ryo's target audience (10K–100K concurrent tasks per process, FFI-heavy
workloads) this is rounding error, so the plan **explicitly defers** true
Loom-style heap-capture as a future optimization. Revisit if:

- 1M+ concurrent tasks become a real Ryo use case.
- A `loom-rs` Rust crate matures, or corosensei gains `freeze`/`thaw` modes.
- On the WASM target: when the WebAssembly stack-switching proposal
  stabilizes, the WASM build can get true Loom semantics natively (the Wasm
  engine handles the capture). Native and WASM may diverge in the runtime;
  that's fine. See the WasmFX section.

---

## Appendix: code example — a realistic workload

HTTP server with config, SQLite (blocking C), libjpeg (compute C):

```ryo
fn handle_request(req: Request, db: shared[SqliteDb]) -> Response:
	user = db.query(req.sql)              # #[blocking] — ~10 µs dispatch
	avatar = libjpeg.decode(user.bytes)  # runs on 2 MB system coroutine — safe
	                                      # ~200 ns overhead
	return Response(user.name, avatar)

fn main():
	db = shared(SqliteDb.open(cfg.db_path))
	http.serve(cfg.port, fn(req): handle_request(req, db))
```

What the runtime buys invisibly (user code stays colorless):

- `libjpeg.decode` is safe for any internal stack usage up to the configured
  `RYO_FFI_STACK_SIZE` (2 MB by default) — this removes the previous 128 KB
  limit for typical libraries.
- `db.query` cost is unchanged.

Adding an explicit DB dispatcher (for connection limiting at the app level).
Only the database operation switches dispatchers; `libjpeg.decode` and the
other CPU work stay on the default dispatcher (and thus on the system
coroutine for FFI):

```ryo
fn handle_request(req: Request, db: shared[SqliteDb], db_dispatcher: Dispatcher) -> Response:
	user = with_dispatcher(db_dispatcher):
		db.query(req.sql)                # only the DB op runs on db_dispatcher —
		                                  # bounded to 16 concurrent queries
	avatar = libjpeg.decode(user.bytes)  # back on the default dispatcher;
	                                      # CPU work never touches db_dispatcher
	return Response(user.name, avatar)

fn main():
	db_dispatcher = dispatcher.custom(workers = 16, name = "db")
	db = shared(SqliteDb.open(cfg.db_path))
	http.serve(cfg.port, fn(req): handle_request(req, db, db_dispatcher))
```

The bound depends on the §4.5 precedence rule: the enclosing
`with_dispatcher(db_dispatcher)` block overrides the `#[blocking]`
auto-routing inside `SqliteDb.query` for the duration of the block — if
`#[blocking]` won instead, the query would escape to the blocking pool and
the 16-concurrent bound would not hold.

---

## Future: WASM Target via WasmFX

> **Not in scope for v0.4.** This section documents the path forward for a
> future WASM backend once the platform matures sufficiently.

### Why WASM Is Deferred

Standard WASM has no accessible execution stack — it is a stack machine at
the bytecode level, but user code cannot swap or inspect stacks. Ryo's
green thread model depends entirely on stack swapping via `corosensei`,
which has no equivalent in standard WASM today. The only alternative —
compiling tasks into stackless state machines — reintroduces function
coloring, which directly contradicts Ryo's core design goal.

### WasmFX — The Right Future Primitive

[WasmFX](http://wasmfx.dev/) (formally: the WebAssembly stack-switching /
typed continuations proposal) is the upcoming WASM feature that would
enable Ryo's concurrency model on WASM without compromising semantics.

**What it provides:**
- First-class typed continuations — snapshots of an execution stack that
  can be suspended and resumed
- A general stack-switching instruction set sufficient to implement green
  threads, async/await, generators, and coroutines at the WASM level
- No whole-program transformation required — unlike CPS or state machine
  approaches

**Current status (early 2026):**
- Phase 3 of the W3C WebAssembly standardisation process
- Active implementation work in V8 and Wasmtime
- Safari status: catching up generally on Wasm but stack-switching timeline
  unclear
- Known as `stack-switching` / WasmFX in the
  [WebAssembly proposals repository](https://github.com/WebAssembly/proposals)

### What a Ryo WASM Backend Would Look Like

The key principle: **Ryo's language semantics do not change. Only the
runtime backend swaps.**

```
v0.4 (native):          v0.x (WASM, future):
corosensei              WasmFX continuations
    +                       +
mio                     WASI 0.3 async I/O
    +                       +
crossbeam-deque         Single-threaded event loop (browser)
                        or WASI threads (server)
```

The runtime abstraction introduced in Phase 1 (`RyoStack`, TLS scheduler
handle) should be designed so that a WASM backend can be dropped in without
touching the scheduler interface.

### WASM Target Variants

| Target | I/O backend | Threading | Notes |
|---|---|---|---|
| Browser | Browser event loop | Single-threaded | No `SharedArrayBuffer` needed |
| WASI server | WASI 0.3 async | WASI threads (if available) | Wasmtime has experimental support |
| Edge functions | WASI 0.3 async | Single-threaded | Cloudflare Workers, Fastly Compute |

### Prerequisites Before Starting

Do not begin WASM work until all of the following are true:

1. WasmFX reaches Phase 4 (standardised) or has stable support in at least
   two major runtimes (V8 + SpiderMonkey, or Wasmtime + V8)
2. Ryo's native runtime (Phases 1–5) is stable and well-tested
3. The `RyoStack` abstraction has been validated as genuinely swappable by
   writing a mock backend for testing purposes first
4. WASI 0.3 is stable (1.0 expected late 2026 / early 2027)

### Risk

The single biggest risk is Safari. If WasmFX support lags significantly,
the browser WASM target may need to fall back to a stackless state machine
compilation mode for Safari only — effectively a per-engine codegen path.
This is significant engineering work and should only be tackled if the
browser target is a stated product priority.

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Windows IOCP + stack unwinding edge cases | Medium | High | CI on Windows from Phase 1, not Phase 5 |
| Stack overflow in recursive user code | Medium | Medium | Adaptive growth + guard pages + `StackOverflow` error delivered to task |
| Work stealing causing cache thrashing | Low | Medium | Profile before tuning; start with random steal |
| `corosensei` platform gap | Low | High | Verified support: x86_64 + aarch64 on all three OSes |
| Cancellation during `select` leaving zombie wakers | Medium | High | Strict ownership of waker registration; cancel deregisters atomically |
| Phase 6.2 effect propagation balloons compile time | Medium | Medium | Measure on real corpus from Phase 6 start; budget under 10% overhead |
| Phase 6.2 dyn-trace error messages turn out unhelpful | Medium | High | Dedicate engineering effort; this is the make-or-break detail for user acceptance |
| Async drop deadline kills useful destructors | Low | Medium | Configurable deadline; default of 5s is generous; logs name the offender |
| Task migration breaks user code that read OS TLS | Medium | Medium | `task_local!` from §3.6 + clear documentation |
| System coroutine stack memory (~2 MB × `RYOMAXPROCS`) | Low | Low | Bounded by worker count; configurable via `RYO_FFI_STACK_SIZE` |
| Nested FFI re-entry from C callbacks deadlocks a worker | Medium | High | Bounded overflow coroutines; exhaustion fails immediately with `FfiReentryLimit` — never queues (§3.5) |
| `dispatcher.custom` from arbitrary code starves the runtime | Medium | Medium | `RYO_MAX_DISPATCHERS` + extra-worker budget with `ResourceExhausted`; capability-gated creation (§4.5) |
| Conflated-channel HB semantics underspecified | Medium | Medium | Rules are draft (data-plane section); formalize in the Ownership-Lite work before Phase 6 sign-off |

---

## Milestone Summary

| Phase | Deliverable | Unlocks |
|---|---|---|
| 1 | Single-threaded green threads, adaptive stacks, panic semantics | Basic `task.run` + `.await` |
| 2 | `mio` I/O, timer wheel | Non-blocking I/O, `task.delay`, `task.timeout` |
| 3 | Work stealing, blocking pool, system-coroutine FFI router, task-local storage, memory model | True M:N parallelism, safe compute + blocking FFI, well-defined visibility |
| 4 | Channels (four modes), `select`, `task.scope`, `task.supervise`, dispatchers, async-drop semantics | Full spec compliance + proposal-only APIs |
| 5 | Hardening, observability (incl. `pool_drained`), Loom + soak testing | Production readiness |
| 6 | `with` enforcement, inferred-effect lock safety (incl. `with_dispatcher`), ARC lint | Static deadlock prevention without coloring |

---

## References

- Spec: [`docs/specification.md`](../specification.md) §9 (Concurrency)
- Historical note: this document began as two drafts — an initial plan and
  the `concurrency_loom_kt.md` Loom/Kotlin alternative. The alternative was
  adopted and merged here.
- Sibling design docs: [`memory_model_comparison.md`](pl_references/memory_model_comparison.md), [`rust.md`](pl_references/rust.md), [`mojo.md`](pl_references/mojo.md), [`arc_optimizer.md`](arc_optimizer.md), [`go.md`](pl_references/go.md) (inspiration), [`proposals/wasm_target.md`](proposals/wasm_target.md)
- Upstream prior art:
  - [JEP 444: Virtual Threads (Java 21 GA)](https://openjdk.org/jeps/444) — Loom (inspiration for the FFI ergonomics goal).
  - [Loom OpenJDK wiki](https://wiki.openjdk.org/display/loom/Main).
  - [Kotlin Coroutines Guide](https://kotlinlang.org/docs/coroutines-guide.html) — source of `with_dispatcher`, `task.supervise`, channel mode patterns.
  - [`kotlinx.coroutines` source](https://github.com/Kotlin/kotlinx.coroutines).
  - [`corosensei`](https://github.com/Amanieu/corosensei) — Rust stack-switching primitive; the foundation of this plan.
  - [`may`](https://github.com/Xudong-Huang/may) — production Rust green-thread runtime (alternative reference).
  - Go's cgo internals — origin of the system-stack switching pattern this plan mirrors via corosensei.
  - [WebAssembly stack-switching proposal](https://github.com/WebAssembly/stack-switching) — future path to true Loom semantics on WASM.
