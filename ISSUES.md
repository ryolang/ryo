# Known Issues

Compiler issues identified during source review. Each entry is independently actionable; severity reflects impact on correctness, future feature work, or code health — not user impact today (the compiler is pre-alpha).

Resolved entries are **removed** from this file (convention changed in M8.4.1 — before that they were marked in place with a `**Resolved:** ✅ <milestone> (<date>)` line). Language-visible decisions behind a resolution are recorded in `docs/specification.md`; for anything else, look at `git log` (or this file's history) for the removed entry. `I-xxx` references in code comments and `docs/dev/architecture_analysis.md` may point to removed entries.

---

## Fix Order for m8.4.2

Sorted by priority; do them top-down. Already-fixed entries are removed from this file — see git log.

1. **I-054** — `parse_source` / lex error paths bypass `finalize_diags`.
2. **I-085** — valgrind smoke tests silently pass when valgrind is absent.
3. **I-014 + I-015 + I-016 + I-077 + I-078** — lexer diagnostic hygiene (fold together).
4. **I-017** — `i64::MIN` integer literal is unrepresentable.
5. **I-088** — ownership sidecar keyed by function name.
6. **I-121** — staged zig zip carried into the toolchain dir on fallback rename.

Everything below this line (remaining 🟡 spec/design items and the 🟢 cleanup tier) is post-m8.4.2 unless it blocks one of the above.

---

## Severity Legend

- 🔴 **Blocking** — prevents implementing roadmap features as currently designed.
- 🟡 **Correctness/Hygiene** — silent bug or invariant gap; works today, will bite later.
- 🟢 **Cleanup** — code health, ergonomics, minor.

---

## 🔴 Blocking

### I-125 — Parser has zero error recovery; one syntax error discards the whole AST

**Files:** `ryo-frontend/src/parser.rs` (`program_parser` :39-65), `ryo-driver/src/pipeline.rs` (`parse_source` :127-143), `ryo-core/src/ast.rs`
**Summary:** R10 requires the parser to emit a diagnostic, synchronize at the next statement/declaration boundary, and produce a partial AST with `Error` nodes. None of that exists: no `recover_with`/`skip_until` combinator anywhere in the parser, no `Error` node kind in the AST, and `parse_source` calls `.into_result()` and returns early on `Err` — so a single syntax error suppresses *all* semantic diagnostics in the file (one bad function stops analysis of the rest, violating R9's accumulation rule at the parse stage). This blocks the IDE/REPL use cases R10 exists for, and it caps diagnostic quality for every later milestone.
**Resolution:** Add an `ExprKind::Error` / `StmtKind::Error` (or a dedicated `Error` literal node) to the AST, thread it through astgen/sema as error-typed no-ops (sema already has error-typed `Unreachable` slots to model from), and add chumsky `recover_with(skip_until(...))` synchronization at statement and declaration boundaries. Golden-test multi-error files. Natural sequencing: after I-014's lexer `DiagSink` migration so lex and parse errors co-surface through one sink (cf. I-030's `MapExtra::emit` note).

---

## 🟡 Correctness / Hygiene

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
**Summary:** `sema.pool.str(name_id) == "assert"` (and similar for `"panic"`, `"print"`) does a string dereference and byte comparison on every `check_call` invocation. Since the intern pool already deduplicates strings, comparing `name_id == assert_id` (where `assert_id` is cached once during builtin registration or sema init) would be a direct integer compare. Negligible today with three builtins and small programs, but the cost scales linearly with both the number of call sites and the number of builtins. Additional sites found in the M8.4.2 audit: `sema.rs:1489` and `:1945` compare `pool.str(name_id) == "str"` for the `str(view)` materialize intercept (explicitly *not* a `BUILTINS`-table entry, so a table-driven fix misses them), codegen detects `main` by `pool.str(tir.name) == "main"` at `codegen.rs:430, :480, :861`, and `sema.rs:281` does `name.starts_with("__ryo_")` per decl.
**Resolution:** Cache `StringId`s for each builtin name (e.g., in `Sema` or alongside `builtins::BUILTINS`) and match on the id instead of the string. Same applies to the codegen-side `name_str == "print"` comparisons. Also intern `"str"`, `"main"`, and the `"__ryo_"` prefix check — the materialize intercept and `main` detection are not covered by a BUILTINS-table-driven fix.

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

### I-047 — UIR `is_move` field is a pass-through

**Files:** `ryo-core/src/uir.rs` (`UirParam`), `ryo-frontend/src/astgen.rs`, `ryo-frontend/src/sema.rs`
**Summary:** `is_move` is threaded lexer → parser → AST → UIR → TIR. The UIR copy is never read: astgen propagates the AST flag in, sema reads it back out into `TirParam`, and no UIR pass inspects it. UIR is structural lowering with no semantic meaning, so `UirParam::is_move` is dead weight that exists only to bridge two layers it shouldn't.
**Resolution:** Drop `UirParam::is_move`. Sema can read the flag straight from the AST `FuncBody` (or via a side-channel keyed by FuncBody) when it constructs `TirParam`. Wait until any other UIR-level pass needs the flag before re-introducing it.

### I-054 — `parse_source` and lex error paths bypass `finalize_diags`

**Files:** `ryo-driver/src/pipeline.rs` (`parse_source`, `display_tokens`)
**Summary:** `finalize_diags` consolidates the drain + render + `Err(CompilerError::Diagnostics(_))` shape for the sink-using stages (`lower_and_analyze`, `ir_command`). The lex error paths and `parse_source`'s parse-error branch still hand-roll the same pattern over a `Vec<Diag>` they build directly (no `DiagSink`). Drift risk is real: parse/lex paths skip the `Severity::Error` filter (they assume every diag they emit is an error, which holds today but isn't enforced), and any future change to the rendering convention has to be applied in three places.
**Resolution:** Generalize `finalize_diags` to take `Vec<Diag>` (or `impl IntoIterator<Item = Diag>`); have `DiagSink::into_diags()` feed the new entry point. Then the three lex/parse error paths become `finalize_diags(vec![diag], input, &name)` and the render+wrap pattern lives in exactly one place. Folds naturally with I-014's lexer-DiagSink migration.

