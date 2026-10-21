# Critical

Be brief.

# Ryo Programming Language - Repository Conventions

**Ryo** is a pre-alpha statically-typed, compiled (AOT/JIT) programming language implemented in Rust. See README.md for language philosophy and design goals.

## Tech Stack & Layout

**Stack:** Rust compiler with Cranelift backend, Zig linker, Logos lexer, Chumsky parser.
**Layout:** Cargo workspace whose members (per the root `Cargo.toml`) are:
- `ryo/` (CLI binary crate)
- `ryo-core/` (shared models: AST, IRs (UIR, TIR), types, diagnostics, and errors)
- `ryo-frontend/` (lexer, indent preprocessor, parser, AST lowering, semantic analysis, builtins, ownership analysis)
- `ryo-backend/` (Cranelift code generation, Zig linking, toolchain management, runtime extraction)
- `ryo-driver/` (pipeline compilation orchestration driver)
- `runtime/` (Ryo static runtime library)
- `build-support/` (shared build-script helpers, e.g. the ryo-runtime archive build used by `ryo/build.rs` and `ryo-backend/build.rs`)

Non-member repository areas (not part of the Cargo workspace):
- `docs/` (spec, roadmap, examples)
- `.github/` (CI)

---

## File Naming Conventions

- **Ryo files:** lowercase with underscores (`error_handling.ryo`, `hello_world.ryo`)
- **Docs:** lowercase with underscores (`getting_started.md`). Special files uppercase (`README.md`, `NOTES.md`, `TODO.md`)
- **Rust files:** lowercase with underscores (`main.rs`, `ast.rs`) following Rust conventions

## Modules Follow Responsibilities

Group new code by its lasting responsibility, not by the task, review,
migration phase, or issue that caused it.

- Name modules after the behavior they own: `views.rs`, `promotions.rs`,
  `replacements.rs`, or `cleanup.rs`.
- Avoid historical or catch-all names such as `gaps.rs`, `fixes.rs`,
  `phase2.rs`, `misc.rs`, or `helpers.rs`.
- Put related modules under their owning component. For example,
  cleanup validation belongs under `ownership/validate/`.
- Keep shared machinery in the parent module or a module with a specific
  responsibility. Use the narrowest visibility necessary.
- Extend an existing module when it already owns the responsibility.
  Split modules when responsibilities differ or size requires it.
- Describe behavior in comments and test names. Do not use review-round
  or scratch-plan labels as permanent terminology.

Before adding a module, identify its responsibility, owner, and reason
it does not belong in an existing module.

---

## Critical Syntax Rules

**⚠️ CRITICAL: Python-Style Syntax is MANDATORY**

All Ryo code examples **must** use Python-style colons and indentation, **NOT** curly braces. Braces appear ONLY in f-strings and struct literals (`Point{x=5, y=9}`) — never for blocks.

**Tab Indentation:** Use TABS (not spaces). Mixing tabs/spaces is a compile-time error. One tab = one indentation level.

---

## Documentation Standards

**Code examples:** Use fenced code blocks with language tag (`ryo`).
**Cross-references:** Use relative paths (`[spec](docs/specification.md)`).
**Milestone completion:** When a milestone ships, update `landing/reference/index.html` if the language surface changed (types, literals, builtins, diagnostics) — and remove any "planned" callout the milestone fulfills. Also remove the shipped feature from the "What's Next" list on that page (it tracks only unimplemented items, in milestone order) and verify the ownership walkthrough (`landing/ownership/index.html`) still matches compiler behavior.
**Committed artifacts are self-contained:** anything under version control (`ISSUES.md`, `benchmarks/README.md` files, code comments, commit messages) must be understandable on its own. Never reference vocabulary or context that lives only in uncommitted scratch — e.g. `docs/superpowers/` specs and plans are gitignored, so committed files must not cite their "Phase 0/1/2" naming or section numbers. Cite the concept inline instead ("the value-range guard-elision work"), optionally with a commit hash or issue ID as a historical pointer.

---

## Build & Test Commands

Standard cargo commands work fully out-of-the-box (even on a clean checkout) because `build.rs` automatically compiles the `ryo-runtime` static library in a separate target directory if it isn't found.

