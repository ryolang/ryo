use super::{CallArgSnapshots, Codegen, FunctionContext, ValueRepr};
use cranelift::codegen::ir::{MemFlagsData, StackSlot};
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::spine::ChainSpine;
use ryo_core::tir::{ParamMode, TirData, TirRef, TirTag};
use ryo_core::types::TypeKind;
use std::collections::HashMap;

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
    static TEST_MACHINE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_machine_call_count() {
    TEST_MACHINE_CALLS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn machine_call_count() -> usize {
    TEST_MACHINE_CALLS.with(|count| count.get())
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
            EvalRequest::View => Self::eval_inst_view_d(builder, ctx, r, context),
            EvalRequest::Struct => Self::eval_inst_struct_d(builder, ctx, r, context)
                .map(|addr| ValueRepr::Struct { addr }),
            EvalRequest::Enum => Self::eval_inst_enum_d(builder, ctx, r, context)
                .map(|addr| ValueRepr::Enum { addr }),
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
    pub(crate) fn eval_expr_machine(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        root: TirRef,
        request: EvalRequest,
    ) -> Result<ValueRepr, String> {
        #[cfg(test)]
        TEST_MACHINE_CALLS.with(|count| count.set(count.get() + 1));
        enum Task {
            Enter(TirRef),
            SnapshotCallArg(TirRef, usize),
            AggregateField(TirRef, usize),
            AfterLhs(TirRef),
            Exit(TirRef),
        }

        let mut tasks = ChainSpine::new();
        let mut snapshots = CallArgSnapshots::new();
        let mut aggregate_slots = HashMap::new();
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
                        for index in (0..view.args.len()).rev() {
                            tasks.push(Task::SnapshotCallArg(node, index));
                            tasks.push(Task::Enter(view.args.at(index)));
                        }
                        continue;
                    }
                    if matches!(inst.tag, TirTag::StructLit | TirTag::EnumLit) {
                        let addr = Self::prepare_aggregate_machine(builder, ctx, node)?;
                        aggregate_slots.insert(node, addr);
                        let children = composite_children(ctx, node);
                        tasks.push(Task::Exit(node));
                        for index in (0..children.len()).rev() {
                            tasks.push(Task::AggregateField(node, index));
                            tasks.push(Task::Enter(children[index]));
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
                Task::SnapshotCallArg(node, index) => {
                    let view = ctx.tir.call_view(node);
                    let arg = view.args.at(index);
                    let mode = view.modes.at(index);
                    if mode != ParamMode::Move {
                        continue;
                    }
                    let context = CodegenExprContext {
                        depth: 0,
                        limit: u32::MAX,
                    };
                    let addr = match Self::cached_repr(ctx, arg) {
                        Some(ValueRepr::Struct { .. }) => Some(Self::emit_struct_call_arg_d(
                            builder, ctx, arg, mode, context,
                        )?),
                        Some(ValueRepr::Enum { .. }) => Some(Self::emit_enum_call_arg_d(
                            builder, ctx, arg, mode, context,
                        )?),
                        _ => None,
                    };
                    if let Some(addr) = addr {
                        snapshots.insert((node, arg), addr);
                    }
                }
                Task::AggregateField(node, index) => {
                    let addr = aggregate_slots[&node];
                    Self::store_aggregate_field_machine(builder, ctx, node, index, addr)?;
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
                            Self::eval_call_machine(builder, ctx, node, root, request, &snapshots)?;
                        }
                        _ if matches!(inst.tag, TirTag::StructLit | TirTag::EnumLit) => {
                            let addr = aggregate_slots.remove(&node).expect("aggregate slot");
                            let repr = if inst.tag == TirTag::StructLit {
                                ValueRepr::Struct { addr }
                            } else {
                                ValueRepr::Enum { addr }
                            };
                            Self::cache_repr(ctx, node, repr);
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

    fn prepare_aggregate_machine(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        node: TirRef,
    ) -> Result<Value, String> {
        let inst = ctx.tir.inst(node);
        let slot = match inst.tag {
            TirTag::StructLit => Self::struct_slot(builder, ctx, inst.ty),
            TirTag::EnumLit => Self::enum_slot(builder, ctx, inst.ty),
            _ => unreachable!("aggregate literal expected"),
        };
        let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
        if inst.tag == TirTag::EnumLit {
            let view = ctx.tir.enum_lit_view(node);
            let tag = builder
                .ins()
                .iconst(types::I32, i64::from(view.variant_index));
            builder.ins().store(MemFlagsData::trusted(), tag, addr, 0);
        }
        Ok(addr)
    }

    fn store_aggregate_field_machine(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        node: TirRef,
        index: usize,
        addr: Value,
    ) -> Result<(), String> {
        let inst = ctx.tir.inst(node);
        let context = CodegenExprContext {
            depth: 0,
            limit: u32::MAX,
        };
        match inst.tag {
            TirTag::StructLit => {
                let view = ctx.tir.struct_lit_view(node);
                let (field_index, value) = view.fields().nth(index).expect("struct field");
                let field = ctx.pool.struct_view(view.ty).fields[field_index as usize];
                Self::store_field_value(builder, ctx, addr, field.offset, field.ty, value, context)
            }
            TirTag::EnumLit => {
                let view = ctx.tir.enum_lit_view(node);
                let (field_index, value) = view.fields().nth(index).expect("enum field");
                let field = ctx
                    .pool
                    .enum_view(view.ty)
                    .variants()
                    .nth(view.variant_index as usize)
                    .expect("enum variant")
                    .fields[field_index as usize];
                Self::store_field_value(builder, ctx, addr, field.offset, field.ty, value, context)
            }
            _ => unreachable!("aggregate literal expected"),
        }
    }

    fn eval_call_machine(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        node: TirRef,
        root: TirRef,
        request: EvalRequest,
        snapshots: &CallArgSnapshots,
    ) -> Result<(), String> {
        let context = CodegenExprContext {
            depth: 0,
            limit: u32::MAX,
        };
        if super::is_fat_type(ctx.tir.inst(node).ty, ctx.pool) {
            let out_slot = if node == root {
                match request {
                    EvalRequest::FatSlot(slot) => slot,
                    _ => None,
                }
            } else {
                None
            };
            Self::eval_inst_fat_slot_with_snapshots(
                builder,
                ctx,
                node,
                out_slot,
                context,
                Some(snapshots),
            )?;
            return Ok(());
        }
        let value = Self::emit_call_slot_with_snapshots(
            builder,
            ctx,
            node,
            None,
            context,
            Some(snapshots),
        )?;
        if Self::cached_repr(ctx, node).is_none() {
            Self::cache_repr(ctx, node, ValueRepr::Scalar(value));
        }
        Ok(())
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
