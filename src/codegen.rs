//! Cranelift codegen over UIR.
//!
//! Phase 3 commit 4: codegen consumes the flat [`Uir`] produced by
//! `astgen` plus the [`TypeTable`] produced by `sema`. The HIR is
//! gone from this module; commit 5 deletes `src/hir.rs` outright.
//!
//! Traversal is *index-driven* — every operand is reached through
//! an [`InstRef`] into `uir.instructions`, never through a
//! recursive descent over a tree-shaped node. Two recursions
//! survive:
//!
//! 1. Materializing an instruction whose operands are themselves
//!    instructions (e.g. `Add %3, %5` materializes `%3` and `%5`
//!    first). Cranelift always needs nested values, so something
//!    has to walk operands; doing it through the `InstRef` index
//!    is the point.
//! 2. The `eval_inst` memoization map (`HashMap<InstRef, Value>`)
//!    so a shared sub-expression isn't re-emitted. UIR today is
//!    tree-shaped (one parent per inst) so this is purely
//!    defensive — but it's the right invariant before TIR / lazy
//!    sema land. Zig calls the analogous mapping in `Air.zig`
//!    "liveness"; we don't need full liveness yet.

use crate::types::{InternPool, StringId, TypeId, TypeKind};
use crate::uir::{FuncBody, InstData, InstRef, InstTag, Uir};
use cranelift::codegen::isa;
use cranelift::codegen::settings::{self, Configurable};
use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
use target_lexicon::Triple;

/// Per-instruction resolved-type table produced by `sema::analyze`.
/// Re-exported here so codegen and the pipeline driver can share
/// the same type alias without an extra `use` line.
pub type TypeTable = [Option<TypeId>];

/// Map a UIR/sema type to the corresponding Cranelift IR type.
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

