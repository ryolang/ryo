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
//! 2. The `eval_inst` memoization map (`HashMap<TirRef, ValueRepr>`)
//!    so a shared sub-expression isn't re-emitted. TIR today is
//!    tree-shaped (one parent per inst) so this is purely
//!    defensive — but it's the right invariant before lazy sema
//!    / inline expansion lands. Zig calls the analogous mapping
//!    in `Air.zig` "liveness"; we don't need full liveness yet.

use cranelift::codegen::ir::{ArgumentPurpose, MemFlagsData};
use cranelift::codegen::isa;
use cranelift::codegen::settings::{self, Configurable};
use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use ryo_core::ast::CompoundOp;
use ryo_core::tir::{ParamMode, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeId, TypeKind};
use std::collections::{HashMap, HashSet};
use target_lexicon::Triple;

mod expr;

/// Fat-str triple layout (24 bytes): ptr at 0, len at 8, cap at 16.
/// Derived from `RyoStrFat`, not re-hardcoded.
const STR_SLOT_SIZE: u32 = 24;

/// How a statement or body ended the current block, if it did.
/// Replaces the `bool` that conflated Break/Continue with Return:
/// callers distinguish "block ended" (`!= None`) from "the function
/// definitely returns" (`== Return`) explicitly.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Terminator {
    None,
    Return,
    Break,
    Continue,
}

/// Returns `true` if `ty` resolves to `Str` in the pool.
///
/// Callers use this to gate multi-value (fat-pointer) paths before
/// reaching `cranelift_type_for`, where a `Str` is a caller bug.
fn is_str_type(ty: TypeId, pool: &InternPool) -> bool {
    matches!(pool.kind(ty), TypeKind::Str)
}

