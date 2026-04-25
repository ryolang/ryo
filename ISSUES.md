# Known Issues

Compiler issues identified during source review. Each entry is independently actionable; severity reflects impact on correctness, future feature work, or code health — not user impact today (the compiler is pre-alpha).

For the two largest items (interned types, lower/sema split), see the dedicated plan in [docs/dev/middle_end_refactor.md](docs/dev/middle_end_refactor.md). They are summarized here for completeness but tracked there.

---

## Severity Legend

- 🔴 **Blocking** — prevents implementing roadmap features as currently designed.
- 🟡 **Correctness/Hygiene** — silent bug or invariant gap; works today, will bite later.
- 🟢 **Cleanup** — code health, ergonomics, minor.

---

## ✅ Resolved

### I-001 — `hir::Type` is a 3-variant enum with no interning ✅
**Files:** `src/hir.rs`, `src/types.rs`, `src/codegen.rs`, `src/builtins.rs`
**Resolved by:** Phase 1 of [middle_end_refactor.md](docs/dev/middle_end_refactor.md). `hir::Type` removed; `TypeId` and `InternPool` live in `src/types.rs`; every consumer routes through the pool. `TypeKind` reserves slots for future structs/enums/tuples/option/error-union/function types so they land additively.

### I-002 — `lower.rs` fuses structural lowering and semantic analysis ✅
**Files:** `src/ast_lower.rs`, `src/sema.rs` (formerly `src/lower.rs`)
**Resolved by:** Phase 2 of [middle_end_refactor.md](docs/dev/middle_end_refactor.md). `lower.rs` deleted; `ast_lower` handles pure AST→HIR structural translation (leaves `HirExpr.ty = None`), `sema` owns scope, type resolution, function signatures, and type-mismatch diagnostics.

### I-007 — Binary operators always produce `Int` ✅
**Files:** `src/sema.rs`
**Resolved by:** The sema extraction. `analyze_expr` for `BinaryOp` rejects mismatched operand types, restricts arithmetic operators to `Int`, allows equality on `Int`/`Bool` only (producing `Bool`), and explicitly rejects `Str`/`Void` equality. Covered by `sema::tests::mixed_type_equality_rejected`, `bool_arithmetic_rejected`, `string_equality_rejected`.

### I-014 — `print` rejects non-literal arguments with a confusing error ✅
**Files:** `src/sema.rs`, `src/codegen.rs`
**Resolved by:** Lifting the arity and string-literal checks out of `codegen::generate_print_call` and into `sema::check_builtin_call`. The codegen path now `debug_assert!`s the invariants sema guarantees. Final fix remains tracked under I-006 (move `print` to a runtime crate).

---

## 🔴 Blocking

### I-003 — No control flow in the compiler
**Files:** `src/lexer.rs`, `src/parser.rs`, `src/hir.rs`, `src/codegen.rs`
**Summary:** `if`/`else`/`match` are lexed as keywords but have no parser, no HIR variants, and no codegen support (no block branching or phi handling). This is the single largest blocker for advancing past Milestone 4.
**Resolution:** Add `HirStmt::If` / `HirExpr::If` (block-expression form), parser productions, and Cranelift multi-block emission. Should land **after** the middle-end refactor so the new HIR shape doesn't have to be rewritten.

### I-004 — String type is a raw pointer with no length
**Files:** `src/codegen.rs` (`HirExprKind::StrLiteral`, `generate_print_call`)
**Summary:** `StrLiteral` codegen returns a bare `global_value` pointer. There is no length, no slice ABI, no ownership metadata. `print()` works only on string literals because it grabs the length from the AST node, not from the runtime value. Any non-literal string operation will require a fat-pointer (`*u8, usize`) ABI decision.
**Resolution:** Design and implement a string slice ABI before adding string operations. Reference: Zig's `[]const u8`.

---

## 🟡 Correctness / Hygiene

### I-005 — `mut` is parsed but never enforced
**Files:** `src/hir.rs` (`HirStmt::VarDecl.mutable`), `src/lower.rs`
**Summary:** `HirStmt::VarDecl` carries `mutable: bool`, but no pass reads it. The README advertises immutable-by-default semantics. Reassignment isn't parsed yet, so the bug is latent — but the invariant should be checked in sema as soon as assignment lands.
**Resolution:** When assignment is added to the parser, sema must reject reassignment to non-`mut` bindings.

### I-006 — `print` is special-cased in codegen
**Files:** `src/codegen.rs` (`generate_print_call`)
**Summary:** Codegen emits a raw `write(2)` syscall wrapper inline for the `print` builtin. Consequences:
- Rejects `print(some_var)` even when the variable is a string.
- No formatting, no automatic newline.
- Already stubbed out on Windows (`return Err(...)`).
- Mixes runtime concerns into the compiler.
**Resolution:** Move `print` to a runtime crate (`ryort` or similar) compiled to an object file and linked in via `zig cc`. Codegen emits a normal call.

### I-008 — `Token<'a>` borrowed from source string complicates threading
**Files:** `src/lexer.rs` (`leak_token`)
**Summary:** Tokens hold `&'a str` slices into the source. To support tests that need `Token<'static>`, `lexer::leak_token` uses `Box::leak`. This is `#[cfg(test)]`-only, but it is a smell: any future pass that needs to retain tokens beyond parse time will fight the lifetime.
**Resolution:** Consider interning identifiers and string literals (e.g., `lasso` or a hand-rolled `SymbolId`) so tokens become `Copy` and `'static`-friendly.

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

- Implementation plan for I-001 and I-002: [docs/dev/middle_end_refactor.md](docs/dev/middle_end_refactor.md)
- Roadmap: [docs/dev/implementation_roadmap.md](docs/dev/implementation_roadmap.md)
- Spec: [docs/specification.md](docs/specification.md)
