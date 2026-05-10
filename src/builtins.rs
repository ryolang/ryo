use crate::types::{InternPool, TypeId};

pub struct BuiltinFunction {
    pub name: &'static str,
    /// Private tag used to look up the actual `TypeId` against a pool.
    return_ty: BuiltinReturn,
}

#[derive(Copy, Clone)]
enum BuiltinReturn {
    Void,
    Never,
}

impl BuiltinFunction {
    pub fn return_type(&self, pool: &InternPool) -> TypeId {
        match self.return_ty {
            BuiltinReturn::Void => pool.void(),
            BuiltinReturn::Never => pool.never(),
        }
    }
}

pub const BUILTINS: &[BuiltinFunction] = &[
    BuiltinFunction {
        name: "print",
        return_ty: BuiltinReturn::Void,
    },
    BuiltinFunction {
        name: "assert",
        return_ty: BuiltinReturn::Void,
    },
    BuiltinFunction {
        name: "panic",
        return_ty: BuiltinReturn::Never,
    },
];

pub fn lookup(name: &str) -> Option<&'static BuiltinFunction> {
    BUILTINS.iter().find(|b| b.name == name)
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
}
