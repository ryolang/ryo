**Status:** Design (v0.2+) | Implementation (Deferred)

# Tensor Safety: DLPack Wrapping Strategy

DLPack is essentially a "Raw C Pointer with Metadata," which is inherently **unsafe**. Exposing `DLTensor` directly to Ryo users would break all safety guarantees (bounds checking, lifetime safety, type safety).

This document describes the standard design pattern ("The Safe Wrapper") Ryo uses to contain the unsafety.

---

### 1. DLPack: The Unsafe Reality
DLPack is a standard C struct (`DLTensor`) used to pass memory between frameworks (e.g., from PyTorch to TileLang) without copying bytes.

**The Anatomy of Danger:**
```c
typedef struct {
    void* data;         // 1. Raw void pointer (Type erased)
    DLContext ctx;      // 2. Device (CPU? GPU 0? GPU 1?)
    int ndim;           // 3. Number of dimensions
    DLDataType dtype;   // 4. Data type code (Float? Int?)
    int64_t* shape;     // 5. Raw pointer to size array
    int64_t* strides;   // 6. Raw pointer to stride array
    uint64_t byte_offset;
} DLTensor;
```

**Safety violations:**
1.  **Type Confusion:** `data` is `void*`. An array of `float` can be misinterpreted as `int`, producing garbage.
2.  **Buffer Overflow:** `shape` is just a pointer. If `ndim` says 3 but the array only has 2 items, reading `shape[2]` segfaults.
3.  **Use-After-Free (The Big One):** DLPack relies on a manual `deleter` function.
    *   Freeing the tensor in Ryo while TileLang is still reading it causes a crash or corruption.
    *   Forgetting to call the deleter causes a memory leak (VRAM OOM).
4.  **Data Races:** DLPack does not track whether the GPU is currently writing to that memory. Reading on CPU might yield partial data.

---

### 2. The Solution: The "Opaque Wrapper" Pattern

Ryo follows the "Rust/C++ Smart Pointer" pattern. The **unsafe** struct exists only inside the library implementation. The **safe** struct is what the user sees.

**User Experience (Safe):**
```ryo
# The user sees a safe, typed object.
# Raw pointers are inaccessible.
t: Tensor[f32] = Tensor.zeros([1024, 1024])

# When 't' goes out of scope, Ryo automatically cleans it up.
```

**Library Implementation (Hidden Unsafe):**

**Encapsulation** and **RAII (Drop)** sanitize the unsafety.

#### Step A: Define the Raw Struct (Internal Module)
Define the C struct in Ryo's `unsafe` FFI layer.
```ryo
# src/internal/dlpack.ryo
# Not exported to users!
unsafe struct DLTensor:
    data: *void
    ndim: int
    shape: *int64
    # ...
```

#### Step B: The Safe Wrapper (Public API)
Define a struct that owns the unsafe resource but exposes a safe API.

```ryo
# src/tensor.ryo
import internal.dlpack

# 1. Generic Type preserves Type Safety (prevents float/int mixups)
pub struct Tensor[T]:
    # 2. The handle is private. User cannot touch it.
    _handle: *dlpack.DLManagedTensor 
    
    # 3. Metadata stored locally for safe access
    _shape: list[int] 

# 4. RAII: When the variable goes out of scope, this runs.
impl Drop for Tensor[T]:
    fn drop(&mut self):
        unsafe:
            # Call the C-level destructor provided by DLPack.
            # External backends invoked via the FFI must return a heap-allocated
            # DLManagedTensor* whose dl_tensor points to the GPU data.
            # Ownership of that DLManagedTensor is transferred exactly once to Ryo.
            # Ryo will call the DLManagedTensor->deleter exactly once when the created
            # Tensor[T] is finalized, so backends must not free the DLManagedTensor or its
            # dl_tensor after returning it (they may only free any temporary host-side structures).
            # The deleter must correctly free GPU memory and the DLManagedTensor itself.
            if self._handle != null:
                self._handle.deleter(self._handle)
```

#### Step C: Safe Accessors
When the user requests data, bounds are checked *before* touching the raw pointer.

```ryo
impl Tensor[T]:
    fn get(self, index: int) -> T:
        # 1. Runtime Bounds Check (Safety)
        if index >= self._shape[0]:
            panic("Index out of bounds")
        
        unsafe:
            # 2. Only now is the pointer accessed
            ptr = self._handle.dl_tensor.data
            return *ptr.offset(index)
```

---

### 3. Integration with TileLang

When calling a TileLang function, the pointer is temporarily "leased" to C.

```ryo
# Ryo Code
fn matmul(a: Tensor[f32], b: Tensor[f32]) -> Tensor[f32]:
    # Validate shapes match
    if a.shape[1] != b.shape[0]: panic("Shape mismatch")
    
    unsafe:
        # Pass raw pointers to TileLang C ABI
        # Safe because 'a' and 'b' are kept alive by Ryo
        # until this function returns.
        raw_c_ptr = tilelang_api.call_kernel(a._handle, b._handle)
        
        # Wrap the result in a new Safe Tensor
        return Tensor.from_raw(raw_c_ptr)
```

### 4. CPU vs. GPU Execution Strategy (Cranelift Limitations)

Because Ryo uses Cranelift as its backend, the compiler can only generate code for **CPUs**. Cranelift cannot emit PTX (Nvidia), AMDGPU, or SPIR-V instructions. To achieve GPU execution, Ryo acts as the **Host Orchestrator**.

**CPU Execution (Ryo + Cranelift SIMD):**
If a tensor's `DLContext` is set to `CPU`, Ryo's internal vectorization pass (see `docs/experimental/lightweight_vectorization.md`) will optimize simple scalar loops over the tensor's memory and emit extremely fast AVX/NEON CPU SIMD instructions natively via Cranelift.

**GPU Execution (Ryo + FFI + External Kernels):**
If a tensor's `DLContext` is set to `GPU`, Ryo does not compile the math loops itself. Instead:
1. The user calls a tensor method (e.g., `a.matmul(b)` or `a + b`).
2. The Ryo standard library checks the device context.
3. If it is on a GPU, Ryo uses the Foreign Function Interface (FFI) to pass the raw `DLTensor` pointers to an external backend (like TileLang, LibTorch, or custom CUDA kernels).
4. The external library executes the math on the GPU, leaving the result in VRAM.
5. Ryo receives the pointer to the result and safely wraps it in a new `Tensor[T]` struct.

This allows Ryo to compile instantly while still driving state-of-the-art GPU workflows.

---

### 5. Summary

*   **DLTensor** is inherently unsafe (raw C pointers, manual memory management).
*   **Ryo's Strategy:** Never expose `DLTensor` to the user.
*   **The Wrapper:** `struct Tensor[T]` owns the DLTensor.
*   **Safety Mechanisms:**
    1.  **Generics** solve Type Confusion.
    2.  **Private Fields + Accessors** solve Bounds Checking.
    3.  **`impl Drop`** solves Memory Leaks and Use-After-Free.

This **contains the blast radius** of unsafe code to the single file where `Tensor` is defined. The rest of the application remains safe.

## References

- Spec: `docs/specification.md`
- Dev: `docs/dev/implementation_roadmap.md`
- Milestone: Milestone 8
