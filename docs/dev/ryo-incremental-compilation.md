**Status:** Design (v0.2+)

# Ryo — Incremental Compilation: Design Note and Roadmap (Draft — v0.2+)

> **Document Status:** Design Note (Pre-Implementation)
> **Last Updated:** 2026-10-14
> **Version:** 1.1.0-draft
> **Interacts with:** [Pipeline migration](pipeline_migration.md), [compiler instructions](ryo-compiler-llm-instructions.md), [concurrency design](concurrency.md), and [implementation roadmap](implementation_roadmap.md).
> **Companion philosophy:** Zig compiler (lazy, demand-driven); rustc query model; Go package caching

---

## 1. Why Now

Explicit dependencies and stable identities reduce future migration work.
This document defines those boundaries and stages cache implementation.
The [roadmap](implementation_roadmap.md) owns delivery dates and feature status.
This proposal does not authorize changes to language semantics.

## 2. The Three Levels (Scope the Word "Incremental")

| Level | Scope | Main requirement |
|-------|-------|------------------|
| **L1 — Package/module cache** | Reuse unchanged package artifacts and relink | Package inputs, dependencies, configuration, and artifacts are compatible |
| **L2 — Declaration-level reuse** | Recompute affected declarations and derived artifacts | Query dependencies, stable identities, and result validation are complete |
| **L3 — In-place binary patching** | Replace executable code without a complete relink | Relocations, active code, and resource lifetimes support replacement |

Start with L1 and measure edit/build workloads before expanding reuse granularity.
Measure clean builds too: hashing, dependency recording, and serialization have costs.
No level has an effort estimate or speed guarantee without measurements.

## 3. How It Works (Mechanics)

The core model is a **demand-driven query system with dependency tracking**:

1. Identify a query by its kind, stable arguments, and relevant configuration.
2. Record the inputs and subqueries that the query reads.
3. Validate recorded dependencies before reusing a result in a new revision.
4. If an input changes, recompute the query and compare its semantic result with the previous result.
5. Propagate invalidation when the result changes. If the result is equivalent, dependents can remain valid.

Separate declaration interfaces, bodies, and derived summaries.
Ordinary call analysis consumes signatures and relevant ownership, capture, and effect summaries.
Inlining, generic instantiation, and comptime also consume bodies.
A body edit invalidates every consumer that depends on that body or a changed derived result.

For example, a callee can gain a yielding call without changing its signature.
Its effect summary changes, so dependent call checks must run again.
A comment edit can leave that summary unchanged and stop the invalidation cascade.

Re-execution replaces the old dependency set with the newly observed set.
This matters when comptime or implementation selection takes a different branch.
If adopting rustc's red-green validation algorithm, preserve dependency read order.
Stop replaying the old path when a changed dependency requires fresh execution.

## 4. Ryo Dependencies and Invalidation

### 4.1 Per-function analysis and identity

- **Local analysis still reads external contracts.** Ownership checks a function's Flow IR, but also consumes callee parameter modes, type properties, and destruction/effect summaries. Record those dependencies.
- **Imports define package dependencies.** Include manifests, resolution decisions, generated declarations, and tracked comptime inputs when applicable.
- **Arena handles are local identities.** Compact `DeclId`, `TypeId`, and `StringId` values are not automatically stable across sessions or declaration edits.
- **Persistent sessions need revision boundaries.** Reuse results only within a compatible snapshot of inputs and interned data.

Keep stable declaration identity separate from revision and arena-slot generation.
An inserted declaration must not make an old cache key refer to another declaration.
Define how identities survive edits, renames, deletion, and recreation.
Generation checks reject stale handles; they do not establish persistent identity.
Serialized types, strings, constants, and references need reconstruction or remapping.
Salsa's database revision model tracks changes; it does not make arbitrary arena indices stable.

### 4.2 Dependencies that cross function boundaries

| Feature | Why it fights incremental | Mitigation |
|---------|---------------------------|------------|
| **Inferred yield effects** | A changed callee can change checks in transitive callers | Solve recursive strongly connected components, then propagate changed summaries through dependent components until convergence |
| **`dyn` devirtualization** | Depends on the **whole-program reachable impl set**; adding one impl anywhere can change a decision in a distant file | Version the reachable set; devirtualization decisions keyed by `(call site, set version)` |
| **Cross-function inlining** (spec promises aggressive inlining) | Creates body-level edges: a callee body change must invalidate callers that inlined it | Record inlining decisions as dependency edges (standard technique) |
| **Monomorphized generics** | Bodies, constraints, selected implementations, and comptime values affect instances | Identify requests by declaration, type arguments, and relevant comptime values; track all consumed dependencies and downstream consumers |
| **`comptime`** | Helper bodies and permitted external inputs affect evaluation | Record each permitted input; reject untracked operations; update dependencies when execution changes paths |
| **Optimization and cleanup** | Inlining, escape analysis, and lowering change calls, storage, and generated helpers | Track phase-specific artifacts and invalidate affected analyses and instance collection |
| **Native components** | Rust features, ABI, libraries, and kernel artifacts affect generated programs | Track compatible native manifests and build inputs alongside Ryo artifacts |

