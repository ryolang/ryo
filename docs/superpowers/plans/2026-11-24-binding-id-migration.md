# Binding Identity Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace name-based local bindings with dense body-local `BindingId` values in Sema, TIR, Flow, and backend storage, while preserving diagnostics and execution.

**Architecture:** Sema allocates IDs per function. A TIR body’s existing `DeclId` supplies context; its binding table carries name, type, span, and originating `ScopeId`. Flow uses a dense `BindingId -> PlaceId` map and keeps `PlaceId` separate for temporary storage and views. Backend uses dense binding slots and resolved Flow places.

**Tech Stack:** Rust, existing Ryo UIR/TIR/Flow representations, Cranelift backend, Cargo workspace.

**Spec:** `docs/superpowers/specs/2026-11-24-binding-id-migration-design.md`

## Global Constraints

- Add no language syntax or semantics, module system, generic system, or ownership-rule changes.
- Keep `DeclId`, `SourceId`, and UIR name handles unchanged.
- Allocate metadata per binding, not per read or instruction.
- Resolve each initializer before introducing its binding; shadowing mints a fresh ID; assignment and `inout` writeback reuse the existing ID.
- `PlaceId` remains distinct from `BindingId` because Flow also represents temporary storage and views.
- Do not remove control-flow state restoration required by joins; remove only undo entries whose sole purpose was name-shadowed binding restoration after tests prove the joins correct.
- Reject missing definitions, out-of-range IDs, cross-body IDs, stale artifacts, and out-of-scope uses before executable publication.
- Keep nominal identity and backend-symbol identity in a separate I-284 PR and plan.
- Preserve current diagnostics and optimized/unoptimized JIT/AOT behavior.

## Review Focus

- Shadowed owned values whose inner and outer bindings both need cleanup: `Task 2`, `Task 4`.
- Initializers and generated `try`/`catch` bindings that must not resolve before introduction: `Task 2`.
- Pattern bindings and loop exits where scope ancestry changes: `Task 3`, `Task 4`.
- Missing definitions, out-of-range IDs, cross-body IDs, stale artifacts, and out-of-scope uses: `Task 1`, `Task 3`, `Task 4`.
- Backend joins and cleanup with optimization on and off: `Task 4`.

---

### Task 1: Core binding identity and metadata

**Files:**
- Create: `ryo-core/src/binding.rs`
- Modify: `ryo-core/src/lib.rs`
- Modify: `ryo-core/src/tir.rs`
- Test: `ryo-core/src/tir/tests.rs`

**Interfaces:**
- Consumes: existing `DeclId`, `StringId`, `TypeId`, `Span`, and `ParamMode`.
- Produces: `BindingId`, `ScopeId`, and `BindingInfo` for all later tasks.

- [ ] **Step 1: Write the failing test**

Add `binding_ids_have_body_local_dense_indices` in `ryo-core/src/tir/tests.rs`:

```rust
let first = BindingId::from_index(0);
let second = BindingId::from_index(1);
assert_eq!(first.raw(), 0);
assert_eq!(second.raw(), 1);
assert_eq!(first.index(), 0);
assert_eq!(second.index(), 1);
let scope = ScopeId::from_index(2);
assert_eq!(scope.index(), 2);
```

Also add `binding_table_rejects_out_of_range_id`:

```rust
let table = BindingTable::new(vec![BindingInfo::new(
    pool.intern_str("value"),
    pool.int(),
    BindingKind::Local { mutable: false, borrowed: false },
    ScopeId::from_index(0),
    Span::EMPTY,
)]);
let error = table.require(BindingId::from_index(1)).unwrap_err();
assert!(error.contains("unregistered binding"));
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ryo-core binding_`
Expected: FAIL because `BindingId`, `ScopeId`, and `BindingTable` do not exist.

- [ ] **Step 3: Implement core identity types**