/// Per-function emission state. Lives only for the duration of one
/// `compile_function` call; reset between functions because
/// Cranelift `Variable` ids and the `InstRef → Value` memo are both
/// function-local.
struct FunctionContext<'a, M: Module> {
    module: &'a mut M,
    data_ctx: &'a mut DataDescription,
    string_data: &'a mut HashMap<StringId, DataId>,
    int_type: types::Type,
    triple: &'a Triple,
    pool: &'a InternPool,
    uir: &'a Uir,
    types: &'a TypeTable,
    locals: HashMap<StringId, Variable>,
    func_ids: &'a HashMap<StringId, FuncId>,
    /// `InstRef → Value` memo. Materializing the same instruction
    /// twice in one function would either duplicate side effects
    /// (calls) or waste Cranelift IR; both are cheap-but-wrong.
    inst_values: HashMap<InstRef, Value>,
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
    pub fn compile(
        &mut self,
        uir: &Uir,
        types: &TypeTable,
        pool: &InternPool,
    ) -> Result<FuncId, String> {
        debug_assert!(
            all_types_resolved(uir, types),
            "codegen::compile requires sema to have filled the TypeTable"
        );
        let func_ids = self.declare_all_functions(uir, pool)?;

        for body in &uir.func_bodies {
            self.compile_function(body, uir, types, &func_ids, pool)?;
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
        uir: &Uir,
        types: &TypeTable,
        pool: &InternPool,
    ) -> Result<String, String> {
        debug_assert!(
            all_types_resolved(uir, types),
            "codegen::compile_and_dump_ir requires sema to have filled the TypeTable"
        );
        let func_ids = self.declare_all_functions(uir, pool)?;

        let mut ir_output = String::new();
        for body in &uir.func_bodies {
            if let Some(ir) = self.compile_function(body, uir, types, &func_ids, pool)? {
                ir_output.push_str(&ir);
                ir_output.push('\n');
            }
        }

        Ok(ir_output)
    }

    fn declare_all_functions(
        &mut self,
        uir: &Uir,
        pool: &InternPool,
    ) -> Result<HashMap<StringId, FuncId>, String> {
        let mut func_ids = HashMap::new();
        for body in &uir.func_bodies {
            let sig = self.build_signature(body, pool);
            let name_str = pool.str(body.name);
            let linkage = if name_str == "main" {
                Linkage::Export
            } else {
                Linkage::Local
            };
            let func_id = self
                .module
                .declare_function(name_str, linkage, &sig)
                .map_err(|e| format!("Failed to declare function '{}': {}", name_str, e))?;
            func_ids.insert(body.name, func_id);
        }
        Ok(func_ids)
    }

    fn build_signature(&self, body: &FuncBody, pool: &InternPool) -> Signature {
        let mut sig = self.module.make_signature();
        for param in &body.params {
            let cl_ty = cranelift_type_for(param.ty, pool, self.int_type);
            sig.params.push(AbiParam::new(cl_ty));
        }
        if body.return_type != pool.void() {
            let cl_ty = cranelift_type_for(body.return_type, pool, self.int_type);
            sig.returns.push(AbiParam::new(cl_ty));
        }
        sig
    }

    fn compile_function(
        &mut self,
        body: &FuncBody,
        uir: &Uir,
        types: &TypeTable,
        func_ids: &HashMap<StringId, FuncId>,
        pool: &InternPool,
    ) -> Result<Option<String>, String> {
        let func_id = *func_ids
            .get(&body.name)
            .ok_or_else(|| format!("Function '{}' not declared", pool.str(body.name)))?;

        self.ctx.func.signature = self.build_signature(body, pool);

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);

            let int_type = self.int_type;
            let mut locals: HashMap<StringId, Variable> = HashMap::new();

            for (i, param) in body.params.iter().enumerate() {
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
                uir,
                types,
                locals,
                func_ids,
                inst_values: HashMap::new(),
            };

            let mut has_return = false;
            for stmt_ref in uir.body_stmts(body) {
                if has_return {
                    break;
                }
                has_return = Self::emit_stmt(&mut builder, &mut ctx, stmt_ref)?;
            }

            if !has_return {
                if body.return_type != pool.void() {
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
            .map_err(|e| format!("Failed to define function '{}': {}", pool.str(body.name), e))?;

        self.ctx.clear();
        Ok(Some(ir_text))
    }

    /// Emit a top-level statement instruction. Returns `true` iff
    /// the statement was a terminator (Return / ReturnVoid) — the
    /// caller stops the body walk on the first one.
    fn emit_stmt(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: InstRef,
    ) -> Result<bool, String> {
        let inst = ctx.uir.inst(r);
        match inst.tag {
            InstTag::VarDecl => {
                let view = ctx.uir.var_decl_view(r);
                let val = Self::eval_inst(builder, ctx, view.initializer)?;
                let init_ty = ctx.types[view.initializer.index()]
                    .expect("sema must have typed the initializer");
                let cl_ty = cranelift_type_for(init_ty, ctx.pool, ctx.int_type);
                let var = builder.declare_var(cl_ty);
                builder.def_var(var, val);
                ctx.locals.insert(view.name, var);
                Ok(false)
            }
            InstTag::Return => {
                let operand = match inst.data {
                    InstData::UnOp(o) => o,
                    _ => unreachable!("Return must carry InstData::UnOp"),
                };
                let val = Self::eval_inst(builder, ctx, operand)?;
                builder.ins().return_(&[val]);
                Ok(true)
            }
            InstTag::ReturnVoid => {
                builder.ins().return_(&[]);
                Ok(true)
            }
            InstTag::ExprStmt => {
                let operand = match inst.data {
                    InstData::UnOp(o) => o,
                    _ => unreachable!("ExprStmt must carry InstData::UnOp"),
                };
                let _ = Self::eval_inst(builder, ctx, operand)?;
                Ok(false)
            }
            other => Err(format!(
                "emit_stmt: instruction at %{} is not a statement (tag={:?})",
                r.index(),
                other
            )),
        }
    }

    /// Materialize an instruction's value, recursively materializing
    /// operand `InstRef`s as needed. Memoized: a second visit hands
    /// back the cached `Value`.
    fn eval_inst(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: InstRef,
    ) -> Result<Value, String> {
        if let Some(&v) = ctx.inst_values.get(&r) {
            return Ok(v);
        }
        let inst = ctx.uir.inst(r);
        let value = match inst.tag {
            InstTag::IntLiteral => match inst.data {
                InstData::Int(v) => builder.ins().iconst(ctx.int_type, v),
                _ => unreachable!("IntLiteral must carry InstData::Int"),
            },
            InstTag::BoolLiteral => match inst.data {
                InstData::Bool(b) => builder.ins().iconst(types::I8, if b { 1 } else { 0 }),
                _ => unreachable!("BoolLiteral must carry InstData::Bool"),
            },
            InstTag::StrLiteral => match inst.data {
                InstData::Str(id) => emit_str_literal(builder, ctx, id)?,
                _ => unreachable!("StrLiteral must carry InstData::Str"),
            },
            InstTag::Var => match inst.data {
                InstData::Var(name) => {
                    let var = ctx
                        .locals
                        .get(&name)
                        .ok_or_else(|| format!("Undefined variable: '{}'", ctx.pool.str(name)))?;
                    builder.use_var(*var)
                }
                _ => unreachable!("Var must carry InstData::Var"),
            },
            InstTag::Neg => match inst.data {
                InstData::UnOp(operand) => {
                    let v = Self::eval_inst(builder, ctx, operand)?;
                    builder.ins().ineg(v)
                }
                _ => unreachable!("Neg must carry InstData::UnOp"),
            },
            InstTag::Add
            | InstTag::Sub
            | InstTag::Mul
            | InstTag::Div
            | InstTag::Eq
            | InstTag::NotEq => {
                let (lhs, rhs) = match inst.data {
                    InstData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!("binary op must carry InstData::BinOp"),
                };
                let lv = Self::eval_inst(builder, ctx, lhs)?;
                let rv = Self::eval_inst(builder, ctx, rhs)?;
                match inst.tag {
                    InstTag::Add => builder.ins().iadd(lv, rv),
                    InstTag::Sub => builder.ins().isub(lv, rv),
                    InstTag::Mul => builder.ins().imul(lv, rv),
                    InstTag::Div => builder.ins().sdiv(lv, rv),
                    InstTag::Eq => builder.ins().icmp(IntCC::Equal, lv, rv),
                    InstTag::NotEq => builder.ins().icmp(IntCC::NotEqual, lv, rv),
                    _ => unreachable!(),
                }
            }
            InstTag::Call => Self::emit_call(builder, ctx, r)?,
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
        r: InstRef,
    ) -> Result<Value, String> {
        let view = ctx.uir.call_view(r);
        let name_id = view.name;
        let name_str = ctx.pool.str(name_id);
        if crate::builtins::lookup(name_str).is_some() {
            return match name_str {
                "print" => {
                    Self::generate_print_call(builder, ctx, &view.args)?;
                    Ok(builder.ins().iconst(ctx.int_type, 0))
                }
                _ => Err(format!(
                    "Builtin '{}' has no codegen implementation",
                    name_str
                )),
            };
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

        Ok(if results.is_empty() {
            // Void-returning callee: the surrounding expression
            // still expects *some* value to plug into the memo
            // table. Use a zero int as a benign placeholder; sema
            // has already rejected programs that try to read it.
            builder.ins().iconst(ctx.int_type, 0)
        } else {
            results[0]
        })
    }

    fn generate_print_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        args: &[InstRef],
    ) -> Result<(), String> {
        // Sema has already validated arity and the string-literal
        // constraint (see `sema::check_builtin_call`). The matches
        // below are therefore infallible.
        debug_assert_eq!(args.len(), 1, "sema should reject print() arity errors");
        let string_id = match ctx.uir.inst(args[0]).data {
            InstData::Str(id) => id,
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
        let fd = builder.ins().iconst(ctx.int_type, 1);

        use target_lexicon::OperatingSystem;
        match ctx.triple.operating_system {
            OperatingSystem::Darwin { .. }
            | OperatingSystem::MacOSX { .. }
            | OperatingSystem::Linux => {}
            _ => {
                return Err(format!(
                    "print() not yet supported on platform: {:?}",
                    ctx.triple.operating_system
                ));
            }
        }

        let mut write_sig = ctx.module.make_signature();
        write_sig.params.push(AbiParam::new(ctx.int_type));
        write_sig.params.push(AbiParam::new(ctx.int_type));
        write_sig.params.push(AbiParam::new(ctx.int_type));
        write_sig.returns.push(AbiParam::new(ctx.int_type));

        let write_func = ctx
            .module
            .declare_function("write", Linkage::Import, &write_sig)
            .map_err(|e| format!("Failed to declare write function: {}", e))?;

        let write_ref = ctx.module.declare_func_in_func(write_func, builder.func);
        let call_inst = builder.ins().call(write_ref, &[fd, string_ptr, string_len]);
        let _bytes_written = builder.inst_results(call_inst)[0];

        Ok(())
    }
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

/// Walks the UIR and asserts every reachable instruction has a
/// resolved type. Used inside `debug_assert!` at codegen entry
/// points; sema is expected to have filled the table before codegen
/// runs.
fn all_types_resolved(uir: &Uir, types: &TypeTable) -> bool {
    if types.len() != uir.instructions.len() {
        return false;
    }
    // Slot 0 is the reserved sentinel; legitimately `None`.
    for body in &uir.func_bodies {
        for stmt_ref in uir.body_stmts(body) {
            if !walk_typed(uir, types, stmt_ref) {
                return false;
            }
        }
    }
    true
}

fn walk_typed(uir: &Uir, types: &TypeTable, r: InstRef) -> bool {
    if types[r.index()].is_none() {
        return false;
    }
    let inst = uir.inst(r);
    match inst.data {
        InstData::None
        | InstData::Int(_)
        | InstData::Str(_)
        | InstData::Bool(_)
        | InstData::Var(_) => true,
        InstData::UnOp(o) => walk_typed(uir, types, o),
        InstData::BinOp { lhs, rhs } => walk_typed(uir, types, lhs) && walk_typed(uir, types, rhs),
        InstData::Extra(_) => match inst.tag {
            InstTag::Call => uir
                .call_view(r)
                .args
                .iter()
                .all(|a| walk_typed(uir, types, *a)),
            InstTag::VarDecl => walk_typed(uir, types, uir.var_decl_view(r).initializer),
            _ => true,
        },
    }
}