A strongly connected component (SCC) contains mutually reachable call-graph nodes.
If `A` calls `B` and `B` calls `C`, a changed effect in `C` can affect all three components.
Call-edge changes can also change SCC membership. Rebuild affected graph structure before solving summaries.
Ordinary query recursion diagnoses cycles unless a query explicitly supports fixed-point evaluation.

### 4.3 Artifact phases, diagnostics, and configuration

Cache TIR, Flow IR phases, ownership/effect/escape analyses, evaluator bodies,
concrete instances, and native objects as distinct artifacts.
Publish their input dependencies, output phase, and compatibility requirements.
Keep cached results immutable, or version each transformed body and its analysis results.
An optimization must not mutate a cached analysis or evaluator body.

Object keys include all consumed body, summary, implementation, layout, and inline dependencies.
Include compiler/cache format versions, target and CPU features, runtime profile and ABI,
codegen options, and native artifact identities where relevant.
Configuration hashing is mandatory for L1 too.
Do not restrict object dependencies to source body and interface hashes.

Treat diagnostics as query outputs with dependencies and source provenance.
Replaying a result must reproduce current diagnostics with valid source locations.
When a failed query becomes valid, remove its old diagnostics.
Even if semantic content is unchanged, source edits can require location remapping.

### 4.4 Package artifacts and native dependencies

