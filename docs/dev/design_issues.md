**Status:** Design (open specification questions)

# Ryo Specification Design Issues

This file tracks flaws, contradictions, and pending decisions in the existing specification.
New features and language extensions belong in [proposals.md](proposals.md).
Compiler implementation work belongs in its development document or `ISSUES.md`.
The [roadmap](implementation_roadmap.md) owns delivery status.

Remove an entry when its resolution reaches the specification. Git history preserves prior discussion.
Clarifications can resolve documented gaps; changes to language behavior require explicit human approval.

**Last updated:** 2026-10-14

## Open Specification Issues

### 1. Inherent Struct Methods Lack an Explicit Contract

**Existing contract:** Section 4.6 describes inherent enum methods through `impl EnumName:`.
Section 4.5 describes named structs but does not give the corresponding inherent-method contract.
The methods milestone uses `impl Rectangle:`.

**Gap:** Contributors lack an explicit statement that inherent implementations also apply to structs.
This is a completeness problem for an already intended feature, not a proposal for new syntax.

**Resolution:** Clarify inherent struct implementations in Section 4.5 and cross-reference the existing receiver rules in Section 5.
Do not introduce different receiver or ownership semantics through this clarification.

**Needed before:** Implementation of inherent struct methods. See the roadmap's methods milestone.

### 2. Error-Trace Wording Does Not Consistently Respect Configuration

**Existing contract:** Section 7.10 defines `full`, `minimal`, and `off` capture modes.
Section 4.10 describes full-stack creation and propagation records without qualifying those statements by the selected mode.
Section 7.6 also describes panic traces in unconditional terms.

**Flaw:** These passages can imply mandatory capture despite the documented configuration controls.
Debug-symbol stripping and runtime display controls are separate from build-time capture policy.

**Resolution:** Qualify default trace descriptions by the existing capture configuration.
Clarify how the documented mode applies to errors and panic reporting, including runtime-profile behavior where applicable.
Preserve the existing distinction between capture, display, and debug symbols.
If those passages intend different panic and error policies, obtain a decision before changing behavior.

**Needed before:** Configurable trace implementation and its diagnostics/documentation tests.
No new flag, attribute, or measured overhead claim is needed to reconcile the current wording.

### 3. Panic Boundaries and Destruction Need a Consistent Contract

**Existing contract:** Section 7.6 says panic immediately aborts the process without cleanup.
Section 14.5.6 says a panic inside `task.run` or `task.spawn_detached` kills only that task.
Section 9 requires cancellation cleanup to run destructors and permits those destructors to yield.

**Flaw:** The general panic rule does not state the task-boundary exception.
The passages do not clearly define cleanup when a destructor panics during cancellation or task-local panic handling.
The former double-panic entry incorrectly assumed panic unwinding and undefined behavior as the general rule.

**Resolution needed:** Define the applicability of process abort and task-isolated panic.
Specify destruction obligations at a task panic boundary and panic behavior during cancellation cleanup.
Distinguish these cases from ordinary resource destruction and cancellation deadline termination.
Clarify existing intent where supported; obtain approval for behavior that the current specification does not settle.
Do not select a new panic policy in a compiler implementation note.

**Needed before:** Task panic isolation, cancellation lowering, and exceptional cleanup implementation.

## References

- Spec: [Section 4.5](../specification.md#45-struct-type-product-type), [Section 4.6](../specification.md#46-enum-type-sum-type--algebraic-data-type---adt), and [Section 5](../specification.md#5-memory-management-ownership-lite) for inherent methods and receivers.
- Spec: [Section 4.10](../specification.md#410-error-trait-and-message-handling) and [Section 7](../specification.md#7-error-handling) for error traces, panic, and capture configuration.
- Spec: [Section 9](../specification.md#9-concurrency-model-taskfuturechannel) and [Section 14](../specification.md#14-standard-library) for cancellation and task panic isolation.
- Feature proposals: [proposals.md](proposals.md).
- Delivery: [Implementation roadmap](implementation_roadmap.md).
