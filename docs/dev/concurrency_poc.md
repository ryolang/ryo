**Status:** Complete — all three platforms (macOS, Linux, Windows) pass all
three gate criteria.

# Concurrency PoC — Results

Results of the pre-Phase-1 proof-of-concept gate defined in
[`concurrency.md`](concurrency.md) (*Gate: proof-of-concept spike before
Phase 1*). PoC code is throwaway and uncommitted (scratch crate under
`local/concurrency-poc/`, gitignored); this file is the only committed
artifact.

**Dependencies:** corosensei 0.3.4, mio 1.2.3 (net, os-ext), libc 0.2 (unix-only).
Go reference programs (`go_reference/`): Go 1.27.1.

**Machine (macOS row):** Apple Silicon (arm64), macOS, release build.
**Machine (Linux rows):** `rust:latest` Docker container (aarch64) on the
same host, release build — kernel/epoll behavior is genuinely Linux; CPU is
shared with the host. One porting fix was needed: `siginfo_t.si_addr` is a
method on Linux, a field on macOS.
**Machine (Windows rows):** Windows 11 24H2 ARM64 Pro VM (UTM/QEMU, 4 GB
RAM) on the same host. corosensei 0.3.4 has no aarch64-windows backend, so
the binary was cross-built on macOS and run under Windows-on-ARM x64
emulation (the in-guest MSYS2 GNU toolchain silently no-ops under x64
emulation). Cross-build recipe:

```sh
rustup target add x86_64-pc-windows-gnu
brew install mingw-w64
# .cargo/config.toml: [target.x86_64-pc-windows-gnu] linker = "x86_64-w64-mingw32-gcc"
cargo build --release --target x86_64-pc-windows-gnu
```

Switch-cost numbers are therefore
emulation-inflated — read them as "works and is fast enough under
emulation", not native performance. The msvc build remains untried (needs
VS Build Tools).

## Results

| Criterion | Platform | Result | Verdict |
|---|---|---|---|
| Task-switch cost (≤ 1 µs mean) | macOS | bare: 19–22 ns/switch (multi-task), 1.9–4.1 ns (ping-pong); deep call chain: 28–37 ns (multi-task), 15–19 ns (ping-pong) | **Pass** (~30× under the bar even at realistic call depth) |
| Task-switch cost | Linux (aarch64, Docker) | bare: 9.2 ns/switch (multi-task), 1.8 ns (ping-pong); deep: 33.4 ns (multi-task), 16.4 ns (ping-pong) | **Pass** |
| Task-switch cost | Windows (x64 emu on ARM64) | bare: 69.7 ns/switch (multi-task), 6.6 ns (ping-pong); deep: 86.5 ns (multi-task), 23.1 ns (ping-pong) — emulation-inflated | **Pass** (~10× under the bar even under emulation) |
| Stack growth (10 000 tasks past 32 KB, zero corruption) | macOS | 10 000/10 000 tasks complete; every stack grew (~3.4 grow events each); overflow run: `STACK_OVERFLOW_DETECTED`, exit 42 | **Pass** |
| Stack growth | Linux (aarch64, Docker) | 10 000/10 000 tasks complete; all grew (31 806 grow events); committed 120–128 KB; overflow run: `STACK_OVERFLOW_DETECTED`, exit 42 | **Pass** |
| Stack growth | Windows (x64 emu on ARM64) | 10 000/10 000 tasks complete; all grew (kernel-driven page-granular growth — finding 6); committed min=max=128 KB; overflow run: `STACK_OVERFLOW_DETECTED`, exit 42 (VEH + hard-guard path) | **Pass** |
| IOCP integration (1 000 conns, zero lost/dup events) | macOS (kqueue control) | 1 000 conns × 4 msgs: 4 000/4 000 echoed and verified, 103 ms | **Pass** (control) |
| IOCP integration | Linux (epoll control) | 4 000/4 000 echoed and verified, 1 513 ms | **Pass** (control) |
| IOCP integration | Windows (IOCP) | 4 000/4 000 echoed and verified (mio IOCP backend), ~1.6 s | **Pass** |

## Bonus probes

- **Context TLS re-pointing** (§3.6 ambient-context mechanism): 64 tasks ×
  1 000 rounds, zero identity mismatches across yields — pattern works.
- **Backtrace sanity** (risk-register: debugging across task stacks):
  `std::backtrace` works on corosensei stacks; frames visibly cross the
  switch trampoline. On Windows, win64 unwinding works across task stacks
  with full symbolication (31 frames before suspend, 31 after resume) —
  this de-risks the Windows side of that risk-register row. gdb/perf
  behavior still untested.

## Go-comparison measurements (post-gate)

Same caveats as the gate results, plus: the Go numbers come from
`go_reference/` (`GOMAXPROCS(1)` for the ping-pong, so both sides run on one
OS thread). The PoC scheduler is a toy FIFO with `RefCell` shared state;
Go's number pays for production channel + scheduler machinery. Read these as
order-of-magnitude headroom checks, not benchmarks of record.

### Channel-style ping-pong (ns/switch, through the scheduler)

| Platform | PoC `pingpong` (pairs=1) | PoC (pairs=1000) | Go (pairs=1) | Go (pairs=1000) |
|---|---|---|---|---|
| macOS | 7.3–16.4 | 16.9–20.7 | 200.4 | 169.7 |
| Linux (aarch64, Docker) | 10.9 | 17.5 | 180.3 | 183.3 |
| Windows (x64 emu on ARM64) | 21.0 | — | — | — |

(PoC intermediate pairs: 9.2–13.6 ns at pairs=10, 10.3–12.5 ns at pairs=100
on macOS; 10.3 / 13.5 ns on Linux. macOS spread is thermal/frequency
variance across runs.)

