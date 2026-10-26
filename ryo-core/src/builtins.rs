use crate::types::{InternPool, StringId, TypeId};

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
    /// borrow alongside the outer call's `inout` args.
    pub view_borrow_params: &'static [usize],
    /// Statically-known upper bound on the produced string's byte
    /// length, when one exists. A bound at or below the SSO inline
    /// capacity (23) makes the result provably always-inline: codegen
    /// elides the guaranteed-no-op `ryo_str_free` for it. `int_to_str`
    /// is 20 (i64::MIN is `-9223372036854775808`); `bool_to_str` is 5.
    /// `float_to_str` is deliberately `None`: ryu's f64 worst case is
    /// 24 bytes (`-2.2250738585072014e-308`), one over the capacity.
    pub max_output_len: Option<u8>,
}

#[derive(Copy, Clone)]
enum BuiltinReturn {
    Void,
    Never,
    Int,
    Str,
    Bytes,
}

impl BuiltinFunction {
    pub fn return_type(&self, pool: &InternPool) -> TypeId {
        match self.return_ty {
            BuiltinReturn::Void => pool.void(),
            BuiltinReturn::Never => pool.never(),
            BuiltinReturn::Int => pool.int(),
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
        max_output_len: None,
    },
    BuiltinFunction {
        name: "assert",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        name: "panic",
        return_ty: BuiltinReturn::Never,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        // TODO(M24): interim call form — replaced by `process.exit`.
        name: "process_exit",
        return_ty: BuiltinReturn::Never,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        // TODO(M24): interim call form — replaced by `io.eprint`.
        name: "io_eprint",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        // TODO(M22): interim call form — replaced by `process.args`.
        name: "process_argc",
        return_ty: BuiltinReturn::Int,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        // TODO(M22): interim call form — replaced by `process.args`.
        name: "process_argv",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        // TODO(M16): interim call form — replaced by `process.env`.
        name: "process_env",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        // TODO(M13.6): interim call form — replaced by
        // `io.read_line() -> IoError!str`.
        name: "io_read_line",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        name: "int_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: Some(20),
    },
    BuiltinFunction {
        name: "float_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        name: "bool_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: Some(5),
    },
    BuiltinFunction {
        name: "str_push",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        name: "bytes_push",
        return_ty: BuiltinReturn::Void,
        borrowed_scalar_params: &[],
        view_borrow_params: &[],
        max_output_len: None,
    },
];

/// Synthesized (non-user-facing) runtime callees with ABI metadata the
/// ownership pass and codegen need. These are NOT user-callable
/// builtins — they are absent from `BUILTINS` and from sema's
/// `emit_builtin_call` dispatch — but the calls sema synthesizes to
/// them still need their ABI recorded here.
const ABI_CALLEES: &[BuiltinFunction] = &[
    BuiltinFunction {
        name: "__ryo_panic",
        return_ty: BuiltinReturn::Never,
        borrowed_scalar_params: &[0],
        view_borrow_params: &[],
        max_output_len: None,
    },
    BuiltinFunction {
        name: "__ryo_str_from_view",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
        max_output_len: None,
    },
    BuiltinFunction {
        name: "__ryo_bytes_repr",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
        max_output_len: None,
    },
    BuiltinFunction {
        name: "__ryo_bytes_from_view",
        return_ty: BuiltinReturn::Bytes,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
        max_output_len: None,
    },
    BuiltinFunction {
        name: "__ryo_bytes_to_str",
        return_ty: BuiltinReturn::Str,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
        max_output_len: None,
    },
    BuiltinFunction {
        name: "__ryo_str_to_bytes",
        return_ty: BuiltinReturn::Bytes,
        borrowed_scalar_params: &[],
        view_borrow_params: &[0],
        max_output_len: None,
    },
];

pub fn lookup(name: &str) -> Option<&'static BuiltinFunction> {
    BUILTINS.iter().find(|b| b.name == name)
}

fn abi_callee(name: &str) -> Option<&'static BuiltinFunction> {
    ABI_CALLEES.iter().find(|b| b.name == name)
}