/// Map a TIR type to the corresponding Cranelift IR type.
///
/// `Int` uses the target's pointer-sized integer (i64 on 64-bit).
/// `Bool` uses I8 (matches Cranelift's `icmp` result width and Rust's bool layout).
/// `Str` is a fat pointer (ptr, len, cap) — it cannot map to a single type;
/// callers must gate with `is_str_type` before reaching this function.
/// Views (`strview`, M8.4) are likewise multi-word `{ptr, len}`; callers
/// must gate with `pool.is_view()` before reaching this function.
/// `Void` has no Cranelift representation and should not be mapped here.
fn cranelift_type_for(ty: TypeId, pool: &InternPool, pointer_ty: types::Type) -> types::Type {
    match pool.kind(ty) {
        TypeKind::Int => pointer_ty,
        TypeKind::Str => {
            unreachable!("cranelift_type_for: str is multi-value; use is_str_type gate")
        }
        TypeKind::View(_) => {
            unreachable!("cranelift_type_for: strview is two-word; use pool.is_view() gate")
        }
        TypeKind::Bool => types::I8,
        TypeKind::Float => types::F64,
        // Dead code after trap, but Cranelift needs a concrete type for every SSA value
        TypeKind::Never => types::I8,
        TypeKind::Void => unreachable!("cranelift_type_for: void has no representation"),
        TypeKind::Error => {
            // Reaching codegen with the Error sentinel means sema
            // accepted a program despite a resolution failure. The
            // driver must short-circuit on `sink.has_errors()`.
            unreachable!("cranelift_type_for: <error> sentinel reached codegen")
        }
        TypeKind::Tuple => {
            // No surface syntax constructs tuples today, so a tuple
            // TypeId cannot reach codegen; the variant exists only to
            // validate the InternPool's sidecar encoding.
            unreachable!("cranelift_type_for: tuple TypeId reached codegen")
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
    /// Cache of `DataId` per compiler-generated guard message
    /// (zero-divisor checks). These strings never pass through the
    /// `InternPool`, so they are keyed on the static text itself.
    guard_msg_data: HashMap<&'static str, DataId>,
}

/// Zero-divisor guard messages, written verbatim by `ryo_panic`
/// (raw bytes, trailing newline — same convention as the runtime's
/// slice-failure messages).
const DIV_ZERO_MSG: &str = "integer division by zero\n";
const MOD_ZERO_MSG: &str = "integer modulo by zero\n";
/// Overflow guard message for the spec §18 checked-arithmetic traps.
const OVERFLOW_MSG: &str = "integer overflow\n";

/// Per-loop codegen state: the Cranelift blocks that `break` and
/// `continue` jump to.
struct LoopContext {
    exit_block: Block,
    /// Where `continue` jumps. For while-loops this is the header
    /// (re-evaluate condition); for for-range loops this is the
    /// increment block (advance the counter before re-checking).
    continue_target: Block,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ValueRepr {
    Scalar(Value),
    Str {
        ptr: Value,
        len: Value,
        cap: Value,
    },
    /// View pair: 16 bytes, non-owning (M8.4). Never freed. Mirrors
    /// `TypeKind::View` — all view kinds share the `{ptr, len}` repr.
    View {
        ptr: Value,
        len: Value,
    },
}

impl ValueRepr {
    #[cfg(test)]
    fn expect_scalar(self) -> Value {
        match self {
            ValueRepr::Scalar(v) => v,
            ValueRepr::Str { .. } => panic!("expected Scalar, got Str"),
            ValueRepr::View { .. } => panic!("expected Scalar, got View"),
        }
    }
}

#[derive(Clone)]
struct StrLocals {
    ptr: Variable,
    len: Variable,
    cap: Variable,
}

#[derive(Clone)]
struct ViewLocals {
    ptr: Variable,
    len: Variable,
}

/// Per-function emission state. Lives only for the duration of one
/// `compile_function` call; reset between functions because
/// Cranelift `Variable` ids and the `TirRef → Value` memo are both
/// function-local — and because `TirRef` itself is scoped to a
/// single `Tir`.
pub(crate) struct FunctionContext<'a, M: Module> {
    module: &'a mut M,
    data_ctx: &'a mut DataDescription,
    string_data: &'a mut HashMap<StringId, DataId>,
    int_type: types::Type,
    pool: &'a InternPool,
    tir: &'a Tir,
    locals: HashMap<StringId, Variable>,
    func_ids: &'a HashMap<StringId, FuncId>,
    /// `TirRef → ValueRepr` memo. Materializing the same instruction
    /// twice in one function would either duplicate side effects
    /// (calls) or waste Cranelift IR; both are cheap-but-wrong.
    ///
    /// Dense table indexed directly by `TirRef::index()` (slot 0
    /// unused — refs are 1-based), sized once per function from the
    /// TIR instruction count: TIR is tree-shaped today, so the memo
    /// almost never hits and per-instruction HashMap hashing was pure
    /// overhead. Param sentinel refs (`TirRef::param`) are not valid
    /// arena indices and live in `param_values` instead.
    ///
    /// INVARIANT: this table is deliberately cross-block (one flat
    /// table per function, not scoped per basic block). That is sound
    /// only because the current TIR producers guarantee:
    ///   (a) TIR instructions are unique per use — no shared
    ///       sub-expressions, so a `TirRef` is materialized in exactly
    ///       one block and read only where that block dominates;
    ///   (b) `BoolAnd`/`BoolOr` merge values via block params (phi
    ///       nodes), so the memoized `Value` is the merge-block param,
    ///       which dominates every downstream use;
    ///   (c) `IfStmt` is statement-level — no values flow out of
    ///       branches, so no branch-local value is ever read after the
    ///       merge.
    /// If a future TIR producer introduces expression-level control
    /// flow (ternary if) or shared sub-expressions across blocks, this
    /// memo MUST be re-scoped per-block or reads will hit Cranelift
    /// dominator errors.
    inst_values: Vec<Option<ValueRepr>>,
    /// Memo entries keyed by param sentinel refs (`TirRef::param`),
    /// which cannot index `inst_values`. Only str/view params ever
    /// land here — two inserts per function at most — so a tiny map
    /// is fine.
    param_values: HashMap<TirRef, ValueRepr>,
    /// Indices into `sidecar.free_schedule` whose Frees have already
    /// been emitted in codegen. A given anchor TirRef can be reached
    /// through both `eval_inst` and `eval_inst_str` (e.g. a `Var`
    /// materialized once as scalar and once as fat-pointer), and the
    /// end-of-stmt sweep can also see anchors that an earlier
    /// per-eval hook already fired. Without this guard each path
    /// would emit the Free, double-freeing the allocation.
    freed_at: HashSet<usize>,
    /// Maps an anchor `TirRef` (`after`) to the indices of `sidecar.free_schedule`
    /// that are anchored on it. Used for O(1) free-lookup at statement-level and
    /// instruction-level emission.
    free_by_after: HashMap<TirRef, Vec<usize>>,
    /// Unfired indices in `sidecar.free_schedule` that still need to be swept.
    /// Used to avoid O(K * S) quadratic scaling during end-of-statement sweep.
    pending_sweep: Vec<usize>,
    loop_stack: Vec<LoopContext>,
    str_locals: HashMap<StringId, StrLocals>,
    /// `strview` view bindings (M8.4): two SSA `Variable`s per binding,
    /// mirroring `str_locals`. Views are non-owning — they never
    /// appear in the free schedule.
    view_locals: HashMap<StringId, ViewLocals>,
    /// Free-target (initializer / Assign value / str-param virtual ref)
    /// → binding-name map, built once per function by
    /// `build_free_binding_names`. `emit_frees` uses it to release a
    /// named binding's CURRENT `StrLocals` rather than the producing
    /// init's possibly-stale cached repr.
    free_binding_names: HashMap<TirRef, StringId>,
    /// M8.3 inout parameters: maps each inout param's name to the
    /// caller-provided slot address (a function-entry block param)
    /// and its pointee `TypeId`. The write-back chokepoint stores each
    /// param's current `Variable` back through this pointer before
    /// every `return_`. Scalars store one field at offset 0; str
    /// pointees (M8.3a Task 9) store three fields.
    inout_ptrs: HashMap<StringId, (Value, TypeId)>,
    /// For str-returning functions: the hidden sret pointer (first block param)
    /// through which the callee writes the (ptr, len, cap) triple.
    sret_ptr: Option<Value>,
    /// Ownership sidecar for the function currently being lowered.
    /// `TirRef`s are scoped per-function — each `Tir`'s arena restarts
    /// at `TirRef(1)` — so codegen must consult only the entry that
    /// belongs to this function. `compile_function` picks
    /// `sidecar.functions[i]`, positional with the `tirs` slice, and
    /// threads the resulting per-function entry here. Both
    /// unconditional (`branch: None`) and branch-gated
    /// (`branch: Some(_)`) entries are filtered through
    /// `branch_active`.
    sidecar: &'a ryo_core::ownership::FunctionSidecar,
    /// Active arm stack for conditional destruction (Task 9). Each
    /// entry is the `BranchId` of an enclosing if/elif/else arm
    /// currently being lowered. `branch_active` walks this stack to
    /// gate branch-tagged `FreePoint`s — `contains` (not `last()`)
    /// so a Free anchored to a parent arm still fires from inside a
    /// nested child arm of the same parent.
    branch_stack: Vec<ryo_core::ownership::BranchId>,
    /// Module-level cache for compiler-generated guard messages;
    /// see `Codegen::guard_msg_data`.
    guard_msg_data: &'a mut HashMap<&'static str, DataId>,
    /// Cold panic blocks for guard failures (overflow, div-by-zero),
    /// paired with their message so all guards with the same message
    /// share one block. A Vec (not a map) keeps the drain order
    /// deterministic — identical input must produce identical binary
    /// bytes — and there are only two guard messages in total.
    /// Emitted at end-of-function (see `compile_function`) so the hot
    /// path falls through the guard `brif` instead of jumping over
    /// inline panic code.
    panic_blocks: Vec<(&'static str, Block)>,
}

impl<M: Module> Codegen<M> {
    fn from_module(module: M) -> Self {
        let int_type = module.target_config().pointer_type();
        Self {
            builder_context: FunctionBuilderContext::new(),
            ctx: module.make_context(),
            module,
            int_type,
            data_ctx: DataDescription::new(),
            string_data: HashMap::new(),
            guard_msg_data: HashMap::new(),
        }
    }
}

/// Shared Cranelift flags for the AOT object pipeline.
///
/// `enable_llvm_abi_extensions` is required for the packed-u128 string
/// runtime ABI (Task-8): without it, Cranelift's x64 ABI panics on any
/// signature containing an i128 ("i128 args/return values not supported
/// unless LLVM ABI extensions are enabled", `isa/x64/abi.rs`). With it,
/// an i128 is split into two i64 halves assigned as consecutive
/// register-sized parts — rax:rdx on both SysV and WindowsFastcall,
/// matching the Rust ABI the `#[unsafe(no_mangle)] pub fn` runtime
/// functions use. aarch64 lowers i128 natively and ignores the flag.
fn aot_shared_flags() -> Result<settings::Flags, String> {
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
    shared_builder
        .enable("enable_llvm_abi_extensions")
        .map_err(|e| format!("Error enabling enable_llvm_abi_extensions: {}", e))?;
    // The Cranelift verifier is a compiler-developer aid (it catches
    // malformed IR our codegen emits); users cannot act on its
    // failures. Keep it in debug builds and the test suite — where
    // compiler developers run — and skip its cost in release builds
    // (the wasmtime pattern). It accounts for ~23–27% of codegen time.
    shared_builder
        .set(
            "enable_verifier",
            if cfg!(debug_assertions) {
                "true"
            } else {
                "false"
            },
        )
        .map_err(|e| format!("Error setting enable_verifier: {}", e))?;
    Ok(settings::Flags::new(shared_builder))
}

impl Codegen<ObjectModule> {
    pub fn new_aot(target_triple: Triple) -> Result<Self, String> {
        let shared_flags = aot_shared_flags()?;

        let isa = isa::lookup(target_triple.clone())
            .map_err(|e| format!("Unsupported target '{}': {}", target_triple, e))?
            .finish(shared_flags)
            .map_err(|e| format!("Failed to build ISA: {}", e))?;

        let obj_builder =
            ObjectBuilder::new(isa, "ryo_module", cranelift_module::default_libcall_names())
                .map_err(|e| format!("Failed to create ObjectBuilder: {}", e))?;

        Ok(Self::from_module(ObjectModule::new(obj_builder)))
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
        // enable_llvm_abi_extensions: same rationale as `aot_shared_flags` —
        // the packed-u128 string runtime ABI requires it on x64.
        // opt_level=speed: run the egraph optimization pipeline (constant
        // folding, algebraic simplification, GVN/LICM) like the AOT path.
        // enable_verifier: debug builds and tests only, same rationale as
        // `aot_shared_flags`.
        let mut jit_builder = JITBuilder::with_flags(
            &[
                ("enable_llvm_abi_extensions", "true"),
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
        jit_builder.symbols([
            (
                "ryo_str_from_literal",
                ryo_runtime::ryo_str_from_literal as *const u8,
            ),
            ("ryo_str_alloc", ryo_runtime::ryo_str_alloc as *const u8),
            ("ryo_str_concat", ryo_runtime::ryo_str_concat as *const u8),
            ("__ryo_str_push", ryo_runtime::__ryo_str_push as *const u8),
            ("__ryo_slice", ryo_runtime::__ryo_slice as *const u8),
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
            ("ryo_print", ryo_runtime::ryo_print as *const u8),
            ("ryo_panic", ryo_runtime::ryo_panic as *const u8),
        ]);

        Ok(Self::from_module(JITModule::new(jit_builder)))
    }

    pub fn execute(mut self, main_id: FuncId) -> Result<i32, String> {
        self.module
            .finalize_definitions()
            .map_err(|e| format!("Failed to finalize JIT definitions: {}", e))?;

        let code_ptr = self.module.get_finalized_function(main_id);
        // SAFETY (R5 exception): `code_ptr` was finalized by
        // cranelift-jit for this module above, and the compiled entry point
        // has the `extern "C" fn() -> isize` signature we emit for `main`
        // (Cranelift's default CallConv is the platform C ABI; Rust's own
        // ABI is unspecified, so the cast must name extern "C").
        #[allow(unsafe_code)]
        let main_fn: extern "C" fn() -> isize = unsafe { std::mem::transmute(code_ptr) };
        let result = main_fn();

        // SAFETY (R5 exception): execution finished above; freeing the
        // module's memory cannot invalidate any live code.
        #[allow(unsafe_code)]
        unsafe {
            self.module.free_memory();
        }

        Ok(result as i32)
    }
}

impl<M: Module> Codegen<M> {
    fn prepare_compilation(
        &mut self,
        tirs: &[Tir],
        pool: &InternPool,
    ) -> Result<HashMap<StringId, FuncId>, String> {
        self.declare_all_functions(tirs, pool)
    }

    /// Read the `TirRef → ValueRepr` memo. Param sentinel refs
    /// (`TirRef::param`) are not valid arena indices, so they are
    /// served from the `param_values` side map.
    fn cached_repr(ctx: &FunctionContext<'_, M>, r: TirRef) -> Option<ValueRepr> {
        if r.is_param() {
            ctx.param_values.get(&r).copied()
        } else {
            ctx.inst_values.get(r.index()).copied().flatten()
        }
    }

    /// Write the `TirRef → ValueRepr` memo; see `cached_repr`.
    fn cache_repr(ctx: &mut FunctionContext<'_, M>, r: TirRef, repr: ValueRepr) {
        if r.is_param() {
            ctx.param_values.insert(r, repr);
        } else {
            ctx.inst_values[r.index()] = Some(repr);
        }
    }

    pub fn compile(
        &mut self,
        tirs: &[Tir],
        pool: &InternPool,
        sidecar: &ryo_core::ownership::OwnershipSidecar,
    ) -> Result<FuncId, String> {
        debug_assert!(
            no_unreachable_in(tirs),
            "codegen::compile requires sema to have produced TIR with no Unreachable instructions"
        );
        let func_ids = self.prepare_compilation(tirs, pool)?;

        for (i, tir) in tirs.iter().enumerate() {
            self.compile_function(tir, &func_ids, pool, sidecar, i)?;
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
        sidecar: &ryo_core::ownership::OwnershipSidecar,
    ) -> Result<String, String> {
        debug_assert!(
            no_unreachable_in(tirs),
            "codegen::compile_and_dump_ir requires sema to have produced TIR with no Unreachable instructions"
        );
        let func_ids = self.prepare_compilation(tirs, pool)?;

        let mut ir_output = String::new();
        for (i, tir) in tirs.iter().enumerate() {
            ir_output.push_str(&self.compile_function(tir, &func_ids, pool, sidecar, i)?);
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
            if param.mode == ParamMode::Inout {
                // Mutable borrow: pass a single pointer to the caller's
                // slot, regardless of pointee type (scalar or str).
                sig.params.push(AbiParam::new(self.int_type));
            } else if is_str_type(param.ty, pool) {
                sig.params.push(AbiParam::new(self.int_type)); // ptr
                sig.params.push(AbiParam::new(types::I64)); // len
                sig.params.push(AbiParam::new(types::I64)); // cap
            } else if pool.is_view(param.ty) {
                // `strview` view: 2-word ABI (ptr, len) — no cap word (M8.4).
                sig.params.push(AbiParam::new(self.int_type)); // ptr
                sig.params.push(AbiParam::new(types::I64)); // len
            } else {
                let cl_ty = cranelift_type_for(param.ty, pool, self.int_type);
                sig.params.push(AbiParam::new(cl_ty));
            }
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
            if is_str_type(tir.return_type, pool) {
                // sret: hidden pointer prepended to regular params, no IR-level return.
                sig.params.insert(
                    0,
                    AbiParam::special(self.int_type, ArgumentPurpose::StructReturn),
                );
            } else {
                let cl_ty = cranelift_type_for(tir.return_type, pool, self.int_type);
                sig.returns.push(AbiParam::new(cl_ty));
            }
        }
        sig
    }

    fn compile_function(
        &mut self,
        tir: &Tir,
        func_ids: &HashMap<StringId, FuncId>,
        pool: &InternPool,
        sidecar: &ryo_core::ownership::OwnershipSidecar,
        sidecar_index: usize,
    ) -> Result<String, String> {
        let func_id = *func_ids
            .get(&tir.name)
            .ok_or_else(|| format!("Function '{}' not declared", pool.str(tir.name)))?;

        // Pick the per-function sidecar entry. `TirRef`s are scoped
        // per-function (each `Tir` arena restarts at `TirRef(1)`), so
        // threading the program-wide sidecar would let frees scheduled
        // for one function fire at numerically-matching TirRefs in
        // another. The sidecar is positional with the `tirs` slice —
        // `ownership::check` pushes exactly one entry per body — so a
        // missing entry is a pipeline contract violation, not a case
        // to paper over with an empty sidecar (compiler-emitted
        // helpers like `__ryo_panic` are imported runtime calls and
        // never appear in `tirs`).
        let func_sidecar = sidecar.functions.get(sidecar_index).ok_or_else(|| {
            format!(
                "ownership sidecar has no entry for '{}' (index {} of {})",
                pool.str(tir.name),
                sidecar_index,
                sidecar.functions.len()
            )
        })?;
        // The length check above cannot detect a `tirs` slice that was
        // reordered or filtered between `ownership::check` and codegen
        // (same length, wrong alignment): every index would resolve to
        // a wrong-but-plausible sidecar. The name recorded at push
        // time pins entry `i` to `tirs[i]`.
        debug_assert_eq!(
            func_sidecar.name, tir.name,
            "ownership sidecar misaligned with tirs at index {}",
            sidecar_index
        );

        self.ctx.func.signature = self.build_signature(tir, pool);

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);

            let int_type = self.int_type;
            let mut locals: HashMap<StringId, Variable> = HashMap::new();

            let is_main = pool.str(tir.name) == "main";
            let returns_str = !is_main && is_str_type(tir.return_type, pool);
            let mut block_idx: usize = if returns_str { 1 } else { 0 };
            let sret_ptr = if returns_str {
                Some(builder.block_params(entry_block)[0])
            } else {
                None
            };

            let mut str_param_locals: HashMap<StringId, StrLocals> = HashMap::new();
            let mut view_param_locals: HashMap<StringId, ViewLocals> = HashMap::new();
            let mut inout_ptrs: HashMap<StringId, (Value, TypeId)> = HashMap::new();

            for param in tir.params.iter() {
                if param.mode == ParamMode::Inout {
                    // inout param: a single pointer to the caller's slot,
                    // regardless of pointee type. Load the current value
                    // into Variables so the body's existing read/mutate
                    // codegen is unchanged; remember the pointer for the
                    // write-back chokepoint before each `return_`.
                    let ptr = builder.block_params(entry_block)[block_idx];
                    block_idx += 1;
                    if is_str_type(param.ty, pool) {
                        // str inout: load the fat-pointer triple into
                        // StrLocals so the body reads/mutates it like any
                        // str local; write all three fields back before
                        // each return_.
                        let p = builder
                            .ins()
                            .load(int_type, MemFlagsData::trusted(), ptr, 0);
                        let l = builder
                            .ins()
                            .load(types::I64, MemFlagsData::trusted(), ptr, 8);
                        let c = builder
                            .ins()
                            .load(types::I64, MemFlagsData::trusted(), ptr, 16);
                        let var_ptr = builder.declare_var(int_type);
                        let var_len = builder.declare_var(types::I64);
                        let var_cap = builder.declare_var(types::I64);
                        builder.def_var(var_ptr, p);
                        builder.def_var(var_len, l);
                        builder.def_var(var_cap, c);
                        str_param_locals.insert(
                            param.name,
                            StrLocals {
                                ptr: var_ptr,
                                len: var_len,
                                cap: var_cap,
                            },
                        );
                    } else {
                        let cl_ty = cranelift_type_for(param.ty, pool, int_type);
                        let cur = builder.ins().load(cl_ty, MemFlagsData::trusted(), ptr, 0);
                        let var = builder.declare_var(cl_ty);
                        builder.def_var(var, cur);
                        locals.insert(param.name, var);
                    }
                    inout_ptrs.insert(param.name, (ptr, param.ty));
                    continue;
                }
                if is_str_type(param.ty, pool) {
                    let var_ptr = builder.declare_var(int_type);
                    let var_len = builder.declare_var(types::I64);
                    let var_cap = builder.declare_var(types::I64);
                    builder.def_var(var_ptr, builder.block_params(entry_block)[block_idx]);
                    builder.def_var(var_len, builder.block_params(entry_block)[block_idx + 1]);
                    builder.def_var(var_cap, builder.block_params(entry_block)[block_idx + 2]);
                    str_param_locals.insert(
                        param.name,
                        StrLocals {
                            ptr: var_ptr,
                            len: var_len,
                            cap: var_cap,
                        },
                    );
                    block_idx += 3;
                } else if pool.is_view(param.ty) {
                    // `strview` view param: two ABI words (ptr, len). Views
                    // are borrows — no cap, never freed.
                    let var_ptr = builder.declare_var(int_type);
                    let var_len = builder.declare_var(types::I64);
                    builder.def_var(var_ptr, builder.block_params(entry_block)[block_idx]);
                    builder.def_var(var_len, builder.block_params(entry_block)[block_idx + 1]);
                    view_param_locals.insert(
                        param.name,
                        ViewLocals {
                            ptr: var_ptr,
                            len: var_len,
                        },
                    );
                    block_idx += 2;
                } else {
                    let cl_ty = cranelift_type_for(param.ty, pool, int_type);
                    let var = builder.declare_var(cl_ty);
                    builder.def_var(var, builder.block_params(entry_block)[block_idx]);
                    locals.insert(param.name, var);
                    block_idx += 1;
                }
            }

            let mut free_by_after: HashMap<TirRef, Vec<usize>> = HashMap::new();
            for (idx, fp) in func_sidecar.free_schedule.iter().enumerate() {
                free_by_after.entry(fp.after).or_default().push(idx);
            }
            let pending_sweep: Vec<usize> = (0..func_sidecar.free_schedule.len()).collect();

            let mut ctx: FunctionContext<'_, M> = FunctionContext {
                module: &mut self.module,
                data_ctx: &mut self.data_ctx,
                string_data: &mut self.string_data,
                int_type,
                pool,
                tir,
                locals,
                func_ids,
                inst_values: vec![None; tir.instructions.len()],
                param_values: HashMap::new(),
                freed_at: HashSet::new(),
                free_by_after,
                pending_sweep,
                loop_stack: Vec::new(),
                str_locals: str_param_locals,
                view_locals: view_param_locals,
                free_binding_names: Self::build_free_binding_names(tir, pool),
                inout_ptrs,
                sret_ptr,
                sidecar: func_sidecar,
                branch_stack: Vec::new(),
                guard_msg_data: &mut self.guard_msg_data,
                panic_blocks: Vec::new(),
            };

            for (idx, param) in tir.params.iter().enumerate() {
                if is_str_type(param.ty, pool) {
                    let locals = ctx
                        .str_locals
                        .get(&param.name)
                        .expect("every str param gets StrLocals in the param preamble above");
                    let virtual_ref = TirRef::param(idx);
                    let repr = ValueRepr::Str {
                        ptr: builder.use_var(locals.ptr),
                        len: builder.use_var(locals.len),
                        cap: builder.use_var(locals.cap),
                    };
                    ctx.param_values.insert(virtual_ref, repr);
                } else if pool.is_view(param.ty) {
                    let locals = ctx
                        .view_locals
                        .get(&param.name)
                        .expect("every view param gets ViewLocals in the param preamble above");
                    let virtual_ref = TirRef::param(idx);
                    let repr = ValueRepr::View {
                        ptr: builder.use_var(locals.ptr),
                        len: builder.use_var(locals.len),
                    };
                    ctx.param_values.insert(virtual_ref, repr);
                }
            }

            // Hoist string literals while the entry block is still the
            // current block: one ryo_str_from_literal call per distinct
            // literal per function, dominating every use.
            Self::hoist_str_literals(&mut builder, &mut ctx)?;

            let body_term = Self::emit_body(&mut builder, &mut ctx, &tir.body_stmts())?;

            if body_term == Terminator::None {
                if is_main {
                    let zero = builder.ins().iconst(int_type, 0);
                    Self::emit_return(&mut builder, &mut ctx, &[zero])?;
                } else if returns_str || tir.return_type == pool.void() {
                    Self::emit_return(&mut builder, &mut ctx, &[])?;
                } else {
                    let zero = builder.ins().iconst(int_type, 0);
                    Self::emit_return(&mut builder, &mut ctx, &[zero])?;
                }
            }

            // No scheduled Free may be dropped without a
            // same-target substitute having fired. The ownership pass
            // deliberately anchors some temp frees twice — once at the
            // consuming sub-expression and once at the enclosing Return
            // (its return-epilogue pass) — because codegen cannot sweep
            // after a terminator. Firing the Return-anchored Free leaves
            // the consumer-anchored duplicate in `pending_sweep`; that
            // is fine as long as the target was freed once. A pending
            // entry with NO fired same-target counterpart means the
            // allocation leaks.
            //
            // This assertion covers the LEAK direction only. Double-free
            // is prevented upstream in the ownership scheduler (the
            // covered/on_path dedup), so no target-uniqueness check is
            // needed here.
            debug_assert!(
                ctx.pending_sweep.iter().all(|&idx| {
                    let target = ctx.sidecar.free_schedule[idx].target;
                    ctx.freed_at
                        .iter()
                        .any(|&fired| ctx.sidecar.free_schedule[fired].target == target)
                }),
                "frees anchored to unmaterialized instructions were dropped: {:?}",
                ctx.pending_sweep
            );

            Self::emit_deferred_panic_blocks(&mut builder, &mut ctx)?;

            builder.finalize(self.module.isa().frontend_config());
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
    ) -> Result<Terminator, String> {
        let mut terminator = Terminator::None;
        for &stmt_ref in stmts {
            if terminator != Terminator::None {
                break;
            }
            terminator = Self::emit_stmt(builder, ctx, stmt_ref)?;
            // Skip Free emission after terminators (Return / Break /
            // Continue): the current block is sealed and Cranelift
            // rejects any instruction after a terminator. Returns also
            // transfer ownership of the returned value to the caller, so
            // emitting a Free here would be incorrect anyway. Break and
            // Continue fire their own Frees before the jump (see
            // emit_stmt), so skipping here drops nothing.
            if terminator == Terminator::None {
                // Anchor-on-stmt Frees first (e.g. dead-store survivors
                // anchored after a VarDecl), then a sweep that catches
                // sub-expression-anchored entries whose consumers have
                // now finished emitting IR.
                Self::emit_due_frees(builder, ctx, stmt_ref)?;
                Self::sweep_due_frees(builder, ctx)?;
            }
        }
        Ok(terminator)
    }

    fn emit_scoped_body(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        stmts: &[TirRef],
    ) -> Result<Terminator, String> {
        let saved_locals = ctx.locals.clone();
        let saved_str_locals = ctx.str_locals.clone();
        let saved_view_locals = ctx.view_locals.clone();
        let terminator = Self::emit_body(builder, ctx, stmts)?;
        ctx.locals = saved_locals;
        ctx.str_locals = saved_str_locals;
        ctx.view_locals = saved_view_locals;
        Ok(terminator)
    }

    /// Store every inout parameter's current `Variable` back through its
    /// caller-provided slot pointer. Called immediately before EVERY
    /// `return_` so mutations are visible to the caller regardless of
    /// which exit the function takes. Panic/abort exits are noreturn and
    /// never reach here — partial mutations are correctly not committed.
    fn emit_inout_writeback(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
    ) -> Result<(), String> {
        for (name, (ptr, ty)) in ctx.inout_ptrs.iter() {
            if is_str_type(*ty, ctx.pool) {
                // str pointee: store all three fat-pointer fields.
                let sl = ctx.str_locals.get(name).ok_or_else(|| {
                    format!("inout str '{}' has no StrLocals", ctx.pool.str(*name))
                })?;
                let p = builder.use_var(sl.ptr);
                let l = builder.use_var(sl.len);
                let c = builder.use_var(sl.cap);
                builder.ins().store(MemFlagsData::trusted(), p, *ptr, 0);
                builder.ins().store(MemFlagsData::trusted(), l, *ptr, 8);
                builder.ins().store(MemFlagsData::trusted(), c, *ptr, 16);
            } else {
                // Scalar pointee: a single store at offset 0.
                let var = ctx.locals.get(name).ok_or_else(|| {
                    format!(
                        "inout scalar '{}' has no local Variable",
                        ctx.pool.str(*name)
                    )
                })?;
                let val = builder.use_var(*var);
                builder.ins().store(MemFlagsData::trusted(), val, *ptr, 0);
            }
        }
        Ok(())
    }

    /// THE single exit point for user functions: inout write-back, then
    /// the return. NEVER emit a bare `return_` for a user-function exit —
    /// a missed write-back silently drops a caller-visible mutation.
    /// Panic/abort paths are noreturn and intentionally skip this.
    fn emit_return(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        vals: &[Value],
    ) -> Result<(), String> {
        Self::emit_inout_writeback(builder, ctx)?;
        builder.ins().return_(vals);
        Ok(())
    }

    /// Emit a top-level statement instruction. Returns the statement's
    /// [`Terminator`] — anything other than `Terminator::None` ends the
    /// current block, and the caller stops the body walk on the first
    /// one.
    fn emit_stmt(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
        let inst = ctx.tir.inst(r);
        match inst.tag {
            TirTag::VarDecl => {
                let view = ctx.tir.var_decl_view(r);
                if is_str_type(inst.ty, ctx.pool) {
                    let repr = Self::eval_inst_str(builder, ctx, view.initializer)?;
                    match repr {
                        ValueRepr::Str { ptr, len, cap } => {
                            let var_ptr = builder.declare_var(ctx.int_type);
                            let var_len = builder.declare_var(types::I64);
                            let var_cap = builder.declare_var(types::I64);
                            builder.def_var(var_ptr, ptr);
                            builder.def_var(var_len, len);
                            builder.def_var(var_cap, cap);
                            ctx.str_locals.insert(
                                view.name,
                                StrLocals {
                                    ptr: var_ptr,
                                    len: var_len,
                                    cap: var_cap,
                                },
                            );
                        }
                        _ => unreachable!("str-typed initializer should produce ValueRepr::Str"),
                    }
                    return Ok(Terminator::None);
                }
                if ctx.pool.is_view(inst.ty) {
                    let repr = Self::eval_inst_view(builder, ctx, view.initializer)?;
                    match repr {
                        ValueRepr::View { ptr, len } => {
                            let var_ptr = builder.declare_var(ctx.int_type);
                            let var_len = builder.declare_var(types::I64);
                            builder.def_var(var_ptr, ptr);
                            builder.def_var(var_len, len);
                            ctx.view_locals.insert(
                                view.name,
                                ViewLocals {
                                    ptr: var_ptr,
                                    len: var_len,
                                },
                            );
                        }
                        _ => unreachable!("view-typed initializer should produce ValueRepr::View"),
                    }
                    return Ok(Terminator::None);
                }
                let val = Self::eval_inst(builder, ctx, view.initializer)?;
                // The variable's resolved type lives in the VarDecl
                // inst's `ty` slot directly — no side-table lookup.
                let cl_ty = cranelift_type_for(inst.ty, ctx.pool, ctx.int_type);
                let var = builder.declare_var(cl_ty);
                builder.def_var(var, val);
                ctx.locals.insert(view.name, var);
                Ok(Terminator::None)
            }
            TirTag::Return => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("Return must carry TirData::UnOp"),
                };
                if is_str_type(ctx.tir.return_type, ctx.pool) {
                    let sret = ctx.sret_ptr.expect("str-returning fn must have sret_ptr");
                    let repr = Self::eval_inst_str(builder, ctx, operand)?;
                    let (ptr, len, cap) = match repr {
                        ValueRepr::Str { ptr, len, cap } => (ptr, len, cap),
                        _ => unreachable!("str return must produce ValueRepr::Str"),
                    };
                    builder.ins().store(MemFlagsData::trusted(), ptr, sret, 0);
                    builder.ins().store(MemFlagsData::trusted(), len, sret, 8);
                    builder.ins().store(MemFlagsData::trusted(), cap, sret, 16);
                    Self::emit_due_frees(builder, ctx, r)?;
                    Self::emit_return(builder, ctx, &[])?;
                } else {
                    let val = Self::eval_inst(builder, ctx, operand)?;
                    Self::emit_due_frees(builder, ctx, r)?;
                    Self::emit_return(builder, ctx, &[val])?;
                }
                Ok(Terminator::Return)
            }
            TirTag::ReturnVoid => {
                // Bare `return` in a void function. If this is
                // `main`, the C ABI demands an int return value.
                let is_main = ctx.pool.str(ctx.tir.name) == "main";
                if is_main {
                    let zero = builder.ins().iconst(ctx.int_type, 0);
                    Self::emit_due_frees(builder, ctx, r)?;
                    Self::emit_return(builder, ctx, &[zero])?;
                } else {
                    Self::emit_due_frees(builder, ctx, r)?;
                    Self::emit_return(builder, ctx, &[])?;
                }
                Ok(Terminator::Return)
            }
            TirTag::ExprStmt => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ExprStmt must carry TirData::UnOp"),
                };
                // Str-typed operands (bare formatter calls, str(view),
                // user str-returning calls) go through the str entry
                // point, which caches the triple for the scheduled temp
                // Free; view-typed operands (bare slices) go through the
                // view entry point; the scalar path rejects both.
                let operand_ty = ctx.tir.inst(operand).ty;
                if is_str_type(operand_ty, ctx.pool) {
                    let _ = Self::eval_inst_str(builder, ctx, operand)?;
                } else if ctx.pool.is_view(operand_ty) {
                    let _ = Self::eval_inst_view(builder, ctx, operand)?;
                } else {
                    let _ = Self::eval_inst(builder, ctx, operand)?;
                }
                Ok(Terminator::None)
            }
            TirTag::IfStmt => Self::generate_if_stmt(builder, ctx, r),
            TirTag::Assign => {
                let view = ctx.tir.assign_view(r);
                if is_str_type(inst.ty, ctx.pool) {
                    let repr = Self::eval_inst_str(builder, ctx, view.value)?;
                    let ValueRepr::Str { ptr, len, cap } = repr else {
                        unreachable!("str-typed assign should produce ValueRepr::Str");
                    };
                    // `.clone()` releases the &ctx.str_locals borrow before the
                    // `declare_str_free` call below needs &mut ctx.module.
                    // StrLocals is three Cranelift `Variable` newtypes; clone is
                    // three integer copies — cheap.
                    let locals = ctx
                        .str_locals
                        .get(&view.name)
                        .ok_or_else(|| {
                            format!(
                                "Undefined string variable in assign: '{}'",
                                ctx.pool.str(view.name)
                            )
                        })?
                        .clone();
                    // Free the old allocation before overwriting locals.
                    // sidecar.free_on_reassign[r] is set whenever the
                    // ownership pass observed a Valid old owner at this
                    // Assign. The old (ptr, cap) live in the binding's
                    // StrLocals Variables — NOT in inst_values[old_owner],
                    // which holds the StrConst's original (ptr, cap) at
                    // the literal's emission point and may be stale
                    // across reassigns.
                    if ctx.sidecar.free_on_reassign.contains_key(&r) {
                        let free_ref = Self::declare_str_free(ctx.module, builder, ctx.int_type)?;
                        let old_ptr = builder.use_var(locals.ptr);
                        let old_cap = builder.use_var(locals.cap);
                        builder.ins().call(free_ref, &[old_ptr, old_cap]);
                    }
                    builder.def_var(locals.ptr, ptr);
                    builder.def_var(locals.len, len);
                    builder.def_var(locals.cap, cap);
                    return Ok(Terminator::None);
                }
                if ctx.pool.is_view(inst.ty) {
                    let repr = Self::eval_inst_view(builder, ctx, view.value)?;
                    let ValueRepr::View { ptr, len } = repr else {
                        unreachable!("view-typed assign should produce ValueRepr::View");
                    };
                    let locals = ctx.view_locals.get(&view.name).ok_or_else(|| {
                        format!(
                            "Undefined strview variable in assign: '{}'",
                            ctx.pool.str(view.name)
                        )
                    })?;
                    // Views are borrows — no free-on-reassign; just
                    // reseat the pair.
                    builder.def_var(locals.ptr, ptr);
                    builder.def_var(locals.len, len);
                    return Ok(Terminator::None);
                }
                let val = Self::eval_inst(builder, ctx, view.value)?;
                let var = ctx.locals.get(&view.name).ok_or_else(|| {
                    format!(
                        "Undefined variable in assign: '{}'",
                        ctx.pool.str(view.name)
                    )
                })?;
                builder.def_var(*var, val);
                Ok(Terminator::None)
            }
            TirTag::CompoundAssign => {
                let view = ctx.tir.compound_assign_view(r);
                let rhs = Self::eval_inst(builder, ctx, view.value)?;
                // Copy the Variable handle out: the zero-divisor guard
                // below needs &mut ctx, which an outstanding borrow of
                // ctx.locals would block.
                let var = *ctx.locals.get(&view.name).ok_or_else(|| {
                    format!(
                        "Undefined variable in compound assign: '{}'",
                        ctx.pool.str(view.name)
                    )
                })?;
                let current = builder.use_var(var);

                let is_float = inst.ty == ctx.pool.float();
                let result = match (view.op, is_float) {
                    // Same spec §18 checked arithmetic as the binop arm.
                    (CompoundOp::Add, false) => {
                        Self::emit_checked_iadd(builder, ctx, current, rhs)?
                    }
                    (CompoundOp::Sub, false) => {
                        Self::emit_checked_isub(builder, ctx, current, rhs)?
                    }
                    (CompoundOp::Mul, false) => {
                        Self::emit_checked_imul(builder, ctx, current, rhs)?
                    }
                    (CompoundOp::Div, false) => {
                        Self::emit_div_zero_guard(builder, ctx, rhs, DIV_ZERO_MSG)?;
                        builder.ins().sdiv(current, rhs)
                    }
                    (CompoundOp::Mod, false) => {
                        Self::emit_div_zero_guard(builder, ctx, rhs, MOD_ZERO_MSG)?;
                        builder.ins().srem(current, rhs)
                    }
                    (CompoundOp::Add, true) => builder.ins().fadd(current, rhs),
                    (CompoundOp::Sub, true) => builder.ins().fsub(current, rhs),
                    (CompoundOp::Mul, true) => builder.ins().fmul(current, rhs),
                    (CompoundOp::Div, true) => builder.ins().fdiv(current, rhs),
                    (CompoundOp::Mod, true) => return Err("float modulo not supported".to_string()),
                };

                builder.def_var(var, result);
                Ok(Terminator::None)
            }
            TirTag::WhileLoop => Self::generate_while_loop(builder, ctx, r),
            TirTag::ForRange => Self::generate_for_range(builder, ctx, r),
            TirTag::Break => {
                debug_assert!(
                    ctx.loop_stack.last().is_some(),
                    "break outside loop should be rejected by sema"
                );
                // Loop-exit Frees scheduled by the ownership pass are
                // anchored on this Break instruction and must fire
                // *before* the Cranelift `jump` terminator: the jump
                // seals the current block, and the post-stmt sweep in
                // `emit_body` skips Free emission on terminating
                // statements. Without this call the Frees would
                // simply never be emitted.
                Self::emit_due_frees(builder, ctx, r)?;
                let Some(loop_ctx) = ctx.loop_stack.last() else {
                    return Err("codegen reached break outside loop".to_string());
                };
                builder.ins().jump(loop_ctx.exit_block, &[]);
                Ok(Terminator::Break)
            }
            TirTag::Continue => {
                debug_assert!(
                    ctx.loop_stack.last().is_some(),
                    "continue outside loop should be rejected by sema"
                );
                // See Break above for why the Frees must be emitted
                // here instead of via the post-stmt sweep.
                Self::emit_due_frees(builder, ctx, r)?;
                let Some(loop_ctx) = ctx.loop_stack.last() else {
                    return Err("codegen reached continue outside loop".to_string());
                };
                builder.ins().jump(loop_ctx.continue_target, &[]);
                Ok(Terminator::Continue)
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
    ) -> Result<Terminator, String> {
        let view = ctx.tir.if_stmt_view(r);
        let merge_block = builder.create_block();

        // Pull the BranchId assignments allocated by the ownership
        // pass for this if. Default-empty if the sidecar has no entry
        // (e.g. an if with no Move-typed bindings live across it):
        // unconditional Frees still fire because their `branch` is
        // `None`, and there are no branch-gated entries to gate.
        let branch_ids = ctx.sidecar.if_branches.get(&r).cloned().unwrap_or_default();

        let cond_val = Self::eval_inst(builder, ctx, view.cond)?;
        let then_block = builder.create_block();

        let elif_count = view.elif_branches.len();
        let has_else = view.else_stmts.is_some();
        // An else-less if whose arms conditionally reseated a
        // binding needs a REAL fall-through block so the arm-gated
        // DeadDrops have somewhere to fire.
        let needs_fallthrough_block = !has_else
            && ctx
                .sidecar
                .conditional_dead_drops
                .iter()
                .any(|d| d.if_stmt == r);
        let capacity = elif_count + usize::from(has_else || needs_fallthrough_block);
        let mut next_blocks: Vec<Block> = Vec::with_capacity(capacity);
        for _ in 0..elif_count {
            next_blocks.push(builder.create_block());
        }
        let else_or_merge = if has_else || needs_fallthrough_block {
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
        // Manual push/pop (not RAII) — `?` propagation interacts
        // poorly with a scope-guard holding `&mut ctx`. We pop on
        // both Ok and Err paths by binding the result first.
        ctx.branch_stack.push(branch_ids.then_branch);
        Self::emit_conditional_dead_drops(builder, ctx, r, branch_ids.then_branch)?;
        let then_term_result = Self::emit_scoped_body(builder, ctx, &view.then_stmts);
        ctx.branch_stack.pop();
        let then_term = then_term_result?;
        if then_term == Terminator::None {
            builder.ins().jump(merge_block, &[]);
        }

        // Two separate questions the old bool conflated —
        // `all_terminated` (every arm ends the block, so the merge
        // block is unreachable) and `all_return` (every arm actually
        // returns, which is what the if reports to its caller).
        let mut all_terminated = then_term != Terminator::None;
        let mut all_return = then_term == Terminator::Return;
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
            let elif_branch_id = branch_ids.elif_branches.get(i).copied().unwrap_or_default();
            ctx.branch_stack.push(elif_branch_id);
            Self::emit_conditional_dead_drops(builder, ctx, r, elif_branch_id)?;
            let elif_term_result = Self::emit_scoped_body(builder, ctx, &elif.body);
            ctx.branch_stack.pop();
            let elif_term = elif_term_result?;
            if elif_term == Terminator::None {
                builder.ins().jump(merge_block, &[]);
            }
            all_terminated = all_terminated && elif_term != Terminator::None;
            all_return = all_return && elif_term == Terminator::Return;
        }

        if let Some(else_stmts) = &view.else_stmts {
            builder.seal_block(else_or_merge);
            builder.switch_to_block(else_or_merge);
            let else_branch_id = branch_ids.else_branch.unwrap_or_default();
            ctx.branch_stack.push(else_branch_id);
            Self::emit_conditional_dead_drops(builder, ctx, r, else_branch_id)?;
            let else_term_result = Self::emit_scoped_body(builder, ctx, else_stmts);
            ctx.branch_stack.pop();
            let else_term = else_term_result?;
            if else_term == Terminator::None {
                builder.ins().jump(merge_block, &[]);
            }
            all_terminated = all_terminated && else_term != Terminator::None;
            all_return = all_return && else_term == Terminator::Return;
        } else if needs_fallthrough_block {
            // The synthetic fall-through — emit the arm-gated
            // DeadDrops for the paths where no arm reseated the binding.
            builder.seal_block(else_or_merge);
            builder.switch_to_block(else_or_merge);
            let fallthrough_id = branch_ids.else_branch.unwrap_or_default();
            ctx.branch_stack.push(fallthrough_id);
            Self::emit_conditional_dead_drops(builder, ctx, r, fallthrough_id)?;
            ctx.branch_stack.pop();
            builder.ins().jump(merge_block, &[]);
            all_terminated = false;
            all_return = false;
        } else {
            all_terminated = false;
            all_return = false;
        }

        builder.seal_block(merge_block);
        if !all_terminated {
            builder.switch_to_block(merge_block);
        }

        // The if terminates the block only when every arm does; it
        // counts as a Return for the caller only when every arm
        // actually returns. For mixed all-terminating shapes (e.g.
        // break in one arm, return in another) the Break variant is a
        // stand-in: callers only distinguish None / Return /
        // "terminated some other way".
        Ok(if all_return {
            Terminator::Return
        } else if all_terminated {
            Terminator::Break
        } else {
            Terminator::None
        })
    }

    fn generate_while_loop(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
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
            exit_block,
            continue_target: header_block,
        });
        let body_term = Self::emit_scoped_body(builder, ctx, &view.body)?;
        ctx.loop_stack.pop();

        if body_term == Terminator::None {
            builder.ins().jump(header_block, &[]);
        }

        // Header has two predecessors: entry fallthrough and body back-edge.
        // Seal it last because the back-edge didn't exist until the body emitted.
        builder.seal_block(header_block);
        builder.seal_block(exit_block);
        builder.switch_to_block(exit_block);

        Ok(Terminator::None)
    }

    fn generate_for_range(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Terminator, String> {
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
        let shadowed_var = ctx.locals.insert(view.var_name, counter);

        let body_term = Self::emit_body(builder, ctx, &view.body)?;

        // Restore locals (loop variable goes out of scope)
        if let Some(old_var) = shadowed_var {
            ctx.locals.insert(view.var_name, old_var);
        } else {
            ctx.locals.remove(&view.var_name);
        }

        if body_term == Terminator::None {
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

        Ok(Terminator::None)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift::codegen::ir::Value as ClifValue;

    #[test]
    fn value_repr_scalar_roundtrip() {
        let v = ClifValue::from_u32(1);
        let repr = ValueRepr::Scalar(v);
        assert_eq!(repr.expect_scalar(), v);
    }

    #[test]
    fn value_repr_str_fields() {
        let repr = ValueRepr::Str {
            ptr: ClifValue::from_u32(1),
            len: ClifValue::from_u32(2),
            cap: ClifValue::from_u32(3),
        };
        match repr {
            ValueRepr::Str { ptr, len, cap } => {
                assert_ne!(ptr, len);
                assert_ne!(len, cap);
            }
            _ => panic!("expected Str"),
        }
    }

    #[test]
    #[should_panic(expected = "expected Scalar, got Str")]
    fn value_repr_expect_scalar_panics_on_str() {
        let repr = ValueRepr::Str {
            ptr: ClifValue::from_u32(1),
            len: ClifValue::from_u32(2),
            cap: ClifValue::from_u32(3),
        };
        repr.expect_scalar();
    }

    /// The three targets CI and the toolchain support: Linux x86-64,
    /// Windows x86-64 (MSVC ABI), macOS aarch64.
    const SUPPORTED_TRIPLES: [&str; 3] = [
        "x86_64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
        "aarch64-apple-darwin",
    ];

    /// Build a minimal function returning an i128 (the packed {ptr, len}
    /// shape the string runtime ABI uses) and compile it with the given
    /// flags. Returns the emitted machine code byte count.
    fn compile_i128_return(flags: settings::Flags, triple: &str) -> Result<usize, String> {
        let triple: Triple = triple
            .parse()
            .map_err(|e| format!("bad triple {triple}: {e}"))?;
        let isa = isa::lookup(triple)
            .map_err(|e| format!("isa lookup: {e}"))?
            .finish(flags)
            .map_err(|e| format!("isa build: {e}"))?;

        let mut sig = Signature::new(isa.default_call_conv());
        sig.returns.push(AbiParam::new(types::I128));
        let mut func = cranelift::codegen::ir::Function::with_name_signature(
            cranelift::codegen::ir::UserFuncName::user(0, 0),
            sig,
        );
        {
            let mut fb_ctx = FunctionBuilderContext::new();
            let frontend_config = isa.frontend_config();
            let mut fb = FunctionBuilder::new(&mut func, &mut fb_ctx);
            let block = fb.create_block();
            fb.switch_to_block(block);
            // iconst only supports i8-i64; build the i128 via uextend.
            let lo = fb.ins().iconst(types::I64, 42);
            let pair = fb.ins().uextend(types::I128, lo);
            fb.ins().return_(&[pair]);
            fb.seal_all_blocks();
            fb.finalize(frontend_config);
        }

        let mut ctx = cranelift::codegen::Context::for_function(func);
        ctx.compile(
            &*isa,
            &mut cranelift::codegen::control::ControlPlane::default(),
        )
        .map_err(|e| format!("compile: {e:?}"))?;
        let code = ctx
            .compiled_code()
            .ok_or_else(|| "no compiled code".to_string())?;
        Ok(code.code_buffer().len())
    }

    #[test]
    fn aot_i128_return_compiles_on_all_supported_targets() {
        // The packed-u128 string ABI puts an i128 in every producing
        // function's signature; the x64 ABI must accept it (LLVM ABI
        // extensions) on Linux AND Windows, and aarch64 must keep working.
        for triple in SUPPORTED_TRIPLES {
            let flags = aot_shared_flags().expect("shared flags");
            let len = compile_i128_return(flags, triple)
                .unwrap_or_else(|e| panic!("i128 return must compile for {triple}: {e}"));
            assert!(len > 0, "empty machine code for {triple}");
        }
    }

    #[test]
    fn x64_i128_return_panics_without_llvm_abi_extensions() {
        // Pins the failure mode this fix addresses: without the flag, the
        // x64 ABI rejects i128 in signatures. If this test starts failing
        // (no panic), Cranelift changed its gating — re-audit the flag.
        let mut b = settings::builder();
        b.set("opt_level", "speed").expect("opt_level");
        let flags = settings::Flags::new(b);
        for triple in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = compile_i128_return(flags.clone(), triple);
            }));
            let err = result.expect_err("i128 return must panic without llvm abi extensions");
            let msg = err
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            assert!(
                msg.contains("i128 args/return values not supported"),
                "unexpected panic for {triple}: {msg}"
            );
        }
    }
}