```bash
cargo build                      # Automatically builds the runtime (if missing) and then compiles the compiler
cargo check                      # Check compiler for errors
cargo test                       # Run all unit + integration tests
./scripts/run_linux_tests.sh             # Build Docker image and run entire test suite in Linux (ASan + Valgrind leak detection)
./scripts/check_cranelift.sh [version]   # Diff Ryo's Cranelift version (from Cargo.lock) against another (default: latest) — see below
./scripts/check_file_length.sh           # Fail on Rust files over 2000 lines (no allowlist; CI runs the same check)
cargo run -- run <file>          # JIT compile and execute
cargo run -- build <file>        # AOT compile to binary
cargo run -- toolchain install   # Download Zig linker
cargo run -- toolchain status    # Check Zig status
RUSTFLAGS=-Dwarnings cargo clippy --workspace --all-targets  # Lint; RUSTFLAGS=-Dwarnings matches CI exactly (ci.yml sets it env-wide). `--workspace` is required — bare `--all-targets` only checks the default member `ryo` and misses other crates' test/bench targets
cargo fmt --check                # Check code formatting style
```

**Tracking Cranelift changes.** Ryo is built on the Cranelift backend, so upstream changes can affect codegen. `./scripts/check_cranelift.sh` resolves Ryo's Cranelift version from `Cargo.lock`, queries crates.io and the GitHub API for the exact commit SHAs, and prints the history of commits touching Cranelift's `cranelift/` directory between that version and a target version (default: latest release), handling parallel release-branch history. Pass a version argument to diff against a specific release instead of latest. Use it before bumping the Cranelift dependency to review what changed.

