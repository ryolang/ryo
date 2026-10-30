//! Cranelift codegen over TIR, driven by the canonical Flow IR.
//!
//! Codegen consumes the typed instruction streams produced by
//! `sema` (one [`Tir`] per function body) and lowers them to
//! Cranelift IR. There is no [`crate::uir::Uir`] import here:
//! every operand is already typed, every variable already
//! resolved. Control structure comes from the Flow IR — one
//! Cranelift block per Flow block, walked in DFS order from the
//! entry (`codegen/control.rs` explains why raw construction order is
//! not sufficient) —
//! never from TIR statement lists: TIR remains readable for
//! spans, types, and operation payloads (see `codegen/control.rs`
//! for the driver and its construct discipline).
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
//! 2. The `eval_inst` memoization table (dense `Vec<Option<ValueRepr>>`
//!    indexed by `TirRef::index()`)
//!    so a shared sub-expression isn't re-emitted. TIR today is
//!    tree-shaped (one parent per inst) so this is purely
//!    defensive — but it's the right invariant before lazy sema
//!    / inline expansion lands. Zig calls the analogous mapping
//!    in `Air.zig` "liveness"; we don't need full liveness yet.

use cranelift::codegen::ir::{MemFlagsData, StackSlot, StackSlotData, StackSlotKind};
use cranelift::codegen::isa;
use cranelift::codegen::settings::{self, Configurable};
use cranelift::prelude::*;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use ryo_core::tir::{ParamMode, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeId, TypeKind};
use std::collections::{HashMap, HashSet};
use target_lexicon::Triple;

mod abi;
use abi::{LoweredAbi, ValueAbi};
mod arith;
mod binding_storage;
mod builtins;
mod bytes;
mod control;
mod decls;
mod enums;
mod errors;
mod expr;
mod frees;
mod function_context;
mod input;
mod jit;
mod literals;
mod match_;
mod ranges;
mod recursion;
mod str_ops;
mod structs;
mod tail;
mod views;

pub(crate) use structs::{is_fat_type, is_struct_type};

pub(crate) use enums::is_enum_type;

pub(crate) use function_context::FunctionContext;

/// Fat-owner triple layout (str/bytes, 24 bytes): ptr at 0, len at 8,
/// cap at 16. Derived from `RyoStrFat`, not re-hardcoded.
const STR_SLOT_SIZE: u32 = 24;

/// Promotion scratch-slot layout for borrowed-param view bases: flag
/// word at 0 (1 = this callee promoted, 0 = pass-through), triple
/// ptr/len/cap at 8/16/24.
pub(crate) const PROMO_SLOT_SIZE: u32 = 32;

/// How a statement-level emitter ended the current block, if it did.
/// The Flow-block driver dispatches control instructions as
/// block terminators, so the only statement results left are "did not
/// end the block" and "returned" (the struct/enum return paths).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Terminator {
    None,
    Return,
}

/// Builtins whose fat result codegen materializes inline — no runtime
/// call, no slot-out write. `writes_out_slot` must exclude exactly
/// these: the inlined arm in `eval_inst_fat_slot` ignores its
/// `out_slot` parameter, so handing one a home slot would leave the
/// slot unwritten and every later home read would load garbage.
/// Keeping the set wrong in the other direction (listing a builtin
/// that is NOT inlined) is harmless — it only forgoes the home.
///
/// Test-only: the production exclusion lives in `writes_out_slot_ids`
/// as an interned-id compare, and the unit test below iterates this
/// table to assert both stay in agreement.
#[cfg(test)]
pub(crate) const CODEGEN_INLINED_BUILTINS: &[&str] = &["bool_to_str"];

/// True for instruction tags whose Flow IR steps are statement
/// boundaries: after such a step the driver fires the end-of-statement
/// free bookkeeping at resolved firing points, matching the old `emit_body`
/// post-statement pass. Control instructions are never steps.
pub(crate) fn is_statement_step(tag: TirTag) -> bool {
    matches!(
        tag,
        TirTag::VarDecl
            | TirTag::Assign
            | TirTag::CompoundAssign
            | TirTag::FieldAssign
            | TirTag::CompoundFieldAssign
            | TirTag::Destructure
            | TirTag::ExprStmt
    )
}

/// Interned-name ids resolved once per compilation (in `compile_all`)
/// so per-function and per-instruction name dispatch — `main`
/// detection, the `__ryo_panic` / `print` / `io_eprint` /
/// `process_exit` / `process_argc` call-form routing, the fat-producer
/// slot-out chain (including `process_argv` / `process_env` /
/// `io_read_line`), the codegen-inlined builtin exclusion, the push
/// mutation scan — is
/// a `StringId` equality check instead of `pool.str(id) == "..."`.
/// The BUILTINS-name probes always resolve to `Some`: `Sema::new`
/// interns every BUILTINS name up front, and astgen interns `main`.
/// Only the `__ryo_*` synthesis names can stay `None` — sema interns
/// each lazily, only when it emits the corresponding rewrite, so a
/// program that never triggers one never has it in the pool. A `None`
/// id simply never matches.
#[derive(Clone, Copy, Default)]
pub(crate) struct CodegenNameIds {
    main: Option<StringId>,
    ryo_panic: Option<StringId>,
    print: Option<StringId>,
    io_eprint: Option<StringId>,
    process_exit: Option<StringId>,
    process_argc: Option<StringId>,
    process_argv: Option<StringId>,
    process_env: Option<StringId>,
    io_read_line: Option<StringId>,
    bool_to_str: Option<StringId>,
    int_to_str: Option<StringId>,
    float_to_str: Option<StringId>,
    str_push: Option<StringId>,
    bytes_push: Option<StringId>,
    str_from_view: Option<StringId>,
    bytes_from_view: Option<StringId>,
    str_to_bytes: Option<StringId>,
    bytes_to_str: Option<StringId>,
    bytes_repr: Option<StringId>,
}

impl CodegenNameIds {
    fn resolve(pool: &InternPool) -> Self {
        CodegenNameIds {
            main: pool.find_str("main"),
            ryo_panic: pool.find_str("__ryo_panic"),
            print: pool.find_str("print"),
            io_eprint: pool.find_str("io_eprint"),
            process_exit: pool.find_str("process_exit"),
            process_argc: pool.find_str("process_argc"),
            process_argv: pool.find_str("process_argv"),
            process_env: pool.find_str("process_env"),
            io_read_line: pool.find_str("io_read_line"),
            bool_to_str: pool.find_str("bool_to_str"),
            int_to_str: pool.find_str("int_to_str"),
            float_to_str: pool.find_str("float_to_str"),
            str_push: pool.find_str("str_push"),
            bytes_push: pool.find_str("bytes_push"),
            str_from_view: pool.find_str("__ryo_str_from_view"),
            bytes_from_view: pool.find_str("__ryo_bytes_from_view"),
            str_to_bytes: pool.find_str("__ryo_str_to_bytes"),
            bytes_to_str: pool.find_str("__ryo_bytes_to_str"),
            bytes_repr: pool.find_str("__ryo_bytes_repr"),
        }
    }
}

