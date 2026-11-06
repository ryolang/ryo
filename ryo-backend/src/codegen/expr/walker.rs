use super::{Codegen, FunctionContext, ValueRepr};
use cranelift::codegen::ir::StackSlot;
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::spine::ChainSpine;
use ryo_core::tir::{TirData, TirRef, TirTag};
use ryo_core::types::TypeKind;

#[derive(Clone, Copy)]
pub(crate) struct CodegenExprContext {
    pub(crate) depth: u32,
    pub(crate) limit: u32,
}

impl CodegenExprContext {
    pub(crate) fn root() -> Self {
        #[cfg(test)]
        if let Some(limit) = TEST_EXPR_LIMIT.with(|limit| limit.get()) {
            return Self { depth: 0, limit };
        }
        Self {
            depth: 0,
            limit: Codegen::<cranelift_jit::JITModule>::CHAIN_RECURSION_LIMIT,
        }
    }

    pub(crate) fn child(self) -> Self {
        Self {
            depth: self.depth.saturating_add(1),
            limit: self.limit,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum EvalRequest {
    Scalar,
    Fat,
    FatSlot(Option<StackSlot>),
    View,
    Struct,
    Enum,
}

#[cfg(test)]
std::thread_local! {
    static TEST_EXPR_LIMIT: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_expr_limit<T>(limit: u32, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<u32>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_EXPR_LIMIT.with(|current| current.set(self.0));
        }
    }

    let previous = TEST_EXPR_LIMIT.with(|current| current.replace(Some(limit)));
    let _reset = Reset(previous);
    f()
}

impl<M: Module> Codegen<M> {
    pub(crate) fn is_generic_scalar_tag(tag: TirTag) -> bool {
        matches!(
            tag,
            TirTag::INeg | TirTag::FNeg | TirTag::BoolNot | TirTag::Call
        ) || Self::is_chain_binop_tag(tag)
    }

    pub(crate) fn is_generic_expr_tag(tag: TirTag) -> bool {
        Self::is_generic_scalar_tag(tag)
            || matches!(
                tag,
                TirTag::StrConcat
                    | TirTag::BytesConcat
                    | TirTag::Slice
                    | TirTag::ToView
                    | TirTag::ViewAsOwner
                    | TirTag::StrLen
                    | TirTag::BytesIndex
                    | TirTag::StructLit
                    | TirTag::EnumLit
                    | TirTag::FieldAccess
                    | TirTag::DebugRepr
                    | TirTag::ErrorMessage
                    | TirTag::ErrorUnionCoerce
                    | TirTag::StructEq
                    | TirTag::StructNe
                    | TirTag::EnumEq
                    | TirTag::EnumNe
                    | TirTag::StrCmpEq
                    | TirTag::StrCmpNe
                    | TirTag::BytesCmpEq
                    | TirTag::BytesCmpNe
            )
    }

    pub(crate) fn eval_inst_with(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
        request: EvalRequest,
        context: CodegenExprContext,
    ) -> Result<ValueRepr, String> {
        if context.depth >= context.limit && Self::is_generic_expr_tag(ctx.tir.inst(r).tag) {
            return Self::eval_expr_machine(builder, ctx, r, request);
        }
        match request {
            EvalRequest::Scalar => {
                Self::eval_inst_d(builder, ctx, r, context).map(ValueRepr::Scalar)
            }
            EvalRequest::Fat => Self::eval_inst_fat_slot_d(builder, ctx, r, None, context),
            EvalRequest::FatSlot(slot) => {
                Self::eval_inst_fat_slot_d(builder, ctx, r, slot, context)
            }
            EvalRequest::View => Self::eval_inst_view(builder, ctx, r),
            EvalRequest::Struct => {
                Self::eval_inst_struct(builder, ctx, r).map(|addr| ValueRepr::Struct { addr })
            }
            EvalRequest::Enum => {
                Self::eval_inst_enum(builder, ctx, r).map(|addr| ValueRepr::Enum { addr })
            }
        }
    }

    fn eval_non_chain(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        node: TirRef,
    ) -> Result<ValueRepr, String> {
        let context = CodegenExprContext {
            depth: 0,
            limit: u32::MAX,
        };
        let ty = ctx.tir.inst(node).ty;
        if super::is_fat_type(ty, ctx.pool) {
            return Self::eval_inst_with(builder, ctx, node, EvalRequest::Fat, context);
        }
        if ctx.pool.is_view(ty) {
            return Self::eval_inst_with(builder, ctx, node, EvalRequest::View, context);
        }
        match ctx.pool.kind(ty) {
            TypeKind::Struct | TypeKind::AnonStruct => {
                Self::eval_inst_with(builder, ctx, node, EvalRequest::Struct, context)
            }
            TypeKind::Enum
            | TypeKind::DeclaredError
            | TypeKind::ErrorSet
            | TypeKind::ErrorUnion => {
                Self::eval_inst_with(builder, ctx, node, EvalRequest::Enum, context)
            }
            _ => Self::eval_inst_with(builder, ctx, node, EvalRequest::Scalar, context),
        }
    }

    pub(super) fn eval_scalar_machine(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        root: TirRef,
    ) -> Result<Value, String> {
        match Self::eval_expr_machine(builder, ctx, root, EvalRequest::Scalar)? {
            ValueRepr::Scalar(value) => Ok(value),
            _ => unreachable!("scalar request must produce a scalar value"),
        }
    }

    #[cold]
    #[inline(never)]
    pub(super) fn eval_expr_machine(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        root: TirRef,
        request: EvalRequest,
    ) -> Result<ValueRepr, String> {
        enum Task {
            Enter(TirRef),
            AfterLhs(TirRef),
            Exit(TirRef),
        }

        let mut tasks = ChainSpine::new();
        tasks.push(Task::Enter(root));
        while let Some(task) = tasks.pop() {
            match task {
                Task::Enter(node) => {
                    if Self::cached_repr(ctx, node).is_some() {
                        continue;
                    }
                    let inst = ctx.tir.inst(node);
                    if inst.tag == TirTag::Call {
                        let view = ctx.tir.call_view(node);
                        tasks.push(Task::Exit(node));
                        for arg in view.args.iter().rev() {
                            tasks.push(Task::Enter(arg));
                        }
                        continue;
                    }
                    if is_composite_tag(inst.tag) {
                        let children = composite_children(ctx, node);
                        tasks.push(Task::Exit(node));
                        for child in children.into_iter().rev() {
                            tasks.push(Task::Enter(child));
                        }
                        continue;
                    }
                    match inst.data {
                        TirData::BinOp { lhs, .. } if Self::is_chain_binop_tag(inst.tag) => {
                            tasks.push(Task::Exit(node));
                            tasks.push(Task::AfterLhs(node));
                            if Self::is_generic_scalar_tag(ctx.tir.inst(lhs).tag)
                                && Self::cached_repr(ctx, lhs).is_none()
                            {
                                tasks.push(Task::Enter(lhs));
                            } else {
                                Self::eval_inst(builder, ctx, lhs)?;
                            }
                        }
                        TirData::UnOp(operand)
                            if matches!(
                                inst.tag,
                                TirTag::INeg | TirTag::FNeg | TirTag::BoolNot
                            ) =>
                        {
                            tasks.push(Task::Exit(node));
                            if Self::is_generic_scalar_tag(ctx.tir.inst(operand).tag)
                                && Self::cached_repr(ctx, operand).is_none()
                            {
                                tasks.push(Task::Enter(operand));
                            } else {
                                Self::eval_inst(builder, ctx, operand)?;
                            }
                        }
                        _ => {
                            Self::eval_non_chain(builder, ctx, node)?;
                        }
                    }
                }
                Task::AfterLhs(node) => {
                    let TirData::BinOp { rhs, .. } = ctx.tir.inst(node).data else {
                        unreachable!("chain tag must carry TirData::BinOp");
                    };
                    if Self::is_generic_scalar_tag(ctx.tir.inst(rhs).tag)
                        && Self::cached_repr(ctx, rhs).is_none()
                    {
                        tasks.push(Task::Enter(rhs));
                    } else {
                        Self::eval_inst(builder, ctx, rhs)?;
                    }
                }
                Task::Exit(node) => {
                    if Self::cached_repr(ctx, node).is_some() {
                        continue;
                    }
                    let inst = ctx.tir.inst(node);
                    match inst.data {
                        TirData::BinOp { lhs, rhs } if Self::is_chain_binop_tag(inst.tag) => {
                            let lv = Self::eval_inst(builder, ctx, lhs)?;
                            let rv = Self::eval_inst(builder, ctx, rhs)?;
                            let value =
                                Self::emit_numeric_binop(builder, ctx, inst.tag, lhs, rhs, lv, rv)?;
                            Self::cache_repr(ctx, node, ValueRepr::Scalar(value));
                        }
                        TirData::UnOp(operand)
                            if matches!(
                                inst.tag,
                                TirTag::INeg | TirTag::FNeg | TirTag::BoolNot
                            ) =>
                        {
                            let acc = Self::eval_inst(builder, ctx, operand)?;
                            let value =
                                Self::emit_unary_link(builder, ctx, inst.tag, operand, acc)?;
                            Self::cache_repr(ctx, node, ValueRepr::Scalar(value));
                        }
                        _ if inst.tag == TirTag::Call => {
                            if super::is_fat_type(inst.ty, ctx.pool) {
                                if let EvalRequest::FatSlot(slot) = request
                                    && node == root
                                {
                                    Self::eval_inst_fat_slot_d(
                                        builder,
                                        ctx,
                                        node,
                                        slot,
                                        CodegenExprContext {
                                            depth: 0,
                                            limit: u32::MAX,
                                        },
                                    )?;
                                } else {
                                    Self::eval_non_chain(builder, ctx, node)?;
                                }
                            } else {
                                let value = Self::emit_call(builder, ctx, node)?;
                                if Self::cached_repr(ctx, node).is_none() {
                                    Self::cache_repr(ctx, node, ValueRepr::Scalar(value));
                                }
                            }
                        }
                        _ if is_composite_tag(inst.tag) => {
                            if let EvalRequest::FatSlot(slot) = request
                                && node == root
                            {
                                Self::eval_inst_fat_slot_d(
                                    builder,
                                    ctx,
                                    node,
                                    slot,
                                    CodegenExprContext {
                                        depth: 0,
                                        limit: u32::MAX,
                                    },
                                )?;
                            } else {
                                Self::eval_non_chain(builder, ctx, node)?;
                            }
                        }
                        _ => unreachable!("only scheduled nodes have exit tasks"),
                    }
                }
            }
        }
        match request {
            EvalRequest::FatSlot(slot) => {
                Self::eval_inst_fat_slot_d(builder, ctx, root, slot, CodegenExprContext::root())
            }
            _ => Self::eval_non_chain(builder, ctx, root),
        }
    }
}

fn is_composite_tag(tag: TirTag) -> bool {
    matches!(
        tag,
        TirTag::StrConcat
            | TirTag::BytesConcat
            | TirTag::Slice
            | TirTag::ToView
            | TirTag::ViewAsOwner
            | TirTag::StrLen
            | TirTag::BytesIndex
            | TirTag::StructLit
            | TirTag::EnumLit
            | TirTag::FieldAccess
            | TirTag::DebugRepr
            | TirTag::ErrorMessage
            | TirTag::ErrorUnionCoerce
            | TirTag::StructEq
            | TirTag::StructNe
            | TirTag::EnumEq
            | TirTag::EnumNe
            | TirTag::StrCmpEq
            | TirTag::StrCmpNe
            | TirTag::BytesCmpEq
            | TirTag::BytesCmpNe
    )
}

fn composite_children<M: Module>(ctx: &FunctionContext<'_, M>, node: TirRef) -> Vec<TirRef> {
    let inst = ctx.tir.inst(node);
    match inst.tag {
        TirTag::Slice => {
            let TirData::Slice { base, start, end } = inst.data else {
                unreachable!()
            };
            std::iter::once(base).chain(start).chain(end).collect()
        }
        TirTag::StructLit => ctx
            .tir
            .struct_lit_view(node)
            .fields()
            .map(|(_, value)| value)
            .collect(),
        TirTag::EnumLit => ctx
            .tir
            .enum_lit_view(node)
            .fields()
            .map(|(_, value)| value)
            .collect(),
        TirTag::FieldAccess => {
            let TirData::FieldAccess { object, .. } = inst.data else {
                unreachable!()
            };
            vec![object]
        }
        TirTag::StrConcat
        | TirTag::BytesConcat
        | TirTag::BytesIndex
        | TirTag::StructEq
        | TirTag::StructNe
        | TirTag::EnumEq
        | TirTag::EnumNe
        | TirTag::StrCmpEq
        | TirTag::StrCmpNe
        | TirTag::BytesCmpEq
        | TirTag::BytesCmpNe => {
            let TirData::BinOp { lhs, rhs } = inst.data else {
                unreachable!()
            };
            vec![lhs, rhs]
        }
        TirTag::ToView
        | TirTag::ViewAsOwner
        | TirTag::StrLen
        | TirTag::DebugRepr
        | TirTag::ErrorMessage
        | TirTag::ErrorUnionCoerce => {
            let TirData::UnOp(operand) = inst.data else {
                unreachable!()
            };
            vec![operand]
        }
        _ => unreachable!("not a composite expression"),
    }
}
