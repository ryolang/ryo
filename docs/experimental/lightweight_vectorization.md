# Lightweight Auto-Vectorization (Mini-MLIR Approach)

**Status:** Experimental Design  
**Prerequisites:** Arrays, Slices, and Pointers must be implemented in Ryo first.

## Objective
Enable automatic SIMD (Single Instruction, Multiple Data) vectorization for High-Performance Computing (HPC) workloads in Ryo, without sacrificing fast compile times or introducing heavy C++ dependencies like LLVM/MLIR.

## The Strategy: A Custom TIR "Middle-End" Pass
Instead of relying on Cranelift (which lacks auto-vectorization) or MLIR (which is a massive dependency), we will build a lightweight "Mini-MLIR" pass directly into Ryo's pipeline. 

This pass will operate strictly on the Typed Intermediate Representation (TIR). It will use the **80/20 Rule**: pattern matching only the simplest, most common loops (Maps, Zips, Folds) and rewriting them into explicit SIMD instructions.

### The Pipeline Architecture
1. **AST generation** -> *Unchanged*
2. **Sema (Typechecking)** -> *Emits standard, scalar TIR*
3. **`src/opt/vectorize.rs` (New)** -> *Scans the TIR, proves safety, and rewrites loops into SIMD chunks*
4. **Codegen** -> *Reads the rewritten TIR and emits Cranelift SIMD primitives trivially*

---

## 1. Pattern Matching (Safety Analysis)
To avoid the immense complexity of mathematical dependency analysis (polyhedral modeling), the vectorizer will only target "Strictly Simple" loops.

A `ForRange` loop is eligible for vectorization **if and only if**:
1. The loop increments strictly by `1`.
2. The loop body contains **no branching** (`if`, `break`, `continue`, function calls).
3. Memory accesses use the loop variable `i` directly (e.g., `array[i]`), guaranteeing contiguous memory loading without loop-carried data dependencies.

**Eligible (Zip pattern):**
```python
for i in range(0, 100):
    c[i] = a[i] + b[i]
```

**Ineligible (Loop-carried dependency):**
```python
for i in range(1, 100):
    a[i] = a[i-1] + b[i]
```

---

## 2. TIR Rewriting (Strip-Mining)
Once a loop is proven safe, the pass rewrites the TIR. It performs **Strip-Mining**, splitting the user's single loop into a SIMD Main Loop and a Scalar Tail Loop.

If a target CPU supports 4x Float SIMD registers, the TIR is rewritten as follows:

### Original TIR (Conceptual)
```ryo
ForRange(start=0, end=len, step=1):
    valA = LoadScalar(a, i)
    valB = LoadScalar(b, i)
    res  = FAddScalar(valA, valB)
    StoreScalar(c, i, res)
```

### Rewritten TIR (Conceptual)
```ryo
// Main SIMD Loop (Steps by 4)
let limit = len - (len % 4)
ForRange(start=0, end=limit, step=4):
    valA = LoadSimd4(a, i)
    valB = LoadSimd4(b, i)
    res  = FAddSimd4(valA, valB)
    StoreSimd4(c, i, res)

// Scalar Tail (Handles the leftovers)
ForRange(start=limit, end=len, step=1):
    valA = LoadScalar(a, i)
    valB = LoadScalar(b, i)
    res  = FAddScalar(valA, valB)
    StoreScalar(c, i, res)
```

## 3. Integrating with Tensors (DLPack) and Operator Ergonomics

This lightweight vectorization strategy works perfectly with Ryo's DLPack Tensor wrappers (see `docs/dev/tensor.md`). However, connecting safe, DLPack-backed Tensors to raw SIMD loops requires addressing two conflicts: **Safety Checks** and **Operator Ergonomics**.

### A. Bridging Safety and SIMD (Bounds Hoisting)
The safe `Tensor.get(index)` method inherently contains a bounds-checking `if` branch, which violates the "Strictly Simple" rule of the vectorizer. To allow Tensor loops to vectorize, the compiler must perform optimizations before the vectorization pass:

