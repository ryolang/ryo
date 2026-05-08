# Known Issues

Compiler issues identified during source review. Each entry is independently actionable; severity reflects impact on correctness, future feature work, or code health — not user impact today (the compiler is pre-alpha).

Resolved entries are removed (not kept around as a changelog). Look at `git log` if you need history.

---

## Severity Legend

- 🔴 **Blocking** — prevents implementing roadmap features as currently designed.
- 🟡 **Correctness/Hygiene** — silent bug or invariant gap; works today, will bite later.
- 🟢 **Cleanup** — code health, ergonomics, minor.

---

## 🔴 Blocking

### I-004 — String type is a raw pointer with no length
**Files:** `src/codegen.rs` (`generate_print_call`)
**Summary:** `StrLiteral` codegen returns a bare `global_value` pointer. There is no length, no slice ABI, no ownership metadata. `print()` works only on string literals because it grabs the length from the AST node, not from the runtime value. Any non-literal string operation will require a fat-pointer (`*u8, usize`) ABI decision.
**Resolution:** Design and implement a string slice ABI before adding string operations. Reference: Zig's `[]const u8`.

---

## 🟡 Correctness / Hygiene

### I-005 — `mut` is parsed but never enforced
**Files:** `src/uir.rs` (`VarDecl`), `src/sema.rs`
**Summary:** `VarDecl` carries `mutable: bool`, but no pass reads it. The README advertises immutable-by-default semantics. Reassignment isn't parsed yet, so the bug is latent — but the invariant should be checked in sema as soon as assignment lands.
**Resolution:** When assignment is added to the parser, sema must reject reassignment to non-`mut` bindings.

### I-006 — `print` is special-cased in codegen
**Files:** `src/codegen.rs` (`generate_print_call`), `src/sema.rs` (`check_builtin_call`)
**Summary:** Codegen emits a raw `write(2)` syscall wrapper inline for the `print` builtin, and sema has a builtin-specific validator hook to match. Consequences:
- Rejects `print(some_var)` even when the variable is a string.
- No formatting, no automatic newline.
- Already stubbed out on Windows (`return Err(...)`).
- Mixes runtime concerns into the compiler.
**Resolution:** Move `print` to a runtime crate (`ryort` or similar) compiled to an object file and linked in via `zig cc`. Codegen emits a normal call; `sema::check_builtin_call` goes away.

### I-010 — Unused `_bytes_written` from `write(2)` call
**Files:** `src/codegen.rs` (`generate_print_call`)
**Summary:** The result of the `write` syscall is fetched and ignored. A short write or `EINTR` will silently truncate output. Acceptable for a bootstrap, but should be documented or fixed when `print` moves to the runtime (I-006).
**Resolution:** Tracked under I-006.

### I-014 — Lexer errors bypass `DiagSink`
**Files:** `src/lexer.rs` (`LexError`), `src/pipeline.rs` (`parse_source`, `display_tokens`)
**Summary:** Phase 1 of `docs/dev/pipeline_alignment.md` routes parse / ast_lower / sema errors through a `DiagSink` so analysis can continue past the first failure. The lexer was on a parallel branch at the time and still uses a single-shot `Result<_, LexError>`. The driver wraps a `LexError` into a one-element `Vec<Diag>` at the boundary, but problems like "invalid integer literal" or "unknown escape sequence" can't co-surface with a sema error in the same run.
**Resolution:** Thread `&mut DiagSink` into `lexer::lex` and `intern_token`. Replace the early return on the first lex error with `sink.emit(...)` followed by a recovery token (e.g. `Token::Error` for the bad span) so the parser still sees a well-formed stream. Eliminate `LexError` once the migration is complete.

### I-015 — Unknown escape sequences silently preserved
**Files:** `src/lexer.rs` (`unescape`)
**Summary:** When `unescape` encounters `\q` (or any other character not in the small known table) it preserves the backslash and the character verbatim instead of reporting a diagnostic. The user gets no feedback that the escape sequence is unrecognised, and the runtime string contains the literal `\q` bytes. Tracked as a TODO at the function definition.
**Resolution:** Folded in with I-014: once `lexer::lex` has a `&mut DiagSink`, the `Some(c)` arm of `unescape` emits a structured `Diag::error(span, DiagCode::UnknownEscape, …)` and proceeds with the raw character (or skips both bytes — TBD by spec discussion).

