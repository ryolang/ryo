use crate::hir::TypeId;
use crate::types::InternPool;

pub struct BuiltinFunction {
    pub name: &'static str,
    /// Private tag used to look up the actual `TypeId` against a pool.
    return_ty: BuiltinReturn,
}

#[derive(Copy, Clone)]
enum BuiltinReturn {
    Int,
}

impl BuiltinFunction {
    pub fn return_type(&self, pool: &InternPool) -> TypeId {
        match self.return_ty {
            BuiltinReturn::Int => pool.int(),
        }
    }
}

pub const BUILTINS: &[BuiltinFunction] = &[BuiltinFunction {
    name: "print",
    return_ty: BuiltinReturn::Int,
}];

pub fn lookup(name: &str) -> Option<&'static BuiltinFunction> {
    BUILTINS.iter().find(|b| b.name == name)
}
