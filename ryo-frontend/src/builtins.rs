use ryo_core::types::{InternPool, StringId, TypeId};

pub struct BuiltinFunction {
    pub name: &'static str,
    /// Private tag used to look up the actual `TypeId` against a pool.
    return_ty: BuiltinReturn,
    /// Parameter indices this builtin/callee passes via the borrowed-scalar
    /// ABI (raw `.rodata` pointer, cap=0, never heap-owned). Read by the
    /// ownership pass and codegen instead of name-string matching. Today
    /// only `__ryo_panic`'s message (param 0) is borrowed-scalar; every
    /// user-facing builtin in `BUILTINS` is `&[]`.
    pub borrowed_scalar_params: &'static [usize],
    /// Parameter indices passed as `strview` whose ROOT owner the call
    /// borrows for its duration (E4). Read by the ownership pass's Rule-7
    /// partition: when a call to this callee appears as a borrow-mode
    /// argument of an outer call, the view's root counts as an immutable
    /// borrow alongside the outer call's `inout` args. `__ryo_str_from_view`
    /// (param 0) borrows a view (M8.4.1.2); the M8.4.2 bytes callees
    /// (`__ryo_bytes_repr`, `__ryo_bytes_from_view`, `__ryo_bytes_to_str`,
    /// `ryo_str_to_bytes`) borrow their view argument's root owner the
    /// same way.
    pub view_borrow_params: &'static [usize],
}

#[derive(Copy, Clone)]
enum BuiltinReturn {
    Void,
    Never,
    Str,
    Bytes,
}

impl BuiltinFunction {
    pub fn return_type(&self, pool: &InternPool) -> TypeId {
        match self.return_ty {
            BuiltinReturn::Void => pool.void(),
            BuiltinReturn::Never => pool.never(),
            BuiltinReturn::Str => pool.str_(),
            BuiltinReturn::Bytes => pool.bytes(),
        }
    }
}

pub const BUILTINS: &[BuiltinFunction] = &[
    BuiltinFunction {
        name: "print",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
    },
    BuiltinFunction {
        name: "assert",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
    },
    BuiltinFunction {
        name: "panic",
        return_ty: BuiltinReturn::Never,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
    },
    BuiltinFunction {
        name: "int_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
    },
    BuiltinFunction {
        name: "float_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
    },
    BuiltinFunction {
        name: "bool_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
    },
    BuiltinFunction {
        name: "str_push",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
    },
    BuiltinFunction {
        name: "bytes_push",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
    },
];

/// Synthesized (non-user-facing) runtime callees with ABI metadata the
/// ownership pass and codegen need. These are NOT user-callable
/// builtins — they are absent from `BUILTINS` and from sema's
/// `emit_builtin_call` dispatch — but the calls sema synthesizes to
/// them still need their ABI recorded here:
/// `__ryo_panic` is emitted by `sema::build_panic_call`; its message
/// (param 0) is passed as a raw `.rodata` pointer with cap=0 and never
/// heap-owned. Kept as `BuiltinFunction`s so `borrowed_scalar_params` is
/// the single source of truth for the ABI.
/// `__ryo_str_from_view` is emitted by `sema::emit_str_materialize` for
/// the `str(view)` call form (M8.4.1.2); it returns an owned `str` and
/// borrows its `strview` argument's root owner for the call's duration
/// (E4, Rule-7 partition).
const ABI_CALLEES: &[BuiltinFunction] = &[
    BuiltinFunction {
        name: "__ryo_panic",
        return_ty: BuiltinReturn::Never,
        borrowed_scalar_params: &[0],
        view_borrow_params: &[],
    },
    BuiltinFunction {
        name: "__ryo_str_from_view",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
    },
    BuiltinFunction {
        name: "__ryo_bytes_repr",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
    },
    BuiltinFunction {
        name: "__ryo_bytes_from_view",
        return_ty: BuiltinReturn::Bytes,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
    },
    BuiltinFunction {
        name: "__ryo_bytes_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
    },
    BuiltinFunction {
        name: "ryo_str_to_bytes",
        return_ty: BuiltinReturn::Bytes,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
    },
];

