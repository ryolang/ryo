//! Cranelift codegen over TIR.
//!
//! Codegen consumes the typed instruction streams produced by
//! `sema` (one [`Tir`] per function body) and lowers them to
//! Cranelift IR. There is no [`crate::uir::Uir`] import here:
//! every operand is already typed, every variable already
//! resolved.
//!
//! Traversal is *index-driven* — operands are reached through
//! [`TirRef`] indices into the current `Tir`'s `instructions`,
//! never through a recursive descent over a tree-shaped node.
//! Two recursions survive:
//!
//! 1. Materializing an instruction whose operands are themselves
//!    instructions (e.g. `IAdd %3, %5` materializes `%3` and `%5`
//!    first). Cranelift always needs nested values; doing it
//!    through `TirRef` indexing is the point.
//! 2. The `eval_inst` memoization map (`HashMap<TirRef, Value>`)
//!    so a shared sub-expression isn't re-emitted. TIR today is
//!    tree-shaped (one parent per inst) so this is purely
//!    defensive — but it's the right invariant before lazy sema
//!    / inline expansion lands. Zig calls the analogous mapping
//!    in `Air.zig` "liveness"; we don't need full liveness yet.

use crate::ast::CompoundOp;
use crate::tir::{Tir, TirData, TirRef, TirTag};
use crate::types::{InternPool, StringId, TypeId, TypeKind};
use cranelift::codegen::ir::{BlockArg, FuncRef};
use cranelift::codegen::isa;
use cranelift::codegen::settings::{self, Configurable};
use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
use target_lexicon::Triple;

/// Map a TIR type to the corresponding Cranelift IR type.
///
/// `Int` uses the target's pointer-sized integer (i64 on 64-bit).
/// `Bool` uses I8 (matches Cranelift's `icmp` result width and Rust's bool layout).
/// `Str` is represented as a pointer (pointer-sized integer).
/// `Void` has no Cranelift representation and should not be mapped here.
fn cranelift_type_for(ty: TypeId, pool: &InternPool, pointer_ty: types::Type) -> types::Type {
    match pool.kind(ty) {
        TypeKind::Int => pointer_ty,
        TypeKind::Str => pointer_ty,
        TypeKind::Bool => types::I8,
        TypeKind::Float => types::F64,
        // Dead code after trap, but Cranelift needs a concrete type for every SSA value
        TypeKind::Never => types::I8,
        TypeKind::Void => panic!("cranelift_type_for: void has no representation"),
        TypeKind::Error => {
            // Reaching codegen with the Error sentinel means sema
            // accepted a program despite a resolution failure. The
            // driver must short-circuit on `sink.has_errors()`.
            panic!("cranelift_type_for: <error> sentinel reached codegen")
        }
        TypeKind::Tuple => {
            // Tuple ABI is not implemented yet; the variant exists
            // only to validate the InternPool's sidecar encoding.
            unimplemented!("cranelift_type_for: tuple lowering")
        }
    }
}

pub struct Codegen<M: Module> {
    builder_context: FunctionBuilderContext,
    ctx: codegen::Context,
    module: M,
    int_type: types::Type,
    data_ctx: DataDescription,
    /// Cache of `Cranelift DataId` per interned string content.
    /// Keyed on `StringId` so duplicate string literals reuse the
    /// same `.rodata` blob without an extra hash on the bytes.
    string_data: HashMap<StringId, DataId>,
    triple: Triple,
}

/// Per-loop codegen state: the Cranelift blocks that `break` and
/// `continue` jump to.
#[allow(dead_code)]
struct LoopContext {
    header_block: Block,
    exit_block: Block,
    /// Where `continue` jumps. For while-loops this is the header
    /// (re-evaluate condition); for for-range loops this is the
    /// increment block (advance the counter before re-checking).
    continue_target: Block,
}

/// Per-function emission state. Lives only for the duration of one
/// `compile_function` call; reset between functions because
/// Cranelift `Variable` ids and the `TirRef → Value` memo are both
/// function-local — and because `TirRef` itself is scoped to a
/// single `Tir`.
struct FunctionContext<'a, M: Module> {
    module: &'a mut M,
    data_ctx: &'a mut DataDescription,
    string_data: &'a mut HashMap<StringId, DataId>,
    int_type: types::Type,
    triple: &'a Triple,
    pool: &'a InternPool,
    tir: &'a Tir,
    locals: HashMap<StringId, Variable>,
    func_ids: &'a HashMap<StringId, FuncId>,
    /// `TirRef → Value` memo. Materializing the same instruction
    /// twice in one function would either duplicate side effects
    /// (calls) or waste Cranelift IR; both are cheap-but-wrong.
    inst_values: HashMap<TirRef, Value>,
    loop_stack: Vec<LoopContext>,
}

impl<M: Module> Codegen<M> {
    fn from_module(module: M, triple: Triple) -> Self {
        let int_type = module.target_config().pointer_type();
        Self {
            builder_context: FunctionBuilderContext::new(),
            ctx: module.make_context(),
            module,
            int_type,
            data_ctx: DataDescription::new(),
            string_data: HashMap::new(),
            triple,
        }
    }
}

