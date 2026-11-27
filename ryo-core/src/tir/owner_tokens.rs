use super::{Tir, TirRef, TirTag};
use crate::tir::BindingId;
use crate::types::{InternPool, TypeId};

#[derive(Clone, Copy, Debug)]
pub(crate) struct OwnerToken {
    pub(crate) owner: TirRef,
    pub(crate) field_index: u32,
    pub(crate) binding: BindingId,
    pub(crate) ty: TypeId,
}

fn validate_tokens(
    tir: &Tir,
    instruction: TirRef,
    kind: &str,
    expected: Vec<OwnerToken>,
) -> Result<Vec<OwnerToken>, String> {
    if expected.len() > instruction.index() {
        return Err(format!(
            "{kind} has {} owner tokens but only {} preceding instructions",
            expected.len(),
            instruction.index()
        ));
    }
    let start = instruction.index() - expected.len();
    let mut tokens = Vec::with_capacity(expected.len());
    for (cursor, mut token) in (start..).zip(expected) {
        if cursor == 0 {
            return Err(format!("{kind} owner token is missing"));
        }
        let owner = TirRef::from_index(cursor);
        let inst = tir.inst(owner);
        if inst.tag != TirTag::FieldAccess {
            return Err(format!(
                "{kind} owner token {owner:?} is not a field access"
            ));
        }
        if inst.ty != token.ty {
            return Err(format!(
                "{kind} owner token type mismatch: expected {:?}, found {:?}",
                token.ty, inst.ty
            ));
        }
        let crate::tir::TirData::FieldAccess { field_index, .. } = inst.data else {
            unreachable!("field access tag checked above")
        };
        if field_index != token.field_index {
            return Err(format!(
                "{kind} owner token field index {field_index} does not match binding field index {}",
                token.field_index
            ));
        }
        token.owner = owner;
        tokens.push(token);
    }
    Ok(tokens)
}

impl Tir {
    pub(crate) fn match_owner_tokens(
        &self,
        r: TirRef,
        pool: &InternPool,
    ) -> Result<Vec<OwnerToken>, String> {
        let expected = self
            .match_view(r)
            .arms()
            .flat_map(|arm| arm.bindings())
            .filter(|binding| !pool.is_copy(binding.ty))
            .map(|binding| OwnerToken {
                owner: r,
                field_index: binding.field_index,
                binding: binding.id,
                ty: binding.ty,
            })
            .collect();
        validate_tokens(self, r, "match", expected)
    }

    pub(crate) fn destructure_owner_tokens(&self, r: TirRef) -> Result<Vec<OwnerToken>, String> {
        let expected = self
            .destructure_view(r)
            .fields
            .iter()
            .filter_map(|field| {
                Some(OwnerToken {
                    owner: r,
                    field_index: field.field_index,
                    binding: field.bind?,
                    ty: field.ty,
                })
            })
            .collect();
        validate_tokens(self, r, "destructure", expected)
    }
}