**Windows testing via UTM (Apple Silicon).** A UTM VM named `Windows` (Windows 11 ARM64, shared networking) is available on the dev machine for interactive Windows testing. Control it from the CLI: `utmctl list`, `utmctl start Windows`, `utmctl stop Windows` (binary at `/Applications/UTM.app/Contents/MacOS/utmctl`); the guest runs an OpenSSH server reachable at `ryo@192.168.64.x` (grab the IP from `arp -a | grep 192.168.64`; local throwaway password — ask the VM's owner). Three caveats learned the hard way: (1) a warm restart can hang at the UEFI "Start boot option" screen — a cold `utmctl stop`/`start` fixes it ([UTM #7648](https://github.com/utmapp/UTM/issues/7648)); (2) the display can go stale while the guest keeps running — verify via network/disk activity before assuming a hang ([UTM #6332](https://github.com/utmapp/UTM/issues/6332)); (3) in-guest `x86_64-pc-windows-gnu` builds break under x64 emulation (MSYS2 `as`/`dlltool` silently no-op), and corosensei has no aarch64-windows backend — so cross-build Windows binaries on macOS instead: `rustup target add x86_64-pc-windows-gnu`, `brew install mingw-w64`, and set `[target.x86_64-pc-windows-gnu] linker = "x86_64-w64-mingw32-gcc"` (see `docs/dev/concurrency_poc.md` for a worked example). For non-interactive x64 coverage, prefer the GitHub Actions Windows job.

**File extensions:** `.ryo` (source), `.md` (docs), `.rs` (Rust), `.o`/`.obj` (generated)

---

## CI

GitHub Actions runs on pushes to `main` and PRs targeting `main` (see `.github/workflows/ci.yml` for the authoritative job list): the file-length check, `cargo fmt --check`, `cargo clippy --workspace --all-targets`, and `cargo test --workspace` across Linux and macOS (plus a Windows test job, ASan/Valgrind leak checks, and a Miri job over the `ryo-runtime` crate's tests). `RUSTFLAGS=-Dwarnings` is set env-wide, so warnings are errors in every job. All jobs must pass for merge.

**Performance is a CI gate.** CodSpeed comments on PRs report both walltime and simulation (compile-time) regressions. A double-digit regression on a tracked benchmark is a defect: fix it in the PR or record a deliberate decision, never merge silently. Before merging codegen changes, run the affected benchmarks via `uv run benchmarks/run.py run --benchmarks <name>` and compare against the README's latest checkpoint. Local walltime A/B can miss compile-time regressions that the simulation metric isolates — trust the CodSpeed gate for compile-time claims.

---

## Development Workflow

**Branch naming:** `feat/`, `docs/`, `fix/`, `chore/`, `design/` prefixes.

**Commit prefixes:** `feat:`, `fix:`, `docs:`, `spec:`, `dev:`, `roadmap:`, `test:`, `chore:`, `refactor:`.
Keep subjects under 72 chars. Add body for non-obvious changes.

**Build-artifact hygiene:** on the maintainer's macOS dev machine, the `target/` snowball is pruned automatically by a weekly launchd agent (`~/Library/LaunchAgents/com.ryo.cargo-sweep.plist`) running `cargo sweep -t 15 -r <repo>` every Sunday 03:17, deleting only artifacts unused for 15+ days; logs at `~/Library/Logs/cargo-sweep.log`. The schedule is machine-local — other contributors are not covered; run `cargo sweep -t 15` manually or set up an equivalent job. Do not run a bare `cargo clean` for routine cleanup — it deletes everything, warm artifacts included; sweep by age instead. Manage the agent with `launchctl bootout/bootstrap gui/$(id -u) <plist>`. Note that every `git worktree` grows its own `target/` — remove throwaway worktrees promptly, or point `CARGO_TARGET_DIR` elsewhere when creating them.

IMPORTANT: Never author Claude on commits nor PRs.

---

## Issue Tracking

Non-immediate issues that affect architecture, correctness, or long-term code health go in `ISSUES.md`. Create an entry when you identify a problem that won't be resolved in the current session but must be addressed for better architecture or sustainability. Use the next sequential `I-XXX` number, set its `**Severity:**` field (Blocking / Correctness / Hygiene / Cleanup), set its `**Area:**` field to one component (frontend-lexer, frontend-parser, sema, ownership, codegen, runtime, linker-toolchain, driver-cli, core-ir, docs-spec, ci-benchmarks, tooling — cross-cutting issues take the component where the fix primarily lands), and include Files, Summary, and Resolution fields.

**Never reuse an `I-XXX` number, even after its entry is deleted.** IDs are cited in commit messages and live on in git history; a reused number silently retargets those references to a different issue. Deleted numbers stay retired — the next entry always takes the highest number ever used + 1.

Do **not** create issues for things you're fixing right now — just fix them. Do **not** use GitHub Issues for these; `ISSUES.md` is the single source of truth.

Cite issue IDs (`I-XXX`) in code comments and docs **only while the issue is still open** in `ISSUES.md`. Resolved entries are deleted from `ISSUES.md`, so a reference to one becomes a dangling pointer — when an issue is resolved, replace the reference with a self-contained inline explanation of the concept. Commit messages always carry the ID; they survive in git history.

**Reading and editing issues:** use `scripts/issue.py` (zero-dependency, runs via `uv run`) instead of grepping `ISSUES.md` by hand:

```bash
uv run scripts/issue.py I-032     # full entry text, prefixed with its line range
uv run scripts/issue.py next      # next issue id to use (highest ever, from the file + git history, + 1)
uv run scripts/issue.py list      # all ids, line ranges, and titles
uv run scripts/issue.py file --title "..." --severity cleanup --area codegen --files "..." --summary "..." --resolution "..."
                                  # append a new entry (fields prompted if omitted; --severity/--area accept short aliases)
uv run scripts/issue.py delete I-032 --yes
                                  # remove an entry (asks to confirm without --yes; the id stays retired)
```

---

## Design Change Escalation

Ryo is pre-alpha. Design changes to the language specification require explicit human approval. Coherence fixes (resolving contradictions, filling documented gaps, tightening phrasing) can proceed as normal work, but anything that adds, removes, or alters a language feature stops for review.

Examples:
- **OK without approval:** Fixing contradictions between spec sections, clarifying ambiguous phrasing, adding missing details for documented features
- **Requires approval:** Adding new syntax, removing features, changing semantics, altering ownership rules, modifying error handling behavior

When in doubt, ask before making language design changes.

---

## Documentation Conventions

For docs-specific conventions when editing files in `docs/`, see `docs/AGENTS.md`.

---

## Compiler Architecture & Developer Guide

Working on compiler internals (pipeline, IRs, parser, sema, codegen, crate map, adding a language feature)? Read `docs/dev/compiler_guide.md` first — the full architecture guide and feature checklist live there.

---

## graphify

This project has a graphify knowledge graph at graphify-out/.

Rules:
- Before answering architecture or codebase questions, read graphify-out/GRAPH_REPORT.md for god nodes and community structure
- If graphify-out/wiki/index.md exists, navigate it instead of reading raw files
- For cross-module "how does X relate to Y" questions, prefer `graphify query "<question>"`, `graphify path "<A>" "<B>"`, or `graphify explain "<concept>"` over grep — these traverse the graph's EXTRACTED + INFERRED edges instead of scanning files
- After modifying code files in this session, run `graphify update .` to keep the graph current (AST-only, no API cost)
