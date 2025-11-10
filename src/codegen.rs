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
//! ## Exit Code Handling
//!
//! All programs currently exit with code 0 (success). This matches industry standards
//! where successful programs return 0 by convention.
//!
//! **Design Decision:** Previous versions (Milestone 3.0) used the last expression's value
//! as the exit code, but this was changed because:
//! - No other mainstream language uses this pattern
//! - Confusing and error-prone (assignments look like exit codes)
//! - Incompatible with future features (functions, types, error handling)
//! - Violates "explicit is better than implicit" principle
//!
//! **Current Behavior (Milestone 3.1+):**
//! ```ryo
//! x = 42    # Program exits with code 0 (not 42)
//! y = 100   # Still exits with code 0
//! ```
//!
//! **Future (Milestone 4+):** Explicit exit codes will be supported via return statements:
//! ```ignore
//! fn main() -> int:
//!     if error_condition:
//!         return 1    // Error
//!     return 0        // Success
//! ```
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
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
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
/// - `data_ctx`: Context for defining data objects (strings, constants)
/// - `string_data`: Maps string content to DataId for deduplication
/// - `triple`: Target platform triple (for platform-specific code generation)
pub struct Codegen {
    builder_context: FunctionBuilderContext,
    ctx: codegen::Context,
    module: ObjectModule,
    int_type: Type, // Store the integer type based on target
    data_ctx: DataDescription,
    string_data: HashMap<String, DataId>,
    triple: Triple,
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
            data_ctx: DataDescription::new(),
            string_data: HashMap::new(),
            triple: target_triple,
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
            let triple = self.triple.clone();

            // Translate the AST into Cranelift IR instructions
            Self::translate_impl(
                &mut builder,
                program,
                &mut self.module,
                &mut self.data_ctx,
                &mut self.string_data,
                int_type,
                triple,
            )?;

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
    /// # Current Behavior (Milestone 3.5)
    ///
    /// - Creates a single basic block (entry block)
    /// - Evaluates each variable declaration in sequence
    /// - Handles string literals and print() calls
    /// - Returns 0 (success) as exit code
    /// - Empty programs return 0
    ///
    /// # Arguments
    ///
    /// * `builder` - Cranelift's FunctionBuilder for emitting IR instructions
    /// * `program` - The parsed AST program to translate
    /// * `module` - Object module for declaring data/functions
    /// * `data_ctx` - Data context for string storage
    /// * `string_data` - Cache for string deduplication
    /// * `int_type` - Target integer type
    /// * `triple` - Target platform triple
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
    fn translate_impl(
        builder: &mut FunctionBuilder,
        program: Program,
        module: &mut ObjectModule,
        data_ctx: &mut DataDescription,
        string_data: &mut HashMap<String, DataId>,
        int_type: Type,
        triple: Triple,
    ) -> Result<(), String> {
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
        // We evaluate the initializer expressions for their side effects (though in
        // Milestone 3, expressions have no side effects - this is preparation for future).
        // NOTE: Variables are NOT stored yet - they're just evaluated and discarded.
        for stmt in program.statements {
            // Evaluate the expression but don't use its value
            // The underscore prefix indicates the value is intentionally unused
            let _val = Self::eval_expr(
                builder,
                &stmt.kind.as_var_decl().initializer,
                module,
                data_ctx,
                string_data,
                int_type,
                &triple,
            )?;
        }

        // Step 4: Return 0 (success) by default
        // All programs exit with code 0, following the Unix convention where 0 indicates success.
        // Explicit exit codes will be added in Milestone 4 via return statements.
        // Design Decision: Changed from "last expression value" to "implicit 0" because:
        // - Aligns with industry standards (Rust, Go, Python, C)
        // - Less confusing (assignments don't look like exit codes)
        // - Future-proof for functions and error handling
        let zero = builder.ins().iconst(int_type, 0);
        builder.ins().return_(&[zero]);

        Ok(())
    }