### I-016 — Indent processor errors carry no span
**Files:** `src/indent.rs` (`process`), `src/lexer.rs` (`lex` fallback)
**Summary:** `indent::process` returns `Result<_, String>` with a free-form message and no source location. The driver currently fakes a span by reusing the last raw token's span. That's "near" the offending newline but not on it; for "spaces are not allowed" / "dedent doesn't match an outer level" the user wants the squiggle on the indentation itself.
**Resolution:** Have `indent::process` return a richer error type carrying the offending span (the `Newline(s)` token whose whitespace failed validation, or the `Dedent` insertion point) and propagate it into `LexError.span`. The TODO at the lexer fallback covers this from the consumer side.

### I-017 — `i64::MIN` integer literal is unrepresentable
**Files:** `src/lexer.rs` (`RawToken::Int` arm)
**Summary:** Integer literals are parsed as `i64` at lex time, then sign is applied later via the unary `-` operator. That makes `-9_223_372_036_854_775_808` (i.e. `i64::MIN`) unspellable: the positive form `9_223_372_036_854_775_808` overflows `i64`. Hits the negation-overflow corner Rust itself fixed via `IntLit` / `IntLitMin` token-level distinction.
**Resolution:** Either parse as `u64` and resolve negation+overflow at sema time, or add an `IntLitMin` token variant that the parser recognises only as the operand of unary `-`. Coordinate with the broader numeric-tower design before picking either.

### I-020 — `inst_values` memoizer is cross-block
**Files:** `src/codegen.rs` (`inst_values: HashMap<TirRef, Value>`)
**Summary:** Codegen lazily memoizes Cranelift `Value`s keyed by `TirRef` in a single flat HashMap shared across all basic blocks within a function. This is sound today because: (a) TIR instructions are unique per use (no shared sub-expressions), (b) BoolAnd/BoolOr use block params (phi nodes) so the memoized result is the merge-block param which dominates downstream uses, and (c) IfStmt is statement-level so no values flow out of branches. However, if future features introduce expression-level if (ternary) or shared sub-expressions across blocks, the memoizer will produce Cranelift dominator errors.
**Resolution:** Scope the memoizer to per-block when expression-level control flow lands. For now the cross-block cache is correct given the TIR invariants.

### I-031 — No return-flow analysis for if/elif/else
**Files:** `src/sema.rs`, `src/codegen.rs`
**Summary:** Sema does not track whether all paths through an if/elif/else return a value. The codegen `emit_stmt` returns `all_branches_return` correctly, but sema has no equivalent — a function with `if/else` where all branches return still gets a "missing return" warning from the implicit `ReturnVoid` appended by the codegen fallthrough path. Currently papered over because void-returning `main` doesn't need a return, and non-void functions with complete coverage happen to work because codegen skips the merge block when all branches return.
**Resolution:** Add `block_definitely_returns` analysis in sema so that functions with exhaustive if/else returns don't get spurious diagnostics. This becomes necessary when `while` loops and `match` land.

### I-032 — IfStmt is statement-only, no expression-level conditional
**Files:** `src/ast.rs`, `src/parser.rs`, `src/sema.rs`, `src/codegen.rs`
**Summary:** `if`/`elif`/`else` is a statement (`StmtKind::IfStmt`), not an expression. There is no way to write `x = if cond: a else: b` (ternary/conditional expression). The spec envisions `if` as an expression in certain contexts. Current codegen emits void for IfStmt and uses no phi-merge for values across branches.
**Resolution:** Add `ExprKind::IfExpr` when the spec finalizes expression-if syntax. Codegen would use block params (like BoolAnd/BoolOr already do) to merge values at the join point. Requires I-020 attention for memoizer correctness.

### I-033 — Variables declared inside if/elif/else branches are not visible after the statement
**Files:** `src/sema.rs` (`analyze_block`)
**Summary:** Each branch of an if/elif/else creates a child scope. Variables declared inside a branch are dropped when the branch scope ends. There is no "variable promotion" — even if all branches declare `x: int`, `x` is not available after the if statement. This is the correct scoping semantics for now, but may surprise users expecting Python-style scoping where if-branches don't create a new scope.
**Resolution:** This is intentional for M8b. If user feedback requests Python-style flat scoping, revisit as a language design decision (requires approval per CLAUDE.md escalation rules).

---

## 🟢 Cleanup

