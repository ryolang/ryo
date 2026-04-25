use crate::hir::{
    BinaryOp, HirExpr, HirExprKind, HirFunction, HirProgram, HirStmt, TypeId, UnaryOp,
};
use crate::types::{InternPool, StringId, TypeKind};
use cranelift::codegen::isa;
use cranelift::codegen::settings::{self, Configurable};
use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
use target_lexicon::Triple;

/// Map a HIR type to the corresponding Cranelift IR type.
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

struct FunctionContext<'a, M: Module> {
    module: &'a mut M,
    data_ctx: &'a mut DataDescription,
    string_data: &'a mut HashMap<StringId, DataId>,
    int_type: types::Type,
    triple: &'a Triple,
    pool: &'a InternPool,
    locals: &'a HashMap<StringId, Variable>,
    func_ids: &'a HashMap<StringId, FuncId>,
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
    pub fn compile(&mut self, program: &HirProgram, pool: &InternPool) -> Result<FuncId, String> {
        debug_assert!(
            all_expr_types_resolved(program),
            "codegen::compile requires sema to have filled all HirExpr.ty"
        );
        let func_ids = self.declare_all_functions(program, pool)?;

        for func in &program.functions {
            self.compile_function(func, &func_ids, pool)?;
        }

        // Resolve "main" through the pool. We can't intern here
        // because pool is `&`; the front-end always interns "main"
        // (ast_lower does it explicitly), so a `get` is enough.
        program
            .functions
            .iter()
            .find(|f| pool.str(f.name) == "main")
            .and_then(|f| func_ids.get(&f.name).copied())
            .ok_or_else(|| "No main function defined".to_string())
    }

    pub fn compile_and_dump_ir(
        &mut self,
        program: &HirProgram,
        pool: &InternPool,
    ) -> Result<String, String> {
        debug_assert!(
            all_expr_types_resolved(program),
            "codegen::compile_and_dump_ir requires sema to have filled all HirExpr.ty"
        );
        let func_ids = self.declare_all_functions(program, pool)?;

        let mut ir_output = String::new();
        for func in &program.functions {
            if let Some(ir) = self.compile_function(func, &func_ids, pool)? {
                ir_output.push_str(&ir);
                ir_output.push('\n');
            }
        }

        Ok(ir_output)
    }

    fn declare_all_functions(
        &mut self,
        program: &HirProgram,
        pool: &InternPool,
    ) -> Result<HashMap<StringId, FuncId>, String> {
        let mut func_ids = HashMap::new();
        for func in &program.functions {
            let sig = self.build_signature(func, pool);
            let name_str = pool.str(func.name);
            let linkage = if name_str == "main" {
                Linkage::Export
            } else {
                Linkage::Local
            };
            let func_id = self
                .module
                .declare_function(name_str, linkage, &sig)
                .map_err(|e| format!("Failed to declare function '{}': {}", name_str, e))?;
            func_ids.insert(func.name, func_id);
        }
        Ok(func_ids)
    }

    fn build_signature(&self, func: &HirFunction, pool: &InternPool) -> Signature {
        let mut sig = self.module.make_signature();
        for param in &func.params {
            let cl_ty = cranelift_type_for(param.ty, pool, self.int_type);
            sig.params.push(AbiParam::new(cl_ty));
        }
        if func.return_type != pool.void() {
            let cl_ty = cranelift_type_for(func.return_type, pool, self.int_type);
            sig.returns.push(AbiParam::new(cl_ty));
        }
        sig
    }

    fn compile_function(
        &mut self,
        func: &HirFunction,
        func_ids: &HashMap<StringId, FuncId>,
        pool: &InternPool,
    ) -> Result<Option<String>, String> {
        let func_id = *func_ids
            .get(&func.name)
            .ok_or_else(|| format!("Function '{}' not declared", pool.str(func.name)))?;

        self.ctx.func.signature = self.build_signature(func, pool);

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);

            let int_type = self.int_type;
            let mut locals: HashMap<StringId, Variable> = HashMap::new();

            for (i, param) in func.params.iter().enumerate() {
                let cl_ty = cranelift_type_for(param.ty, pool, int_type);
                let var = builder.declare_var(cl_ty);
                let param_val = builder.block_params(entry_block)[i];
                builder.def_var(var, param_val);
                locals.insert(param.name, var);
            }

            let mut has_return = false;

            for stmt in &func.body {
                if has_return {
                    break;
                }

                match stmt {
                    HirStmt::VarDecl {
                        name, initializer, ..
                    } => {
                        let mut func_ctx: FunctionContext<'_, M> = FunctionContext {
                            module: &mut self.module,
                            data_ctx: &mut self.data_ctx,
                            string_data: &mut self.string_data,
                            int_type,
                            triple: &self.triple,
                            pool,
                            locals: &locals,
                            func_ids,
                        };
                        let val = Self::eval_expr(&mut builder, initializer, &mut func_ctx)?;
                        let cl_ty = cranelift_type_for(initializer.expect_ty(), pool, int_type);
                        let var = builder.declare_var(cl_ty);
                        builder.def_var(var, val);
                        locals.insert(*name, var);
                    }
                    HirStmt::Return(Some(expr), _) => {
                        let mut func_ctx: FunctionContext<'_, M> = FunctionContext {
                            module: &mut self.module,
                            data_ctx: &mut self.data_ctx,
                            string_data: &mut self.string_data,
                            int_type,
                            triple: &self.triple,
                            pool,
                            locals: &locals,
                            func_ids,
                        };
                        let val = Self::eval_expr(&mut builder, expr, &mut func_ctx)?;
                        builder.ins().return_(&[val]);
                        has_return = true;
                    }
                    HirStmt::Return(None, _) => {
                        builder.ins().return_(&[]);
                        has_return = true;
                    }
                    HirStmt::Expr(expr, _) => {
                        let mut func_ctx: FunctionContext<'_, M> = FunctionContext {
                            module: &mut self.module,
                            data_ctx: &mut self.data_ctx,
                            string_data: &mut self.string_data,
                            int_type,
                            triple: &self.triple,
                            pool,
                            locals: &locals,
                            func_ids,
                        };
                        let _val = Self::eval_expr(&mut builder, expr, &mut func_ctx)?;
                    }
                }
            }

            if !has_return {
                if func.return_type != pool.void() {
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
            .map_err(|e| format!("Failed to define function '{}': {}", pool.str(func.name), e))?;

        self.ctx.clear();
        Ok(Some(ir_text))
    }

    fn store_string(
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

    fn eval_expr(
        builder: &mut FunctionBuilder,
        expr: &HirExpr,
        ctx: &mut FunctionContext<'_, M>,
    ) -> Result<Value, String> {
        match &expr.kind {
            HirExprKind::IntLiteral(val) => Ok(builder.ins().iconst(ctx.int_type, *val)),

            HirExprKind::BoolLiteral(val) => {
                Ok(builder.ins().iconst(types::I8, if *val { 1 } else { 0 }))
            }

            HirExprKind::StrLiteral(id) => {
                let content = ctx.pool.str(*id);
                let data_id =
                    Self::store_string(*id, content, ctx.module, ctx.data_ctx, ctx.string_data)?;
                let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
                let ptr = builder.ins().global_value(ctx.int_type, data_ref);
                Ok(ptr)
            }

            HirExprKind::Var(name) => {
                let var = ctx
                    .locals
                    .get(name)
                    .ok_or_else(|| format!("Undefined variable: '{}'", ctx.pool.str(*name)))?;
                Ok(builder.use_var(*var))
            }

            HirExprKind::UnaryOp(UnaryOp::Neg, sub_expr) => {
                let sub_val = Self::eval_expr(builder, sub_expr, ctx)?;
                Ok(builder.ins().ineg(sub_val))
            }

            HirExprKind::BinaryOp(lhs, op, rhs) => {
                let lhs_val = Self::eval_expr(builder, lhs, ctx)?;
                let rhs_val = Self::eval_expr(builder, rhs, ctx)?;

                let result = match op {
                    BinaryOp::Add => builder.ins().iadd(lhs_val, rhs_val),
                    BinaryOp::Sub => builder.ins().isub(lhs_val, rhs_val),
                    BinaryOp::Mul => builder.ins().imul(lhs_val, rhs_val),
                    BinaryOp::Div => builder.ins().sdiv(lhs_val, rhs_val),
                    BinaryOp::Eq => builder.ins().icmp(IntCC::Equal, lhs_val, rhs_val),
                    BinaryOp::NotEq => builder.ins().icmp(IntCC::NotEqual, lhs_val, rhs_val),
                };

                Ok(result)
            }

            HirExprKind::Call(name, args) => {
                let name_str = ctx.pool.str(*name);
                if crate::builtins::lookup(name_str).is_some() {
                    match name_str {
                        "print" => {
                            Self::generate_print_call(builder, args, ctx)?;
                            Ok(builder.ins().iconst(ctx.int_type, 0))
                        }
                        _ => Err(format!(
                            "Builtin '{}' has no codegen implementation",
                            name_str
                        )),
                    }
                } else if let Some(&callee_id) = ctx.func_ids.get(name) {
                    let mut arg_values = Vec::new();
                    for arg in args {
                        let val = Self::eval_expr(builder, arg, ctx)?;
                        arg_values.push(val);
                    }

                    let callee_ref = ctx.module.declare_func_in_func(callee_id, builder.func);
                    let call = builder.ins().call(callee_ref, &arg_values);
                    let results = builder.inst_results(call);

                    if results.is_empty() {
                        Ok(builder.ins().iconst(ctx.int_type, 0))
                    } else {
                        Ok(results[0])
                    }
                } else {
                    Err(format!("Undefined function: '{}'", name_str))
                }
            }
        }
    }

    fn generate_print_call(
        builder: &mut FunctionBuilder,
        args: &[HirExpr],
        ctx: &mut FunctionContext<'_, M>,
    ) -> Result<(), String> {
        // Sema has already validated arity and the string-literal
        // constraint (see `sema::check_builtin_call`). The matches
        // below are therefore infallible.
        debug_assert_eq!(args.len(), 1, "sema should reject print() arity errors");
        let string_id = match &args[0].kind {
            HirExprKind::StrLiteral(id) => *id,
            other => unreachable!(
                "sema should reject non-literal print() args, got {:?}",
                other
            ),
        };
        let string_content = ctx.pool.str(string_id);

        let data_id = Self::store_string(
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

/// Walks the HIR and asserts every expression has a resolved type.
/// Used inside `debug_assert!` at codegen entry points; sema is
/// expected to have filled all types before codegen runs.
fn all_expr_types_resolved(program: &HirProgram) -> bool {
    fn walk(e: &HirExpr) -> bool {
        if e.ty.is_none() {
            return false;
        }
        match &e.kind {
            HirExprKind::BinaryOp(l, _, r) => walk(l) && walk(r),
            HirExprKind::UnaryOp(_, s) => walk(s),
            HirExprKind::Call(_, args) => args.iter().all(walk),
            _ => true,
        }
    }
    for func in &program.functions {
        for stmt in &func.body {
            let ok = match stmt {
                HirStmt::VarDecl { initializer, .. } => walk(initializer),
                HirStmt::Return(Some(e), _) => walk(e),
                HirStmt::Return(None, _) => true,
                HirStmt::Expr(e, _) => walk(e),
            };
            if !ok {
                return false;
            }
        }
    }
    true
}
