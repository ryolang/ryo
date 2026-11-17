**Status:** Implementation (current source-identity PR)

# Compilation Context and Source Identity

This temporary proposal defines the first source-identity PR in the compiler
architecture migration. Delete it after the implementation lands and the
architecture guide describes the shipped contracts.

## Intent and why

Diagnostics currently receive one source string and filename, and every label
uses that filename regardless of where its span originated. Spans stored in the
IR contain only byte offsets. Introduce compilation-owned immutable source
revisions and source-identified spans before declaration resolution and Flow
begin to rely on provenance.

This PR preserves language syntax, semantics, accepted programs, and execution
behavior.

## Goals

- Introduce a `CompilationContext` that owns source storage. Later PRs will add
  declaration tables and interned semantic types; they are not added here.
- Add a compact `SourceId` that identifies one immutable source revision within
  one compilation context.
- Carry source identity through AST, UIR, TIR, Flow, and structured diagnostics.
- Resolve primary and secondary diagnostic labels through the source database,
  including labels in different files.
- Preserve old revisions for old artifacts while rejecting stale revisions when
  a caller requires the current source.
- Reject unknown, wrong-context, and malformed source references without
  panicking or silently selecting another file.

## Non-goals and anti-goals

- No `DeclId`, `BindingId`, module resolution, generic instantiation, or
  `InternPool` ownership migration.
- No multi-file parser or new module syntax.
- No target layout, CLIF source locations, or debug-information format.
- No persistent source format or globally stable IDs across compiler processes.
- No parallel filename-based source authority and no default-to-file-zero
  fallback for an invalid or source-less span.
- No language-visible ownership, diagnostic-code, or runtime changes.

## Approaches

### Chosen: source-identity vertical slice

Add the context, source database, source-identified spans, IR propagation, and
source-aware diagnostic rendering together. This proves the central contract
end to end while leaving declaration and binding identities to later PRs.

### Rejected: core model only

Add only the context, source database, and span types. This is smaller but
leaves the renderer applying one filename to every label and provides no
cross-file diagnostic proof.

### Rejected: all identity work in one PR

Combine source, declaration, binding, nominal-type, and backend-storage
identities. This mixes several resolution and execution cutovers and is too
large to review or isolate safely.

## Representation

### Compilation and source identities

`CompilationContext` owns a process-unique context tag and a `SourceDb`.
`SourceId` contains that tag and a context-local revision index. The context tag
is for validation, not display or persistence. Two contexts can therefore use
the same local index without accepting each other's handles.

A source record owns its display path as a `PathBuf` and its UTF-8 text as a
`String`; expose immutable accessors only. Display paths are labels, not source
identity.

`SourceDb` provides owned lookup and span validation. Validation checks the
context tag, revision existence, offset ordering, source bounds, and UTF-8
character boundaries. A source-aware renderer may retain the current
byte-to-character conversion behavior, but must key its render cache by
`SourceId` rather than filename.

### Revision lifecycle

Each source lineage has one current revision and immutable prior revisions:

- Registering a new source creates a lineage and its first current revision.
- Replacing the current text creates a new revision in that lineage and makes
  it current. The old revision remains available to old artifacts.
- Replacing a non-current or foreign revision fails and changes nothing.
- Ordinary lookup accepts any retained revision owned by the context.
- `require_current` rejects a retained but stale revision.
- Dropping the compilation context releases all revisions; no IR or diagnostic
  consumer may retain the source database beyond that point.

No deletion or in-place text mutation occurs in this PR.

### Spans and synthetic operations

`Span` carries an explicit `SourceId` or an explicit source-less sentinel.
`Span::EMPTY` and source-less constructors never denote the first registered
file. Real lexer/parser spans use a source-aware constructor. Derived spans use
operations such as `with_start` and `with_end` so they retain provenance.

Source-less spans are limited to diagnostics or synthetic operations with no
originating source operation. Existing synthetic operations with a source
origin must preserve it. Source-less diagnostics render without attributing
their location to an arbitrary file.

The UIR and TIR match side arenas currently serialize only span offsets. Extend
those wires with the source handle's raw parts and update their literal wire
fixtures and decoders. These are in-memory context-local representations, not a
persistent format.

## Pipeline changes

- Driver entry points create a `CompilationContext`, register the input file,
  and pass its `SourceId` through lexing and parsing. The context outlives every
  AST/IR/diagnostic artifact that refers to its sources.
- `ParseState` carries the source identity so parser-built and recovered nodes
  use the same source as lexer diagnostics.
- AST/UIR/TIR construction and derived-span helpers preserve the identity;
  existing TIR-to-Flow lowering then propagates it without executable changes.
- Diagnostic finalization takes the source database instead of one input/name
  pair. Ariadne report locations and caches use `SourceId`; each primary or
  secondary label resolves its own text and display path.
- Invalid source references become an internal compiler error. They must not
  render against a different source or disappear into a warning-only success.
- Single-file CLI output, diagnostic ordering, broken-pipe handling, JIT, and
  AOT behavior remain unchanged for valid inputs.

## Verification

Add focused tests before migrating each boundary:

- Source registration, immutable replacement, current/stale lookup, failed
  replacement rollback, unknown IDs, and same-index wrong-context rejection.
- Span bounds and Unicode character-boundary validation; source-less span
  behavior.
- Lexer/parser/AST provenance, derived spans, UIR/TIR match-wire round trips,
  and Flow preservation.
- One diagnostic whose primary and secondary labels are in different files,
  including Unicode before a label; assert both filenames and source excerpts.
- Source-less diagnostics and invalid-source rendering failures.
- Existing single-file diagnostic fixtures, parser recovery, and full workspace
  tests.
- End-to-end JIT and AOT smoke tests for a valid program and a diagnosed
  invalid program.

Run formatting, workspace Clippy with warnings denied, the file-length gate,
and CodeRabbit review. Compare tiny and large parser workloads if the span
representation changes measured parser performance; investigate any tracked
regression before completion.

## Builder discretion

Choose concrete Rust names and private helper structure consistent with the
existing modules. Keep source ownership in `ryo-core`, parsing in
`ryo-frontend`, and rendering in `ryo-driver`. Do not add dependencies or widen
public APIs beyond what the source contracts and tests require.

## References

- Spec: [Section 3](../../specification.md#3-syntax--grammar) (source syntax) and
  [Section 4.5](../../specification.md) (compilation and diagnostics).
- Dev: [Compiler pipeline and Flow IR migration](pipeline_migration.md) and
  [compiler architecture guide](../guide_compiler.md).
- Architecture proposals: I-284 and I-288 in `ISSUES.md`; cross-boundary tests
  from I-292.
- Milestones and delivery: [Implementation roadmap](../implementation_roadmap.md).
