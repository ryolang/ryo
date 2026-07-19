use ryo_core::types::{InternPool, StringId, TypeId};

pub struct BuiltinFunction {
    pub name: &'static str,
    /// Private tag used to look up the actual `TypeId` against a pool.
    return_ty: BuiltinReturn,
    /// Parameter indices this builtin/callee passes via the borrowed-scalar
    /// ABI (raw `.rodata` pointer, cap=0, never heap-owned). Read by the
    /// ownership pass and codegen instead of name-string matching. Today
    /// only `__ryo_panic`'s message (param 0) is borrowed-scalar; every
    /// user-facing builtin in `BUILTINS` is `&[]`. See I-059.
    pub borrowed_scalar_params: &'static [usize],
}

#[derive(Copy, Clone)]
enum BuiltinReturn {
    Void,
    Never,
    Str,
}

impl BuiltinFunction {
    pub fn return_type(&self, pool: &InternPool) -> TypeId {
        match self.return_ty {
            BuiltinReturn::Void => pool.void(),
            BuiltinReturn::Never => pool.never(),
            BuiltinReturn::Str => pool.str_(),
        }
    }
}

pub const BUILTINS: &[BuiltinFunction] = &[
    BuiltinFunction {
        name: "print",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
    },
    BuiltinFunction {
        name: "assert",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
    },
    BuiltinFunction {
        name: "panic",
        return_ty: BuiltinReturn::Never,
        borrowed_scalar_params: &[],
    },
    BuiltinFunction {
        name: "int_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
    },
    BuiltinFunction {
        name: "float_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
    },
    BuiltinFunction {
        name: "bool_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
    },
    BuiltinFunction {
        name: "str_push",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
    },
];

/// Synthesized (non-user-facing) runtime callees that use the
/// borrowed-scalar ABI. These are NOT user-callable builtins — they are
/// absent from `BUILTINS` and from sema's `emit_builtin_call` dispatch —
/// but the ownership pass and codegen still need their ABI metadata.
/// `__ryo_panic` is emitted by `sema::build_panic_call`; its message
/// (param 0) is passed as a raw `.rodata` pointer with cap=0 and never
/// heap-owned. Kept as `BuiltinFunction`s so `borrowed_scalar_params` is
/// the single source of truth for the ABI. See I-059.
const ABI_CALLEES: &[BuiltinFunction] = &[BuiltinFunction {
    name: "__ryo_panic",
    return_ty: BuiltinReturn::Never,
    borrowed_scalar_params: &[0],
}];

pub fn lookup(name: &str) -> Option<&'static BuiltinFunction> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Look up a synthesized borrowed-scalar-ABI callee (e.g. `__ryo_panic`)
/// that is not a user-facing builtin. See I-059.
fn abi_callee(name: &str) -> Option<&'static BuiltinFunction> {
    ABI_CALLEES.iter().find(|b| b.name == name)
}

/// True if callee `name` passes parameter `idx` via the borrowed-scalar
/// ABI (raw `.rodata` pointer, cap=0, never heap-owned). Consults both the
/// user-facing `BUILTINS` table and the synthesized `ABI_CALLEES` registry
/// (e.g. `__ryo_panic`). Returns false for unknown names and out-of-range
/// indices. Replaces the old `pool.str(name) == "__ryo_panic"` name-match
/// in the ownership pass. See I-059.
pub fn is_borrowed_scalar_param(name_id: StringId, pool: &InternPool, idx: usize) -> bool {
    let name = pool.str(name_id);
    lookup(name)
        .or_else(|| abi_callee(name))
        .map(|b| b.borrowed_scalar_params.contains(&idx))
        .unwrap_or(false)
}

/// Names that are not callable builtins but cannot be redefined by user code.
pub const RESERVED_NAMES: &[&str] = &["range"];

pub fn is_reserved_name(name: &str) -> bool {
    RESERVED_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_print_exists() {
        assert!(lookup("print").is_some());
    }

    #[test]
    fn lookup_assert_exists_and_returns_void() {
        let pool = InternPool::new();
        let b = lookup("assert").unwrap();
        assert_eq!(b.return_type(&pool), pool.void());
    }

    #[test]
    fn lookup_panic_exists_and_returns_never() {
        let pool = InternPool::new();
        let b = lookup("panic").unwrap();
        assert_eq!(b.return_type(&pool), pool.never());
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("nonexistent").is_none());
    }

    #[test]
    fn range_is_reserved() {
        assert!(is_reserved_name("range"));
    }

    #[test]
    fn non_reserved_name() {
        assert!(!is_reserved_name("foo"));
    }

    #[test]
    fn ryo_panic_uses_borrowed_scalar_abi_for_param_0_only() {
        // I-059: `__ryo_panic`'s message (param 0) is the only
        // borrowed-scalar ABI parameter; param 1 (length) is not, and
        // the registry must reject out-of-range / unknown callees.
        let mut pool = InternPool::new();
        let panic_name = pool.intern_str("__ryo_panic");
        assert!(is_borrowed_scalar_param(panic_name, &pool, 0));
        assert!(!is_borrowed_scalar_param(panic_name, &pool, 1));

        // User-facing builtins never use the borrowed-scalar ABI today.
        let print_name = pool.intern_str("print");
        assert!(!is_borrowed_scalar_param(print_name, &pool, 0));

        // Unknown callees are never borrowed-scalar.
        let unknown = pool.intern_str("not_a_builtin");
        assert!(!is_borrowed_scalar_param(unknown, &pool, 0));
    }
}