pub fn lookup(name: &str) -> Option<&'static BuiltinFunction> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Look up a synthesized borrowed-scalar-ABI callee (e.g. `__ryo_panic`)
/// that is not a user-facing builtin.
fn abi_callee(name: &str) -> Option<&'static BuiltinFunction> {
    ABI_CALLEES.iter().find(|b| b.name == name)
}

/// True if callee `name` passes parameter `idx` via the borrowed-scalar
/// ABI (raw `.rodata` pointer, cap=0, never heap-owned). Consults both the
/// user-facing `BUILTINS` table and the synthesized `ABI_CALLEES` registry
/// (e.g. `__ryo_panic`). Returns false for unknown names and out-of-range
/// indices. Replaces the old `pool.str(name) == "__ryo_panic"` name-match
/// in the ownership pass.
pub fn is_borrowed_scalar_param(name_id: StringId, pool: &InternPool, idx: usize) -> bool {
    let name = pool.str(name_id);
    lookup(name)
        .or_else(|| abi_callee(name))
        .map(|b| b.borrowed_scalar_params.contains(&idx))
        .unwrap_or(false)
}

/// Parameter indices of callee `name_id` passed as `strview` whose root
/// owner the call borrows for its duration (E4). Consults both the
/// user-facing `BUILTINS` table and the synthesized `ABI_CALLEES`
/// registry (e.g. `__ryo_str_from_view`). Read by the ownership pass's
/// Rule-7 partition to look through materialization calls; empty for
/// unknown names. See M8.4.1.2.
pub fn view_borrow_params(name_id: StringId, pool: &InternPool) -> &'static [usize] {
    let name = pool.str(name_id);
    lookup(name)
        .or_else(|| abi_callee(name))
        .map(|b| b.view_borrow_params)
        .unwrap_or(&[])
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
        // `__ryo_panic`'s message (param 0) is the only
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

    #[test]
    fn str_from_view_registered_as_str_returning_view_borrower() {
        // M8.4.1.2: the ABI registry records that the synthesized
        // `__ryo_str_from_view` callee returns an owned `str` and
        // borrows its `strview` argument (param 0) for the call's
        // duration (Rule-7 partition). It is NOT a user-facing builtin.
        let mut pool = InternPool::new();
        let name = pool.intern_str("__ryo_str_from_view");
        assert!(lookup("__ryo_str_from_view").is_none());
        let entry = abi_callee("__ryo_str_from_view").expect("ABI entry");
        assert_eq!(entry.return_type(&pool), pool.str_());
        assert_eq!(view_borrow_params(name, &pool), &[0]);

        // User-facing builtins and unknown callees borrow no views.
        let print = pool.intern_str("print");
        assert!(view_borrow_params(print, &pool).is_empty());
        let unknown = pool.intern_str("not_a_builtin");
        assert!(view_borrow_params(unknown, &pool).is_empty());
    }

    #[test]
    fn bytes_builtins_and_callees_registered() {
        let mut pool = InternPool::new();
        // User-facing.
        assert_eq!(
            lookup("bytes_push").unwrap().return_type(&pool),
            pool.void()
        );
        // Synthesized callees.
        let repr = abi_callee("__ryo_bytes_repr").expect("ABI entry");
        assert_eq!(repr.return_type(&pool), pool.str_());
        let from_view = abi_callee("__ryo_bytes_from_view").expect("ABI entry");
        assert_eq!(from_view.return_type(&pool), pool.bytes());
        let to_str = abi_callee("__ryo_bytes_to_str").expect("ABI entry");
        assert_eq!(to_str.return_type(&pool), pool.str_());
        let to_bytes = abi_callee("ryo_str_to_bytes").expect("ABI entry");
        assert_eq!(to_bytes.return_type(&pool), pool.bytes());
        // All four borrow their view argument's root owner (E4). The names
        // are not interned until sema synthesizes them, so intern here.
        for name in [
            "__ryo_bytes_repr",
            "__ryo_bytes_from_view",
            "__ryo_bytes_to_str",
            "ryo_str_to_bytes",
        ] {
            let id = pool.intern_str(name);
            assert_eq!(view_borrow_params(id, &pool), &[0], "{name}");
        }
    }
}
