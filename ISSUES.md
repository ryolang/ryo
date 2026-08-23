# Known Issues

Compiler issues identified during source review. Each entry is independently actionable; severity reflects impact on correctness, future feature work, or code health — not user impact today (the compiler is pre-alpha).

Resolved entries are **removed** from this file. Language-visible decisions behind a resolution are recorded in `docs/specification.md`; for anything else, look at `git log` (or this file's history) for the removed entry. `I-xxx` references in code comments and `docs/dev/architecture_analysis.md` may point to removed entries.

---

## Severity Legend

- 🔴 **Blocking** — prevents implementing roadmap features as currently designed.
- 🟡 **Correctness/Hygiene** — silent bug or invariant gap; works today, will bite later.
- 🟢 **Cleanup** — code health, ergonomics, minor.

---

## 🟡 Correctness / Hygiene

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

### I-013 — `--emit` flag surface is fragmented across subcommands

**Files:** `ryo/src/main.rs`, `ryo-driver/src/pipeline.rs`
**Summary:** `lex`, `parse`, `ir` are separate subcommands. Each stage already exists and is wired up; users would benefit from a single `ryo build --emit=tokens|ast|hir|clif|obj` surface (mirroring `zig build-exe -femit-…`).
**Resolution:** Unify under one subcommand with an `--emit` flag.

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

### I-024 — Single `float` type, no `float32` / `float64` distinction

**Files:** `ryo-core/src/types.rs` (`Tag::Float`, `TypeKind::Float`), `ryo-backend/src/codegen.rs` (`cranelift_type_for`)
**Summary:** M7 ships one float type (`float`), lowered to Cranelift `F64`. Matches today's `int` (one width, machine-word). Users who need 32-bit floats for memory, GPU work, or C interop have no surface syntax to ask for one.
**Resolution:** Add `Tag::Float32` alongside the existing `Tag::Float` (which becomes `Float64` semantically), expose `: float32` / `: float64` annotations, and pick one as the default for unannotated `1.5`-style literals. Coordinate with the broader numeric-tower design (sized integers, `usize` / `isize`) so the widening story is consistent across types.

### I-025 — No implicit `int` ↔ `float` promotion or conversion functions

**Files:** `ryo-frontend/src/sema.rs` (`check_binary_op` mixed-type branch)
**Summary:** `1 + 2.0` is a hard `TypeMismatch` error; users must spell every conversion explicitly, but there are no conversion intrinsics yet either — `int(x)` and `float(x)` don't exist. The result is that mixed numeric arithmetic is currently *unspellable*. Acceptable today (no programs need it), but blocks any real numeric workload.
**Resolution:** Land conversion intrinsics first (`int(float) -> int`, `float(int) -> float`, with Cranelift `fcvt_to_sint_sat` / `fcvt_from_sint`). At that point introduce limited widening (e.g. `int + float -> float` only when the int is a literal, Swift stance). Document.

### I-026 — Float modulo (`%` on `float`) rejected

**Files:** `ryo-frontend/src/sema.rs` (`check_binary_op` is_modulo branch)
**Summary:** `1.0 % 2.0` produces `"modulo operator '%' not supported for type 'float'"`. The plan deferred this because `fmod` has surprising semantics on negatives and on NaN, and there is no concrete user demand yet.
**Resolution:** When a real use case appears, decide between `libm::fmod` (C / IEEE remainder semantics) and a `frem`-style "sign of dividend" lowering, then add a `TirTag::FMod` and route `% on float` through it in sema. Document the chosen semantics in `docs/specification.md` before implementing.

### I-027 — Restricted float literal grammar

**Files:** `ryo-frontend/src/lexer.rs` (`RawToken::Float` regex `[0-9]+\.[0-9]+`)
**Summary:** Float literals must have digits on both sides of the dot. None of `.5`, `5.`, `1e10`, `1.5e-3`, `1_000_000.0` parse. Sufficient for M7's example programs but obviously incomplete.
**Resolution:** Extend the regex to cover `[0-9]+(_[0-9]+)*(\.[0-9]+(_[0-9]+)*)?([eE][+-]?[0-9]+)?` (or break it into named sub-patterns). Mirror the same underscore + exponent treatment for integer literals at the same time so the two grammars stay parallel. Watch out for ambiguities with method-call syntax (`5.bit_count()`) once methods land.

### I-154 — No way to name infinity (or NaN) in Ryo source

**Files:** `ryo-frontend/src/lexer.rs` (`RawToken::Float` regex, cf. I-027), `ryo-frontend/src/builtins.rs`, `ryo-frontend/src/sema.rs`, `docs/specification.md`
**Summary:** There is no source-level spelling for IEEE infinity or NaN. The float literal grammar (`[0-9]+\.[0-9]+`, I-027) cannot express either — infinity has no decimal spelling, and the grammar has no exponent notation. IEEE edge cases are reachable at runtime (`1.0 / 0.0` yields `+inf`, see `examples/float_zero_div.ryo`) but can only be *detected* indirectly via identities like `x > 0.0 and x * 2.0 == x`, which is opaque and fragile. Almost no language spells infinity as a literal (Rust, Go, Python, C all use named constants), so this is a naming gap, not a grammar gap.
**Resolution:** Add `inf` as a predefined name that sema resolves to `FloatLit(f64::INFINITY.to_bits())` — same mechanism as the other builtins, no new literal grammar. Decide `nan` deliberately rather than by default: a `nan` constant makes `nan == nan` false in surface syntax, which is a real footgun; consider whether `x != x` suffices for NaN detection instead. This is a language design change — it requires explicit spec approval and a paragraph in the specification's literals/constants section before implementation.

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
**Summary:** `sema.pool.str(name_id) == "assert"` (and similar for `"panic"`, `"print"`) does a string dereference and byte comparison on every `check_call` invocation. Since the intern pool already deduplicates strings, comparing `name_id == assert_id` (where `assert_id` is cached once during builtin registration or sema init) would be a direct integer compare. Negligible today with three builtins and small programs, but the cost scales linearly with both the number of call sites and the number of builtins. Additional sites found in the M8.4.2 audit: `sema.rs:1489` and `:1945` compare `pool.str(name_id) == "str"` for the `str(view)` materialize intercept (explicitly *not* a `BUILTINS`-table entry, so a table-driven fix misses them), codegen detects `main` by `pool.str(tir.name) == "main"` at `codegen.rs:457, :496, :567, :955` (line refs refreshed 2026-08), and `sema.rs:281` does `name.starts_with("__ryo_")` per decl. New sites from the 2026-08 arena-perf review: `astgen.rs:355` compares `pool.str(iterator.name) != "range"` per for-loop, `astgen.rs:234` hash-probes `pool.find_str("main")` per function def (the already-interned id could be threaded through), and `sema.rs:2448` runs `check_reserved_builtin` per VarDecl.
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

### I-073 — Zig download has no integrity verification and races concurrent installs

**Files:** `ryo-backend/src/toolchain.rs` (`download_zig` :54-112)
**Summary:** The tarball is streamed HTTPS → XZ → tar with no sha256/signature check even though ziglang.org publishes shasums and `.minisig` files — a supply-chain gap. The fixed temp dir `.zig-{v}-downloading` (:62) lets two concurrent first-runs delete each other's in-flight download (`remove_dir_all` at :67), and `remove_dir_all(&desired_path)` (:101) can delete a working toolchain out from under another running compile.
**Resolution:** Hardcode the three pinned sha256s (one per supported target) and verify before extraction; use a pid-suffixed temp dir (matching `runtime_lib.rs`'s discipline) and atomic rename; never delete `desired_path` until the replacement is staged.

### I-076 — `str` ABI is hardcoded to 64-bit layout

**Files:** `ryo-backend/src/codegen.rs` (all str stack slots), `runtime/src/lib.rs` (`RyoStrFat`)
**Summary:** Every str stack slot hardcodes 24 bytes / align 3 / offsets 0,8,16 (:1607, :1667, :1727, :1800, :1833, :1894, :1956, :746-748), and `len`/`cap` are hardcoded `types::I64` (:404-405) while `ptr` is pointer-sized. On a 32-bit target, caller and callee layouts silently mismatch.
**Resolution:** Centralize the fat-pointer layout in one place (offsets and size computed from `module.target_config().pointer_type()`) and mirror it in the runtime. Prerequisite for any 32-bit target; interacts with I-021 (bool FFI width) when FFI lands.

### I-079 — Unary minus on `float` is rejected

**Files:** `ryo-frontend/src/sema.rs` (`InstTag::Neg` arm :932-953)
**Summary:** The `Neg` arm only handles `TypeKind::Int` (`INeg`); `-x` on a float operand emits `UnsupportedOperator` even though float arithmetic is otherwise fully supported (:1269-1279). Asymmetric and undocumented; smells like an oversight rather than a decision.
**Resolution:** Add `TirTag::FNeg` lowering to Cranelift `fneg` and accept `Float` in the `Neg` arm.

### I-080 — UIR/TIR `extra`-layout modules are duplicated with subtly different layouts

**Files:** `ryo-core/src/uir.rs` (`var_decl_extra` etc.), `ryo-core/src/tir.rs` (`call_extra` :337-342, `var_decl_extra` :355-362, `assign_extra`/… :370-418)
**Summary:** tir.rs re-defines near-identical `extra`-layout modules with different layouts: `call_extra` appends a modes tail; `var_decl_extra` drops the `TY` slot (`LEN: 3` vs uir's `4`). Same names, same constants, different meanings — a footgun when editing one side. `ExtraRange` itself is also byte-duplicated (`uir.rs:107-118` vs `tir.rs:87-98`), and `IfStmt` has no layout doc module at all in tir.rs (:677-715).
**Resolution:** Unify the shared pieces (`ExtraRange` at minimum) in one module; rename or document the layout differences explicitly; add the missing `if_stmt_extra` doc module.

### I-127 — In-tree `unsafe` sites don't meet the R5 bar

**Files:** `ryo-backend/src/codegen.rs` (:311, :314 — JIT `execute`), `ryo-core/src/types.rs` (:568 — `InternPool::str`)
**Summary:** R5 permits forced `unsafe` in tree code only with a `// SAFETY:` comment proving the invariant, a linked issue, and human sign-off. The only two compiler-side sites fall short: (a) `Codegen<JITModule>::execute` transmutes a finalized code pointer to `fn() -> isize` and calls `module.free_memory()` with no SAFETY comment and no linked issue at all; (b) `InternPool::str` uses `str::from_utf8_unchecked` with a SAFETY comment (append-only arena, only valid UTF-8 pushed) but no linked issue. Both unsafe blocks themselves are justified (cranelift-jit API / airtight interner invariant) — the gap is process, and R5 exists precisely so the set of in-tree unsafe stays audited.
**Resolution:** Add the `// SAFETY:` comment at the JIT site (function was finalized by cranelift-jit for this module; signature matches the compiled entry point; memory freed after execution), link this issue from both sites, and record the sign-off here. No code-semantics change.
**Status (2026-08-03):** SAFETY comments + linked issue are in place at both sites, and `unsafe_code = "deny"` now guards the rest of the tree via `[workspace.lints]` (compiler crates opt in; `runtime/` is the curated boundary). Remaining: human sign-off in review.

### I-155 — Ownership param-map `expect("param exists")` sites

**Files:** `ryo-frontend/src/ownership.rs` (:89, :1827, :2903, :2923 — `expect("param exists")`)
**Summary:** The ownership pass `expect("param exists")`s on its param-index map in four places — compiler panics on internal-invariant violation, invisible to the R9 diagnostics pipeline. (The sema `analyze_stmt`/`analyze_expr` fallthrough `panic!`s originally tracked here were folded into the documented UIR trusted-producer contract as `unreachable!` — resolution option (b) for the sema half.)
**Resolution:** Either route through the sink as internal-error diagnostics (R9), or convert to `unreachable!` with a comment folding them into the documented trusted-producer contract so the next audit covers them.

### I-132 — Runtime FFI boundary conflates failure modes and under-checks inputs

**Files:** `runtime/src/lib.rs` (`oom_abort` call sites :166, :170, :200, :204, :270, :353, :361-363, :404, :426, :440-441; null checks :105, :117, :272)
**Summary:** Two robustness gaps at the C-ABI boundary. (a) `oom_abort` is the handler for three distinct failure modes — genuine allocation failure, `u64 → usize` narrowing (32-bit-only), and `checked_add` capacity overflow — so all three abort identically with an OOM message. (b) `ryo_print` / `ryo_panic` / the slice path guard their pointer args with `debug_assert!(!ptr.is_null())` only; a release build passes a null pointer to `write`/`memcpy` unchecked when `len > 0`.
**Resolution:** Split `oom_abort` into distinct abort paths (or a reason-code parameter) so overflow/narrowing is distinguishable from OOM in the message; downgrade the null checks to real `if ptr.is_null() { abort }` guards at the FFI entry points — they cost one branch on a cold path.

### I-138 — `INT_MIN / -1` (and `% -1`) signed-overflow is UB at codegen

**Files:** `ryo-backend/src/codegen.rs` (`emit_div_zero_guard` call sites: `TirTag::ISDiv`, `TirTag::IMod`, compound-assign arms)
**Summary:** The zero-divisor guard covers `x / 0` and `x % 0`, but Cranelift `sdiv`/`srem` are also UB on signed overflow: `INT_MIN / -1` (and `INT_MIN % -1`) has no representable result. x86-64 `idiv` traps (#DE); aarch64 `sdiv` silently wraps to `INT_MIN`. Sema's literal-zero check doesn't catch it either (`x / -1` is a unary-minus expression, not a literal).
**Resolution:** Extend `emit_div_zero_guard` to also check `dividend == INT_MIN && divisor == -1`, branching to the same `ryo_panic` path with an "integer division overflow" message. Sema can reject the literal form `x / -1` only when the dividend is a known `INT_MIN` constant — likely not worth it; the runtime guard alone suffices.

---

## 🟢 Cleanup

### I-091 — UIR/TIR view decoders allocate a `Vec` per decode

**Files:** `ryo-core/src/uir.rs` (`call_view` :843-847, `if_stmt_view` :980-1001, `body_stmts` :320-326, `while_loop_view` :915, `for_range_view` :931-935, `method_call_view` :955-959), `ryo-core/src/tir.rs` (`call_view`), `ryo-backend/src/codegen.rs` (call view args/modes)
**Summary:** Every accessor decode collects refs out of `extra` into a fresh `Vec<InstRef>`/`Vec<TirRef>`, and `body_stmts()` collects a slice that is already contiguous. Sema and codegen call these in their hottest loops. Multipliers found in the 2026-08 arena-perf review: `Tir::walk_operands` (`tir.rs:1194-1266`) decodes views per visited instruction, so every `collect_reachable` costs several Vec allocs per inst; sema calls `uir.body_stmts(body)` twice per function (`sema.rs:478-479`); ownership calls `tir.body_stmts()` per whole-body-walk query (`ownership.rs:558, :619, :742, :802, :852, :1135, :1601, :1713, :1729`). Additionally `ExtraRange.len` is write-only metadata (decoders re-derive counts from inline `argc` words) — a second source of truth.
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

### I-095 — `emit_scoped_body` clones the locals maps per block

**Files:** `ryo-backend/src/codegen.rs` (:803-805)
**Summary:** Each if-arm/loop body clones the `locals`, `str_locals`, and `view_locals` HashMaps to get restore-on-exit semantics — O(locals) per block, quadratic-ish on deep nesting. (Entry predates `view_locals`; all three maps are cloned today.)
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
**Summary:** Correction of the earlier text: the AOT lanes DO have registered benchmarks — `codspeed.yml` (root, since e7efcc4) maps `fibonacci-aot` and `eager-destruction-aot` to the compiled binaries, and `codspeed run` executes `codspeed.yml` entries per the [CodSpeed CLI docs](https://codspeed.io/docs/cli). The walltime lane (`codspeed-macro`) and the memory lane (`ubuntu-latest`, eBPF-capable) should both report data, and the memory lane is the closest thing to automated validation of the "2× less heap" claim that exists. Real gaps: (1) both lanes set `allow-empty: true`, so any future drift (renamed binary, broken config, deleted `codspeed.yml`) silently degrades them to measuring nothing again — the I-085 pattern; (2) ~~no Cranelift-codegen/linking instrumented benchmarks~~ — addressed: `ryo-backend/benches/backend.rs` (simulation-mode codegen benches over the JIT module, run by the `backend-benchmarks` job); linking remains unmeasured; (3) the 2× ratio itself is computed by hand from `benchmarks/eager_destruction/run_benchmarks.sh`, not asserted anywhere.
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

### I-128 — Pass entry points far exceed the R7 size discipline

**Files:** measured by brace-depth scan, tests excluded — worst offenders: `ryo-frontend/src/ownership.rs` `visit_expr` :3633 (~411 lines), `analyze_function` :1560 (~383), `analyze_if_stmt` :2588 (~283); `ryo-frontend/src/sema.rs` `analyze_stmt` :483 (~370), `check_binary_op` :1210 (~264), `analyze_expr` :932 (~259), `emit_builtin_call` :1715 (~218), `check_call` :1475 (~215); `ryo-backend/src/codegen.rs` `emit_call` :2198 (~310), `eval_inst` :1272 (~249), `emit_stmt` :773 (~245), `compile_function` :448 (~228), `eval_inst_str` :1803 (~201); `ryo-frontend/src/parser.rs` `expression_parser` :400 (~228); `ryo-core/src/uir.rs` `write_inst` :1106 (~152) and the same pattern in `ryo-core/src/tir.rs:1155` (~108)
**Summary:** R7 targets functions under 50 lines so a human reviewer can hold each one in their head. Sixteen functions sit between ~150 and ~410 lines, almost all of them giant per-tag dispatch `match`es in the hottest passes. These are the files every milestone touches; review cost and merge-conflict surface scale with their length. (Distinct from I-090/I-094/I-111, which track *content* problems inside three of these functions, not size.)
**Resolution:** Split the entry points into one helper per tag/arm family (`lower_match_expr`-style naming per R7), keeping the dispatch match as a thin table. Do it opportunistically when a function is next touched for a feature — starting with `visit_expr` and `analyze_stmt`, the two worst — rather than as one big-bang refactor. `clippy::too_many_lines` is denied workspace-wide with `too-many-lines-threshold = 360` as a ratchet; lower the threshold towards 50 as functions split.

### I-129 — Dense-index state kept in `HashMap`s where R18 wants `Vec` side tables

**Files:** `ryo-frontend/src/ownership.rs` (`origin` :113, `owner_at_read` :140, `view_last_use` :201, `view_defer_loop` :214, `consumer_of` :1749, `program_order` :1298-1315 built per function at :1504 and :1620), `ryo-frontend/src/sema.rs` (`call_arg_refs: HashSet<InstRef>` :198, :204-215, queried at :1169), `ryo-backend/src/codegen.rs` (`freed_at: HashSet<usize>` :218, `free_by_after` :222, `locals`/`str_locals`/`view_locals` :188-231 keyed by dense `StringId`, sidecar maps `free_on_reassign`/`if_branches` from `ryo-core/src/ownership.rs:86-90`), plus the `collect_loop_nesting` map (`ownership.rs:1087-1137`, also per-statement `HashSet` allocations)
**Summary:** R18's rule: side tables keyed by a dense arena index belong in a `Vec` indexed by that index; hash maps are for sparse/string-keyed/unbounded data only. The ownership pass keeps five per-inst `HashMap<TirRef, _>` tables on the hot per-expression path, builds a whole-body `HashMap<TirRef, u32>` program-order map twice per function, and sema keeps a whole-program `HashSet<InstRef>` queried per `Borrow` inst — all keyed by dense `u32` arena indices. Codegen has the same class: per-statement `HashSet`/`HashMap` side tables keyed by `TirRef` or `StringId` (the `inst_values` memo was the worst of these and was converted to a `Vec` side table in the 2026-08 perf pass). Distinct from I-064/I-065/I-107/I-119, which cover recomputation and linear lookups in other helpers.
**Resolution:** Convert to `Vec<Option<…>>`/`Vec<bool>` side tables sized from the arena length (`TirRef::index()`/`InstRef::index()`), built once per function (per program for `call_arg_refs`). Same refactor shape as I-107's param-index map; do them together.

### I-134 — Stale in-tree comments found during the 2026-08-20 architecture re-verification

**Files:** `ryo-core/src/tir.rs` (:1-6), `ryo-core/src/uir.rs` (:1-6), `ryo-frontend/src/ownership.rs` (:1932-1934)
**Summary:** Three comments describe states the code has moved past: (a) the `tir.rs`/`uir.rs` module headers claim `ryo ir --emit=tir|uir` is "still TODO" — both are wired (`pipeline.rs:349,378-380,398-401`); (b) the dead-store drain comment says "Today no `free_on_reassign` entries exist; this guard activates with Task 6" — the field is populated and test-covered (`reassignment_records_free_on_old_owner`). (The `tir.rs:55` dangling issue-ID cite originally tracked here was dropped from the comment, per AGENTS.md.)
**Resolution:** One-pass comment sweep; no code changes. (a)/(b) reword to current behavior.

### I-135 — Rule-7 call-arg partition duplicates the view look-through logic

**Files:** `ryo-frontend/src/ownership.rs` (:3752-3758 — owner partition, :3847-3853 — E0031 span search)
**Summary:** The `mode == Borrow && tag == ViewAsStr → projection_root else underlying_owner` look-through is written out twice, near-verbatim, in two helpers that must agree for the P6'/E4 rules to stay coherent. A change to one side (e.g. a new look-through case) silently desynchronizes the diagnostic span search from the ownership partition.
**Resolution:** Extract one `fn call_arg_owner(own, tir, pool, mode, arg) -> Owner` helper used by both sites.

### I-136 — Ownership pass clones whole state maps on hot paths

**Files:** `ryo-frontend/src/ownership.rs` (:2799, :2819, :2840-2841 — 15-field `Ownership` clone per if/elif/else arm; :3072-3078 — four map clones + `sidecar.clone()` per propagate pass)
**Summary:** Every branch arm and every loop-propagate pass deep-clones the full ownership state (15 fields, several `HashMap`s). Correct, but against R3's allocation discipline on the hottest analysis path; the cost grows with function body size. (Codegen's per-block map clones are tracked separately as I-095.)
**Resolution:** After I-129 converts the dense-index maps to `Vec` side tables, replace whole-state clones with snapshot/restore of the four non-monotone fields only, or a copy-on-write per-arm overlay. Measure on the benchmark suite before and after.

### I-137 — No file-length gate; three files exceed 3000 lines

**Files:** `ryo-frontend/src/ownership.rs` (9504), `ryo-frontend/src/sema.rs` (4168), `ryo/tests/integration_tests.rs` (4002)
**Summary:** Nothing stops source files from growing unbounded; three files are already past the 3000-line mark used as the tidy limit (rust-lang `src/tools/tidy` convention, tests included). Full split plans with per-module anchors are in `docs/dev/architecture_analysis_2026_08_20.md` §4. Related to I-128 (function-level sizes) but distinct: this is file-level navigability, review surface, and merge-conflict scope.
**Resolution:** Add a tidy check to CI failing on `*.rs` files over 3000 lines with an explicit allowlist for the three current files; shrink the allowlist as the §4 splits land (`ownership/` and `sema/` module directories, per-area integration test binaries sharing `common/mod.rs`).

### I-140 — Cranelift upgrade 0.131.1 → 0.135.x (MSRV ladder + breaking removals)

**Files:** `Cargo.toml` (workspace deps), `Cargo.lock`, `scripts/check_cranelift.sh`, `ryo-backend/src/codegen.rs`
**Summary:** Ryo pins Cranelift 0.131.1; latest is 0.135.0. Upgrading is blocked on two things: (1) the MSRV ladder — 0.132 needs Rust 1.93, 0.133 needs 1.94, 0.135 needs 1.95; (2) instruction-set removals that surface at compile time — all `*_imm` instructions removed in 0.133 (`iadd_imm`, `imul_imm`, `icmp_imm`, `udiv_imm`, `sdiv_imm` — Ryo uses none of these today), and in 0.134 `global_value`, `band_not`/`bor_not`/`bxor_not`, `stack_load`/`stack_store` removed plus `MemFlags` renamed to `MemFlagsData`. No new overflow-detection instructions exist in 0.132–0.135, so the upgrade is not urgent for correctness.
**Resolution:** Bump the Cranelift workspace deps release-by-release with `./scripts/check_cranelift.sh <version>` review per step, fixing compile breaks from the removals above; confirm CI toolchains meet the MSRV of the target release before merging.

### I-141 — Adopt 0.134/0.135 guard-codegen and compile-time improvements after the Cranelift upgrade

**Files:** `ryo-backend/src/codegen.rs` (`emit_panic_guard` and the checked-arithmetic/div-zero call sites)
**Summary:** Two upstream changes directly benefit the panic guards added for div-by-zero and signed-overflow: 0.134 folds branch-to-trap patterns into single conditional traps in the egraph pass (#13688) and treats trapping blocks as cold during lowering (#13689); 0.135 reuses `regalloc2` context/output across function compilations and trims hashmaps on the lowering hot path, cutting compile time. These apply automatically once the upgrade (I-140) lands, but the guard codegen should be re-inspected to confirm the brif→panic-block shape actually gets the cold-block treatment (our guards branch to a `ryo_panic` call, not a raw `trap`, so #13688's trap folding does not apply — switching to `trapz`/`trapnz` was rejected because it would bypass the ryo_panic message/exit-code contract).
**Resolution:** After I-140, diff the emitted CLIF/disassembly of the overflow and div-zero test cases before/after the upgrade; verify guard blocks are laid out cold and measure compile time on the benchmark suite. Keep the explicit `ryo_panic` call convention.

### I-142 — Overflow guards fire on operations a value-range analysis could prove safe

**Files:** `ryo-backend/src/codegen.rs` (`emit_checked_iadd`, `emit_checked_isub`, `emit_checked_imul`)
**Summary:** Spec §18 checked arithmetic costs 3–4 machine instructions per integer op, and codegen currently elides a guard only when a constant operand makes it unreachable (`x + 0`, `x - 0`, `x * 0`, `x * 1`, non-zero constant divisors). Everything else pays, including operations whose operands are already bounded by a dominating comparison. `benchmarks/fibonacci/fib.ryo` is the worst case: `if n <= 1: return n` proves `n >= 2`, so neither `n - 1` nor `n - 2` can overflow, yet both are guarded. Measured cost of the guards on that benchmark — aarch64 (CodSpeed walltime runner): the `fibonacci` hot path grows from 19 to 29 instructions per call, 1.10 s → 1.47 s for `fib(40)` (+33%); x86-64 (callgrind, `fib(28)`): 19.7 M → 25.3 M instructions (+29%). Encoding experiments (`icmp`-based checks that Cranelift fuses into one compare-and-branch, comparisons against precomputed boundary constants, `trapnz` instead of a branch to the shared panic block) all moved the totals by ≤4% in either direction on one ISA while regressing the other — the cost is the number of checks, not their encoding. Two upstream gaps add to it on aarch64: Cranelift materialises the constant operand into a register instead of using the immediate form (`mov x1, #1` + `subs x1, x0, x1`), and branches on the overflow flag through `cset` + `uxtb` + `cbnz` instead of `b.vs`.
**Resolution:** Give codegen a small value-range fact map (variable → inclusive bounds) seeded from dominating `if`/`while` comparisons against constants — including the fall-through path of an if whose arms all terminate, which is the fibonacci shape — and skip the guard when the operand bounds make overflow impossible. Facts must be invalidated on assignment, on `inout` argument passing, and at every join whose predecessors disagree; each elision needs a pinning test at the boundary value, since a wrong one silently drops a mandated trap. Until then the checked-arithmetic cost on arithmetic-heavy code is expected and matches other trap-on-overflow languages (Swift is ~1.28× Rust on the same benchmark).

### I-144 — Per-if clone and repeated dead-drop scans in codegen

**Files:** `ryo-backend/src/codegen.rs` (`if_branches.get(...).cloned().unwrap_or_default()` :1154, `conditional_dead_drops` linear scan :1164-1169, `emit_conditional_dead_drops` :2092-2114 called per arm :1195/:1231/:1247/:1263)
**Summary:** Every if-statement clones the `IfBranchIds` payload (heap `Vec` for elif branches) out of the sidecar even when there is no entry, because `.cloned().unwrap_or_default()` goes through `ctx`. Separately, `emit_conditional_dead_drops` re-scans the whole per-function `conditional_dead_drops` Vec at the start of *every* if arm with no empty-check early exit, and re-imports `ryo_str_free` inside the drop loop (:2108, cross-ref I-093). On if-heavy functions with dead drops this is O(ifs × arms × drops).
**Resolution:** Borrow the sidecar out of `ctx` first so `get` returns a reference instead of cloning; add the same `is_empty()` early-return `emit_due_frees` already has (:1960) or index dead drops by `if_stmt` in a map built once per function; hoist the `ryo_str_free` import out of the loop.

### I-145 — Ownership materializes the full states map per break/continue

**Files:** `ryo-frontend/src/ownership.rs` (`schedule_break_continue_frees` :3537-3539, per-jump scans :3500-3532)
**Summary:** Every break/continue jump clones the entire `own.states` map into a sorted `Vec`, then builds `on_path`/`covers_this_jump`/`free_inside_loop` sets and scans the whole `free_schedule` — all per jump, though the snapshot is constant within a loop body walk. I-064 precomputed the per-loop invariants; this per-jump residue was out of its scope.
**Resolution:** Hoist the sorted snapshot to once per loop body walk (or iterate the map with an index); reuse scratch sets across jumps.

### I-146 — `collect_view_liveness` clones the bindings map per if/arm/loop

**Files:** `ryo-frontend/src/ownership.rs` (:1260-1319, :1346)
**Summary:** The view-liveness pre-walk clones the full `bindings` map per if statement (`pre = bindings.clone()`) and again per arm (:1278, :1294, :1310), plus per-arm fresh read maps, and clones per loop body (:1346). Same class as I-136's merge-path clones but a different pass, so I-136's resolution won't sweep it up unless extended.
**Resolution:** Apply the same snapshot/undo-log or overlay approach chosen for I-136; fix both passes together.

### I-147 — `emit_builtin_call` allocates mode Vecs per builtin call

**Files:** `ryo-frontend/src/sema.rs` (:1922-1932)
**Summary:** Every `print`/`panic`/`assert`/conversion call site builds `vec![ParamMode::Borrow; arg_tirs.len()]` and clones it — two allocations per builtin call though builtin arities and modes are statically known. Adjacent to I-092(b), which covers `check_call` but not the builtin path.
**Resolution:** Static per-builtin mode tables; only `str_push` needs a non-uniform one.

### I-148 — Per-argument callee-name string lookups in the ownership pass

**Files:** `ryo-frontend/src/ownership.rs` (`is_borrowed_scalar_param` :3665, `view_borrow_params` :3735), `ryo-frontend/src/builtins.rs` (:128-148)
**Summary:** `is_borrowed_scalar_param` runs `pool.str(name_id)` plus two linear `&'static str` table scans *per argument of every call*, though the result depends only on the callee; `view_borrow_params` repeats it per borrow-mode Call arg. Same string-compare class as I-034, but the per-arg (not per-call) repetition is a new facet.
**Resolution:** Hoist the lookup out of the arg loop (once per Call inst); the longer-term fix is I-034's cached-`StringId` table.

### I-149 — Lexer allocates a `String` per escape-free string literal

**Files:** `ryo-frontend/src/lexer.rs` (`unescape` :425-491, called at :542)
**Summary:** Every string literal gets an owned `String` from `unescape` even when it contains no escapes — the common case. Per string literal.
**Resolution:** Fast-path with `memchr(b'\\')` (or a byte scan) returning `Cow::Borrowed(inner)` when no escape is present; build the owned string only on the escape path.

### I-150 — Each function's Cranelift `Signature` is built twice

**Files:** `ryo-backend/src/codegen.rs` (`declare_all_functions` :455, `compile_function` :555)
**Summary:** `declare_all_functions` builds every function's `Signature` to register the `FuncId`, then `compile_function` rebuilds the identical signature — redundant pool queries and two Vec allocations per function.
**Resolution:** Store the `Signature` alongside the `FuncId` in `func_ids` and move/clone it into `ctx.func`.

### I-151 — `collect_loop_nesting` allocates per statement and per instruction

**Files:** `ryo-frontend/src/ownership.rs` (:1087-1137)
**Summary:** Once per function, but O(body²) worst case: a fresh `HashSet` + `collect_reachable` per body statement (:1095-1096), then `inner.clone()` (:1105) / `stack.to_vec()` (:1118) per subtree instruction into a `HashMap<TirRef, Vec<TirRef>>`. Dense-index keyed — same R18 class as I-129 (listed there); the per-statement set allocations are the extra cost.
**Resolution:** Reuse one scratch set (clear between statements); share nesting stacks via parent-pointer chains or a `Vec<Vec<TirRef>>` indexed by depth; fold into the I-129 side-table conversion.

### I-152 — Parser builds a throwaway `Vec` per call/params node before the arena copy

**Files:** `ryo-frontend/src/parser.rs` (:604-615, :650-661, :534-538, :436-446)
**Summary:** Call args, method args, params, and elif branches are `collect::<Vec<_>>()`ed into a temporary, copied into the AST side arena by the builder, then dropped — a double buffer per node. Partly inherent to chumsky's `IterParser`; impact is small next to the win the arena already delivered.
**Resolution:** A custom collector writing straight into the arena (chumsky 0.12 collects via `FromIterator`, so an arena-append adapter is feasible), or accept as-is. Measure before bothering.

### I-153 — `expect_used` audit before promoting to deny

**Files:** the `cargo clippy --all-targets -- -W clippy::expect_used` hit list (`ryo-frontend/src/ownership.rs`, `ryo-core/src/ast.rs`, `ryo-core/src/types.rs`, `ryo-core/src/uir.rs`, `ryo-core/src/tir.rs` are the dense ones)
**Summary:** `expect_used` is the one panic-family lint still at `allow` in `[workspace.lints.clippy]` (`panic`/`todo`/`unimplemented`/`unwrap_used` are denied). 70 sites fire at last count, 56 of them outside `ryo/tests/`; many are deliberate arena-boundary guards (`from_index`, side-arena overflow checks) — legitimate invariant enforcement, not laziness.
**Resolution:** Classify each site as keep-with-message (genuine internal invariant) vs convert-to-diagnostic (reachable from user input), then consider promoting `expect_used` to `deny`.

---

## Cross-References

- Architecture analysis: [docs/dev/architecture_analysis.md](docs/dev/architecture_analysis.md), refreshed at [docs/dev/architecture_analysis_2026_08_20.md](docs/dev/architecture_analysis_2026_08_20.md) (source of I-131–I-137)
- Roadmap: [docs/dev/implementation_roadmap.md](docs/dev/implementation_roadmap.md)
- Spec: [docs/specification.md](docs/specification.md)
- Phase plan: [docs/dev/pipeline_alignment.md](docs/dev/pipeline_alignment.md)
