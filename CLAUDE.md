# Ryo Programming Language - Repository Conventions

**Ryo** is a pre-alpha statically-typed, compiled (AOT/JIT) programming language implemented in Rust. See README.md for language philosophy and design goals.

## Tech Stack & Layout

**Stack:** Rust compiler with Cranelift backend, Zig linker, Logos lexer, Chumsky parser.
**Layout:** `src/` (compiler), `docs/` (spec, roadmap, examples), `experimental/` (design work), `.github/` (CI).

---

## File Naming Conventions

- **Ryo files:** lowercase with underscores (`error_handling.ryo`, `hello_world.ryo`)
- **Docs:** lowercase with underscores (`getting_started.md`). Special files uppercase (`README.md`, `CLAUDE.md`, `TODO.md`)
- **Rust files:** lowercase with underscores (`main.rs`, `ast.rs`) following Rust conventions

---

## Critical Syntax Rules

**⚠️ CRITICAL: Python-Style Syntax is MANDATORY**

All Ryo code examples **must** use Python-style colons and indentation, **NOT** curly braces. Braces are ONLY for f-strings.

**Tab Indentation:** Use TABS (not spaces). Mixing tabs/spaces is a compile-time error. One tab = one indentation level.

---

## Documentation Standards

**Code examples:** Use fenced code blocks with language tag (````ryo`).
**Cross-references:** Use relative paths (`[spec](docs/specification.md)`).

---

## Build & Test Commands

```bash
cargo fmt                        # Auto-format (CI runs --check with -Dwarnings)
cargo clippy --all-targets       # Lint (CI enforces, warnings are errors)
cargo check                      # Check for errors
cargo build [--release]          # Build debug or release
cargo run -- run <file>          # JIT compile and execute
cargo run -- build <file>        # AOT compile to binary
cargo test                       # Run tests
cargo run -- toolchain install   # Download Zig linker
cargo run -- toolchain status    # Check Zig status
```

**File extensions:** `.ryo` (source), `.md` (docs), `.rs` (Rust), `.o`/`.obj` (generated)

---

## CI

GitHub Actions runs on pushes to `main` and PRs targeting `main`: `cargo fmt --check` with `-Dwarnings`, `cargo clippy --all-targets` (warnings are errors), and `cargo test` (Ubuntu x64, Ubuntu ARM64, macOS). All three must pass for merge.

---

## Development Workflow

**Branch naming:** `feat/`, `docs/`, `fix/`, `chore/`, `design/` prefixes.

**Commit prefixes:** `feat:`, `fix:`, `docs:`, `spec:`, `dev:`, `roadmap:`, `test:`, `chore:`, `refactor:`.
Keep subjects under 72 chars. Add body for non-obvious changes.

---

## Design Change Escalation

Ryo is pre-alpha. Design changes to the language specification require explicit human approval. Coherence fixes (resolving contradictions, filling documented gaps, tightening phrasing) can proceed as normal work, but anything that adds, removes, or alters a language feature stops for review.

Examples:
- **OK without approval:** Fixing contradictions between spec sections, clarifying ambiguous phrasing, adding missing details for documented features
- **Requires approval:** Adding new syntax, removing features, changing semantics, altering ownership rules, modifying error handling behavior

When in doubt, ask before making language design changes.

---

## Documentation Conventions

For docs-specific conventions when editing files in `docs/`, see `docs/CLAUDE.md`.

---

## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, read graphify-out/GRAPH_REPORT.md for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code files in this session, run `graphify update .` to keep the graph current (AST-only, no API cost)
