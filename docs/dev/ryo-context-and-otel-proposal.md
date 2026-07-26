# Ryo — Ambient Context Propagation & std.otel

> **Document Status:** Design Proposal (Pre-Implementation)
> **Last Updated:** 2026-07-22
> **Version:** 1.0.0-draft
> **Resolves:** GAP-1 (`ryo-missing-features-and-gaps.md` §2)
> **Interacts with:** D5 (scoped task borrows), D9 (runtime profiles), base spec §9.1 (ambient runtime), §9.2.1 (structured concurrency), §5.5 (`with` blocks), §4.10 (error capture)

---

## 1. Motivation

Every production service needs request-scoped context: **deadlines, cancellation, trace identity**. The industry's record is uniform and unflattering:

- **Go** bolted `context.Context` on years after launch. It requires threading a parameter through every signature, is invisible to the type system in all the ways that matter, and remains the top ergonomics complaint in Go services.
- **Tokio** reinvented it as task-locals; **Python** as contextvars. Every async ecosystem reinvents it because it is mandatory.

Ryo is positioned to do what none of them could: the ambient runtime already lives in **TLS** (§9.1), and structured concurrency already defines a **lexical tree of work** (§9.2.1, extended by D5). The context tree and the task tree are the same tree. Propagation can flow through the structure the scheduler already maintains — no parameter threading, no function coloring.

And the flagship consumer is observability: **OpenTelemetry as a batteries-included standard** turns three existing Ryo investments (TLS runtime, task tree, always-on error capture) into a story no competitor can tell: *production tracing as a `with` block, not a framework.*

---

## 2. Part One — Ambient Context

### 2.1 Model

A **context** is scope-bound runtime state carried in the ambient runtime's TLS. It is bound lexically with the `with` statement and propagates **down** the task tree:

```ryo
fn handle_request(req: http.Request) -> !http.Response:
    with deadline(500ms):                    # lexical bound — reviewer sees the extent
        user = try fetch_user(pool, id)      # cancelled automatically if deadline fires
        task.scope:
            task.run fn(): enrich(user)      # inherits context — child of this scope
        return render(user)
```

Rules:

1. **Lexical binding.** `with deadline(d):`, `with cancellation():`, and `with trace_context(tc):` push a context frame; `Drop` pops it — always, including on `try`-propagation and panic. Same machinery as §5.5 resource management.
2. **Inheritance.** `task.scope` children and `task.run` closures **inherit** the current context frame (a reference; no copying of user data). Cancellation of a parent cancels the subtree — the scope join (D5) already guarantees no child outlives the frame it borrowed from.
3. **`spawn_detached` is linked, not parented.** Detached work may outlive the request; pretending otherwise is a lie that corrupts both cancellation and traces. Detached tasks capture the current trace identity as a **link** but hold no cancellation ownership. (Explicit, recorded here as the one non-obvious rule; reviewers should not have to guess it.)
4. **No generic value bag.** Go's `context.Value` — a stringly-typed, allocation-happy grab-bag — is rejected. The context carries **named, typed fields**: `deadline`, `cancelled`, `trace_context`, `baggage` (W3C string pairs for cross-service propagation). Domain data (auth identity, tenant) travels by ordinary parameters or, where truly ambient, a typed extension point deferred to `comptime` (Q1).
5. **Interrogation.** `ctx.deadline()`, `ctx.cancelled()` (pollable, and usable in `select` as a channel arm: `ctx.done()`), `ctx.trace_id()`. `ctx` is an ambient accessor, not a parameter — like the runtime itself.
6. **Blocking integration.** `#[blocking]` FFI calls and stdlib I/O check cancellation at suspension points; a fired deadline unwinds with a dedicated `DeadlineExceeded` error in the propagated union — visible in `match`, not swallowed.

### 2.2 What this replaces

| Pattern elsewhere | In Ryo |
|---|---|
| `ctx context.Context` in every signature | Nothing — ambient, inherited |
| `ctx, cancel := context.WithTimeout(...); defer cancel()` | `with deadline(500ms):` |
| `select { case <-ctx.Done(): }` | `select` arm on `ctx.done()` |
| `context.WithValue(ctx, key, val)` | Rejected (rule 4); typed fields + parameters |
| Goroutine leaks after request end | Scope join already forbids (D5); cancellation reaches the whole subtree |

### 2.3 Interaction with existing decisions

- **D5 (scoped borrows):** the freeze tree and the context tree are one structure — one implementation, two views. Context propagation adds no new analysis; it rides the scope join.
- **D9 (profiles):** context machinery is **hosted-only**. `core` profile compiles `with deadline(...)` to a compile error pointing at `--profile=hosted` (same pattern as `task.*`, §8.2 rule 2).
- **`with` statement:** context frames are ordinary RAII resources; no new syntax class introduced.

---

## 3. Part Two — `std.otel`