### I-011 — Manual error enum where `thiserror` would suffice
**Files:** `src/errors.rs` (33 lines)
**Summary:** Hand-rolled `enum CompilerError` with manual `Display` and `From<io::Error>` impls. `thiserror` would cut ~20 lines and make variants more uniform.
**Resolution:** Add `thiserror`, derive `Error` and `Display`, drop the hand-written impls.

### I-012 — `pretty_print` lives on AST nodes
**Files:** `src/ast.rs`
**Summary:** Presentation logic (tree-drawing, prefixes) is mixed into the AST data types. Convenient now, painful as the AST grows. A separate `pretty` module — or `Debug`-derived JSON output for `--parse` — scales better and keeps `ast.rs` focused on data.
**Resolution:** Extract into `src/ast_pretty.rs` (or similar) when the next AST node type is added.

### I-013 — `--emit` flag surface is fragmented across subcommands
**Files:** `src/main.rs`, `src/pipeline.rs`
**Summary:** `lex`, `parse`, `ir` are separate subcommands. Each stage already exists and is wired up; users would benefit from a single `ryo build --emit=tokens|ast|hir|clif|obj` surface (mirroring `zig build-exe -femit-…`).
**Resolution:** Unify under one subcommand with an `--emit` flag. Keep current subcommands as deprecated aliases for one release.

### I-018 — `TypeId` is a newtype, not a typed enum
**Files:** `src/types.rs` (`TypeId`)
**Summary:** Phase 2 §2.2 of `docs/dev/pipeline_alignment.md` originally called for `TypeId` to become an `enum { Void = 0, Bool = 1, ..., Error = 4, Dynamic(NonZeroU32) }` so primitive matches are exhaustive at compile time and the `pool.int()` accessor disappears. The risk register allowed a fallback to a plain `Copy` newtype if the enum encoding fights the borrow checker, which is what we shipped. Cost: the `TypeKind::Tuple` arm we added in `cranelift_type_for` and a couple of sema sites are not statically guaranteed to be covered when a new primitive lands.
**Resolution:** Re-attempt the enum encoding using `repr(u32)` + `Dynamic(NonZeroU32)` once the borrow-checker pain points (mostly around `pool.kind` returning a value that contains a `TypeId`) are characterised. Low priority — the matches we have today still go through `TypeKind`, which *is* exhaustive, so the gap is small.

### I-019 — `tuple_elements_vec` allocates a `Vec` per call
**Files:** `src/types.rs` (`tuple_elements_vec`)
**Summary:** The accessor copies the element-id slice out of `extra` rather than returning a borrowed view, because `TypeId` is not `#[repr(transparent)]` over `u32` and the unsafe transmute to `&[TypeId]` would be UB without it. Today the function is called only by `Display` for diagnostics and by tests; not a hot path.
**Resolution:** Tag `TypeId` with `#[repr(transparent)]` and expose `tuple_elements(id) -> &[TypeId]` alongside the copying accessor. Migrate non-perf-critical callers to it lazily. Defer until tuple codegen lands and the accessor shows up in a profile.

### I-021 — `bool` lowered as `types::I8` will mis-ABI across FFI boundaries
**Files:** `src/codegen.rs` (`cranelift_type_for`)
**Summary:** `TypeKind::Bool` maps to Cranelift `I8`. Fine for internal logic, but C ABIs typically pass `_Bool` zero/sign-extended to a full register (often i32 on SysV, register-width on Win64). Passing or returning our raw `I8` across an FFI call would leave the upper bits undefined from the callee's perspective.
**Resolution:** When FFI lands, insert explicit `uext` (zero-extension) on bool arguments at call sites and `ireduce` on bool returns, per the target ABI. Decide at the FFI design stage whether `bool` keeps its `I8` storage type and only widens at the boundary, or becomes register-width throughout. Latent until FFI exists.

### I-022 — String equality not implemented
**Files:** `src/sema.rs` (`check_binary_op`)
**Summary:** `==` / `!=` on `str` is rejected with `"not supported for type 'str' (yet)"`. Blocked on I-004: with strings represented as bare pointers, there is no length to compare against. Fixing this requires the fat-pointer ABI from I-004 plus a `memcmp` libcall (or an inlined byte-compare loop) at codegen.
**Resolution:** After I-004 lands the `(*u8, usize)` slice ABI, implement `==`/`!=` in codegen as length-check + `memcmp`. Sema just needs the rejection removed.

