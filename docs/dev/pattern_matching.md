**Status:** Implementation (v0.1)

# Pattern Matching

How the compiler delivers the spec's `match` (§4.6, §7.4): exhaustive
destructuring of enums, literal matching over `int` / `bool` / `str`, and
anonymous-struct scrutinees with literal-field guards — including the
ownership rules for payload bindings and the codegen merge strategy.

`match` is an expression: every arm yields a value of one joined type, or
the whole match is a statement (void). Arms are tried in written order;
the first matching arm wins.

```ryo
match opt:
	Option.Some(v): v
	Option.None: 0
```

## Pipeline shape

Each stage owns one file; the stages pass a decoded *view* of the flat
wire format downstream, never a re-parse of source:

| Stage | File | Responsibility |
|---|---|---|
| AST | `ryo-core/src/ast/patterns.rs` | `ExprKind::Match` with arms; `PatternFieldKind` (pun / rename / wildcard / literal) shared with M10 destructuring |
| UIR | `ryo-core/src/uir/match_.rs`, `ryo-frontend/src/astgen/match_.rs` | `UirBuilder::match_expr` wire; patterns lowered directly (literal fields never route through `InstTag::Destructure`) |
| Sema | `ryo-frontend/src/sema/match_.rs` | pattern resolution + E0129–E0136 family, exhaustiveness, arm typing; emits typed `TirTag::Match` via `TirBuilder::match_expr` |
| Ownership | `ryo-frontend/src/ownership/match_.rs` | per-arm branch walking, union merge, binding owners, arm-start payload drops |
| TIR | `ryo-core/src/tir/match_.rs` | typed wire (discriminant + bindings + guards per arm), owner-token convention, decoded views |
| Codegen | `ryo-backend/src/codegen/match_.rs` | tag dispatch / guard chains / literal chains, binding prologues, merge block |

The matchable scrutinee kinds (E0136 gates the rest): enums by variant,
exact literals over `int` / `bool` / `str`, and anonymous structs by
shape. Named structs (nominal patterns), floats (NaN/IEEE literal story
undefined), views, `bytes`, and `void` / `never` (no value to match) are
rejected.

## Exhaustiveness and redundancy

Coverage is tracked as *discriminant sets* during arm resolution:

- **Enum scrutinee** — one bit per variant index: a `u128` bitset up to
  128 variants, a `Vec<bool>` fallback above. Every `Enum.Variant:` arm
  sets its bit. If the union of all arms is not full and no `_:` arm
  exists → **E0129**, listing the missing variants in declaration order.
- **`bool` scrutinee** — two bits (`true`, `false`); same rule.
- **`int` / `str` scrutinees** — uncoverable: infinitely (or
  content-wise) many values, so only a `_:` arm makes them exhaustive;
  without one → E0129 naming the kind.
- **Anonymous-struct scrutinee** — no M12 discriminant exists
  (`Discriminant::Wildcard` is a coverage placeholder only); structural
  exhaustiveness is deferred to M12.5, so a `_:` arm is always required.

Redundancy is **W0005** (warning): any arm after the wildcard, and any
duplicate discriminant before it, can never execute. Written order wins
at runtime. Deliberate no-double-report: an arm that already errored
(E0131/E0132/…) gets no W0005 on top, and W0005 never suppresses E0129
or any other diagnostic.

## Arm typing

Each cleanly resolved arm is typed in its own child scope seeded with
the payload bindings, then the arm types are joined:

- Every value arm must agree on one `TypeId` → else **E0134**.
- `never` / error-typed arms join freely (diverging arms have no value).
- A statement arm whose body falls through is legal only when no arm
  produces a value → else **E0135**.
- Termination is judged by `block_definitely_terminates`
  (return | break | continue), not return-only: a `break`/`continue`
  arm diverges from the merge like a `never` arm and is legal in a
  value-producing match inside a loop.

One broken arm never suppresses another: resolution, exhaustiveness, and
typing are per-arm, and an errored match recovers as the error-typed
`Unreachable` sentinel (codegen never runs on an errored unit).

## Move-only bindings and E0133

A payload binding of a non-`Copy` field is a *move* out of the scrutinee
(the whole-value model: the payload leaves through the arm, the shell is
dead). Binding a non-`Copy` field by move out of a **borrowed** scrutinee
(`match &opt:` or a borrow-mode parameter) is **E0133**; `Copy` binds and
wildcard arms stay legal on borrows. The diagnostic suggests matching on
an owned value or binding `_`.

The ownership pass models each arm as a branch walked from the pre-match
snapshot (`snapshot_branch` / `take_branch`), joined by `merge_branches`
— moved in any falling-through arm ⇒ moved after the match. On an owned
scrutinee, an arm that binds at least one non-`Copy` field by move
*consumes* the scrutinee on that arm's path; its unbound needs-drop
payload fields (`Some(_)` positions) register as arm-start drops
(`MatchArmDrop`) that codegen fires when the arm begins. Borrowed
scrutinees register nothing. Arm-local dead binds (never read in their
arm) are swept at arm end into arm-gated Frees — an ungated after-match
Free would free garbage on the sibling arms' paths.

