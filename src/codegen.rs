use crate::hir;
use crate::hir::{BinaryOp, HirExpr, HirExprKind, HirFunction, HirProgram, HirStmt, UnaryOp};
use cranelift::codegen::isa;
use cranelift::codegen::settings::{self, Configurable};
use cranelift::prelude::*;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
use target_lexicon::Triple;

pub struct Codegen {
    builder_context: FunctionBuilderContext,
    ctx: codegen::Context,
    module: ObjectModule,
    int_type: types::Type,
    data_ctx: DataDescription,
    string_data: HashMap<String, DataId>,
    triple: Triple,
}

impl Codegen {
    pub fn new(target_triple: Triple) -> Result<Self, String> {
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

        let module = ObjectModule::new(obj_builder);
        let int_type = module.target_config().pointer_type();

        Ok(Self {
            builder_context: FunctionBuilderContext::new(),
            ctx: module.make_context(),
            module,
            int_type,
            data_ctx: DataDescription::new(),
            string_data: HashMap::new(),
            triple: target_triple,
        })
    }

    pub fn compile(&mut self, program: &HirProgram) -> Result<FuncId, String> {
        let mut func_ids: HashMap<String, FuncId> = HashMap::new();

        for func in &program.functions {
            let sig = self.build_signature(func);
            let linkage = if func.name == "main" {
                Linkage::Export
            } else {
                Linkage::Local
            };
            let func_id = self
                .module
                .declare_function(&func.name, linkage, &sig)
                .map_err(|e| format!("Failed to declare function '{}': {}", func.name, e))?;
            func_ids.insert(func.name.clone(), func_id);
        }

        for func in &program.functions {
            self.compile_function(func, &func_ids)?;
        }

        func_ids
            .get("main")
            .copied()
            .ok_or_else(|| "No main function defined".to_string())
    }

    fn build_signature(&self, func: &HirFunction) -> Signature {
        let mut sig = self.module.make_signature();
        for _ in &func.params {
            sig.params.push(AbiParam::new(self.int_type));
        }
        if func.return_type != hir::Type::Void {
            sig.returns.push(AbiParam::new(self.int_type));
        }
        sig
    }

    fn compile_function(
        &mut self,
        func: &HirFunction,
        func_ids: &HashMap<String, FuncId>,
    ) -> Result<(), String> {
        let func_id = *func_ids
            .get(&func.name)
            .ok_or_else(|| format!("Function '{}' not declared", func.name))?;

        self.ctx.func.signature = self.build_signature(func);

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);

            let int_type = self.int_type;
            let mut locals: HashMap<String, Variable> = HashMap::new();