### I-023 — Integer division / modulo by zero is undefined behavior at codegen
**Files:** `src/codegen.rs` (`TirTag::ISDiv`, `TirTag::IMod` arms), `src/sema.rs` (`check_binary_op`)
**Summary:** `a / 0` and `a % 0` lower directly to Cranelift `sdiv` / `srem` with no guard. Cranelift treats both as UB; the resulting native instruction (`idiv` on x86-64, `sdiv` on aarch64) traps or returns garbage depending on target. Sema does not constant-fold a known-zero divisor either.
**Resolution:** When safety checks land, insert a runtime check at codegen: emit a compare-against-zero, branch to a trap / panic block on hit. Optionally constant-fold the obvious `x / 0` / `x % 0` cases at sema and emit a diagnostic. Coordinate with the eventual panic / safe-mode design so the trap path has somewhere to go.

### I-024 — Single `float` type, no `float32` / `float64` distinction
**Files:** `src/types.rs` (`Tag::Float`, `TypeKind::Float`), `src/codegen.rs` (`cranelift_type_for`)
**Summary:** M7 ships one float type (`float`), lowered to Cranelift `F64`. Matches today's `int` (one width, machine-word). Users who need 32-bit floats for memory, GPU work, or C interop have no surface syntax to ask for one.
**Resolution:** Add `Tag::Float32` alongside the existing `Tag::Float` (which becomes `Float64` semantically), expose `: float32` / `: float64` annotations, and pick one as the default for unannotated `1.5`-style literals. Coordinate with the broader numeric-tower design (sized integers, `usize` / `isize`) so the widening story is consistent across types.

### I-025 — No implicit `int` ↔ `float` promotion or conversion functions
**Files:** `src/sema.rs` (`check_binary_op` mixed-type branch)
**Summary:** `1 + 2.0` is a hard `TypeMismatch` error; users must spell every conversion explicitly, but there are no conversion intrinsics yet either — `int(x)` and `float(x)` don't exist. The result is that mixed numeric arithmetic is currently *unspellable*. Acceptable today (no programs need it), but blocks any real numeric workload.
**Resolution:** Land conversion intrinsics first (`int(float) -> int`, `float(int) -> float`, with Cranelift `fcvt_to_sint_sat` / `fcvt_from_sint`). Decide at that point whether to keep mixed arithmetic as an error (Zig stance) or introduce limited widening (e.g. `int + float -> float` only when the int is a literal, Swift stance). Both are coherent; pick one and document.

### I-026 — Float modulo (`%` on `float`) rejected
**Files:** `src/sema.rs` (`check_binary_op` is_modulo branch)
**Summary:** `1.0 % 2.0` produces `"modulo operator '%' not supported for type 'float'"`. The plan deferred this because `fmod` has surprising semantics on negatives and on NaN, and there is no concrete user demand yet.
**Resolution:** When a real use case appears, decide between `libm::fmod` (C / IEEE remainder semantics) and a `frem`-style "sign of dividend" lowering, then add a `TirTag::FMod` and route `% on float` through it in sema. Document the chosen semantics in `docs/specification.md` before implementing.

### I-027 — Restricted float literal grammar
**Files:** `src/lexer.rs` (`RawToken::Float` regex `[0-9]+\.[0-9]+`)
**Summary:** Float literals must have digits on both sides of the dot. None of `.5`, `5.`, `1e10`, `1.5e-3`, `1_000_000.0` parse. Sufficient for M7's example programs but obviously incomplete.
**Resolution:** Extend the regex to cover `[0-9]+(_[0-9]+)*(\.[0-9]+(_[0-9]+)*)?([eE][+-]?[0-9]+)?` (or break it into named sub-patterns). Mirror the same underscore + exponent treatment for integer literals at the same time so the two grammars stay parallel. Watch out for ambiguities with method-call syntax (`5.bit_count()`) once methods land.

### I-028 — No `print(float)` (or `print` on anything but a `str` literal)
**Files:** `src/builtins.rs`, `src/sema.rs` (`check_builtin_call`), `src/codegen.rs` (`generate_print_call`)
**Summary:** Float arithmetic has no observability beyond the program exit code. `print` is hard-wired to take a *string literal* (see I-006). Inspecting a float at runtime requires either a formatter (`f"{x:.2}"`) or polymorphic `print`, neither of which exists.
**Resolution:** Tracked under I-006 (move `print` to a runtime crate). The float-specific piece lands when the runtime crate gains `print_f64` (or a polymorphic dispatch) and the sema-side argument-kind whitelist accepts non-`StrLiteral` `float` arguments.