### Owner-token convention

Sema's obligation, checked by `Tir::match_by_move_owner_refs`
(`TirBuilder::match_expr`'s doc): immediately before the `Match`
instruction, sema emits exactly one `FieldAccess(scrutinee, field_index,
ty)` instruction per by-move binding — arm order, then binding order
within each arm. Copy bindings skip the token (they copy out of the
scrutinee rather than move). The ownership pass adopts those refs as the
fresh owners of the moved-out fields; codegen never evaluates them.

## The merge-value strategy

A merge block is created up front; how each arm's result reaches it
depends on the result type family:

- **Scalar result** (`int` / `bool` / `float`): one merge-block *param*.
  Value arms jump to the merge passing their value; the merge reads the
  param (the `enums.rs` Eq precedent).
- **Void match**: no param; arms jump bare.
- **Aggregate result** (`str` / `bytes` / struct / anon-struct / enum):
  a pre-allocated result *slot*, sized from the pool **before** the
  dispatch, so the slot address is a constant dominating every arm and
  the merge. Each value arm writes the slot through the canonical
  per-type aggregate writes (the exact helpers the function-return paths
  use); the merge reads the slot (a fat-triple load for `str`/`bytes`,
  the slot address for memory-first structs/enums) and caches the family
  `ValueRepr` on the inst.
- **All arms diverge**: the merge is unreachable and traps.

Why the slot is allocated pre-dispatch: a value materialized inside one
arm's block cannot dominate the merge — reading it from the merge block
is a Cranelift verifier error in debug and a silent miscompile in
release. A pre-dispatch slot address is the dominance-safe constant; the
structural alternative (phi nodes per arm) is what block params do for
scalars, and aggregates do not fit in one machine value.

Diverging arms (never-typed bodies, `return` / `break` / `continue`)
write nothing and leave no merge edge. Sema types an expression arm
through the never-tolerant entry point, so an inline `panic(...)` arm
joins as diverging (not E0017); a statement arm whose trailing statement
diverges (a bare `panic(...)`, the `stmt_terminates` sense) diverges the
same way — codegen leaves no merge edge and terminates the noreturn
call's dead block.

### I-210: aggregate value arms are rejected for now

Aggregate-result matches whose value arms *yield owned storage* — heap
calls, concats, needs-drop literals, by-value reads of tracked
bindings — are rejected at compile time, naming **I-210**. Root cause:
the ownership pass never consumes an arm-body result into the `Match`
inst, so the drop schedules (producer temporaries anchored on the match,
binding last-use Frees) would release storage the result slot aliases —
a double-free or a post-merge read of arm-local SSA values that do not
dominate the merge. Codegen diagnoses the owned producer families (reusing the
elision logic, so the rejected set tracks actually-emitting drops); two
families the guard cannot see are diagnosed earlier, in the ownership
pass. The **borrowed family** — an arm yielding a borrow/`inout`
parameter or binding, or a field projection of one — is E0138
(`arm_result_borrows_caller`): a borrowed owner carries no scheduled
drop, so the codegen guard could never see it, and pre-fix the shape
escaped as corrupted output + SIGTRAP. The **by-move-payload family**
(`Some(v): v` — a bare read of one of the match's own by-move payload
bindings as the arm result) is E0139 (`arm_result_yields_match_payload`):
the payload's drop stays anchored outside the arm and the guard's
binding-read probe missed it while match payload tokens were absent
from the free-binding tables (debug aborted on the dropped free;
release miscompiled in Cranelift's egraph). The payload tokens are
registered in `build_free_binding_names` now, so the guard's
binding-read family is complete as a backstop. Lift: in
`analyze_match`, consume each non-`never` value arm's aggregate result
into the match inst (mirror `analyze_var_decl`'s
`consume_for_assignment` + `rebind_to_init`), then delete the codegen
diagnosis, the E0138 check, and the E0139 check. Until then the
workaround is a `mut` binding assigned inside an if/else restructure.
Tracked in `ISSUES.md` (I-210); the sanitizer fixture
`match_discarded_value` is structurally leak-insensitive today and
should gain a heap-carrying arm when the lift lands.

### View-typed results are rejected (E0137)

A match whose value arms all yield one view type passes the E0134 join
but has no merge shape: a projection's ownership/liveness across arms
needs design (M12.6 carry-over, lifted together with the I-210
consume), and `eval_inst_view` has no `Match` case. Sema rejects the
shape at compile time (E0137), naming the view type and the deferral —
never silently miscompiled.

## Dispatch lowering

- **Enum scrutinee** — load the i32 tag, `switch` (`emit_tag_dispatch`):
  each variant's case routes to the *first* arm covering it (written
  order wins; W0005-dead duplicates stay unrouted), the wildcard arm's
  entry is the switch's `otherwise`, and a fully-covered match without a
  wildcard keeps the invalid-tag trap.
- **Anonymous-struct scrutinee** — a linear chain of per-arm guard
  conjunctions: each literal-field guard is one field-equality compare
  (`icmp_imm eq` for `int`/`bool`, the runtime's bytewise `ryo_str_eq`
  for `str` — the M9.1 `emit_field_eq` convention), a failed compare
  short-circuits to the next arm's test block, and the final wildcard arm
  is the fallthrough.
- **Literal scrutinee** (`int` / `bool` / `str`) — the same chain shape
  over `Discriminant::Int/Bool/Str` arms: one compare plus `brif` per
  arm in written order, the first bare wildcard the fallthrough, and a
  defensive trap as the chain tail when no wildcard exists (reachable
  only for a violated E0129).

Arms run in their own block under the undo-log slot-table scope
(`emit_scoped_body`'s discipline — bindings are arm-local): arm-start
payload drops, then the binding prologue through the shared
`bind_field_move` (fat triples, struct/enum slot copies, scalar loads —
the M10 destructure logic), then the body. The post-merge drop of an
owned needs-drop scrutinee *temporary* needs no codegen of its own: the
ownership sweep schedules the whole-value Free anchored after the
`Match` inst, post-merge on every path, with `freed_at` guaranteeing one
emission.

## Deferred / known limitations

### Deferred to M12.5 (v0.2)

Nested patterns, guards, or-patterns, float-literal patterns, nominal
struct patterns, borrow-bindings with binding-mode inference, full
structural exhaustiveness for anon/tuple patterns (cross-arm
literal-field coverage — today a `_:` arm is always required), and a
bare irrefutable bind as a whole-arm pattern. See the roadmap's M12.5
deferral register.

### Deferred to M12.6 (v0.2) — match ownership completion

Mutable (`inout`) payload bindings, scrutinee liveness/reassignment,
conditional-move merges, nested-place scrutinees. Additionally, the M12
ownership review carried eight items into M12.6's register (see the
roadmap): never-read by-move bind in a continue-ending arm can leak
(narrow); `collect_jump_path` sibling-arm over-approximation on
returns-in-arms (leak; needs core-TIR headroom); non-`Var` moved
scrutinee reports UAM twice; copy-bind shadowing leak (same convention
as M10); continue-ending arms and match-inside-loop interplay lack
committed tests (break-in-arm coverage — statement-level and
value-position — has landed: ownership unit tests + ASan/Valgrind
sanitizer mirrors); four ownership walkers — `outermost_branch_of` and
`enclosing_if_main_cond` (loops.rs), `collect_bound_call_sites`
(frees.rs), and the `is_exit_anchor` predicate (mod.rs) — have no
`Match` arm at all, so they miss bare matches, not just
`ExprStmt`-wrapped ones; a `break` in a cond/bounds-position match arm
gets no loop-exit Frees yet (the loop-exit walk skips operand descent
at loop statements to avoid the stale outer context; exotic shape);
and `branch_may_not_return` has a `Match` arm but no operand descent
(pre-existing, backstopped by the return epilogue). I-210's consume fix
(above) is also queued there, as is the view-result merge (E0137's
deferral — a match whose value arms all yield one view type is rejected
at compile time until the merge shape exists).

### Deferred to M13 / M16

Matching over error unions (the `catch` arms of §7.4) ships inside M13;
`?T` / `none` patterns ship inside M16. M12's machinery is the substrate
for both.

### Known limitations shipped with M12

- **Parser:** `match` in call-argument position requires the closing
  `)` on its own line — the indent preprocessor emits dedents only at
  line boundaries, so a `match` block cannot close mid-line.
- **Anon scrutinees** dispatch as Int/Bool/Str field-equality chains
  (above); deeper structure waits for M12.5.
- **Diagnostics anchor:** the UIR match wire carries no per-pattern or
  per-arm spans, so pattern-level diagnostics (E0132/W0005) anchor on
  the arm body span via one re-anchor function in `sema/match_.rs`. A
  future wire revision carrying pattern spans re-anchors there.
- **Errored arm after a wildcard** gets no W0005 (deliberate
  no-double-report, see above); M12.5's wildcard-precision work should
  revisit knowingly.
- **View-typed match results** are rejected with E0137 (above) — no
  merge shape exists for a projection result until the M12.6 carry-over
  lifts it with the I-210 consume.
- The UIR `ir --emit` dump renders `Match` as `<malformed>` until a dump
  arm lands.

## References

- Spec: [Section 4.6](../specification.md) (enum type & `match`), [Section 7.4](../specification.md) (`catch` — error-union matching)
- Dev: `pipeline_alignment.md`
- Milestone: M12 — see `implementation_roadmap.md` (deferral registers: M12.5, M12.6; consumers: M13, M16)
- Issue: I-210 (`ISSUES.md`) — aggregate match value arms pending the ownership consume fix
