//! Code generation module using Cranelift
//!
//! This module translates Ryo AST into Cranelift IR and generates native object files
//! for linking with system libraries.
//!
//! # Architecture
//!
//! The code generation pipeline consists of:
//! 1. **AST → Cranelift IR**: Translate abstract syntax tree to intermediate representation
//! 2. **IR → Machine Code**: Cranelift compiles IR to target-specific machine code
//! 3. **Object File**: Package machine code into linkable object file format
//!
//! # Current Implementation (Milestone 3)
//!
//! - **AOT Only**: Ahead-of-time compilation to object files (no JIT yet)
//! - **Single Function**: Generates one `main` function per program
//! - **Integer Arithmetic**: Supports integer literals and basic arithmetic operations
//! - **No Variable Storage**: Variables are evaluated but not stored (will be added in Milestone 4)
//!
//! # Example Usage
//!
//! ```ignore
//! use target_lexicon::Triple;
//! use ryo::codegen::Codegen;
//!
//! // Create codegen for host architecture
//! let mut codegen = Codegen::new(Triple::host())?;
//!
//! // Compile a program
//! let program = /* ... parsed AST ... */;
//! codegen.compile(program)?;
//!
//! // Get object file bytes
//! let obj_bytes = codegen.finish()?;
//!
//! // Write to file
//! std::fs::write("program.o", obj_bytes)?;
//! ```
//!
//! # Generated IR Example
//!
//! For Ryo code: `x = 2 + 3 * 4`
//!
//! Generated Cranelift IR (conceptual):
//! ```text
//! function main() -> i64 {
//! block0:
//!     v0 = iconst.i64 2
//!     v1 = iconst.i64 3
//!     v2 = iconst.i64 4
//!     v3 = imul v1, v2      ; 3 * 4 = 12
//!     v4 = iadd v0, v3      ; 2 + 12 = 14
//!     return v4
//! }
//! ```
//!
//! # Design Decisions
//!
//! ## Position-Independent Code (PIC)
//!
//! We enable PIC for all generated code because:
//! - **Portability**: Code can be loaded at any memory address
//! - **Shared Libraries**: Required for future dynamic linking support
//! - **Modern Practice**: Standard for contemporary compilers
//! - **Minor Overhead**: Small performance cost, worth the flexibility
//!
//! ## Target Integer Type
//!
//! Uses pointer-sized integers for all integer operations:
//! - **64-bit systems**: i64
//! - **32-bit systems**: i32
//!
//! This ensures integers can hold any address and provides natural word-size operations.
//!
//! ## Empty Program Handling
//!
//! Empty programs (no statements) return 0 as exit code:
//! - Consistent with Unix convention (0 = success)
//! - Prevents undefined behavior
//! - Simple implementation
//!
//! ## No Bounds Checking
//!
//! Current Milestone 3 limitations:
//! - Division by zero: Not checked (undefined behavior, will crash)
//! - Integer overflow: Wraps (standard two's complement)
//! - Will be addressed with error handling in future milestones
//!
//! # Future Evolution
//!
//! - **Milestone 4**: Multiple functions, local variables with stack allocation
//! - **Milestone 5+**: Optimizations, JIT mode for REPL, more types
//! - **Long-term**: Full optimization pipeline, debugging info, profiling support

use crate::ast::{BinaryOperator, Expression, ExprKind, Literal, Program, UnaryOperator};
use cranelift::codegen::isa;
use cranelift::codegen::settings::{self, Configurable};
use cranelift::prelude::*;
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use target_lexicon::Triple;

/// Code generator for Ryo programs using Cranelift.
///
/// This struct encapsulates all state needed for code generation:
/// - Cranelift compilation contexts
/// - Object file builder
/// - Target-specific configuration
///
/// # Fields
///
/// - `builder_context`: Reusable context for FunctionBuilder (avoids allocations)
/// - `ctx`: Function compilation context (stores IR being built)
/// - `module`: Object file builder (AOT compilation target)
/// - `int_type`: Target's native integer type (i64 on 64-bit, i32 on 32-bit)
pub struct Codegen {
    builder_context: FunctionBuilderContext,
    ctx: codegen::Context,
    module: ObjectModule,
    int_type: Type, // Store the integer type based on target
}