impl Codegen<ObjectModule> {
    pub fn new_aot(target_triple: Triple) -> Result<Self, String> {
        let mut shared_builder = settings::builder();
        shared_builder
            .enable("is_pic")
            .map_err(|e| format!("Error enabling is_pic: {}", e))?;
        shared_builder
            .set("opt_level", "speed")
            .map_err(|e| format!("Error setting opt_level: {}", e))?;
        shared_builder
            .set("preserve_frame_pointers", "true")
            .map_err(|e| format!("Error setting preserve_frame_pointers: {}", e))?;
        let shared_flags = settings::Flags::new(shared_builder);

        let isa = isa::lookup(target_triple.clone())
            .map_err(|e| format!("Unsupported target '{}': {}", target_triple, e))?
            .finish(shared_flags)
            .map_err(|e| format!("Failed to build ISA: {}", e))?;

        let obj_builder =
            ObjectBuilder::new(isa, "ryo_module", cranelift_module::default_libcall_names())
                .map_err(|e| format!("Failed to create ObjectBuilder: {}", e))?;

        Ok(Self::from_module(
            ObjectModule::new(obj_builder),
            target_triple,
        ))
    }

    pub fn finish(self) -> Result<Vec<u8>, String> {
        self.module
            .finish()
            .emit()
            .map_err(|e| format!("Failed to emit object file: {}", e))
    }
}

impl Codegen<JITModule> {
    pub fn new_jit() -> Result<Self, String> {
        let jit_builder = JITBuilder::new(cranelift_module::default_libcall_names())
            .map_err(|e| format!("Failed to create JIT builder: {}", e))?;

        Ok(Self::from_module(
            JITModule::new(jit_builder),
            Triple::host(),
        ))
    }

    pub fn execute(mut self, main_id: FuncId) -> Result<i32, String> {
        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize JIT definitions: {}", e))?;

        let code_ptr = self.module.get_finalized_function(main_id);
        let main_fn: fn() -> isize = unsafe { std::mem::transmute(code_ptr) };
        let result = main_fn();

        unsafe {
            self.module.free_memory();
        }

        Ok(result as i32)
    }
}

impl<M: Module> Codegen<M> {
    fn declare_runtime_helpers(
        module: &mut M,
        builder_context: &mut FunctionBuilderContext,
        ctx: &mut codegen::Context,
        int_type: types::Type,
        triple: &Triple,
        pool: &InternPool,
        func_ids: &mut HashMap<StringId, FuncId>,
    ) -> Result<(), String> {
        if let Some(panic_name) = pool.find_str("__ryo_panic") {
            let panic_func_id =
                emit_ryo_panic_function(module, builder_context, ctx, int_type, triple)?;
            func_ids.insert(panic_name, panic_func_id);
        }
        Ok(())
    }

    fn prepare_compilation(
        &mut self,
        tirs: &[Tir],
        pool: &InternPool,
    ) -> Result<HashMap<StringId, FuncId>, String> {
        let mut func_ids = self.declare_all_functions(tirs, pool)?;
        Self::declare_runtime_helpers(
            &mut self.module,
            &mut self.builder_context,
            &mut self.ctx,
            self.int_type,
            &self.triple,
            pool,
            &mut func_ids,
        )?;
        Ok(func_ids)
    }

    pub fn compile(&mut self, tirs: &[Tir], pool: &InternPool) -> Result<FuncId, String> {
        debug_assert!(
            no_unreachable_in(tirs),
            "codegen::compile requires sema to have produced TIR with no Unreachable instructions"
        );
        let func_ids = self.prepare_compilation(tirs, pool)?;

        for tir in tirs {
            self.compile_function(tir, &func_ids, pool)?;
        }

        // Resolve "main" through the pool. `astgen` always interns
        // the string "main" (it does so explicitly when synthesising
        // implicit-main and when checking for an explicit-main
        // collision), so the read-only `find_str` probe is
        // guaranteed to hit if the program declares one.
        let main_id = pool
            .find_str("main")
            .ok_or_else(|| "No main function defined".to_string())?;
        func_ids
            .get(&main_id)
            .copied()
            .ok_or_else(|| "No main function defined".to_string())
    }

    pub fn compile_and_dump_ir(
        &mut self,
        tirs: &[Tir],
        pool: &InternPool,
    ) -> Result<String, String> {
        debug_assert!(
            no_unreachable_in(tirs),
            "codegen::compile_and_dump_ir requires sema to have produced TIR with no Unreachable instructions"
        );
        let func_ids = self.prepare_compilation(tirs, pool)?;

        let mut ir_output = String::new();
        for tir in tirs {
            ir_output.push_str(&self.compile_function(tir, &func_ids, pool)?);
            ir_output.push('\n');
        }

        Ok(ir_output)
    }

