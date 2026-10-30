use ryo_core::types::{InternPool, StringId};

pub use ryo_core::builtins::{
    BUILTINS, BuiltinFunction, is_borrowed_scalar_param, lookup, max_output_len, view_borrow_params,
};

/// Pre-interned `StringId`s for every name sema compares on a hot path:
/// each `BUILTINS` dispatch name, the `str`/`bytes` materialize
/// intercepts (not `BUILTINS` entries), the reserved `range` name, and
/// the receiver-method names dispatched per `MethodCall`.
/// Built once per `Sema::run`; downstream dispatch is a `StringId`
/// equality check instead of `pool.str(id) == "..."`.
#[derive(Clone, Copy)]
pub struct BuiltinNameIds {
    pub(crate) str_: StringId,
    pub(crate) bytes: StringId,
    pub(crate) range: StringId,
    pub(crate) print: StringId,
    pub(crate) assert: StringId,
    pub(crate) panic: StringId,
    pub(crate) process_exit: StringId,
    pub(crate) io_eprint: StringId,
    pub(crate) process_argc: StringId,
    pub(crate) process_argv: StringId,
    pub(crate) process_env: StringId,
    pub(crate) io_read_line: StringId,
    pub(crate) int_to_str: StringId,
    pub(crate) float_to_str: StringId,
    pub(crate) bool_to_str: StringId,
    pub(crate) str_push: StringId,
    pub(crate) bytes_push: StringId,
    pub(crate) len: StringId,
    pub(crate) is_empty: StringId,
    pub(crate) to_str: StringId,
    pub(crate) to_bytes: StringId,
    pub(crate) as_bytes: StringId,
    pub(crate) message: StringId,
}

impl BuiltinNameIds {
    pub(crate) fn intern(pool: &mut InternPool) -> Self {
        BuiltinNameIds {
            str_: pool.intern_str("str"),
            bytes: pool.intern_str("bytes"),
            range: pool.intern_str("range"),
            print: pool.intern_str("print"),
            assert: pool.intern_str("assert"),
            panic: pool.intern_str("panic"),
            process_exit: pool.intern_str("process_exit"),
            io_eprint: pool.intern_str("io_eprint"),
            process_argc: pool.intern_str("process_argc"),
            process_argv: pool.intern_str("process_argv"),
            process_env: pool.intern_str("process_env"),
            io_read_line: pool.intern_str("io_read_line"),
            int_to_str: pool.intern_str("int_to_str"),
            float_to_str: pool.intern_str("float_to_str"),
            bool_to_str: pool.intern_str("bool_to_str"),
            str_push: pool.intern_str("str_push"),
            bytes_push: pool.intern_str("bytes_push"),
            len: pool.intern_str("len"),
            is_empty: pool.intern_str("is_empty"),
            to_str: pool.intern_str("to_str"),
            to_bytes: pool.intern_str("to_bytes"),
            as_bytes: pool.intern_str("as_bytes"),
            message: pool.intern_str("message"),
        }
    }
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
    fn range_is_reserved() {
        assert!(is_reserved_name("range"));
    }

    #[test]
    fn non_reserved_name() {
        assert!(!is_reserved_name("foo"));
    }
}
