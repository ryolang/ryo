# Known Issues

Compiler issues identified during source review. Each entry is independently actionable; severity reflects impact on correctness, future feature work, or code health — not user impact today (the compiler is pre-alpha).

Resolved entries are removed (not kept around as a changelog). Look at `git log` if you need history.

---

## Severity Legend

- 🔴 **Blocking** — prevents implementing roadmap features as currently designed.
- 🟡 **Correctness/Hygiene** — silent bug or invariant gap; works today, will bite later.
- 🟢 **Cleanup** — code health, ergonomics, minor.

---

## 🟡 Correctness / Hygiene

### I-006 — `print` is special-cased in codegen

**Files:** `ryo-backend/src/codegen.rs` (`generate_print_call`), `ryo-frontend/src/sema.rs` (`check_builtin_call`)
**Summary:** Codegen emits a raw `write(2)` syscall wrapper inline for the `print` builtin, and sema has a builtin-specific validator hook to match. Consequences:

- Rejects `print(some_var)` even when the variable is a string.
- No formatting, no automatic newline.
- Already stubbed out on Windows (`return Err(...)`).
- Mixes runtime concerns into the compiler.
**Resolution:** Move `print` to a runtime crate (`ryort` or similar) compiled to an object file and linked in via `zig cc`. Codegen emits a normal call; `sema::check_builtin_call` goes away.

### I-010 — Unused `_bytes_written` from `write(2)` call

**Files:** `ryo-backend/src/codegen.rs` (`generate_print_call`)
**Summary:** The result of the `write` syscall is fetched and ignored. A short write or `EINTR` will silently truncate output. Acceptable for a bootstrap, but should be documented or fixed when `print` moves to the runtime (I-006).
**Resolution:** Tracked under I-006.

### I-014 — Lexer errors bypass `DiagSink`

**Files:** `ryo-frontend/src/lexer.rs` (`LexError`), `ryo-driver/src/pipeline.rs` (`parse_source`, `display_tokens`)
**Summary:** Phase 1 of `docs/dev/pipeline_alignment.md` routes parse / ast_lower / sema errors through a `DiagSink` so analysis can continue past the first failure. The lexer was on a parallel branch at the time and still uses a single-shot `Result<_, LexError>`. The driver wraps a `LexError` into a one-element `Vec<Diag>` at the boundary, but problems like "invalid integer literal" or "unknown escape sequence" can't co-surface with a sema error in the same run.
**Resolution:** Thread `&mut DiagSink` into `lexer::lex` and `intern_token`. Replace the early return on the first lex error with `sink.emit(...)` followed by a recovery token (e.g. `Token::Error` for the bad span) so the parser still sees a well-formed stream. Eliminate `LexError` once the migration is complete.

### I-015 — Unknown escape sequences silently preserved

**Files:** `ryo-frontend/src/lexer.rs` (`unescape`)
**Summary:** When `unescape` encounters `\q` (or any other character not in the small known table) it preserves the backslash and the character verbatim instead of reporting a diagnostic. The user gets no feedback that the escape sequence is unrecognised, and the runtime string contains the literal `\q` bytes. Tracked as a TODO at the function definition.
**Resolution:** Folded in with I-014: once `lexer::lex` has a `&mut DiagSink`, the `Some(c)` arm of `unescape` emits a structured `Diag::error(span, DiagCode::UnknownEscape, …)` and proceeds with the raw character (or skips both bytes — TBD by spec discussion).

### I-016 — Indent processor errors carry no span

**Files:** `ryo-frontend/src/indent.rs` (`process`), `ryo-frontend/src/lexer.rs` (`lex` fallback)
**Summary:** `indent::process` returns `Result<_, String>` with a free-form message and no source location. The driver currently fakes a span by reusing the last raw token's span. That's "near" the offending newline but not on it; for "spaces are not allowed" / "dedent doesn't match an outer level" the user wants the squiggle on the indentation itself.
**Resolution:** Have `indent::process` return a richer error type carrying the offending span (the `Newline(s)` token whose whitespace failed validation, or the `Dedent` insertion point) and propagate it into `LexError.span`. The TODO at the lexer fallback covers this from the consumer side.

### I-017 — `i64::MIN` integer literal is unrepresentable

**Files:** `ryo-frontend/src/lexer.rs` (`RawToken::Int` arm)
**Summary:** Integer literals are parsed as `i64` at lex time, then sign is applied later via the unary `-` operator. That makes `-9_223_372_036_854_775_808` (i.e. `i64::MIN`) unspellable: the positive form `9_223_372_036_854_775_808` overflows `i64`. Hits the negation-overflow corner Rust itself fixed via `IntLit` / `IntLitMin` token-level distinction.
**Resolution:** Either parse as `u64` and resolve negation+overflow at sema time, or add an `IntLitMin` token variant that the parser recognises only as the operand of unary `-`. Coordinate with the broader numeric-tower design before picking either.

### I-020 — `inst_values` memoizer is cross-block

**Files:** `ryo-backend/src/codegen.rs` (`inst_values: HashMap<TirRef, Value>`)
**Summary:** Codegen lazily memoizes Cranelift `Value`s keyed by `TirRef` in a single flat HashMap shared across all basic blocks within a function. This is sound today because: (a) TIR instructions are unique per use (no shared sub-expressions), (b) BoolAnd/BoolOr use block params (phi nodes) so the memoized result is the merge-block param which dominates downstream uses, and (c) IfStmt is statement-level so no values flow out of branches. However, if future features introduce expression-level if (ternary) or shared sub-expressions across blocks, the memoizer will produce Cranelift dominator errors.
**Resolution:** Scope the memoizer to per-block when expression-level control flow lands. For now the cross-block cache is correct given the TIR invariants.

### I-031 — No return-flow analysis for if/elif/else

**Files:** `ryo-frontend/src/sema.rs`, `ryo-backend/src/codegen.rs`
**Summary:** Sema does not track whether all paths through an if/elif/else return a value. The codegen `emit_stmt` returns `all_branches_return` correctly, but sema has no equivalent — a function with `if/else` where all branches return still gets a "missing return" warning from the implicit `ReturnVoid` appended by the codegen fallthrough path. Currently papered over because void-returning `main` doesn't need a return, and non-void functions with complete coverage happen to work because codegen skips the merge block when all branches return.
**Resolution:** Add `block_definitely_returns` analysis in sema so that functions with exhaustive if/else returns don't get spurious diagnostics. This becomes necessary when `while` loops and `match` land.

### I-032 — IfStmt is statement-only, no expression-level conditional

**Files:** `ryo-core/src/ast.rs`, `ryo-frontend/src/parser.rs`, `ryo-frontend/src/sema.rs`, `ryo-backend/src/codegen.rs`
**Summary:** `if`/`elif`/`else` is a statement (`StmtKind::IfStmt`), not an expression. There is no way to write `x = if cond: a else: b` (ternary/conditional expression). The spec envisions `if` as an expression in certain contexts. Current codegen emits void for IfStmt and uses no phi-merge for values across branches.
**Resolution:** Add `ExprKind::IfExpr` when the spec finalizes expression-if syntax. Codegen would use block params (like BoolAnd/BoolOr already do) to merge values at the join point. Requires I-020 attention for memoizer correctness.

### I-033 — Variables declared inside if/elif/else branches are not visible after the statement

**Files:** `ryo-frontend/src/sema.rs` (`analyze_block`)
**Summary:** Each branch of an if/elif/else creates a child scope. Variables declared inside a branch are dropped when the branch scope ends. There is no "variable promotion" — even if all branches declare `x: int`, `x` is not available after the if statement. This is the correct scoping semantics for now, but may surprise users expecting Python-style scoping where if-branches don't create a new scope.
**Resolution:** This is intentional for M8b. If user feedback requests Python-style flat scoping, revisit as a language design decision (requires approval per CLAUDE.md escalation rules).

