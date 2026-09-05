**Status:** Design (v0.2+)

# Testing Framework

Spec §15 now defines the baseline framework: `#[test]` with timeouts, `#[bench]` benchmarks, RAII (`Drop`) fixtures, and the `tests/` black-box integration directory. Those proposals have been absorbed into the spec and are no longer tracked here.

What remains below are the gaps the spec does **not** yet cover — the recommendations still needed for a modern "DX-First" language to compete with Python (pytest) and Go.

---

### 1. The "Mocking" Gap (Critical for Backend)
**The Constraint:** Ryo uses **Static Dispatch** (Monomorphization) for Traits in v0.1.0.
**The Problem:**
*   In Python, objects are patched at runtime. Easy.
*   In Go/Java, Interfaces are used.
*   In Ryo (v0.1), without Dynamic Dispatch (`dyn Trait`), dependency injection becomes verbose.

*Scenario:* Testing a function `save_user` without hitting the real database.
```ryo
# If traits are static only, app code looks like this:
fn save_user[D: Database](db: D, user: User) ...

# A separate binary must be compiled for tests where 'D' is 'MockDatabase'.
```
**The DX Fail:** This forces users to make *everything* generic just to be testable. This creates "Generic Soup" (visual noise), violating the "Simple like Python" goal.

**Proposal:**
**Conditional Compilation (Test-only Swapping).**
Without vtables (dynamic dispatch), allow swapping implementations at compile time specifically for the test profile.
```ryo
# src/db.ryo
pub struct Database: ... 

# tests/mocks.ryo
pub struct MockDatabase: ...

# In code
#[cfg(test, swap=Database with MockDatabase)] 
# (Likely too complex for v0.1, but the problem needs acknowledgement).
```
**Better Proposal for v0.1:**
For general-purpose backend work, **Interfaces (Dynamic Dispatch)** are almost mandatory for testing. If v0.1 lacks them, a standard pattern for **Dependency Injection via Function Pointers** must be provided, or testing DB interactions becomes impractical.

---

### 2. Drop-on-Panic Output Capture
Spec §15 covers the RAII (`Drop`) fixture pattern for setup/teardown. One runner requirement remains:

**Action:** Ensure the Test Runner captures output *during* Drop panics, and ensure `Drop` is guaranteed to run even if the test assertion fails.

---

### 3. Assertions & Diffing
**The Spec:** `assert(bool)` and `assert_eq(a, b)`.
**The Smell:**
*   `assert(user_a == user_b)` fails with: `Assertion failed`.
*   `assert_eq(user_a, user_b)` fails with: `Left != Right`.

**DX Requirement:**
For a language claiming "Python-like DX," **structural diffs** are needed.
Comparing two large `User` structs that differ by one field should produce a field-level diff.
*   *Bad:* `User(id=1, name="A") != User(id=1, name="B")`
*   *Good:* 
    ```text
    Diff:
      User {
        id: 1,
    -   name: "A",
    +   name: "B",
      }
    ```
**Proposal:**
`assert_eq` should require that types implement a `Debug` or `Diff` trait (auto-derivable), and print the actual field-level difference.

---

### 4. Table-Driven Tests (Parametrized)
**The Context:** Go developers use table-driven tests. Python developers use `@pytest.mark.parametrize`.
**The Missing Piece:** The Spec does not mention how to do this cleanly.

**Proposal:**
Since Ryo supports struct literals and arrays elegantly, explicitly endorse/document the loop pattern, or add a macro/attribute later.

```ryo
#[test]
fn test_addition():
    cases = [
        (1, 2, 3),
        (0, 0, 0),
        (-1, 1, 0),
    ]
    
    for (a, b, expected) in cases:
        # CRITICAL: If this fails, identify WHICH case failed.
        # Standard 'assert' is not enough.
        assert_eq(a + b, expected, f"Failed on case {a} + {b}")
```

---

### Summary of Recommendations

1.  **Diffs:** Ensure `assert_eq` prints struct-level diffs, not just `!=`.
2.  **Dependency Injection:** Since Dynamic Dispatch is missing in v0.1, provide a standard library helper or documentation on mocking via **Function Pointers** (e.g., struct fields that hold `fn` types) so users are not blocked on testing DB interactions.
3.  **Table-Driven Tests:** Endorse/document the loop-over-cases pattern, with failure messages that identify which case failed.
4.  **Drop-on-Panic Capture:** The Test Runner must capture output during Drop panics and guarantee `Drop` runs even when a test assertion fails.

## References
- Spec: `docs/specification.md` Section 15 (Testing Framework)
- Roadmap: `docs/dev/implementation_roadmap.md` (Milestone 26)
