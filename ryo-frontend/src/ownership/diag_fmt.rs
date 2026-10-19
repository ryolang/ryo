//! Diagnostic name formatting — split from `mod.rs`; see module docs there.

use super::{Owner, Ownership, struct_base_name};
use ryo_core::flow::Flow;
use ryo_core::tir::{ExtraList, Tir, TirData, TirRef};
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

/// The diagnostic label for an owner: the owning place's source name,
/// or `value` for anonymous storage (temps, match results).
pub(crate) fn owner_name_for_diag(owner: Owner, flow: &Flow, pool: &InternPool) -> String {
    let name = match flow.place(owner.0) {
        ryo_core::flow::Place::Param { name, .. }
        | ryo_core::flow::Place::Local { name, .. }
        | ryo_core::flow::Place::LoopVar { name, .. }
        | ryo_core::flow::Place::Token { name, .. } => Some(*name),
        ryo_core::flow::Place::Temp { .. }
        | ryo_core::flow::Place::View { .. }
        | ryo_core::flow::Place::MatchResult { .. } => None,
    };
    format_binding(name, pool)
}

/// Rule 7 (E0032) binding name: scan the call's args for a `Var` read
/// (or a `FieldAccess` chain's root binding, M9) that resolves to
/// `owner` and use ITS name. `owner_name_for_diag`
/// inspects the binding's initializer (an IntConst/StrConst — never a
/// `Var`), so it falls back to "value" for locals; the conflicting arg
/// reads always carry the name.
pub(crate) fn rule7_owner_name(
    own: &Ownership,
    tir: &Tir,
    flow: &Flow,
    pool: &InternPool,
    args: &ExtraList<'_, TirRef>,
    owner: Owner,
) -> String {
    for arg in args {
        let name = match tir.inst(arg).data {
            TirData::Var(name) => Some(name),
            TirData::FieldAccess { .. } => struct_base_name(tir, arg),
            _ => None,
        };
        if let Some(name) = name
            && own.underlying_owner(arg) == owner
        {
            return format!("`{}`", pool.str(name));
        }
    }
    owner_name_for_diag(owner, flow, pool)
}
