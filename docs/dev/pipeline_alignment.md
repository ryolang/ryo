**Status:** Design (v0.2+)

# Pipeline Alignment with Zig: UIR/TIR Split, Deeper InternPool, Structured Diagnostics

A multi-phase plan that closes the gaps between Ryo's current middle-end and the Zig-style pipeline it was modelled after. Each phase is independently mergeable and pays off on its own; together they put the foundation in place for the language features queued in the roadmap that the current shape can't comfortably absorb — most importantly **compile-time execution (comptime)**, **generics with monomorphization**, and **inline expansion of closures**.

This plan picks up where [middle_end_refactor.md](middle_end_refactor.md) left off. Read that first; it explains why we have an `InternPool`, an `ast_lower → sema → codegen` pipeline, and a HIR with `HirExpr.ty: Option<TypeId>`. Everything below is incremental on top of that branch.

## Motivation

The middle-end refactor adopted the *intent* of the Zig pipeline (interned types, structural-then-semantic split) but kept the *form* small: one tree-shaped HIR mutated in place, one type-only InternPool, and `String` errors. That trade-off was correct for the size of the language at the time. Three planned features will outgrow it:

1. **Comptime** is fundamentally an evaluator. Evaluating a tree-shaped HIR by recursive descent works for pure expressions, but comptime must drive resolution lazily (one declaration at a time, with cycles), suspend on dependencies, and substitute values back into instructions. That requires a flat instruction stream and a value/decl pool, not a tree.
2. **Generics** require monomorphization: one generic UIR body becomes N typed TIR bodies, one per instantiation. With our current "Sema mutates HIR in place" shape there's no place to *put* those N copies — the HIR has only one set of slots.
3. **Inline expansion** (closures, `@inline` calls, comptime-evaluated calls) is one-instruction-becomes-many. Mutating a tree node in place can't represent that; emitting into a fresh TIR stream can.

A fourth concern is diagnostics. Once Sema becomes lazy, errors no longer happen at a single textual call site — they happen mid-evaluation, possibly multiple at once, possibly in code that originated from a different file via `comptime`. `Result<_, String>` short-circuits the first error; `Module.errors: Vec<ErrorMsg>` doesn't.

## A Note on Naming: UIR / TIR vs. Zig's ZIR / AIR

The two instruction streams introduced by Phases 3 and 4 are called **UIR** (Untyped IR) and **TIR** (Typed IR). They are the direct structural analogue of Zig's **ZIR** (`src/Zir.zig`) and **AIR** (`src/Air.zig`):

| Ryo  | Zig | Role |
|------|-----|------|
| UIR  | ZIR | Flat, untyped instruction stream emitted by `astgen` from the AST. Input to Sema. |
| TIR  | AIR | Flat, fully-typed instruction stream emitted by Sema, one per function body. Input to codegen. |

Why not just call them ZIR and AIR?

- **ZIR is internal to the Zig compiler.** Its on-disk shape, instruction set, and encoding are an implementation detail of `zig` and change between versions. Ryo neither produces nor consumes Zig's ZIR — we use Zig only as a linker driver — and adopting the name would falsely imply an interchange format.
- **Symmetry.** Renaming only ZIR while keeping AIR breaks the pair; renaming both to descriptive names (Untyped / Typed) keeps the staircase obvious and parallels Rust's HIR / THIR / MIR convention.
- **Avoiding collision.** A bare "RIR" ("Ryo IR") is ambiguous — HIR is also a Ryo IR today. UIR / TIR each say what they are.

Throughout the rest of this document, **UIR works exactly like Zig's ZIR** (untyped, produced by `astgen`, consumed by Sema) and **TIR works exactly like Zig's AIR** (typed, per-function-body, produced by Sema, consumed by codegen). Where this plan cites `Zir.zig`, `Air.zig`, `Air.Liveness`, or `Air.Legalize`, those remain Zig file references — they describe the *upstream design* we're borrowing from, not a Ryo module name.

## Guiding Principles