Package artifacts are separate from the compiler's private query cache.
Export declarations, constraints, implementation identities, required generic bodies,
optional inline bodies, ownership/effect summaries, ABI descriptions, and native manifests.
Follow the [package metadata contract](pipeline_migration.md#package-metadata-and-separate-compilation).
Use the same query interfaces for source-built and imported results.

Validate compiler/format compatibility and target-dependent artifact settings before reuse.
Missing required metadata needs source rebuilding or a diagnostic.
Missing optional optimization data can disable that optimization.
Native dependencies include selected Rust crates/features, compatible toolchains,
system libraries, runtime ABI, and external backend or kernel artifacts.
Record their build inputs rather than assuming an unchanged Ryo module implies an unchanged native component.

### 4.5 Query lifecycle and parallel execution

Define query states for pending, running, completed, failed, and cancelled work.
Deduplicate concurrent requests for the same query and input revision.
Publish a result and its dependency set together only after successful completion.
Do not expose partial mutable IR or combine results from incompatible revisions.

Track query ownership and wait dependencies so parallel cycles cannot deadlock.
Distinguish a recursive query cycle from another worker's valid in-progress request.
Give effect SCC solvers and instance worklists explicit convergence or expansion limits.
On cancellation or input revision changes, discard unfinished work or retain it only with a valid dependency proof.
Specify intern-pool synchronization and artifact ownership before enabling parallel queries.

### 4.6 Persistent JIT sessions

Just-in-time (JIT) replacement needs a contract beyond analysis invalidation.
Define which version a new call observes and how existing calls continue.
Keep old code, constants, callback environments, and loaded native libraries alive
until active calls, suspended tasks, callbacks, and required destruction finish.
An ABI-incompatible edit cannot replace a target behind an old call signature.

Initially, a session can reject replacement of live declarations or retain prior generations.
Choose and document the policy before implementation. Do not imply in-place replacement from session caching alone.

## 5. Staged Roadmap

Delivery versions remain in the implementation roadmap. The order below describes dependencies.

| Stage | Deliverable | Exit condition |
|-------|-------------|----------------|
| **L1** package cache | Hash inputs, dependency artifacts, and configuration; reuse compatible packages and relink | Warm builds agree with clean builds after package and configuration edits |
| Session reuse | Persistent query/session contexts with revision and code-lifetime policy | No stale handles, diagnostics, symbols, or live-code replacement violations |
| **L2** analysis queries | Dependency-aware reuse and explicit recursive analysis solvers | Edit replay covers summaries, generics, comptime, and dynamic dependencies |
| **L2** codegen cache | Phase-aware object/native artifact reuse with complete dependencies | Inline, ABI, target, and native changes invalidate all affected artifacts |
| **L3** binary patching | Optional replacement mechanism after measured need | Active-code lifetime and relocation contracts are proved and tested |

## 6. The Five Architectural Hooks

- **H-1 — Query-shaped analyses, enforced.** No hidden global reads; inputs flow through parameters (extends LLM instructions R4 from "lazy" to "lazy *and* dependency-explicit").
- **H-2 — Explicit dependency recording.** Record query inputs and reads, including dynamic paths. Publish dependencies with completed results.
- **H-3 — Separate interface, body, and summary fingerprints.** Consumers depend on the results they use (§3).
- **H-4 — Stable identities and valid handles.** Separate persistent declaration identity, input revisions, and arena generations. Remap serialized references across sessions.
- **H-5 — Tracked comptime execution.** Record every permitted input and reject untracked operations from the first evaluator implementation (§4.2).

## 7. Relationship to Existing Decisions

- **Base spec §16's "whole-program AOT" claims** need no retraction — whole-program *analysis results* are themselves cacheable queries with versioned inputs. Whole-program knowledge and incremental compilation coexist if dependencies are recorded; they conflict only if analyses read global state untracked (forbidden by H-1/H-2).
- **Effect-analysis performance:** treat budgets as measured goals, not guaranteed worst-case costs. Correctness permits a full recomputation when dependency changes require it.
- **Flow IR migration:** mandatory analysis and lowering remain correct with cache reuse and optional optimizations disabled. Instance collection expands until generated callees and helpers are resolved.
- **LLM instructions:** H-1 tightens R4; H-2/H-4 extend R1/R18 (index-keyed side tables make invalidation surgical). An amendment to `ryo-compiler-llm-instructions.md` §1 folding in H-1…H-5 is the natural next step on approval.

## 8. Risks

| Risk | Mitigation |
|------|------------|
| Recorded dependencies omit a consumed input | Add dependency inspection and clean-build differential tests; graph shape alone does not prove completeness |
| Cache machinery adds clean-build overhead | Measure query granularity, hashing, memory, and serialization; persist only useful results |
| Comptime consumes untracked inputs | Enforce tracked input access from the first evaluator implementation |
| A failed write leaves a partial disk artifact | Publish artifacts atomically; validate format and content before reading; rebuild unusable cache entries |

### Required incremental correctness tests

For each edit sequence, compare an incremental build with a clean build of the same final source and configuration.
Compare diagnostics and observable execution in supported JIT and ahead-of-time (AOT) modes.
Do not require identical binary bytes unless the build contract guarantees deterministic output.

- Change a body, parameter mode, type property, or destructor; check affected callers and cleanup.
- Change a callee effect across several SCCs; add and remove recursive call edges.
- Change an inline or generic body, constraints, implementation set, or comptime argument.
- Change a permitted comptime input and switch evaluation branches; check replacement of dependency edges.
- Insert, rename, delete, and recreate declarations; check persistent identity and handle remapping.
- Change target, CPU features, options, runtime profile, ABI, Rust features, or native/kernel artifacts.
- Move source locations, introduce an error, and fix it; check diagnostic replay and removal.
- Compare source imports with package artifact imports; test missing and incompatible metadata.
- Request a query from multiple workers, trigger a cycle, cancel work, and change revisions during execution.
- Keep an old JIT callback or task active during an edit; check rejection or generation retention under the selected policy.

Run cache-disabled and optimization-disabled variants where supported.
Measure cold, warm, and edited builds, memory use, and cache storage separately.
These tests are acceptance gates for the corresponding stages, not optional follow-up work.

## 9. Open Questions

- **Q1 — Persistence granularity:** begin L2 with in-memory results; choose disk persistence by measured reuse benefit and serialization cost.
- **Q2 — Identity strategy:** choose stable declaration keys and edit matching, including rename and delete/recreate behavior.
- **Q3 — JIT replacement policy:** choose rejection of live replacement, retained generations, or another explicit mechanism.
- **Q4 — Parallel scheduler:** choose query ownership, cycle detection, intern-pool synchronization, and cancellation mechanisms.

---

## References

- Spec: [Section 5](../specification.md#5-memory-management-ownership-lite) (ownership and destruction), [Section 9](../specification.md#9-concurrency-model-taskfuturechannel) (tasks and effects), and [Section 16](../specification.md#16-tooling) (tooling).
- Dev: [Pipeline migration](pipeline_migration.md), [concurrency](concurrency.md), [tensor integration](tensor.md), and [compiler instructions](ryo-compiler-llm-instructions.md).
- Delivery: [Implementation roadmap](implementation_roadmap.md).
- rustc: [Incremental compilation](https://rustc-dev-guide.rust-lang.org/queries/incremental-compilation.html) for result fingerprints, dependency validation, and ordered reads.
- Salsa: [Incremental algorithm](https://salsa-rs.github.io/salsa/reference/algorithm.html) for revisions, dependency checks, and unchanged-result reuse.
- Go: [Compiler architecture and export data](https://go.dev/src/cmd/compile/README) for package metadata and analysis summaries.
