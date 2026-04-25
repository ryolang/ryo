# Known Issues

Compiler issues identified during source review. Each entry is independently actionable; severity reflects impact on correctness, future feature work, or code health — not user impact today (the compiler is pre-alpha).

---

## Severity Legend

- 🔴 **Blocking** — prevents implementing roadmap features as currently designed.
- 🟡 **Correctness/Hygiene** — silent bug or invariant gap; works today, will bite later.
- 🟢 **Cleanup** — code health, ergonomics, minor.

---

## 🔴 Blocking

### I-003 — No control flow in the compiler
**Files:** `src/lexer.rs`, `src/parser.rs`, `src/hir.rs`, `src/codegen.rs`
**Summary:** `if`/`else`/`match` are lexed as keywords but have no parser, no HIR variants, and no codegen support (no block branching or phi handling). This is the single largest blocker for advancing past Milestone 4.
**Resolution:** Add `HirStmt::If` / `HirExpr::If` (block-expression form), parser productions, and Cranelift multi-block emission.

### I-004 — String type is a raw pointer with no length
**Files:** `src/codegen.rs` (`HirExprKind::StrLiteral`, `generate_print_call`)
**Summary:** `StrLiteral` codegen returns a bare `global_value` pointer. There is no length, no slice ABI, no ownership metadata. `print()` works only on string literals because it grabs the length from the AST node, not from the runtime value. Any non-literal string operation will require a fat-pointer (`*u8, usize`) ABI decision.
**Resolution:** Design and implement a string slice ABI before adding string operations. Reference: Zig's `[]const u8`.

---

## 🟡 Correctness / Hygiene

### I-005 — `mut` is parsed but never enforced
**Files:** `src/hir.rs` (`HirStmt::VarDecl.mutable`), `src/sema.rs`
**Summary:** `HirStmt::VarDecl` carries `mutable: bool`, but no pass reads it. The README advertises immutable-by-default semantics. Reassignment isn't parsed yet, so the bug is latent — but the invariant should be checked in sema as soon as assignment lands.
**Resolution:** When assignment is added to the parser, sema must reject reassignment to non-`mut` bindings.

### I-006 — `print` is special-cased in codegen
**Files:** `src/codegen.rs` (`generate_print_call`), `src/sema.rs` (`check_builtin_call`)
**Summary:** Codegen emits a raw `write(2)` syscall wrapper inline for the `print` builtin, and sema has a builtin-specific validator hook to match. Consequences:
- Rejects `print(some_var)` even when the variable is a string.
- No formatting, no automatic newline.
- Already stubbed out on Windows (`return Err(...)`).
- Mixes runtime concerns into the compiler.
**Resolution:** Move `print` to a runtime crate (`ryort` or similar) compiled to an object file and linked in via `zig cc`. Codegen emits a normal call; `sema::check_builtin_call` goes away.

### I-008 — `Token<'a>` borrowed from source string complicates threading — RESOLVED
**Files:** `src/lexer.rs`
**Summary:** Tokens used to hold `&'a str` slices into the source, requiring `lexer::leak_token` to lift them to `'static` for tests.
**Resolution:** Phase 2 of `docs/dev/pipeline_alignment.md` interned identifiers and string literals through `InternPool::intern_str` and made `Token` a `Copy`, lifetime-free enum carrying `StringId` / `i64` payloads. The borrowed `RawToken<'a>` form is now private to `src/lexer.rs` and `leak_token` is gone.

### I-009 — `FunctionContext` rebuilt per HIR statement
**Files:** `src/codegen.rs` (`compile_function`)
**Summary:** A fresh `FunctionContext` struct is constructed for every `HirStmt` inside the loop. Harmless functionally but noisy and obscures intent.
**Resolution:** Lift the `FunctionContext` above the loop, or restructure so `eval_expr` takes `&mut self` directly. Mostly a borrow-checker exercise.

### I-010 — Unused `_bytes_written` from `write(2)` call
**Files:** `src/codegen.rs` (`generate_print_call`)
**Summary:** The result of the `write` syscall is fetched and ignored. A short write or `EINTR` will silently truncate output. Acceptable for a bootstrap, but should be documented or fixed when `print` moves to the runtime (I-006).
**Resolution:** Tracked under I-006.

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

---

## Cross-References

- Roadmap: [docs/dev/implementation_roadmap.md](docs/dev/implementation_roadmap.md)
- Spec: [docs/specification.md](docs/specification.md)