- **No user-visible changes per phase.** Each phase merges with identical CLI behaviour, identical accepted programs, identical (or strictly better) error messages.
- **Each phase is reversible.** A phase that explodes reverts to the previous one without dragging unrelated work back with it.
- **Additive over rewriting.** Reserved variants, sidecar arrays, and tagged enums are chosen specifically so the next feature lands as a new variant and a new switch arm — not as another middle-end overhaul.
- **No language-feature work inside this plan.** Per root `CLAUDE.md`, language design changes need approval; this is internal restructuring. New features (comptime, generics, control flow) ride on top once their phase lands.
- **Repo conventions.** `refactor:` commit prefix, branches `refactor/diagnostics`, `refactor/internpool-deepen`, `refactor/uir`, `refactor/tir`, `refactor/lazy-sema`. CI gates: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test` on Ubuntu + macOS.

## Phase Map

| # | Name | Unblocks | Risk | Effort |
|---|---|---|---|---|
| 1 | Structured Diagnostics | every later phase's testability and UX | Low | ~1 day |
| 2 | InternPool Deepening | tuples, funcs, structs, UIR strings | Low | ~1.5 days |
| 3 | UIR — Untyped Instruction Stream | comptime evaluation surface | Medium | ~3 days |
| 4 | TIR — Typed Instruction Stream | monomorphization, inline expansion | Medium | ~3 days |
| 5 | Lazy Sema / Worklist Driver | comptime, generics | High | ~3–4 days |

Total: **~11–13 working days** for one implementer, including review.

Phases 1 and 2 can land in any order or in parallel branches. Phases 3, 4, 5 are sequenced.

---

## Phase 1 — Structured Diagnostics

**Goal:** replace `Result<_, String>` in the middle-end with a structured `Diag { span, severity, code, message }` and let analysis collect multiple diagnostics before returning.

### 1.1 New module: `src/diag.rs`

```rust
// Public API sketch — design, not final code.
#[derive(Debug, Clone)]
pub struct Diag {
    pub span: Span,
    pub severity: Severity,
    pub code: DiagCode,
    pub message: String,
    pub notes: Vec<DiagNote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity { Error, Warning, Note }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagCode {
    UnknownType,
    UndefinedVariable,
    UndefinedFunction,
    TypeMismatch,
    ArityMismatch,
    BuiltinArgKind,
    // Reserved (not constructed in Phase 1, listed for forward-compat):
    // ConstEvalFailure,
    // CycleInComptime,
    // GenericInstantiation,
}

#[derive(Default)]
pub struct DiagSink {
    diags: Vec<Diag>,
    error_count: usize,
}

impl DiagSink {
    pub fn emit(&mut self, d: Diag) { /* increments error_count if severity == Error */ }
    pub fn has_errors(&self) -> bool;
    pub fn into_diags(self) -> Vec<Diag>;
}
```

`DiagCode` is an enum so renderers, tests, and future LSP code can pattern-match without scraping message text.

### 1.2 Replace `String` errors in `ast_lower` and `sema`

- `ast_lower::lower(...) -> Result<HirProgram, Vec<Diag>>` — collects diagnostics, returns either the HIR (zero errors) or the diag list.
- `sema::analyze(...) -> Result<(), Vec<Diag>>` — same shape.
- Each existing `Err(format!(...))` becomes `sink.emit(Diag { span: expr.span, code: ..., message: ..., notes: vec![] })`. Sema continues past errors where it can (e.g. an unknown variable reference still gets `ty = pool.error_type()` and analysis continues — see 1.4).

### 1.3 New `pool.error_type()`

To allow Sema to recover after a single error and keep type-checking the rest of the function, add a sentinel `TypeKind::Error` to `InternPool`. Anywhere a type is required but the operand failed to resolve, we splice in `pool.error_type()`. Subsequent type comparisons treat it as "compatible with anything" so we don't get a cascade of follow-on errors.

This is exactly Zig's `Type.@"anyerror"`-but-for-type-resolution-failure: see Zig's `Sema.fail` + `Type.err_set_inferred`.

### 1.4 Renderer

Pipeline now owns a renderer (Ariadne already wired up for parse errors) and converts `Vec<Diag>` to formatted output. Existing parse-error rendering moves into the same path, so all stages emit the same shape.

### 1.5 Commits

1. `refactor: add diag module with Diag, Severity, DiagCode, DiagSink`
2. `refactor: introduce TypeKind::Error sentinel`
3. `refactor: convert ast_lower errors to Diag`
4. `refactor: convert sema errors to Diag with multi-error continuation`
5. `refactor: route parse errors through Diag renderer`

### 1.6 Exit criteria

- `grep -rn 'CompilerError::LowerError(String' src/` returns nothing.
- Sema can report two errors from one function in a single run; covered by a regression test.
- `ryo run` and `ryo build` produce strictly more useful errors (every middle-end error now has a span).
- All existing tests still pass, with assertions updated from `err.contains("...")` to `diag.code == DiagCode::...` where appropriate.

---

## Phase 2 — InternPool Deepening

**Goal:** make `InternPool` storage-shape-correct for everything we'll throw at it (tuples, function types, struct/enum payloads, interned strings) and tighten its ergonomics where Zig's pool is meaningfully better.

### 2.1 Sub-step A — Sidecar `extra: Vec<u32>` for variable-size payloads

Zig stores types in two arrays: a `MultiArrayList(Item)` of fixed-size `(tag, data)` pairs, and an `extra: ArrayListUnmanaged(u32)` where variable-size payloads (tuple element lists, function signatures) live. Dedup hashes the *content* of the extra range, not a Rust enum value.

For Ryo:

```rust
pub struct InternPool {
    items: Vec<Item>,             // (tag, data)
    extra: Vec<u32>,              // variable-size payloads
    strings: Vec<u8>,             // see 2.3
    dedup: HashMap<TypeKey, TypeId>,
}

#[repr(u8)]
enum Tag {
    Void, Bool, Int, Str, Error,
    // Phase 2 only adds Tuple to prove the encoding works;
    // Func, Struct, Enum, Option, ErrorUnion ride on top later.
    Tuple,
}

struct Item { tag: Tag, data: u32 } // `data` is either inline payload or an `extra` index
```

Dedup of e.g. `Tuple([T1, T2, T3])` keys on `(Tag::Tuple, hash([T1.0, T2.0, T3.0]))`, not on cloning a `Box<[TypeId]>`. **This is the change that gets harder if deferred** — every later type variant pays the clone cost otherwise.

### 2.2 Sub-step B — `TypeId` becomes a typed enum with named primitive variants

Replace the plain `TypeId(u32)` newtype with:

```rust
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum TypeId {
    // Reserved primitive slots — interned at fixed indices in `new()`.
    Void  = 0,
    Bool  = 1,
    Int   = 2,
    Str   = 3,
    Error = 4,
    // Everything else lives at runtime indices >= FIRST_DYNAMIC.
    Dynamic(NonZeroU32) // tagged so the discriminant lives in the upper bits
}
```

(Exact representation TBD; Zig uses `enum(u32)` with sentinel high values, Rust will need a manual `transmute`-or-match scheme. The interface is what matters: pattern-matching on primitives is exhaustive at compile time.)

Two payoffs:

- `match` on `TypeId` for primitive cases is exhaustive — adding a primitive forces every consumer to update, just like adding a `TypeKind` variant does today.
- `pool.int()` accessor disappears. `TypeId::Int` *is* the int. One less indirection, one fewer way to be wrong.

### 2.3 Sub-step C — Strings interned in the same pool

Add `StringId(u32)` and `pool.intern_str(&str) -> StringId`. Identifiers (`Var`, `Call`, function names, parameter names) move from `String` to `StringId`. Tokens stop holding `&'a str` slices into source — they hold `StringId` — which kills `lexer::leak_token` and resolves I-008.

This is the change that lets UIR (Phase 3) be a pure `Vec<u32>`-shaped thing instead of carrying owned `String`s.

### 2.4 Sub-step D — Cached primitive-only fast path stays

`pool.void()`, `pool.int()` etc. remain as `const fn` returning `TypeId::Void`, `TypeId::Int`. They become trivial; existing call sites don't have to change. Eventually we'd remove them but it's not required for this phase.

### 2.5 Commits

1. `refactor: split InternPool storage into items + extra + dedup-on-content`
2. `refactor: introduce Tuple variant to prove sidecar encoding`
3. `refactor: TypeId becomes typed enum with named primitive variants`
4. `refactor: add StringId and intern identifiers via the pool`
5. `refactor: lexer tokens hold StringId, not &str`

### 2.6 Exit criteria

- `cargo test` green; no behavioural change.
- A new test interns 1000 identical `Tuple([Int, Int])` entries and asserts they all return the same `TypeId` with no allocation growth after the first.
- `lexer::leak_token` deleted; tokens are `Copy`.
- `String` count in HIR drops by ≥ 80% (function names, var names, param names, str literals all become `StringId`; only `StrLiteral`'s payload remains owned, and even that becomes `StringId` once string ops land).

---

## Phase 3 — UIR (Untyped Instruction Stream)

**Goal:** replace the tree-shaped HIR with a flat instruction stream produced by `ast_lower` (rename to `astgen`). UIR is the input to Sema. Codegen no longer touches it.

### 3.1 Shape

```rust
// src/uir.rs
pub struct Uir {
    pub instructions: Vec<Inst>,        // (tag, data) pairs
    pub extra: Vec<u32>,                // variable-size payloads
    pub func_bodies: Vec<FuncBody>,     // entry index + extra range per function
}

#[repr(u8)]
pub enum InstTag {
    IntLiteral, StrLiteral, BoolLiteral,
    VarRef, ParamRef,
    Add, Sub, Mul, Div, Eq, NotEq, Neg,
    Call,
    Return, ReturnVoid,
    VarDecl, // declaration with optional annotation, both as TypeId::None when absent
    // Reserved: If, Loop, Break, Continue, Block — for control flow milestone.
    // Reserved: ComptimeBlock, Decl — for comptime milestone.
}

pub struct Inst { tag: InstTag, data: InstData }

pub union InstData {
    int_literal: isize,
    bin_op: BinOp,                  // { lhs: InstRef, rhs: InstRef }
    call: ExtraIndex,               // points into `extra`
    var_ref: StringId,
    // ...
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct InstRef(NonZeroU32);     // index into `instructions`
```

`InstRef` is what was `Box<HirExpr>` — but now any number of instructions can refer to the same operand without ownership games. This is the shape that lets monomorphization duplicate a body cheaply: copy the `InstRef` slice; don't deep-clone a tree.

### 3.2 `astgen` produces UIR

`src/ast_lower.rs` is renamed `src/astgen.rs` and rewritten to produce UIR. Its responsibilities are unchanged from middle_end_refactor.md §2.4:

- Pure structural translation.
- Resolves syntactic type annotations to `TypeId`.
- Emits no types on instructions (every instruction's `result_ty` slot stays `TypeId::None`-equivalent — but UIR may not even have that slot; types attach to TIR, not UIR).

### 3.3 Sema is rewritten for UIR (interim)

In Phase 3 only, Sema *still mutates a side-table* (`Vec<Option<TypeId>>` indexed by `InstRef`) — i.e. types live in a parallel array, not on the instruction. This is the interim shape. Phase 4 replaces this with TIR.

The reason for this two-step: switching Sema's input (AST→HIR vs. UIR) is one change; switching its output (mutate-in-place vs. emit-TIR) is another. Doing both at once is the kind of step that's hard to review and harder to revert.

### 3.4 Codegen consumes UIR + side-table

Codegen's traversal becomes index-driven (`for inst in uir.instructions[func.body_range]`) instead of recursive (`eval_expr(builder, expr)`). The expression tree's recursive structure is reconstructed only where Cranelift needs nested values — which is everywhere. To avoid evaluating an instruction twice, codegen memoizes `InstRef → cranelift::Value` for the lifetime of the function (Zig calls this an `Air.Liveness`-like mapping; we won't need full liveness yet).

### 3.5 Commits

1. `refactor: introduce uir module with Inst, InstRef, instructions+extra storage`
2. `refactor: rename ast_lower to astgen and emit Uir instead of HirProgram`
3. `refactor: sema consumes Uir and writes types to a side-table`
4. `refactor: codegen consumes Uir + side-table with InstRef memoization`
5. `chore: delete src/hir.rs (HirProgram, HirExpr, HirStmt) — replaced by uir`

### 3.6 Exit criteria

- `src/hir.rs` deleted.
- Codegen has zero recursive `eval_expr` calls; everything is index-driven.
- `Uir::dump` reproduces a Zig-style listing (`%0 = int 42`, `%1 = add %0, %0`) which becomes the new `ryo ir --emit=uir` output.
- `cargo test` green; no behavioural change.

---

## Phase 4 — TIR (Typed Instruction Stream)

**Goal:** Sema *consumes* UIR and *emits* TIR — a fresh, fully-typed instruction stream — instead of mutating a side-table. Codegen consumes TIR.

### 4.1 Shape

```rust
// src/tir.rs
pub struct Tir {
    pub instructions: Vec<TypedInst>,   // (tag, ty, data)
    pub extra: Vec<u32>,
    pub func_bodies: Vec<FuncBody>,
}

pub struct TypedInst {
    tag: TirTag,
    ty: TypeId,                         // never None
    data: InstData,
}

#[repr(u8)]
pub enum TirTag {
    // Mostly mirrors InstTag but lowered: e.g. UIR's polymorphic
    // Add becomes TIR's IAdd / FAdd / etc. once float lands.
    IConst, BConst, StrConst,
    IAdd, ISub, IMul, ISDiv,
    ICmpEq, ICmpNe,
    INeg,
    Load, Store,
    Call, Ret, RetVoid,
    Br, CondBr, Block,                  // reserved for control flow
}
```

Crucially: **TIR is per-function-body, not per-program.** One `Tir` per `FuncBody`. This is what lets monomorphization produce N TIR copies from one UIR body during Phase 5.

### 4.2 Sema becomes a translator

`sema::analyze(uir: &Uir, pool: &mut InternPool, sink: &mut DiagSink) -> Vec<Tir>` — one TIR per UIR function body. Sema:

1. Allocates an `Tir` for each function.
2. Walks UIR instructions, emits TIR instructions, records `UirRef → TirRef` mapping.
3. Errors go to `sink`; on error, emits `TirTag::Unreachable` so codegen still has a well-formed TIR.

This is the change that **enables comptime**: a comptime block in UIR doesn't emit TIR at all — it evaluates and stores its result as a value in the InternPool. A generic call in UIR triggers a *new* TIR emission from a different UIR body. Both are additive on this shape and impossible on the current one.

### 4.3 Codegen becomes thin

Codegen is now a near-mechanical TIR-to-Cranelift translator. No type lookups (every TIR instruction carries `ty`), no `expect_ty()`, no `debug_assert!(all_expr_types_resolved)`. The "sema must run before codegen" invariant becomes "codegen takes `&[Tir]`, not `&Uir`" — encoded in the type system.

**Note on the missing MIR layer.** Zig's pipeline has a per-backend MIR (`AnyMir`) between TIR and machine code; ours doesn't. Cranelift IR *is* our MIR — instruction selection, register allocation, and target legalization all live inside Cranelift. TIR-to-Cranelift is therefore a thinner translation than TIR-to-MIR would be in Zig, and `Air.Legalize`-style passes (scalarizing vectors, expanding overflow checks) are deferred to Cranelift rather than implemented here. See *Out of Scope* for the conditions under which that decision would need to be revisited.

### 4.4 Commits

1. `refactor: introduce tir module with TypedInst and per-function bodies`
2. `refactor: sema emits Tir instead of mutating UIR side-table`
3. `refactor: codegen consumes Tir, drops type-resolution asserts`
4. `chore: drop UIR side-table; sema is the translator`

### 4.5 Exit criteria

- Codegen function signature is `fn compile(tir: &[Tir], pool: &InternPool, ...)`. No UIR import.
- `ryo ir --emit=tir` produces a typed listing distinct from `--emit=uir`.
- A regression test feeds a UIR with a deliberate type error and verifies (a) TIR still produced for the rest of the function, (b) `Unreachable` instruction at the failure point, (c) `DiagSink` has exactly one diagnostic.
- All existing examples produce byte-identical machine code to pre-Phase-3.

---

## Phase 5 — Lazy Sema / Worklist Driver

**Goal:** Sema stops being a top-down recursion over functions and becomes a worklist-driven evaluator that resolves declarations on demand. This is the prerequisite for comptime.

### 5.1 Shape

```rust
// src/sema.rs (rewritten)
pub struct Sema<'a> {
    pool: &'a mut InternPool,
    uir: &'a Uir,
    decls: HashMap<DeclId, DeclState>,
    queue: VecDeque<DeclId>,
    sink: &'a mut DiagSink,
}

enum DeclState {
    Unresolved,
    InProgress,                         // for cycle detection
    Resolved(TirRef),                   // for functions: the emitted TIR
    ResolvedValue(ValueId),             // for comptime constants
    Failed,
}
```

This maps onto Zig's three resolution strategies (eager / lazy / sema): `Resolved*` is the eager state ("already done, just hand it back"), `Unresolved` is the lazy state ("defer until someone asks"), and the act of popping a decl off the queue and transitioning it to `InProgress` is sema-time resolution. Naming the three modes explicitly matters because they're what enables forward references in struct fields and generic parameters — exactly the cases Phase 5 claims to unblock.

Sema works by:

1. Seeding the queue with `main` (or every exported decl).
2. Popping a decl, marking it `InProgress`, evaluating it.
3. Each UIR instruction may *depend* on another decl (`call foo` needs `foo`'s signature; `comptime expr` needs the values it references). On a missing dependency, push the dependency to the queue and re-queue this decl behind it.
4. Cycles → `DeclState::InProgress` is hit again → diagnostic.

This is structurally the same as Zig's `Module.semaDecl` + `Sema.analyzeBodyInner` driver loop, scaled down.

### 5.2 What this enables

- **`comptime` blocks**: a `comptime { ... }` UIR instruction asks Sema to evaluate its body to a value (not emit TIR for it). The result is interned in the pool as a `ValueId` and substituted at TIR emission. Implementation is `~200 LOC` of evaluator on top of the worklist.
- **Generics**: `fn foo[T](x: T) -> T` in UIR is a *template*. A call site `foo(42)` asks Sema to resolve `foo` *with `T = Int`*. The resolved decl is keyed on `(DeclId, [TypeId])` rather than just `DeclId`. New TIR per instantiation; UIR is emitted once.
- **Inline expansion**: Sema, instead of emitting `TirTag::Call`, splices the callee's TIR instructions into the caller's TIR with arguments substituted. Same mechanism as generics, different trigger.

### 5.3 Commits

1. `refactor: introduce Sema struct with decl table and worklist`
2. `refactor: sema seeds worklist from exported decls and drives to fixpoint`
3. `refactor: cycle detection via DeclState::InProgress`
4. `refactor: dependency ordering — function signatures resolved before bodies`
5. `test: comptime / generics smoke tests gated behind cfg(unimplemented)`

The last commit is intentionally degenerate — it adds *infrastructure* tests for comptime/generics behind a cfg flag, with no language surface. Those features land as their own roadmap milestones; this phase's job is to make the *plumbing* ready.

### 5.4 Exit criteria

- Sema's public entrypoint is `Sema::run(uir, pool, sink) -> Vec<Tir>`; the function tree is no longer walked recursively from outside.
- A cycle test (`fn a() -> int: return b()` / `fn b() -> int: return a()`) produces a typed cycle diagnostic, not a stack overflow.
- The `unresolved-call-resolves-callee-signature-first` test passes — i.e. the order of function definitions in source no longer matters for type resolution (it doesn't today either, but the worklist makes it inherent rather than incidental).

---

## Risk Register

| Risk | Likelihood | Mitigation |
|---|---|---|
| Phase 3's interim "side-table" Sema feels like throwaway work | Medium | It is throwaway, deliberately. Splitting the input change (HIR → UIR) from the output change (mutate → emit-TIR) keeps each commit reviewable. The side-table is ~50 LOC. |
| `TypeId` becoming an enum with `Dynamic(NonZeroU32)` is awkward in Rust | Medium | Fall back to a plain newtype with `const fn` accessors if the enum encoding fights the borrow checker. Loses compile-time exhaustiveness; keeps everything else. |
| UIR's `union InstData` requires `unsafe` | High | Use `enum InstData` with the largest variant determining size — costs a few bytes per inst but stays safe. Zig has the luxury of `extern union` without unsafety; we don't. |
| `DiagSink` accumulates diagnostics that mask later real errors | Low | Cap at 100 diagnostics per file; further errors are dropped with a "too many errors" note. Same as `rustc`. |
| Worklist driver introduces non-determinism in error order | Low | Stable ordering: priority queue keyed by source span. Diagnostics are sorted by span before render. |
| Feature work lands during the refactor and bit-rots | Medium | Same mitigation as the previous refactor: feature PRs need approval anyway, so they're naturally serialized behind this work. Each phase merges before the next starts. |
| Performance regression from per-instruction memoization in codegen | Low | Cranelift already SSA-numbers; one extra `HashMap<InstRef, Value>` per function is negligible. Profile if curious; don't optimize preemptively. |
| Single-threaded `VecDeque` worklist and `HashMap`-backed `InternPool` block future parallel Sema | Low now, Medium post-v0.2 | Zig shards its `InternPool` and runs analysis on a thread pool specifically because the worklist is concurrent. Keep `Sema` and `InternPool` behind narrow `&mut` APIs so swapping in sharded structures later is a local change; don't leak internal iterators or `&` references to the dedup map across phase boundaries. |

## Out of Scope

These belong on the roadmap, not this plan:

- The actual comptime evaluator (this plan delivers the *substrate*, not the feature).
- Generic monomorphization logic (substrate only).
- Inline expansion logic (substrate only).
- Splitting UIR into per-file files (Zig does this for incremental compilation; Ryo doesn't compile incrementally yet).
- Replacing `String`-backed source storage with a `SourceMap`. Useful for multi-file diagnostics; orthogonal here.
- Moving `print` to a runtime crate (ISSUES.md I-006). Falls out trivially after Phase 4 — `sema::check_builtin_call` becomes a normal type-checked call against an external decl — but is not gated on this plan.
- Control flow (`if`/`else`/`match`) — ISSUES.md I-003. Reserved variants in UIR/TIR are listed above; the actual feature lands as its own milestone after Phase 4.
- **TIR legalization** (Zig's `Air.Legalize` / per-backend `legalizeFeatures()`). Zig has an explicit pass between Sema and codegen that scalarizes vectors, expands overflow checks, and rewrites operations the target backend can't handle. Cranelift owns target legalization for us, so we don't need a parallel pass — but if/when a non-Cranelift backend lands, this gap re-opens and a `air::legalize` module becomes the right place for it.
- **A separate MIR layer.** Zig lowers TIR → MIR (per-backend, unified through `AnyMir`) → machine code. Cranelift IR plays the role of MIR for us: codegen translates TIR directly into Cranelift IR and Cranelift handles instruction selection, register allocation, and emission. The plan's "TIR-to-Cranelift translator" framing in §4.3 is deliberate — there is no Ryo-owned MIR and won't be unless we drop Cranelift.
- **`Compilation` / `Zcu` split.** Zig separates the overall build (`Compilation`: C interop, linking, output) from the Zig-source-only analysis context (`Zcu`: InternPool, decl table, export tracking). Today a Ryo program is a single compilation unit with no C interop, so we collapse both into the pipeline driver + `InternPool`. The split becomes interesting alongside C interop or multi-package builds; until then it's noise.

## Effort Estimate

| Phase | Days |
|---|---|
| 1 — Diagnostics | ~1 |
| 2 — InternPool deepening | ~1.5 |
| 3 — UIR | ~3 |
| 4 — TIR | ~3 |
| 5 — Lazy Sema | ~3–4 |
| **Total** | **~11–13** |

For one implementer, including review cycles. Phases 1 and 2 can run in parallel branches against `main`; Phases 3–5 are strictly sequenced.

---

## References

- Spec: §4 (Types), §10 (Compile-time evaluation — when written) — the type and comptime systems this work enables.
- Dev: [middle_end_refactor.md](middle_end_refactor.md) — the predecessor plan this builds on; [compilation_pipeline.md](compilation_pipeline.md), [parser.rs.md](parser.rs.md).
- Roadmap: prerequisite for *Compile-time Execution (comptime)* and *Full Generics System* in [implementation_roadmap.md](implementation_roadmap.md) (Phase 5, post-v0.1.0).
- Issues: [../../ISSUES.md](../../ISSUES.md) — Phases 1, 2, 3 collectively close I-008 (token lifetime), and Phase 4 makes I-006 (print to runtime) a one-day follow-up.
- Inspiration: Zig's `src/InternPool.zig` (sidecar `extra` array, named primitive indices, single pool for types/strings/values), `src/AstGen.zig` → `src/Sema.zig` separation, `src/Zir.zig` and `src/Air.zig` shapes, `src/Module.zig`'s decl worklist.
