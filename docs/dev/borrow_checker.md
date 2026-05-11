**Status:** Design (v0.1) | Implementation (v0.1)

# Borrow Checker Algorithm Sketch

> Draft — implementation-level detail extracted from specification.md Section 5.
> See specification.md Section 5.3 for the formalized rules this algorithm enforces.

This section describes the algorithmic approach an implementer would use to enforce Rules 1–7 during compilation. The borrow checker runs as a pass over the HIR, after type checking.

### Move State Tracking

Each variable in scope carries a state:

```pseudocode
enum VarState:
    Uninitialized   # declared but not yet assigned
    Valid           # owns a value, accessible
    Moved           # ownership transferred, unusable
    PartiallyMoved  # struct with some fields moved out

fn check_use(var, location):
    match var.state:
        Uninitialized: error("use of uninitialized variable", location)
        Moved: error(f"use of moved variable '{var.name}' (moved at {var.moved_at})", location)
        PartiallyMoved: error(f"use of partially moved '{var.name}'", location)
        Valid: ok
```

State transitions:

```pseudocode
# Assignment: target becomes Valid
on assignment(target, expr):
    target.state = Valid

# Function call with move parameter (Rule 4):
on call(f, arg) where param is `move`:
    check_use(arg)
    arg.state = Moved
    arg.moved_at = current_location

# Return (Rule 1): returned variable is moved
on return(var):
    check_use(var)
    var.state = Moved

# Reassignment of mut variable: resets to Valid
on reassign(var) where var.is_mutable:
    var.state = Valid
```

### Rule 2 — Implicit Borrow for Parameters

When a function call passes an owned value to a regular parameter (not `move`, not `&mut`):

```pseudocode
on call(f, arg) where param is default (immutable borrow):
    check_use(arg)
    create_temporary_borrow(arg, kind=Immutable)
    # arg.state stays Valid — caller retains ownership
    # borrow is released when callee returns

on call(f, arg) where param is `&mut`:
    check_use(arg)
    assert arg.is_mutable, "cannot mutably borrow immutable variable"
    assert no_active_borrows(arg), "Rule 7: exclusive access violated"
    create_temporary_borrow(arg, kind=Mutable)
    # arg.state stays Valid
    # borrow is released when callee returns
```

### Rules 5 & 6 — Early Rejection (AST/Type Check Phase)

These are enforced before the borrow checker runs:

```pseudocode
# Rule 5: checked during type resolution
on function_signature(f):
    if f.return_type contains BorrowType:
        error("functions cannot return borrows (Rule 5)")

# Rule 6: checked during struct definition
on struct_definition(s):
    for field in s.fields:
        if field.type contains BorrowType:
            error("struct fields cannot be references (Rule 6); use shared[T] or an ID")
```

### Scope-Locked View Validation (Section 5.7 Enforcement)

Views (iterators) borrow their source collection and must not escape their block:

```pseudocode
on view_creation(view, source_collection, block_scope):
    create_borrow(source_collection, kind=Immutable, scope=block_scope)
    view.escape_scope = block_scope

on assignment(target, view) where target.scope > view.escape_scope:
    error("view cannot escape its enclosing block")

on return(view):
    error("views cannot be returned from functions (Rule 5 applies)")

on call(f, view) where f stores argument:
    error("view cannot be stored beyond its scope")

# Source collection is locked while view exists:
on mutation(collection) where active_view_borrows(collection):
    error("cannot mutate collection while a view is active (Rule 7)")
```

### Hybrid Eager Destruction (Section 5.4)

To implement Hybrid Eager Destruction, the borrow checker pass calculates the **last use** of every variable. 

```pseudocode
on function_end(f):
    # Path-sensitive ownership query checks if var is still Valid at a given location.
    # Applies merge rules at control-flow joins to skip scheduling on paths where moved.
    for var in function_variables:
        if var.type.implements_drop():
            # Resource: destroyed lexically at block end
            if query_ownership_state(var, at=var.scope.end) == Valid:
                insert_drop(var, location=var.scope.end)
        else:
            # Pure Memory: destroyed eagerly after last use
            last_use = find_last_read_or_write(var)
            if last_use != null:
                if query_ownership_state(var, at=last_use.next_instruction) == Valid:
                    insert_free(var, location=last_use.next_instruction)
            else:
                if query_ownership_state(var, at=var.declaration) == Valid:
                    insert_free(var, location=var.declaration)
```

### Edge Cases

**Conditional moves — branches must agree:**

```pseudocode
on if_else(cond, then_branch, else_branch):
    snapshot = save_var_states()
    check_branch(then_branch) -> then_states
    restore(snapshot)
    check_branch(else_branch) -> else_states

    for var in all_variables:
        if then_states[var] != else_states[var]:
            # Disagreement: var is "potentially moved" after the if
            var.state = Moved  # conservative — require reinit before use
```

**Loop mutations — condition variable must remain valid:**

```pseudocode
on for_condition_loop(cond_var, body):
    # cond_var must be Valid at loop re-entry
    check_body(body)
    if cond_var.state != Valid at end of body:
        error("loop condition variable moved/invalidated inside loop body")
```

**Closure captures:**

```pseudocode
on closure_creation(closure, captured_vars):
    for var in captured_vars:
        if closure.is_move_closure:
            var.state = Moved  # ownership transferred to closure
        else:
            create_borrow(var, kind=Immutable, scope=closure.lifetime)
            # Rule 7 still applies: no mutable access while borrow active
```

**Nested function calls — borrow stacking:**

```pseudocode
# f(g(x)) where both f and g take implicit borrows of x
on nested_call f(g(x)):
    # g borrows x (immutable) -> returns owned value
    # f receives g's return value (owned) — no conflict
    # SAFE: borrows don't overlap, g's borrow ends before f runs

# f(x, g(x)) where f and g both borrow x immutably
on call f(x, g(x)):
    # Multiple immutable borrows are allowed (Rule 7: many readers OK)
    # SAFE: both are immutable

# f(&mut x, g(x)) — mutable + immutable conflict
on call f(&mut x, g(x)):
    error("cannot mutably borrow 'x' while it is also immutably borrowed")
```

**Pattern matching — arms that move:**

```pseudocode
on match(scrutinee, arms):
    for arm in arms:
        snapshot = save_var_states()
        if arm.pattern binds by move:
            scrutinee.state = Moved
        check_body(arm.body) -> arm_states
        restore(snapshot)

    # After match: scrutinee is Moved (all arms may move from it)
    # Conservative: if ANY arm moves scrutinee, it's moved after match
    if any(arm moves scrutinee):
        scrutinee.state = Moved
```

## References

- Spec: `docs/specification.md`
- Dev: `docs/dev/implementation_roadmap.md`
- Milestone: Milestone 8
