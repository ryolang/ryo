# Milestone 7 — Float, Ordering, and Modulo: Design Notes

**Status:** Implemented (2026-04-29).
**Plan:** [`docs/superpowers/plans/2026-04-29-milestone-7-float-and-extended-ops.md`](../plans/2026-04-29-milestone-7-float-and-extended-ops.md).
**Roadmap entry:** [`docs/dev/implementation_roadmap.md`](../../dev/implementation_roadmap.md) — *Milestone 7: Expressions & Operators (Extended)*.

## Summary

M7 closes the numeric-primitives gap that M6.5 left open: it introduces the
`float` type, IEEE-754 float literals, the ordering operators `<` / `>` / `<=`
/ `>=`, and the modulo operator `%`. Integer division (`/`) was already in the
language; M7 formalizes its semantics as truncation toward zero and adds the
matching signed-remainder operator. The work threads through every existing
compiler stage (lexer → parser → UIR → astgen → sema → TIR → codegen) without
restructuring any of them.

## Motivation

M7 is the last numeric milestone before the M8 control-flow work. The
ordering operators in particular gate `if a < b:` and `while i < n:`, so M8
cannot land until M7 makes those comparisons type-check and lower correctly.
Floats (and `%`) are bundled in here rather than split out because they share
the same per-stage extension pattern that M6.5 used for booleans and equality
— landing them together keeps the per-stage diff small and predictable.

## Scope

### In scope

- A single `float` primitive type, displayed as `float`. Lowers to Cranelift `F64`.
- Float literals of the form `[0-9]+\.[0-9]+` (e.g. `3.14`, `2.5`).
- Ordering operators `<`, `>`, `<=`, `>=`, non-associative, between additive
  and equality precedence.