/// Map a TIR type to the corresponding Cranelift IR type.
///
/// `Int` uses the target's pointer-sized integer (i64 on 64-bit).
/// `Bool` uses I8 (matches Cranelift's `icmp` result width and Rust's bool layout).
/// `Str`/`Bytes` are fat pointers (ptr, len, cap) — they cannot map to a
/// single type; callers must gate with `is_fat_type` before reaching this
/// function.
/// Views (`strview`, M8.4) are likewise multi-word `{ptr, len}`; callers
/// must gate with `pool.is_view()` before reaching this function.
/// `Void` has no Cranelift representation and should not be mapped here.
fn cranelift_type_for(ty: TypeId, pool: &InternPool, pointer_ty: types::Type) -> types::Type {
    match pool.kind(ty) {
        TypeKind::Int => pointer_ty,
        TypeKind::Str | TypeKind::Bytes => {
            unreachable!("cranelift_type_for: fat type is multi-value; use is_fat_type gate")
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
        TypeKind::AnonStruct => {
            // Anonymous structs (M10) are aggregates exactly like named
            // ones: every struct-typed value is gated to the struct
            // slot paths before codegen, so reaching the scalar type
            // mapping is a caller bug — same contract as `Struct`.
            unreachable!("cranelift_type_for: anon struct TypeId reached codegen")
        }
        // Struct codegen (aggregate layout) lands in a later M9 task.
        TypeKind::Struct => unreachable!("cranelift_type_for: struct TypeId reached codegen"),
        // Enum-shaped values use slot-allocated aggregates exactly like
        // structs; the memory-first paths gate before scalar mapping.
        TypeKind::Enum | TypeKind::DeclaredError | TypeKind::ErrorSet | TypeKind::ErrorUnion => {
            unreachable!("cranelift_type_for: enum-shaped TypeId reached codegen")
        }
        TypeKind::InferredErrorUnion => {
            unreachable!("cranelift_type_for: unresolved error union reached codegen")
        }
    }
}

pub struct Codegen<M: Module> {
    optional_optimizations: bool,
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
    /// (division and checked-arithmetic guards). These strings never
    /// pass through the `InternPool`, so they are keyed on the static
    /// text itself.
    guard_msg_data: HashMap<&'static str, DataId>,
    /// Module-level name → `FuncId` cache for imported runtime
    /// functions: one `declare_function` import per symbol per module,
    /// regardless of how many call sites (or functions) use it.
    /// `FuncId`s are module-scoped, so this is valid for every
    /// function compiled into the same module; the per-function
    /// `FuncRef` is derived cheaply via `declare_func_in_func`.
    runtime_fns: HashMap<&'static str, FuncId>,
    /// Interned-name ids for per-function / per-instruction dispatch
    /// (`main` detection, call-form routing, the slot-out producer
    /// chain). Resolved from the pool at the top of `compile_all` and
    /// valid for the duration of that compilation; `Default` (all
    /// `None`) only until the first compile.
    name_ids: CodegenNameIds,
    /// Names of functions whose bodies contain a syntactic
    /// tail-position self-call, collected by the pre-pass in
    /// `prepare_compilation`. `lower_abi` compiles marked
    /// functions with `CallConv::Tail` — the only convention from
    /// which Cranelift allows `return_call`. Recomputed per
    /// compilation; empty before the first one.
    tail_candidates: HashSet<StringId>,
    /// Strongly-connected-component id of every function on a
    /// call-graph cycle, from `recursion::scan_recursive_sccs`
    /// in `prepare_compilation`. Calls between two functions sharing
    /// an id are the only ones that get the stack-limit check.
    /// Recomputed per compilation; empty before the first one.
    recursive_sccs: HashMap<StringId, u32>,
}

/// The drop fire map (see `build_drop_fire_map`): the three
/// anchor-keyed dense buckets, the block-keyed at-terminator
/// bucket, and the consumed flags.
struct DropFireMap(
    Vec<Vec<usize>>,
    Vec<Vec<usize>>,
    Vec<Vec<usize>>,
    HashMap<ryo_core::flow::BlockId, Vec<usize>>,
    HashMap<ryo_core::flow::BlockId, Vec<usize>>,
    Vec<bool>,
);

/// Overflow guard message for the spec §18 checked-arithmetic traps.
const OVERFLOW_MSG: &str = "integer overflow\n";

/// Declared functions: `FuncId` plus the `Signature` built at
/// declaration time, so `compile_function` can install it into
/// `ctx.func` instead of rebuilding it.
type DeclaredFunctions = HashMap<StringId, (FuncId, LoweredAbi)>;

/// Undo-log marks for the four binding slot tables (`locals`,
/// `fat_locals`, `view_locals`, `struct_locals`), captured when a
/// control construct (if-cascade / loop / match) begins. Region-entry
/// blocks and the construct's join restore the tables to these marks,
/// giving every sibling region the pre-construct table state without
/// any recursive body driver (the flat Flow-block driver has no
/// enclosing scope to replay).
#[derive(Clone, Copy, Default)]
pub(crate) struct SlotMarks {
    pub(crate) locals: usize,
    pub(crate) fat_locals: usize,
    pub(crate) view_locals: usize,
    pub(crate) struct_locals: usize,
}

/// Per-`IfStmt` codegen state for the Flow-block driver.
/// Created when the cascade-entry block's terminator is seen (BEFORE
/// its condition steps emit, so the marks predate condition
/// evaluations), consumed by the arm/elif/else/join block prologues.
pub(crate) struct IfRecord {
    /// `range_facts_undo.len()` before the cascade's condition.
    pub(crate) outer_facts_mark: usize,
    /// `assigned_log.len()` before the cascade.
    pub(crate) scope_mark: usize,
    /// Slot-table marks before the cascade.
    pub(crate) slot_marks: SlotMarks,
    /// Conditions whose FALSE path dominates later cascade blocks
    /// (each elif cond block and the else arm): the entry cond first,
    /// each elif cond appended when its branch emits.
    pub(crate) negated_conds: Vec<TirRef>,
    /// Whether the TIR carries an else arm (the Flow IR's else block
    /// covers both the else body and the else-less fallthrough; only
    /// the view distinguishes them).
    pub(crate) has_else: bool,
}

/// Per-`while` state: the post-body restore targets (marks are taken
/// when the body block's prologue runs — after the header's condition
/// evaluation, exactly where the old recursive emitter took them).
pub(crate) struct WhileRecord {
    pub(crate) pre_facts_mark: usize,
    pub(crate) scope_mark: usize,
    pub(crate) slot_marks: SlotMarks,
}

/// Per-`for range` state: the induction counter plus the shadowed
/// binding the loop variable replaced (restored at the exit block).
pub(crate) struct ForRecord {
    pub(crate) counter: Variable,
    pub(crate) var_name: StringId,
    pub(crate) saved_local: Option<Variable>,
    pub(crate) saved_fact: Option<ranges::IntRange>,
}

pub(crate) enum LoopRecord {
    While(WhileRecord),
    For(ForRecord),
}

/// The block-level prologue the Flow-block driver runs when it REACHES
/// a block (before block-start drops and steps): scope restores,
/// condition-fact seeding, loop-variable binding, and match merge
/// handling. Every variant names the construct record it reads.
#[derive(Clone, Copy, Debug)]
pub(crate) enum BlockPrologue {
    None,
    /// Then arm of an if-cascade: slot restore + true-polarity seed.
    IfThen {
        if_inst: TirRef,
    },
    /// Elif condition block: fact re-baseline (all negations so far).
    IfElifCond {
        if_inst: TirRef,
    },
    /// Elif arm body: slot restore + this elif's true-polarity seed.
    IfElifBody {
        if_inst: TirRef,
        idx: usize,
    },
    /// Else arm (or else-less fallthrough): slot restore + fact
    /// re-baseline with every negation.
    IfElse {
        if_inst: TirRef,
    },
    /// Cascade join: full fact join (restore, home invalidation,
    /// fall-through negation seeding, assigned kills) + slot restore.
    IfJoin {
        if_inst: TirRef,
    },
    /// Match arm: slot restore, arm-start payload drops, binding moves.
    MatchArm {
        match_inst: TirRef,
        arm: usize,
    },
    WhileBody {
        loop_inst: TirRef,
    },
    WhileExit {
        loop_inst: TirRef,
    },
    ForBody {
        loop_inst: TirRef,
    },
    ForExit {
        loop_inst: TirRef,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ValueRepr {
    Scalar(Value),
    Str {
        ptr: Value,
        len: Value,
        cap: Value,
    },
    /// Owned `bytes` triple: 24 bytes, freed via `ryo_bytes_free`
    /// (M8.4.2). Distinct variant from `Str` so type-keyed dispatch
    /// (frees, call ABI) cannot confuse the two families.
    Bytes {
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
    /// Struct value (M9): the address of the value's stack slot.
    /// Field reads/writes, copies, and drops all go through this
    /// pointer (see `codegen/structs.rs`).
    Struct {
        addr: Value,
    },
    /// Enum value (M11): the address of the value's stack slot,
    /// exactly like structs — the i32 discriminant and the active
    /// variant's payload share one slot (`pool.enum_view` layout), and
    /// tag-dispatched copies and drops go through this pointer (see
    /// `codegen/enums.rs`).
    Enum {
        addr: Value,
    },
}

impl ValueRepr {
    #[cfg(test)]
    fn expect_scalar(self) -> Value {
        match self {
            ValueRepr::Scalar(v) => v,
            ValueRepr::Str { .. } => panic!("expected Scalar, got Str"),
            ValueRepr::Bytes { .. } => panic!("expected Scalar, got Bytes"),
            ValueRepr::View { .. } => panic!("expected Scalar, got View"),
            ValueRepr::Struct { .. } => panic!("expected Scalar, got Struct"),
            ValueRepr::Enum { .. } => panic!("expected Scalar, got Enum"),
        }
    }
}

/// Fat-owner locals (str or bytes, M8.4.2): a binding is one XOR the
/// other; the type is tracked in the TIR, not here.
///
/// A binding initialized by a slot-out producer call gets a canonical
/// 24-byte stack-slot `home`: the producer writes it directly (no
/// temp slot + reload + regalloc-spill copy), and every read/write of
/// the binding goes through the home via `emit_fat_load` /
/// `emit_fat_store` instead of the SSA `Variable`s (which stay
/// declared-but-unused in that flavor). Branch merges flow through
/// memory, so no block-param phis are needed.
#[derive(Clone, Copy)]
struct FatLocals {
    binding: Option<TirRef>,
    ptr: Variable,
    len: Variable,
    cap: Variable,
    home: Option<StackSlot>,
    /// Provenance of the CURRENT home contents: true iff the stored
    /// triple is provably free-noop (a provably-inline producer result
    /// or a static cap==0 literal) with no in-place mutation since the
    /// store. Cleared on push/inout/promote and at control-flow joins
    /// (`invalidate_home_inline_flags`). Meaningless when `home` is
    /// `None`.
    home_inline: bool,
}

#[derive(Clone, Copy)]
struct ViewLocals {
    ptr: Variable,
    len: Variable,
}

impl<M: Module> Codegen<M> {
    fn from_module(module: M) -> Self {
        let int_type = module.target_config().pointer_type();
        Self {
            optional_optimizations: true,
            builder_context: FunctionBuilderContext::new(),
            ctx: module.make_context(),
            module,
            int_type,
            data_ctx: DataDescription::new(),
            string_data: HashMap::new(),
            guard_msg_data: HashMap::new(),
            runtime_fns: HashMap::new(),
            name_ids: CodegenNameIds::default(),
            tail_candidates: HashSet::new(),
            recursive_sccs: HashMap::new(),
        }
    }
}

/// Shared Cranelift flags for the AOT object pipeline.
fn aot_shared_flags(optional_optimizations: bool) -> Result<settings::Flags, String> {
    let mut shared_builder = settings::builder();
    shared_builder
        .enable("is_pic")
        .map_err(|e| format!("Error enabling is_pic: {}", e))?;
    shared_builder
        .set(
            "opt_level",
            if optional_optimizations {
                "speed"
            } else {
                "none"
            },
        )
        .map_err(|e| format!("Error setting opt_level: {}", e))?;
    shared_builder
        .set("preserve_frame_pointers", "true")
        .map_err(|e| format!("Error setting preserve_frame_pointers: {}", e))?;
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
        Self::new_aot_with_optimizations(target_triple, true)
    }

    pub fn new_aot_with_optimizations(
        target_triple: Triple,
        enabled: bool,
    ) -> Result<Self, String> {
        let shared_flags = aot_shared_flags(enabled)?;

        let isa = isa::lookup(target_triple.clone())
            .map_err(|e| format!("Unsupported target '{}': {}", target_triple, e))?
            .finish(shared_flags)
            .map_err(|e| format!("Failed to build ISA: {}", e))?;

        let obj_builder =
            ObjectBuilder::new(isa, "ryo_module", cranelift_module::default_libcall_names())
                .map_err(|e| format!("Failed to create ObjectBuilder: {}", e))?;

        let mut result = Self::from_module(ObjectModule::new(obj_builder));
        result.optional_optimizations = enabled;
        Ok(result)
    }

    pub fn finish(self) -> Result<Vec<u8>, String> {
        self.module
            .finish()
            .emit()
            .map_err(|e| format!("Failed to emit object file: {}", e))
    }
}

impl<M: Module> Codegen<M> {
    fn prepare_compilation(
        &mut self,
        tirs: &[Tir],
        pool: &InternPool,
    ) -> Result<DeclaredFunctions, String> {
        // Tail-call pre-pass: mark functions whose bodies contain a
        // syntactic tail-position self-call, so `lower_abi`
        // compiles them with CallConv::Tail (see
        // `tail::scan_tail_call_candidates` for the scope).
        self.tail_candidates = if self.optional_optimizations {
            tail::scan_tail_call_candidates(tirs, self.name_ids.main)
        } else {
            HashSet::new()
        };
        // Stack-check pre-pass: find the call-graph cycles whose edges get
        // the stack-limit check (see `recursion` for the rationale).
        self.recursive_sccs = recursion::scan_recursive_sccs(tirs);
        self.declare_all_functions(tirs, pool)
    }

    /// Read the `TirRef → ValueRepr` memo. Param sentinel refs
    /// (`TirRef::param`) are not valid arena indices, so they are
    /// served from the `param_values` table indexed by
    /// `TirRef::as_param_index()`.
    pub(crate) fn cached_repr(ctx: &FunctionContext<'_, M>, r: TirRef) -> Option<ValueRepr> {
        if let Some(idx) = r.as_param_index() {
            ctx.param_values[idx as usize]
        } else {
            ctx.inst_values.get(r.index()).copied().flatten()
        }
    }

    /// Write the `TirRef → ValueRepr` memo; see `cached_repr`.
    pub(crate) fn cache_repr(ctx: &mut FunctionContext<'_, M>, r: TirRef, repr: ValueRepr) {
        if let Some(idx) = r.as_param_index() {
            ctx.param_values[idx as usize] = Some(repr);
        } else {
            ctx.inst_values[r.index()] = Some(repr);
        }
    }

    /// Compile every function in `tirs` and return `main`'s `FuncId`
    /// together with the rendered CLIF text. The text is only
    /// produced when `dump_ir` is set (an `--emit=clif`-style
    /// request): the per-function pretty-print is pure overhead on
    /// the normal compile path, where the caller discards it.
    pub fn compile(
        &mut self,
        tirs: &[Tir],
        flows: &[ryo_core::flow::Flow],
        pool: &InternPool,
        sidecar: &ryo_core::ownership::OwnershipSidecar,
        dump_ir: bool,
    ) -> Result<(FuncId, String), String> {
        let (func_ids, ir_output) = self.compile_all(tirs, flows, pool, sidecar, dump_ir)?;

        // Resolve "main" through the id cache `compile_all` built.
        // `astgen` always interns the string "main" (it does so
        // explicitly when synthesising implicit-main and when checking
        // for an explicit-main collision), so the read-only `find_str`
        // probe is guaranteed to hit if the program declares one.
        let main_id = self
            .name_ids
            .main
            .ok_or_else(|| "No main function defined".to_string())?;
        let main = func_ids
            .get(&main_id)
            .map(|(id, _)| *id)
            .ok_or_else(|| "No main function defined".to_string())?;
        Ok((main, ir_output))
    }

    /// Compile every function and return the rendered CLIF text.
    /// Unlike `compile` this does not resolve `main` — the dump path
    /// serves `ryo ir --emit=clif`, which also runs on main-less
    /// files.
    pub fn compile_and_dump_ir(
        &mut self,
        tirs: &[Tir],
        flows: &[ryo_core::flow::Flow],
        pool: &InternPool,
        sidecar: &ryo_core::ownership::OwnershipSidecar,
    ) -> Result<String, String> {
        Ok(self.compile_all(tirs, flows, pool, sidecar, true)?.1)
    }

    /// Shared driver for `compile` / `compile_and_dump_ir`: declare
    /// all functions, then lower each body. `dump_ir` gates the
    /// per-function CLIF pretty-print so only the dump paths pay for
    /// it. Resolves [`CodegenNameIds`] once, up front, and stores it
    /// on `self` for the declaration and per-function lowering below.
    fn compile_all(
        &mut self,
        tirs: &[Tir],
        flows: &[ryo_core::flow::Flow],
        pool: &InternPool,
        sidecar: &ryo_core::ownership::OwnershipSidecar,
        dump_ir: bool,
    ) -> Result<(DeclaredFunctions, String), String> {
        let checked = input::require_backend_input(tirs, flows, sidecar, pool)?;
        if !self.optional_optimizations
            && sidecar
                .functions
                .iter()
                .any(|body| body.consumed_concat_lhs.iter().any(Option::is_some))
        {
            return Err(
                "unoptimized backend requires cleanup without allocation-reuse transfers".into(),
            );
        }
        self.name_ids = CodegenNameIds::resolve(pool);
        let func_ids = self.prepare_compilation(tirs, pool)?;
        for (tir, flow) in tirs.iter().zip(flows) {
            let calls = flow
                .steps
                .iter()
                .filter_map(|step| (tir.inst(step.inst).tag == TirTag::Call).then_some(step.inst))
                .chain(flow.terms.iter().filter_map(|term| match term {
                    ryo_core::flow::Terminator::Call { call, .. } => Some(*call),
                    _ => None,
                }));
            for inst in calls {
                let call = tir.call_view(inst);
                if let Some((_, abi)) = func_ids.get(&call.name) {
                    abi.require_call(tir, inst)?;
                }
            }
        }

        let mut ir_output = String::new();
        for i in 0..tirs.len() {
            let ready = checked.prepare_body(i)?;
            let text = self.compile_function(ready, &func_ids, pool, dump_ir)?;
            if dump_ir {
                ir_output.push_str(&text);
                ir_output.push('\n');
            }
        }

        Ok((func_ids, ir_output))
    }

    fn declare_all_functions(
        &mut self,
        tirs: &[Tir],
        pool: &InternPool,
    ) -> Result<DeclaredFunctions, String> {
        let mut func_ids = HashMap::new();
        for tir in tirs {
            let is_main = self.name_ids.main == Some(tir.name);
            let abi = self.lower_abi(tir, pool, is_main);
            debug_assert_eq!(&abi.target, self.module.isa().triple());
            debug_assert_eq!(abi.pointer_type, self.module.isa().pointer_type());
            debug_assert_eq!(abi.layout_context, pool.identity());
            let name_str = pool.str(tir.name);
            let linkage = if is_main {
                Linkage::Export
            } else {
                Linkage::Local
            };
            let func_id = self
                .module
                .declare_function(name_str, linkage, &abi.signature)
                .map_err(|e| format!("Failed to declare function '{}': {}", name_str, e))?;
            // Keep the signature: `compile_function` installs it into
            // `ctx.func` instead of rebuilding it.
            func_ids.insert(tir.name, (func_id, abi));
        }
        Ok(func_ids)
    }

    /// position-mechanical: build the drop fire map — every scheduled drop keyed by its
    /// Flow fire position (see the `FunctionContext` field docs) — and
    /// run the lowering-time provably-inline pre-filter over the drop
    /// list (frees that are guaranteed no-ops at runtime are marked
    /// consumed, not swept; see `pre_elide_drops`). Position
    /// interpretation is the shared resolver's job
    /// (`ryo_core::flow::fire_points` — the single authority both sides
    /// consume): this build only maps resolved points onto the emission
    /// buckets; no private re-anchoring lives here. Nothing reads cached
    /// values or emission progress.
    #[allow(clippy::too_many_arguments)]
    fn build_drop_fire_map(
        tir: &Tir,
        flow: &ryo_core::flow::Flow,
        pool: &InternPool,
        fat_mutated: &[bool],
        view_base_insts: &[bool],
        plan: &ryo_core::flow::cleanup::CleanupPlan,
        optional_optimizations: bool,
        func_sidecar: &ryo_core::ownership::FunctionSidecar,
    ) -> DropFireMap {
        let mut fire_before_step: Vec<Vec<usize>> = vec![Vec::new(); tir.instructions.len()];
        let mut fire_after_stmt: Vec<Vec<usize>> = vec![Vec::new(); tir.instructions.len()];
        let mut fire_by_payload: Vec<Vec<usize>> = vec![Vec::new(); tir.instructions.len()];
        let mut fire_at_term: HashMap<ryo_core::flow::BlockId, Vec<usize>> = HashMap::new();
        let mut fire_at_block_start: HashMap<ryo_core::flow::BlockId, Vec<usize>> = HashMap::new();
        // The shared firing-point resolver is the single authority on
        // position interpretation (ryo-core flow::fire_points): the
        // buckets below are keyed FROM its resolutions — no private
        // re-anchoring lives here anymore.
        let fire_points = ryo_core::flow::fire_points::FirePoints::new(flow);
        let positions = func_sidecar.drops.iter().map(|drop| drop.position).chain(
            func_sidecar
                .promotion_frees
                .iter()
                .map(|release| release.position),
        );
        for (idx, position) in positions.enumerate() {
            match position {
                ryo_core::flow::DropPosition::BlockStart { block } => {
                    // The unified cleanup schedule: arm-start payload
                    // destruction and conditional dead drops travel with
                    // everything else. Under the 1:1 Flow-block →
                    // Cranelift-block mapping each Flow block has
                    // exactly one start — the driver fires this bucket
                    // when it enters the block, before the block's
                    // steps.
                    debug_assert!(
                        matches!(
                            fire_points.resolve(position),
                            ryo_core::flow::fire_points::FireVerdict::Resolved(p)
                            if p.block == block && p.ordinal == 0
                        ),
                        "BlockStart resolves to the block's entry point"
                    );
                    fire_at_block_start.entry(block).or_default().push(idx);
                }
                ryo_core::flow::DropPosition::BeforeStep { inst } => {
                    debug_assert!(
                        matches!(
                            fire_points.resolve(position),
                            ryo_core::flow::fire_points::FireVerdict::Resolved(_)
                        ),
                        "BeforeStep anchors a real step"
                    );
                    debug_assert!(!inst.is_param(), "drop anchors are never param refs");
                    fire_before_step[inst.index()].push(idx);
                }
                ryo_core::flow::DropPosition::PreTerminator { block } => {
                    debug_assert!(
                        matches!(
                            fire_points.resolve(position),
                            ryo_core::flow::fire_points::FireVerdict::Resolved(_)
                        ),
                        "PreTerminator names a real block"
                    );
                    fire_at_term.entry(block).or_default().push(idx);
                }
                ryo_core::flow::DropPosition::AfterStep { inst } => {
                    debug_assert!(!inst.is_param(), "drop anchors are never param refs");
                    match fire_points.resolve_after_step(inst) {
                        ryo_core::flow::fire_points::AfterStepFire::StatementBoundary(p) => {
                            // The enclosing statement's boundary: the
                            // free lands after the statement's consumer
                            // — the only sound position for a last-use
                            // free (see the resolver's contract rule a).
                            let root = flow.block_steps(p.block)[p.ordinal as usize - 1].inst;
                            fire_after_stmt[root.index()].push(idx);
                        }
                        ryo_core::flow::fire_points::AfterStepFire::BlockTerminator(p) => {
                            // No statement follows the anchor (the
                            // anchor is in a match-arm body or a
                            // deferred return operand): fire inside the
                            // block's terminator handler.
                            fire_at_term.entry(p.block).or_default().push(idx);
                        }
                        ryo_core::flow::fire_points::AfterStepFire::Payload(verdict) => {
                            // Terminator payload (an if/loop/match/
                            // return/break/continue instruction): the
                            // construct's handler is the instruction's
                            // single emit position.
                            debug_assert!(
                                !matches!(
                                    verdict,
                                    ryo_core::flow::fire_points::FireVerdict::Unresolved
                                ),
                                "payload anchor {inst:?} does not resolve"
                            );
                            fire_by_payload[inst.index()].push(idx);
                        }
                        ryo_core::flow::fire_points::AfterStepFire::Unresolved => {
                            unreachable!(
                                "cleanup drop anchored on {inst:?}, which is neither a Flow step nor a terminator payload"
                            )
                        }
                    }
                }
            }
        }
        let mut freed_at = vec![false; func_sidecar.drops.len()];
        if optional_optimizations {
            Self::pre_elide_drops(
                tir,
                pool,
                fat_mutated,
                view_base_insts,
                flow,
                plan,
                func_sidecar,
                &mut freed_at,
            );
        }
        DropFireMap(
            fire_before_step,
            fire_after_stmt,
            fire_by_payload,
            fire_at_term,
            fire_at_block_start,
            freed_at,
        )
    }

    fn compile_function(
        &mut self,
        ready: input::BackendReadyBody<'_>,
        func_ids: &DeclaredFunctions,
        pool: &InternPool,
        dump_ir: bool,
    ) -> Result<String, String> {
        let (tir, flow, func_sidecar, cleanup_plan) = ready.into_parts();
        // Interned-name ids for this compilation (resolved by
        // `compile_all`); copied out so the block below can read them
        // while `builder` holds mutable borrows of `self`.
        let ids = self.name_ids;
        let (func_id, abi) = func_ids
            .get(&tir.name)
            .ok_or_else(|| format!("Function '{}' not declared", pool.str(tir.name)))?;
        let func_id = *func_id;

        let (free_binding_names, free_binding_param_names) =
            Self::build_free_binding_names(tir, flow, pool);
        let (fat_mutated, view_base_insts) = Self::build_fat_mutation_tables(tir, flow, pool);
        let DropFireMap(
            fire_before_step,
            fire_after_stmt,
            fire_by_payload,
            fire_at_term,
            fire_at_block_start,
            freed_at,
        ) = Self::build_drop_fire_map(
            tir,
            flow,
            pool,
            &fat_mutated,
            &view_base_insts,
            &cleanup_plan,
            self.optional_optimizations,
            func_sidecar,
        );

        let assignment_aliases = Self::assignment_aliases(tir, flow);

        // The signature built at declaration time; cloning two small
        // Vecs beats rebuilding the signature (pool queries + per-param
        // ABI branching) a second time.
        self.ctx.func.signature = abi.signature.clone();

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);

            let int_type = self.int_type;
            // The Cranelift pointer type, for the hosted-main argv
            // entry param and the `ryo_rt_init` call. Read before the
            // `ctx` borrow of `self.module` below.
            let pointer_type = self.module.target_config().pointer_type();
            let mut locals: Vec<Option<Variable>> = vec![None; pool.string_count()];
            let mut locals_undo: Vec<(u32, Option<Variable>)> = Vec::new();

            let is_main = abi.hosted_main;
            let has_sret = !is_main && abi.result.indirect_return();
            let mut block_idx: usize = if has_sret { 1 } else { 0 };
            let sret_ptr = if has_sret {
                Some(builder.block_params(entry_block)[0])
            } else {
                None
            };

            let mut fat_param_locals: Vec<Option<FatLocals>> = vec![None; pool.string_count()];
            let mut fat_locals_undo: Vec<(u32, Option<FatLocals>)> = Vec::new();
            let mut view_param_locals: Vec<Option<ViewLocals>> = vec![None; pool.string_count()];
            let mut view_locals_undo: Vec<(u32, Option<ViewLocals>)> = Vec::new();
            let mut struct_param_locals: Vec<Option<Variable>> = vec![None; pool.string_count()];
            let mut struct_locals_undo: Vec<(u32, Option<Variable>)> = Vec::new();
            let mut inout_ptrs: Vec<Option<(Value, TypeId)>> = vec![None; pool.string_count()];

            for (param_index, (param, lowered)) in tir.params.iter().zip(&abi.params).enumerate() {
                if lowered.mode == ParamMode::Inout {
                    // inout param: a single pointer to the caller's slot,
                    // regardless of pointee type. Load the current value
                    // into Variables so the body's existing read/mutate
                    // codegen is unchanged; remember the pointer for the
                    // write-back chokepoint before each `return_`.
                    let ptr = builder.block_params(entry_block)[block_idx];
                    block_idx += 1;
                    if lowered.value == ValueAbi::Fat {
                        // fat inout: load the fat-pointer triple into
                        // FatLocals so the body reads/mutates it like any
                        // fat local; write all three fields back before
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
                        Self::write_slot(
                            &mut fat_param_locals,
                            &mut fat_locals_undo,
                            param.name,
                            Some(FatLocals {
                                binding: Some(TirRef::param(param_index)),
                                ptr: var_ptr,
                                len: var_len,
                                cap: var_cap,
                                home: None,
                                home_inline: false,
                            }),
                        );
                    } else if matches!(lowered.value, ValueAbi::Struct | ValueAbi::Enum) {
                        // Struct (M9 named / M10 anon) and enum (M11)
                        // inout pointees: mutations happen in place
                        // through the caller's slot — bind the pointer
                        // directly and skip the write-back table (there
                        // is nothing to store back).
                        let var = builder.declare_var(int_type);
                        builder.def_var(var, ptr);
                        Self::write_slot(
                            &mut struct_param_locals,
                            &mut struct_locals_undo,
                            param.name,
                            Some(var),
                        );
                        continue;
                    } else {
                        let cl_ty = cranelift_type_for(param.ty, pool, int_type);
                        let cur = builder.ins().load(cl_ty, MemFlagsData::trusted(), ptr, 0);
                        let var = builder.declare_var(cl_ty);
                        builder.def_var(var, cur);
                        Self::write_slot(&mut locals, &mut locals_undo, param.name, Some(var));
                    }
                    inout_ptrs[param.name.raw() as usize] = Some((ptr, param.ty));
                    continue;
                }
                if lowered.value == ValueAbi::Fat {
                    let var_ptr = builder.declare_var(int_type);
                    let var_len = builder.declare_var(types::I64);
                    let var_cap = builder.declare_var(types::I64);
                    builder.def_var(var_ptr, builder.block_params(entry_block)[block_idx]);
                    builder.def_var(var_len, builder.block_params(entry_block)[block_idx + 1]);
                    builder.def_var(var_cap, builder.block_params(entry_block)[block_idx + 2]);
                    Self::write_slot(
                        &mut fat_param_locals,
                        &mut fat_locals_undo,
                        param.name,
                        Some(FatLocals {
                            binding: Some(TirRef::param(param_index)),
                            ptr: var_ptr,
                            len: var_len,
                            cap: var_cap,
                            home: None,
                            home_inline: false,
                        }),
                    );
                    block_idx += 3;
                } else if lowered.value == ValueAbi::View {
                    // `strview` view param: two ABI words (ptr, len). Views
                    // are borrows — no cap, never freed.
                    let var_ptr = builder.declare_var(int_type);
                    let var_len = builder.declare_var(types::I64);
                    builder.def_var(var_ptr, builder.block_params(entry_block)[block_idx]);
                    builder.def_var(var_len, builder.block_params(entry_block)[block_idx + 1]);
                    Self::write_slot(
                        &mut view_param_locals,
                        &mut view_locals_undo,
                        param.name,
                        Some(ViewLocals {
                            ptr: var_ptr,
                            len: var_len,
                        }),
                    );
                    block_idx += 2;
                } else if matches!(lowered.value, ValueAbi::Struct | ValueAbi::Enum) {
                    // Struct (M9 named / M10 anon) and enum (M11)
                    // params: a single pointer to the caller-side slot
                    // (borrow) or to a transferred field-wise copy
                    // (move/copy). Enum bindings share the
                    // family-agnostic `struct_locals` table.
                    let var = builder.declare_var(int_type);
                    builder.def_var(var, builder.block_params(entry_block)[block_idx]);
                    Self::write_slot(
                        &mut struct_param_locals,
                        &mut struct_locals_undo,
                        param.name,
                        Some(var),
                    );
                    block_idx += 1;
                } else {
                    let cl_ty = cranelift_type_for(param.ty, pool, int_type);
                    let var = builder.declare_var(cl_ty);
                    builder.def_var(var, builder.block_params(entry_block)[block_idx]);
                    Self::write_slot(&mut locals, &mut locals_undo, param.name, Some(var));
                    block_idx += 1;
                }
            }

            // One scratch slot per borrowed-param view base scheduled
            // for a promotion free: flag word (offset 0) + promoted
            // triple (8/16/24), per PROMO_SLOT_SIZE.
            let mut promo_slots: HashMap<TirRef, StackSlot> = HashMap::new();
            for pf in &func_sidecar.promotion_frees {
                promo_slots.entry(pf.base).or_insert_with(|| {
                    builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        PROMO_SLOT_SIZE,
                        3,
                    ))
                });
            }

            let mut ctx: FunctionContext<'_, M> = FunctionContext {
                optional_optimizations: self.optional_optimizations,
                module: &mut self.module,
                data_ctx: &mut self.data_ctx,
                string_data: &mut self.string_data,
                int_type,
                pool,
                tir,
                is_main,
                tail_conv_candidate: self.tail_candidates.contains(&tir.name),
                name_ids: ids,
                scalar_param_storage: tir
                    .params
                    .iter()
                    .map(|param| Self::read_slot(&locals, param.name))
                    .collect(),
                locals,
                locals_undo,
                range_facts: vec![None; pool.string_count()],
                range_facts_undo: Vec::new(),
                assigned_log: Vec::new(),
                func_ids,
                abi,
                inst_values: vec![None; tir.instructions.len()],
                param_values: vec![None; tir.params.len()],
                freed_at,
                #[cfg(debug_assertions)]
                fire_marked: vec![false; func_sidecar.drops.len()],
                freed_targets: std::collections::HashSet::new(),
                fire_before_step,
                fire_after_stmt,
                fire_by_payload,
                fire_at_term,
                fire_at_block_start,
                flow_blocks: Vec::new(),
                flow_sealed: Vec::new(),
                #[cfg(debug_assertions)]
                flow_visit_state: Vec::new(),
                flow_preds_emitted: Vec::new(),
                block_prologue: Vec::new(),
                match_join: Vec::new(),
                match_fallthrough: Vec::new(),
                bool_join: Vec::new(),
                tail_candidate: Vec::new(),
                if_arm_owner: Vec::new(),
                if_records: HashMap::new(),
                loop_records: HashMap::new(),
                match_records: HashMap::new(),
                has_sret,
                cleanup_plan,
                assignment_aliases,
                fat_binding_storage: tir
                    .params
                    .iter()
                    .enumerate()
                    .filter_map(|(index, param)| {
                        Self::read_slot(&fat_param_locals, param.name)
                            .map(|storage| (TirRef::param(index), storage))
                    })
                    .collect(),
                fat_locals: fat_param_locals,
                fat_locals_undo,
                promo_slots,
                promo_freed_at: vec![false; func_sidecar.promotion_frees.len()],
                view_locals: view_param_locals,
                view_locals_undo,
                aggregate_binding_storage: tir
                    .params
                    .iter()
                    .enumerate()
                    .filter_map(|(index, param)| {
                        Self::read_slot(&struct_param_locals, param.name)
                            .map(|storage| (TirRef::param(index), storage))
                    })
                    .collect(),
                struct_locals: struct_param_locals,
                struct_locals_undo,
                free_binding_names,
                free_binding_param_names,
                inout_ptrs,
                sret_ptr,
                sidecar: func_sidecar,
                flow,
                guard_msg_data: &mut self.guard_msg_data,
                runtime_fns: &mut self.runtime_fns,
                panic_blocks: Vec::new(),
                stack_cold_block: None,
                recursive_sccs: &self.recursive_sccs,
                stack_checked_block: None,
            };

            for (idx, param) in tir.params.iter().enumerate() {
                if is_fat_type(param.ty, pool) {
                    let locals = Self::read_slot(&ctx.fat_locals, param.name)
                        .expect("every fat param gets FatLocals in the param preamble above");
                    let ptr = builder.use_var(locals.ptr);
                    let len = builder.use_var(locals.len);
                    let cap = builder.use_var(locals.cap);
                    let repr = if matches!(pool.kind(param.ty), TypeKind::Bytes) {
                        ValueRepr::Bytes { ptr, len, cap }
                    } else {
                        ValueRepr::Str { ptr, len, cap }
                    };
                    ctx.param_values[idx] = Some(repr);
                } else if pool.is_view(param.ty) {
                    let locals = Self::read_slot(&ctx.view_locals, param.name)
                        .expect("every view param gets ViewLocals in the param preamble above");
                    let repr = ValueRepr::View {
                        ptr: builder.use_var(locals.ptr),
                        len: builder.use_var(locals.len),
                    };
                    ctx.param_values[idx] = Some(repr);
                } else if is_struct_type(param.ty, pool) {
                    let var = Self::read_slot(&ctx.struct_locals, param.name)
                        .expect("every struct param gets a struct_locals entry above");
                    ctx.param_values[idx] = Some(ValueRepr::Struct {
                        addr: builder.use_var(var),
                    });
                } else if is_enum_type(param.ty, pool) {
                    let var = Self::read_slot(&ctx.struct_locals, param.name)
                        .expect("every enum param gets a struct_locals entry above");
                    ctx.param_values[idx] = Some(ValueRepr::Enum {
                        addr: builder.use_var(var),
                    });
                }
            }

            // The promotion flag must start cleared: the
            // free-before-overwrite in emit_ensure_heap_for_view_base
            // and the scheduled promo frees both branch on it, and a
            // first-iteration garbage flag would free garbage. Iterate
            // promotion_frees (deduped), not the HashMap: sidecar order
            // is deterministic, HashMap iteration order is not — identical
            // input must yield identical emission.
            let mut zeroed: HashSet<TirRef> = HashSet::new();
            for pf in &func_sidecar.promotion_frees {
                if !zeroed.insert(pf.base) {
                    continue;
                }
                let slot = *ctx
                    .promo_slots
                    .get(&pf.base)
                    .expect("every promotion-free base has a scratch slot");
                let addr = builder.ins().stack_addr(ctx.int_type, slot, 0);
                let zero = builder.ins().iconst(types::I64, 0);
                builder.ins().store(MemFlagsData::trusted(), zero, addr, 0);
            }

            // Entry shim for hosted `main`: publish the C runtime's
            // (argc, argv) to the runtime before the first user
            // instruction. Entry-block params 0/1 are argc/argv — the
            // `is_main` signature branch appends them after (an empty)
            // Ryo param list, and sema rejects a parametrized main.
            // Hoisted literals and the body follow this call, so it
            // dominates every `process_argc`/`process_argv` read.
            if is_main {
                // Copy the params out: `block_params` borrows the
                // builder, which the `call` below needs mutably.
                let entry_params = builder.block_params(entry_block);
                let (argc, argv) = (entry_params[0], entry_params[1]);
                // Declared argc is `i64`, but `ryo_rt_init` takes
                // `c_int` — sound in this call direction: the callee
                // reads the low 32 bits of the (zero-extended)
                // register, and argc is small and non-negative. Don't
                // "fix" the runtime side to `i64`: reading the full
                // 64 bits would trust upper bits the C ABI leaves
                // unspecified.
                let rt_init = Self::declare_runtime_fn(
                    &mut ctx,
                    &mut builder,
                    "ryo_rt_init",
                    &[int_type, pointer_type],
                    &[],
                )?;
                builder.ins().call(rt_init, &[argc, argv]);
            }

            // Hoist string and bytes literals while the entry block is
            // still the current block: one from-literal call per distinct
            // literal per function, dominating every use.
            if ctx.optional_optimizations {
                Self::hoist_str_literals(&mut builder, &mut ctx)?;
            }

            // The backend drives emission from the canonical Flow
            // IR — one pre-created Cranelift block per Flow block, walked
            // in DFS order from the entry, with per-step evaluation through the
            // `eval_inst` family and terminator handlers dispatching the
            // block's explicit `Terminator`. No TIR statement list is
            // traversed to discover branches, scopes, or cleanup paths.
            Self::emit_flow_body(&mut builder, &mut ctx, entry_block)?;

            Self::assert_backend_invariants(&ctx);

            Self::emit_deferred_panic_blocks(&mut builder, &mut ctx)?;
            Self::emit_stack_cold_block(&mut builder, &mut ctx)?;

            builder.finalize(self.module.isa().frontend_config());
        }

        // The CLIF pretty-print is only worth its cost when the
        // caller asked for a dump (`--emit=clif`); the plain compile
        // path would discard it.
        let ir_text = if dump_ir {
            format!("{}", self.ctx.func)
        } else {
            String::new()
        };

        self.module
            .define_function(func_id, &mut self.ctx)
            .map_err(|e| {
                // Debug aid: surface the offending CLIF with the
                // verifier errors (define_function's Display drops them).
                format!(
                    "Failed to define function '{}': {}\n{}",
                    pool.str(tir.name),
                    e,
                    self.ctx.func
                )
            })?;

        self.ctx.clear();
        Ok(ir_text)
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
        // Iterate by index ascending: identical input must emit
        // identical instruction order (a HashMap's iteration order is
        // randomized per run).
        for (index, param) in ctx.tir.params.iter().enumerate() {
            let name = param.name;
            let Some((ptr, ty)) = ctx.inout_ptrs[name.raw() as usize] else {
                continue;
            };
            if is_fat_type(ty, ctx.pool) {
                // fat pointee: store all three fat-pointer fields.
                let sl = ctx
                    .fat_binding_storage
                    .get(&TirRef::param(index))
                    .copied()
                    .ok_or_else(|| {
                        format!("inout fat '{}' has no FatLocals", ctx.pool.str(name))
                    })?;
                let p = builder.use_var(sl.ptr);
                let l = builder.use_var(sl.len);
                let c = builder.use_var(sl.cap);
                builder.ins().store(MemFlagsData::trusted(), p, ptr, 0);
                builder.ins().store(MemFlagsData::trusted(), l, ptr, 8);
                builder.ins().store(MemFlagsData::trusted(), c, ptr, 16);
            } else {
                // Scalar pointee: a single store at offset 0.
                let var = ctx.scalar_param_storage[index].ok_or_else(|| {
                    format!(
                        "inout scalar '{}' has no local Variable",
                        ctx.pool.str(name)
                    )
                })?;
                let val = builder.use_var(var);
                builder.ins().store(MemFlagsData::trusted(), val, ptr, 0);
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
        if vals.len() != ctx.abi.signature.returns.len() {
            return Err("return values disagree with the lowered function ABI".into());
        }
        Self::emit_inout_writeback(builder, ctx)?;
        builder.ins().return_(vals);
        Ok(())
    }

    /// Emit one Flow IR STEP: a statement-level instruction, or the
    /// materialization of a value instruction whose consumers come
    /// later in the block (steps are in evaluation order, so operand
    /// steps populate the value caches before their consumers).
    /// Statement-level steps also fire the end-of-statement free
    /// bookkeeping at resolved firing points — the Flow-block driver's
    /// analogue of the old `emit_body` post-statement pass.
    ///
    /// Control instructions (if/while/for/match/return/break/continue)
    /// are NEVER steps — they are Flow IR block terminators, dispatched
    /// by `emit_flow_block` — so reaching one here is an invariant
    /// violation.
    fn emit_step(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<(), String> {
        let inst = ctx.tir.inst(r);
        match inst.tag {
            // Variable declaration — family dispatch lives in
            // decls.rs (keeps this dispatcher under the
            // small-function ratchet).
            TirTag::VarDecl => {
                Self::emit_var_decl(builder, ctx, r)?;
            }
            TirTag::ExprStmt => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ExprStmt must carry TirData::UnOp"),
                };
                Self::emit_expr_stmt_operand(builder, ctx, r, operand, false)?;
            }
            TirTag::Assign => {
                Self::emit_assign(builder, ctx, r)?;
            }
            TirTag::CompoundAssign => {
                let view = ctx.tir.compound_assign_view(r);
                let rhs = Self::eval_inst(builder, ctx, view.value)?;
                let var = Self::read_slot(&ctx.locals, view.name).ok_or_else(|| {
                    format!(
                        "Undefined variable in compound assign: '{}'",
                        ctx.pool.str(view.name)
                    )
                })?;
                let current = builder.use_var(var);

                let is_float = inst.ty == ctx.pool.float();
                let lhs_range = Self::read_slot(&ctx.range_facts, view.name);
                let rhs_range = ranges::int_range_of(ctx.tir, &ctx.range_facts, view.value);
                // Same spec §18 checked arithmetic as the binop arm.
                let result = Self::emit_compound_op(
                    builder, ctx, view.op, is_float, lhs_range, rhs_range, current, rhs,
                )?;

                Self::kill_fact(ctx, view.name);
                builder.def_var(var, result);
            }
            // M9 struct field assignment — lowered in codegen/structs.rs.
            TirTag::FieldAssign => {
                Self::emit_field_assign(builder, ctx, r)?;
            }
            TirTag::CompoundFieldAssign => {
                Self::emit_compound_field_assign(builder, ctx, r)?;
            }
            // M10 destructuring — lowered in codegen/structs.rs.
            TirTag::Destructure => {
                Self::emit_destructure(builder, ctx, r)?;
            }
            TirTag::Return
            | TirTag::ReturnVoid
            | TirTag::IfStmt
            | TirTag::WhileLoop
            | TirTag::ForRange
            | TirTag::Break
            | TirTag::Continue
            | TirTag::Match => {
                unreachable!(
                    "emit_step: control instruction at %{} is a Flow terminator, never a step",
                    r.index()
                );
            }
            _ => {
                // Value step: materialize it (memoized — a consumer
                // later in the block reads the cached repr). Fat/view/
                // struct/enum-typed values go through their family
                // entry points, exactly like the old operand-driven
                // evaluation did.
                let ty = inst.ty;
                if is_fat_type(ty, ctx.pool)
                    && structs::writes_out_slot_ids(ctx.tir, &ctx.name_ids, r)
                {
                    // Slot-out producers (concat, producer builtins,
                    // fat user calls) must NOT be materialized
                    // eagerly: the VarDecl/Assign naming the binding
                    // passes its home slot through
                    // `eval_inst_fat_slot`, and an eager temp-slot
                    // emission here would cache a repr that leaves
                    // the home unwritten. The consumer's evaluation
                    // materializes the producer instead — the same
                    // lazy order as the old operand-driven emitter.
                } else if is_fat_type(ty, ctx.pool) {
                    let _ = Self::eval_inst_fat(builder, ctx, r)?;
                } else if ctx.pool.is_view(ty) {
                    let _ = Self::eval_inst_view(builder, ctx, r)?;
                } else if matches!(ctx.pool.kind(ty), TypeKind::Struct | TypeKind::AnonStruct) {
                    let _ = Self::eval_inst_struct(builder, ctx, r)?;
                } else if is_enum_type(ty, ctx.pool) {
                    let _ = Self::eval_inst_enum(builder, ctx, r)?;
                } else {
                    let _ = Self::eval_inst(builder, ctx, r)?;
                }
            }
        }
        if is_statement_step(inst.tag) {
            // position-mechanical: fire this statement's drop bucket — the AfterStep
            // drops whose anchor step lives in this statement (the
            // anchor's step index maps here at fire-map build time).
            // Firing lands each free after the statement's consumer, the
            // only sound position for a last-use free, including promotion releases.
            Self::fire_after_statement(builder, ctx, r)?;
        }
        Ok(())
    }

    /// Evaluate an `ExprStmt` operand: the statement's value is
    /// discarded, but the call's side effects must be emitted.
    /// `in_tail_position` marks the last statement of a tail-allowed
    /// body (function fall-off or if-cascade arm): a call operand there
    /// may lower to `return_call`.
    ///
    /// The tail-attempt blocks (tail-candidate blocks) defer their
    /// operand subtree to this function so the eligibility cache state
    /// matches the old recursive emitter exactly.
    fn emit_expr_stmt_operand(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        stmt: TirRef,
        operand: TirRef,
        in_tail_position: bool,
    ) -> Result<bool, String> {
        // The last statement of a body/scope in tail position whose
        // operand is a call may be an eligible self-tail-call (e.g.
        // the eager_destruction benchmark's trailing `recursive(x - 1)`);
        // emit it as `return_call` and report the termination so the
        // driver skips the block's terminator. Ineligible calls fall
        // through to the plain evaluation below.
        if in_tail_position
            && matches!(ctx.tir.inst(operand).tag, TirTag::Call)
            && Self::try_emit_tail_call(builder, ctx, operand, None, stmt)?
        {
            return Ok(true);
        }
        // Fat-typed operands (bare formatter calls, str(view),
        // user str/bytes-returning calls) go through the fat entry
        // point, which caches the triple for the scheduled temp Free;
        // view-typed operands (bare slices) go through the view entry
        // point; the scalar path rejects both.
        let operand_ty = ctx.tir.inst(operand).ty;
        if is_fat_type(operand_ty, ctx.pool) {
            let _ = Self::eval_inst_fat(builder, ctx, operand)?;
        } else if ctx.pool.is_view(operand_ty) {
            let _ = Self::eval_inst_view(builder, ctx, operand)?;
        } else if matches!(
            ctx.pool.kind(operand_ty),
            TypeKind::Struct | TypeKind::AnonStruct
        ) {
            // Bare struct-valued statement (e.g. a discarded
            // struct-returning call): materialize so the call is
            // emitted; scheduled temp Frees handle the drop.
            let _ = Self::eval_inst_struct(builder, ctx, operand)?;
        } else if is_enum_type(operand_ty, ctx.pool) {
            // Bare enum-valued statement (e.g. a discarded
            // enum-returning call): materialize so the call is
            // emitted; scheduled temp Frees handle the drop.
            let _ = Self::eval_inst_enum(builder, ctx, operand)?;
        } else {
            let _ = Self::eval_inst(builder, ctx, operand)?;
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
