# Alpha Scope (v0.0.x)

**Status:** Implementation (v0.0.x alpha)

The alpha is a **delivery slice** of the v0.1.0 plan, not a separate language. It is the smallest subset of features that makes the language usable enough for external evaluation. Everything in alpha is also in v0.1.0; v0.1.0 adds more.

This document is the canonical scope definition for the alpha release. The full milestone list lives in [implementation_roadmap.md](implementation_roadmap.md); milestones included in alpha are tagged `[alpha]` there.

---

## Definition

> **Alpha is shipped when the code snippet on `landing/index.html` compiles and runs with the expected output.**

That snippet exercises the core surface area Ryo promises in its public branding. Anything not required by the snippet (or by basic program ergonomics around it) is deferred to beta (`v0.1.0`).

This is a falsifiable acceptance criterion: either the snippet runs or it doesn't. No subjective "feels alpha-quality" judgment.

---

## Acceptance Criterion (Litmus Test)

The following Ryo program must compile via `ryo build` and print the expected output when run:

```ryo
# Errors as values · optionals · safe by default

error NotFound

fn find_user(id: int) -> NotFound!str:
	if id == 0:
		return NotFound
	return f"user_{id}"

fn main():
	greeting = "Welcome to Ryo"

	maybe: ?str = "Alice"
	name = maybe orelse "guest"

	user = find_user(42) catch |err|:
		match err:
			NotFound: "unknown"

	print(f"{greeting}, {name} → {user}")
```

**Expected output:**
```text
Welcome to Ryo, Alice → user_42
```

A second test program covers the negative path (`find_user(0)` returns `NotFound`, `catch` recovers with `"unknown"`). Additional regression programs live under `tests/alpha/` (one file per scenario). Alpha ships when **all** test programs pass on macOS and Linux.

---

## Included Milestones

Cross-reference: [implementation_roadmap.md](implementation_roadmap.md). Alpha-included milestones are tagged `[alpha]` inline.

### Already complete

| Milestone | Status |
|---|---|
| M1 — Setup & Lexer Basics | ✅ Complete |
| M2 — Parser & AST Basics | ✅ Complete |
| M3 — "Hello, Exit Code!" (Cranelift Integration) | ✅ Complete |
| M3.5 — "Hello, World!" (String Literals & Print) | ✅ Complete |
| M4 — Functions & Calls | ✅ Complete |
| M6.5 — Booleans & Equality | ✅ Complete |

### Remaining for alpha

| Milestone | Why required |
|---|---|
| **M7** — Expressions & Operators (Extended) | Comparisons in `if id == 0` |
| **M8** — Control Flow & Booleans | `if` / `elif` / `else`, `while` |
| **M8.1** — Heap-Allocated `str` & Move Semantics | `f"user_{id}"` allocates owned strings |
| **M8.2** — Immutable Borrows (`&T`) | Function parameters that read but don't take ownership |
| **M8.3** — Mutable Borrows (`&mut T`) | Required for any non-trivial helper functions; without it, alpha programs cannot mutate through references |
| **M8.4** — String Slices (`&str`) | `print(s: &str)` and other str-borrowing builtins |
| **M11** — Enums (Algebraic Data Types) | `error NotFound` lowers to a unit enum variant |
| **M12** — Pattern Matching | `match err: NotFound: "unknown"` |
| **M13** — Error Types & Unions | `NotFound!str`, `catch \|err\|:` |
| **M16** — Optional Types (`?T`) | `?str`, `none`, `orelse` |
| **f-string interpolation** (extension of M3.5) | `f"user_{id}"`, `f"{greeting}, {name} → {user}"` |

### Partial milestones (alpha needs subset only)

These are large milestones in the v0.1.0 roadmap; alpha ships only the slice required by the litmus test plus minimum ergonomics.

| Milestone | Alpha subset | Beta adds |
|---|---|---|
| **M24** — Standard Library Core | `print`, `arg(i: int) -> ?str`, `arg_count() -> int`, `read_line() -> ?str`, `int.parse(s: &str) -> ParseError!int` | Full stdlib (collections, file I/O, time, etc.) |
| **M25** — Panic & Debugging Support | `panic(msg: &str)`, `assert(cond: bool, msg: &str)` | Pretty stack traces, debug formatting |
| **M26.5** — Distribution & Installer | macOS + Linux only; `curl ... \| sh` works; `ryo build`, `ryo run` work | Windows installer, `ryo upgrade`, signed binaries |

---

## Bonus Inclusions (Not in the Snippet, But Required for Useful Alpha)

The snippet alone doesn't make alpha useful — users need basic I/O to write small interactive programs. These items roll into the partial milestones above:

| API | Purpose | Folds into |
|---|---|---|
| `arg(i: int) -> ?str` | Read CLI argument by index | M24 partial |
| `arg_count() -> int` | Count of CLI arguments | M24 partial |
| `read_line() -> ?str` | Read one line from stdin (`none` on EOF) | M24 partial |
| `int.parse(s: &str) -> ParseError!int` | Parse integer from string | M24 partial |
| `assert(cond: bool, msg: &str)` | Aborts with message on `false` | M25 partial |
| `panic(msg: &str)` | Aborts with message | M25 partial |

