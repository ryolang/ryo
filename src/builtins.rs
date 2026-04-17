use crate::hir::Type;

pub struct BuiltinFunction {
    pub name: &'static str,
    pub return_type: Type,
}

pub const BUILTINS: &[BuiltinFunction] = &[BuiltinFunction {
    name: "print",
    return_type: Type::Int,
}];

pub fn lookup(name: &str) -> Option<&'static BuiltinFunction> {
    BUILTINS.iter().find(|b| b.name == name)
}
