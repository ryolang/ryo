# Cranelift Lessons

**Status:** Complete

Hard-won findings from the 2026-08 performance work (Cranelift 0.131 → 0.135.1 upgrade, packed-`u128` string runtime ABI, value-range guard elision). Read this before touching `ryo-backend/src/codegen/` for optimization work — each item cost real investigation time and is not written down upstream in an obvious place.

## ABI and calling conventions

- **A 24-byte struct cannot be returned in registers across the C ABI.** SysV AMD64 and AAPCS64 both classify aggregates over 16 bytes as MEMORY → hidden sret pointer. "Return `{ptr, len, cap}` by value" is therefore unreachable for an `extern "C"` runtime; the original string runtime ABI (out-pointer + per-call-site stack slot) was a consequence of this, not a mistake.
- **The fix that shipped (commit `7d0a047`):** drop `extern "C"` for string-producing runtime functions and return `{ptr, len}` packed in one `u128` under the Rust ABI — registers on all targets (rax:rdx on x86-64, x0:x1 on aarch64). `cap` is derived at the codegen call site (0 for literals, `len` for allocating producers). Safe because the build.rs lockstep rebuild guarantees the runtime archive and compiler are always produced by the same rustc. Recorded on `pack_pair` in `runtime/src/lib.rs`; pinned by the `clif_string_ops_use_packed_return_no_stack_slots` integration test.
- **`enable_llvm_abi_extensions` is required for this pattern** despite its docs mentioning only Windows Fastcall: the x64 Rust-ABI `i128` return convention goes through it too (commit `50625fc`). The unrelated `enable_multi_ret_implicit_sret` flag does not conform to platform ABIs and may change between Cranelift versions — do not use it at the runtime boundary.
- **Why the old ABI was unfixable by optimization:** the out-pointer escaped into an opaque extern call, which is a hard alias-analysis barrier — no pass can prove the callee didn't stash or write through it. The only sound fixes were annotating the call (not expressible, see below) or not passing a pointer (what shipped).

## What Cranelift 0.135.1 cannot express

- **No non-returning calls.** Nothing matching `noreturn`/`does_not_return` exists in `cranelift-codegen/src/ir/` — calls are modeled as always-returning, so every `ryo_panic` call site keeps a trailing `trap` to satisfy the verifier.
- **No memory-access annotations on calls or signatures.** `MemFlags::readonly` exists on individual loads/stores only (`src/ir/memflags.rs`). There is no way to mark `ryo_str_eq` as non-clobbering, so every runtime call is an opaque memory clobber for the e-graph pass.
- **Trap-folding does not cover branch-to-call.** The 0.134/0.135 branch-to-trap fusion applies to guards that branch to a raw `trap`, not to Ryo's guards, which branch to a shared cold block calling `ryo_panic`. Switching to `trapz`/`trapnz` was considered and rejected — it would bypass the panic message/exit-code contract.

## Overflow-guard lowering

- Checked arithmetic (`sadd_overflow` + branch) lowers on aarch64 to `cset` + `tst` + `b.ne` when the flag is materialized into an SSA boolean before the branch — three instructions where one `b.vs` suffices. Fusion requires the branch to consume the flag value directly at the checked-op site. Tracked as I-165 in `ISSUES.md` (deferred: cheap-but-invisible on out-of-order hardware; see the measurement lesson below).

## Measurement lessons

- **Instruction count ≠ walltime on out-of-order cores.** The value-range guard elision removed 10 of 29 instructions per fib call (the exact predicted static win) and changed walltime by ~0%: the removed instructions were perfectly-predicted not-taken branches with ready inputs. Instruction counts are the right *regression gate* (deterministic); walltime is the headline — never extrapolate one from the other.
- **The Cranelift 0.131 → 0.135 upgrade produced no measurable benchmark change** — it landed for dependency currency and compile-time improvements, not generated-code speed. Expect the same from future version bumps; do not schedule them as performance work.
- **The remaining fibonacci gap decomposes structurally, not instructionally:** Rust's 1.00× comes from LLVM converting the second recursive call into an accumulator loop (halving call count) — a middle-end transform Cranelift does not have and Swift doesn't do either. Swift's 1.26× edge over Ryo is per-call instruction economy (fused guard branch, less regalloc churn). Closing either is TIR-level work, not codegen tweaks.
- JIT vs AOT: the `string_slicing` JIT gap (~2× AOT) persisted with the JIT at `opt_level=speed` — it is not an unoptimized-codegen artifact (measured 2026-08-26, `benchmarks/string_slicing/README.md`).

## References

- Issues: `ISSUES.md` — I-157, I-161, I-164, I-165 (and resolved I-140/I-141/I-142 in git history)
- Code: `runtime/src/lib.rs` (`pack_pair`), `ryo-backend/src/codegen/` (`mod.rs`, `expr.rs`)
- Benchmarks: `benchmarks/fibonacci/README.md` (guard-elision checkpoint), `benchmarks/string_slicing/README.md` (JIT/AOT gap), `benchmarks/string_building/README.md`
- Upstream: [Cranelift aegraph mid-end](https://cfallin.org/blog/2026/04/09/aegraph/), `cranelift-codegen-meta-0.135.1/src/shared/settings.rs` (flag definitions)
