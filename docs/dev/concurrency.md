# Ryo v0.4 Concurrency Implementation Plan
> Task / Future / Channel — Green Thread M:N Runtime

---

## Overview

This document describes the phased implementation plan for Ryo's concurrency
model. The goal is a working, cross-platform (Linux, macOS, Windows) M:N green
thread runtime backing `task.run`, `future[T]`, and channels by the v0.4
release.

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
| Blocking thread pool cap | `10000` | Mirrors Go's default M cap. Hard ceiling on concurrent `#[blocking]` calls. |
| Initial task stack | `32 KB` | Grows on demand. See §1.1. |
| Maximum task stack | `128 KB` (v0.4) | Hard cap; overflow delivers `StackOverflow` to the task. |
| Timer wheel resolution | `1 ms` | Sufficient for scripting workloads. |

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

### 1.2 Task and Future Primitives

- Define `Task<T>` as an owned handle to a green thread
- Define `Future<T>` as the user-facing return value of `task.run`
- Implement `Drop` on `Future<T>` to request cancellation
- States: `Pending | Running | Completed(T) | Cancelled | Panicked(Reason)`

```
Future<T>:
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
> Phase 3.5 introduces a `task_local!` macro that survives migration.

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
- Inside `task.scope` (Phase 4.2), a panicking child cancels its siblings and
  propagates the panic out of the scope

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
  minimal single-thread fallback pool until Phase 3.4 builds the real one.
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
work-stealing.

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

### 3.4 `#[blocking]` Thread Pool

- Separate thread pool for blocking FFI calls
- When a `#[blocking]` function is called from a green thread:
  1. Move the blocking call to the thread pool
  2. Suspend the green thread
  3. Resume when the thread pool call completes
