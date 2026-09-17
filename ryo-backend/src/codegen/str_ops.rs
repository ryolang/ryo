//! Inlined tiny string ops — split from `expr.rs` to keep every file
//! under the 2000-line CI cap (`scripts/check_file_length.sh`).
//!
//! The bodies of `__ryo_slice` / `__ryo_bytes_slice` and the
//! short-literal specialization of `ryo_str_eq` are emitted as inline
//! Cranelift IR at the call site instead of extern calls: each body is
//! a handful of instructions, and the call boundary cost dominated
//! (`benchmarks/string_slicing` made two such calls per scan
//! iteration). Slice panic paths keep the runtime contract (stderr
//! message + exit 101) by branching to the shared cold `ryo_panic`
//! blocks (`emit_panic_guard`).

use cranelift::codegen::ir::{BlockArg, MemFlagsData};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::{TirData, TirRef, TirTag};
use ryo_core::types::StringId;

use super::{Codegen, FunctionContext};

/// Upper size bound for the inline byte-compare specialization of
/// `==`/`!=` against a string literal.
const INLINE_LITERAL_MAX: usize = 16;

impl<M: Module> Codegen<M> {
    /// Inline form of `__ryo_slice` / `__ryo_bytes_slice`
    /// (`runtime/src/lib.rs`): bounds check, UTF-8 char-boundary checks
    /// (str only — bytes slices skip them), then pointer arithmetic.
    /// Failures branch to cold `ryo_panic` blocks with the same
    /// messages and exit-101 contract the runtime `slice_fail` used.
    ///
    /// Preserves the null-when-empty invariant: the result pointer is
    /// null when the base length is 0, and every consumer guards on
    /// `len == 0` before dereferencing.
    pub(crate) fn emit_slice_inline(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        base_ptr: Value,
        base_len: Value,
        start: Value,
        end: Value,
        is_bytes: bool,
    ) -> Result<(Value, Value), String> {
        let start_gt_end = builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThan, start, end);
        let end_gt_len = builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThan, end, base_len);
        let out_of_range = builder.ins().bor(start_gt_end, end_gt_len);
        Self::emit_panic_guard(builder, ctx, out_of_range, "slice index out of range")?;

        if !is_bytes {
            Self::emit_char_boundary_guard(builder, ctx, base_ptr, base_len, start)?;
            Self::emit_char_boundary_guard(builder, ctx, base_ptr, base_len, end)?;
        }

        let out_len = builder.ins().isub(end, start);
        let advanced = builder.ins().iadd(base_ptr, start);
        let zero_len = builder.ins().iconst(types::I64, 0);
        let base_empty = builder.ins().icmp(IntCC::Equal, base_len, zero_len);
        let null = builder.ins().iconst(ctx.int_type, 0);
        let out_ptr = builder.ins().select(base_empty, null, advanced);
        Ok((out_ptr, out_len))
    }

    /// Panic unless index `i` lies on a UTF-8 char boundary of
    /// `ptr[..len]`: boundaries at 0 and `len` pass trivially;
    /// otherwise the byte at `i` must not be a continuation byte
    /// (`b & 0xC0 != 0x80`). The byte load sits in its own block so it
    /// never executes at the edges, where `ptr + i` can be one past
    /// the allocation (or null for an empty base).
    fn emit_char_boundary_guard(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        ptr: Value,
        len: Value,
        i: Value,
    ) -> Result<(), String> {
        let zero = builder.ins().iconst(types::I64, 0);
        let at_start = builder.ins().icmp(IntCC::Equal, i, zero);
        let at_end = builder.ins().icmp(IntCC::Equal, i, len);
        let is_edge = builder.ins().bor(at_start, at_end);

        let check_block = builder.create_block();
        let cont_block = builder.create_block();
        builder.ins().brif(is_edge, cont_block, &[], check_block, &[]);

        // Single predecessor (the brif above) — seal immediately.
        builder.seal_block(check_block);
        builder.switch_to_block(check_block);
        let addr = builder.ins().iadd(ptr, i);
        let byte = builder
            .ins()
            .load(types::I8, MemFlagsData::trusted(), addr, 0);
        let masked = builder.ins().band_imm_u(byte, 0xC0);
        let is_continuation = builder.ins().icmp_imm_u(IntCC::Equal, masked, 0x80);
        Self::emit_panic_guard(
            builder,
            ctx,
            is_continuation,
            "slice index is not a UTF-8 char boundary",
        )?;
        // emit_panic_guard switched to its own fall-through block.
        builder.ins().jump(cont_block, &[]);

        // Two predecessors (edge brif arm + checked fall-through), both
        // added above.
        builder.seal_block(cont_block);
        builder.switch_to_block(cont_block);
        Ok(())
    }

    /// `str`/`strview` equality (M8.4 §3.3). When either operand is a
    /// string literal of at most `INLINE_LITERAL_MAX` bytes, emits an
    /// inline compare — length check plus per-byte compares of the
    /// other side against the compile-time bytes, gated behind the
    /// length check so loads never run past the other buffer. Anything
    /// else falls back to the extern `ryo_str_eq(ptr, len, ptr, len)`.
    pub(crate) fn emit_str_eq(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        tag: TirTag,
        lhs: TirRef,
        rhs: TirRef,
    ) -> Result<Value, String> {
        // Operands may be owned str triples or strview view pairs
        // (mixed equality wraps the owned side in ToView); only
        // (ptr, len) is read.
        let (l_ptr, l_len) = Self::eval_str_or_view_parts(builder, ctx, lhs)?;
        let (r_ptr, r_len) = Self::eval_str_or_view_parts(builder, ctx, rhs)?;

        let literal = Self::strconst_id(ctx, lhs)
            .map(|id| (true, id))
            .or_else(|| Self::strconst_id(ctx, rhs).map(|id| (false, id)));
        let mut inline = None;
        if let Some((is_lhs, id)) = literal {
            let content = ctx.pool.str(id);
            if content.len() <= INLINE_LITERAL_MAX {
                let mut bytes = [0u8; INLINE_LITERAL_MAX];
                bytes[..content.len()].copy_from_slice(content.as_bytes());
                inline = Some((is_lhs, content.len(), bytes));
            }
        }

        let result = if let Some((is_lhs, n, bytes)) = inline {
            let (other_ptr, other_len) = if is_lhs {
                (r_ptr, r_len)
            } else {
                (l_ptr, l_len)
            };
            Self::emit_literal_eq(builder, other_ptr, other_len, n, &bytes)
        } else {
            let eq_ref = Self::declare_runtime_fn(
                ctx.module,
                builder,
                "ryo_str_eq",
                &[ctx.int_type, types::I64, ctx.int_type, types::I64],
                &[types::I8],
            )?;
            let call = builder
                .ins()
                .call(eq_ref, &[l_ptr, l_len, r_ptr, r_len]);
            builder.inst_results(call)[0]
        };

        if tag == TirTag::StrCmpNe {
            let one = builder.ins().iconst(types::I8, 1);
            Ok(builder.ins().bxor(result, one))
        } else {
            Ok(result)
        }
    }

    /// The `StringId` behind a `str`-typed operand when it is
    /// statically a string literal — directly (`StrConst`) or through
    /// the owner→view `ToView` wrap. Anything else is `None`.
    fn strconst_id(ctx: &FunctionContext<'_, M>, r: TirRef) -> Option<StringId> {
        let inst = ctx.tir.inst(r);
        match (inst.tag, inst.data) {
            (TirTag::StrConst, TirData::Str(id)) => Some(id),
            (TirTag::ToView, TirData::UnOp(inner)) => Self::strconst_id(ctx, inner),
            _ => None,
        }
    }

    /// Inline `other == <n known bytes>`: `other_len == n`, then `n`
    /// byte compares. The compares live in their own block reached only
    /// when the lengths match, so `other_ptr` is never read past
    /// `other_len` bytes. Result is an I8 0/1, the `ryo_str_eq` shape.
    fn emit_literal_eq(
        builder: &mut FunctionBuilder,
        other_ptr: Value,
        other_len: Value,
        n: usize,
        bytes: &[u8; INLINE_LITERAL_MAX],
    ) -> Value {
        let n_const = builder.ins().iconst(types::I64, n as i64);
        let len_ok = builder.ins().icmp(IntCC::Equal, other_len, n_const);
        if n == 0 {
            return len_ok;
        }

        let cmp_block = builder.create_block();
        let false_block = builder.create_block();
        let merge_block = builder.create_block();
        builder.append_block_param(merge_block, types::I8);
        builder.ins().brif(len_ok, cmp_block, &[], false_block, &[]);

        // Single predecessor each — seal on entry.
        builder.seal_block(cmp_block);
        builder.switch_to_block(cmp_block);
        let mut acc = builder.ins().iconst(types::I8, 1);
        for (i, &b) in bytes[..n].iter().enumerate() {
            let offset = i32::try_from(i).expect("inline literal byte offset fits i32");
            let byte = builder
                .ins()
                .load(types::I8, MemFlagsData::trusted(), other_ptr, offset);
            let expect = builder.ins().iconst(types::I8, i64::from(b));
            let eq = builder.ins().icmp(IntCC::Equal, byte, expect);
            acc = builder.ins().band(acc, eq);
        }
        builder.ins().jump(merge_block, &[BlockArg::Value(acc)]);

        builder.seal_block(false_block);
        builder.switch_to_block(false_block);
        let zero = builder.ins().iconst(types::I8, 0);
        builder.ins().jump(merge_block, &[BlockArg::Value(zero)]);

        // Two predecessors (both jumps above), all added.
        builder.seal_block(merge_block);
        builder.switch_to_block(merge_block);
        builder.block_params(merge_block)[0]
    }
}