### I-029 — AST loses `Eq` because `Literal::Float` carries an `f64`
**Files:** `src/ast.rs` (`Literal`, `Expression`, `Statement`, `Program`, `StmtKind`, `ExprKind`, `VarDecl`, `FunctionDef`)
**Summary:** `Literal::Float(f64)` cannot derive `Eq` (NaN ≠ NaN), and `Eq` derivation propagates up the containment chain, so every AST struct that transitively holds a `Literal` had to drop the `Eq` derive. No consumer hashes or `Eq`-compares AST nodes today, so the change is currently invisible.
**Resolution:** If a future pass needs `HashMap<Expression, _>` or similar, introduce a `FloatBits(u64)` newtype that derives `Eq + Hash` on the bit pattern and *also* implements `PartialEq` with IEEE semantics. Wrap `f64` inside `Literal::Float` with it. Until then, leave the derives off.

### I-030 — Unused chumsky 0.12 ergonomics worth revisiting
**Files:** `src/parser.rs`, `src/pipeline.rs`
**Summary:** chumsky 0.12 (released 2025-12-15) shipped several quality-of-life features. The 0.11 → 0.12 bump only adopted `Input::split_token_span` (replacing the `Stream::from_iter(...).map(eoi, |(t, s)| (t, s))` boilerplate at the lex/parse boundary). The remaining features are not currently a fit, but each becomes interesting as the parser grows:
- **`MapExtra::emit` / `InputRef::emit`** — emit *secondary* errors during mapping or in custom parsers without aborting the parse. Useful for soft-rejecting chained non-associative operators like `a < b < c` and `a == b == c` with a structured diagnostic, instead of the current "unexpected token" produced by the trailing operator falling off `or_not()`. Becomes attractive once parser diagnostics get their own `DiagCode` taxonomy (cf. I-014 for the lexer-side equivalent).
- **`spanned` combinator** — wraps a parser's output in `(O, Span)`. Today every `select! { ... }.map_with(|x, e| Foo::new(x, e.span()))` site builds the typed AST node directly, which is already one line; `spanned` would force an extra destructure. Worth reconsidering if/when the AST grows a uniform `Spanned<T>` wrapper instead of per-node `span` fields.
- **`labelled_with`** — label parsers without requiring `Clone` on the label value. Only relevant once the parser starts attaching labels for error-message quality; not used today.
- **`Parser::debug` (experimental)** — parser-level debugging utilities. Useful when triaging surprise grammar conflicts; pull in ad-hoc when needed, no permanent wiring required.
- **`IterParser::parse_iter` (experimental)**, **`nested_in` flexibility**, **`Input::split_spanned`** — no current call sites. `split_spanned` in particular is the `WrappingSpan`-flavoured sibling of `split_token_span`; we use plain `(Token, SimpleSpan)` tuples so the latter is the right fit.

