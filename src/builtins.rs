use crate::types::{InternPool, TypeId};

pub struct BuiltinFunction {
    pub name: &'static str,
    /// Private tag used to look up the actual `TypeId` against a pool.
    return_ty: BuiltinReturn,
}

#[derive(Copy, Clone)]
enum BuiltinReturn {
    Void,
}

impl BuiltinFunction {
    pub fn return_type(&self, pool: &InternPool) -> TypeId {
        match self.return_ty {
            BuiltinReturn::Void => pool.void(),
        }
    }
}

pub const BUILTINS: &[BuiltinFunction] = &[BuiltinFunction {
    name: "print",
    return_ty: BuiltinReturn::Void,
}];

pub fn lookup(name: &str) -> Option<&'static BuiltinFunction> {
    BUILTINS.iter().find(|b| b.name == name)
}
