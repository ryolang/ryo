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
```
ForRange(start=0, end=len, step=1):
    valA = LoadScalar(a, i)
    valB = LoadScalar(b, i)
    res  = FAddScalar(valA, valB)
    StoreScalar(c, i, res)
```

### Rewritten TIR (Conceptual)
```
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

---

## 3. Codegen Simplicity
By moving the complexity into the TIR rewriting phase, `codegen.rs` remains incredibly simple. 

When the Codegen phase encounters a `TirTag::FAddSimd4`, it simply requests Cranelift's native `fadd` instruction acting on Cranelift's native `f32x4` or `f64x2` types. It does not need to know that a loop was vectorized.

---

## Roadmap

To achieve this, the following implementation steps are required:

1. **Arrays and Memory:** Implement arrays, slices, and pointers in the parser, sema, and codegen. SIMD requires contiguous memory.
2. **SIMD TIR Primitives:** Expand `TirTag` to include explicit SIMD operations (e.g., `SimdLoad`, `SimdStore`, `SimdFAdd`).
3. **Cranelift SIMD Support:** Update `codegen.rs` to lower the new SIMD TIR tags into Cranelift SIMD types (`f64x2`, `i32x4`).
4. **The Pass:** Implement `src/opt/vectorize.rs` to detect simple loops and perform the strip-mining rewrite.