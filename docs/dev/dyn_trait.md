# Dynamic Dispatch

---

### 1. Overview

Dynamic dispatch is best understood in contrast with static dispatch.

#### Static Dispatch (Monomorphization)
Ryo plans static dispatch for v0.1 via Generics. The compiler knows exactly which function to call at compile time.

*   **Scenario:** A function prints the area of a shape.
*   **Code:** `fn print_area[T: Shape](s: T) ...`
*   **Compiler Action:** If called with a `Circle` and a `Square`, the compiler generates two specialized copies:
    1.  `fn print_area_Circle(s: Circle)`
    2.  `fn print_area_Square(s: Square)`
*   **Pros:** Extremely fast (inlining).
*   **Cons:** Binary size bloat (code duplication).

#### Dynamic Dispatch (`dyn Trait`)
The compiler **does not know** the concrete type at compile time. It only knows that the object implements a given trait.

*   **Scenario:** A list containing both Circles and Squares.
*   **Code:** `fn print_area(s: &dyn Shape) ...`
*   **Compiler Action:** One function is generated. It expects a "Fat Pointer" containing:
    1.  A pointer to the data (the struct).
    2.  A pointer to a **vtable** (a lookup table of functions for that specific type).
*   **Runtime:** When `s.area()` is called, the program looks up the vtable to find the correct function address, then jumps to it.

---

### 2. Key Use Cases

Dynamic dispatch is effectively required for languages targeting "Web Backend" and "CLI Tools."

#### A. Heterogeneous Collections
A list of different objects that share behavior.
*   **Without Dynamic Dispatch:** Only `list[Circle]` or `list[Square]` is possible.
*   **With Dynamic Dispatch:** `list[&dyn Shape]` can hold a Circle, a Square, and a Triangle.

#### B. Binary Size & Compilation Time
Static dispatch (Generics) causes binary growth with every new type.
*   *Example:* `fn sort[T](list: [T])`. Sorting Integers, Floats, Strings, and Users produces 4 copies of the sort logic.
*   *With Dynamic:* The sort logic is generated once.

#### C. Testing (Dependency Injection)
This is the most critical use case.
*   **Real World:** A `Service` depends on a `Database`.
*   **Static Way:** `struct Service[DB: DatabaseTrait]`.
    *   This "infects" the entire codebase. Every struct holding the Service must also be generic ("Generic Soup").
*   **Dynamic Way:** `struct Service { db: &dyn DatabaseTrait }`.
    *   The Service is decoupled. At runtime, pass `PostgresDB`. In tests, pass `MockDB`. The struct definition stays clean.

---

### 3. What Dynamic Dispatch Enables

Dynamic dispatch enables **decoupling**.

It supports plugin architectures where the main application defines a `Plugin` trait, and users load libraries at runtime that implement that trait. The main app does not know `MyCoolPlugin` exists at compile time, but communicates with it through dynamic dispatch.

---

### 4. Roadmap Placement

Implementing **Vtables** (Virtual Method Tables) requires careful memory layout design and ABI stability.

#### Option A: True `dyn Trait`
Full Vtable implementation like Rust/C++.
*   **Roadmap:** Phase 5 (Post v0.1). Too complex for the initial release.

#### Option B: Enum Dispatch (Recommended for v0.1)
Since Ryo prioritizes "Ownership Lite" and DX, v0.1 can skip full dynamic dispatch by promoting **Enum Wrappers** as the official pattern.

**The Pattern:**
Instead of an interface `Database`, define an Enum that holds the possibilities.

```ryo
# The "Dynamic" Type
enum Database:
    Postgres(PostgresDB)
    SQLite(SqliteDB)
    Mock(MockDB)

# The Dispatch Logic
impl Database:
    fn query(self, sql: str):
        match self:
            Database.Postgres(db): db.query(sql)
            Database.SQLite(db):   db.query(sql)
            Database.Mock(db):     db.record(sql)
```

**Why this works for v0.1:**
1.  **Solves the Collection problem:** `list[Database]` is valid.
2.  **Solves the Testing problem:** `Database.Mock` can be injected.
3.  **Solves the Implementation problem:** Enums and Pattern Matching are already in the roadmap (Phase 2). This feature comes "for free" without complex Vtable logic in the compiler.

#### Recommendation

1.  **Phase 2 (Enums):** Ensure Enums are robust enough to hold Structs (Algebraic Data Types).
2.  **Documentation:** Write a guide on "Polymorphism in Ryo" explaining the Enum Pattern for testing and collections.
3.  **Phase 5 (Future):** Add true `dyn Trait` (Dynamic Dispatch) for cases where Enums are too rigid (e.g., user-defined plugins).

**Verdict:** Do **not** add `dyn Trait` / Vtables to the v0.1 roadmap. Use **Enums** to solve the user need instead.