            for (i, param) in func.params.iter().enumerate() {
                let var = builder.declare_var(int_type);
                let param_val = builder.block_params(entry_block)[i];
                builder.def_var(var, param_val);
                locals.insert(param.name.clone(), var);
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
                        let val = Self::eval_expr(
                            &mut builder,
                            initializer,
                            &mut self.module,
                            &mut self.data_ctx,
                            &mut self.string_data,
                            int_type,
                            &self.triple,
                            &locals,
                            func_ids,
                        )?;
                        let var = builder.declare_var(int_type);
                        builder.def_var(var, val);
                        locals.insert(name.clone(), var);
                    }
                    HirStmt::Return(Some(expr), _) => {
                        let val = Self::eval_expr(
                            &mut builder,
                            expr,
                            &mut self.module,
                            &mut self.data_ctx,
                            &mut self.string_data,
                            int_type,
                            &self.triple,
                            &locals,
                            func_ids,
                        )?;
                        builder.ins().return_(&[val]);
                        has_return = true;
                    }
                    HirStmt::Return(None, _) => {
                        builder.ins().return_(&[]);
                        has_return = true;
                    }
                    HirStmt::Expr(expr, _) => {
                        let _val = Self::eval_expr(
                            &mut builder,
                            expr,
                            &mut self.module,
                            &mut self.data_ctx,
                            &mut self.string_data,
                            int_type,
                            &self.triple,
                            &locals,
                            func_ids,
                        )?;
                    }
                }
            }

            if !has_return {
                if func.return_type != hir::Type::Void {
                    let zero = builder.ins().iconst(int_type, 0);
                    builder.ins().return_(&[zero]);
                } else {
                    builder.ins().return_(&[]);
                }
            }

            builder.finalize();
        }

        self.module
            .define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Failed to define function '{}': {}", func.name, e))?;

        self.ctx.clear();
        Ok(())
    }

    fn store_string(
        content: &str,
        module: &mut ObjectModule,
        data_ctx: &mut DataDescription,
        string_data: &mut HashMap<String, DataId>,
    ) -> Result<DataId, String> {
        if let Some(&data_id) = string_data.get(content) {
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

        string_data.insert(content.to_string(), data_id);
        Ok(data_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_expr(
        builder: &mut FunctionBuilder,
        expr: &HirExpr,
        module: &mut ObjectModule,
        data_ctx: &mut DataDescription,
        string_data: &mut HashMap<String, DataId>,
        int_type: types::Type,
        triple: &Triple,
        locals: &HashMap<String, Variable>,
        func_ids: &HashMap<String, FuncId>,
    ) -> Result<Value, String> {
        match &expr.kind {
            HirExprKind::IntLiteral(val) => Ok(builder.ins().iconst(int_type, *val as i64)),

            HirExprKind::StrLiteral(content) => {
                let data_id = Self::store_string(content, module, data_ctx, string_data)?;
                let data_ref = module.declare_data_in_func(data_id, builder.func);
                let ptr = builder.ins().global_value(int_type, data_ref);
                Ok(ptr)
            }

            HirExprKind::Var(name) => {
                let var = locals
                    .get(name.as_str())
                    .ok_or_else(|| format!("Undefined variable: '{}'", name))?;
                Ok(builder.use_var(*var))
            }

            HirExprKind::UnaryOp(UnaryOp::Neg, sub_expr) => {
                let sub_val = Self::eval_expr(
                    builder,
                    sub_expr,
                    module,
                    data_ctx,
                    string_data,
                    int_type,
                    triple,
                    locals,
                    func_ids,
                )?;
                Ok(builder.ins().ineg(sub_val))
            }

            HirExprKind::BinaryOp(lhs, op, rhs) => {
                let lhs_val = Self::eval_expr(
                    builder,
                    lhs,
                    module,
                    data_ctx,
                    string_data,
                    int_type,
                    triple,
                    locals,
                    func_ids,
                )?;
                let rhs_val = Self::eval_expr(
                    builder,
                    rhs,
                    module,
                    data_ctx,
                    string_data,
                    int_type,
                    triple,
                    locals,
                    func_ids,
                )?;

                let result = match op {
                    BinaryOp::Add => builder.ins().iadd(lhs_val, rhs_val),
                    BinaryOp::Sub => builder.ins().isub(lhs_val, rhs_val),
                    BinaryOp::Mul => builder.ins().imul(lhs_val, rhs_val),
                    BinaryOp::Div => builder.ins().sdiv(lhs_val, rhs_val),
                };

                Ok(result)
            }

            HirExprKind::Call(name, args) => {
                if name == "print" {
                    Self::generate_print_call(
                        builder,
                        args,
                        module,
                        data_ctx,
                        string_data,
                        int_type,
                        triple,
                    )?;
                    Ok(builder.ins().iconst(int_type, 0))
                } else if let Some(&callee_id) = func_ids.get(name.as_str()) {
                    let mut arg_values = Vec::new();
                    for arg in args {
                        let val = Self::eval_expr(
                            builder,
                            arg,
                            module,
                            data_ctx,
                            string_data,
                            int_type,
                            triple,
                            locals,
                            func_ids,
                        )?;
                        arg_values.push(val);
                    }

                    let callee_ref = module.declare_func_in_func(callee_id, builder.func);
                    let call = builder.ins().call(callee_ref, &arg_values);
                    let results = builder.inst_results(call);

                    if results.is_empty() {
                        Ok(builder.ins().iconst(int_type, 0))
                    } else {
                        Ok(results[0])
                    }
                } else {
                    Err(format!("Undefined function: '{}'", name))
                }
            }
        }
    }

    fn generate_print_call(
        builder: &mut FunctionBuilder,
        args: &[HirExpr],
        module: &mut ObjectModule,
        data_ctx: &mut DataDescription,
        string_data: &mut HashMap<String, DataId>,
        int_type: types::Type,
        triple: &Triple,
    ) -> Result<(), String> {
        if args.len() != 1 {
            return Err(format!(
                "print() takes exactly 1 argument, got {}",
                args.len()
            ));
        }

        let arg = &args[0];
        let string_content = match &arg.kind {
            HirExprKind::StrLiteral(content) => content,
            _ => return Err("print() argument must be a string literal".to_string()),
        };

        let data_id = Self::store_string(string_content, module, data_ctx, string_data)?;
        let data_ref = module.declare_data_in_func(data_id, builder.func);
        let string_ptr = builder.ins().global_value(int_type, data_ref);

        let string_len = builder.ins().iconst(int_type, string_content.len() as i64);
        let fd = builder.ins().iconst(int_type, 1);

        use target_lexicon::OperatingSystem;
        match triple.operating_system {
            OperatingSystem::Darwin { .. }
            | OperatingSystem::MacOSX { .. }
            | OperatingSystem::Linux => {}
            _ => {
                return Err(format!(
                    "print() not yet supported on platform: {:?}",
                    triple.operating_system
                ));
            }
        }

        let mut write_sig = module.make_signature();
        write_sig.params.push(AbiParam::new(int_type));
        write_sig.params.push(AbiParam::new(int_type));
        write_sig.params.push(AbiParam::new(int_type));
        write_sig.returns.push(AbiParam::new(int_type));

        let write_func = module
            .declare_function("write", Linkage::Import, &write_sig)
            .map_err(|e| format!("Failed to declare write function: {}", e))?;

        let write_ref = module.declare_func_in_func(write_func, builder.func);
        let call_inst = builder.ins().call(write_ref, &[fd, string_ptr, string_len]);
        let _bytes_written = builder.inst_results(call_inst)[0];

        Ok(())
    }

    pub fn finish(self) -> Result<Vec<u8>, String> {
        self.module
            .finish()
            .emit()
            .map_err(|e| format!("Failed to emit object file: {}", e))
    }
}
