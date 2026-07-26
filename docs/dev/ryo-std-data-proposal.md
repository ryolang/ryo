# Ryo — Standard Library Data Layer: Databases, Pools & Caches

> **Document Status:** Design Proposal (Pre-Implementation)
> **Last Updated:** 2026-07-22
> **Version:** 1.0.0-draft
> **Fills:** the data-storage gap deliberately left by `std_ext.md` (which covered json/http/regex/time/rand/crypto/fs but no persistence)
> **Interacts with:** D1 (projections), D2 (`bytes`), D4 (unsafe policy), D9 (runtime profiles), GAP-1 (ambient context), base spec §4.11 (FFI), §5.5 (`with`/Drop), t-strings (SQL/HTML safety), the validated MVC flow (`ryo-slicing-and-memory-model-final-spec.md` §2.3)

---

## 1. Placement

```text
std.db            # database-agnostic API surface: connection concepts, errors, row mapping
std.db.sqlite     # bundled SQLite — the batteries-included engine
std.db.postgres   # pure-Ryo wire-protocol driver (later)
std.pool          # generic resource pool (DB connections, cache clients, any expensive resource)
std.cache         # in-process cache (LRU/TTL) — pure library type
std.redis         # remote cache client (RESP protocol) — pure Ryo
```

Profile mapping (D9):

| Component | `core` | `hosted` | Reason |
|---|:---:|:---:|---|
| `std.cache` (in-process LRU) | ✅ | ✅ | Allocator only — a data structure, no I/O |
| `std.db.sqlite` in-memory | ✅ | ✅ | Allocator + VFS shim only |
| `std.db.sqlite` file-backed | ❌ | ✅ | Requires `std.fs` |
| `std.db.postgres`, `std.redis` | ❌ | ✅ | Sockets |
| `std.pool` | ✅ | ✅ | Synchronization only (async-aware waiting is hosted) |

---

## 2. Milestones