- **Pool cap: 10 000 threads (matches Go's default M cap).** Starts small,
  grows on demand, shrinks idle threads after a timeout
- This replaces the minimal fallback pool from Phase 2.2

### 3.5 Task-Local Storage

The OS-TLS caveat from §1.4 becomes load-bearing once tasks migrate. Provide:

```
task_local! { static REQUEST_ID: Cell<u64> = Cell::new(0); }
```

Implementation: each task owns a small map of task-local slots. Reads and
writes go through the current task's map, not OS TLS. Survives migration.
Inherited by child tasks spawned within the same `task.scope` (Phase 4.2)
unless explicitly overridden.

### 3.6 Memory Model

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

A separate `concurrency-memory-model.md` document will spell this out
precisely. This phase only commits to the high-level shape.

### 3.7 Test Strategy

- CPU-bound benchmark: 10 000 tasks doing pure computation, scaling near
  linearly with `RYOMAXPROCS`
- Steal stress test: deliberately imbalanced producer/consumer pattern,
  verify all cores stay busy
- Migration correctness: task that yields and resumes verifies its
  task-locals survived
- `#[blocking]` saturation: 1 000 simultaneous blocking calls do not stall
  the scheduler

**Exit criteria:** Linear scaling for embarrassingly parallel workloads.
`task_local!` works across migration. Blocking pool does not stall green
threads.

---

## Phase 4 — Channels and High-Level Primitives

**Goal:** Implement all concurrency primitives described in the spec.

### 4.1 Channels

```
std.channel.create[T]() -> (sender[T], receiver[T])
std.channel.bounded[T](capacity) -> (sender[T], receiver[T])
```

- **MPMC by default.** `sender[T]` and `receiver[T]` are reference-counted
  handles; `clone()` produces another handle to the same end. The channel
  closes on that side when the **last** clone is dropped.
- Bounded and unbounded variants
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
- `VecDeque<T>` as the ring buffer
- Intrusive wait lists for suspended senders and receivers
- No `Mutex` held during task resumption — only during queue manipulation

### 4.2 `task.scope` (Structured Concurrency)

- All tasks spawned inside a scope must complete before the scope exits
- If any task panics or the scope exits early: cancel all remaining tasks
  and await their unwind (the scope blocks until all children are settled)
- Scope holds a `JoinHandle` list; on exit, awaits all of them
- **This is the recommended default over `task.spawn_detached`**

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

### 4.5 Cancellation Sources

| Source | Mechanism | Sync or async? |
|---|---|---|
| `drop(future)` | Sets cancel flag, wakes task | Sync request, async observation |
| `task.scope` exit | Cancels all child futures, awaits unwind | Async — scope blocks until done |
| `select` losing case | Cancels non-winning operations | Async — `select` blocks until cancelled siblings settle |
| `task.timeout` expiry | Delivers `Timeout` at next suspension | Async |
| `fut.cancel()` | Explicit cancel call | **Async** — returns a future that resolves when the cancelled task has fully unwound. `fut.cancel_now()` is the fire-and-forget variant for cases where the caller does not need to await unwind. |

### 4.6 Async Drop / Destructor-Yield Semantics

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

### 4.7 Test Strategy

- All §9.4 spec examples pass
- `task.scope` leak test: child task that holds a file handle, parent panics,
  assert handle is closed before scope exits
- `select` race test: 1 000 iterations of contended select, no zombie wakers
- Channel ownership test: clone senders, drop them in arbitrary order, verify
  close happens exactly when last clone drops
- Async drop test: destructor that yields completes before scope exit;
  destructor that exceeds `unwind_deadline` logs and force-terminates

**Exit criteria:** All spec examples in §9.4 run correctly. `task.scope`
prevents resource leaks under cancellation. `select` with `default` is
non-blocking. Channel ownership is correct under reference counting.

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
- Foundation for a future `ryo profile` tool

### 5.6 Test Strategy

- Loom-style permutation testing of the scheduler core (channels, select,
  cancellation) — catches scheduler-order-dependent races
- 24-hour soak test on each platform: random workload, no leaks, no panics,
  no deadlocks
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
   timer ops, bounded-channel `send` when buffer is full.
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
statically non-yielding. Bounded `sender[T].send` is `[yields]`. The user
chooses the right tool for the location.

#### 6.2.6 Error message requirements

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

### 6.4 Test Strategy

- Snapshot-test the inferred-effect propagation on a curated corpus of
  real Ryo programs (catches regressions in the analysis)
- Snapshot-test error messages (especially the dyn impl trace — small
  output changes are easy to break)
- Compile-time benchmark: measure the cost of effect propagation on a
  representative large program; track over time
- Negative tests: every documented "this should be rejected" case has a
  test that confirms rejection with the right error

**Exit criteria:** All §9.x spec examples involving locks pass. Compile-time
overhead from the analysis is under 10% on the benchmark corpus. Error
messages include the impl trace.

---

## Dependency Summary

| Crate | Purpose | Alternatives considered |
|---|---|---|
| `corosensei` | Stack switching, all platforms | `context`, raw `ucontext` |
| `mio` | I/O polling abstraction | `tokio` (rejected — wrong model) |
| `crossbeam-deque` | Work-stealing queues | Manual Chase-Lev |
| `crossbeam-channel` | Internal runtime messaging | `std::sync::mpsc` |

---

## What Is Explicitly Out of Scope

| Feature | Reason |
|---|---|
| Generator-style `yield` | Channels cover all use cases more idiomatically |
| WASM target | Stack swapping not available in standard WASM — see WasmFX section |
| Stackless coroutines | Reintroduces function coloring, wrong for Ryo |
| Tokio as scheduler | Stackless-first, conflicts with stack-swapping model |
| `io_uring` direct | Linux only, use `mio` for cross-platform |
| Preemptive scheduling | Cooperative is sufficient for scripting; adds significant complexity |
| User-defined panic handlers | v0.5 concern — log-to-stderr is the v0.4 default |

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
| Task migration breaks user code that read OS TLS | Medium | Medium | `task_local!` from Phase 3.5 + clear documentation |

---

## Milestone Summary

| Phase | Deliverable | Unlocks |
|---|---|---|
| 1 | Single-threaded green threads, adaptive stacks, panic semantics | Basic `task.run` + `.await` |
| 2 | `mio` I/O, timer wheel | Non-blocking I/O, `task.delay`, `task.timeout` |
| 3 | Work stealing, blocking pool, task-local storage, memory model | True M:N parallelism, safe FFI, well-defined visibility |
| 4 | Channels, `select`, `task.scope`, async-drop semantics | Full spec compliance |
| 5 | Hardening, observability, Loom + soak testing | Production readiness |
| 6 | `with` enforcement, inferred-effect lock safety, ARC lint | Static deadlock prevention without coloring |