**Note on collections:** Alpha intentionally has *no* user-facing list, map, or array types. The CLI argv API uses indexed access (`arg(i)`) instead of iteration over a list. This keeps M21/M22 entirely out of alpha scope and forces ownership ergonomics to be validated on simpler data first. Users who want to iterate can use recursion + integer indices.

---

## Explicit Non-Goals (Deferred to Beta v0.1.0)

These are documented in the roadmap as v0.1.0 milestones but are **not** part of alpha:

| Feature | Roadmap | Why deferred from alpha |
|---|---|---|
| Module system | M5, M6 | Single-file programs are sufficient for alpha-scale evaluation |
| Default parameters & named arguments | M8.5 | Pure ergonomics; positional args work |
| Structs | M9 | Snippet uses no structs; tuples + enums cover early needs |
| Tuples | M10 | Not in snippet |
| Methods (`impl` blocks) | M17 | Free functions cover all alpha use cases |
| Array slices `&[T]` | M21 | Depends on collections |
| Collections (`list`, `map`) | M22 | Largest deferral; see "Note on collections" above |
| RAII & `Drop` | M23 | No alpha types own non-memory resources |
| Testing framework (`#[test]`) | M26 | Use `assert` + `if cond: panic("...")` for alpha self-tests |
| Documentation generator (`ryo doc`) | M26 | Markdown + manual examples are sufficient |
| Pretty error messages with snippets | M27 | Plain "error at line N: ..." is enough for alpha feedback |
| Windows support | M26.5 | macOS + Linux only |
| Generics, traits, FFI, REPL, concurrency | Phase 5 | All v0.2+ in the existing roadmap |

---

## Release Tagging

Use semver pre-release suffixes so alpha doesn't burn version numbers needed for beta and stable:

| Tag | Meaning |
|---|---|
| `0.0.1-alpha.1` | First alpha. Litmus test passes; nothing else guaranteed. |
| `0.0.1-alpha.2`, `0.0.1-alpha.3`, … | Bug-fix re-rolls. |
| `0.0.2-alpha.1` | Next alpha if scope expands (e.g., adding structs after community feedback). |
| `0.1.0` | Beta. Full v0.1.0 spec column from `specification.md`. |
| `1.0.0` | Stable. Backwards-compatibility commitment begins. |

---

## Pre-Release Discipline

Three rules for keeping alpha tractable:

1. **No spec changes for alpha-included features once implementation begins.** Once M11 (enums) lands, alpha-blocking enum syntax is frozen until `0.0.1-alpha.1` ships. Design iteration during implementation is the #1 cause of compiler projects stalling.
2. **Pre-announce the gaps.** The alpha announcement must list "Not yet: collections, modules, methods, concurrency, REPL, Windows" prominently. Setting expectations early is free; resetting them later costs trust.
3. **Installer is alpha-blocking.** If users can't `curl ... | sh` and have `ryo run hello.ryo` work in 30 seconds, the alpha doesn't exist from their perspective. M26.5 (partial) is non-negotiable. Linux ARM too.

---

## Release Programs (`tests/alpha/`)

These programs gate the alpha release. Each must compile and run with the expected output:

| File | What it exercises |
|---|---|
| `landing.ryo` | The exact `landing/index.html` snippet |
| `landing_negative.ryo` | Same as above with `find_user(0)` (exercises the catch path) |
| `fizzbuzz.ryo` | Loops + conditionals + integer math |
| `factorial.ryo` | Recursion + integers |
| `string_reverse.ryo` | String manipulation + ownership |
| `guessing_game.ryo` | `read_line` + `int.parse` + error handling + control flow |
| `assert_works.ryo` | `assert(true)` passes; `assert(false, "msg")` aborts with message |

Alpha is released when `ryo build && ./<program>` succeeds on macOS and Linux for every file above.

---

## Estimated Timeline

Based on the roadmap's stated cadence (2–3 weeks per medium milestone at ~8 hr/week):

- **Remaining alpha milestones:** M7, M8, M8.1, M8.2, M8.3, M8.4, M11, M12, M13, M16 plus partial M24/M25/M26.5
- **Effort:** ~10 milestones × 2.5 weeks ≈ **25 weeks** of evening work
- **Calendar estimate:** **5–6 months** to alpha at current pace, assuming no design re-litigation

This is achievable. The scope is deliberately narrow precisely because shipping alpha matters more than making alpha comprehensive.

---

## References

- Spec: `docs/specification.md` §1.1 (Feature Availability table — alpha is a strict subset of the v0.1 column)
- Spec: `docs/specification.md` §4.8 (Optional Types), §4.9 (Error Types) — both required for the litmus test
- Dev: `docs/dev/implementation_roadmap.md` — full milestone list; alpha milestones tagged `[alpha]`
- Milestone: `docs/dev/implementation_roadmap.md` — alpha milestones tagged `[alpha]`
- Landing: `landing/index.html` — source of the litmus test snippet