| Item | Milestone | Dependency chain |
|------|-----------|------------------|
| `sqlite-sys` raw binding | **v0.2** | FFI + `ryo-bindgen` (already the spec's running FFI example) + D4 unsafe policy |
| `std.db.sqlite` safe wrapper | **v0.2–v0.3** | The binding + t-strings (parameter binding) + error unions |
| `std.db` common API | **v0.3** | User generics for `query_one(T, ...)` row mapping; manual `from_row` impls until `comptime` enables derives |
| `std.pool` basic (mutex free-list) | **v0.3** | Generic `Pool[T]` with factory closure requires v0.3 generics |
| `std.pool` async-aware (channel-based) | **v0.4** | Waiting for a free resource on a green thread requires the scheduler; deadline-aware `acquire` requires GAP-1 context |
| `std.cache` (LRU/TTL) | **v0.3** | `map` + `std.time`; pure library |
| `std.redis` | **v0.3–v0.4** | RESP over `bytes`; pure Ryo |
| `std.db.postgres` | **v0.4+** | Pure-Ryo wire driver; flagship protocol-parsing proof; needs the scheduler for real workloads |

**Ordering logic:** v0.2 makes SQLite work, v0.3 makes it generic and ergonomic, v0.4 makes it concurrent and production-shaped. Each stage is usable on its own; none waits on ORM ambitions.

---

## 3. SQLite as Standard

### 3.1 Why first, why bundled

1. **Audience parity.** Python ships `sqlite3` in its stdlib; Ryo's audience expects an embedded database with zero setup. For CLI tools — a stated target domain — SQLite is frequently the entire storage layer.
2. **Zero-dependency bundling is nearly free.** The toolchain already ships a C compiler (the Zig linker bundles clang). Compiling the SQLite amalgamation at toolchain-build time means `import std.db.sqlite` works with no system package manager and no `libsqlite3` version lottery.
3. **Reference implementation of the safe-wrapper pattern.** Opaque handle + `Drop` + D4 SAFETY comments + structured error mapping — the pattern `tensor.md` and `unsafe.md` fumbled in details (see `ryo-proposal-review-issues.md`, T-1/T-2/U-3/U-4). Shipping it in std sets the correct example by authority, including the review memo's G-1 requirement (field-level visibility for the private handle).
4. **T-string SQL is the differentiator** (§3.3).

### 3.2 Explicitly not wrapping Rust crates

The `std_ext` wrapping strategy does **not** apply here: `sqlx` drags in Tokio (correctly banned before v0.4 by `std_ext.md`'s own gating note), and `rusqlite` adds a Rust shim where direct amalgamation FFI is cleaner. Crate wrapping is reserved for *hard* problems (TLS, HTTP semantics); a stable C ABI is what `ryo-bindgen` exists for.

### 3.3 The canonical API

```ryo
import std.db
import std.db.sqlite

fn fetch_user(db: sqlite.Db, id: int) -> (db.Error | db.NotFound)!User:
	# t-string → Template → values bound as parameters, never concatenated.
	# SQL injection is impossible by construction (base spec §2).
	user = try db.query_one(User, t"SELECT id, name, email FROM users WHERE id = {id}")
	return user orelse return db.NotFound(f"user {id}")
```

- **Rows map to owned structs** (`User` with owned `str` fields) — the MVC validation's finding: DB results are owned in every language (wire buffers are reused); Ownership Lite costs nothing here.
- **Errors are typed and matchable**: `db.Error | db.NotFound`, carrying location per §4.10. No integer codes crossing the API boundary.
- **Scanning is zero-copy internally:** the driver reads SQLite's result buffers via projections (D1) and copies out only the field values it materializes — short strings hit SSO (§5.9 #7).

---

## 4. `std.pool` — One Design, Three Tenants

Pools look trivial and are not. Two tiers, by milestone:

### 4.1 Basic pool (v0.3)

Mutex-guarded free-list; `Pool[T]` with a factory closure:

```ryo
pool = shared(pool.new(8, fn(): sqlite.Db.open("app.db")))

fn fetch_user(pool: Pool[sqlite.Db], id: int) -> (db.Error | db.NotFound)!User:
	with pool.acquire() as conn:          # Drop returns conn — even on panic (§5.5)
		return query(conn, id)
```

Sufficient for SQLite single-writer workloads and CLI tools. `acquire` blocks a thread when exhausted — acceptable at this tier.

### 4.2 Async-aware pool (v0.4)

Channel-based; correctness by construction:

- `acquire` **waits on the scheduler** instead of blocking an OS thread when the pool is exhausted.
- **Deadline-aware:** `acquire` respects `ctx.deadline()` (GAP-1) — a request-scoped `with deadline(500ms):` cancels a waiting acquire with `DeadlineExceeded`, matchable like any error. This is where the context design pays rent.
- **Leak-proof:** `Drop` returns the connection on every exit path, including panic — pool leaks are structurally impossible.
- **Health:** idle reaping and liveness checks ride the pool's own green thread.

Tenants: `std.db.postgres` connections, `std.redis` connections, and user-defined expensive resources (model handles, sandboxed interpreters) — one abstraction amortized across the whole data layer.

---

## 5. Cache — Two Meanings, Two Homes

Do not conflate them:

| | In-process (`std.cache`) | Remote (`std.redis`) |
|---|---|---|
| What | LRU/TTL map — a pure data structure | RESP client over TCP |
| Profile | `core` + `hosted` | `hosted` only |
| Milestone | v0.3 | v0.3–v0.4 |
| Cross-task access | `shared[mutex[Cache]]`, or a built-in synchronized variant | Via `std.pool` (§4) |
| Covers | The 90% case: memoization, hot lookup tables | Shared state across processes/machines |

`std.redis` is deliberately a **pure-Ryo implementation**: RESP is a trivial wire protocol, making the client a ~1k-line *demonstration* of D2 `bytes`, D1 projections (zero-copy reply parsing), error unions, and the pool — ecosystem proof, not risk. Valkey/Redis wire-compatible.

---

## 6. Risks & Notes

| Risk / note | Disposition |
|-------------|-------------|
| DB-agnostic `std.db` abstraction without `dyn Trait` (pre-v0.3) | Static dispatch only: drivers are used concretely (`sqlite.Db`, `postgres.Db`) with shared *concepts* and error types; no runtime polymorphism until `dyn` lands — and possibly never needed |
| Row-mapping boilerplate before `comptime` | Manual `from_row` impls (AI-writes-human-reviews absorbs this); `#[derive(FromRow)]` with v0.3 |
| SQLite write contention under green threads | Document single-writer discipline; the async pool serializes writers naturally; WAL mode recommended in docs |
| ORM pressure from the community | Out of scope for std. Third-party, post-`comptime`. Std ships honest drivers + t-string safety; magic stays in packages |
| `spawn_detached` + pooled connections | A connection must never escape its `with pool.acquire()` scope — Drop guarantees return; detached tasks acquire their own (context rule 3, linked-not-parented, applies to tracing of these acquisitions) |

---

## 7. Open Questions

- **Q1 — Transactions:** `with db.transaction() as tx:` (Drop = rollback, explicit `tx.commit()`) is the RAII-native shape; confirm over the more common `tx.begin()/commit()` spelling. Recommendation: the `with` form — rollback-by-default is the safe default.
- **Q2 — Migrations:** std or toolchain? Recommendation: toolchain (`ryo db migrate`), plain-SQL files, not a framework.
- **Q3 — `std.db.mysql`:** demand-driven; the wire protocol is messier than Postgres'. Defer until `std.db.postgres` proves the driver pattern.
- **Q4 — Cache stampede protection** (request coalescing / single-flight): built into `std.cache` or a separate `std.cache.singleflight`? Recommendation: separate module; ride GAP-1's task tree.

---

*End of Proposal*