### I-011 — Manual error enum where `thiserror` would suffice

**Files:** `ryo-core/src/errors.rs` (33 lines)
**Summary:** Hand-rolled `enum CompilerError` with manual `Display` and `From<io::Error>` impls. `thiserror` would cut ~20 lines and make variants more uniform.
**Resolution:** Add `thiserror`, derive `Error` and `Display`, drop the hand-written impls.

### I-012 — `pretty_print` lives on AST nodes

**Files:** `ryo-core/src/ast.rs`
**Summary:** Presentation logic (tree-drawing, prefixes) is mixed into the AST data types. Convenient now, painful as the AST grows. A separate `pretty` module — or `Debug`-derived JSON output for `--parse` — scales better and keeps `ast.rs` focused on data.
**Resolution:** Extract into `ryo-core/src/ast_pretty.rs` (or similar) when the next AST node type is added.

### I-013 — `--emit` flag surface is fragmented across subcommands

**Files:** `ryo/src/main.rs`, `ryo-driver/src/pipeline.rs`
**Summary:** `lex`, `parse`, `ir` are separate subcommands. Each stage already exists and is wired up; users would benefit from a single `ryo build --emit=tokens|ast|hir|clif|obj` surface (mirroring `zig build-exe -femit-…`).
**Resolution:** Unify under one subcommand with an `--emit` flag. Keep current subcommands as deprecated aliases for one release.

### I-018 — `TypeId` is a newtype, not a typed enum

**Files:** `ryo-core/src/types.rs` (`TypeId`)
**Summary:** Phase 2 §2.2 of `docs/dev/pipeline_alignment.md` originally called for `TypeId` to become an `enum { Void = 0, Bool = 1, ..., Error = 4, Dynamic(NonZeroU32) }` so primitive matches are exhaustive at compile time and the `pool.int()` accessor disappears. The risk register allowed a fallback to a plain `Copy` newtype if the enum encoding fights the borrow checker, which is what we shipped. Cost: the `TypeKind::Tuple` arm we added in `cranelift_type_for` and a couple of sema sites are not statically guaranteed to be covered when a new primitive lands.
**Resolution:** Re-attempt the enum encoding using `repr(u32)` + `Dynamic(NonZeroU32)` once the borrow-checker pain points (mostly around `pool.kind` returning a value that contains a `TypeId`) are characterised. Low priority — the matches we have today still go through `TypeKind`, which *is* exhaustive, so the gap is small.

### I-019 — `tuple_elements_vec` allocates a `Vec` per call

**Files:** `ryo-core/src/types.rs` (`tuple_elements_vec`)
**Summary:** The accessor copies the element-id slice out of `extra` rather than returning a borrowed view, because `TypeId` is not `#[repr(transparent)]` over `u32` and the unsafe transmute to `&[TypeId]` would be UB without it. Today the function is called only by `Display` for diagnostics and by tests; not a hot path.
**Resolution:** Tag `TypeId` with `#[repr(transparent)]` and expose `tuple_elements(id) -> &[TypeId]` alongside the copying accessor. Migrate non-perf-critical callers to it lazily. Defer until tuple codegen lands and the accessor shows up in a profile.

### I-021 — `bool` lowered as `types::I8` will mis-ABI across FFI boundaries

**Files:** `ryo-backend/src/codegen.rs` (`cranelift_type_for`)
**Summary:** `TypeKind::Bool` maps to Cranelift `I8`. Fine for internal logic, but C ABIs typically pass `_Bool` zero/sign-extended to a full register (often i32 on SysV, register-width on Win64). Passing or returning our raw `I8` across an FFI call would leave the upper bits undefined from the callee's perspective.
**Resolution:** When FFI lands, insert explicit `uext` (zero-extension) on bool arguments at call sites and `ireduce` on bool returns, per the target ABI. Decide at the FFI design stage whether `bool` keeps its `I8` storage type and only widens at the boundary, or becomes register-width throughout. Latent until FFI exists.

### I-023 — Integer division / modulo by zero is undefined behavior at codegen

**Files:** `ryo-backend/src/codegen.rs` (`TirTag::ISDiv`, `TirTag::IMod` arms), `ryo-frontend/src/sema.rs` (`check_binary_op`)
**Summary:** `a / 0` and `a % 0` lower directly to Cranelift `sdiv` / `srem` with no guard. Cranelift treats both as UB; the resulting native instruction (`idiv` on x86-64, `sdiv` on aarch64) traps or returns garbage depending on target. Sema does not constant-fold a known-zero divisor either.
**Resolution:** When safety checks land, insert a runtime check at codegen: emit a compare-against-zero, branch to a trap / panic block on hit. Optionally constant-fold the obvious `x / 0` / `x % 0` cases at sema and emit a diagnostic. Coordinate with the eventual panic / safe-mode design so the trap path has somewhere to go.

### I-024 — Single `float` type, no `float32` / `float64` distinction

**Files:** `ryo-core/src/types.rs` (`Tag::Float`, `TypeKind::Float`), `ryo-backend/src/codegen.rs` (`cranelift_type_for`)
**Summary:** M7 ships one float type (`float`), lowered to Cranelift `F64`. Matches today's `int` (one width, machine-word). Users who need 32-bit floats for memory, GPU work, or C interop have no surface syntax to ask for one.
**Resolution:** Add `Tag::Float32` alongside the existing `Tag::Float` (which becomes `Float64` semantically), expose `: float32` / `: float64` annotations, and pick one as the default for unannotated `1.5`-style literals. Coordinate with the broader numeric-tower design (sized integers, `usize` / `isize`) so the widening story is consistent across types.

### I-025 — No implicit `int` ↔ `float` promotion or conversion functions

**Files:** `ryo-frontend/src/sema.rs` (`check_binary_op` mixed-type branch)
**Summary:** `1 + 2.0` is a hard `TypeMismatch` error; users must spell every conversion explicitly, but there are no conversion intrinsics yet either — `int(x)` and `float(x)` don't exist. The result is that mixed numeric arithmetic is currently *unspellable*. Acceptable today (no programs need it), but blocks any real numeric workload.
**Resolution:** Land conversion intrinsics first (`int(float) -> int`, `float(int) -> float`, with Cranelift `fcvt_to_sint_sat` / `fcvt_from_sint`). Decide at that point whether to keep mixed arithmetic as an error (Zig stance) or introduce limited widening (e.g. `int + float -> float` only when the int is a literal, Swift stance). Both are coherent; pick one and document.

### I-026 — Float modulo (`%` on `float`) rejected

**Files:** `ryo-frontend/src/sema.rs` (`check_binary_op` is_modulo branch)
**Summary:** `1.0 % 2.0` produces `"modulo operator '%' not supported for type 'float'"`. The plan deferred this because `fmod` has surprising semantics on negatives and on NaN, and there is no concrete user demand yet.
**Resolution:** When a real use case appears, decide between `libm::fmod` (C / IEEE remainder semantics) and a `frem`-style "sign of dividend" lowering, then add a `TirTag::FMod` and route `% on float` through it in sema. Document the chosen semantics in `docs/specification.md` before implementing.

### I-027 — Restricted float literal grammar

**Files:** `ryo-frontend/src/lexer.rs` (`RawToken::Float` regex `[0-9]+\.[0-9]+`)
**Summary:** Float literals must have digits on both sides of the dot. None of `.5`, `5.`, `1e10`, `1.5e-3`, `1_000_000.0` parse. Sufficient for M7's example programs but obviously incomplete.
**Resolution:** Extend the regex to cover `[0-9]+(_[0-9]+)*(\.[0-9]+(_[0-9]+)*)?([eE][+-]?[0-9]+)?` (or break it into named sub-patterns). Mirror the same underscore + exponent treatment for integer literals at the same time so the two grammars stay parallel. Watch out for ambiguities with method-call syntax (`5.bit_count()`) once methods land.

