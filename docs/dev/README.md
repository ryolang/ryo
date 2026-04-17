# Development Notes

This directory contains implementation notes, architectural decisions, and design explorations for the Ryo compiler and runtime. These are **working documents for contributors** — not user-facing documentation.

**Lifecycle:** Every file here should eventually be either absorbed into the [specification](../specification.md), implemented in code, or deleted. This directory should be empty by v1.0.

## File Index

### Compiler Implementation Guides

| File | Content | Next Action |
|---|---|---|
| [compilation_pipeline.md](compilation_pipeline.md) | 6-phase pipeline (lex → parse → lower/HIR → codegen → link → execute) with Rust code examples | **Delete** when compiler source code is self-documenting with inline comments |
| [token.rs.md](token.rs.md) | Lexer implementation guide using `logos` crate | **Delete** when lexer is stable — reference lives in source code |
| [parser.rs.md](parser.rs.md) | Parser implementation guide using `chumsky` crate | **Delete** when parser is stable — reference lives in source code |
| [project_structure.md](project_structure.md) | Rust workspace crate organization (ryo-core, ryo-parser, ryo-codegen, etc.) | **Delete** when workspace structure is stable — a top-level `ARCHITECTURE.md` or cargo workspace layout is sufficient |

### Architecture & Design Decisions

| File | Content | Next Action |
|---|---|---|
| [built_in.md](built_in.md) | Compiler magic types vs stdlib boundary (what the compiler must know vs what's a library) | **Move to spec** §4 (Types) or §13 (Stdlib) — then delete |
| [std.md](std.md) | Rust runtime + Ryo wrapper strategy for stdlib (`std.sys` hidden layer + `std.io` public API) | **Move to spec** §13 (Standard Library) — then delete |
| [std_ext.md](std_ext.md) | Curated Rust crates to wrap for stdlib (serde_json, ureq, regex, chrono, rand) | **Move to spec** §13 or a dedicated stdlib contributor guide — then delete |
| [unsafe.md](unsafe.md) | `kind = "system"` gatekeeper pattern for unsafe blocks in `ryo.toml` | **Move to spec** §17 (FFI & unsafe) — then delete |
| [dyn_trait.md](dyn_trait.md) | Enum dispatch workaround for v0.1 (no `dyn Trait` yet), vtable explanation | **Delete** when `dyn Trait` is implemented in v0.3+ |

### Ecosystem & Tooling

| File | Content | Next Action |
|---|---|---|
| [installation.md](installation.md) | Installation UX: one-line script, `~/.ryo/`, zig auto-download, `ryo upgrade` | **Implement** the installer — then move user-facing parts to `docs/installation.md` and delete |
| [official_pkg.md](official_pkg.md) | Recommended official packages for v0.2 (cli, postgres, dotenv, image) | **Implement** the packages — then delete (packages document themselves) |
| [testing.md](testing.md) | Testing framework gaps: fixtures, mocking, RAII patterns, benchmarks | **Move to spec** testing section or implement — then delete |

### Production & Future Concerns

| File | Content | Next Action |
|---|---|---|
| [considerations.md](considerations.md) | Supply chain security, observability hooks, DWARF debug info, Windows UTF-8 paths | **Move relevant items to spec** as they become actionable — then delete |
| [tensor.md](tensor.md) | DLPack/GPU interop, opaque wrapper pattern for safe FFI to TensorFlow/PyTorch | **Move to spec** when data science features are designed — then delete |

### Roadmap & Proposals

| File | Content | Next Action |
|---|---|---|
| [implementation_roadmap.md](implementation_roadmap.md) | 27 milestones across 6 phases with detailed tasks, test counts, completion dates | **Keep updating** as milestones complete — delete when v1.0 ships |
| [proposals.md](proposals.md) | Future feature designs: generics, iterators, comptime, SIMD, dynamic dispatch, Jupyter kernel | **Move accepted proposals to spec** as they're designed — delete when all are resolved |
