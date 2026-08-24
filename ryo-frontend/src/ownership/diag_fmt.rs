//! Diagnostic name formatting — split from `mod.rs`; see module docs there.

use super::{Owner, Ownership, inout_owner};
use ryo_core::tir::{Tir, TirData, TirRef};
use ryo_core::types::{InternPool, StringId};

/// Render a binding name for inclusion in a diagnostic message.
/// Returns `` `name` `` for known bindings and `value` for anonymous
/// temporaries (concat results, fresh allocations, etc.).
pub(crate) fn format_binding(name: Option<StringId>, pool: &InternPool) -> String {
    match name {
        Some(n) => format!("`{}`", pool.str(n)),
        None => "value".to_string(),
    }
}

/// If `r` is a direct `Var` read, return the binding name it aliases.
/// Used at consume sites to thread the source binding name into
/// E0020/E0021/E0022 messages. Returns `None` for fresh producers
/// (StrConst, StrConcat, Call), where there's no source binding.
pub(crate) fn consumed_binding_name(tir: &Tir, r: TirRef) -> Option<StringId> {
    match tir.inst(r).data {
        TirData::Var(n) => Some(n),
        _ => None,
    }
}

pub(crate) fn owner_name_for_diag(owner: Owner, tir: &Tir, pool: &InternPool) -> String {
    match owner {
        Owner::Param(name) => format!("`{}`", pool.str(name)),
        Owner::Inst(r) => format_binding(consumed_binding_name(tir, r), pool),
    }
}

/// Rule 7 (E0032) binding name: scan the call's args for a `Var` read
/// that resolves to `owner` and use ITS name. `owner_name_for_diag`
/// inspects the binding's initializer (an IntConst/StrConst — never a
/// `Var`), so it falls back to "value" for locals; the conflicting arg
/// reads always carry the name.
pub(crate) fn rule7_owner_name(
    own: &Ownership,
    tir: &Tir,
    pool: &InternPool,
    args: &[TirRef],
    owner: Owner,
) -> String {
    for arg in args {
        if let TirData::Var(name) = tir.inst(*arg).data
            && inout_owner(own, tir, *arg) == owner
        {
            return format!("`{}`", pool.str(name));
        }
    }
    owner_name_for_diag(owner, tir, pool)
}