### I-028 — No `print(float)` (or `print` on anything but a `str` literal)

**Files:** `ryo-frontend/src/builtins.rs`, `ryo-frontend/src/sema.rs` (`check_builtin_call`), `ryo-backend/src/codegen.rs` (`generate_print_call`)
**Summary:** Float arithmetic has no observability beyond the program exit code. `print` is hard-wired to take a *string literal* (see I-006). Inspecting a float at runtime requires either a formatter (`f"{x:.2}"`) or polymorphic `print`, neither of which exists.
**Resolution:** Tracked under I-006 (move `print` to a runtime crate). The float-specific piece lands when the runtime crate gains `print_f64` (or a polymorphic dispatch) and the sema-side argument-kind whitelist accepts non-`StrLiteral` `float` arguments.

### I-029 — AST loses `Eq` because `Literal::Float` carries an `f64`

**Files:** `ryo-core/src/ast.rs` (`Literal`, `Expression`, `Statement`, `Program`, `StmtKind`, `ExprKind`, `VarDecl`, `FunctionDef`)
**Summary:** `Literal::Float(f64)` cannot derive `Eq` (NaN ≠ NaN), and `Eq` derivation propagates up the containment chain, so every AST struct that transitively holds a `Literal` had to drop the `Eq` derive. No consumer hashes or `Eq`-compares AST nodes today, so the change is currently invisible.
**Resolution:** If a future pass needs `HashMap<Expression, _>` or similar, introduce a `FloatBits(u64)` newtype that derives `Eq + Hash` on the bit pattern and *also* implements `PartialEq` with IEEE semantics. Wrap `f64` inside `Literal::Float` with it. Until then, leave the derives off.

### I-030 — Unused chumsky 0.12 ergonomics worth revisiting

**Files:** `ryo-frontend/src/parser.rs`, `ryo-driver/src/pipeline.rs`
**Summary:** chumsky 0.12 (released 2025-12-15) shipped several quality-of-life features. The 0.11 → 0.12 bump only adopted `Input::split_token_span` (replacing the `Stream::from_iter(...).map(eoi, |(t, s)| (t, s))` boilerplate at the lex/parse boundary). The remaining features are not currently a fit, but each becomes interesting as the parser grows:

- **`MapExtra::emit` / `InputRef::emit`** — emit *secondary* errors during mapping or in custom parsers without aborting the parse. Useful for soft-rejecting chained non-associative operators like `a < b < c` and `a == b == c` with a structured diagnostic, instead of the current "unexpected token" produced by the trailing operator falling off `or_not()`. Becomes attractive once parser diagnostics get their own `DiagCode` taxonomy (cf. I-014 for the lexer-side equivalent).
- **`spanned` combinator** — wraps a parser's output in `(O, Span)`. Today every `select! { ... }.map_with(|x, e| Foo::new(x, e.span()))` site builds the typed AST node directly, which is already one line; `spanned` would force an extra destructure. Worth reconsidering if/when the AST grows a uniform `Spanned<T>` wrapper instead of per-node `span` fields.
- **`labelled_with`** — label parsers without requiring `Clone` on the label value. Only relevant once the parser starts attaching labels for error-message quality; not used today.
- **`Parser::debug` (experimental)** — parser-level debugging utilities. Useful when triaging surprise grammar conflicts; pull in ad-hoc when needed, no permanent wiring required.
- **`IterParser::parse_iter` (experimental)**, **`nested_in` flexibility**, **`Input::split_spanned`** — no current call sites. `split_spanned` in particular is the `WrappingSpan`-flavoured sibling of `split_token_span`; we use plain `(Token, SimpleSpan)` tuples so the latter is the right fit.

