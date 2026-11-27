# I-284 Body-Local Binding Identity Migration

**Status:** Design approved for review; implementation not started

This is a temporary design artifact. Delete it when the migration is complete.

## Intent

Replace name-based local-variable identity with resolved body-local identities. Sema resolves each local binding once, emits that identity in typed reads and writes, and makes Flow and backend consume it without repeating lexical resolution.

This is I-284, required before self-contained executable Flow in I-286. It adds no module syntax, generic syntax, or language semantics.

## Current problem

- Sema scopes key locals by `StringId`.
- TIR parameters, reads, declarations, assignments, compound assignments, and loop bindings carry names.
- Flow resolves those names again and stores them in `Place::Param`, `Local`, `LoopVar`, and `Token`.
- Backend tables use dense `StringId` slots and scoped undo logs to restore representations.
- Same-spelled locals in one body and same-spelled declarations in different environments therefore depend on lookup conventions rather than resolved identity.

`DeclId` and `SourceId` already exist and must not change.

## Goals

- Allocate a dense `BindingId` for every parameter and local binding in a function body.
- Resolve each initializer before introducing its binding. Shadowing mints a fresh ID; assignment and `inout` writeback reuse the existing ID.
- Cover parameters, locals, loop variables, pattern bindings, and generated `try`/`catch` bindings.
- Carry IDs in every typed definition, read, and write.
- Keep names and spans only for lookup, diagnostics, and IR dumps.
- Preserve cleanup, control-flow state restoration, and execution behavior.
- Reject missing definitions, out-of-range IDs, cross-body IDs, and uses outside a binding's scope before executable publication.
- Avoid a compilation-global binding table and per-read metadata allocation.

## Non-goals and anti-goals

- No language feature, module, generic, or ownership-rule changes.
- No self-contained Flow redesign (I-286), ownership-pass split (I-287), or runtime ABI work (I-293).
- No global binding namespace and no renaming to make identities unique.
- No fallback to name-based binding resolution in Flow or backend.
- No deletion of control-flow state restoration that joins still require.

## Approaches considered

### Body-local IDs — selected

Sema allocates sequential IDs per function. The body's existing `DeclId` supplies context, so `(DeclId, BindingId)` is globally meaningful without storing the declaration in every operation.

Advantages: exact match for I-284, cheap dense tables, natural per-body lifecycle, and no global invalidation machinery.

### Compilation-global IDs

`CompilationContext` would allocate all binding IDs. This adds lifecycle, cross-context validation, and dependent-invalidation complexity without benefiting local bindings. Rejected.

### Flow-only IDs

TIR would keep names and Flow would allocate IDs. This does not satisfy typed TIR identity or remove repeated resolution. Rejected.

## Representation changes

### Core identity and metadata

Add `BindingId`, a small newtype around `u32` with raw/index accessors. It is valid only in its originating TIR body. `Tir` already carries its `DeclId`.

Validation is at the artifact boundary, not by provenance in the bare ID. Copying an ID from another body can coincide with a legitimate local ID, so it is not independently detectable. Rejection instead uses the enclosing artifact's `DeclId`, producer identity, and source/staleness checks, plus scope and metadata validation for every local ID. The design does not require global IDs or per-operation provenance.

Publish per-body binding metadata ordered by ID:

- name (`StringId`)
- resolved type (`TypeId`)
- parameter/borrow mode or local-binding kind
- declaration span
- originating `ScopeId`

Metadata is allocated per binding, not per read. Backend and diagnostic rendering use it instead of lexical lookup.

### Sema allocation and scopes

`FuncCtx` owns the next ID. Parameters are allocated first in declaration order. Local declarations, for-loop bindings, match and destructure payload bindings, and generated `try`/`catch` bindings receive subsequent IDs in emission order.

`Scope` has a body-local `ScopeId` and maps source names to binding records containing the ID and existing type/mutability/borrow/dead-store state. Lookup returns the resolved record. Shadowing mints a fresh ID even when the name is unchanged. Scope entry and exit never reassign IDs. Initializers are resolved before their new binding is introduced.

Name-based semantic operations continue to use this lookup. `resolve_var_read` and every write emit the returned ID instead of the name.

### TIR

Parameters carry both name and ID. Variable reads, declarations, assignments, compound assignments, and for-loop bindings carry the ID. Wire encoders, decoders, dumps, builders, validators, and tests use the same representation.