impl Codegen {
    /// Creates a new Codegen instance for the given target architecture.
    ///
    /// This initializes the entire code generation pipeline:
    /// 1. Looks up the ISA (Instruction Set Architecture) for the target
    /// 2. Configures compilation flags (enables PIC)
    /// 3. Creates the object file builder
    /// 4. Determines the native integer type for the target
    ///
    /// # Arguments
    ///
    /// * `target_triple` - The target architecture (e.g., `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu`)
    ///
    /// # Returns
    ///
    /// * `Ok(Codegen)` - Initialized code generator ready to compile programs
    /// * `Err(String)` - Error message if target is unsupported or configuration fails
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - The target triple is not supported by Cranelift
    /// - PIC (Position Independent Code) cannot be enabled
    /// - The ISA cannot be constructed with the given settings
    /// - The object builder cannot be created
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use target_lexicon::Triple;
    /// use ryo::codegen::Codegen;
    ///
    /// // Create codegen for the host architecture
    /// let codegen = Codegen::new(Triple::host())?;
    ///
    /// // Create codegen for a specific target
    /// let triple = "x86_64-unknown-linux-gnu".parse().unwrap();
    /// let codegen = Codegen::new(triple)?;
    /// ```
    pub fn new(target_triple: Triple) -> Result<Self, String> {
        // Step 1: Configure Cranelift settings
        // We enable PIC (Position Independent Code) for all generated code.
        // This allows the code to be loaded at any memory address, which is required
        // for shared libraries and is standard practice for modern compilers.
        let mut shared_builder = settings::builder();
        shared_builder
            .enable("is_pic")
            .map_err(|e| format!("Error enabling is_pic: {}", e))?;
        let shared_flags = settings::Flags::new(shared_builder);

        // Step 2: Look up the ISA (Instruction Set Architecture)
        // This determines what assembly instructions we can generate for the target.
        // For example: x86_64, aarch64 (ARM64), etc.
        let isa = isa::lookup(target_triple.clone())
            .map_err(|e| format!("Unsupported target '{}': {}", target_triple, e))?
            .finish(shared_flags)
            .map_err(|e| format!("Failed to build ISA: {}", e))?;

        // Step 3: Create the ObjectBuilder
        // This is responsible for generating the actual object file format (.o or .obj)
        // that can be linked with system libraries to create an executable.
        let obj_builder = ObjectBuilder::new(
            isa,          // The ISA determines instruction encoding
            "ryo_module", // Module name (appears in debug info, not critical for now)
            cranelift_module::default_libcall_names(), // Standard C library function names
        )
        .map_err(|e| format!("Failed to create ObjectBuilder: {}", e))?;

        // Step 4: Create the ObjectModule and determine integer type
        let module = ObjectModule::new(obj_builder);
        // Use the target's pointer size as our integer type:
        // - 64-bit platforms (x86_64, aarch64): i64
        // - 32-bit platforms (x86, arm): i32
        // This ensures integers can hold any memory address.
        let int_type = module.target_config().pointer_type();

        Ok(Self {
            builder_context: FunctionBuilderContext::new(),
            ctx: module.make_context(),
            module,
            int_type,
        })
    }

    /// Compiles a Ryo program into Cranelift IR and prepares it for object file generation.
    ///
    /// This method translates a parsed Ryo AST into Cranelift's intermediate representation,
    /// then defines a `main` function that will be exported in the object file.
    ///
    /// # Current Behavior (Milestone 3)
    ///
    /// The generated `main` function:
    /// - Takes no parameters
    /// - Returns a pointer-sized integer (i64 on 64-bit, i32 on 32-bit)
    /// - Evaluates all variable declarations in order
    /// - Returns the value of the last initializer expression as the exit code
    ///
    /// # Arguments
    ///
    /// * `program` - The parsed AST representing the Ryo source code
    ///
    /// # Returns
    ///
    /// * `Ok(FuncId)` - Function identifier for the generated `main` function
    /// * `Err(String)` - Error message if compilation fails
    ///
    /// # Errors
    ///
    /// This function will return an error if:
    /// - Function declaration fails (name collision, invalid signature)
    /// - AST translation to IR fails (unsupported expression, type error)
    /// - Function definition fails (invalid IR, verification error)
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use ryo::codegen::Codegen;
    /// use ryo::ast::Program;
    /// use target_lexicon::Triple;
    ///
    /// let mut codegen = Codegen::new(Triple::host())?;
    /// let program = /* parsed from "x = 42" */;
    /// let func_id = codegen.compile(program)?;
    /// let obj_bytes = codegen.finish()?;
    /// ```
    pub fn compile(&mut self, program: Program) -> Result<FuncId, String> {
        // Step 1: Create function signature for `main`
        // Signature: () -> int_type
        // The return type is the target's pointer-sized integer (i64 or i32)
        let sig = {
            let mut sig = self.module.make_signature();
            sig.returns.push(AbiParam::new(self.int_type));
            sig
        };

        // Step 2: Declare the function in the module
        // `Linkage::Export` makes this symbol visible to the linker,
        // allowing it to be used as the program's entry point
        let func_id = self
            .module
            .declare_function("main", Linkage::Export, &sig)
            .map_err(|e| format!("Failed to declare function: {}", e))?;

        // Step 3: Set up the compilation context with the signature
        self.ctx.func.signature = sig;

        // Step 4: Build the function body
        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
            let int_type = self.int_type;

            // Translate the AST into Cranelift IR instructions
            Self::translate(&mut builder, program, int_type)?;

            // Finalize the function (verifies IR is well-formed)
            builder.finalize();
        }

