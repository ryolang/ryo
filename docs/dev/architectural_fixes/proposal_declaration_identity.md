**Status:** Design (v0.2+)

# Function Declaration Identity and Direct Calls (Draft — v0.2+)

This temporary proposal defines the next small identity PR after the source-identity
slice. Delete it after implementation and final review, once permanent architecture
documentation describes the shipped contracts.

## Intent and why

Sema currently allocates a function-local `DeclId` as a `u32` index into
`uir.func_bodies`, but TIR serializes direct calls by `StringId`. Other compilers
keep semantic declaration identity separate from source spelling and executable
values: rustc uses `DefId`, Go resolves identifiers to `types.Object`, Zig uses
`Decl.Index`, and Swift distinguishes AST declarations from SIL values.

Introduce the same boundary for Ryo: a compilation-owned function `DeclId` that
Sema and TIR direct calls carry, while names remain lookup keys during resolution
and diagnostic/backend-compatibility labels afterward. This is a prerequisite for
local binding, nominal-type, generic-instance, and backend-symbol identities.

## Goals

- Give every function declaration a compact `DeclId` composed of the compilation
  context tag and a context-local declaration index.
- Own exactly one function declaration table in `CompilationContext`, constructed
  with the same context tag as its source database.
- Allocate IDs before Sema resolution and retain source provenance through each
  declaration's `Span`.
- Use `DeclId` in Sema's declaration map, worklist, error-inference dependencies,
  `CallPlan`, and direct TIR calls.
- Add declaration identity to each TIR body and direct-call payload while
  retaining the callee name only as a diagnostic/backend-compatibility label.
- Reject wrong-context handles, including equal local indices from different
  contexts, before call resolution or executable publication.
- Preserve duplicate-name behavior, recursion, inferred error unions, diagnostics,
  JIT/AOT execution, and accepted programs.

## Non-goals and anti-goals

- No `BindingId`, local storage changes, nominal-type identity, generic instances,
  or backend symbol changes.
- No UIR annotation-resolution move, new modules, syntax, or language semantics.
- No second declaration table owned by Sema and no allocation of replacement IDs
  during expression analysis or backend emission.
- No process-global or persistent declaration identities.
- No requirement that backend consume `DeclId` in this PR; the existing name-based
  backend path remains an explicit compatibility boundary, not a second Sema resolver.

## Approaches

### Chosen: function declarations and direct calls

Migrate function identity and direct-call payloads first. This follows rustc's
`DefId`, Zig's declaration table, and Go's resolved-object model while leaving
local bindings and executable values to later PRs.

### Rejected: local bindings first

Start `BindingId` and storage places before declaration identity. This touches
Sema scopes, TIR reads/writes, Flow places, and backend slot restoration, while
leaving direct calls name-based. It has a larger blast radius and does not
establish the declaration boundary needed by the other work.

### Rejected: declarations, bindings, and backend symbols together

Migrate all identity layers in one PR. This mixes semantic resolution, storage,
and physical emission and is too large to review or isolate safely.

## Representation and ownership

Add a declaration module in `ryo-core` containing:

- `DeclId { context: CompilationContextId, index: u32 }`, with a dense local
  index for arena indexing and deterministic local-only debug output.
- `DeclId::NONE`, reserved for synthetic test bodies, never for a registered
  declaration or executable production artifact.
- `FunctionDecl { id: DeclId, name: StringId, uir_index: usize, span: Span }`.
- `DeclarationError::{WrongContext, UnknownDeclaration, ConflictingRegistration}`.
- `DeclarationTable`, which allocates function IDs in UIR source order and provides
  `get(DeclId) -> Result<&FunctionDecl, DeclarationError>` after validating the
  context tag.

`CompilationContext` owns exactly one `DeclarationTable`. Sema registers all UIR
function bodies there, then uses the returned IDs instead of constructing its own
`DeclId`. Duplicate function spellings retain the current first-wins name lookup
and duplicate diagnostic behavior; duplicate declarations still receive unique
IDs and are analyzed.

