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

use cranelift::codegen::ir::{ArgumentPurpose, BlockArg, FuncRef, StackSlot};
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

/// Fat-str triple layout (24 bytes): ptr at 0, len at 8, cap at 16.
/// View layout (16 bytes): ptr at 0, len at 8. Derived, not re-hardcoded.
const STR_SLOT_SIZE: u32 = 24;
const VIEW_SLOT_SIZE: u32 = 16;
const OFF_PTR: i32 = 0;
const OFF_LEN: i32 = 8;

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
/// reaching `cranelift_type_for`, which panics on `Str`.
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
        TypeKind::Str => panic!("cranelift_type_for: str is multi-value; use is_str_type gate"),
        TypeKind::View(_) => {
            panic!("cranelift_type_for: strview is two-word; use pool.is_view() gate")
        }
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
}

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
enum ValueRepr {
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
struct FunctionContext<'a, M: Module> {
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
    /// INVARIANT: this map is deliberately cross-block (one flat map
    /// per function, not scoped per basic block). That is sound only
    /// because the current TIR producers guarantee:
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
    inst_values: HashMap<TirRef, ValueRepr>,
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
        let mut jit_builder = JITBuilder::new(cranelift_module::default_libcall_names())
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
                        let p = builder.ins().load(int_type, MemFlags::trusted(), ptr, 0);
                        let l = builder.ins().load(types::I64, MemFlags::trusted(), ptr, 8);
                        let c = builder.ins().load(types::I64, MemFlags::trusted(), ptr, 16);
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
                        let cur = builder.ins().load(cl_ty, MemFlags::trusted(), ptr, 0);
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
                inst_values: HashMap::new(),
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
            };

            for (idx, param) in tir.params.iter().enumerate() {
                if is_str_type(param.ty, pool) {
                    let locals = ctx.str_locals.get(&param.name).unwrap();
                    let virtual_ref = TirRef::param(idx);
                    let repr = ValueRepr::Str {
                        ptr: builder.use_var(locals.ptr),
                        len: builder.use_var(locals.len),
                        cap: builder.use_var(locals.cap),
                    };
                    ctx.inst_values.insert(virtual_ref, repr);
                } else if pool.is_view(param.ty) {
                    let locals = ctx.view_locals.get(&param.name).unwrap();
                    let virtual_ref = TirRef::param(idx);
                    let repr = ValueRepr::View {
                        ptr: builder.use_var(locals.ptr),
                        len: builder.use_var(locals.len),
                    };
                    ctx.inst_values.insert(virtual_ref, repr);
                }
            }

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
                builder.ins().store(MemFlags::trusted(), p, *ptr, 0);
                builder.ins().store(MemFlags::trusted(), l, *ptr, 8);
                builder.ins().store(MemFlags::trusted(), c, *ptr, 16);
            } else {
                // Scalar pointee: a single store at offset 0.
                let var = ctx.locals.get(name).ok_or_else(|| {
                    format!(
                        "inout scalar '{}' has no local Variable",
                        ctx.pool.str(*name)
                    )
                })?;
                let val = builder.use_var(*var);
                builder.ins().store(MemFlags::trusted(), val, *ptr, 0);
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
                    builder.ins().store(MemFlags::trusted(), ptr, sret, 0);
                    builder.ins().store(MemFlags::trusted(), len, sret, 8);
                    builder.ins().store(MemFlags::trusted(), cap, sret, 16);
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

    /// Materialize an instruction's value, recursively materializing
    /// operand `TirRef`s as needed. Memoized: a second visit hands
    /// back the cached `Value`.
    fn eval_inst(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        if let Some(repr) = ctx.inst_values.get(&r) {
            return match repr {
                ValueRepr::Scalar(v) => Ok(*v),
                // Str/view-typed values have no scalar stand-in.
                // A multi-word repr reaching the scalar entry point
                // means a consumer forgot to gate through eval_inst_str
                // / eval_inst_view — reject loudly instead of silently
                // handing out the data pointer.
                ValueRepr::Str { .. } | ValueRepr::View { .. } => Err(format!(
                    "eval_inst: str/view-typed inst %{} reached the scalar entry point; use eval_inst_str / eval_inst_view",
                    r.index()
                )),
            };
        }
        let inst = ctx.tir.inst(r);
        // Str- and view-typed insts are multi-word and have no
        // business on the scalar path. Calls are checked separately in
        // the Call arm below (bare-statement str calls route through
        // emit_call / eval_inst_str instead).
        if inst.tag != TirTag::Call && (is_str_type(inst.ty, ctx.pool) || ctx.pool.is_view(inst.ty))
        {
            return Err(format!(
                "eval_inst: str/view-typed inst %{} reached the scalar entry point; use eval_inst_str / eval_inst_view",
                r.index()
            ));
        }
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
            TirTag::StrConst => {
                // Unreachable — the entry guard above rejects str-typed
                // insts. __ryo_panic's message pointer goes through
                // emit_strconst_rodata_ptr instead.
                Err(format!(
                    "eval_inst: StrConst %{} reached the scalar entry point",
                    r.index()
                ))?
            }
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
            TirTag::Call => {
                // Str/view-returning calls are multi-word — they
                // must come through eval_inst_str / eval_inst_view,
                // never the scalar path.
                if is_str_type(inst.ty, ctx.pool) || ctx.pool.is_view(inst.ty) {
                    return Err(format!(
                        "eval_inst: str/view-returning call %{} reached the scalar entry point; use eval_inst_str",
                        r.index()
                    ));
                }
                Self::emit_call(builder, ctx, r)?
            }
            TirTag::IfStmt => {
                Self::generate_if_stmt(builder, ctx, r)?;
                builder.ins().iconst(ctx.int_type, 0)
            }
            TirTag::StrLen => {
                let operand = match inst.data {
                    TirData::UnOp(r) => r,
                    _ => unreachable!("StrLen must carry TirData::UnOp"),
                };
                Self::eval_str_or_view_len(builder, ctx, operand)?
            }
            TirTag::StrCmpEq | TirTag::StrCmpNe => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!(),
                };
                // M8.4 §3.3: operands may be owned str triples or strview
                // view pairs (mixed equality wraps the owned side in
                // ViewOfStr); ryo_str_eq only needs (ptr, len).
                let (l_ptr, l_len) = Self::eval_str_or_view_parts(builder, ctx, lhs)?;
                let (r_ptr, r_len) = Self::eval_str_or_view_parts(builder, ctx, rhs)?;

                let eq_ref = Self::declare_runtime_fn(
                    ctx.module,
                    builder,
                    "ryo_str_eq",
                    &[ctx.int_type, types::I64, ctx.int_type, types::I64],
                    &[types::I8],
                )?;
                let call = builder.ins().call(eq_ref, &[l_ptr, l_len, r_ptr, r_len]);
                let result = builder.inst_results(call)[0];

                if inst.tag == TirTag::StrCmpNe {
                    let one = builder.ins().iconst(types::I8, 1);
                    builder.ins().bxor(result, one)
                } else {
                    result
                }
            }
            TirTag::StrConcat => {
                return Err("StrConcat must be materialized through eval_inst_str".to_string());
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
        // Scalar-only entry point: str/view-typed insts are
        // rejected above, so no path here can have cached a non-scalar
        // repr for `r` mid-evaluation.
        ctx.inst_values.insert(r, ValueRepr::Scalar(value));
        Ok(value)
    }

    /// Emit a string literal's raw `.rodata` pointer (no fat-pointer
    /// triple). Used by `__ryo_panic`'s scalar (ptr, len) ABI — the one
    /// deliberate exception to the rule that str-typed insts never
    /// flow through the scalar entry point.
    fn emit_strconst_rodata_ptr(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        id: StringId,
    ) -> Result<Value, String> {
        let content = ctx.pool.str(id);
        let data_id = store_string(id, content, ctx.module, ctx.data_ctx, ctx.string_data)?;
        let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
        Ok(builder.ins().global_value(ctx.int_type, data_ref))
    }

    /// Declare an external runtime function by name and return a
    /// `FuncRef` usable in the current function being built.
    fn declare_runtime_fn(
        module: &mut M,
        builder: &mut FunctionBuilder,
        name: &str,
        params: &[types::Type],
        returns: &[types::Type],
    ) -> Result<FuncRef, String> {
        let mut sig = module.make_signature();
        for &p in params {
            sig.params.push(AbiParam::new(p));
        }
        for &r in returns {
            sig.returns.push(AbiParam::new(r));
        }
        let func_id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| format!("Failed to declare {}: {}", name, e))?;
        Ok(module.declare_func_in_func(func_id, builder.func))
    }

    /// True if a `FreePoint` with the given `branch` tag is eligible
    /// to fire at the current point in codegen. Unconditional entries
    /// (`branch == None`) always pass; branch-gated entries fire only
    /// when their `BranchId` is on `branch_stack`. We use `contains`
    /// rather than `last() == Some(&b)` so a Free anchored to a
    /// parent arm still fires when codegen is inside a nested child
    /// arm of that parent.
    fn branch_active(
        branch: Option<ryo_core::ownership::BranchId>,
        stack: &[ryo_core::ownership::BranchId],
    ) -> bool {
        match branch {
            None => true,
            Some(b) => stack.contains(&b),
        }
    }

    /// Emit `ryo_str_free(ptr, cap)` for any scheduled Free whose
    /// anchor is `tir_ref` and whose `branch` tag is active on the
    /// current `branch_stack`. Called at the end of each
    /// materialisation (`eval_inst` / `eval_inst_str`) so that Task
    /// 4's anonymous-temporary Frees, anchored on the consuming
    /// `Call`, fire after the consumer has emitted its IR.
    ///
    /// Scheduled Frees only target `Str`-cached owners. A
    /// `Scalar`-cached target is an ownership-pass bug — the
    /// borrowed-scalar ABI never owns its argument and the ownership
    /// pass excludes such args from `temp_owners`. If a
    /// `Scalar` target is observed here, this function returns `Err`.
    ///
    /// `freed_at` (a set of `free_schedule` indices) guards against
    /// double-emission across the eval-end hooks and the end-of-stmt
    /// sweep.
    fn emit_due_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        tir_ref: TirRef,
    ) -> Result<(), String> {
        if ctx.sidecar.free_schedule.is_empty() {
            return Ok(());
        }
        let Some(indices) = ctx.free_by_after.get(&tir_ref) else {
            return Ok(());
        };
        let pending: Vec<(usize, TirRef)> = indices
            .iter()
            .copied()
            .filter(|&idx| {
                let fp = &ctx.sidecar.free_schedule[idx];
                Self::branch_active(fp.branch, &ctx.branch_stack) && !ctx.freed_at.contains(&idx)
            })
            .map(|idx| (idx, ctx.sidecar.free_schedule[idx].target))
            .collect();
        Self::emit_frees(builder, ctx, pending)
    }

    /// End-of-statement sweep: fire any scheduled Free whose anchor
    /// was materialised within the just-emitted statement but hasn't
    /// been emitted yet. This covers Task 3's last-use Frees where
    /// `after` is a sub-expression `Var` read — by the time the
    /// statement finishes, the consumer has already issued its IR,
    /// so a Free here lands after the consumer's use of the buffer.
    /// Eager firing during the inner `eval_inst_str(Var)` would have
    /// dropped the allocation before the consumer (e.g. `print`'s
    /// `write` syscall) finished reading from it.
    ///
    /// Branch-gated entries are filtered through `branch_active`, so
    /// only Frees whose `BranchId` is on the current `branch_stack`
    /// fire here.
    fn sweep_due_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
    ) -> Result<(), String> {
        if ctx.pending_sweep.is_empty() {
            return Ok(());
        }
        let pending: Vec<(usize, TirRef)> = ctx
            .pending_sweep
            .iter()
            .copied()
            .filter(|&idx| {
                let fp = &ctx.sidecar.free_schedule[idx];
                Self::branch_active(fp.branch, &ctx.branch_stack)
                    && ctx.inst_values.contains_key(&fp.after)
                    && ctx.inst_values.contains_key(&fp.target)
            })
            .map(|idx| (idx, ctx.sidecar.free_schedule[idx].target))
            .collect();
        Self::emit_frees(builder, ctx, pending)
    }

    /// Shared emission body for `emit_due_frees` / `sweep_due_frees`.
    /// Given the already-filtered `(free_schedule index, target)`
    /// pairs, declare `ryo_str_free` and emit one call per pair, marking
    /// each index as fired in `ctx.freed_at`. A `Scalar`-cached target
    /// (borrowed-scalar ABI, never heap-owned) returns an error and aborts
    /// code generation — the ABI registry is supposed to keep such args out
    /// of `temp_owners`.
    ///
    /// When the target is a named binding's initializer/value (or a str
    /// param's virtual ref), the Free is emitted from the binding's
    /// CURRENT `StrLocals` instead of the producing inst's cached repr:
    /// after a reassign, a branch merge, or an `inout` write-back the
    /// cached triple may be stale (freed/replaced), while the binding's
    /// `Variable`s are SSA-correct at every program point (the
    /// same reasoning the `free_on_reassign` path documents).
    fn emit_frees(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        pending: Vec<(usize, TirRef)>,
    ) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let free_ref = Self::declare_str_free(ctx.module, builder, ctx.int_type)?;
        for (idx, target) in pending {
            ctx.freed_at.insert(idx);
            let binding = ctx
                .free_binding_names
                .get(&target)
                .and_then(|name| ctx.str_locals.get(name).cloned());
            if let Some(sl) = binding {
                let ptr = builder.use_var(sl.ptr);
                let cap = builder.use_var(sl.cap);
                builder.ins().call(free_ref, &[ptr, cap]);
                continue;
            }
            let repr = ctx.inst_values.get(&target).copied().ok_or_else(|| {
                format!(
                    "ownership pass scheduled Free for %{} but no ValueRepr cached",
                    target.index()
                )
            })?;
            // M8.4: views are borrows, never owners — the ownership pass
            // must never schedule a Free for one (Task 9 invariant). The
            // repr check below doubles as the release-mode guard.
            debug_assert!(
                !matches!(repr, ValueRepr::View { .. }),
                "ownership pass scheduled Free for strview %{}; views are never freed",
                target.index()
            );
            match repr {
                ValueRepr::Str { ptr, cap, .. } => {
                    builder.ins().call(free_ref, &[ptr, cap]);
                }
                ValueRepr::View { .. } => {
                    return Err(format!(
                        "ownership pass scheduled Free for non-owning strview %{}; views are never owners",
                        target.index()
                    ));
                }
                ValueRepr::Scalar(_) => {
                    return Err(format!(
                        "ownership pass scheduled Free for borrowed-scalar value %{}; the ABI registry should have excluded it.",
                        target.index()
                    ));
                }
            }
        }
        ctx.pending_sweep.retain(|idx| !ctx.freed_at.contains(idx));
        Ok(())
    }

    /// Emit conditional DeadDrops for (`if_stmt`, `arm`): frees of
    /// the pre-if buffer of a conditionally-reassigned binding on the
    /// paths where the reassign did NOT happen. Fired at the START of an
    /// untouched arm, where the binding's `StrLocals` still hold the
    /// pre-if value. Resolves `target` through `free_binding_names` (the
    /// init→name map), so the freed buffer is the binding's
    /// current triple at that program point.
    fn emit_conditional_dead_drops(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        if_stmt: TirRef,
        arm: ryo_core::ownership::BranchId,
    ) -> Result<(), String> {
        for drop in ctx.sidecar.conditional_dead_drops.iter() {
            if drop.if_stmt != if_stmt || !drop.arms.contains(&arm) {
                continue;
            }
            let Some(name) = ctx.free_binding_names.get(&drop.target).copied() else {
                continue;
            };
            let Some(sl) = ctx.str_locals.get(&name).cloned() else {
                continue;
            };
            let free_ref = Self::declare_str_free(ctx.module, builder, ctx.int_type)?;
            let ptr = builder.use_var(sl.ptr);
            let cap = builder.use_var(sl.cap);
            builder.ins().call(free_ref, &[ptr, cap]);
        }
        Ok(())
    }

    /// Map every str-producing named initializer to its binding: VarDecl
    /// initializers, Assign values, and str params' virtual refs. Built
    /// once per function; `emit_frees` consults it to free a binding's
    /// current `StrLocals` rather than a stale cached repr.
    fn build_free_binding_names(tir: &Tir, pool: &InternPool) -> HashMap<TirRef, StringId> {
        fn walk(tir: &Tir, stmts: &[TirRef], map: &mut HashMap<TirRef, StringId>) {
            for &r in stmts {
                match tir.inst(r).tag {
                    TirTag::VarDecl => {
                        let view = tir.var_decl_view(r);
                        map.insert(view.initializer, view.name);
                    }
                    TirTag::Assign => {
                        let view = tir.assign_view(r);
                        map.insert(view.value, view.name);
                    }
                    TirTag::IfStmt => {
                        let view = tir.if_stmt_view(r);
                        walk(tir, &view.then_stmts, map);
                        for elif in &view.elif_branches {
                            walk(tir, &elif.body, map);
                        }
                        if let Some(else_stmts) = &view.else_stmts {
                            walk(tir, else_stmts, map);
                        }
                    }
                    TirTag::WhileLoop => walk(tir, &tir.while_loop_view(r).body, map),
                    TirTag::ForRange => walk(tir, &tir.for_range_view(r).body, map),
                    _ => {}
                }
            }
        }
        let mut map = HashMap::new();
        for (idx, param) in tir.params.iter().enumerate() {
            if is_str_type(param.ty, pool) {
                map.insert(TirRef::param(idx), param.name);
            }
        }
        walk(tir, &tir.body_stmts(), &mut map);
        map
    }

    /// Declare `extern "C" fn ryo_str_free(ptr: *mut u8, cap: u64)` for
    /// the function being built. Returns a `FuncRef` callable via
    /// `builder.ins().call(_, &[ptr, cap])`. `cap == 0` is a runtime
    /// no-op (covers static `.rodata` strings emitted by
    /// `ryo_str_from_literal`).
    fn declare_str_free(
        module: &mut M,
        builder: &mut FunctionBuilder,
        int_type: types::Type,
    ) -> Result<FuncRef, String> {
        Self::declare_runtime_fn(
            module,
            builder,
            "ryo_str_free",
            &[int_type, types::I64],
            &[],
        )
    }

    /// Materialize a str-typed TIR instruction, returning a
    /// `ValueRepr::Str` triple. Falls back to scalar `eval_inst`
    /// for non-str instructions.
    /// Shared sret pattern for runtime calls that produce a `str`
    /// through a caller-allocated stack slot: pass `args` plus the
    /// slot address, then reload the (ptr, len, cap) triple. Used by
    /// both the value path (`eval_inst_str`) and the bare-statement
    /// path (`emit_call`) so the two cannot drift.
    /// Does NOT touch `ctx.inst_values` — caching is the caller's job.
    fn emit_sret_str_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        fn_name: &str,
        args: &[(Type, Value)],
    ) -> Result<ValueRepr, String> {
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            STR_SLOT_SIZE,
            3,
        ));
        let out_ptr = builder.ins().stack_addr(ctx.int_type, slot, 0);

        let param_tys: Vec<Type> = args
            .iter()
            .map(|(ty, _)| *ty)
            .chain([ctx.int_type])
            .collect();
        let func_ref = Self::declare_runtime_fn(ctx.module, builder, fn_name, &param_tys, &[])?;
        let mut call_args: Vec<Value> = args.iter().map(|(_, val)| *val).collect();
        call_args.push(out_ptr);
        builder.ins().call(func_ref, &call_args);

        let ptr = builder
            .ins()
            .load(ctx.int_type, MemFlags::trusted(), out_ptr, 0);
        let len = builder
            .ins()
            .load(types::I64, MemFlags::trusted(), out_ptr, 8);
        let cap = builder
            .ins()
            .load(types::I64, MemFlags::trusted(), out_ptr, 16);
        Ok(ValueRepr::Str { ptr, len, cap })
    }

    fn eval_inst_str(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<ValueRepr, String> {
        if let Some(repr) = ctx.inst_values.get(&r) {
            return Ok(*repr);
        }
        let inst = ctx.tir.inst(r);
        let repr = match inst.tag {
            TirTag::StrConst => {
                let id = match inst.data {
                    TirData::Str(id) => id,
                    _ => unreachable!(),
                };
                Self::emit_str_literal_fat(builder, ctx, id)?
            }
            TirTag::Var => {
                let name = match inst.data {
                    TirData::Var(name) => name,
                    _ => unreachable!(),
                };
                if let Some(locals) = ctx.str_locals.get(&name) {
                    ValueRepr::Str {
                        ptr: builder.use_var(locals.ptr),
                        len: builder.use_var(locals.len),
                        cap: builder.use_var(locals.cap),
                    }
                } else {
                    // Not a str local — fall through to scalar
                    let val = Self::eval_inst(builder, ctx, r)?;
                    return Ok(ValueRepr::Scalar(val));
                }
            }
            TirTag::Call => {
                let view = ctx.tir.call_view(r);
                let name_str = ctx.pool.str(view.name);
                if name_str == "__ryo_str_from_view" {
                    // M8.4.1.2 `str(view)` materialization: the argument
                    // is a view pair evaluated via `eval_inst_view`.
                    let ValueRepr::View {
                        ptr: v_ptr,
                        len: v_len,
                    } = Self::eval_inst_view(builder, ctx, view.args[0])?
                    else {
                        unreachable!("__ryo_str_from_view argument must produce ValueRepr::View")
                    };
                    Self::emit_sret_str_call(
                        builder,
                        ctx,
                        "ryo_str_from_view",
                        &[(ctx.int_type, v_ptr), (types::I64, v_len)],
                    )?
                } else if name_str == "int_to_str"
                    || name_str == "float_to_str"
                    || name_str == "bool_to_str"
                {
                    let arg_val = Self::eval_inst(builder, ctx, view.args[0])?;
                    let (fn_name, param_ty) = match name_str {
                        "int_to_str" => ("ryo_int_to_str", ctx.int_type),
                        "float_to_str" => ("ryo_float_to_str", types::F64),
                        "bool_to_str" => ("ryo_bool_to_str", types::I8),
                        _ => unreachable!(),
                    };
                    Self::emit_sret_str_call(builder, ctx, fn_name, &[(param_ty, arg_val)])?
                } else {
                    // User call — emit_call handles sret for str-returning
                    // calls and caches ValueRepr::Str. Called directly
                    // (not via eval_inst): the scalar path rejects
                    // str-returning calls.
                    Self::emit_call(builder, ctx, r)?;
                    if let Some(repr) = ctx.inst_values.get(&r) {
                        return Ok(*repr);
                    }
                    unreachable!("str-returning user call must cache ValueRepr::Str via emit_call");
                }
            }
            TirTag::StrConcat => {
                let (lhs, rhs) = match inst.data {
                    TirData::BinOp { lhs, rhs } => (lhs, rhs),
                    _ => unreachable!(),
                };
                let l_repr = Self::eval_inst_str(builder, ctx, lhs)?;
                let r_repr = Self::eval_inst_str(builder, ctx, rhs)?;
                let (l_ptr, l_len) = match l_repr {
                    ValueRepr::Str { ptr, len, .. } => (ptr, len),
                    _ => unreachable!(),
                };
                let (r_ptr, r_len) = match r_repr {
                    ValueRepr::Str { ptr, len, .. } => (ptr, len),
                    _ => unreachable!(),
                };

                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    STR_SLOT_SIZE,
                    3,
                ));
                let out_ptr = builder.ins().stack_addr(ctx.int_type, slot, 0);

                let concat_ref = Self::declare_runtime_fn(
                    ctx.module,
                    builder,
                    "ryo_str_concat",
                    &[
                        ctx.int_type,
                        types::I64,
                        ctx.int_type,
                        types::I64,
                        ctx.int_type,
                    ],
                    &[],
                )?;
                builder
                    .ins()
                    .call(concat_ref, &[l_ptr, l_len, r_ptr, r_len, out_ptr]);

                let ptr = builder
                    .ins()
                    .load(ctx.int_type, MemFlags::trusted(), out_ptr, 0);
                let len = builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), out_ptr, 8);
                let cap = builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), out_ptr, 16);

                ValueRepr::Str { ptr, len, cap }
            }
            TirTag::ViewAsStr => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ViewAsStr must carry TirData::UnOp"),
                };
                // Re-borrow into the fat triple: cap=0 static sentinel,
                // identical to string literals. No allocation.
                let ValueRepr::View { ptr, len } = Self::eval_inst_view(builder, ctx, operand)?
                else {
                    unreachable!("ViewAsStr operand must produce ValueRepr::View")
                };
                let cap = builder.ins().iconst(types::I64, 0);
                ValueRepr::Str { ptr, len, cap }
            }
            _ => {
                // Delegate to scalar eval_inst for non-str instructions
                let val = Self::eval_inst(builder, ctx, r)?;
                return Ok(ValueRepr::Scalar(val));
            }
        };
        ctx.inst_values.insert(r, repr);
        Ok(repr)
    }

    /// Materialize a `strview`-typed TIR instruction as a `ValueRepr::View`
    /// pair (M8.4). Views are 16-byte non-owning `{ptr, len}` values —
    /// they never materialize into the 24-byte str triple and never
    /// enter the free schedule. Views do NOT go through `eval_inst`'s
    /// dummy-scalar pattern: only view-aware consumers
    /// (`print`, `StrLen`, `StrCmpEq/Ne`, call args, view bindings)
    /// reach them, via `eval_str_or_view_parts` or directly.
    fn eval_inst_view(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<ValueRepr, String> {
        if let Some(repr) = ctx.inst_values.get(&r) {
            return Ok(*repr);
        }
        let inst = ctx.tir.inst(r);
        let repr = match inst.tag {
            TirTag::Slice => {
                let (base, start, end) = match inst.data {
                    TirData::Slice { base, start, end } => (base, start, end),
                    _ => unreachable!("Slice must carry TirData::Slice"),
                };
                // Base may be an owned str (triple) or a view (pair).
                let (base_ptr, base_len) = Self::eval_str_or_view_parts(builder, ctx, base)?;
                let start_v = match start {
                    Some(s) => Self::eval_inst(builder, ctx, s)?,
                    None => builder.ins().iconst(types::I64, 0),
                };
                let end_v = match end {
                    Some(e) => Self::eval_inst(builder, ctx, e)?,
                    None => base_len,
                };
                let slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    VIEW_SLOT_SIZE,
                    3,
                ));
                let out_ptr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                let slice_ref = Self::declare_runtime_fn(
                    ctx.module,
                    builder,
                    "__ryo_slice",
                    &[
                        ctx.int_type,
                        types::I64,
                        types::I64,
                        types::I64,
                        ctx.int_type,
                    ],
                    &[],
                )?;
                builder
                    .ins()
                    .call(slice_ref, &[base_ptr, base_len, start_v, end_v, out_ptr]);
                let ptr = builder
                    .ins()
                    .load(ctx.int_type, MemFlags::trusted(), out_ptr, OFF_PTR);
                let len = builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), out_ptr, OFF_LEN);
                ValueRepr::View { ptr, len }
            }
            TirTag::ViewOfStr => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ViewOfStr must carry TirData::UnOp"),
                };
                // Representation conversion only: drop the cap word.
                let ValueRepr::Str { ptr, len, .. } = Self::eval_inst_str(builder, ctx, operand)?
                else {
                    unreachable!("ViewOfStr operand must produce ValueRepr::Str")
                };
                ValueRepr::View { ptr, len }
            }
            TirTag::Var => {
                let name = match inst.data {
                    TirData::Var(name) => name,
                    _ => unreachable!("Var must carry TirData::Var"),
                };
                let locals = ctx.view_locals.get(&name).ok_or_else(|| {
                    format!("Undefined strview variable: '{}'", ctx.pool.str(name))
                })?;
                ValueRepr::View {
                    ptr: builder.use_var(locals.ptr),
                    len: builder.use_var(locals.len),
                }
            }
            TirTag::Call => {
                // Sema rejects `strview` return types (Rule 5), so no call
                // can produce a view today.
                return Err(
                    "eval_inst_view: calls returning strview are rejected by sema (Rule 5)"
                        .to_string(),
                );
            }
            other => {
                return Err(format!(
                    "eval_inst_view: instruction at %{} is not a strview value (tag={:?})",
                    r.index(),
                    other
                ));
            }
        };
        ctx.inst_values.insert(r, repr);
        Ok(repr)
    }

    /// Evaluate a `str`/`strview`-typed operand and hand back its
    /// `(ptr, len)` words regardless of representation — owned triple
    /// or borrowed view pair (M8.4). Consumers that only need the
    /// viewed bytes (`print`, `StrLen`, `StrCmpEq/Ne`, the
    /// `__ryo_str_push` suffix, the `__ryo_slice` base) use this;
    /// anything needing the cap must stay on `eval_inst_str`.
    fn eval_str_or_view_parts(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<(Value, Value), String> {
        let ty = ctx.tir.inst(r).ty;
        if ctx.pool.is_view(ty) {
            let ValueRepr::View { ptr, len } = Self::eval_inst_view(builder, ctx, r)? else {
                unreachable!("eval_inst_view must produce ValueRepr::View");
            };
            return Ok((ptr, len));
        }
        match Self::eval_inst_str(builder, ctx, r)? {
            ValueRepr::Str { ptr, len, .. } => Ok((ptr, len)),
            ValueRepr::View { ptr, len } => Ok((ptr, len)),
            ValueRepr::Scalar(_) => Err(format!(
                "eval_str_or_view_parts: instruction at %{} is not a str/strview value",
                r.index()
            )),
        }
    }

    /// The `len` word of a `str`/`strview`-typed operand, from either
    /// representation (M8.4). Backs the `StrLen` arm.
    fn eval_str_or_view_len(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let (_, len) = Self::eval_str_or_view_parts(builder, ctx, r)?;
        Ok(len)
    }

    /// Emit a string literal as a fat pointer triple (ptr, len, cap)
    /// by calling `ryo_str_from_literal` at runtime.
    fn emit_str_literal_fat(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        id: StringId,
    ) -> Result<ValueRepr, String> {
        let content = ctx.pool.str(id);
        let data_id = store_string(id, content, ctx.module, ctx.data_ctx, ctx.string_data)?;
        let data_ref = ctx.module.declare_data_in_func(data_id, builder.func);
        let rodata_ptr = builder.ins().global_value(ctx.int_type, data_ref);
        let lit_len = builder.ins().iconst(types::I64, content.len() as i64);

        // Allocate 24-byte stack slot for out parameter (8-byte aligned)
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            STR_SLOT_SIZE,
            3,
        ));
        let out_ptr = builder.ins().stack_addr(ctx.int_type, slot, 0);

        // Call ryo_str_from_literal(data, len, out)
        let from_literal_ref = Self::declare_runtime_fn(
            ctx.module,
            builder,
            "ryo_str_from_literal",
            &[ctx.int_type, types::I64, ctx.int_type],
            &[],
        )?;
        builder
            .ins()
            .call(from_literal_ref, &[rodata_ptr, lit_len, out_ptr]);

        // Load the triple back from the stack slot
        let ptr = builder
            .ins()
            .load(ctx.int_type, MemFlags::trusted(), out_ptr, 0);
        let len = builder
            .ins()
            .load(types::I64, MemFlags::trusted(), out_ptr, 8);
        let cap = builder
            .ins()
            .load(types::I64, MemFlags::trusted(), out_ptr, 16);

        Ok(ValueRepr::Str { ptr, len, cap })
    }

    fn emit_call(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let view = ctx.tir.call_view(r);
        let name_id = view.name;
        let name_str = ctx.pool.str(name_id);

        // print and __ryo_panic are ordinary runtime calls. They
        // do NOT use the str-triple expansion that user functions use.
        if name_str == "__ryo_panic" {
            // __ryo_panic(ptr, len) keeps its raw scalar ABI — the StrConst
            // .rodata pointer and an int len — now backed by ryo_panic in
            // the runtime (stderr + exit 101). The trap after the call is
            // unreachable in practice; it keeps Cranelift honest about the
            // never-returns contract.
            let mut arg_values = Vec::with_capacity(view.args.len());
            for arg in &view.args {
                // The message is a StrConst whose .rodata pointer the
                // scalar (ptr, len) ABI consumes directly — the one
                // deliberate exception to the scalar-path rule.
                match ctx.tir.inst(*arg).data {
                    TirData::Str(id) => {
                        arg_values.push(Self::emit_strconst_rodata_ptr(builder, ctx, id)?)
                    }
                    _ => arg_values.push(Self::eval_inst(builder, ctx, *arg)?),
                }
            }
            let panic_ref = Self::declare_runtime_fn(
                ctx.module,
                builder,
                "ryo_panic",
                &[ctx.int_type, ctx.int_type],
                &[],
            )?;
            builder.ins().call(panic_ref, &arg_values);
            builder.ins().trap(TrapCode::user(1).unwrap());
            let dead = builder.create_block();
            builder.seal_block(dead);
            builder.switch_to_block(dead);
            return Ok(builder.ins().iconst(types::I8, 0));
        }

        if name_str == "print" {
            // print is an ordinary runtime call. Accepts either
            // repr — owned str triple or strview pair; ryo_print(ptr,
            // len) only needs the viewed bytes.
            debug_assert_eq!(
                view.args.len(),
                1,
                "sema should reject print() arity errors"
            );
            debug_assert!(
                matches!(
                    ctx.pool.kind(ctx.tir.inst(view.args[0]).ty),
                    TypeKind::Str | TypeKind::View(_)
                ),
                "sema should reject non-str print() args",
            );
            let (ptr, len) = Self::eval_str_or_view_parts(builder, ctx, view.args[0])?;
            let print_ref = Self::declare_runtime_fn(
                ctx.module,
                builder,
                "ryo_print",
                &[ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(print_ref, &[ptr, len]);
            return Ok(builder.ins().iconst(ctx.int_type, 0));
        }

        if name_str == "str_push" {
            // str_push(&s, suffix): spill s's fat pointer to a 24-byte
            // slot, call __ryo_str_push(slot_addr, suffix_ptr, suffix_len),
            // then reload the mutated triple back into s's StrLocals.
            // arg 0 is `&s` (lowered to Var(s)); arg 1 is the suffix str.
            let s_ref = view.args[0];
            let suffix_ref = view.args[1];
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                STR_SLOT_SIZE,
                3,
            ));
            let s_addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
            let s_repr = Self::eval_inst_str(builder, ctx, s_ref)?;
            let ValueRepr::Str { ptr, len, cap } = s_repr else {
                unreachable!("str_push target must be a str");
            };
            builder.ins().store(MemFlags::trusted(), ptr, s_addr, 0);
            builder.ins().store(MemFlags::trusted(), len, s_addr, 8);
            builder.ins().store(MemFlags::trusted(), cap, s_addr, 16);
            // M8.4: the suffix may be either repr — an owned `str`
            // passes its ptr+len, a slice/view passes directly (no
            // ViewOfStr wrap: builtins bypass check_call's §3.4
            // conversion, so sema accepts `Str | View(_)` here).
            let (suf_ptr, suf_len) = Self::eval_str_or_view_parts(builder, ctx, suffix_ref)?;
            let func_ref = Self::declare_runtime_fn(
                ctx.module,
                builder,
                "__ryo_str_push",
                &[ctx.int_type, ctx.int_type, types::I64],
                &[],
            )?;
            builder.ins().call(func_ref, &[s_addr, suf_ptr, suf_len]);
            // Reload the mutated fat pointer back into the caller's StrLocals.
            let np = builder
                .ins()
                .load(ctx.int_type, MemFlags::trusted(), s_addr, 0);
            let nl = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), s_addr, 8);
            let nc = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), s_addr, 16);
            if let Some(name) = Self::local_name_of(ctx, s_ref)
                && let Some(sl) = ctx.str_locals.get(&name).cloned()
            {
                builder.def_var(sl.ptr, np);
                builder.def_var(sl.len, nl);
                builder.def_var(sl.cap, nc);
            }
            return Ok(builder.ins().iconst(ctx.int_type, 0));
        }

        let callee_id = *ctx
            .func_ids
            .get(&name_id)
            .ok_or_else(|| format!("Undefined function: '{}'", name_str))?;

        let mut arg_values = Vec::with_capacity(view.args.len() * 3 + 1);
        // inout args: spill the current value to a stack slot, pass the
        // slot address, then reload after the call. Scalar spills one
        // field; str spills the fat-pointer triple.
        let mut inout_reloads: Vec<(TirRef, StackSlot)> = Vec::new();
        for (i, arg) in view.args.iter().enumerate() {
            let mode = view.modes.get(i).copied().ok_or_else(|| {
                format!(
                    "internal error: call '{name_str}' has {} args but {} modes",
                    view.args.len(),
                    view.modes.len()
                )
            })?;
            let arg_ty = ctx.tir.inst(*arg).ty;
            if mode == ParamMode::Inout {
                if is_str_type(arg_ty, ctx.pool) {
                    let slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        STR_SLOT_SIZE,
                        3,
                    ));
                    let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                    let repr = Self::eval_inst_str(builder, ctx, *arg)?;
                    let ValueRepr::Str { ptr, len, cap } = repr else {
                        unreachable!("inout str arg must produce ValueRepr::Str");
                    };
                    builder.ins().store(MemFlags::trusted(), ptr, addr, 0);
                    builder.ins().store(MemFlags::trusted(), len, addr, 8);
                    builder.ins().store(MemFlags::trusted(), cap, addr, 16);
                    arg_values.push(addr);
                    inout_reloads.push((*arg, slot));
                } else {
                    let cl_ty = cranelift_type_for(arg_ty, ctx.pool, ctx.int_type);
                    let bytes = cl_ty.bytes().max(8);
                    let slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        bytes,
                        3,
                    ));
                    let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                    let cur = Self::eval_inst(builder, ctx, *arg)?;
                    builder.ins().store(MemFlags::trusted(), cur, addr, 0);
                    arg_values.push(addr);
                    inout_reloads.push((*arg, slot));
                }
            } else if is_str_type(arg_ty, ctx.pool) {
                let repr = Self::eval_inst_str(builder, ctx, *arg)?;
                match repr {
                    ValueRepr::Str { ptr, len, cap } => {
                        arg_values.push(ptr);
                        arg_values.push(len);
                        arg_values.push(cap);
                    }
                    _ => unreachable!("str-typed arg must produce ValueRepr::Str"),
                }
            } else if ctx.pool.is_view(arg_ty) {
                // `strview` arg → 2-word ABI (ptr, len), matching the
                // callee's build_signature. Sema has already inserted
                // ViewOfStr for owned-str actuals (§3.4).
                let (ptr, len) = Self::eval_str_or_view_parts(builder, ctx, *arg)?;
                arg_values.push(ptr);
                arg_values.push(len);
            } else {
                arg_values.push(Self::eval_inst(builder, ctx, *arg)?);
            }
        }

        let callee_ref = ctx.module.declare_func_in_func(callee_id, builder.func);

        let ret_ty = ctx.tir.inst(r).ty;

        // If the callee returns never (e.g. __ryo_panic), the call is
        // a terminator. Emit a trap + dead block for subsequent IR.
        if ctx.pool.is_never(ret_ty) {
            builder.ins().call(callee_ref, &arg_values);
            // Reload inout slots before the trap: Cranelift models the
            // callee as an ordinary (returning) call, so the mutations
            // must be visible on the path where control resumes.
            Self::reload_inout_args(builder, ctx, &inout_reloads)?;
            builder.ins().trap(TrapCode::user(1).unwrap());
            let dead = builder.create_block();
            builder.seal_block(dead);
            builder.switch_to_block(dead);
            let dummy_ty = cranelift_type_for(ret_ty, ctx.pool, ctx.int_type);
            return Ok(builder.ins().iconst(dummy_ty, 0));
        }

        if is_str_type(ret_ty, ctx.pool) {
            // sret: allocate 24-byte slot, prepend pointer to args
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                STR_SLOT_SIZE,
                3,
            ));
            let out = builder.ins().stack_addr(ctx.int_type, slot, 0);

            let mut all_args = Vec::with_capacity(arg_values.len() + 1);
            all_args.push(out);
            all_args.extend(arg_values);

            builder.ins().call(callee_ref, &all_args);
            Self::reload_inout_args(builder, ctx, &inout_reloads)?;

            let ptr = builder
                .ins()
                .load(ctx.int_type, MemFlags::trusted(), out, 0);
            let len = builder.ins().load(types::I64, MemFlags::trusted(), out, 8);
            let cap = builder.ins().load(types::I64, MemFlags::trusted(), out, 16);
            ctx.inst_values.insert(r, ValueRepr::Str { ptr, len, cap });
            return Ok(ptr); // dummy scalar — consumers use eval_inst_str
        }

        let call = builder.ins().call(callee_ref, &arg_values);
        Self::reload_inout_args(builder, ctx, &inout_reloads)?;
        let results = builder.inst_results(call);

        if results.is_empty() {
            Ok(builder.ins().iconst(ctx.int_type, 0))
        } else {
            Ok(results[0])
        }
    }

    /// Reload each inout slot after a call and write the updated value
    /// back into the caller's local. The inout arg was sema-lowered to
    /// its inner `Var(name)` ref, so `*arg_ref` is that `Var` inst —
    /// read its binding name to find the local. Scalar args reload one
    /// field into `locals`; str args reload the fat-pointer triple into
    /// `str_locals`.
    fn reload_inout_args(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        reloads: &[(TirRef, StackSlot)],
    ) -> Result<(), String> {
        for (arg_ref, slot) in reloads {
            let addr = builder.ins().stack_addr(ctx.int_type, *slot, 0);
            let arg_ty = ctx.tir.inst(*arg_ref).ty;
            if is_str_type(arg_ty, ctx.pool) {
                let np = builder
                    .ins()
                    .load(ctx.int_type, MemFlags::trusted(), addr, 0);
                let nl = builder.ins().load(types::I64, MemFlags::trusted(), addr, 8);
                let nc = builder
                    .ins()
                    .load(types::I64, MemFlags::trusted(), addr, 16);
                if let Some(name) = Self::local_name_of(ctx, *arg_ref)
                    && let Some(sl) = ctx.str_locals.get(&name).cloned()
                {
                    builder.def_var(sl.ptr, np);
                    builder.def_var(sl.len, nl);
                    builder.def_var(sl.cap, nc);
                }
            } else {
                let cl_ty = cranelift_type_for(arg_ty, ctx.pool, ctx.int_type);
                let updated = builder.ins().load(cl_ty, MemFlags::trusted(), addr, 0);
                if let Some(name) = Self::local_name_of(ctx, *arg_ref)
                    && let Some(var) = ctx.locals.get(&name).copied()
                {
                    builder.def_var(var, updated);
                }
            }
        }
        Ok(())
    }

    /// Returns the binding name when `r` is a `TirTag::Var` inst, else
    /// `None`. Used to resolve an inout arg (lowered to its inner
    /// `Var(name)`) back to the caller local that must receive the
    /// reloaded value.
    fn local_name_of(ctx: &FunctionContext<'_, M>, r: TirRef) -> Option<StringId> {
        let inst = ctx.tir.inst(r);
        match inst.tag {
            TirTag::Var => match inst.data {
                TirData::Var(name) => Some(name),
                _ => None,
            },
            _ => None,
        }
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
}