    fn declare_all_functions(
        &mut self,
        tirs: &[Tir],
        pool: &InternPool,
    ) -> Result<HashMap<StringId, FuncId>, String> {
        let mut func_ids = HashMap::new();
        for tir in tirs {
            let sig = self.build_signature(tir, pool);
            let name_str = pool.str(tir.name);
            let linkage = if name_str == "main" {
                Linkage::Export
            } else {
                Linkage::Local
            };
            let func_id = self
                .module
                .declare_function(name_str, linkage, &sig)
                .map_err(|e| format!("Failed to declare function '{}': {}", name_str, e))?;
            func_ids.insert(tir.name, func_id);
        }
        Ok(func_ids)
    }

    fn build_signature(&self, tir: &Tir, pool: &InternPool) -> Signature {
        let mut sig = self.module.make_signature();
        for param in &tir.params {
            let cl_ty = cranelift_type_for(param.ty, pool, self.int_type);
            sig.params.push(AbiParam::new(cl_ty));
        }
        // C-ABI shim for `main`: Ryo's `fn main()` is void, but the
        // host C runtime (crt0 via zig cc, or our JIT trampoline)
        // calls `main` as `int main()`. Always emit an int-returning
        // signature for `main`; `compile_function` falls through to
        // an explicit `return 0` when Ryo's return type is void.
        let is_main = pool.str(tir.name) == "main";
        if is_main {
            sig.returns.push(AbiParam::new(self.int_type));
        } else if tir.return_type != pool.void() {
            let cl_ty = cranelift_type_for(tir.return_type, pool, self.int_type);
            sig.returns.push(AbiParam::new(cl_ty));
        }
        sig
    }

    fn compile_function(
        &mut self,
        tir: &Tir,
        func_ids: &HashMap<StringId, FuncId>,
        pool: &InternPool,
    ) -> Result<String, String> {
        let func_id = *func_ids
            .get(&tir.name)
            .ok_or_else(|| format!("Function '{}' not declared", pool.str(tir.name)))?;

        self.ctx.func.signature = self.build_signature(tir, pool);

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);

            let int_type = self.int_type;
            let mut locals: HashMap<StringId, Variable> = HashMap::new();

            for (i, param) in tir.params.iter().enumerate() {
                let cl_ty = cranelift_type_for(param.ty, pool, int_type);
                let var = builder.declare_var(cl_ty);
                let param_val = builder.block_params(entry_block)[i];
                builder.def_var(var, param_val);
                locals.insert(param.name, var);
            }

