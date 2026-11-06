**Status:** Proposal (Draft — v0.1) — pending language-owner approval per the design-change escalation rule. Amends the approved 2026-10-20 comptime design decision (the "Zig bet"): same architecture, evaluator sequencing moved to the v0.2 debut.

# Comptime substrate sequencing: axis pre-v0.1, evaluator and surface at the v0.2 debut

## Decision

Comptime is Ryo's only generics substrate — generic functions are functions with comptime type parameters, generic types are types with comptime type parameters; there is no separate classic-generics system and no `[T]` type-parameter syntax. That architecture is settled. What remains is **sequencing**, and this proposal decides it:

- **Pre-v0.1: the instance axis only.** The (decl × comptime-argument identities) machinery through sema, Flow build, ownership, and codegen ships with M25.1's enum collections, built and proven on real containers. M25's hardcoded instantiations are its trivial first customers; M25.1's internal generic body is written once and migrates to public syntax at the debut.
- **v0.2 debut: the evaluator, `type`-value layer, budgets, and all user surface**, co-designed with derives as their load-bearing consumer. `type_info`, sequences, `comptime for`, compiler-known bounds (`where T: Eq`), and generic type declarations all land in the debut milestone, sized by its own design doc against the Serialize/FromRow acceptance criteria.

## Why this sequencing

- **v0.1 has no evaluator customer.** Containers need only compiler-internal monomorphization — substitute the element type into a builtin method set, emit per-instantiation codegen — which M25.1 already plans and which needs no evaluator, no budgets, no re-entrant sema. Default parameters (Phase 5) need constant folding, a fraction of an evaluator. Derives land at v0.2 under any sequencing, and `type_info` is designed *safer* alongside them than frozen a year early.
- **The axis is the risky half, and it stays pre-v0.1.** Per-instantiation analysis through all four downstream stages is the core engineering; landing it via M25.1 proves it on real containers before any evaluator runs on it.
- **v0.1 ships no comptime surface**, so the "voice surface before substrate" failure mode this sequencing guard exists to prevent cannot trigger before the debut — where evaluator, `type_info`, and derives arriving together is the strongest available mitigation.

## Roadmap consequences once approved

- **M25.1:** scope unchanged (enum collections via internal generic bodies); its mechanism note is rewritten as the instance-axis statement — the substrate's first committed slice — not an ad-hoc "compiler-generic builtins" rung.
- **Phase 5 "Traits & Generics System":** rewritten to comptime form (comptime type parameters, compiler-known bounds promoting to real traits, derives as comptime functions emitting `impl`s); the `fn max[T: Comparable]` / `struct Box[T]` promises become their comptime equivalents. The standalone one-line comptime bullet folds into this section.
- **"No Generics in v0.1.0":** stays literally true; gains one line — the instance axis exists internally (M25.1) while the evaluator and user surface debut at v0.2.
- **`pipeline_migration.md`:** records shared-parametric analysis as superseded by per-instantiation analysis (M25.1's monomorphization is already per-instantiation).
- **The approved design record** (gating programs, `type_info` handle design, checking modes, anti-goals) is committed to `docs/dev/` with milestone numbers remapped to the current schedule and amended inline to this sequencing.
- **Concurrency measurements memo** (the standing approval condition) lands with this proposal's acceptance.

## Risks

1. The v0.2 debut milestone is heavy (evaluator, type values, `type_info`, bounds, derives, traits together). Mitigated by the proven axis and the debut's own design-doc scoping.
2. Comptime is the one identity feature not validated before the v0.1 promise. Acceptable: unlike concurrency's unproven mechanisms, the evaluator is standard CTFE machinery with no open research questions.
3. If v0.2 slips, user-defined generics slip with it — true under every sequencing.

## Acceptance criteria

- Owner approves this sequencing (or records a counter-decision); the design record is committed with remapped numbers and the amendment inline.
- M25.1's mechanism note and Phase 5's generics surface are edited as above in the same commit.
- `pipeline_migration.md` carries the supersession note; the concurrency memo lands; the "No Generics" entry states which sequencing it operates under.

## References

- Spec: [Section 10](../specification.md) (comptime — one-line reserved today), [Section 2](../specification.md) (reserved keywords), [Section 19](../specification.md) (Future Work — comptime entries removed when the surface ships)
- Roadmap: [implementation_roadmap.md](implementation_roadmap.md) (M25, M25.1, Phase 5 Traits & Generics, "No Generics in v0.1.0")
- Dev: [pipeline_migration.md](pipeline_migration.md) (step 8; shared-analysis default), [pipeline_alignment.md](pipeline_alignment.md) (comptime hook status), [record_concurrency_poc.md](record_concurrency_poc.md) (proven gate), [alpha_scope.md](alpha_scope.md) (non-goals table — stays true)
