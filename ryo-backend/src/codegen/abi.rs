//! One target-specific ABI description for declarations, calls, entry, and returns.

use super::{Codegen, cranelift_type_for, is_enum_type, is_fat_type, is_struct_type};
use cranelift::codegen::ir::{AbiParam, ArgumentPurpose, Signature, Type, types};
use cranelift::codegen::isa::CallConv;
use cranelift_module::Module;
use ryo_core::tir::{ParamMode, Tir};
use ryo_core::types::{InternPool, TypeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValueAbi {
    Void,
    Scalar(Type),
    Fat,
    View,
    Struct,
    Enum,
}

impl ValueAbi {
    fn classify(ty: TypeId, pool: &InternPool, word: Type) -> Self {
        if ty == pool.void() {
            Self::Void
        } else if is_fat_type(ty, pool) {
            Self::Fat
        } else if pool.is_view(ty) {
            Self::View
        } else if is_struct_type(ty, pool) {
            Self::Struct
        } else if is_enum_type(ty, pool) {
            Self::Enum
        } else {
            Self::Scalar(cranelift_type_for(ty, pool, word))
        }
    }

    pub(super) fn indirect_return(self) -> bool {
        matches!(self, Self::Fat | Self::Struct | Self::Enum)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ParamAbi {
    pub ty: TypeId,
    pub mode: ParamMode,
    pub value: ValueAbi,
}

/// Immutable within one target module and its interned layout context.
#[derive(Clone, Debug)]
pub(super) struct LoweredAbi {
    pub target: target_lexicon::Triple,
    pub pointer_type: Type,
    pub layout_context: ryo_core::artifact::ArtifactId,
    pub signature: Signature,
    pub params: Vec<ParamAbi>,
    pub result: ValueAbi,
    pub result_type: TypeId,
    pub hosted_main: bool,
}

impl<M: Module> Codegen<M> {
    pub(super) fn lower_abi(&self, tir: &Tir, pool: &InternPool, is_main: bool) -> LoweredAbi {
        let params: Vec<_> = tir
            .params
            .iter()
            .map(|param| ParamAbi {
                ty: param.ty,
                mode: param.mode,
                value: ValueAbi::classify(param.ty, pool, self.int_type),
            })
            .collect();
        let result = ValueAbi::classify(tir.return_type, pool, self.int_type);
        let mut signature = self.module.make_signature();
        if !is_main && result.indirect_return() {
            signature.params.push(AbiParam::special(
                self.int_type,
                ArgumentPurpose::StructReturn,
            ));
        }
        for param in &params {
            if param.mode == ParamMode::Inout {
                signature.params.push(AbiParam::new(self.int_type));
                continue;
            }
            match param.value {
                ValueAbi::Fat => {
                    signature.params.push(AbiParam::new(self.int_type));
                    signature.params.push(AbiParam::new(types::I64));
                    signature.params.push(AbiParam::new(types::I64));
                }
                ValueAbi::View => {
                    signature.params.push(AbiParam::new(self.int_type));
                    signature.params.push(AbiParam::new(types::I64));
                }
                ValueAbi::Struct | ValueAbi::Enum => {
                    signature.params.push(AbiParam::new(self.int_type))
                }
                ValueAbi::Scalar(ty) => signature.params.push(AbiParam::new(ty)),
                ValueAbi::Void => {}
            }
        }
        if is_main {
            signature.params.push(AbiParam::new(self.int_type));
            signature
                .params
                .push(AbiParam::new(self.module.isa().pointer_type()));
            signature.returns.push(AbiParam::new(self.int_type));
        } else if let ValueAbi::Scalar(ty) = result {
            signature.returns.push(AbiParam::new(ty));
        }
        if !is_main && self.tail_candidates.contains(&tir.name) {
            signature.call_conv = CallConv::Tail;
        }
        LoweredAbi {
            target: self.module.isa().triple().clone(),
            pointer_type: self.module.isa().pointer_type(),
            layout_context: pool.identity(),
            signature,
            params,
            result,
            result_type: tir.return_type,
            hosted_main: is_main,
        }
    }
}

impl LoweredAbi {
    pub(super) fn require_call(
        &self,
        tir: &Tir,
        call: ryo_core::tir::TirRef,
    ) -> Result<(), String> {
        let view = tir.call_view(call);
        if view.args.len() != self.params.len()
            || view.modes.len() != self.params.len()
            || tir.inst(call).ty != self.result_type
        {
            return Err("call shape disagrees with the lowered callee ABI".into());
        }
        for ((arg, mode), param) in view.args.iter().zip(&view.modes).zip(&self.params) {
            if tir.inst(arg).ty != param.ty || mode != param.mode {
                return Err("call parameter disagrees with the lowered callee ABI".into());
            }
        }
        Ok(())
    }
}
