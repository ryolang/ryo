# Closure Representation and ABI

> Draft (v0.2+) — implementation-level detail extracted from specification.md Section 6.2.
> See specification.md Section 6.2 for closure semantics.

This section specifies the memory representation and calling conventions for closures. It complements the semantic rules in 6.2.2–6.2.3 with implementation-level details required for code generation and FFI interoperability.

## Memory Layout per Category

| Closure Category | Layout | Size | Notes |
|---|---|---|---|
| No captures | Thin function pointer | 1 pointer | Identical to a C function pointer. No environment allocated. |
| Immutable captures (`Fn`) | Environment struct + function pointer | 2 pointers (fat pointer) | Environment is stack-allocated when closure does not escape; heap-allocated (via ownership transfer) when it does. |
| Mutable captures (`FnMut`) | Environment struct + function pointer | 2 pointers (fat pointer) | Requires exclusive (`&mut`) access to the environment on each call. |
| Move captures (`FnMove`) | Owned environment struct + function pointer | 2 pointers (fat pointer) | Environment is consumed on call. Single-use unless the compiler can prove otherwise. |

The **environment struct** is an anonymous, compiler-generated struct containing each captured variable (or a reference to it) in declaration order. Field alignment follows platform ABI rules (same as user-defined structs with `#[repr(C)]` layout).

> **Design Choice:** Should the environment struct preserve capture declaration order, or should the compiler reorder fields for optimal packing? Declaration order aids debuggability; reordering reduces padding. *Resolution deferred to implementation phase.*

## Calling Convention

All closures use the **environment-passing** calling convention:

```
closure_fn(env_ptr, arg1, arg2, ...) -> ReturnType
```

- `env_ptr` is a pointer to the environment struct (hidden first parameter).
- For no-capture closures, `env_ptr` is `null` (or elided entirely when the compiler can prove no environment exists).

```ryo
# Source-level closure
adder = fn(x: int) -> int: x + offset

# Conceptual lowering (not user-visible syntax)
# struct __adder_env { offset: int }
# fn __adder_fn(env: &__adder_env, x: int) -> int: x + env.offset
```

**Optimization:** When a closure captures nothing, the compiler emits a bare function pointer without the environment parameter. This enables direct use as a C function pointer without wrapping.

> **Design Choice:** Should the no-capture optimization be guaranteed (a language invariant) or merely permitted (an optimization hint)? Guaranteeing it simplifies FFI rules but constrains future ABI evolution. *Recommended: guarantee for v0.2 FFI milestone.*

## FFI Constraints

FFI interoperability (v0.2+) imposes the following rules on closures crossing language boundaries:

1. **No-capture closures** may be passed directly as C function pointers (`fn(args) -> ret` maps to the equivalent C signature).
2. **Closures with captures** cannot cross FFI boundaries directly. They require an explicit wrapper:
   - A `#[callback]` attribute (future) that packages the closure as a C-compatible `(fn_ptr, void* user_data)` pair.
   - The caller on the C side invokes via `fn_ptr(user_data, args...)`.
3. **Lifetime safety:** Closures passed to C must not reference stack-local variables that may be deallocated before the C code invokes the callback. The compiler should reject such patterns or require `move` capture.

```ryo
# Valid: no-capture closure as C callback
extern fn register_handler(cb: fn(int) -> int):
	pass

register_handler(fn(x: int) -> int: x * 2)  # OK: no captures

# Invalid: closure with captures cannot be passed directly
offset = 10
# register_handler(fn(x: int) -> int: x + offset)  # ERROR: closure captures 'offset'
```

> **Design Choice:** The `#[callback]` wrapper mechanism vs. automatic thunk generation. Explicit wrappers are safer and more predictable (Zig philosophy); automatic thunks reduce boilerplate. *Resolution deferred to v0.2 FFI design.*

## Task Closure Interaction

Closures spawned as concurrent tasks (see Section 12) have additional representation constraints:

1. **Must be `FnMove`:** Task closures own their environment entirely. No borrowed references to the spawning scope are permitted.
2. **Send-safety:** The environment struct must contain only types that are safe to transfer across task boundaries. Specifically:
   - `shared[T]` handles must be explicitly cloned before capture (not borrowed from the parent scope).
   - Raw mutable references (`&mut T`) are prohibited in task closure environments.
3. **Heap allocation:** Task closure environments are always heap-allocated because their lifetime is decoupled from the spawning scope.

```ryo
fn spawn_worker(data: [int]):
	# Move data into the task closure — task owns it
	spawn move fn():
		for item in data:
			process(item)
	# 'data' is no longer accessible here

fn share_state():
	counter = shared[mutex[int]](0)
	handle = counter.clone()  # Explicit clone for task capture
	spawn move fn():
		lock val = handle.lock():
			val += 1
```

*(Rationale: Closure representation must balance performance — zero-cost for the common no-capture case — with safety guarantees for concurrent and FFI contexts. The fat-pointer model aligns with the planned `dyn Trait` representation (see `docs/dev/dyn_trait.md`), enabling future unification of closure trait objects and general trait objects under a single dispatch mechanism.)*