**Resolution:** No action today. Revisit `MapExtra::emit` first when the parser gains structured diagnostics (likely alongside I-014's lexer-sink work, so parse and lex errors co-surface through the same `DiagSink`). Revisit `spanned` if the AST representation of spans is ever unified.

### I-034 — Builtin name comparison uses string compare instead of interned ID

**Files:** `ryo-frontend/src/sema.rs` (`check_call`, `check_builtin_call`)
**Summary:** `sema.pool.str(name_id) == "assert"` (and similar for `"panic"`, `"print"`) does a string dereference and byte comparison on every `check_call` invocation. Since the intern pool already deduplicates strings, comparing `name_id == assert_id` (where `assert_id` is cached once during builtin registration or sema init) would be a direct integer compare. Negligible today with three builtins and small programs, but the cost scales linearly with both the number of call sites and the number of builtins.
**Resolution:** Cache `StringId`s for each builtin name (e.g., in `Sema` or alongside `builtins::BUILTINS`) and match on the id instead of the string. Same applies to the codegen-side `name_str == "print"` comparisons.

### I-037 — Panic/Assert mechanism lacks `#file` / `#line` intrinsic expansion

**Files:** `ryo-frontend/src/sema.rs`, `ryo-backend/src/codegen.rs`
**Summary:** The `panic` implementation bakes the source location (line, column) directly into a unique formatted string literal per call site at compile time. If a user asserts in ten places, the binary interns ten distinct copies of the assertion string format.
**Resolution:** Add macro-style `#file` and `#line` intrinsics or special UIR nodes (e.g. `InstTag::FileLoc`) to sema/codegen. `__ryo_panic` can then take `line` and `col` as integer arguments and construct the format string dynamically via `libc` functions or standard runtime printing, sharing the user's message string across sites.

### I-038 — Assert checks cannot be stripped in Release mode

**Files:** `ryo-frontend/src/sema.rs`, `ryo-backend/src/codegen.rs`
**Summary:** Ryo has no mechanism to strip `assert` checks in `--release` configurations. The condition evaluates and branches at runtime unconditionally.
**Resolution:** Introduce a compilation mode flag (`--release` vs `--debug`) and strip `assert` AST/UIR nodes during semantic analysis when building for release. Provide a `precondition` or `fatal` variant that explicitly ignores the release flag for mandatory bounds checks.

### I-039 — `panic` provides no stack unwinding or stack traces

**Files:** `ryo-backend/src/codegen.rs` (`__ryo_panic`)
**Summary:** A panic terminates execution instantly (`exit(101)`) and prints only the line/col of the `panic()` or `assert()` call site. If a shared utility function calls `panic`, the user gets no traceback to the caller.
**Resolution:** Add DWARF debug info generation to Cranelift (`.debug_line`, `.debug_info`, `.debug_frame`). Implement a simple stack walker in the runtime (e.g., `backtrace` from `libc` or via DWARF frame unwinding) to print the call stack inside `__ryo_panic`.
**Note:** DWARF emission is the shared prerequisite. Once it lands, interactive debugging via DAP ([Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/)) comes nearly for free — lldb already speaks DAP, so VS Code / JetBrains attach without Ryo-specific work. The stack-trace feature in `__ryo_panic` is additive runtime work on top of that same DWARF foundation.

### I-040 — `for-range` arity: only 2-arg form supported

**Files:** `ryo-frontend/src/parser.rs` (for-range parser)
**Summary:** Python allows `range(stop)` (implied start=0) and `range(start, stop, step)`. Ryo's parser strictly enforces `range(start, end)` (exactly 2 arguments). This is documented v0.1 behaviour. Users coming from Python will inevitably try `for i in range(10):` and receive a generic arity error.
**Resolution:** Consider supporting `range(end)` as sugar for `range(0, end)` in a future milestone. The 3-arg `range(start, end, step)` form requires a more complex increment block in codegen. Both are additive and non-breaking.

### I-041 — `range` is a syntactic hack, not a function

**Files:** `ryo-frontend/src/builtins.rs`, `ryo-frontend/src/sema.rs`
**Summary:** `range(0, 5)` is hardcoded as a reserved keyword in semantic analysis rather than a standard library function. If a generic `for element in collection:` loop is implemented in the future, the `range` hardcoding will need to be removed in favor of a true `RangeIterator` protocol.
**Resolution:** Defer until Structs, Generics, and Iterator Interfaces are formally designed and implemented. Once they exist, remove the specific `range` semantic checks and transition it to a standard library function.

### I-042 — For loop codegen needs to be desugared into while loops

**Files:** `ryo-backend/src/codegen.rs`
**Summary:** Currently, `for-range` loops have bespoke code generation that manually emits basic blocks, jump instructions, and raw counter increments. When general iterators are added, loops should be desugared during the AST-to-UIR phase into standard `while` loops that call `.next()`.
**Resolution:** Once iterators land, remove the `generate_for_range` codegen entirely and rely on standard `while` codegen to emit loops.

### I-043 — Migrate `ryo-runtime` to `#![no_std]`

**Files:** `runtime/src/lib.rs`, `runtime/Cargo.toml`, `ryo-backend/src/linker.rs`
**Summary:** The runtime staticlib only uses `std::alloc`, `std::process::abort()`, and `eprintln!`, yet linking against precompiled `std` bundles objects with `_Unwind_*` symbol references. This forces the linker to pass `-lunwind` on Linux (workaround in `ryo-backend/src/linker.rs`). Migrating to `#![no_std]` with `extern crate alloc` eliminates the dependency entirely.
**Resolution:** Replace `std::alloc` with `alloc::alloc` (identical API). Replace `eprintln!` + `process::abort()` with `extern "C" { fn abort() -> !; }`. Add `#[panic_handler]` that aborts. Keep the `rlib` crate-type for `cargo test` via a `#[cfg(test)]` std gate. `ryu` already supports `no_std`. Benefits: smaller archive, faster link times, no hidden unwind dependency, simpler cross-compilation.

**Note:** `no_std` scopes only the core floor; OS-backed modules (`net`/`fs`/`time`) are linked on reachability, not forced into `no_std` — see `docs/dev/built_in.md` (`no_std` scope).

### I-045 — Loop fixed-point uses a scratch sink and re-walks the body to suppress duplicates

**Files:** `ryo-frontend/src/ownership.rs` (`analyze_while_loop`, `analyze_for_range`)
**Summary:** Both loop helpers walk the body once into a throwaway `DiagSink`, compare entry vs. post-body Moved-ness via `states_differ`, and then either replay the scratch diagnostics (converged) or re-walk the body from the merged state against the real sink (didn't converge). Diagnostic output is implicitly a function of *which iteration emitted it*, not of the converged lattice. Today this happens to work because the M8.1 patterns converge in ≤2 iterations, but the body-twice-with-discard shape couples diagnostic emission to control-flow analysis instead of running checks against a fixed-point.
**Resolution:** Refactor to a propagate-only first pass (state mutations only, no diagnostics), iterate to fixed-point, then a single check pass at the converged lattice that emits diagnostics. Removes the scratch sink entirely and makes the loop helpers symmetric with `analyze_if_stmt`.

### I-047 — UIR `is_move` field is a pass-through

**Files:** `ryo-core/src/uir.rs` (`UirParam`), `ryo-frontend/src/astgen.rs`, `ryo-frontend/src/sema.rs`
**Summary:** `is_move` is threaded lexer → parser → AST → UIR → TIR. The UIR copy is never read: astgen propagates the AST flag in, sema reads it back out into `TirParam`, and no UIR pass inspects it. UIR is structural lowering with no semantic meaning, so `UirParam::is_move` is dead weight that exists only to bridge two layers it shouldn't.
**Resolution:** Drop `UirParam::is_move`. Sema can read the flag straight from the AST `FuncBody` (or via a side-channel keyed by FuncBody) when it constructs `TirParam`. Wait until any other UIR-level pass needs the flag before re-introducing it.

### I-053 — `OwnerState::Borrowed` is currently parameter-only

**Files:** `ryo-frontend/src/ownership.rs` (`OwnerState`)
**Summary:** `OwnerState::Borrowed` is set only at parameter init; no expression produces it. The two sites that read it (E0021, E0022) could equivalently look up `tir.params` for the underlying owner's source param and check `is_move`. The state is anticipating real borrow expressions (`&x`) which the spec migrated to in commit 2ccf6b6 but the compiler doesn't lower yet.
**Resolution:** Document the invariant inline ("only ever set at param init in M8.1b; transitions arrive when `&x` borrow expressions land in a future milestone"). No code change today.

### I-054 — `parse_source` and lex error paths bypass `finalize_diags`

**Files:** `ryo-driver/src/pipeline.rs` (`parse_source`, `display_tokens`)
**Summary:** `finalize_diags` consolidates the drain + render + `Err(CompilerError::Diagnostics(_))` shape for the sink-using stages (`lower_and_analyze`, `ir_command`). The lex error paths and `parse_source`'s parse-error branch still hand-roll the same pattern over a `Vec<Diag>` they build directly (no `DiagSink`). Drift risk is real: parse/lex paths skip the `Severity::Error` filter (they assume every diag they emit is an error, which holds today but isn't enforced), and any future change to the rendering convention has to be applied in three places.
**Resolution:** Generalize `finalize_diags` to take `Vec<Diag>` (or `impl IntoIterator<Item = Diag>`); have `DiagSink::into_diags()` feed the new entry point. Then the three lex/parse error paths become `finalize_diags(vec![diag], input, &name)` and the render+wrap pattern lives in exactly one place. Folds naturally with I-014's lexer-DiagSink migration.

### I-064 — `inside_loop` / `has_any` reachability sets recomputed per jump

**Files:** `ryo-frontend/src/ownership.rs` (`schedule_break_continue_frees`, `schedule_loop_exit_frees_in`)
**Summary:** `schedule_break_continue_frees` rebuilds the `inside_loop` HashSet via `collect_loop_body_refs` on every break/continue jump. `has_any` (which depends only on `sidecar.free_schedule`) is also rebuilt per jump. For a loop with K jumps that's K full traversals of the same loop body and K full scans of `free_schedule`. The same data is invariant across jumps in the same loop. Today's fixtures hit ≤1 jump per loop so the cost is invisible, but `match` and labelled break expand the jump count and the asymmetry will become visible.
**Resolution:** Lift `inside_loop` and `has_any` to per-loop scope inside `schedule_loop_exit_frees_in`'s `WhileLoop`/`ForRange` arm; pass them down by reference into the per-jump call. `covers_this_jump` and `on_path` stay per-jump (they depend on `jump_inst`).

### I-065 — `collect_jump_path` builds throwaway HashSets per arm for containment

**Files:** `ryo-frontend/src/ownership.rs` (`collect_jump_path`)
**Summary:** For each arm of an `IfStmt` containing the target, the helper builds a fresh `HashSet<TirRef>` via full `collect_refs_recursive` walk just to test `sub.contains(&target)`. The matched arm is then walked AGAIN by the recursive `collect_jump_path` call to populate `set` for real. The non-matching arm's full subtree gets walked into a throwaway set to confirm the jump isn't there. For an N-arm if where the jump is in arm K, the prefix is walked twice; for an N-stmt body where the jump is in stmt N, the prefix gets walked into per-stmt throwaway sets. Worst-case O(N²) in body size.
**Resolution:** Two options that compose:

- (a) Replace the per-arm containment probe with a short-circuiting `contains_recursive(tir, root, target) -> bool` that DFS-returns at first hit (no allocation).
- (b) Precompute, alongside `inside_loop` (see I-064), a `HashMap<TirRef, TirRef>` from each ref to its enclosing top-level loop-body stmt. `collect_jump_path` then does a single lookup to find the chosen arm's containing stmt — no per-stmt walks.
Option (b) composes naturally with I-064's per-loop precomputation.

### I-066 — TIR reachability helpers belong next to `walk_operands` in `ryo-core/src/tir.rs`

**Files:** `ryo-frontend/src/ownership.rs` (`collect_loop_body_refs`, `collect_refs_recursive`, `collect_jump_path`); `ryo-core/src/tir.rs` (`walk_operands`)
**Summary:** `walk_operands` is documented as the single source of truth for TIR-shape coverage. The three new helpers introduced for I-058 (`collect_loop_body_refs`, `collect_refs_recursive`, `collect_jump_path`) are pure structural reachability over TIR shape — they consult no ownership state, just walk the IR. Today they live as private helpers in `ryo-frontend/src/ownership.rs` and re-encode the same dispatch on `WhileLoop`/`ForRange`/`IfStmt` that `walk_operands` performs internally. Adding any new control-flow shape (e.g. `match` arms) means editing `walk_operands` plus three call sites in `ownership.rs`. The same pattern keeps reappearing — `find_consumers` and `collect_last_uses` already drive recursion via `walk_operands` closures.
**Resolution:** Promote a small TIR-reachability surface to `ryo-core/src/tir.rs`. Suggested API: `Tir::collect_reachable(r) -> HashSet<TirRef>` (transitive closure of `walk_operands`) and `Tir::collect_jump_path(body, target) -> Option<HashSet<TirRef>>` (path-set reachability). Ownership-pass scheduling becomes a thin policy layer; future passes (early-return ownership, exception-arm Frees, `match`-arm Frees) reuse the same reachability primitives. Folds with I-051 (loop helper extraction) on the same TIR module.

### I-068 — `free_schedule` emission order is nondeterministic

**Files:** `ryo-frontend/src/ownership.rs` (last-use/anon-temp/dead-store post passes), `ryo-backend/src/codegen.rs` (`sweep_due_frees`)
**Summary:** The post passes push `FreePoint`s while iterating std `HashMap`s/`HashSet`s (`own.states` :430, `own.temp_owners` :487, `own.pending_dead_store` :520), whose iteration order varies per run (`RandomState`). Codegen consumes `free_schedule` in order, so emitted binaries are not reproducible run-to-run; frees commute semantically, so this is invisible today.
**Resolution:** Sort owners (by `TirRef`/`StringId`) before scheduling frees in each post pass, or switch the three collections to ordered iteration. Add a determinism test: compile the same program twice, assert identical `free_schedule` (or identical object bytes).

### I-069 — Loop fixed-point re-walk leaves speculative sidecar entries behind

**Files:** `ryo-frontend/src/ownership.rs` (`analyze_loop_body`, `analyze_if_stmt`)
**Summary:** `analyze_loop_body` passes the live sidecar to both the scratch walk (:1097) and the real re-walk (:1117). A diverged `if` inside a loop body runs `analyze_if_stmt` twice: it pushes branch-gated `FreePoint`s (:1058-1063) and `if_branches` entries (:928) both times, while `next_branch_id` is monotone and never restored. Pass-1 `FreePoint`s end up gated on `BranchId`s codegen never activates. Benign today only because ids are function-unique; the "speculative writes are not rolled back" invariant is undocumented.
**Resolution:** Either have the scratch walk write to a staging sidecar merged only on convergence (folds with I-045's propagate-only refactor), or document the invariant and assert in codegen that unknown `BranchId`s are inert.

### I-070 — `sweep_due_frees` silently drops frees anchored to unmaterialized instructions

**Files:** `ryo-backend/src/codegen.rs` (`sweep_due_frees` :1480, filter at :1494-1495)
**Summary:** The sweep only fires a `FreePoint` when `inst_values.contains_key(&fp.after)`; a free anchored to an instruction codegen never materialized stays in `pending_sweep` forever — a silent leak with no diagnostic. Correctness currently depends on the ownership pass only anchoring frees to materialized instructions, an implicit cross-crate invariant.
**Resolution:** Assert the invariant: `debug_assert!(pending_sweep.is_empty())` at function end, or emit an internal diagnostic listing dropped frees.

### I-071 — Non-void function can fall off the end with no diagnostic

**Files:** `ryo-frontend/src/sema.rs`, `ryo-backend/src/codegen.rs` (:414 fallthrough)
**Summary:** Sema validates an explicit bare `return` in a non-void fn and value/void mismatches, but a non-void function whose body simply ends gets no diagnostic; codegen just "falls through". No `MissingReturn` diag code exists. I-031 covers the inverse direction (spurious diagnostics on exhaustive if/else returns).
**Resolution:** Add return-flow analysis in sema (shared with I-031's `block_definitely_returns`): a non-void function must return on all paths; emit `MissingReturn` otherwise.

### I-072 — `TirRef::param` encoding has no predicate or guard

**Files:** `ryo-core/src/tir.rs` (`TirRef::param` :80-82), `ryo-frontend/src/ownership.rs` (:90), `ryo-backend/src/codegen.rs` (:575)
**Summary:** `TirRef::param(idx)` = `u32::MAX - idx` produces a valid `NonZeroU32` indistinguishable from a real instruction index; there is no `is_param()`/`as_param()` predicate anywhere. Passing one to `Tir::inst()` panics out-of-bounds. Correctness rests entirely on consumer discipline — current consumers use param refs as map keys only.
**Resolution:** Add `TirRef::is_param()`/`as_param_index()` and a `debug_assert!(!r.is_param())` in `Tir::inst()`. Longer term, replace the informal encoding with a clean `Owner` enum as planned in `docs/dev/implementation_roadmap.md` (:1046).

### I-073 — Zig download has no integrity verification and races concurrent installs

**Files:** `ryo-backend/src/toolchain.rs` (`download_zig` :54-112)
**Summary:** The tarball is streamed HTTPS → XZ → tar with no sha256/signature check even though ziglang.org publishes shasums and `.minisig` files — a supply-chain gap. The fixed temp dir `.zig-{v}-downloading` (:62) lets two concurrent first-runs delete each other's in-flight download (`remove_dir_all` at :67), and `remove_dir_all(&desired_path)` (:101) can delete a working toolchain out from under another running compile.
**Resolution:** Hardcode the three pinned sha256s (one per supported target) and verify before extraction; use a pid-suffixed temp dir (matching `runtime_lib.rs`'s discipline) and atomic rename; never delete `desired_path` until the replacement is staged.

### I-075 — Duplicate function definitions silently accepted

**Files:** `ryo-frontend/src/sema.rs` (`Sema::new` :219-227)
**Summary:** `name_to_decl` is first-wins on duplicates: both bodies are analyzed and get TIR, but calls bind to the first definition and no redefinition diagnostic is emitted. The inline comment defers to "a dedicated redefinition pass".
**Resolution:** Emit `DiagCode::DuplicateDeclaration` (E0029) for duplicate function names at seed time; keep first-wins binding for error recovery.

### I-076 — `str` ABI is hardcoded to 64-bit layout

**Files:** `ryo-backend/src/codegen.rs` (all str stack slots), `runtime/src/lib.rs` (`RyoStrFat`)
**Summary:** Every str stack slot hardcodes 24 bytes / align 3 / offsets 0,8,16 (:1607, :1667, :1727, :1800, :1833, :1894, :1956, :746-748), and `len`/`cap` are hardcoded `types::I64` (:404-405) while `ptr` is pointer-sized. On a 32-bit target, caller and callee layouts silently mismatch.
**Resolution:** Centralize the fat-pointer layout in one place (offsets and size computed from `module.target_config().pointer_type()`) and mirror it in the runtime. Prerequisite for any 32-bit target; interacts with I-021 (bool FFI width) when FFI lands.

### I-077 — Invalid characters surface as parse errors, not lex errors

**Files:** `ryo-frontend/src/lexer.rs` (:327-330, :394), `ryo-frontend/src/parser.rs`
**Summary:** Logos failures map to `RawToken::Error` → `Token::Error` and are pushed into the token stream with no diagnostic; the parser later fails with a generic "unexpected token" error. A bad byte surfaces as a parse error divorced from its cause.
**Resolution:** Emit a structured lex diagnostic at the `Token::Error` site (folds with I-014's `DiagSink` migration), then let the parser see the recovery token.

### I-078 — Parse diagnostics leak interned-handle placeholders (`<id#N>`)

**Files:** `ryo-frontend/src/lexer.rs` (`Token` `Display` :105-170), `ryo-driver/src/pipeline.rs` (`parse_source` :130-142, `emit_one`)
**Summary:** `Token`'s `Display` renders `Ident` as `<id#N>` and `StrLit` as `<str#N>`; chumsky `Rich` errors are rendered via `e.reason().to_string()` with no pool available, so user-facing parse errors can contain opaque handle ids instead of source text. The lexer comment (:106-110) claiming the driver re-renders with the pool is stale — it does not.
**Resolution:** Make parse-error rendering pool-aware (re-render expected/found tokens through the pool in `parse_source`), or carry structured expected/found data in `Diag` instead of a pre-formatted `String`.

### I-079 — Unary minus on `float` is rejected

**Files:** `ryo-frontend/src/sema.rs` (`InstTag::Neg` arm :932-953)
**Summary:** The `Neg` arm only handles `TypeKind::Int` (`INeg`); `-x` on a float operand emits `UnsupportedOperator` even though float arithmetic is otherwise fully supported (:1269-1279). Asymmetric and undocumented; smells like an oversight rather than a decision.
**Resolution:** Add `TirTag::FNeg` lowering to Cranelift `fneg` and accept `Float` in the `Neg` arm.

### I-080 — UIR/TIR `extra`-layout modules are duplicated with subtly different layouts

**Files:** `ryo-core/src/uir.rs` (`var_decl_extra` etc.), `ryo-core/src/tir.rs` (`call_extra` :337-342, `var_decl_extra` :355-362, `assign_extra`/… :370-418)
**Summary:** tir.rs re-defines near-identical `extra`-layout modules with different layouts: `call_extra` appends a modes tail; `var_decl_extra` drops the `TY` slot (`LEN: 3` vs uir's `4`). Same names, same constants, different meanings — a footgun when editing one side. `ExtraRange` itself is also byte-duplicated (`uir.rs:107-118` vs `tir.rs:87-98`), and `IfStmt` has no layout doc module at all in tir.rs (:677-715).
**Resolution:** Unify the shared pieces (`ExtraRange` at minimum) in one module; rename or document the layout differences explicitly; add the missing `if_stmt_extra` doc module.

### I-081 — `break`/`continue` conflated with `return` in codegen terminator tracking

**Files:** `ryo-backend/src/codegen.rs` (`emit_stmt` :758, :882, :896; `generate_if_stmt` :987-1003; `emit_body` :620-622)
**Summary:** `emit_stmt` returns `true` for `Return`, `Break`, and `Continue` alike, so `all_branches_return` treats an arm ending in `break` as "returns" and `emit_body` drops all subsequent statements including their scheduled frees. Works today only because break/continue fire their own frees first; the boolean is doing double duty.
**Resolution:** Return a small enum (`Terminator::{None, Return, Break, Continue}`) from `emit_stmt` and treat only `Return` in `all_branches_return`.

### I-082 — `never`-returning call path skips inout write-back

**Files:** `ryo-backend/src/codegen.rs` (:1944-1952)
**Summary:** A callee typed `never` (today only `__ryo_panic`) is emitted as call + trap + dead block and skips `reload_inout_args` — any inout argument's mutated value is dropped. Latent: `__ryo_panic` takes no inout params today, but the path exists for any future `never`-typed function.
**Resolution:** Call `reload_inout_args` before the trap, or assert at sema that `never`-typed callees cannot take inout params.

### I-083 — `eval_inst` returns a str's data pointer as a dummy scalar

**Files:** `ryo-backend/src/codegen.rs` (`eval_inst` :1141-1148, :1976; dual entry points with `eval_inst_str`)
**Summary:** The scalar entry point returns the str `ptr` as a "dummy scalar" stand-in for str-typed insts, so any scalar-path consumer of a str inst silently gets the data pointer. The two entry points share the `inst_values` memo with an ordering dependency (`or_insert` at :1384), and `StrConst` has different meanings per entry point (:1164-1173 vs :1574-1580).
**Resolution:** Make `eval_inst` reject str-typed insts (or return the full repr), and split the memo by entry point. Related to I-020's memoizer scoping.

### I-084 — `ryo build` writes artifacts to the CWD; same-stem sources collide

**Files:** `ryo-driver/src/pipeline.rs` (`get_output_filenames` :34-44, `build_file` :485-522)
**Summary:** Output names are derived from `file_stem` only, so `{stem}.o`/`{stem}` land in the current working directory — two same-stem sources built from the same CWD clobber each other, and on link failure the `.o` is left behind (the early `?` at :509 skips cleanup).
**Resolution:** Place outputs next to the source (or under a `target/` dir), include a disambiguator when needed, and clean up the `.o` on the link-failure path.

### I-085 — Valgrind smoke tests silently pass when valgrind is absent

**Files:** `ryo/tests/valgrind_smoke.rs` (:36-40)
**Summary:** `run_valgrind_smoke` prints "skipping" and returns success when valgrind is not installed, so local green runs may have exercised nothing. Only the CI lane that `apt-get install`s valgrind guarantees coverage; the suite exists because LSan misses leaks from Cranelift-emitted code (not ASan-instrumented).
**Resolution:** Fail loudly (or require an opt-in env var to skip) outside CI; at minimum print a prominent end-of-suite summary of skipped tests.

### I-086 — E-code taxonomy conflicts with the roadmap and has no stability test

**Files:** `ryo-driver/src/pipeline.rs` (`diag_code_str` :219-257), `docs/dev/implementation_roadmap.md` (:1039, :1050, :1069)
**Summary:** The `DiagCode`→E-number mapping exists only in `diag_code_str`. The roadmap's M8.2 section uses **E0023** for move-while-borrowed-in-calls, but code assigns `MoveWhileBorrowedInCall = "E0031"` (E0023 is `FloatModulo`). Arms are unordered (E0019 between E0012/E0013), ranges have gaps, and no test pins code stability/uniqueness — a transposition typo would silently renumber a diagnostic. `diag.rs:51` calls `DiagCode` the "stable error identity" while the actual stable strings live in the driver.
**Resolution:** Add a test asserting each `DiagCode` maps to its expected unique string (explicit list); fix the roadmap references; consider moving the code strings onto `DiagCode` itself.

### I-087 — Loop fixed-point convergence compares only Moved-ness

**Files:** `ryo-frontend/src/ownership.rs` (`states_differ_snapshot` :1206-1234)
**Summary:** The re-walk gate compares only Moved-ness between snapshots; `Valid`↔`Borrowed` flips don't force another iteration. Sound today only because `Borrowed` never transitions after param init (I-053) and merges are monotone — the maintainer note at :1168-1172 already flags this must become a real fixed point if either changes.
**Resolution:** Fold into I-045's propagate-only refactor: iterate until the full state tuple (not just Moved-ness) is stable, or document the reduced comparison as load-bearing next to the gate.

### I-088 — Ownership sidecar is keyed by function name

**Files:** `ryo-core/src/ownership.rs` (`OwnershipSidecar` :50-52), `ryo-frontend/src/ownership.rs` (:301-309)
**Summary:** `functions` is a `HashMap<StringId, FunctionSidecar>` keyed by interned name. Correct today because `TirRef` arenas restart per function and names are unique (I-075 notwithstanding), but any future overloading or same-name functions in different scopes will silently collide.
**Resolution:** Key by `DeclId`/body index (positional with `Vec<Tir>`) when the declaration model supports it.

---

## 🟢 Cleanup

### I-089 — `ParamMode::from_u32` silently coerces unknown values to `Borrow`

**Files:** `ryo-core/src/tir.rs` (:240-246)
**Summary:** Decoding an unknown mode word yields `Borrow` — the least restrictive convention — instead of an error. Harmless today; a footgun if the enum grows or a payload is corrupted.
**Resolution:** Return `Option<ParamMode>` and internal-error on unknown values at the single decode site (`call_view`).

### I-090 — Two near-verbatim branch-merge implementations in the ownership pass

**Files:** `ryo-frontend/src/ownership.rs` (`merge_branches` :163-294, `merge_non_monotone` :1238-1310)
**Summary:** The N-way and 2-way merges re-implement the same any-Moved-wins + binding-aware-override + dead-store intersect/union logic; the override blocks (:226-260 vs :1261-1290) are near-verbatim copies. Changing merge semantics requires editing both.
**Resolution:** Express `merge_non_monotone` in terms of the shared merge core, or extract the override/intersect helpers used by both.

### I-091 — UIR/TIR view decoders allocate a `Vec` per decode

**Files:** `ryo-core/src/uir.rs` (`call_view` :843-847, `if_stmt_view` :980-1001, `body_stmts` :320-326, `while_loop_view` :915, `for_range_view` :931-935, `method_call_view` :955-959), `ryo-core/src/tir.rs` (`call_view`), `ryo-backend/src/codegen.rs` (call view args/modes)
**Summary:** Every accessor decode collects refs out of `extra` into a fresh `Vec<InstRef>`/`Vec<TirRef>`, and `body_stmts()` collects a slice that is already contiguous. Sema and codegen call these in their hottest loops. Additionally `ExtraRange.len` is write-only metadata (decoders re-derive counts from inline `argc` words) — a second source of truth.
**Resolution:** Return borrowed slices (`&[InstRef]` over `extra`) or `impl Iterator` from the views; `body_stmts` can be a slice iter directly. Add `assert_eq!(size_of::<Inst>(), 24)` before any `InstData` refactor.

### I-092 — Sema per-function and per-call allocation churn

**Files:** `ryo-frontend/src/sema.rs` (`FuncCtx` :396, `check_call` :1361-1384, method calls :996-1014)
**Summary:** (a) `inst_map` is `vec![None; uir.instructions.len()]` — the program-wide UIR size — allocated per function; (b) `check_call` clones `callee_modes`, `sig.params`, and builds `modes`/`arg_tirs` per call (3-4 allocations); (c) method dispatch does `pool.str(..).to_string()` per method call site, allocated even before the receiver-type check.
**Resolution:** (a) `HashMap<InstRef, TirRef>` or per-function UIR slice (the expr memo is the only consumer that needs random access); (b) borrow from the signatures table instead of cloning; (c) match on pre-interned `StringId`s for `len`/`is_empty` instead of a `String`.

### I-093 — Runtime functions are re-imported per use site; JIT symbol list is hand-synced

**Files:** `ryo-backend/src/codegen.rs` (`declare_runtime_fn` :1390-1408 and call sites; `new_jit` :249)
**Summary:** No name→`FuncId` cache exists; two `int_to_str` calls in one function produce two import declarations. Same for libc `write` (:2139) and `exit` (:2118-2123). Additionally `ryo_str_alloc` is registered in the JIT symbol table (:249) with no call site anywhere — the symbol list and the call sites are kept in sync by hand.
**Resolution:** Add a per-module `HashMap<&'static str, FuncId>` cache on `Codegen`; drive the JIT symbol list from the same table.

### I-094 — `compile_function` renders CLIF text unconditionally

**Files:** `ryo-backend/src/codegen.rs` (:603, discarded at :333-335)
**Summary:** `compile_function` always `format!`s the Cranelift function even on the plain `compile` path where the caller discards it — one full CLIF pretty-print per function per compile, thrown away.
**Resolution:** Only render when an IR dump was requested (thread a flag, or render separately in `compile_and_dump_ir`).

### I-095 — `emit_scoped_body` clones both locals maps per block

**Files:** `ryo-backend/src/codegen.rs` (:646-650)
**Summary:** Each if-arm/loop body clones the `locals` and `str_locals` HashMaps to get restore-on-exit semantics — O(locals) per block, quadratic-ish on deep nesting.
**Resolution:** Track per-block bindings as a small undo log (name → previous `Variable`) and restore on exit instead of cloning whole maps.

### I-096 — `~/.ryo/cache` grows unbounded

**Files:** `ryo-backend/src/runtime_lib.rs` (:17-40)
**Summary:** Runtime archives are cached by content hash and never evicted (42 archives / 556 MB observed on a dev machine). `extract_runtime_to_temp` is a misnomer (persistent cache, not temp) and `cleanup_runtime_temp` is a no-op; stale `.tmp.{pid}` files linger after a kill.
**Resolution:** Keep-last-N eviction by mtime (or a `ryo toolchain clean` command); rename the functions to reflect cache semantics; sweep stale `.tmp.*` on extract.

### I-097 — Embedded runtime archive is ~17 MB

**Files:** `ryo-backend/src/runtime_lib.rs` (:5), `runtime/` (build profile)
**Summary:** `include_bytes!` bakes the full staticlib into the compiler binary; the archive bundles Rust std (also the root cause of the `_Unwind_*` link wart, I-043). Measured locally: 17.9 MB debug / 17.7 MB release.
**Resolution:** Build the embedded archive with a slim profile (`opt-level="z"`, strip, LTO — the build scripts control that invocation) and/or land I-043's `no_std` migration, which removes most of std from the archive.

### I-098 — Integration tests spawn `cargo run` per test

**Files:** `ryo/tests/integration_tests.rs` (`run_ryo_command` :7-16)
**Summary:** 149 of 155 tests invoke `cargo run --` as a subprocess, paying cargo's startup, a workspace freshness re-check, and build.rs execution (git calls + sha256 of the runtime archive) per test. The smoke-test harness already demonstrates the cheap pattern: `env!("CARGO_BIN_EXE_ryo")` (`ryo/tests/common/mod.rs:11,38`).
**Resolution:** Point `run_ryo_command` at `env!("CARGO_BIN_EXE_ryo")`; keep one `cargo run` smoke test to cover that entry path. Largest single test-time win available.

### I-099 — `run_file` debug output is load-bearing for the integration suite

**Files:** `ryo-driver/src/pipeline.rs` (`run_file` :457-483), `ryo/tests/integration_tests.rs`
**Summary:** `ryo run` echoes `[Input Source]`, the full AST, and `[Codegen]` on every invocation; ~63 test assertions key on `"[Result] => 0"` and the section headers as the pass/fail signal, and tests post-filter stdout (split on `"[Codegen]"`). Any cleanup of the chatter breaks the suite.
**Resolution:** Gate the debug sections behind a `--verbose` flag, then migrate tests to exit-code assertions; do the harness migration (with I-098) before touching `run_file`.

### I-100 — CodSpeed AOT lanes measure nothing; no backend benchmarks exist

**Files:** `.github/workflows/codspeed.yml` (:80, :110), `ryo-frontend/benches/frontend.rs`
**Summary:** Both AOT lanes run `codspeed run` with `allow-empty: true`; no codspeed-instrumented AOT benchmarks exist in the repo. The lanes still build the full compiler in release and rebuild both benchmark binaries. There are also no backend benchmarks at all — Cranelift codegen, linking, and the ownership/eager-destruction story have zero automated benchmark coverage; the headline "2× less heap" claim is validated only by a manual shell script.
**Resolution:** Either add instrumented AOT/codegen benchmarks (e.g. compile `benchmarks/fibonacci`) or delete the lanes. Add an automated eager-destruction regression check.

### I-101 — `examples/` is exercised by nothing

**Files:** `examples/*.ryo`, `examples/future/`
**Summary:** 12 top-level examples plus ~20 aspirational files are referenced by no test, script, or workflow; nothing verifies they even parse.
**Resolution:** Add a CI step (or integration test) that runs `ryo parse` over `examples/` (excluding `future/`), and ideally `ryo run` on the runnable ones.

### I-102 — Smoke suites duplicate work across lanes and fixture builds

**Files:** `ryo/tests/asan_smoke.rs`, `ryo/tests/valgrind_smoke.rs`, `ryo/tests/common/mod.rs`, `.github/workflows/ci.yml` (:83)
**Summary:** Both suites iterate the same 11 fixtures (`common/mod.rs:81-210`), compiling+linking each twice per full run; each `build_and_link` also shells out to `ryo toolchain status --path` to find zig (:10-22). `cargo test --workspace` in the test lane already includes `asan_smoke`, so it runs twice on ubuntu (test lane + dedicated asan lane); valgrind "runs" (silently skips, I-085) in lanes without valgrind.
**Resolution:** Share fixture compilation across suites, cache the zig path, and exclude the smoke suites from the default test lane (or from the dedicated lanes).

### I-103 — Diagnostics print twice on failure; `emit_one` can panic mid-report

**Files:** `ryo-driver/src/pipeline.rs` (`emit_one` :174-205), `ryo/src/main.rs` (:71)
**Summary:** The report header and the label both use `d.message`, so every diagnostic prints its text twice; `.expect("diag render")` (:204) can panic mid-report. After the ariadne report, `main`'s `Box<dyn Error>` return makes the std `Termination` handler print a second, differently-formatted summary line.
**Resolution:** Use a short label message (or set the message only once); handle the `eprint` result gracefully; consider `ExitCode`-based `main` to control the final line.

### I-104 — `ryo-core` depends on chumsky solely for `SimpleSpan`

**Files:** `ryo-core/src/diag.rs` (:18-20), `ryo-core/Cargo.toml`
**Summary:** The "core" IR/types crate pulls in a parser crate for one span type, coupling every consumer of `ryo-core` to chumsky's release cycle.
**Resolution:** Define a small `Span` newtype in `ryo-core` and convert at the parser boundary (`pipeline.rs` already adapts spans).

### I-105 — `__ryo_str_push` takes `suffix_len: i64` while all other lengths are `u64`

**Files:** `runtime/src/lib.rs` (:156-160, :168), `ryo-backend/src/codegen.rs` (:1851-1857)
**Summary:** The one signed length in the runtime ABI is silently clamped via `suffix_len.max(0)`; every other length parameter is `u64`. Negative values from a buggy caller are accepted rather than rejected.
**Resolution:** Change the parameter to `u64` on both sides (codegen declares `types::I64` at :1855), or assert non-negative in debug builds.

### I-106 — Decode paths panic instead of reporting an internal error

**Files:** `ryo-core/src/uir.rs` (view decoders, `InstRef::from_raw` :99-101), `ryo-frontend/src/sema.rs` (:469, :514, :782-786, :878-890, :1071-1075), `ryo-backend/src/codegen.rs` (~20 `unreachable!` arms, `cranelift_type_for` :56-67, `unimplemented!` Tuple :71)
**Summary:** View decoders `debug_assert` the tag then `unreachable!` on mismatch; sema hard-trusts astgen with `panic!`/`unreachable!` on tag mismatches; codegen mixes `Result<_, String>` with panics. Malformed IR crashes the compiler with no internal-error diagnostic. Fine with exactly one producer per IR; brittle for any future producer (caches, plugins, alternative front ends).
**Resolution:** Low priority by design. If a second UIR/TIR producer ever lands, convert the decode paths to an internal-error `Diag`; until then, document the "trusted producer" invariant at each IR boundary.

### I-107 — Ownership pass does linear, panicking param lookups in hot paths

**Files:** `ryo-frontend/src/ownership.rs` (`Owner::tirref` :81-93, :450-454, :1026-1031, :1049-1055)
**Summary:** Four sites do `tir.params.iter().position(...).expect("param exists")` — O(P) per call inside per-owner loops, and a panic (not a diagnostic) on malformed TIR.
**Resolution:** Build a name→param-index map once per function (or key `Owner` by param index directly); keep the `expect` as a `debug_assert` once the invariant is verified at entry.

### I-108 — Owned params are freed after the last body statement, not their last use

**Files:** `ryo-frontend/src/ownership.rs` (:448-462)
**Summary:** A still-`Valid` `Owner::Param` at function exit is freed after the last body statement; only `Inst` owners get true last-use anchoring. Owned params thus live longer than necessary — coarser than the local-variable policy.
**Resolution:** Extend last-use tracking to params (they have stable `Owner`s already); verify against the valgrind suite.

### I-109 — No instruction→function reverse mapping in UIR

**Files:** `ryo-core/src/uir.rs` (`func_bodies` :272, :279-284)
**Summary:** `func_bodies` lists only top-level statement refs; given an arbitrary `InstRef` you cannot tell which function owns it without walking every body. Any pass wanting per-function slices of the shared arena (diagnostics, per-function codegen, future incremental sema) re-derives this by traversal.
**Resolution:** Add a computed inst→body index map (built lazily or at `finish()`), or move to per-function UIR arenas mirroring TIR when Phase 5 lands.

### I-110 — Tree-shaped-TIR assumption is undocumented

**Files:** `ryo-frontend/src/ownership.rs` (`find_consumers` :1351-1369)
**Summary:** `find_consumers` uses first-parent-wins `or_insert` because "TIR is tree-shaped per function"; the anon-temp free anchor silently breaks if TIR ever becomes a DAG (shared subexpressions, CSE). The invariant lives in one comment.
**Resolution:** Document the tree-shape invariant on `Tir` itself (`ryo-core/src/tir.rs`) and add a debug assertion or validation pass that each inst has at most one parent.

### I-111 — Lexer token boilerplate is four touch points per variant

**Files:** `ryo-frontend/src/lexer.rs` (`RawToken` :176-300, `Token` :30-103, `intern_token` :392-495, `Display` :105-170)
**Summary:** Adding a token means editing `RawToken`, `Token`, the giant manual `intern_token` match, and `Display` (plus the parser downstream) — ~45 non-payload variants of pure boilerplate.
**Resolution:** Generate the quadruple from a single macro table (variant name, logos pattern, payload kind).

---

## Cross-References

- Architecture analysis: [docs/dev/architecture_analysis.md](docs/dev/architecture_analysis.md)
- Roadmap: [docs/dev/implementation_roadmap.md](docs/dev/implementation_roadmap.md)
- Spec: [docs/specification.md](docs/specification.md)
- Phase plan: [docs/dev/pipeline_alignment.md](docs/dev/pipeline_alignment.md)