TIR bodies record their `DeclId`. Production TIR builders use `with_decl` to set
it. Synthetic test builders may use `DeclId::NONE` explicitly; it is rejected by
`DeclarationTable::get` and by backend-preparation validation. Call payloads
record both the resolved `DeclId` and the existing callee `StringId`; the name is
only a diagnostic/backend-compatibility label. UIR/TIR wire indexes after the new
identity words are updated with literal fixtures. Existing backend name-based
symbol generation continues unchanged and will be replaced by a later
physical-lowering PR.

Sema's `signatures` map changes from name-keyed to `DeclId`-keyed entries. Name
lookup remains only in `name_to_decl`, which returns the allocated ID, and call
analysis uses the ID for all subsequent signature and dependency work.
`TirBuilder::call` and `CallView` carry the resolved ID. Method calls and
builtins remain unchanged in this PR.

## State machine review

ID allocation transitions a declaration from unregistered to registered before
Sema begins. Registered declarations and their spans are immutable for the
context's lifetime.

Registration is idempotent only when the same UIR body is registered with exactly
the same metadata. Repeating an identical registration returns the existing ID.
A conflicting registration for the same UIR body index is rejected without
changing the table. Registering a new source revision creates new declaration
IDs; it never reuses an ID from the old revision. Old IDs remain attached to old
source revisions for diagnostics and stale-artifact rejection.

Existing Sema states remain `Unresolved -> InProgress -> Resolved/Failed`.
Signature and body resolution are not merged into a new state model here.
Failed definitions and unresolved placeholders may remain available for
inspection, but current source-error gating still prevents backend preparation.
Wrong-context handles fail lookup rather than being re-registered or silently
selecting another declaration.

## Migration and compatibility

- Extend `sema::analyze*` entry points to accept the caller's real
  `CompilationContext`; no temporary context is created inside Sema.
- Register declarations once at the start of `Sema::run`, then replace the
  current private Sema `DeclId` with the core type.
- Update driver and test harnesses to retain and pass the same context through
  parsing, Sema, and backend preparation.
- Keep the old callee name in TIR for backend compatibility, but Sema and error
  inference no longer use it as semantic identity.
- Update TIR builders, call views, dumps, artifact stamps, and wire fixtures so
  identity changes cannot silently leave stale executable artifacts.

## Verification

- Allocation is source-ordered, unique per function body, and preserves spans.
- Identical registration is idempotent; conflicting registration rolls back.
- Same-index wrong-context lookup fails, including duplicate names in separate
  declaration environments.
- Source replacement allocates fresh IDs and does not make old executable
  artifacts current.
- Recursive signatures, legal recursion, inferred error dependencies, duplicate
  spellings, and failed dependencies preserve current behavior.
- Direct TIR calls round-trip both `DeclId` and the compatibility name, including
  equal names in separate contexts.
- `DeclId::NONE` and wrong-context bodies/calls are rejected before backend
  preparation.
- Changing a declaration identity changes relevant TIR/artifact stamps.
- Full workspace tests, formatting, Clippy with warnings denied, and file length.
- End-to-end JIT and AOT execution for direct, recursive, and inferred-error calls.
- CodeRabbit and independent whole-branch review with important findings fixed.

## Builder discretion

Choose exact private helper structure and test-module names consistent with the
repository. Keep declaration ownership in `ryo-core`, resolution in
`ryo-frontend`, and backend symbol generation unchanged. Do not add dependencies
or migrate binding/nominal identities in this PR.

## References

- Prior art: rustc `DefId`/`LocalDefId`, Go `go/types.Object`, Zig `Decl.Index`,
  and Swift AST `Decl` versus `SILValue`.
- Dev: [Compiler pipeline and Flow IR migration](pipeline_migration.md) and
  [compiler architecture guide](../guide_compiler.md).
- Architecture proposals: I-284 and cross-boundary tests from I-292 in `ISSUES.md`.
- Milestones and delivery: [Implementation roadmap](../implementation_roadmap.md).
