//! JIT module construction, runtime symbol registration, and the
//! trampoline that enters the compiled `main` (`Codegen::execute`).
//! The AOT path links the runtime archive instead — see `linker.rs`.

use super::Codegen;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::FuncId;
use std::ffi::{CString, c_char};

/// Every runtime symbol the JIT must resolve, with its address. This
/// table is the single source of truth for JIT registration — the
/// names must stay in sync with the string literals codegen passes to
/// `declare_runtime_fn` (the module-level import cache is keyed on the
/// same names). Functions whose bodies codegen now inlines (literal
/// packing, slicing) are deliberately absent.
fn runtime_symbols() -> [(&'static str, *const u8); 26] {
    [
        ("ryo_str_concat", ryo_runtime::ryo_str_concat as *const u8),
        ("__ryo_str_push", ryo_runtime::__ryo_str_push as *const u8),
        (
            "__ryo_str_ensure_heap",
            ryo_runtime::__ryo_str_ensure_heap as *const u8,
        ),
        (
            "__ryo_bytes_ensure_heap",
            ryo_runtime::__ryo_bytes_ensure_heap as *const u8,
        ),
        ("ryo_str_eq", ryo_runtime::ryo_str_eq as *const u8),
        ("ryo_int_to_str", ryo_runtime::ryo_int_to_str as *const u8),
        (
            "ryo_str_from_view",
            ryo_runtime::ryo_str_from_view as *const u8,
        ),
        (
            "ryo_float_to_str",
            ryo_runtime::ryo_float_to_str as *const u8,
        ),
        ("ryo_bool_to_str", ryo_runtime::ryo_bool_to_str as *const u8),
        ("ryo_str_free", ryo_runtime::ryo_str_free as *const u8),
        // M8.4.2 bytes family — names match the runtime's
        // `#[unsafe(no_mangle)]` exports verbatim.
        (
            "ryo_bytes_concat",
            ryo_runtime::ryo_bytes_concat as *const u8,
        ),
        (
            "__ryo_bytes_push",
            ryo_runtime::__ryo_bytes_push as *const u8,
        ),
        (
            "__ryo_bytes_index",
            ryo_runtime::__ryo_bytes_index as *const u8,
        ),
        ("ryo_bytes_eq", ryo_runtime::ryo_bytes_eq as *const u8),
        (
            "ryo_bytes_from_view",
            ryo_runtime::ryo_bytes_from_view as *const u8,
        ),
        (
            "__ryo_bytes_to_str",
            ryo_runtime::__ryo_bytes_to_str as *const u8,
        ),
        (
            "__ryo_str_to_bytes",
            ryo_runtime::__ryo_str_to_bytes as *const u8,
        ),
        (
            "__ryo_bytes_repr",
            ryo_runtime::__ryo_bytes_repr as *const u8,
        ),
        ("ryo_bytes_free", ryo_runtime::ryo_bytes_free as *const u8),
        ("ryo_print", ryo_runtime::ryo_print as *const u8),
        ("ryo_eprint", ryo_runtime::ryo_eprint as *const u8),
        ("ryo_panic", ryo_runtime::ryo_panic as *const u8),
        ("ryo_exit", ryo_runtime::ryo_exit as *const u8),
        // M9.2 PR2 argv family — `ryo_rt_init` is called at main entry
        // by the codegen entry shim; `ryo_process_*` back the
        // process_argc/process_argv intrinsics.
        ("ryo_rt_init", ryo_runtime::ryo_rt_init as *const u8),
        (
            "ryo_process_argc",
            ryo_runtime::ryo_process_argc as *const u8,
        ),
        (
            "ryo_process_argv",
            ryo_runtime::ryo_process_argv as *const u8,
        ),
    ]
}

impl Codegen<JITModule> {
    pub fn new_jit() -> Result<Self, String> {
        // opt_level=speed: run the egraph optimization pipeline (constant
        // folding, algebraic simplification, GVN/LICM) like the AOT path.
        // enable_verifier: debug builds and tests only, same rationale as
        // `aot_shared_flags`.
        let mut jit_builder = JITBuilder::with_flags(
            &[
                ("opt_level", "speed"),
                (
                    "enable_verifier",
                    if cfg!(debug_assertions) {
                        "true"
                    } else {
                        "false"
                    },
                ),
            ],
            cranelift_module::default_libcall_names(),
        )
        .map_err(|e| format!("Failed to create JIT builder: {}", e))?;

        // Register runtime symbols so the JIT can resolve them.
        jit_builder.symbols(runtime_symbols());

        Ok(Self::from_module(JITModule::new(jit_builder)))
    }

    /// Enter the compiled `main`, forwarding `argv` (the program
    /// arguments collected after the source file — argv[0]-less, unlike
    /// a C runtime's table) to the runtime's argv storage via the
    /// entry shim codegen emits at `main`'s entry. Returns the shim's
    /// int exit word.
    pub fn execute(mut self, main_id: FuncId, argv: &[String]) -> Result<i32, String> {
        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize JIT definitions: {}", e))?;

        // NUL-encode the program args; both the CStrings and the
        // pointer array built from them must outlive the call below.
        let cstrings: Vec<CString> = argv
            .iter()
            .map(|arg| {
                CString::new(arg.as_str())
                    .map_err(|e| format!("Invalid program argument (interior NUL): {e}"))
            })
            .collect::<Result<_, _>>()?;
        let ptrs: Vec<*const c_char> = cstrings.iter().map(|c| c.as_ptr()).collect();
        let argc = i64::try_from(ptrs.len())
            .map_err(|_| "Too many program arguments to forward".to_string())?;

        let code_ptr = self.module.get_finalized_function(main_id);
        // SAFETY (R5 exception): `code_ptr` was finalized by
        // cranelift-jit for this module above, and the compiled entry
        // point has exactly the `extern "C" fn(i64, *const *const
        // c_char) -> i64` type of `main_fn`: codegen emits hosted
        // `main` with argc as the target's pointer-sized int (i64 on
        // 64-bit targets — see the `is_main` branch of
        // `build_signature`), argv as a raw pointer, and the int
        // return word (Cranelift's default CallConv is the platform
        // C ABI; Rust's own ABI is unspecified, so the cast must name
        // extern "C"). `ptrs.as_ptr()` names `ptrs`'s backing storage,
        // and `cstrings`/`ptrs` are both live across the call: every
        // element is one of the CStrings' interior pointers, valid and
        // NUL-terminated for the whole call, and `argc` matches the
        // array length (an empty `argv` passes argc 0, which the
        // runtime never dereferences).
        #[allow(unsafe_code)]
        let main_fn: extern "C" fn(i64, *const *const c_char) -> i64 =
            unsafe { std::mem::transmute(code_ptr) };
        let result = main_fn(argc, ptrs.as_ptr());

        // SAFETY (R5 exception): execution finished above; freeing the
        // module's memory cannot invalidate any live code.
        #[allow(unsafe_code)]
        unsafe {
            self.module.free_memory();
        }

        Ok(result as i32)
    }
}