    /// Stores a string literal in the data section and returns its DataId.
    ///
    /// String literals are stored in the `.rodata` section (read-only data) of the object file.
    /// This method implements deduplication: identical strings are only stored once.
    ///
    /// # Arguments
    ///
    /// * `content` - The string content to store
    /// * `module` - The object module
    /// * `data_ctx` - Data context for defining data
    /// * `string_data` - Cache for string deduplication
    ///
    /// # Returns
    ///
    /// * `Ok(DataId)` - Identifier for the stored string data
    /// * `Err(String)` - Error message if storage fails
    fn store_string(
        content: &str,
        module: &mut ObjectModule,
        data_ctx: &mut DataDescription,
        string_data: &mut HashMap<String, DataId>,
    ) -> Result<DataId, String> {
        // Check if we've already stored this string (deduplication)
        if let Some(&data_id) = string_data.get(content) {
            return Ok(data_id);
        }

        // Declare a new data object in the module
        let data_id = module
            .declare_anonymous_data(false, false) // read-only, not thread-local
            .map_err(|e| format!("Failed to declare string data: {}", e))?;

        // Clear any previous data and set up the new string
        data_ctx.clear();

        // Store the string bytes (without null terminator for now)
        let bytes = content.as_bytes();
        data_ctx.define(bytes.into());

        // Define the data object in the module
        module
            .define_data(data_id, data_ctx)
            .map_err(|e| format!("Failed to define string data: {}", e))?;

        // Cache for deduplication
        string_data.insert(content.to_string(), data_id);

        Ok(data_id)
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
    /// * `module` - Object module for declaring data/functions
    /// * `data_ctx` - Data context for string storage
    /// * `string_data` - Cache for string deduplication
    /// * `int_type` - The target's integer type (i64 or i32)
    /// * `triple` - Target platform triple
    ///
    /// # Returns
    ///
    /// * `Ok(Value)` - A Cranelift value (SSA virtual register) containing the result
    /// * `Err(String)` - Error message if the expression cannot be evaluated
    ///
    /// # Supported Expressions (Milestone 3.5)
    ///
    /// - **Literals**: `42`, `123`, `"hello"`
    /// - **Unary Operations**: `-x` (negation)
    /// - **Binary Operations**: `+`, `-`, `*`, `/` (all signed integer operations)
    /// - **Function Calls**: `print("message")` (syscall to stdout)
    ///
    /// # Safety Notes
    ///
    /// - **Division by zero**: Not checked, will cause undefined behavior (crash)
    /// - **Integer overflow**: Wraps (standard two's complement behavior)
    /// - These will be addressed with error handling in future milestones
    fn eval_expr(
        builder: &mut FunctionBuilder,
        expr: &Expression,
        module: &mut ObjectModule,
        data_ctx: &mut DataDescription,
        string_data: &mut HashMap<String, DataId>,
        int_type: Type,
        triple: &Triple,
    ) -> Result<Value, String> {
        match &expr.kind {
            // Integer literals: Load constant value
            // Example: `42` → iconst.i64 42
            ExprKind::Literal(Literal::Int(val)) => {
                Ok(builder.ins().iconst(int_type, *val as i64))
            }

            // String literals: Store in data section and return pointer
            // Example: `"hello"` → address of string in .rodata section
            ExprKind::Literal(Literal::Str(content)) => {
                // Store the string in the data section
                let data_id = Self::store_string(content, module, data_ctx, string_data)?;

                // Get a reference to the data (pointer to the string)
                let data_ref = module.declare_data_in_func(data_id, builder.func);

                // Load the address of the string
                let ptr = builder.ins().global_value(int_type, data_ref);

                Ok(ptr)
            }

            // Unary negation: Negate the subexpression
            // Example: `-5` → iconst.i64 5, then ineg
            // This performs two's complement negation: -x = ~x + 1
            ExprKind::UnaryOp(UnaryOperator::Neg, sub_expr) => {
                let sub_val = Self::eval_expr(builder, sub_expr, module, data_ctx, string_data, int_type, triple)?;
                Ok(builder.ins().ineg(sub_val))
            }

            // Binary operations: Evaluate both sides, then apply operator
            // Example: `2 + 3` → iconst 2, iconst 3, iadd
            ExprKind::BinaryOp(lhs, op, rhs) => {
                // Recursively evaluate left and right operands
                let lhs_val = Self::eval_expr(builder, lhs, module, data_ctx, string_data, int_type, triple)?;
                let rhs_val = Self::eval_expr(builder, rhs, module, data_ctx, string_data, int_type, triple)?;

                // Emit the appropriate instruction based on operator
                let result = match op {
                    BinaryOperator::Add => builder.ins().iadd(lhs_val, rhs_val), // Signed integer addition
                    BinaryOperator::Sub => builder.ins().isub(lhs_val, rhs_val), // Signed integer subtraction
                    BinaryOperator::Mul => builder.ins().imul(lhs_val, rhs_val), // Signed integer multiplication
                    BinaryOperator::Div => builder.ins().sdiv(lhs_val, rhs_val), // Signed integer division (rounds toward zero)
                };

                Ok(result)
            }

            // Function calls: Handle print() with syscall
            ExprKind::Call(name, args) => {
                if name == "print" {
                    Self::generate_print_call(builder, args, module, data_ctx, string_data, int_type, triple)?;

                    // IMPORTANT: print() should return void/unit type (nothing)
                    // Currently returns int(0) as a placeholder until proper void type is implemented.
                    // This value is semantically meaningless and should be ignored.
                    //
                    // Design Note: Aligns with Python's None and Rust's () conventions.
                    // The libc write() call internally returns bytes written, but we discard it
                    // since print() is a side-effect operation with no meaningful return value.
                    //
                    // TODO(Milestone 6 - Type System): Implement proper void/unit type
                    // TODO(Milestone 6): Change signature to: fn print(s: &str) -> void
                    // TODO(Milestone 15+ - Error Handling): Consider: fn print(s: &str) -> io.WriteError!void
                    Ok(builder.ins().iconst(int_type, 0))
                } else {
                    Err(format!("Unknown function: {}", name))
                }
            }
        }
    }

    /// Generates a print() syscall to write a string to stdout.
    ///
    /// This method implements print() using platform-specific syscalls:
    /// - **Linux**: syscall 1 (write)
    /// - **macOS**: syscall 0x2000004 (write)
    /// - **Windows**: Not yet supported (would use WriteFile)
    ///
    /// The syscall signature is: `write(fd, buf, len) -> ssize_t`
    /// where fd=1 is stdout.
    ///
    /// # Arguments
    ///
    /// * `builder` - Cranelift FunctionBuilder for emitting instructions
    /// * `args` - Arguments to print() (must be exactly one string literal)
    /// * `module` - Object module for declaring functions
    /// * `data_ctx` - Data context for string storage
    /// * `string_data` - Cache for string deduplication
    /// * `int_type` - Target integer type
    /// * `triple` - Target platform triple
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Print call generated successfully
    /// * `Err(String)` - Error if validation fails or platform unsupported
    fn generate_print_call(
        builder: &mut FunctionBuilder,
        args: &[Expression],
        module: &mut ObjectModule,
        data_ctx: &mut DataDescription,
        string_data: &mut HashMap<String, DataId>,
        int_type: Type,
        triple: &Triple,
    ) -> Result<(), String> {
        // Validate: print() takes exactly one argument
        if args.len() != 1 {
            return Err(format!("print() takes exactly 1 argument, got {}", args.len()));
        }

        // Extract the string literal argument
        let arg = &args[0];
        let string_content = match &arg.kind {
            ExprKind::Literal(Literal::Str(content)) => content,
            _ => return Err("print() argument must be a string literal".to_string()),
        };

        // Store the string and get its address
        let data_id = Self::store_string(string_content, module, data_ctx, string_data)?;
        let data_ref = module.declare_data_in_func(data_id, builder.func);
        let string_ptr = builder.ins().global_value(int_type, data_ref);

        // String length
        let string_len = builder.ins().iconst(int_type, string_content.len() as i64);

        // File descriptor: 1 = stdout
        let fd = builder.ins().iconst(int_type, 1);

        // Validate platform support
        use target_lexicon::OperatingSystem;
        match triple.operating_system {
            OperatingSystem::Darwin { .. } | OperatingSystem::MacOSX { .. } | OperatingSystem::Linux => {
                // Supported platforms (Darwin is the kernel name for macOS)
            }
            _ => {
                return Err(format!(
                    "print() not yet supported on platform: {:?}",
                    triple.operating_system
                ));
            }
        }

        // Generate the call to write()
        // We use libc's write() function instead of raw syscalls for portability.
        // The linker will resolve this to the system's libc.

        // Declare write as external function: write(fd: i64, buf: *i8, len: i64) -> i64
        let mut write_sig = module.make_signature();
        write_sig.params.push(AbiParam::new(int_type)); // fd
        write_sig.params.push(AbiParam::new(int_type)); // buf (pointer)
        write_sig.params.push(AbiParam::new(int_type)); // len
        write_sig.returns.push(AbiParam::new(int_type)); // return value (bytes written)

        let write_func = module
            .declare_function("write", Linkage::Import, &write_sig)
            .map_err(|e| format!("Failed to declare write function: {}", e))?;

        let write_ref = module.declare_func_in_func(write_func, builder.func);

        // Call write(1, string_ptr, string_len)
        let call_inst = builder.ins().call(write_ref, &[fd, string_ptr, string_len]);

        // Get the return value (bytes written) - we don't use it for now
        let _bytes_written = builder.inst_results(call_inst)[0];

        Ok(())
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
