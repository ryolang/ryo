// TODO: Update for new AST (Program, Statement, Expression) after Milestone 2
// use crate::ast::Expr;
use cranelift::codegen::isa;
use cranelift::codegen::settings::{self, Configurable};
use cranelift::prelude::*;
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use target_lexicon::Triple;

/// The main code generation structure for creating object files.
pub struct Codegen {
    builder_context: FunctionBuilderContext,
    ctx: codegen::Context,
    module: ObjectModule,
    int_type: Type, // Store the integer type based on target
}

impl Codegen {
    /// Creates a new Codegen instance for the given target architecture.
    pub fn new(target_triple: Triple) -> Result<Self, String> {
        // Look up the ISA based on the target triple.
        // Configure settings: enable Position Independent Code (PIC)
        let mut shared_builder = settings::builder();
        shared_builder
            .enable("is_pic")
            .map_err(|e| format!("Error enabling is_pic: {}", e))?;
        let shared_flags = settings::Flags::new(shared_builder);
        let isa = isa::lookup(target_triple.clone())
            .map_err(|e| format!("Unsupported target '{}': {}", target_triple, e))?
            .finish(shared_flags)
            .map_err(|e| format!("Failed to build ISA: {}", e))?;

        // Create the ObjectBuilder using the ISA
        let obj_builder = ObjectBuilder::new(
            isa,          // Pass the constructed ISA here
            "ryo_module", // TODO: Make the module name configurable or derive from input
            cranelift_module::default_libcall_names(),
        )
        .map_err(|e| format!("Failed to create ObjectBuilder: {}", e))?;

        let module = ObjectModule::new(obj_builder);
        let int_type = module.target_config().pointer_type(); // Use target's pointer size for integers

        Ok(Self {
            builder_context: FunctionBuilderContext::new(),
            ctx: module.make_context(),
            module,
            int_type,
        })
    }

    // TODO: Update compile, translate, build_expr methods for new AST after Milestone 2
    // /// Compiles the provided expression into the main function of the module.
    // pub fn compile(&mut self, input: Expr) -> Result<FuncId, String> {
    //     let sig = {
    //         let mut sig = self.module.make_signature();
    //         sig.returns.push(AbiParam::new(self.int_type));
    //         sig
    //     };
    //
    //     let func_id = self
    //         .module
    //         .declare_function("main", Linkage::Export, &sig)
    //         .map_err(|e| format!("Failed to declare function: {}", e))?;
    //
    //     self.ctx.func.signature = sig;
    //
    //     {
    //         let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
    //         let int_type = self.int_type; // Get int_type before borrowing self mutably via builder
    //
    //         // Pass int_type explicitly, don't borrow self
    //         Self::translate(&mut builder, input, int_type)?;
    //
    //         builder.finalize(); // Finalize the builder before the borrow of self.ctx.func ends
    //     }
    //
    //     // Define the function body using the populated context
    //     self.module
    //         .define_function(func_id, &mut self.ctx)
    //         .map_err(|e| format!("Failed to define function: {}", e))?;
    //
    //     self.ctx.clear(); // Clear context for next function
    //
    //     Ok(func_id)
    // }
    //
    // /// Translates an expression AST into Cranelift IR using the provided FunctionBuilder.
    // // Does not take self, receives int_type explicitly
    // fn translate(builder: &mut FunctionBuilder, expr: Expr, int_type: Type) -> Result<(), String> {
    //     let entry_block = builder.create_block();
    //     builder.switch_to_block(entry_block);
    //     builder.seal_block(entry_block);
    //
    //     // Build the IR for the expression, passing int_type
    //     let result_val = Self::build_expr(builder, expr, int_type)?;
    //
    //     builder.ins().return_(&[result_val]);
    //     // Finalization is handled in compile after this function returns
    //     Ok(())
    // }
    //
    // /// Recursively builds Cranelift IR for an expression.
    // // Does not take self, receives int_type explicitly
    // fn build_expr(
    //     builder: &mut FunctionBuilder,
    //     expr: Expr,
    //     int_type: Type,
    // ) -> Result<Value, String> {
    //     match expr {
    //         Expr::Int(val) => {
    //             // Use passed int_type
    //             Ok(builder.ins().iconst(int_type, val as i64))
    //         }
    //         Expr::Neg(sub_expr) => {
    //             // Pass int_type down recursively
    //             let sub_val = Self::build_expr(builder, *sub_expr, int_type)?;
    //             Ok(builder.ins().ineg(sub_val))
    //         }
    //         Expr::Add(lhs, rhs) => {
    //             let lhs_val = Self::build_expr(builder, *lhs, int_type)?;
    //             let rhs_val = Self::build_expr(builder, *rhs, int_type)?;
    //             Ok(builder.ins().iadd(lhs_val, rhs_val))
    //         }
    //         Expr::Sub(lhs, rhs) => {
    //             let lhs_val = Self::build_expr(builder, *lhs, int_type)?;
    //             let rhs_val = Self::build_expr(builder, *rhs, int_type)?;
    //             Ok(builder.ins().isub(lhs_val, rhs_val))
    //         }
    //         Expr::Mul(lhs, rhs) => {
    //             let lhs_val = Self::build_expr(builder, *lhs, int_type)?;
    //             let rhs_val = Self::build_expr(builder, *rhs, int_type)?;
    //             Ok(builder.ins().imul(lhs_val, rhs_val))
    //         }
    //         Expr::Div(lhs, rhs) => {
    //             let lhs_val = Self::build_expr(builder, *lhs, int_type)?;
    //             let rhs_val = Self::build_expr(builder, *rhs, int_type)?;
    //             Ok(builder.ins().sdiv(lhs_val, rhs_val))
    //         }
    //     }
    // }

    /// Finalizes the module and returns the raw object file bytes.
    pub fn finish(self) -> Result<Vec<u8>, String> {
        self.module
            .finish()
            .emit()
            .map_err(|e| format!("Failed to emit object file: {}", e))
    }
}

// TODO: Add tests for the Codegen module.
// Example usage would involve:
// 1. Get target triple (e.g., using `Triple::host()`).
// 2. Create `Codegen::new(triple)`.
// 3. Call `codegen.compile(expr)`.
// 4. Call `codegen.finish()` to get bytes.
// 5. Write bytes to a `.o` file using std::fs::write.