In `ryo-core/src/binding.rs` define:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BindingId(u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScopeId(u32);

impl BindingId {
    pub const fn from_index(index: usize) -> Self;
    pub const fn raw(self) -> u32;
    pub const fn index(self) -> usize;
}
impl ScopeId {
    pub const fn from_index(index: usize) -> Self;
    pub const fn index(self) -> usize;
}

pub enum BindingKind {
    Param(ParamMode),
    Local { mutable: bool, borrowed: bool },
    Temporary,
}

pub struct BindingInfo {
    pub name: StringId,
    pub ty: TypeId,
    pub kind: BindingKind,
    pub scope: ScopeId,
    pub span: Span,
}

pub struct BindingTable {
    bindings: Vec<BindingInfo>,
}
impl BindingTable {
    pub fn new(bindings: Vec<BindingInfo>) -> Self;
    pub fn len(&self) -> usize;
    pub fn get(&self, id: BindingId) -> Option<&BindingInfo>;
    pub fn require(&self, id: BindingId) -> Result<&BindingInfo, String>;
}
```

`BindingInfo::new` takes `(name, ty, kind, scope, span)`. `require` returns an `unregistered binding` error for an out-of-range ID. `BindingTable` does not resolve names or allocate global state.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p ryo-core binding_`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add ryo-core/src/binding.rs ryo-core/src/lib.rs ryo-core/src/tir.rs ryo-core/src/tir/tests.rs
git commit -m "feat: add body-local binding identities"
```

### Task 2: Sema allocation and TIR reads/writes

**Files:**
- Modify: `ryo-frontend/src/sema/mod.rs`
- Modify: `ryo-frontend/src/sema/expr/mod.rs`
- Modify: `ryo-frontend/src/sema/stmt.rs`
- Modify: `ryo-frontend/src/sema/match_.rs`
- Modify: `ryo-frontend/src/sema/errors.rs`
- Modify: `ryo-core/src/tir.rs`
- Modify: `ryo-core/src/tir/wires.rs`
- Modify: `ryo-core/src/tir/dump.rs`
- Test: `ryo-frontend/src/sema/tests.rs`
- Test: `ryo-frontend/src/sema/tests_errors.rs`
- Test: `ryo-frontend/src/sema/tests_match_typing.rs`
- Test: `ryo-core/src/tir/tests.rs`

**Interfaces:**
- Consumes: `BindingId`, `ScopeId`, `BindingInfo`, `BindingKind`, and `BindingTable` from Task 1.
- Produces: `Tir::bindings: BindingTable`; TIR params, reads, declarations, assignments, compound assignments, and for-loop bindings use `BindingId`. `FuncCtx` exposes `next_binding() -> BindingId` and `binding_table() -> &BindingTable`.

- [ ] **Step 1: Write failing tests**

Add `shadowed_bindings_have_distinct_ids_and_initializer_cannot_resolve_early` in `ryo-frontend/src/sema/tests.rs`:

```rust
let src = "fn main():\n\tvalue = 1\n\tif true:\n\t\tvalue = value + 1\n\tprint(value)\n";
let (tirs, pool) = run(src).unwrap();
let main = tir_named(&tirs, &pool, "main");
let declarations = (1..main.instructions.len())
    .filter_map(|index| {
        let reference = TirRef::from_raw(index as u32);
        (main.inst(reference).tag == TirTag::VarDecl)
            .then(|| main.var_decl_view(reference))
    })
    .collect::<Vec<_>>();
assert_eq!(declarations.len(), 2);
let outer = &declarations[0];
let inner = &declarations[1];
assert_ne!(outer.id, inner.id);
assert_eq!(pool.str(main.bindings().get(outer.id).name), "value");
assert_eq!(pool.str(main.bindings().get(inner.id).name), "value");
assert_ne!(
    main.bindings().get(outer.id).scope,
    main.bindings().get(inner.id).scope
);
```

Use the actual TIR declaration-view fields to compare `BindingId` values and scope IDs. Assert that the initializer reads the outer ID, not the inner ID.

Add `generated_try_and_catch_bindings_receive_ids` in `ryo-frontend/src/sema/tests_errors.rs`: compile a direct `try` and a direct `catch`, then assert that every generated `__ryo_try_success_*`, `__ryo_try_error_*`, `__ryo_catch_success_*`, and `__ryo_catch_error_*` has metadata and distinct IDs within its originating scope.

Add `tir_binding_wire_rejects_unregistered_id` in `ryo-core/src/tir/tests.rs`: construct a TIR body whose variable read uses `BindingId::from_index(99) and assert validation rejects it before backend preparation.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ryo-frontend binding_ && cargo test -p ryo-core tir_binding_wire_rejects_unregistered_id`
Expected: FAIL because TIR still emits `StringId` bindings and has no binding table.

- [ ] **Step 3: Implement Sema allocation and TIR metadata**

In `FuncCtx`, maintain `next_binding: u32` and `bindings: Vec<BindingInfo>`; `next_binding()` returns the current ID and increments it. Allocate parameters first in declaration order.

`Scope` gains `id: ScopeId` and maps names to records containing `BindingId`, type, mutability, borrow state, and dead-store state. Lookup returns that record. Initializers resolve before the new binding is inserted. Shadowing always calls `next_binding()`.

Update `resolve_var_read` to emit the record’s ID. Update declarations, assignments, compound assignments, for-loop variables, match/destructure bindings, and generated `try`/`catch` bindings to emit IDs. Assignment and `inout` writeback use the existing record’s ID.

`TirBuilder::finish` publishes `bindings: BindingTable`. `TirParam` gains `id: BindingId`; `TirParam::new` takes `(id, name, ty, mode, span)`. TIR variable operations and wire encoders use IDs. Dumps resolve labels through the table rather than lexical scopes.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p ryo-frontend binding_ && cargo test -p ryo-core tir_binding_wire_rejects_unregistered_id`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add ryo-frontend/src/sema ryo-core/src/tir.rs ryo-core/src/tir ryo-core/src/tir/tests.rs
git commit -m "feat: resolve local bindings in Sema"
```

### Task 3: Dense Flow lookup and scope validation

**Files:**
- Modify: `ryo-core/src/flow.rs`
- Modify: `ryo-core/src/flow/build.rs`
- Modify: `ryo-core/src/flow/invariants.rs`
- Modify: `ryo-frontend/src/pipeline.rs`
- Test: `ryo-core/src/flow/tests.rs`
- Test: `ryo-frontend/src/sema/declaration_identity_tests.rs`

**Interfaces:**
- Consumes: `Tir::bindings` and TIR `BindingId` operations from Task 2.
- Produces: Flow places with `BindingId`, dense `BindingId -> PlaceId` lookup, and publication errors for invalid scope ancestry.

- [ ] **Step 1: Write failing tests**

Add `flow_uses_binding_ids_without_name_resolution`:

```rust
let (flow, pool) = build_flow_with_shadowed_value();
let inner_id = BindingId::from_index(1);
let place = flow.resolve(inner_id).expect("inner binding");
assert_eq!(flow.place(place).ty(), pool.int());
assert_eq!(flow.resolve(BindingId::from_index(99)), None);
```

Use `Param`, `Local`, `LoopVar`, and `Token` fixtures to assert that `Place` stores the TIR `BindingId` and that `PlaceId` remains distinct.

Add `flow_rejects_use_outside_binding_scope`: build a body where an inner-scope ID is read after the scope closes. Expect a publication error containing `outside binding scope`.

Add `flow_rejects_cross_body_binding_id`: pair a valid TIR body with a Flow place carrying the same numeric ID from another body and assert the existing body/context checks reject the association.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ryo-core flow_`
Expected: FAIL because Flow still resolves `StringId` names lexically.

- [ ] **Step 3: Implement dense Flow places and validation**

Add `BindingId` to `Place::Param`, `Local`, `LoopVar`, and `Token`; remove their dependence on `StringId` for lookup. `PlaceId` remains the storage index.

In `FlowBuilder`, replace name-scopes with dense `BindingId -> PlaceId` maps plus `ScopeId` ancestry. Register IDs and places as TIR statements are built. `resolve` checks the current scope and ancestors; missing IDs or uses outside the originating scope return a publication error.

`require_backend_transition` validates every Flow place against the TIR binding table and existing body/context checks before names are resolved for diagnostics.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p ryo-core flow_`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add ryo-core/src/flow.rs ryo-core/src/flow/build.rs ryo-core/src/flow/invariants.rs ryo-core/src/flow/tests.rs ryo-frontend/src/pipeline.rs ryo-frontend/src/sema/declaration_identity_tests.rs
git commit -m "feat: resolve bindings in Flow"
```

### Task 4: Backend slots, cleanup, and end-to-end verification

**Files:**
- Modify: `ryo-backend/src/codegen/binding_storage.rs`
- Modify: `ryo-backend/src/codegen/function_context.rs`
- Modify: `ryo-backend/src/codegen/control.rs`
- Modify: `ryo-backend/src/codegen/mod.rs`
- Modify: `ryo-backend/src/codegen/decls.rs`
- Modify: `ryo-backend/src/codegen/match_.rs`
- Modify: `ryo-backend/src/codegen/frees.rs`
- Test: `ryo/tests/integration_basics.rs`
- Test: `ryo/tests/deep_expr_chain.rs`
- Test: `ryo-backend/tests/*`

**Interfaces:**
- Consumes: TIR binding metadata, Flow places, and `BindingId -> PlaceId` resolution from Tasks 2 and 3.
- Produces: dense backend binding tables, valid cleanup at joins, and stale/out-of-scope artifact rejection.

- [ ] **Step 1: Write failing execution and publication tests**

Add `shadowed_owned_value_cleanup_matches_optimized_and_unoptimized` in `ryo/tests/integration_basics.rs`:

```ryo
fn main():
	value = "outer"
	if true:
		value = "inner"
		print(value)
	print(value)
```

Run it with the project’s optimized and unoptimized JIT/AOT modes and assert `inner\nouter\n`. Add a second same-spelled payload with non-Copy storage to exercise both cleanup obligations.

Add `loop_exit_and_inout_writeback_preserve_binding_identity`: use a shadowed loop variable and an `inout` writeback; assert the inner binding is destroyed at loop exit and the caller’s original binding is updated, not duplicated.

Add `backend_rejects_cross_body_binding_id` and `backend_rejects_out_of_scope_binding_use` to backend artifact tests. Assert publication fails before Cranelift emission.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ryo-backend backend_ && cargo test -p ryo binding_`
Expected: FAIL because backend storage and validation still use `StringId`.

- [ ] **Step 3: Implement dense backend storage and cleanup validation**

Change `binding_storage.rs` slot keys from `StringId` to `BindingId`. Size tables from `Tir::bindings.len()` or the Flow body’s binding count. Update locals, fat locals, view locals, struct locals, and range facts to use IDs.

Route reads and writes by ID. Route cleanup by resolved Flow places. Remove undo entries whose sole purpose was name-shadowed binding restoration only after join tests prove that control-flow state still restores correctly. Keep required state restoration.

Use binding metadata for diagnostic names and spans. Reject unregistered, out-of-range, cross-body, stale, and out-of-scope IDs before mechanical emission.

- [ ] **Step 4: Run the tests and full verification**

Run: `cargo test -p ryo-backend backend_ && cargo test -p ryo binding_`
Expected: PASS

Run all standard gates:

```bash
cargo fmt --check
./scripts/check_file_length.sh
git diff --check
RUSTFLAGS=-Dwarnings cargo clippy --workspace --all-targets
cargo test --workspace
coderabbit review --agent --fresh
uv run benchmarks/run.py run --benchmarks fibonacci,fizzbuzz
```

Expected: all pass; no tracked performance regression. If CodSpeed flags a double-digit regression, fix it or record an explicit maintainer-approved decision.

- [ ] **Step 5: Commit**

```bash
git add ryo-backend/src/codegen ryo/tests/integration_basics.rs ryo/tests/deep_expr_chain.rs ryo-backend/tests
git commit -m "feat: use resolved bindings in backend"
```

### Task 5: Final branch review and handoff

**Files:**
- Modify: `docs/dev/guide_compiler.md`
- Modify: `docs/dev/architectural_fixes/pipeline_migration.md`

**Interfaces:**
- Consumes: completed binding-identity implementation from Tasks 1-4.
- Produces: accurate architecture documentation and a reviewed PR ready for nominal/backend-symbol work.

- [ ] **Step 1: Update architecture documentation**

Document body-local ID allocation, binding metadata, dense Flow lookup, scope-validity checks, backend slots, and the separate nominal/backend-symbol PR. Do not describe the temporary design as uncommitted context.

- [ ] **Step 2: Run final checks and independent review**

Run the full verification sequence from Task 4 Step 4, then request an independent read-only review of the complete branch and a fresh `coderabbit review --agent --fresh`. Verify each finding against current code and fix valid issues with regression coverage.

- [ ] **Step 3: Commit and open the PR**

```bash
git add docs/dev/guide_compiler.md docs/dev/architectural_fixes/pipeline_migration.md
git commit -m "docs: describe resolved binding identity"
gh pr create --fill
```

Push only after the user’s one-hour embargo permits it. Keep the PR unmerged until all checks pass and explicit merge approval is given. Keep the temporary design file until the nominal/backend-symbol slice is also complete.
