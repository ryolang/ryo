# Testing Framework

The current roadmap (Milestone 26) proposes a standard "Cargo-like" or "Go-like" implementation (`#[test]`, `assert_eq`). While functional, **it is too bare-bones for a modern "DX-First" language.**

To compete with Python (pytest) and Go, Ryo needs to address the "Setup/Teardown" and "Mocking" problems, which are notoriously painful in compiled languages.

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

### 2. The "Setup/Teardown" Trap
**The Spec:** `#[test]` runs a function.
**The Problem:** Real backend tests need state.
*   Create a temp DB.
*   Seed data.
*   Run test.
*   **Cleanup (even if test fails/panics).**

In Go, this is manual and error-prone (`defer`). In Python (pytest), `fixtures` are powerful but implicit.

**Proposal:**
**RAII Fixtures (The Ryo Way).**
Since Ryo has a strict `Drop` trait (RAII), it can be leveraged for test fixtures.

```ryo
struct DbFixture:
    conn: Connection

# Runs on setup
fn create_db() -> DbFixture:
    # create temp db...
    return DbFixture(...)

# Runs on teardown (automatically called when 'fix' goes out of scope)
impl Drop for DbFixture:
    fn drop(&mut self):
        # delete temp db...

#[test]
fn test_query():
    fix = create_db() # Setup happens here
    # do testing...
    # Teardown happens automatically here, even on panic!
```
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

### 5. Benchmarks
**The Missing Piece:** `ryo test` exists, but `ryo bench` is missing.
**Why it matters:** Ryo positions itself between Go and Python. Users *will* want to measure if their Ryo code actually beats Python.

**Proposal:**
Add `#[bench]` attribute for v0.1.
```ryo
#[bench]
fn bench_parsing(b: &mut Bencher):
    for _ in b.iter():
        parse_heavy_json()
```
This is low effort to implement (run loop N times, measure time) but high value for adoption.

---

### 6. Private vs Public Testing
**The Question:** Where do tests live?
*   **Unit Tests:** Inside `src/my_module.ryo`. Can access private functions.
*   **Integration Tests:** Inside `tests/` directory. Treat the package as a black box (Public API only).

**Proposal:**
Formalize the `tests/` directory in the package structure.
*   Files in `src/*.ryo` are compiled as part of the library (access to privates).
*   Files in `tests/*.ryo` are compiled as *separate executables* that import the library (access to public only).

---

### Summary of Recommendations

1.  **Diffs:** Ensure `assert_eq` prints struct-level diffs, not just `!=`.
2.  **Fixtures:** Document and strictly test the **RAII (Drop)** pattern for test teardown. This is Ryo's "killer feature" for testing compared to Go's `defer` or Python's `yield`.
3.  **Benchmarks:** Add `#[bench]` to validate performance claims.
4.  **Integration Tests:** Define the `tests/` folder structure for black-box testing.
5.  **Dependency Injection:** Since Dynamic Dispatch is missing in v0.1, provide a standard library helper or documentation on mocking via **Function Pointers** (e.g., struct fields that hold `fn` types) so users are not blocked on testing DB interactions.