            let mut ctx: FunctionContext<'_, M> = FunctionContext {
                module: &mut self.module,
                data_ctx: &mut self.data_ctx,
                string_data: &mut self.string_data,
                int_type,
                triple: &self.triple,
                pool,
                tir,
                locals,
                func_ids,
                inst_values: HashMap::new(),
                loop_stack: Vec::new(),
            };

            let has_return = Self::emit_body(&mut builder, &mut ctx, &tir.body_stmts())?;

            if !has_return {
                let is_main = pool.str(tir.name) == "main";
                if is_main || tir.return_type != pool.void() {
                    // `main` always returns int 0 to the OS even
                    // when Ryo declares it void; non-main
                    // non-void functions also fall through to a
                    // zero return today (sema accepts missing
                    // returns; control-flow analysis lands in M8b).
                    let zero = builder.ins().iconst(int_type, 0);
                    builder.ins().return_(&[zero]);
                } else {
                    builder.ins().return_(&[]);
                }
            }

            builder.finalize();
        }

        let ir_text = format!("{}", self.ctx.func);

        self.module
            .define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Failed to define function '{}': {}", pool.str(tir.name), e))?;

        self.ctx.clear();
        Ok(ir_text)
    }

    fn emit_body(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        stmts: &[TirRef],
    ) -> Result<bool, String> {
        let mut block_terminated = false;
        for &stmt_ref in stmts {
            if block_terminated {
                break;
            }
            block_terminated = Self::emit_stmt(builder, ctx, stmt_ref)?;
        }
        Ok(block_terminated)
    }

    fn emit_scoped_body(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        stmts: &[TirRef],
    ) -> Result<bool, String> {
        let saved_locals = ctx.locals.clone();
        let block_terminated = Self::emit_body(builder, ctx, stmts)?;
        ctx.locals = saved_locals;
        Ok(block_terminated)
    }

    /// Emit a top-level statement instruction. Returns `true` iff
    /// the statement was a terminator (Return / ReturnVoid) — the
    /// caller stops the body walk on the first one.
    fn emit_stmt(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<bool, String> {
        let inst = ctx.tir.inst(r);
        match inst.tag {
            TirTag::VarDecl => {
                let view = ctx.tir.var_decl_view(r);
                let val = Self::eval_inst(builder, ctx, view.initializer)?;
                // The variable's resolved type lives in the VarDecl
                // inst's `ty` slot directly — no side-table lookup.
                let cl_ty = cranelift_type_for(inst.ty, ctx.pool, ctx.int_type);
                let var = builder.declare_var(cl_ty);
                builder.def_var(var, val);
                ctx.locals.insert(view.name, var);
                Ok(false)
            }
            TirTag::Return => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("Return must carry TirData::UnOp"),
                };
                let val = Self::eval_inst(builder, ctx, operand)?;
                builder.ins().return_(&[val]);
                Ok(true)
            }
            TirTag::ReturnVoid => {
                // Bare `return` in a void function. If this is
                // `main`, the C ABI demands an int return value.
                let is_main = ctx.pool.str(ctx.tir.name) == "main";
                if is_main {
                    let zero = builder.ins().iconst(ctx.int_type, 0);
                    builder.ins().return_(&[zero]);
                } else {
                    builder.ins().return_(&[]);
                }
                Ok(true)
            }
            TirTag::ExprStmt => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ExprStmt must carry TirData::UnOp"),
                };
                let _ = Self::eval_inst(builder, ctx, operand)?;
                Ok(false)
            }
            TirTag::IfStmt => Self::generate_if_stmt(builder, ctx, r),
            TirTag::Assign => {
                let view = ctx.tir.assign_view(r);
                let val = Self::eval_inst(builder, ctx, view.value)?;
                let var = ctx.locals.get(&view.name).ok_or_else(|| {
                    format!(
                        "Undefined variable in assign: '{}'",
                        ctx.pool.str(view.name)
                    )
                })?;
                builder.def_var(*var, val);
                Ok(false)
            }
            TirTag::CompoundAssign => {
                let view = ctx.tir.compound_assign_view(r);
                let rhs = Self::eval_inst(builder, ctx, view.value)?;
                let var = ctx.locals.get(&view.name).ok_or_else(|| {
                    format!(
                        "Undefined variable in compound assign: '{}'",
                        ctx.pool.str(view.name)
                    )
                })?;
                let current = builder.use_var(*var);

                let is_float = inst.ty == ctx.pool.float();
                let result = match (view.op, is_float) {
                    (CompoundOp::Add, false) => builder.ins().iadd(current, rhs),
                    (CompoundOp::Sub, false) => builder.ins().isub(current, rhs),
                    (CompoundOp::Mul, false) => builder.ins().imul(current, rhs),
                    (CompoundOp::Div, false) => builder.ins().sdiv(current, rhs),
                    (CompoundOp::Mod, false) => builder.ins().srem(current, rhs),
                    (CompoundOp::Add, true) => builder.ins().fadd(current, rhs),
                    (CompoundOp::Sub, true) => builder.ins().fsub(current, rhs),
                    (CompoundOp::Mul, true) => builder.ins().fmul(current, rhs),
                    (CompoundOp::Div, true) => builder.ins().fdiv(current, rhs),
                    (CompoundOp::Mod, true) => return Err("float modulo not supported".to_string()),
                };

                builder.def_var(*var, result);
                Ok(false)
            }
            TirTag::WhileLoop => Self::generate_while_loop(builder, ctx, r),
            TirTag::ForRange => Self::generate_for_range(builder, ctx, r),
            TirTag::Break => {
                debug_assert!(
                    ctx.loop_stack.last().is_some(),
                    "break outside loop should be rejected by sema"
                );
                let Some(loop_ctx) = ctx.loop_stack.last() else {
                    return Err("codegen reached break outside loop".to_string());
                };
                builder.ins().jump(loop_ctx.exit_block, &[]);
                Ok(true)
            }
            TirTag::Continue => {
                debug_assert!(
                    ctx.loop_stack.last().is_some(),
                    "continue outside loop should be rejected by sema"
                );
                let Some(loop_ctx) = ctx.loop_stack.last() else {
                    return Err("codegen reached continue outside loop".to_string());
                };
                builder.ins().jump(loop_ctx.continue_target, &[]);
                Ok(true)
            }
            other => Err(format!(
                "emit_stmt: instruction at %{} is not a statement (tag={:?})",
                r.index(),
                other
            )),
        }
    }

    fn generate_if_stmt(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<bool, String> {
        let view = ctx.tir.if_stmt_view(r);
        let merge_block = builder.create_block();

        let cond_val = Self::eval_inst(builder, ctx, view.cond)?;
        let then_block = builder.create_block();

        let elif_count = view.elif_branches.len();
        let has_else = view.else_stmts.is_some();
        let capacity = elif_count + usize::from(has_else);
        let mut next_blocks: Vec<Block> = Vec::with_capacity(capacity);
        for _ in 0..elif_count {
            next_blocks.push(builder.create_block());
        }
        let else_or_merge = if has_else {
            let eb = builder.create_block();
            next_blocks.push(eb);
            eb
        } else {
            merge_block
        };

        let first_fallthrough = next_blocks.first().copied().unwrap_or(else_or_merge);

        builder
            .ins()
            .brif(cond_val, then_block, &[], first_fallthrough, &[]);

        builder.seal_block(then_block);
        builder.switch_to_block(then_block);
        let then_returns = Self::emit_scoped_body(builder, ctx, &view.then_stmts)?;
        if !then_returns {
            builder.ins().jump(merge_block, &[]);
        }

        let mut all_branches_return = then_returns;
        for (i, elif) in view.elif_branches.iter().enumerate() {
            let elif_cond_block = next_blocks[i];
            builder.seal_block(elif_cond_block);
            builder.switch_to_block(elif_cond_block);

            let elif_cond_val = Self::eval_inst(builder, ctx, elif.cond)?;
            let elif_body_block = builder.create_block();

            let elif_fallthrough = if i + 1 < next_blocks.len() {
                next_blocks[i + 1]
            } else {
                merge_block
            };

            builder
                .ins()
                .brif(elif_cond_val, elif_body_block, &[], elif_fallthrough, &[]);

            builder.seal_block(elif_body_block);
            builder.switch_to_block(elif_body_block);
            let elif_returns = Self::emit_scoped_body(builder, ctx, &elif.body)?;
            if !elif_returns {
                builder.ins().jump(merge_block, &[]);
            }
            all_branches_return = all_branches_return && elif_returns;
        }

        if let Some(else_stmts) = &view.else_stmts {
            builder.seal_block(else_or_merge);
            builder.switch_to_block(else_or_merge);
            let else_returns = Self::emit_scoped_body(builder, ctx, else_stmts)?;
            if !else_returns {
                builder.ins().jump(merge_block, &[]);
            }
            all_branches_return = all_branches_return && else_returns;
        } else {
            all_branches_return = false;
        }

        builder.seal_block(merge_block);
        if !all_branches_return {
            builder.switch_to_block(merge_block);
        }

        Ok(all_branches_return)
    }

    fn generate_while_loop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<bool, String> {
        let view = ctx.tir.while_loop_view(r);

        let header_block = builder.create_block();
        let body_block = builder.create_block();
        let exit_block = builder.create_block();

        builder.ins().jump(header_block, &[]);

        builder.switch_to_block(header_block);
        let cond_val = Self::eval_inst(builder, ctx, view.cond)?;
        builder
            .ins()
            .brif(cond_val, body_block, &[], exit_block, &[]);

        builder.seal_block(body_block);
        builder.switch_to_block(body_block);

        ctx.loop_stack.push(LoopContext {
            header_block,
            exit_block,
            continue_target: header_block,
        });
        let body_terminated = Self::emit_scoped_body(builder, ctx, &view.body)?;
        ctx.loop_stack.pop();

        if !body_terminated {
            builder.ins().jump(header_block, &[]);
        }

        // Header has two predecessors: entry fallthrough and body back-edge.
        // Seal it last because the back-edge didn't exist until the body emitted.
        builder.seal_block(header_block);
        builder.seal_block(exit_block);
        builder.switch_to_block(exit_block);

        Ok(false)
    }

    fn generate_for_range(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<bool, String> {
        let view = ctx.tir.for_range_view(r);

        // 1. Create all blocks up front
        let header_block = builder.create_block();
        let body_block = builder.create_block();
        let increment_block = builder.create_block();
        let exit_block = builder.create_block();

        // 2. Evaluate bounds once, create hidden counter
        let start_val = Self::eval_inst(builder, ctx, view.start)?;
        let end_val = Self::eval_inst(builder, ctx, view.end)?;
        let counter = builder.declare_var(ctx.int_type);
        builder.def_var(counter, start_val);
        builder.ins().jump(header_block, &[]);

        // 3. Header — DO NOT seal yet (back-edge from increment not emitted)
        builder.switch_to_block(header_block);
        let i = builder.use_var(counter);
        let cond = builder.ins().icmp(IntCC::SignedLessThan, i, end_val);
        builder.ins().brif(cond, body_block, &[], exit_block, &[]);

        // Push loop context: continue targets increment
        ctx.loop_stack.push(LoopContext {
            header_block,
            exit_block,
            continue_target: increment_block,
        });

        // 4. Body — seal immediately (only predecessor is header's brif true-arm)
        builder.seal_block(body_block);
        builder.switch_to_block(body_block);

        // Scope the loop variable: map var_name to the counter Variable.
        // We deliberately use emit_body rather than emit_scoped_body here
        // because we need to insert the counter binding between the save
        // and the emit; emit_scoped_body's internal save would shadow our
        // insertion.
        let saved_locals = ctx.locals.clone();
        ctx.locals.insert(view.var_name, counter);

        let body_terminated = Self::emit_body(builder, ctx, &view.body)?;

        // Restore locals (loop variable goes out of scope)
        ctx.locals = saved_locals;

        if !body_terminated {
            builder.ins().jump(increment_block, &[]);
        }

        ctx.loop_stack.pop();

        // 5. Increment — seal after body
        builder.seal_block(increment_block);
        builder.switch_to_block(increment_block);
        let i_current = builder.use_var(counter);
        let one = builder.ins().iconst(ctx.int_type, 1);
        let i_next = builder.ins().iadd(i_current, one);
        builder.def_var(counter, i_next);
        builder.ins().jump(header_block, &[]);

        // 6. Seal header (predecessors: entry jump + increment back-edge)
        builder.seal_block(header_block);

        // 7. Exit — always reachable
        builder.seal_block(exit_block);
        builder.switch_to_block(exit_block);

        Ok(false)
    }

    /// Materialize an instruction's value, recursively materializing
    /// operand `TirRef`s as needed. Memoized: a second visit hands
    /// back the cached `Value`.
    fn eval_inst(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        if let Some(&v) = ctx.inst_values.get(&r) {
            return Ok(v);
        }
        let inst = ctx.tir.inst(r);
        let value = match inst.tag {
            TirTag::IntConst => match inst.data {
                TirData::Int(v) => builder.ins().iconst(ctx.int_type, v),
                _ => unreachable!("IntConst must carry TirData::Int"),
            },
            TirTag::BoolConst => match inst.data {
                TirData::Bool(b) => builder.ins().iconst(types::I8, if b { 1 } else { 0 }),
                _ => unreachable!("BoolConst must carry TirData::Bool"),
            },
            TirTag::FloatConst => match inst.data {
                TirData::Float(v) => builder.ins().f64const(v),
                _ => unreachable!("FloatConst must carry TirData::Float"),
            },
            TirTag::StrConst => match inst.data {
                TirData::Str(id) => emit_str_literal(builder, ctx, id)?,
                _ => unreachable!("StrConst must carry TirData::Str"),
            },
            TirTag::Var => match inst.data {
                TirData::Var(name) => {
                    let var = ctx
                        .locals
                        .get(&name)
                        .ok_or_else(|| format!("Undefined variable: '{}'", ctx.pool.str(name)))?;
                    builder.use_var(*var)
                }
                _ => unreachable!("Var must carry TirData::Var"),
            },
            TirTag::INeg => match inst.data {
                TirData::UnOp(operand) => {
                    let v = Self::eval_inst(builder, ctx, operand)?;
                    builder.ins().ineg(v)
                }
                _ => unreachable!("INeg must carry TirData::UnOp"),
            },
            TirTag::BoolNot => match inst.data {
                TirData::UnOp(operand) => {
                    let v = Self::eval_inst(builder, ctx, operand)?;
                    let one = builder.ins().iconst(types::I8, 1);
                    builder.ins().bxor(v, one)
                }
                _ => unreachable!("BoolNot must carry TirData::UnOp"),
            },
            TirTag::IAdd
            | TirTag::ISub
            | TirTag::IMul
            | TirTag::ISDiv
            | TirTag::IMod
            | TirTag::ICmpEq
            | TirTag::ICmpNe
            | TirTag::ICmpLt
            | TirTag::ICmpLe
            | TirTag::ICmpGt
            | TirTag::ICmpGe
            | TirTag::FAdd
            | TirTag::FSub
            | TirTag::FMul
            | TirTag::FDiv
            | TirTag::FCmpEq
            | TirTag::FCmpNe
            | TirTag::FCmpLt
            | TirTag::FCmpLe
            | TirTag::FCmpGt
            | TirTag::FCmpGe => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("binary op must carry TirData::BinOp"),
                };
                let lv = Self::eval_inst(builder, ctx, lhs)?;
                let rv = Self::eval_inst(builder, ctx, rhs)?;
                match inst.tag {
                    TirTag::IAdd => builder.ins().iadd(lv, rv),
                    TirTag::ISub => builder.ins().isub(lv, rv),
                    TirTag::IMul => builder.ins().imul(lv, rv),
                    TirTag::ISDiv => builder.ins().sdiv(lv, rv),
                    TirTag::IMod => builder.ins().srem(lv, rv),
                    TirTag::ICmpEq => builder.ins().icmp(IntCC::Equal, lv, rv),
                    TirTag::ICmpNe => builder.ins().icmp(IntCC::NotEqual, lv, rv),
                    TirTag::ICmpLt => builder.ins().icmp(IntCC::SignedLessThan, lv, rv),
                    TirTag::ICmpLe => builder.ins().icmp(IntCC::SignedLessThanOrEqual, lv, rv),
                    TirTag::ICmpGt => builder.ins().icmp(IntCC::SignedGreaterThan, lv, rv),
                    TirTag::ICmpGe => builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, lv, rv),
                    TirTag::FAdd => builder.ins().fadd(lv, rv),
                    TirTag::FSub => builder.ins().fsub(lv, rv),
                    TirTag::FMul => builder.ins().fmul(lv, rv),
                    TirTag::FDiv => builder.ins().fdiv(lv, rv),
                    TirTag::FCmpEq => builder.ins().fcmp(FloatCC::Equal, lv, rv),
                    TirTag::FCmpNe => builder.ins().fcmp(FloatCC::NotEqual, lv, rv),
                    TirTag::FCmpLt => builder.ins().fcmp(FloatCC::LessThan, lv, rv),
                    TirTag::FCmpLe => builder.ins().fcmp(FloatCC::LessThanOrEqual, lv, rv),
                    TirTag::FCmpGt => builder.ins().fcmp(FloatCC::GreaterThan, lv, rv),
                    TirTag::FCmpGe => builder.ins().fcmp(FloatCC::GreaterThanOrEqual, lv, rv),
                    _ => unreachable!(),
                }
            }
            TirTag::BoolAnd => {
                let (lhs_ref, rhs_ref) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("BoolAnd must carry TirData::BinOp"),
                };

                let lhs_val = Self::eval_inst(builder, ctx, lhs_ref)?;

                let rhs_block = builder.create_block();
                let false_block = builder.create_block();
                let merge_block = builder.create_block();
                builder.append_block_param(merge_block, types::I8);

                builder
                    .ins()
                    .brif(lhs_val, rhs_block, &[], false_block, &[]);

                builder.seal_block(rhs_block);
                builder.switch_to_block(rhs_block);
                let rhs_val = Self::eval_inst(builder, ctx, rhs_ref)?;
                builder.ins().jump(merge_block, &[BlockArg::Value(rhs_val)]);

                builder.seal_block(false_block);
                builder.switch_to_block(false_block);
                let false_val = builder.ins().iconst(types::I8, 0);
                builder
                    .ins()
                    .jump(merge_block, &[BlockArg::Value(false_val)]);

                builder.seal_block(merge_block);
                builder.switch_to_block(merge_block);
                builder.block_params(merge_block)[0]
            }
            TirTag::BoolOr => {
                let (lhs_ref, rhs_ref) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("BoolOr must carry TirData::BinOp"),
                };

                let lhs_val = Self::eval_inst(builder, ctx, lhs_ref)?;

                let true_block = builder.create_block();
                let rhs_block = builder.create_block();
                let merge_block = builder.create_block();
                builder.append_block_param(merge_block, types::I8);

                builder.ins().brif(lhs_val, true_block, &[], rhs_block, &[]);

                builder.seal_block(true_block);
                builder.switch_to_block(true_block);
                let true_val = builder.ins().iconst(types::I8, 1);
                builder
                    .ins()
                    .jump(merge_block, &[BlockArg::Value(true_val)]);

                builder.seal_block(rhs_block);
                builder.switch_to_block(rhs_block);
                let rhs_val = Self::eval_inst(builder, ctx, rhs_ref)?;
                builder.ins().jump(merge_block, &[BlockArg::Value(rhs_val)]);

                builder.seal_block(merge_block);
                builder.switch_to_block(merge_block);
                builder.block_params(merge_block)[0]
            }
            TirTag::Call => Self::emit_call(builder, ctx, r)?,
            TirTag::IfStmt => {
                Self::generate_if_stmt(builder, ctx, r)?;
                builder.ins().iconst(ctx.int_type, 0)
            }
            TirTag::Unreachable => {
                return Err(
                    "codegen reached an Unreachable TIR inst — sema must have errored".to_string(),
                );
            }
            other => {
                return Err(format!(
                    "eval_inst: instruction at %{} is not a value (tag={:?})",
                    r.index(),
                    other
                ));
            }
        };
        ctx.inst_values.insert(r, value);
        Ok(value)
    }

    fn emit_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let view = ctx.tir.call_view(r);
        let name_id = view.name;
        let name_str = ctx.pool.str(name_id);

        // print is the only builtin with custom codegen (inline syscall).
        // __ryo_panic and user functions go through the normal call path.
        if name_str == "print" {
            Self::generate_print_call(builder, ctx, &view.args)?;
            return Ok(builder.ins().iconst(ctx.int_type, 0));
        }

        let callee_id = *ctx
            .func_ids
            .get(&name_id)
            .ok_or_else(|| format!("Undefined function: '{}'", name_str))?;

        let mut arg_values = Vec::with_capacity(view.args.len());
        for arg in &view.args {
            arg_values.push(Self::eval_inst(builder, ctx, *arg)?);
        }

        let callee_ref = ctx.module.declare_func_in_func(callee_id, builder.func);
        let call = builder.ins().call(callee_ref, &arg_values);
        let results = builder.inst_results(call);

        // If the callee returns never (e.g. __ryo_panic), the call is
        // a terminator. Emit a trap + dead block for subsequent IR.
        // The dead block needs no explicit terminator — compile_function's
        // fallthrough `return 0` provides one. Cranelift verifier is
        // happy as long as every block has exactly one terminator.
        let ret_ty = ctx.tir.inst(r).ty;
        if ctx.pool.is_never(ret_ty) {
            builder.ins().trap(TrapCode::user(1).unwrap());
            let dead = builder.create_block();
            builder.seal_block(dead);
            builder.switch_to_block(dead);
            let dummy_ty = cranelift_type_for(ret_ty, ctx.pool, ctx.int_type);
            return Ok(builder.ins().iconst(dummy_ty, 0));
        }

        if results.is_empty() {
            Ok(builder.ins().iconst(ctx.int_type, 0))
        } else {
            Ok(results[0])
        }
    }

    fn generate_print_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        args: &[TirRef],
    ) -> Result<(), String> {
        // Sema has already validated arity and the string-literal
        // constraint (see `sema::check_builtin_call`). The matches
        // below are therefore infallible.
        debug_assert_eq!(args.len(), 1, "sema should reject print() arity errors");
        let string_id = match ctx.tir.inst(args[0]).data {
            TirData::Str(id) => id,
            other => unreachable!(
                "sema should reject non-literal print() args, got {:?}",
                other
            ),
        };
        let string_content = ctx.pool.str(string_id);

        let data_id = store_string(
            string_id,
            string_content,
            ctx.module,
            ctx.data_ctx,
            ctx.string_data,
        )?;
        let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
        let string_ptr = builder.ins().global_value(ctx.int_type, data_ref);

        let string_len = builder
            .ins()
            .iconst(ctx.int_type, string_content.len() as i64);
        let fd = builder.ins().iconst(types::I32, 1);

        check_platform_support(ctx.triple)?;

        let write_ref = declare_write(ctx.module, builder, ctx.int_type)?;
        let call_inst = builder.ins().call(write_ref, &[fd, string_ptr, string_len]);
        let _bytes_written = builder.inst_results(call_inst)[0];

        Ok(())
    }
}

