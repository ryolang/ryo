# LLM Coding Instructions — Ryo Compiler (Rust + Cranelift)

> **Status:** Normative contributor instructions for AI coding agents
> **Last Updated:** 2026-07-22
> **Applies to:** the Ryo compiler, runtime, and toolchain repositories
> **Source of truth (in order):** `../specification.md` → `ryo-slicing-and-memory-model-final-spec.md` (D1–D11) → `ryo-proposal-review-issues.md` → these instructions
> **Foundational reference:** Andrew Kelley, *Practical Data-Oriented Design* (Handmade Seattle 2021, <https://www.youtube.com/watch?v=IroPQ150F6c>) — the Zig-compiler talk this architecture follows. Treat its techniques as normative below; where a rule cites "Kelley", it refers to this talk.

You are helping build the Ryo compiler. Ryo is an AI-era language: its design rule is *"a human reviewer must understand the code by reading it, without memorizing special rules."* That rule applies to **your output** with full force. The human reviews; you write for the reviewer.

The compiler architecture follows the **Zig self-hosted compiler philosophy**: lazy, demand-driven analysis; flat data; indices instead of pointers; arena-scoped allocation; boring code over clever code. **Data-Oriented Design is the baseline, not an optimization.** Retrofitting it later is a rewrite — do it from the first commit.

---

## 1. Non-Negotiable Data-Oriented Rules

**R1 — Flat arrays and typed indices, never pointer trees.**
AST, IR, types, symbols, and diagnostics live in flat `Vec`s (or arena vectors), referenced by newtype indices:

```rust
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct NodeId(u32);   // 4 bytes, not 8 — half the cache footprint of a pointer (Kelley)
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct TypeId(u32);
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct Symbol(u32);   // interned string

struct Ast {
    tags: Vec<NodeTag>,      // hot: passes iterate dense tag arrays
    payloads: Vec<Payload>,  // cold: touched only for matching nodes
    spans: Vec<Span>,        // struct-of-arrays where hot
    strings: Interner,       // all identifiers/literals interned
}
```

- **Never** `Box<Node>` trees, `Rc<RefCell<Node>>`, or reference-linked structures in the IR. If you reach for `Rc`/`RefCell` in compiler data structures, stop — the design is wrong, ask.
- Indices are `u32`, not `usize`: 4-byte references double cache density and keep node structs small (Kelley). A compiler never has 4 billion nodes in one array; use `usize` only at the memory-allocation boundary.
- Indices are `Copy`: no lifetime parameters on IR types, no borrow fights. This is deliberate — it keeps your output simple and reviewable.
- Node enums are plain `enum Node { Call { f: NodeId, args: Range }, ... }` matched exhaustively. **No visitor-trait hierarchies, no `dyn` in hot paths.**

**R1a — Struct layout discipline (Kelley's padding rule).**
- Order fields by descending alignment (`u64` before `u32` before `bool`); a `bool` after a `u64` costs 8 bytes, not 1. Check `std::mem::size_of` for any struct allocated per-node/per-token — if the number surprises you, reorder.
- For hot arrays, **split hot tags from cold payloads** (struct-of-arrays): a pass that only needs node kinds must iterate a dense `Vec<NodeTag>`, never stride over fat payload structs.
- Pack flag sets into bitfields (`u8`/`u32` masks) for per-node/per-token flags; boolean fields multiply padding.

**R2 — Intern everything textual.**
All identifiers, string literals, and type names go through one `Interner`. Compare `Symbol`, never `str`. No `String` fields in IR.

**R3 — Phase-scoped allocation, and count every allocation.**
Allocate per compilation phase/unit in an arena (`bumpalo` or plain `Vec`s retained per phase); free wholesale at phase end. No per-node deallocation logic, ever. **Heap allocation is one of the slowest operations a CPU performs** (Kelley) — treat allocation *count* as a first-class metric: the compile benchmark suite records allocations per phase, and a regression blocks merge the same way a time regression does (§6). Reserve `Vec` capacity when the size is knowable; amortized growth is fine, per-item allocation is not.

**R4 — Laziness (Zig-style).**
Analyze only reachable declarations, on demand. The driver asks "give me the type of decl X" and the compiler computes it, caching results. Do not build whole-program passes that touch everything upfront. Whole-program *analyses* that are required by the spec (ownership pass, `[yields]` effect inference — final spec §7, concurrency plan §6.2) are explicit exceptions, and are still demand-seeded from reachable roots.

---

## 2. Rust Discipline

**R5 — No `unsafe` in the compiler.** Runtime stack-switching lives in the curated `corosensei` dependency. If a benchmark ever forces `unsafe` in tree code, it requires: a `// SAFETY:` comment proving the invariant, a linked issue, and human sign-off in review. (We dogfood D4.)

**R6 — Curated, pinned dependencies.** Core set: `cranelift-*`, `corosensei`, `mio`, `crossbeam-*`, `bumpalo`/`typed-arena`, `insta` (tests). Every new dependency needs a one-line justification in the PR. No `anyhow`/`thiserror` sprawl in the compiler — diagnostics are structured (§3), not stringly.

**R7 — Explicit over clever.**
- No proc macros, no macro_rules beyond trivial repetition. Ryo has no macros; neither does its compiler.
- Small functions (target < 50 lines). Names say what things are (`lower_match_expr`, not `process`).
- Comments explain **why**, referencing spec sections: `// P2 freeze: mutation while projected is an error (final spec §3.2)`.
- No premature generics, no builder-pattern ceremony, no abstraction with a single implementation.

---

## 3. Diagnostics Are the Product

Ryo's error messages are a flagship feature (spec §7.1 style). Rules:

**R8 — Never panic on user input.** No `unwrap`/`expect`/`panic!`/`unreachable!` on any path reachable from source code. ICEs (`internal compiler error`) only for provably-invariant violations, and they print a report, not a bare backtrace.

**R9 — Diagnostics accumulate.** One bad function must not stop analysis of the rest. Emit `Diagnostic { span, message, notes, help }`, recover, continue.

**R10 — The parser is error-resilient.** On syntax error: emit diagnostic, synchronize at the next statement/declaration boundary, produce a partial AST with `Error` nodes. This is required for IDE/REPL use and for showing *all* errors in one pass.

**R11 — Match the spec's error style exactly**: primary span with `^^^`, secondary spans with `---`, `note:`/`help:` lines, suggested fixes where safe. Golden-test every spec error example (base spec §7.2 samples; the exact diagnostic wording in the final spec — e.g. the `--profile` error in §8 and the P2/E1–E4 conditions in §3.5).

---

## 4. Spec Fidelity

**R12 — Cite the spec, don't invent it.** Every language-rule implementation carries a comment with the section reference. If the spec is ambiguous or silent: open an issue labeled `spec-gap` and implement the documented decision only after it lands in the spec. Do not silently choose semantics.

**R13 — Milestone discipline.** Implement only the current milestone's feature set (base spec rollout table). Deferred features (binary pattern matching, `yield`, true-Loom) do not exist. `todo!()` in released milestones is a bug; gate incomplete work behind explicit milestone flags.

---

## 5. Cranelift Discipline

**R14 —** Use `cranelift-frontend` `FunctionBuilder` correctly: declare all SSA variables up front, seal blocks when done, call `finalize`. Run CLIF verification (`enable_verifier`) in debug builds.

**R15 — Boring lowering.** One Ryo-IR node lowers to one obvious CLIF sequence. No peephole cleverness in the lowering pass — Cranelift optimizes; we lower. (Dogfood R15 of the language: "no magic when functions suffice.")

**R16 —** AOT via `cranelift-object` + Zig linker (per spec §14.3); REPL via `cranelift-jit` with a persistent session context (hosted profile only — final spec §8.2).

---

## 6. Performance Rules

**R17 — DOD from day one** (§1) covers 90% of performance. Beyond that: **profile before optimizing** (dogfood spec §5.9). Keep the compile-time benchmark suite in CI; a regression > 5% blocks merge (the concurrency plan's < 10% effect-analysis budget is the model).

**R18 — No pessimized idioms:** avoid `collect()` into intermediate `Vec`s in hot passes; avoid per-node `HashMap` where a dense `Vec` indexed by `NodeId` works — side tables keyed by index are the standard pattern (Kelley: prefer arrays over hash maps when the key space is a dense index; hash maps are for sparse, string-keyed, or unbounded data only). Deduplicate eagerly: interned types and symbols mean equality is a `u32` compare, not a structural walk.

---

## 7. Testing Rules

**R19 —** Snapshot tests (`insta`) for all diagnostics; golden tests for every example in the spec documents; a regression test **before** the fix for every bug.

**R20 —** Property/fuzz the parser (arbitrary byte streams must never ICE — R8/R10). Loom-style permutation tests for the scheduler core (concurrency plan §5.6).

**R21 —** Cross-platform CI (Linux, macOS, **Windows**) from the first commit — per the concurrency plan's risk register, Windows is hardened from Phase 1, not Phase 5.

---

## 8. When Rules Conflict

Ask. Present the trade-off with spec references and let the human decide — the same escalation protocol Ryo asks of its design proposals. Never resolve a conflict by silently dropping a rule.

**Priority order:** correctness of user-visible semantics (spec) > diagnostic quality > architecture (DOD) > performance > brevity.

---

*End of Instructions*