/// True if callee `name` passes parameter `idx` via the borrowed-scalar
/// ABI (raw `.rodata` pointer, cap=0, never heap-owned). Consults both the
/// user-facing `BUILTINS` table and the synthesized `ABI_CALLEES` registry.
/// Returns false for unknown names and out-of-range indices.
pub fn is_borrowed_scalar_param(name_id: StringId, pool: &InternPool, idx: usize) -> bool {
    let name = pool.str(name_id);
    lookup(name)
        .or_else(|| abi_callee(name))
        .map(|b| b.borrowed_scalar_params.contains(&idx))
        .unwrap_or(false)
}

/// Parameter indices of callee `name_id` passed as `strview` whose root
/// owner the call borrows for its duration (E4). Empty for unknown names.
pub fn view_borrow_params(name_id: StringId, pool: &InternPool) -> &'static [usize] {
    let name = pool.str(name_id);
    lookup(name)
        .or_else(|| abi_callee(name))
        .map(|b| b.view_borrow_params)
        .unwrap_or(&[])
}

/// Statically-known upper bound on the byte length of the string a
/// builtin produces, or `None` when unbounded / unknown. Consults both
/// user-facing builtins and synthesized runtime callees.
pub fn max_output_len(name: &str) -> Option<u8> {
    lookup(name)
        .or_else(|| abi_callee(name))
        .and_then(|b| b.max_output_len)
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
    fn bounded_producers_annotated() {
        assert_eq!(max_output_len("int_to_str"), Some(20));
        assert_eq!(max_output_len("bool_to_str"), Some(5));
        assert_eq!(max_output_len("float_to_str"), None);
        assert_eq!(max_output_len("print"), None);
        assert_eq!(max_output_len("not_a_builtin"), None);
    }

    #[test]
    fn ryo_panic_uses_borrowed_scalar_abi_for_param_0_only() {
        let mut pool = InternPool::new();
        let panic_name = pool.intern_str("__ryo_panic");
        assert!(is_borrowed_scalar_param(panic_name, &pool, 0));
        assert!(!is_borrowed_scalar_param(panic_name, &pool, 1));

        let print_name = pool.intern_str("print");
        assert!(!is_borrowed_scalar_param(print_name, &pool, 0));

        let unknown = pool.intern_str("not_a_builtin");
        assert!(!is_borrowed_scalar_param(unknown, &pool, 0));
    }

    #[test]
    fn str_from_view_registered_as_str_returning_view_borrower() {
        let mut pool = InternPool::new();
        let name = pool.intern_str("__ryo_str_from_view");
        assert!(lookup("__ryo_str_from_view").is_none());
        let entry = abi_callee("__ryo_str_from_view").expect("ABI entry");
        assert_eq!(entry.return_type(&pool), pool.str_());
        assert_eq!(view_borrow_params(name, &pool), &[0]);

        let print = pool.intern_str("print");
        assert!(view_borrow_params(print, &pool).is_empty());
        let unknown = pool.intern_str("not_a_builtin");
        assert!(view_borrow_params(unknown, &pool).is_empty());
    }

    #[test]
    fn bytes_builtins_and_callees_registered() {
        let mut pool = InternPool::new();
        assert_eq!(
            lookup("bytes_push").unwrap().return_type(&pool),
            pool.void()
        );
        let repr = abi_callee("__ryo_bytes_repr").expect("ABI entry");
        assert_eq!(repr.return_type(&pool), pool.str_());
        let from_view = abi_callee("__ryo_bytes_from_view").expect("ABI entry");
        assert_eq!(from_view.return_type(&pool), pool.bytes());
        let to_str = abi_callee("__ryo_bytes_to_str").expect("ABI entry");
        assert_eq!(to_str.return_type(&pool), pool.str_());
        let to_bytes = abi_callee("__ryo_str_to_bytes").expect("ABI entry");
        assert_eq!(to_bytes.return_type(&pool), pool.bytes());
        for name in [
            "__ryo_bytes_repr",
            "__ryo_bytes_from_view",
            "__ryo_bytes_to_str",
            "__ryo_str_to_bytes",
        ] {
            let id = pool.intern_str(name);
            assert_eq!(view_borrow_params(id, &pool), &[0], "{name}");
        }
    }
}