1. **Inlining:** The compiler automatically inlines `get()` and `set()` into the loop body.
2. **Bounds Hoisting:** The compiler detects that the loop boundaries (`0` to `len`) are bounded by the tensor shape, and hoists the panic-branch *outside* the loop.
3. **Contiguity Verification:** The compiler inserts a runtime check ensuring the DLPack `stride == 1` before entering the SIMD loop.

Once these safety checks are hoisted, the loop body becomes a "Strictly Simple" sequence of raw pointer offsets, allowing the vectorizer to strip-mine it safely.

### B. Operator Ergonomics (The "PyTorch" Standard)
High-Performance Computing and Machine Learning users (coming from PyTorch, TileLang, or Mojo) absolutely expect mathematical operators (`a + b`, `w * x`) to work on Tensors. However, allowing arbitrary user-level operator overloading creates significant complexity in the compiler (trait resolution, dynamic dispatch).

To keep the compiler simple while satisfying industry standards, Ryo uses **Compiler-Hardcoded Ergonomics for Tensors**:

1. **No User Overloading:** Users cannot define `impl Add` for their own structs.
2. **Semantic Interception:** In `sema.rs`, if a `BinaryOp::Add` involves two `Tensor` types, the compiler silently desugars it into a method call: `a.add(b)`.
3. **Standard Library Power:** The `.add()` method is written natively in the Ryo standard library using a simple, bounds-checked `for-range` scalar loop.

**The Full Pipeline in Action:**
1. User writes PyTorch-style math: `c = a + b`
2. Sema desugars to: `c = a.add(b)`
3. The stdlib `add()` function contains a simple scalar loop.
4. The optimizer hoists the bounds checks.
5. The Mini-MLIR pass detects the now-branchless loop and rewrites it into explicit SIMD.
6. Cranelift emits AVX/NEON instructions.

This architecture grants Ryo world-class Tensor ergonomics and raw C/SIMD performance while keeping the semantic analyzer and codegen engines radically simple.

---

## 4. Performance Observability (Missed Vectorization Warnings)

A major pain point in languages like C++ and Rust is "silent vectorization failure." A developer writes a loop expecting it to run 4x faster via SIMD, but accidentally introduces a branch or a memory dependency. The compiler silently aborts vectorization and emits slow scalar code, leaving the user guessing.

To solve this, Ryo's vectorizer will embrace **explicit performance observability**:

1. **Diagnostic Feedback:** If the compiler analyzes a loop that performs array/tensor memory access but *fails* the "Strictly Simple" checklist, it will emit a compiler warning (`DiagCode::MissedVectorization`).
2. **Actionable Reasons:** The warning will explicitly state *why* vectorization failed (e.g., `"Loop vectorization failed: detected conditional 'if' branch at line 42"` or `"detected loop-carried dependency on variable 'a'"`).
3. **Controlling Noise (Opt-In):** To prevent spamming users writing basic non-math scripts, these warnings must be opt-in. This can be controlled via:
   * A compiler flag: `ryo build --warn-simd`
   * Or an explicit loop annotation: `@simd for i in range(0, 100):` (If annotated, failure to vectorize becomes a hard compiler error).

---

## 5. Codegen Simplicity
By moving the complexity into the TIR rewriting phase, `codegen.rs` remains incredibly simple. 

When the Codegen phase encounters a `TirTag::FAddSimd4`, it simply requests Cranelift's native `fadd` instruction acting on Cranelift's native `f32x4` or `f64x2` types. It does not need to know that a loop was vectorized.

---

## Roadmap

To achieve this, the following implementation steps are required:

1. **Arrays and Memory:** Implement arrays, slices, and pointers in the parser, sema, and codegen. SIMD requires contiguous memory.
2. **SIMD TIR Primitives:** Expand `TirTag` to include explicit SIMD operations (e.g., `SimdLoad`, `SimdStore`, `SimdFAdd`).
3. **Cranelift SIMD Support:** Update `codegen.rs` to lower the new SIMD TIR tags into Cranelift SIMD types (`f64x2`, `i32x4`).
4. **The Pass:** Implement `src/opt/vectorize.rs` to detect simple loops and perform the strip-mining rewrite.