fn check_platform_support(triple: &Triple) -> Result<(), String> {
    use target_lexicon::OperatingSystem;
    match triple.operating_system {
        OperatingSystem::Darwin { .. }
        | OperatingSystem::MacOSX { .. }
        | OperatingSystem::Linux => Ok(()),
        _ => Err(format!(
            "POSIX syscalls not yet supported on platform: {:?}",
            triple.operating_system
        )),
    }
}

fn emit_ryo_panic_function<M: Module>(
    module: &mut M,
    builder_context: &mut FunctionBuilderContext,
    ctx: &mut codegen::Context,
    int_type: types::Type,
    triple: &Triple,
) -> Result<FuncId, String> {
    check_platform_support(triple)?;

    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(int_type)); // ptr
    sig.params.push(AbiParam::new(int_type)); // len

    let func_id = module
        .declare_function("__ryo_panic", Linkage::Local, &sig)
        .map_err(|e| format!("Failed to declare __ryo_panic: {}", e))?;

    ctx.func.signature = sig.clone();

    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, builder_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        let ptr = builder.block_params(entry)[0];
        let len = builder.block_params(entry)[1];
        let fd = builder.ins().iconst(types::I32, 2); // stderr

        let write_ref = declare_write(module, &mut builder, int_type)?;
        builder.ins().call(write_ref, &[fd, ptr, len]);

        let mut exit_sig = module.make_signature();
        exit_sig.params.push(AbiParam::new(types::I32));
        let exit_func = module
            .declare_function("exit", Linkage::Import, &exit_sig)
            .map_err(|e| format!("Failed to declare exit: {}", e))?;
        let exit_ref = module.declare_func_in_func(exit_func, builder.func);
        let exit_code = builder.ins().iconst(types::I32, 101);
        builder.ins().call(exit_ref, &[exit_code]);

        builder.ins().trap(TrapCode::user(1).unwrap());
        builder.finalize();
    }

    module
        .define_function(func_id, ctx)
        .map_err(|e| format!("Failed to define __ryo_panic: {}", e))?;
    ctx.clear();

    Ok(func_id)
}