### 3.1 Position

OpenTelemetry is the CNCF industry standard for traces, metrics, and logs; no major language ships it in its standard library (Go, Rust, Java keep it external). Ryo includes it — **`std.otel`** — because observability is not optional in the primary target domain, and because runtime integration lets Ryo offer what external libraries cannot: **automatic, correct propagation across the task tree.**

### 3.2 The canonical idiom

Spans are lexically-scoped resources — the `with` statement's home turf:

```ryo
import std.otel

fn handle_request(req: http.Request) -> !http.Response:
    with otel.span("handle_request") as span:      # Drop ends the span — always,
        span.set_attr("user.id", user_id)           # including error paths and panic
        user = try fetch_user(pool, id)             # error auto-recorded as span event
        task.scope:
            task.run fn(): enrich(user)             # child span — no context parameter
        return render(user)
```

Compare Go: `ctx, span := tracer.Start(ctx, ...); defer span.End()` — plus manual context threading through every callee. The Ryo form is shorter, cannot leak a span, shows its extent lexically, and propagates across green threads for free.

### 3.3 The three integrations that make it Ryo-native

1. **Task-tree spans.** Span context is a context field (§2.1); `task.scope`/`task.run` children are automatically child spans. `spawn_detached` creates a span **link** (rule 3 above) — honest topology in the trace backend.
2. **Error capture → span events.** The §4.10 machinery already collects stack traces at error creation — the 5–10% DX tax. Wire it in: every propagated error becomes a span event with its stack trace; every panic sets span status to error. **The overhead that makes Ryo slower than Go becomes the instrumentation that makes Ryo services more observable than Go services, with zero user code.**
3. **std.http auto-instrumentation.** Since std owns both HTTP (per `std_ext.md`) and OTel, W3C `traceparent` extraction on inbound requests and injection on outbound calls happen invisibly. Cross-service traces work out of the box.

### 3.4 Cost discipline (D9-aligned)

- **Pay-per-import:** no `import std.otel` → nothing linked, nothing checked (same rule as TLS in `std_ext.md`).
- **Noop by default:** with no exporter configured, span operations are no-ops (one branch per span); the SDK initializes on explicit `otel.init_otlp(endpoint)`.
- **Hosted-only:** meaningless on `core`; a noop stub at most.
- **Std surface is OTel-native and minimal** — tracer/span/meter vocabulary directly (the standard's own words; no translation layer to rot). Std ships the **OTLP exporter only**; vendor-specific exporters are external packages.

### 3.5 Metrics and logs (sequenced)

- **Traces first** (v0.4): the differentiation lives here.
- **Metrics** (v0.5): `meter.counter(...)`, histograms; OTLP.
- **Logs:** `std.log` structured facade (slog-style levels + key-values) with an OTel bridge so log records carry the ambient trace ID — correlation by construction, not convention.

---

## 4. Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Std bloat vs. D9's pay-per-profile principle | Pay-per-import + noop default + hosted-only (§3.4) |
| Tying std stability to an evolving external spec | Minimal OTel-native surface; only the OTLP exporter in std; vendor exporters external (§3.4) |
| Shipping OTel before the context foundation | One design (this document); context lands **with** the v0.4 scheduler, never after (§2) |
| Hidden-context magic violating the reviewer test | Context *binding* is lexical and visible (`with deadline(...)`); only *propagation* is ambient — and propagation is the part that was pure boilerplate in Go |
| `spawn_detached` context ambiguity | Explicit rule 3 (§2.1): linked, not parented |

---

## 5. Milestones

| Item | Milestone | Dependencies |
|------|-----------|--------------|
| Ambient context (binding, inheritance, cancellation, `ctx` accessors) | **v0.4** | Lands with the concurrency runtime — non-negotiable sequencing |
| `std.otel` traces + `std.http` auto-instrumentation | **v0.4–v0.5** | Ambient context |
| Error-capture → span-event integration | v0.5 | §4.10 capture machinery |
| Metrics; `std.log` + OTel bridge | v0.5 | Traces |

---

## 6. Open Questions

- **Q1 — Typed context extensions:** domain-ambient data (auth identity, tenant) — parameters only, or a `comptime`-generated typed context field? Recommendation: parameters until `comptime` proves a typed extension ergonomic.
- **Q2 — Cancellation granularity:** may a task install a *shield* (`with shield():`) against parent cancellation (needed for cleanup paths, e.g., flushing a buffer)? Recommendation: yes, explicit and lexical — but specify at v0.4 design freeze.
- **Q3 — Naming:** `std.otel` (honest, searchable) vs. `std.observe` (backend-neutral). Recommendation: `std.otel` — the vocabulary is the standard's either way.
- **Q4 — Sampling defaults:** parent-based, head-based per OTel defaults; confirm no Ryo-specific deviation is needed.

---

*End of Proposal*