A TIR body publishes its binding metadata. Any read or write whose ID lacks metadata, is out of range, or belongs to another body fails validation before backend preparation.

### Flow

`PlaceId` remains the dense storage index, distinct from `BindingId` because Flow also represents temporary storage and views. Semantic binding places carry both IDs.

`Place::Param`, `Local`, `LoopVar`, and `Token` use the TIR-assigned `BindingId`. Flow construction uses a dense `BindingId -> PlaceId` map instead of lexical name lookup. View bases continue to reference their owner `PlaceId`.

Flow scopes retain `ScopeId` ancestry. Each use must resolve to a `PlaceId` registered for that ID in the current scope or an ancestor. This validates uses outside a binding's scope without resolving names again.

### Backend

Backend slot tables are indexed by `BindingId`, not `StringId`. Table sizes come from the current body's binding count. Reads and writes use the ID; cleanup and drops use resolved Flow places.

Undo entries that existed only to restore name-shadowed bindings are removed after join tests prove that control-flow state still restores correctly. Required state restoration remains.

Backend dump and diagnostic labels resolve names through binding metadata. Backend-ready validation rejects raw artifacts with unregistered, out-of-scope, or cross-body IDs before mechanical emission. Backend symbols derive from declaration identity in the separate nominal-symbol slice.

## State and publication model

- Allocation is monotonic within a body; IDs are never reused within that body.
- A binding begins as a source-name record. Resolution either resolves it to a typed ID or fails without publishing an executable body.
- Suspension of a lexical scope does not invalidate IDs or metadata.
- Frontend publication requires complete binding metadata and valid ID references in every TIR read/write.
- Flow publication requires semantic binding places to reference the same IDs and valid `ScopeId` ancestry.
- Backend publication requires IDs from the current body only. Cross-context handles, stale artifacts, unregistered IDs, and out-of-scope uses fail closed.
- Failed resolution retains diagnostic-only recovery artifacts and cannot produce an executable.

## Migration sequence

### PR 1 — Binding identity

1. Add `BindingId` and per-body metadata, with lifecycle and cross-context tests.
2. Migrate Sema scopes, initializers, generated bindings, TIR parameters/reads/writes, and wire fixtures.
3. Migrate Flow places, dense ID lookup, and scope-validity checks.
4. Migrate backend storage and cleanup to dense `BindingId` slots and validate artifacts end to end.

Intermediate name/ID representations are not independently publishable, so these changes land as one coherent PR.

### PR 2 — Nominal and backend-symbol identity

1. Key nominal types by declaration identity and future generic instances by declaration plus type arguments.
2. Derive backend symbols from declaration identity rather than source spelling.
3. Add duplicate-spelling and independent-environment coverage.

Nominal and backend-symbol work stays a separate I-284 slice rather than expanding the binding-identity diff.

### Verification order

Start with a shadowed owned value whose inner and outer bindings both require cleanup. Trace it through TIR, Flow, and codegen. Then cover pattern bindings, loop exits, `try` propagation, and `inout` writeback. Compare diagnostics and execution with optimization on and off. Finally run publication, review, and benchmark gates and delete this document.

## Verification

- Same-spelled parameters, locals, loop bindings, match payloads, generated `try`/`catch` bindings, and shadowed bindings receive distinct IDs.
- Initializers cannot resolve a binding before it is introduced; assignment and `inout` writeback retain the original ID.
- Same local ID in different bodies is interpreted only with its own `DeclId`; cross-body and cross-context use is rejected.
- Missing definitions, out-of-range IDs, stale bodies, and out-of-scope uses fail publication.
- Recursive signatures and reentrant resolution preserve existing behavior; failed dependencies cannot publish executable artifacts.
- Join restoration preserves control-flow state without name-shadowing undo entries.
- Dumps retain source names while operations use IDs.
- Optimized and unoptimized JIT/AOT execution has unchanged stdout and exit status, including ownership-heavy shadowing, loop exits, `try` propagation, and `inout` cases.
- Existing workspace tests, file-length, formatting, Clippy, CodeRabbit, and CodSpeed gates pass.

## Builder decisions

Use existing modules for `BindingId` and metadata rather than creating a catch-all module. Keep `DeclId`, `SourceId`, and UIR names unchanged. Preserve current APIs where practical, but make the identity transition explicit at TIR and Flow boundaries rather than adding a parallel name-based compatibility path.