        // Step 5: Define the function in the module
        // This completes the compilation and makes the function available
        // for object file generation
        self.module
            .define_function(func_id, &mut self.ctx)
            .map_err(|e| format!("Failed to define function: {}", e))?;

        // Step 6: Clear the context for potential reuse
        // (Important if compiling multiple functions in the future)
        self.ctx.clear();

        Ok(func_id)
    }

    /// Translates a Ryo AST program into Cranelift IR instructions.
    ///
    /// This method is the core of the code generation process. It walks through the AST
    /// and emits corresponding Cranelift IR instructions into the function body.
    ///
    /// # Current Behavior (Milestone 3)
    ///
    /// - Creates a single basic block (entry block)
    /// - Evaluates each variable declaration in sequence
    /// - Returns the value of the last initializer expression
    /// - Empty programs return 0
    ///
    /// # Arguments
    ///
    /// * `builder` - Cranelift's FunctionBuilder for emitting IR instructions
    /// * `program` - The parsed AST program to translate
    /// * `int_type` - The target's integer type (i64 or i32)
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Translation succeeded
    /// * `Err(String)` - Error message if translation fails
    ///
    /// # Implementation Notes
    ///
    /// Currently, variable declarations are **evaluated but not stored**. This means:
    /// - `x = 42` evaluates to 42 but `x` is not accessible later
    /// - Only the last expression's value is kept and returned
    /// - This limitation will be removed in Milestone 4 (Functions & Variables)
    ///
    /// # Future Extensions (Milestone 4+)
    ///
    /// - Symbol table for storing variable values on the stack
    /// - Multiple functions with local scopes
    /// - Control flow (if/else, loops) requiring multiple basic blocks
    fn translate(builder: &mut FunctionBuilder, program: Program, int_type: Type) -> Result<(), String> {
        // Step 1: Create and set up the entry basic block
        // In Cranelift, all code must be in basic blocks. For simple programs,
        // we only need one block. Control flow (if/loops) will require multiple blocks.
        let entry_block = builder.create_block();
        builder.switch_to_block(entry_block);

        // Sealing the block tells Cranelift that no other blocks will jump here.
        // This enables optimizations and is required before finalization.
        builder.seal_block(entry_block);

        // Step 2: Handle empty programs
        // An empty program (no statements) returns 0 as the exit code.
        // This is consistent with Unix convention: 0 = success.
        if program.statements.is_empty() {
            let zero = builder.ins().iconst(int_type, 0);
            builder.ins().return_(&[zero]);
            return Ok(());
        }

        // Step 3: Evaluate each statement
        // Currently, we only support variable declarations (e.g., `x = 42`).
        // We evaluate the initializer expression and keep track of the last value.
        // NOTE: Variables are NOT stored yet - they're just evaluated.
        // The last evaluated value becomes the program's return value.
        let mut result_val = None;
        for stmt in program.statements {
            // Extract the variable declaration from the statement
            let val = Self::eval_expr(builder, &stmt.kind.as_var_decl().initializer, int_type)?;
            result_val = Some(val);
        }

        // Step 4: Return the last value (or 0 if no statements produced a value)
        let return_val = result_val.unwrap_or_else(|| builder.ins().iconst(int_type, 0));
        builder.ins().return_(&[return_val]);

        Ok(())
    }

    /// Recursively evaluates an expression into a Cranelift SSA value.
    ///
    /// This method is the heart of expression translation. It walks the AST recursively
    /// and emits Cranelift instructions that compute the expression's value.
    ///
    /// # SSA Form
    ///
    /// Cranelift uses Static Single Assignment (SSA) form, where:
    /// - Each value is assigned exactly once
    /// - All values are represented by `Value` objects (virtual registers)
    /// - Operations produce new values rather than modifying existing ones
    ///
    /// For example, `2 + 3 * 4` generates:
    /// ```text
    /// v0 = iconst.i64 2      ; Load constant 2
    /// v1 = iconst.i64 3      ; Load constant 3
    /// v2 = iconst.i64 4      ; Load constant 4
    /// v3 = imul v1, v2       ; v3 = 3 * 4 = 12
    /// v4 = iadd v0, v3       ; v4 = 2 + 12 = 14
    /// ```
    ///
    /// # Arguments
    ///
    /// * `builder` - Cranelift's FunctionBuilder for emitting instructions
    /// * `expr` - The expression AST node to evaluate
    /// * `int_type` - The target's integer type (i64 or i32)
    ///
    /// # Returns
    ///
    /// * `Ok(Value)` - A Cranelift value (SSA virtual register) containing the result
    /// * `Err(String)` - Error message if the expression cannot be evaluated
    ///
    /// # Supported Expressions (Milestone 3)
    ///
    /// - **Literals**: `42`, `123`
    /// - **Unary Operations**: `-x` (negation)
    /// - **Binary Operations**: `+`, `-`, `*`, `/` (all signed integer operations)
    ///
    /// # Safety Notes
    ///
    /// - **Division by zero**: Not checked, will cause undefined behavior (crash)
    /// - **Integer overflow**: Wraps (standard two's complement behavior)
    /// - These will be addressed with error handling in future milestones
    fn eval_expr(builder: &mut FunctionBuilder, expr: &Expression, int_type: Type) -> Result<Value, String> {
        match &expr.kind {
            // Integer literals: Load constant value
            // Example: `42` → iconst.i64 42
            ExprKind::Literal(Literal::Int(val)) => {
                Ok(builder.ins().iconst(int_type, *val as i64))
            }

            // Unary negation: Negate the subexpression
            // Example: `-5` → iconst.i64 5, then ineg
            // This performs two's complement negation: -x = ~x + 1
            ExprKind::UnaryOp(UnaryOperator::Neg, sub_expr) => {
                let sub_val = Self::eval_expr(builder, sub_expr, int_type)?;
                Ok(builder.ins().ineg(sub_val))
            }

            // Binary operations: Evaluate both sides, then apply operator
            // Example: `2 + 3` → iconst 2, iconst 3, iadd
            ExprKind::BinaryOp(lhs, op, rhs) => {
                // Recursively evaluate left and right operands
                let lhs_val = Self::eval_expr(builder, lhs, int_type)?;
                let rhs_val = Self::eval_expr(builder, rhs, int_type)?;

                // Emit the appropriate instruction based on operator
                let result = match op {
                    BinaryOperator::Add => builder.ins().iadd(lhs_val, rhs_val), // Signed integer addition
                    BinaryOperator::Sub => builder.ins().isub(lhs_val, rhs_val), // Signed integer subtraction
                    BinaryOperator::Mul => builder.ins().imul(lhs_val, rhs_val), // Signed integer multiplication
                    BinaryOperator::Div => builder.ins().sdiv(lhs_val, rhs_val), // Signed integer division (rounds toward zero)
                };

                Ok(result)
            }
        }
    }

    /// Finalizes the module and emits the complete object file as raw bytes.
    ///
    /// This method consumes the `Codegen` instance and produces the final object file
    /// in the platform-specific format (ELF on Linux, Mach-O on macOS, COFF on Windows).
    ///
    /// # Object File Contents
    ///
    /// The generated object file contains:
    /// - **Compiled machine code** for all defined functions (currently just `main`)
    /// - **Symbol table** with exported symbols (`main` with external linkage)
    /// - **Relocation information** for linking with other object files and libraries
    /// - **Platform-specific headers** (ELF, Mach-O, or COFF)
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<u8>)` - Raw bytes of the object file, ready to be written to disk
    /// * `Err(String)` - Error message if object file emission fails
    ///
    /// # Usage
    ///
    /// After getting the object file bytes, you can:
    /// 1. Write to a file: `std::fs::write("program.o", obj_bytes)?`
    /// 2. Pass to a linker: `zig cc program.o -o program`
    /// 3. Inspect with tools: `objdump -d program.o` or `otool -tV program.o`
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use ryo::codegen::Codegen;
    /// use target_lexicon::Triple;
    ///
    /// let mut codegen = Codegen::new(Triple::host())?;
    /// codegen.compile(program)?;
    ///
    /// // Finalize and get object file bytes
    /// let obj_bytes = codegen.finish()?;
    ///
    /// // Write to disk
    /// std::fs::write("program.o", obj_bytes)?;
    /// ```
    ///
    /// # Note
    ///
    /// This method **consumes** the `Codegen` instance (takes `self`, not `&mut self`).
    /// You cannot use the same `Codegen` instance after calling `finish()`.
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