fn declare_write<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder,
    int_type: types::Type,
) -> Result<FuncRef, String> {
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I32));
    sig.params.push(AbiParam::new(int_type));
    sig.params.push(AbiParam::new(int_type));
    sig.returns.push(AbiParam::new(int_type));
    let func_id = module
        .declare_function("write", Linkage::Import, &sig)
        .map_err(|e| format!("Failed to declare write function: {}", e))?;
    Ok(module.declare_func_in_func(func_id, builder.func))
}

/// Materialize a string literal pointer into the function. Pulled
/// out of the `Codegen` impl so it can be called without juggling
/// `&mut self` borrows alongside the `FunctionContext`'s mutable
/// references to the same fields.
fn emit_str_literal<M: Module>(
    builder: &mut FunctionBuilder,
    ctx: &mut FunctionContext<'_, M>,
    id: StringId,
) -> Result<Value, String> {
    let content = ctx.pool.str(id);
    let data_id = store_string(id, content, ctx.module, ctx.data_ctx, ctx.string_data)?;
    let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
    Ok(builder.ins().global_value(ctx.int_type, data_ref))
}

fn store_string<M: Module>(
    content_id: StringId,
    content: &str,
    module: &mut M,
    data_ctx: &mut DataDescription,
    string_data: &mut HashMap<StringId, DataId>,
) -> Result<DataId, String> {
    if let Some(&data_id) = string_data.get(&content_id) {
        return Ok(data_id);
    }

    let data_id = module
        .declare_anonymous_data(false, false)
        .map_err(|e| format!("Failed to declare string data: {}", e))?;

    data_ctx.clear();
    data_ctx.define(content.as_bytes().into());

    module
        .define_data(data_id, data_ctx)
        .map_err(|e| format!("Failed to define string data: {}", e))?;

    string_data.insert(content_id, data_id);
    Ok(data_id)
}

/// Walk every TIR body and assert no `Unreachable` instruction is
/// reachable. Used inside `debug_assert!` at codegen entry points;
/// the driver short-circuits on `sink.has_errors()` long before we
/// get here, so any `Unreachable` past that gate is a sema bug.
fn no_unreachable_in(tirs: &[Tir]) -> bool {
    for tir in tirs {
        // Slot 0 is the reserved sentinel and intentionally has
        // tag = Unreachable in the builder; it is *never* part of a
        // body. Skip it.
        for idx in 1..tir.instructions.len() {
            if matches!(tir.instructions[idx].tag, TirTag::Unreachable) {
                return false;
            }
        }
    }
    true
}