### I-073 — Zig download has no integrity verification and races concurrent installs

**Files:** `ryo-backend/src/toolchain.rs` (`download_zig` :54-112)
**Summary:** The tarball is streamed HTTPS → XZ → tar with no sha256/signature check even though ziglang.org publishes shasums and `.minisig` files — a supply-chain gap. The fixed temp dir `.zig-{v}-downloading` (:62) lets two concurrent first-runs delete each other's in-flight download (`remove_dir_all` at :67), and `remove_dir_all(&desired_path)` (:101) can delete a working toolchain out from under another running compile.
**Resolution:** Hardcode the three pinned sha256s (one per supported target) and verify before extraction; use a pid-suffixed temp dir (matching `runtime_lib.rs`'s discipline) and atomic rename; never delete `desired_path` until the replacement is staged.

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

### I-085 — Valgrind smoke tests silently pass when valgrind is absent

**Files:** `ryo/tests/valgrind_smoke.rs` (:36-40)
**Summary:** `run_valgrind_smoke` prints "skipping" and returns success when valgrind is not installed, so local green runs may have exercised nothing. Only the CI lane that `apt-get install`s valgrind guarantees coverage; the suite exists because LSan misses leaks from Cranelift-emitted code (not ASan-instrumented).
**Resolution:** Fail loudly (or require an opt-in env var to skip) outside CI; at minimum print a prominent end-of-suite summary of skipped tests.

### I-088 — Ownership sidecar is keyed by function name

**Files:** `ryo-core/src/ownership.rs` (`OwnershipSidecar` :50-52), `ryo-frontend/src/ownership.rs` (:301-309)
**Summary:** `functions` is a `HashMap<StringId, FunctionSidecar>` keyed by interned name. Correct today because `TirRef` arenas restart per function and names are unique (I-075 notwithstanding), but any future overloading or same-name functions in different scopes will silently collide.
**Resolution:** Key by `DeclId`/body index (positional with `Vec<Tir>`) when the declaration model supports it.

### I-126 — AST is a `Box<Expression>` pointer tree, contradicting R1

**Files:** `ryo-core/src/ast.rs` (`ExprKind` :412-428 — `BinaryOp`, `UnaryOp`, `MethodCall.receiver`, `Borrow`, `Slice` all box children), consumed by `ryo-frontend/src/astgen.rs` (`gen_expr` :370)
**Summary:** R1 is absolute: *never* `Box<Node>` trees in compiler IR — flat `Vec`s with `u32` newtype indices, hot tags split from cold payloads. The AST is the one pipeline IR that still violates it: a recursive pointer tree with per-node heap allocation (R3's "count every allocation" applies too). Everything downstream (UIR/TIR) already follows the flat-arena discipline, so the AST is the outlier, and every new expression form deepens the tree shape that will eventually have to be rewritten. Works today; bites later — the rewrite only gets bigger.
**Resolution:** Migrate `ast.rs` to a flat `Ast { tags: Vec<ExprTag>, payloads, spans }` struct-of-arrays with `ExprId(u32)` indices, mirroring the UIR/TIR encoding already in tree. Parser builds into the arena; astgen walks indices instead of `&Box`. Sizeable but mechanical; do it before the AST gains many more node kinds, and sequence it with I-012 (extract `pretty_print`) and I-029 (`FloatBits`) since all three touch the same file.

### I-127 — In-tree `unsafe` sites don't meet the R5 bar

**Files:** `ryo-backend/src/codegen.rs` (:311, :314 — JIT `execute`), `ryo-core/src/types.rs` (:568 — `InternPool::str`)
**Summary:** R5 permits forced `unsafe` in tree code only with a `// SAFETY:` comment proving the invariant, a linked issue, and human sign-off. The only two compiler-side sites fall short: (a) `Codegen<JITModule>::execute` transmutes a finalized code pointer to `fn() -> isize` and calls `module.free_memory()` with no SAFETY comment and no linked issue at all; (b) `InternPool::str` uses `str::from_utf8_unchecked` with a SAFETY comment (append-only arena, only valid UTF-8 pushed) but no linked issue. Both unsafe blocks themselves are justified (cranelift-jit API / airtight interner invariant) — the gap is process, and R5 exists precisely so the set of in-tree unsafe stays audited.
**Resolution:** Add the `// SAFETY:` comment at the JIT site (function was finalized by cranelift-jit for this module; signature matches the compiled entry point; memory freed after execution), link this issue from both sites, and record the sign-off here. No code-semantics change.
**Status (2026-08-03):** SAFETY comments + linked issue are in place at both sites, and `unsafe_code = "deny"` now guards the rest of the tree via `[workspace.lints]` (compiler crates opt in; `runtime/` is the curated boundary). Remaining: human sign-off in review.

---

## 🟢 Cleanup

### I-089 — `ParamMode::from_u32` silently coerces unknown values to `Borrow`

**Files:** `ryo-core/src/tir.rs` (:240-246)
**Summary:** Decoding an unknown mode word yields `Borrow` — the least restrictive convention — instead of an error. Harmless today; a footgun if the enum grows or a payload is corrupted.
**Resolution:** Return `Option<ParamMode>` and internal-error on unknown values at the single decode site (`call_view`).

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
**Summary:** `include_bytes!` bakes the full staticlib into the compiler binary. The archive is `no_std` since I-043 (std, and the `_Unwind_*` link wart with it, is gone), but it still bundles all of core's precompiled objects, which is what keeps it large. Measured locally: 17.9 MB debug / 17.7 MB release.
**Resolution:** Build the embedded archive with a slim profile (`opt-level="z"`, strip, LTO — the build scripts control that invocation). The `no_std` half of the original resolution (I-043) already landed and did not shrink the archive on its own.

### I-098 — Integration tests spawn `cargo run` per test

**Files:** `ryo/tests/integration_tests.rs` (`run_ryo_command` :7-16)
**Summary:** 149 of 155 tests invoke `cargo run --` as a subprocess, paying cargo's startup, a workspace freshness re-check, and build.rs execution (git calls + sha256 of the runtime archive) per test. The smoke-test harness already demonstrates the cheap pattern: `env!("CARGO_BIN_EXE_ryo")` (`ryo/tests/common/mod.rs:11,38`).
**Resolution:** Point `run_ryo_command` at `env!("CARGO_BIN_EXE_ryo")`; keep one `cargo run` smoke test to cover that entry path. Largest single test-time win available.

### I-099 — `run_file` debug output is load-bearing for the integration suite

**Files:** `ryo-driver/src/pipeline.rs` (`run_file` :457-483), `ryo/tests/integration_tests.rs`
**Summary:** `ryo run` echoes `[Input Source]`, the full AST, and `[Codegen]` on every invocation; ~63 test assertions key on `"[Result] => 0"` and the section headers as the pass/fail signal, and tests post-filter stdout (split on `"[Codegen]"`). Any cleanup of the chatter breaks the suite.
**Resolution:** Gate the debug sections behind a `--verbose` flag, then migrate tests to exit-code assertions; do the harness migration (with I-098) before touching `run_file`.

### I-100 — CodSpeed AOT lanes are unverified and masked by `allow-empty`; no backend benchmarks

**Files:** `.github/workflows/codspeed.yml` (:53-111), `codspeed.yml` (repo root), `ryo-frontend/benches/frontend.rs`
**Summary:** Correction of the earlier text: the AOT lanes DO have registered benchmarks — `codspeed.yml` (root, since e7efcc4) maps `fibonacci-aot` and `eager-destruction-aot` to the compiled binaries, and `codspeed run` executes `codspeed.yml` entries per the [CodSpeed CLI docs](https://codspeed.io/docs/cli). The walltime lane (`codspeed-macro`) and the memory lane (`ubuntu-latest`, eBPF-capable) should both report data, and the memory lane is the closest thing to automated validation of the "2× less heap" claim that exists. Real gaps: (1) both lanes set `allow-empty: true`, so any future drift (renamed binary, broken config, deleted `codspeed.yml`) silently degrades them to measuring nothing again — the I-085 pattern; (2) no Cranelift-codegen/linking instrumented benchmarks (frontend benches cover the frontend only); (3) the 2× ratio itself is computed by hand from `benchmarks/eager_destruction/run_benchmarks.sh`, not asserted anywhere.
**Resolution:** Root cause of the historical empty output found (2026-07): the jobs passed `run: codspeed run`, nesting the CLI inside the action's runner — CodSpeed's docs state config-file benchmarks must OMIT `run:` so the action reads `codspeed.yml` directly. Fixed by dropping `run:` from both AOT jobs and removing `allow-empty: true` so any future drift (renamed binary, broken config, deleted `codspeed.yml`) fails CI loudly. Remaining: verify in the CodSpeed dashboard that both lanes report data for both registered benchmarks; optionally add a CI step asserting eager_destruction's peak RSS stays under a fixed bound (the manual script's check, automated).

### I-102 — Smoke suites duplicate work across lanes and fixture builds

**Files:** `ryo/tests/asan_smoke.rs`, `ryo/tests/valgrind_smoke.rs`, `ryo/tests/common/mod.rs`, `.github/workflows/ci.yml` (:83)
**Summary:** Both suites iterate the same 11 fixtures (`common/mod.rs:81-210`), compiling+linking each twice per full run; each `build_and_link` also shells out to `ryo toolchain status --path` to find zig (:10-22). `cargo test --workspace` in the test lane already includes `asan_smoke`, so it runs twice on ubuntu (test lane + dedicated asan lane); valgrind "runs" (silently skips, I-085) in lanes without valgrind.
**Resolution:** Share fixture compilation across suites, cache the zig path, and exclude the smoke suites from the default test lane (or from the dedicated lanes).

### I-104 — `ryo-core` depends on chumsky solely for `SimpleSpan`

**Files:** `ryo-core/src/diag.rs` (:18-20), `ryo-core/Cargo.toml`
**Summary:** The "core" IR/types crate pulls in a parser crate for one span type, coupling every consumer of `ryo-core` to chumsky's release cycle.
**Resolution:** Define a small `Span` newtype in `ryo-core` and convert at the parser boundary (`pipeline.rs` already adapts spans).

### I-109 — No instruction→function reverse mapping in UIR

**Files:** `ryo-core/src/uir.rs` (`func_bodies` :272, :279-284)
**Summary:** `func_bodies` lists only top-level statement refs; given an arbitrary `InstRef` you cannot tell which function owns it without walking every body. Any pass wanting per-function slices of the shared arena (diagnostics, per-function codegen, future incremental sema) re-derives this by traversal.
**Resolution:** Add a computed inst→body index map (built lazily or at `finish()`), or move to per-function UIR arenas mirroring TIR when Phase 5 lands.

### I-111 — Lexer token boilerplate is four touch points per variant

**Files:** `ryo-frontend/src/lexer.rs` (`RawToken` :176-300, `Token` :30-103, `intern_token` :392-495, `Display` :105-170)
**Summary:** Adding a token means editing `RawToken`, `Token`, the giant manual `intern_token` match, and `Display` (plus the parser downstream) — ~45 non-payload variants of pure boilerplate.
**Resolution:** Generate the quadruple from a single macro table (variant name, logos pattern, payload kind).

### I-121 — Staged zig zip is carried into the toolchain dir on fallback rename

**Files:** `ryo-backend/src/toolchain.rs` (`download_zig` fallback rename; staged `zig-download.zip`)
**Summary:** The zip path stages the download as `zig-download.zip` inside the temp dir and never deletes it after `extract_zip` succeeds. If a future zig zip ever lacks the `zig-{target}-{version}/` top-level directory, `source` falls back to `temp_path` and the rename carries the ~100 MB staged archive into the installed toolchain dir. Inert (zig still runs), but sloppy, and the staged file is never cleaned up on the happy path either.
**Resolution:** `fs::remove_file(&zip_path).ok()` immediately after `extract_zip` succeeds.

### I-128 — Pass entry points far exceed the R7 size discipline

**Files:** measured by brace-depth scan, tests excluded — worst offenders: `ryo-frontend/src/ownership.rs` `visit_expr` :3633 (~411 lines), `analyze_function` :1560 (~383), `analyze_if_stmt` :2588 (~283); `ryo-frontend/src/sema.rs` `analyze_stmt` :483 (~370), `check_binary_op` :1210 (~264), `analyze_expr` :932 (~259), `emit_builtin_call` :1715 (~218), `check_call` :1475 (~215); `ryo-backend/src/codegen.rs` `emit_call` :2198 (~310), `eval_inst` :1272 (~249), `emit_stmt` :773 (~245), `compile_function` :448 (~228), `eval_inst_str` :1803 (~201); `ryo-frontend/src/parser.rs` `expression_parser` :400 (~228); `ryo-core/src/uir.rs` `write_inst` :1106 (~152) and the same pattern in `ryo-core/src/tir.rs:1155` (~108)
**Summary:** R7 targets functions under 50 lines so a human reviewer can hold each one in their head. Sixteen functions sit between ~150 and ~410 lines, almost all of them giant per-tag dispatch `match`es in the hottest passes. These are the files every milestone touches; review cost and merge-conflict surface scale with their length. (Distinct from I-090/I-094/I-111, which track *content* problems inside three of these functions, not size.)
**Resolution:** Split the entry points into one helper per tag/arm family (`lower_match_expr`-style naming per R7), keeping the dispatch match as a thin table. Do it opportunistically when a function is next touched for a feature — starting with `visit_expr` and `analyze_stmt`, the two worst — rather than as one big-bang refactor. `clippy::too_many_lines` is denied workspace-wide with `too-many-lines-threshold = 500` as a ratchet; lower the threshold towards 50 as functions split.

### I-129 — Dense-index state kept in `HashMap`s where R18 wants `Vec` side tables

**Files:** `ryo-frontend/src/ownership.rs` (`origin` :113, `owner_at_read` :140, `view_last_use` :201, `view_defer_loop` :214, `consumer_of` :1749, `program_order` :1298-1315 built per function at :1504 and :1620), `ryo-frontend/src/sema.rs` (`call_arg_refs: HashSet<InstRef>` :198, :204-215, queried at :1169)
**Summary:** R18's rule: side tables keyed by a dense arena index belong in a `Vec` indexed by that index; hash maps are for sparse/string-keyed/unbounded data only. The ownership pass keeps five per-inst `HashMap<TirRef, _>` tables on the hot per-expression path, builds a whole-body `HashMap<TirRef, u32>` program-order map twice per function, and sema keeps a whole-program `HashSet<InstRef>` queried per `Borrow` inst — all keyed by dense `u32` arena indices. Distinct from I-064/I-065/I-107/I-119, which cover recomputation and linear lookups in other helpers.
**Resolution:** Convert to `Vec<Option<…>>`/`Vec<bool>` side tables sized from the arena length (`TirRef::index()`/`InstRef::index()`), built once per function (per program for `call_arg_refs`). Same refactor shape as I-107's param-index map; do them together.

---

## Cross-References

- Architecture analysis: [docs/dev/architecture_analysis.md](docs/dev/architecture_analysis.md)
- Roadmap: [docs/dev/implementation_roadmap.md](docs/dev/implementation_roadmap.md)
- Spec: [docs/specification.md](docs/specification.md)
- Phase plan: [docs/dev/pipeline_alignment.md](docs/dev/pipeline_alignment.md)
