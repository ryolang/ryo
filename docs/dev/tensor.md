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
            # Call the C-level destructor provided by DLPack
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

### 4. Summary

*   **DLTensor** is inherently unsafe (raw C pointers, manual memory management).
*   **Ryo's Strategy:** Never expose `DLTensor` to the user.
*   **The Wrapper:** `struct Tensor[T]` owns the DLTensor.
*   **Safety Mechanisms:**
    1.  **Generics** solve Type Confusion.
    2.  **Private Fields + Accessors** solve Bounds Checking.
    3.  **`impl Drop`** solves Memory Leaks and Use-After-Free.

This **contains the blast radius** of unsafe code to the single file where `Tensor` is defined. The rest of the application remains safe.