**Resolution:** No action today. Revisit `MapExtra::emit` first when the parser gains structured diagnostics (likely alongside I-014's lexer-sink work, so parse and lex errors co-surface through the same `DiagSink`). Revisit `spanned` if the AST representation of spans is ever unified.

### I-034 — Builtin name comparison uses string compare instead of interned ID
**Files:** `src/sema.rs` (`check_call`, `check_builtin_call`)
**Summary:** `sema.pool.str(name_id) == "assert"` (and similar for `"panic"`, `"print"`) does a string dereference and byte comparison on every `check_call` invocation. Since the intern pool already deduplicates strings, comparing `name_id == assert_id` (where `assert_id` is cached once during builtin registration or sema init) would be a direct integer compare. Negligible today with three builtins and small programs, but the cost scales linearly with both the number of call sites and the number of builtins.
**Resolution:** Cache `StringId`s for each builtin name (e.g., in `Sema` or alongside `builtins::BUILTINS`) and match on the id instead of the string. Same applies to the codegen-side `name_str == "print"` comparisons.

### I-037 — Panic/Assert mechanism lacks `#file` / `#line` intrinsic expansion
**Files:** `src/sema.rs`, `src/codegen.rs`
**Summary:** The `panic` implementation bakes the source location (line, column) directly into a unique formatted string literal per call site at compile time. If a user asserts in ten places, the binary interns ten distinct copies of the assertion string format.
**Resolution:** Add macro-style `#file` and `#line` intrinsics or special UIR nodes (e.g. `InstTag::FileLoc`) to sema/codegen. `__ryo_panic` can then take `line` and `col` as integer arguments and construct the format string dynamically via `libc` functions or standard runtime printing, sharing the user's message string across sites.

### I-038 — Assert checks cannot be stripped in Release mode
**Files:** `src/sema.rs`, `src/codegen.rs`
**Summary:** Ryo has no mechanism to strip `assert` checks in `--release` configurations. The condition evaluates and branches at runtime unconditionally.
**Resolution:** Introduce a compilation mode flag (`--release` vs `--debug`) and strip `assert` AST/UIR nodes during semantic analysis when building for release. Provide a `precondition` or `fatal` variant that explicitly ignores the release flag for mandatory bounds checks.

### I-039 — `panic` provides no stack unwinding or stack traces
**Files:** `src/codegen.rs` (`__ryo_panic`)
**Summary:** A panic terminates execution instantly (`exit(101)`) and prints only the line/col of the `panic()` or `assert()` call site. If a shared utility function calls `panic`, the user gets no traceback to the caller.
**Resolution:** Add DWARF debug info generation to Cranelift (`.debug_line`, `.debug_info`, `.debug_frame`). Implement a simple stack walker in the runtime (e.g., `backtrace` from `libc` or via DWARF frame unwinding) to print the call stack inside `__ryo_panic`.
**Note:** DWARF emission is the shared prerequisite. Once it lands, interactive debugging via DAP ([Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/)) comes nearly for free — lldb already speaks DAP, so VS Code / JetBrains attach without Ryo-specific work. The stack-trace feature in `__ryo_panic` is additive runtime work on top of that same DWARF foundation.

### I-040 — Cascading `UndefinedVariable` after reserved-name rejection

**Files:** `src/sema.rs` (`InstTag::VarDecl`, `InstTag::AssignOrDecl`)
**Summary:** When a user writes `mut range = 42`, sema emits `ReservedBuiltinName` and deliberately skips inserting the binding. If the user subsequently references `range` (e.g., `print(range)`), they get a secondary `UndefinedVariable` error. This is standard single-pass behaviour but could be avoided by inserting an error-typed "poisoned" binding on rejection, so downstream references resolve silently without cascading diagnostics.
**Resolution:** On reserved-name rejection, call `scope.insert_binding(name, pool.error_type(), false)` instead of skipping the insert entirely. The error type is already treated as compatible with everything, so subsequent uses will type-check without emitting further errors.

### I-041 — `for-range` arity: only 2-arg form supported

**Files:** `src/parser.rs` (for-range parser)
**Summary:** Python allows `range(stop)` (implied start=0) and `range(start, stop, step)`. Ryo's parser strictly enforces `range(start, end)` (exactly 2 arguments). This is documented v0.1 behaviour. Users coming from Python will inevitably try `for i in range(10):` and receive a generic arity error.
**Resolution:** Consider supporting `range(end)` as sugar for `range(0, end)` in a future milestone. The 3-arg `range(start, end, step)` form requires a more complex increment block in codegen. Both are additive and non-breaking.

### I-042 — HashMap clone per for-loop iteration in codegen

**Files:** `src/codegen.rs` (`generate_for_range`, `emit_scoped_body`)
**Summary:** `generate_for_range` (and `emit_scoped_body` for while-loops) clones the entire `ctx.locals` HashMap to establish a temporary scope for the loop body, then restores it afterward. For typical functions with <20 locals this is negligible, but it scales linearly with both nesting depth and local count.
**Resolution:** Restructure `locals` as a scoped environment (e.g., linked list of scope frames or a `Vec`-backed stack with push/pop) rather than flat-map cloning. This is a codegen-internal refactor with no semantic impact. Low priority until profiling shows it matters.

---

## Cross-References

- Roadmap: [docs/dev/implementation_roadmap.md](docs/dev/implementation_roadmap.md)
- Spec: [docs/specification.md](docs/specification.md)
- Phase plan: [docs/dev/pipeline_alignment.md](docs/dev/pipeline_alignment.md)