### Per-task memory (parked tasks, each touched ~4 KB of stack)

| Platform | PoC RSS/task (10k / 100k) | PoC VMEM/task | Go RSS/goroutine (10k / 100k) |
|---|---|---|---|
| macOS | 16.5 / 16.5 KB | 384 KB | 3.3 / 2.8 KB |
| Linux (aarch64, Docker) | 11.7 / 13.9 KB | 384 KB | 2.9 / 2.8 KB |
| Windows (x64 emu on ARM64) | 8.4 / 8.3 KB | ~62 KB commit/task (PagefileUsage) | — |

PoC RSS/task is page-granular: one 16 KB page (macOS) / ~3 × 4 KB pages
(Linux) per task; Windows is lowest of the three (4 KB pages +
page-granular kernel growth vs the macOS 16 KB page floor). On unix the
VMEM/task is exactly the 384 KB mapping (128 KB reservation + 256 KB guard
gap); on Windows the second column is private commit, not VA. Go parks
goroutines on GC-shrunk 2 KB stacks pooled in runtime arenas (StackSys ≈
2 KB/goroutine at 1M).

### Concurrent-task ceiling (parked tasks)

| Platform | PoC ceiling | Limiting resource | Go ceiling |
|---|---|---|---|
| macOS | 1 000 000 (no ceiling hit) | none found (no `max_map_count`; virtual space and RAM only) | 1 000 000 OK |
| Linux (aarch64, Docker) | **131 059** | 2 VMAs per stack mapping vs `vm.max_map_count` = 262 144 (Docker Desktop VM; the common 65 530 distro default would cap at ~32k tasks) | 1 000 000 OK |
| Windows (x64 emu on ARM64) | **160 391** | commit charge (RAM + pagefile; 4 GB VM) — pagefile-tunable, not a kernel VMA cap | — |

The Linux ceiling is a design trade-off, not a bug: per-task mmap + guard
gap buys sound overflow detection at the cost of a VMA ceiling (finding 5
below, fed back into §1.1). The three ceilings differ in kind: Linux is a
kernel VMA-count parameter (raise `vm.max_map_count` or move to a pooled
arena), Windows is commit charge (pagefile-tunable), macOS showed none at
1M.

## Findings that change the runtime design (fed back into §1.1)

1. **A plain `PROT_NONE` reservation does not reliably catch overflow on
   macOS.** Adjacent writable mappings can sit immediately below the
   reservation; an overflowing stack silently trampled ~197 KB of neighbor
   memory before faulting. Fix: a permanent 256 KB guard gap *inside* our own
   mapping below the usable region; a fault in the gap is the overflow event.
   Without this, overflow detection is unsound.
2. **Chunk alignment can over-commit past the reservation.** Commit
   alignment must clamp at the reservation limit.
3. **`sigaltstack` + `SA_ONSTACK` is mandatory, not optional** — at
   grow/overflow time the coroutine stack is exhausted, so the fault handler
   cannot run on it.
4. **Bare-vs-deep switch cost gap (~2×) confirms the return-address-predictor
   effect** noted in the WAW 2025 WasmFX retrofit: bare microbenchmarks
   understate real switch cost by roughly half. The deep variant is the
   meaningful number — and it still passes by a wide margin.
5. **Per-task mmap caps concurrent tasks on Linux.** Each stack mapping is 2
   VMAs (guard gap + committed region), so `vm.max_map_count` bounds live
   tasks: measured 131 059 at the Docker Desktop default (262 144); the
   common 65 530 distro default caps at ~32k. macOS has no such limit (1M
   parked tasks OK). If high task counts matter, a pooled stack arena (one
   reservation carved into slots, 1 VMA total) is the escape hatch — sound
   overflow detection is kept, VMA cost is paid once.
6. **On Windows the kernel owns stack growth.** corosensei loads the
   stack's `teb_fields()` into the real TEB on every switch, so guard-page
   hits inside our mapping are handled by the kernel's automatic
   thread-stack growth before any VEH runs — a VEH that commits chunks
   itself would either never fire or require lying in the TEB (which breaks
   SEH unwinding of panics). Consequences: growth is page-granular (no 32 KB
   chunks), grow events are only observable via `update_teb_fields`
   read-backs, and the VEH's job shrinks to the terminal
   `STATUS_STACK_OVERFLOW` → marker + `ExitProcess(42)`. No 256 KB guard gap
   is needed: a `MEM_RESERVE` boundary cannot be claimed by a neighbor
   mapping, so the macOS trample class does not exist on Windows.

## Notes

- corosensei 0.3.4 API as expected; custom `Stack` trait is
  `base()`/`limit()` pointers, limit must include guard pages, base
  16-aligned. Dropping a live suspended coroutine force-unwinds it — safe
  for task teardown.
- The `stack` scenario's recursion commits the full 128 KB reservation at
  depth 96 (frames ~1.1 KB in release) — zero headroom by construction;
  fine for the gate, not a model for the real size-class progression.
- Windows results were obtained under x64 emulation on an ARM64 VM (see the
  machine note for the cross-build recipe); native ARM64 runs await an
  aarch64-windows backend in corosensei. The msvc target build remains
  untried. A Windows CI job is the follow-up, which also exercises the
  Phase-1-onward Windows CI commitment early.

## References

- Plan: [`concurrency.md`](concurrency.md) — gate criteria, §1.1 stack
  strategy, §3.6 context machinery
- Spec: [§9.2](../specification.md#92-core-primitives-and-safety) (concurrency semantics the runtime must deliver)