- The modulo operator `%` at multiplicative precedence (same level as `*`, `/`).
- Type-checked numeric arithmetic on `float`: `+`, `-`, `*`, `/`.
- Equality (`==`, `!=`) extended to `float` with ordered comparisons (NaN
  matches Rust's `PartialEq<f64>`).
- Localized diagnostics for unsupported combinations (mixed `int`/`float`,
  ordering on `bool`/`str`, `%` on `float`).

### Out of scope (deferred)

- `float32` / `float64` distinction. Single `float` type for now.
- Mixed-type promotions (`int + float`). Rejected at sema; revisit alongside
  conversion functions in a later milestone.
- Conversion intrinsics (`int(x)` / `float(x)`). Deferred to the stdlib milestone.
- Richer literal grammar: bare `.5`, trailing `5.`, exponent `1e10`,
  underscore separators.
- Float modulo (`%` on `float`). Rejected at sema; reconsidered later if
  there is a concrete user need.
- Runtime checks for integer `/ 0` and `% 0`. Same status as M6.5; tracked
  as a stdlib / safety follow-up.
- `print(float)` and any other observability beyond exit codes — the
  formatter machinery lands with the stdlib work.

## User-visible behavior

These programs compile and run after M7:

```ryo
fn main() -> int:
	x: float = 3.14
	y: float = 2.71
	pi_approx = x + y / 2.0   # float arithmetic
	cmp = x > y               # ordering on float, result is bool

	a = 10
	b = 3
	q = a / b                 # 3 (integer division, truncates toward zero)
	r = a % b                 # 1 (signed remainder, sign of dividend)
	in_range = q < a          # ordering on int, result is bool

	return r
```

These programs are rejected with localized diagnostics:

| Source | Diagnostic |
|---|---|
| `x = 1 + 2.0` | `TypeMismatch`: "type mismatch in '+': left is 'int', right is 'float'" |
| `x = 1 < 2.0` | `TypeMismatch` (same shape, on `'<'`) |
| `x = 1.0 % 2.0` | `UnsupportedOperator`: "modulo operator '%' not supported for type 'float'" |
| `x = true < false` | `UnsupportedOperator`: "ordering operator '<' not supported for type 'bool'" |
| `x = "a" < "b"` | `UnsupportedOperator`: "ordering operator '<' not supported for type 'str' (yet)" |
| `x = a < b < c` | parse error (ordering is non-associative, like equality) |

## Type-system rules

- Same-type operands required for all binary operators; sema's
  `pool.compatible(lhs, rhs)` check (which absorbs the error sentinel) gates
  the per-operator dispatch.
- Arithmetic (`+`, `-`, `*`, `/`) accepts `int` (lowers to `I*` TIR tags) and
  `float` (lowers to `F*` TIR tags). Result type matches the operand type.
- Modulo (`%`) accepts `int` only (lowers to `IMod` → Cranelift `srem`).
- Ordering (`<`, `>`, `<=`, `>=`) accepts `int` and `float`; result is
  always `bool`. `int` lowers to signed `icmp`; `float` lowers to ordered
  `fcmp`.
- Equality (`==`, `!=`) — already covered by M6.5 — gains a `float` branch
  in sema that lowers to `FCmpEq` / `FCmpNe`.
- Ordering on `bool` and `str` is rejected. `str` carries the `(yet)` suffix
  to signal future support; `bool` does not (no plans to compare booleans
  ordinally).

## Codegen mapping

| TIR tag | Cranelift instruction |
|---|---|
| `FloatConst` | `f64const(v)` |
| `FAdd` / `FSub` / `FMul` / `FDiv` | `fadd` / `fsub` / `fmul` / `fdiv` |
| `IMod` | `srem(lhs, rhs)` |
| `ICmpLt` / `ICmpLe` / `ICmpGt` / `ICmpGe` | `icmp(IntCC::Signed{LessThan,LessThanOrEqual,GreaterThan,GreaterThanOrEqual}, …)` |
| `FCmpEq` / `FCmpNe` / `FCmpLt` / `FCmpLe` / `FCmpGt` / `FCmpGe` | `fcmp(FloatCC::{Equal,NotEqual,LessThan,LessThanOrEqual,GreaterThan,GreaterThanOrEqual}, …)` |

`FloatCC` uses the **ordered** comparison family. NaN compared with anything
yields `false` for `==`/`<`/`<=`/`>`/`>=` and `true` for `!=`, matching
Rust's `PartialOrd<f64>`. This is the choice users will most often expect; if
unordered semantics are ever needed they can be added as new TIR tags
without changing the existing ones.

`TypeKind::Float` maps to `types::F64` in `cranelift_type_for`. Float values
flow through Cranelift `Variable`s the same way `int` and `bool` already do
— no new ABI wrinkle.

## Why these semantics (and not the alternatives)

- **Single `float` type, not `float32` / `float64`.** Phase-1 numeric tower
  lands `int` as `i64` only; floats follow the same shape until a real need
  for explicit widths shows up. A future widening to `float32` is a new
  `Tag::Float32` variant alongside the existing `Tag::Float` (which becomes
  `Float64`); none of the surface syntax has to move.
- **Mixed-type rejection over implicit promotion.** Implicit `int → float`
  promotion would interact badly with `==`-as-`bool`, with future `usize` /
  `isize`, and with comptime evaluation. Zig's experience is that explicit
  conversions cost users almost nothing and remove a whole class of bugs.
- **`srem` for `%` (sign of dividend).** Matches C, Rust, Go. The
  alternative — Python-style "sign of divisor" — would force a sign-fixup
  sequence at every `%` and would surprise users coming from any of the
  cousin languages.
- **Ordered float comparisons.** NaN-makes-everything-false matches Rust;
  unordered comparisons would silently change the meaning of `if x > y:`
  on NaN inputs.
- **Float `%` rejected, not silently `fmod`.** `fmod` has surprising
  semantics on negatives and on NaN, and there is no concrete user demand
  yet. Better to leave the operator unbound than to ship a behavior we
  might want to change.

## Test plan (what landed)

- **Unit tests** at every layer — types, lexer, ast, parser, uir, astgen,
  tir, sema — exercising both happy paths (literal round-trip, builder
  whitelist, sema lowering) and rejections (mixed types, float `%`, ordering
  on `bool`).
- **Two integration tests** in `tests/integration_tests.rs`:
  - `float_program_compiles_and_runs` — float literal, float arithmetic,
    float ordering, exit 0.
  - `integer_division_and_modulo_compile_and_run` — integer `/` and `%`,
    integer ordering, exit code = `10 % 3` = 1.

## Reversibility

Each task in the plan ends with a single-purpose commit. Reverting any
later task leaves earlier tasks consistent: e.g., reverting Task 9
(codegen) leaves the language parsing and type-checking new syntax with no
codegen — equivalent to a feature-flag-off state. Reverting all ten tasks
restores the M6.5 baseline byte-for-byte.
