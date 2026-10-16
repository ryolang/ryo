//! Shared read-only storage for string and byte literals.

use cranelift_module::{DataDescription, DataId, Module};
use ryo_core::types::StringId;
use std::collections::HashMap;

/// Define a string literal's content as a read-only `.rodata` object,
/// deduped per module through the `string_data` map (keyed on the
/// interned `StringId`, so duplicate literals share one blob).
pub(crate) fn store_string<M: Module>(
    content_id: StringId,
    content: &str,
    module: &mut M,
    data_ctx: &mut DataDescription,
    string_data: &mut HashMap<StringId, DataId>,
) -> Result<DataId, String> {
    store_bytes(
        content_id,
        content.as_bytes(),
        module,
        data_ctx,
        string_data,
    )
}

/// `store_string` for raw bytes (M8.4.2 `b"..."` payloads, which need
/// not be valid UTF-8). Shares the `string_data` dedup map: identical
/// byte content shares one `.rodata` object regardless of literal kind.
pub(super) fn store_bytes<M: Module>(
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
