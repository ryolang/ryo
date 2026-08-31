//! Bytes codegen (M8.4.2) — split from `expr.rs` to keep both files
//! under the 2000-line CI cap (`scripts/check_file_length.sh`).
//! Everything here mirrors the `str` path: same 24-byte fat-pointer
//! ABI, same packed-u128 producer convention, `ryo_bytes_*` symbols.
//! Also hosts the shared `.rodata` dedup helpers (`store_string` /
//! `store_bytes`), displaced from `mod.rs` by the same cap.

use cranelift::codegen::ir::{FuncRef, types};
use cranelift::prelude::*;
use cranelift_module::{DataDescription, DataId, Module};
use ryo_core::tir::{Tir, TirRef, TirTag};
use ryo_core::types::{StringId, TypeKind};
use std::collections::HashMap;

use super::expr::CapRule;
use super::{Codegen, FunctionContext, ValueRepr};

impl<M: Module> Codegen<M> {
    /// Bytes-producing variant of `emit_rv_str_call`: appends the
    /// derived `cap` word so the triple lands entirely in SSA values.
    /// Does NOT touch `ctx.inst_values` — caching is the caller's job.
    pub(crate) fn emit_rv_bytes_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        fn_name: &str,
        args: &[(Type, Value)],
        cap_rule: CapRule,
    ) -> Result<ValueRepr, String> {
        let (ptr, len) = Self::emit_rv_pair_call(builder, ctx, fn_name, args)?;
        let cap = match cap_rule {
            CapRule::Static => builder.ins().iconst(types::I64, 0),
            CapRule::LenIsCap => len,
        };
        Ok(ValueRepr::Bytes { ptr, len, cap })
    }

    /// Emit a bytes literal as a fat pointer triple (ptr, len, cap=0)
    /// by calling `ryo_bytes_from_literal` at runtime. Mirrors
    /// `emit_str_literal_fat`; the payload is raw bytes (not
    /// necessarily UTF-8), so it reads through `pool.bytes_payload`.
    pub(crate) fn emit_bytes_literal_fat(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        id: StringId,
    ) -> Result<ValueRepr, String> {
        let content = ctx.pool.bytes_payload(id);
        let data_id = store_bytes(id, content, ctx.module, ctx.data_ctx, ctx.string_data)?;
        let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
        let rodata_ptr = builder.ins().symbol_value(ctx.int_type, data_ref);
        let lit_len = builder.ins().iconst(types::I64, content.len() as i64);

        Self::emit_rv_bytes_call(
            builder,
            ctx,
            "ryo_bytes_from_literal",
            &[(ctx.int_type, rodata_ptr), (types::I64, lit_len)],
            CapRule::Static,
        )
    }

    /// Declare `extern "C" fn ryo_bytes_free(ptr: *mut u8, cap: u64)` for
    /// the function being built — the bytes-family counterpart of
    /// `declare_str_free`. Returns a `FuncRef` callable via
    /// `builder.ins().call(_, &[ptr, cap])`. `cap == 0` is a runtime
    /// no-op (covers static `.rodata` payloads emitted by
    /// `ryo_bytes_from_literal`).
    pub(crate) fn declare_bytes_free(
        module: &mut M,
        builder: &mut FunctionBuilder,
        int_type: types::Type,
    ) -> Result<FuncRef, String> {
        Self::declare_runtime_fn(
            module,
            builder,
            "ryo_bytes_free",
            &[int_type, types::I64],
            &[],
        )
    }

    /// True when a scheduled Free target is a `bytes` owner (M8.4.2) —
    /// freed via `ryo_bytes_free` instead of `ryo_str_free`. Handles
    /// param sentinel refs (type comes from `tir.params`).
    pub(crate) fn free_target_is_bytes(ctx: &FunctionContext<'_, M>, target: TirRef) -> bool {
        let ty = if let Some(idx) = target.as_param_index() {
            ctx.tir.params[idx as usize].ty
        } else {
            ctx.tir.inst(target).ty
        };
        matches!(ctx.pool.kind(ty), TypeKind::Bytes)
    }
}

/// Define a string literal's content as a read-only `.rodata` object,
/// deduped per module through the `string_data` map (keyed on the
/// interned `StringId`, so duplicate literals share one blob). Moved
/// here from `mod.rs` to keep that file under the 2000-line CI cap.
pub(crate) fn store_string<M: Module>(
    content_id: StringId,
    content: &str,
    module: &mut M,
    data_ctx: &mut DataDescription,
    string_data: &mut HashMap<StringId, DataId>,
) -> Result<DataId, String> {
    if let Some(&data_id) = string_data.get(&content_id) {
        return Ok(data_id);
    }

    let data_id = module
        .declare_anonymous_data(false, false)
        .map_err(|e| format!("Failed to declare string data: {}", e))?;

    data_ctx.clear();
    data_ctx.define(content.as_bytes().into());

    module
        .define_data(data_id, data_ctx)
        .map_err(|e| format!("Failed to define string data: {}", e))?;

    string_data.insert(content_id, data_id);
    Ok(data_id)
}

/// `store_string` for raw bytes (M8.4.2 `b"..."` payloads, which need
/// not be valid UTF-8). Shares the `string_data` dedup map: identical
/// byte content shares one `.rodata` object regardless of literal kind.
fn store_bytes<M: Module>(
    content_id: StringId,
    content: &[u8],
    module: &mut M,
    data_ctx: &mut DataDescription,
    string_data: &mut HashMap<StringId, DataId>,
) -> Result<DataId, String> {
    if let Some(&data_id) = string_data.get(&content_id) {
        return Ok(data_id);
    }

    let data_id = module
        .declare_anonymous_data(false, false)
        .map_err(|e| format!("Failed to declare string data: {}", e))?;

    data_ctx.clear();
    data_ctx.define(content.into());

    module
        .define_data(data_id, data_ctx)
        .map_err(|e| format!("Failed to define string data: {}", e))?;

    string_data.insert(content_id, data_id);
    Ok(data_id)
}

/// Walk every TIR body and assert no `Unreachable` instruction is
/// reachable. Used inside `debug_assert!` at codegen entry points;
/// the driver short-circuits on `sink.has_errors()` long before we
/// get here, so any `Unreachable` past that gate is a sema bug.
/// (Not bytes-specific — displaced from `mod.rs` by the line cap.)
pub(crate) fn no_unreachable_in(tirs: &[Tir]) -> bool {
    for tir in tirs {
        // Slot 0 is the reserved sentinel and intentionally has
        // tag = Unreachable in the builder; it is *never* part of a
        // body. Skip it.
        for idx in 1..tir.instructions.len() {
            if matches!(tir.instructions[idx].tag, TirTag::Unreachable) {
                return false;
            }
        }
    }
    true
}
