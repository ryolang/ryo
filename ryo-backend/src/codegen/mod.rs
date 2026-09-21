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
//! 2. The `eval_inst` memoization table (dense `Vec<Option<ValueRepr>>`
//!    indexed by `TirRef::index()`)
//!    so a shared sub-expression isn't re-emitted. TIR today is
//!    tree-shaped (one parent per inst) so this is purely
//!    defensive — but it's the right invariant before lazy sema
//!    / inline expansion lands. Zig calls the analogous mapping
//!    in `Air.zig` "liveness"; we don't need full liveness yet.

use cranelift::codegen::ir::{
    ArgumentPurpose, MemFlagsData, StackSlot, StackSlotData, StackSlotKind,
};
use cranelift::codegen::isa;
use cranelift::codegen::settings::{self, Configurable};
use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use ryo_core::tir::{ParamMode, Tir, TirData, TirRef, TirTag};
use ryo_core::types::{InternPool, StringId, TypeId, TypeKind};
use std::collections::{HashMap, HashSet};
use target_lexicon::Triple;

mod arith;
mod bytes;
mod control;
mod expr;
mod frees;
mod ranges;
mod str_ops;
mod structs;
mod views;

/// Fat-owner triple layout (str/bytes, 24 bytes): ptr at 0, len at 8,
/// cap at 16. Derived from `RyoStrFat`, not re-hardcoded.
const STR_SLOT_SIZE: u32 = 24;

/// Promotion scratch-slot layout for borrowed-param view bases: flag
/// word at 0 (1 = this callee promoted, 0 = pass-through), triple
/// ptr/len/cap at 8/16/24.
pub(crate) const PROMO_SLOT_SIZE: u32 = 32;

/// How a statement or body ended the current block, if it did.
/// Replaces the `bool` that conflated Break/Continue with Return:
/// callers distinguish "block ended" (`!= None`) from "the function
/// definitely returns" (`== Return`) explicitly.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Terminator {
    None,
    Return,
    Break,
    Continue,
}

/// Returns `true` if `ty` is a 24-byte fat owner (`str` or `bytes`,
/// M8.4.2) in the pool.
///
/// Callers use this to gate multi-value (fat-pointer) paths before
/// reaching `cranelift_type_for`, where a fat type is a caller bug.
pub(crate) fn is_fat_type(ty: TypeId, pool: &InternPool) -> bool {
    matches!(pool.kind(ty), TypeKind::Str | TypeKind::Bytes)
}

/// Builtins whose fat result codegen materializes inline — no runtime
/// call, no slot-out write. `writes_out_slot` must exclude exactly
/// these: the inlined arm in `eval_inst_fat_slot` ignores its
/// `out_slot` parameter, so handing one a home slot would leave the
/// slot unwritten and every later home read would load garbage.
/// Keeping the set wrong in the other direction (listing a builtin
/// that is NOT inlined) is harmless — it only forgoes the home.
pub(crate) const CODEGEN_INLINED_BUILTINS: &[&str] = &["bool_to_str"];

/// True when instruction `r` produces its fat result through a
/// slot-out call (`emit_slot_out_call` or user-call sret) and can
/// therefore write a caller-provided home slot directly. Concat and
/// every fat-returning call qualify — except codegen-inlined builtins
/// (`CODEGEN_INLINED_BUILTINS`), which never touch a slot.
pub(crate) fn writes_out_slot(tir: &Tir, pool: &InternPool, r: TirRef) -> bool {
    match tir.inst(r).tag {
        TirTag::StrConcat | TirTag::BytesConcat => true,
        TirTag::Call => !CODEGEN_INLINED_BUILTINS.contains(&pool.str(tir.call_view(r).name)),
        _ => false,
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
        TypeKind::Tuple => {
            // No surface syntax constructs tuples today, so a tuple
            // TypeId cannot reach codegen; the variant exists only to
            // validate the InternPool's sidecar encoding.
            unreachable!("cranelift_type_for: tuple TypeId reached codegen")
        }
        // Struct codegen (aggregate layout) lands in a later M9 task.
        TypeKind::Struct => unreachable!("cranelift_type_for: struct TypeId reached codegen"),
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
}

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
    /// Scalar binding name → Cranelift `Variable`. Dense table indexed
    /// by `StringId::raw()`, sized once per function from
    /// `pool.string_count()` (codegen never interns, so every name in
    /// the TIR is in bounds). Binding names are not unique per
    /// function — a nested VarDecl shadows the outer slot — so every
    /// write goes through `write_slot`, which records the previous
    /// slot value on `locals_undo`; `emit_scoped_body` replays the log
    /// in reverse to restore the pre-scope state.
    locals: Vec<Option<Variable>>,
    locals_undo: Vec<(u32, Option<Variable>)>,
    /// Value-range facts for spec §18 guard elision: binding name →
    /// inclusive `[lo, hi]` bounds proven by dominating comparisons
    /// (see `codegen/ranges.rs` for the seeding rules). Dense
    /// `StringId::raw()`-indexed table with an undo log, scoped like
    /// `locals`: `emit_scoped_body` and the if/while join handling
    /// restore it via `restore_slots`, and any binding assigned inside
    /// a restored scope is killed at the join via `assigned_log`.
    range_facts: Vec<Option<ranges::IntRange>>,
    range_facts_undo: Vec<(u32, Option<ranges::IntRange>)>,
    /// Flat log of every binding assigned (or passed as `inout`) since
    /// function entry. Branch/loop emitters snapshot the length before
    /// a scoped body and kill those names from `range_facts` after the
    /// restore, so a join whose predecessors disagree invalidates the
    /// fact instead of resurrecting the pre-scope one. Distinct from
    /// the slot-table undo logs: those restore pre-scope slot values,
    /// while this drives the resurrect-then-re-kill join invalidation.
    assigned_log: Vec<StringId>,
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
    /// Memo entries for param sentinel refs (`TirRef::param`), which
    /// cannot index `inst_values`. Dense table indexed by
    /// `TirRef::as_param_index()`, sized once per function from
    /// `tir.params.len()`. Only fat (str/bytes) and view params ever
    /// land here.
    param_values: Vec<Option<ValueRepr>>,
    /// Dense flag per `sidecar.free_schedule` index: true once that
    /// entry's Free has been emitted in codegen. A given anchor TirRef
    /// can be reached through both `eval_inst` and `eval_inst_fat`
    /// (e.g. a `Var` materialized once as scalar and once as
    /// fat-pointer), and the end-of-stmt sweep can also see anchors
    /// that an earlier per-eval hook already fired. Without this guard
    /// each path would emit the Free, double-freeing the allocation.
    freed_at: Vec<bool>,
    /// Maps an anchor `TirRef` (`after`) to the indices of `sidecar.free_schedule`
    /// that are anchored on it. Dense table indexed by `TirRef::index()`
    /// (slot 0 unused — refs are 1-based); an empty Vec means no frees
    /// anchor there. Anchors are never param sentinel refs.
    free_by_after: Vec<Vec<usize>>,
    /// Unfired indices in `sidecar.free_schedule` that still need to be swept.
    /// Used to avoid O(K * S) quadratic scaling during end-of-statement sweep.
    pending_sweep: Vec<usize>,
    loop_stack: Vec<LoopContext>,
    /// Owned fat (str/bytes) bindings: three SSA `Variable`s (ptr, len,
    /// cap) per binding name. Dense `StringId::raw()`-indexed table with
    /// an undo log, same scoping discipline as `locals`.
    fat_locals: Vec<Option<FatLocals>>,
    fat_locals_undo: Vec<(u32, Option<FatLocals>)>,
    /// Per-name mutation flags from `build_fat_mutation_tables`: a
    /// provably-inline-initialized binding's free is elided only when
    /// its name is NOT flagged here.
    fat_mutated: Vec<bool>,
    /// Per-inst view-base flags from `build_fat_mutation_tables`: a
    /// provably-inline temp's free is elided only when its producing
    /// inst is NOT a slice/`ToView` base (promotion would turn it into
    /// a real heap buffer).
    view_base_insts: Vec<bool>,
    /// Borrowed-param view bases (Slice/ToView) that were promoted
    /// through a scratch slot instead of the param's FatLocals: base
    /// inst → slot holding flag (offset 0) + promoted triple (8/16/24).
    promo_slots: HashMap<TirRef, StackSlot>,
    /// Dense flag per `sidecar.promotion_frees` index (emission-time
    /// dedup, same discipline as `freed_at`).
    promo_freed_at: Vec<bool>,
    /// Anchor `TirRef` → indices into `sidecar.promotion_frees`.
    promo_free_by_after: Vec<Vec<usize>>,
    /// Unfired `promotion_frees` indices for the end-of-statement sweep.
    pending_promo_sweep: Vec<usize>,
    /// `strview` view bindings (M8.4): two SSA `Variable`s per binding,
    /// mirroring `fat_locals`. Views are non-owning — they never
    /// appear in the free schedule.
    view_locals: Vec<Option<ViewLocals>>,
    view_locals_undo: Vec<(u32, Option<ViewLocals>)>,
    /// Struct-typed bindings (M9): one address-holding `Variable` per
    /// binding — the value itself lives in a stack slot (see
    /// `codegen/structs.rs`). Dense `StringId::raw()`-indexed table
    /// with an undo log, same scoping discipline as `locals`.
    struct_locals: Vec<Option<Variable>>,
    struct_locals_undo: Vec<(u32, Option<Variable>)>,
    /// Free-target (initializer / Assign value / fat-param virtual ref)
    /// → binding-name map, built once per function by
    /// `build_free_binding_names`. `emit_frees` uses it to release a
    /// named binding's CURRENT `FatLocals` rather than the producing
    /// init's possibly-stale cached repr.
    ///
    /// Split into two dense tables dispatched on `TirRef::is_param()`
    /// (same shape as `cached_repr`): `free_binding_names` is indexed
    /// by `TirRef::index()` for real instruction refs (slot 0 unused),
    /// `free_binding_param_names` by `TirRef::as_param_index()` for
    /// fat-param sentinel refs.
    free_binding_names: Vec<Option<StringId>>,
    free_binding_param_names: Vec<Option<StringId>>,
    /// M8.3 inout parameters: for each inout param's name, the
    /// caller-provided slot address (a function-entry block param)
    /// and its pointee `TypeId`. The write-back chokepoint stores each
    /// param's current `Variable` back through this pointer before
    /// every `return_`. Scalars store one field at offset 0; fat
    /// (str/bytes) pointees store three fields.
    ///
    /// Dense table indexed by `StringId::raw()`, sized once per
    /// function from `pool.string_count()` (codegen never interns, so
    /// every name in the TIR is in bounds). Never scoped — params are
    /// bound at function entry — so it has no undo log.
    inout_ptrs: Vec<Option<(Value, TypeId)>>,
    /// For fat-returning functions: the hidden sret pointer (first block param)
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
    /// Active arm stack for conditional destruction. Each
    /// entry is the `BranchId` of an enclosing if/elif/else arm
    /// currently being lowered. `branch_active` walks this stack to
    /// gate branch-tagged `FreePoint`s — `contains` (not `last()`)
    /// so a Free anchored to a parent arm still fires from inside a
    /// nested child arm of the same parent.
    branch_stack: Vec<ryo_core::ownership::BranchId>,
    /// Module-level cache for compiler-generated guard messages;
    /// see `Codegen::guard_msg_data`.
    guard_msg_data: &'a mut HashMap<&'static str, DataId>,
    /// Module-level import cache for runtime functions; see
    /// `Codegen::runtime_fns`.
    runtime_fns: &'a mut HashMap<&'static str, FuncId>,
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
            runtime_fns: HashMap::new(),
        }
    }
}

/// Shared Cranelift flags for the AOT object pipeline.
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

/// Every runtime symbol the JIT must resolve, with its address. This
/// table is the single source of truth for JIT registration — the
/// names must stay in sync with the string literals codegen passes to
/// `declare_runtime_fn` (the module-level import cache is keyed on the
/// same names). Functions whose bodies codegen now inlines (literal
/// packing, slicing) are deliberately absent.
fn runtime_symbols() -> [(&'static str, *const u8); 21] {
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
        ("ryo_panic", ryo_runtime::ryo_panic as *const u8),
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

    /// Read the free-target → binding-name map. Param sentinel refs
    /// are served from `free_binding_param_names`; real instruction
    /// refs from `free_binding_names`. Same dispatch shape as
    /// `cached_repr`.
    pub(crate) fn free_binding_name(ctx: &FunctionContext<'_, M>, r: TirRef) -> Option<StringId> {
        if let Some(idx) = r.as_param_index() {
            ctx.free_binding_param_names[idx as usize]
        } else {
            ctx.free_binding_names.get(r.index()).copied().flatten()
        }
    }

    /// True if any instruction reachable from `root` (transitive
    /// operands and nested body statements) is a `Var` read of `name`.
    /// Conservative aliasing probe for the producer-into-home Assign
    /// path: an RHS that mentions the target binding (including a
    /// shadowed same-name read — the probe cannot distinguish shadowing)
    /// forbids the free-before-overwrite order.
    fn expr_refs_name(tir: &Tir, root: TirRef, name: StringId) -> bool {
        let mut stack = vec![root];
        while let Some(r) = stack.pop() {
            if let TirData::Var(n) = tir.inst(r).data
                && n == name
            {
                return true;
            }
            tir.walk_operands(r, &mut |_parent, child, _kind| stack.push(child));
        }
        false
    }

    /// Provenance of a freshly stored home value: true iff the value
    /// is provably free-noop — a provably-inline producer result or a
    /// static (cap == 0) literal — so a later free on the home may be
    /// elided.
    fn home_value_provably_inline(
        ctx: &FunctionContext<'_, M>,
        func: &cranelift::codegen::ir::Function,
        value: TirRef,
        cap: Value,
    ) -> bool {
        Self::provably_inline_producer(ctx, value) || Self::is_static_cap_zero(func, cap)
    }

    /// Set the home-provenance flag on a fat binding. No-op for
    /// home-less bindings (the flag is meaningless without a home).
    pub(crate) fn set_home_inline(ctx: &mut FunctionContext<'_, M>, name: StringId, inline: bool) {
        if let Some(mut fl) = Self::read_slot(&ctx.fat_locals, name)
            && fl.home.is_some()
            && fl.home_inline != inline
        {
            fl.home_inline = inline;
            Self::write_slot(
                &mut ctx.fat_locals,
                &mut ctx.fat_locals_undo,
                name,
                Some(fl),
            );
        }
    }

    /// Clear every home-provenance flag. Called at control-flow joins
    /// (if merges, loop headers and exits): the home slot is memory,
    /// so stores inside an arm or iteration persist while the scoped
    /// table restore reverts the flag — a join must not trust
    /// pre-branch provenance.
    pub(crate) fn invalidate_home_inline_flags(ctx: &mut FunctionContext<'_, M>) {
        let flagged: Vec<StringId> = (0..ctx.fat_locals.len())
            .filter(|&i| ctx.fat_locals[i].is_some_and(|fl| fl.home.is_some() && fl.home_inline))
            .map(|i| StringId::from_raw(u32::try_from(i).expect("StringId index out of range")))
            .collect();
        for name in flagged {
            Self::set_home_inline(ctx, name, false);
        }
    }

    pub fn compile(
        &mut self,
        tirs: &[Tir],
        pool: &InternPool,
        sidecar: &ryo_core::ownership::OwnershipSidecar,
    ) -> Result<FuncId, String> {
        debug_assert!(
            bytes::no_unreachable_in(tirs),
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
            bytes::no_unreachable_in(tirs),
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
                // slot, regardless of pointee type (scalar or fat owner).
                sig.params.push(AbiParam::new(self.int_type));
            } else if is_fat_type(param.ty, pool) {
                // Fat owner (str/bytes): 3-word ABI.
                sig.params.push(AbiParam::new(self.int_type)); // ptr
                sig.params.push(AbiParam::new(types::I64)); // len
                sig.params.push(AbiParam::new(types::I64)); // cap
            } else if pool.is_view(param.ty) {
                // `strview` view: 2-word ABI (ptr, len) — no cap word (M8.4).
                sig.params.push(AbiParam::new(self.int_type)); // ptr
                sig.params.push(AbiParam::new(types::I64)); // len
            } else if matches!(pool.kind(param.ty), TypeKind::Struct) {
                // Struct (M9): a single pointer to the value's stack
                // slot, regardless of mode (borrow/move/copy).
                sig.params.push(AbiParam::new(self.int_type));
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
            if is_fat_type(tir.return_type, pool)
                || matches!(pool.kind(tir.return_type), TypeKind::Struct)
            {
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
            let mut locals: Vec<Option<Variable>> = vec![None; pool.string_count()];
            let mut locals_undo: Vec<(u32, Option<Variable>)> = Vec::new();

            let is_main = pool.str(tir.name) == "main";
            let returns_fat = !is_main && is_fat_type(tir.return_type, pool);
            let returns_struct = !is_main && matches!(pool.kind(tir.return_type), TypeKind::Struct);
            let has_sret = returns_fat || returns_struct;
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

            for param in tir.params.iter() {
                if param.mode == ParamMode::Inout {
                    // inout param: a single pointer to the caller's slot,
                    // regardless of pointee type. Load the current value
                    // into Variables so the body's existing read/mutate
                    // codegen is unchanged; remember the pointer for the
                    // write-back chokepoint before each `return_`.
                    let ptr = builder.block_params(entry_block)[block_idx];
                    block_idx += 1;
                    if is_fat_type(param.ty, pool) {
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
                                ptr: var_ptr,
                                len: var_len,
                                cap: var_cap,
                                home: None,
                                home_inline: false,
                            }),
                        );
                    } else if matches!(pool.kind(param.ty), TypeKind::Struct) {
                        // Struct inout pointee (M9): mutations happen in
                        // place through the caller's slot — bind the
                        // pointer directly and skip the write-back
                        // table (there is nothing to store back).
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
                if is_fat_type(param.ty, pool) {
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
                            ptr: var_ptr,
                            len: var_len,
                            cap: var_cap,
                            home: None,
                            home_inline: false,
                        }),
                    );
                    block_idx += 3;
                } else if pool.is_view(param.ty) {
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
                } else if matches!(pool.kind(param.ty), TypeKind::Struct) {
                    // Struct param (M9): a single pointer to the
                    // caller-side slot (borrow) or to a transferred
                    // field-wise copy (move/copy).
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

            let mut free_by_after: Vec<Vec<usize>> = vec![Vec::new(); tir.instructions.len()];
            for (idx, fp) in func_sidecar.free_schedule.iter().enumerate() {
                debug_assert!(
                    !fp.after.is_param(),
                    "free anchors are never param sentinel refs"
                );
                free_by_after[fp.after.index()].push(idx);
            }
            let pending_sweep: Vec<usize> = (0..func_sidecar.free_schedule.len()).collect();
            let (free_binding_names, free_binding_param_names) =
                Self::build_free_binding_names(tir, pool);
            let (fat_mutated, view_base_insts) = Self::build_fat_mutation_tables(tir, pool);

            let mut promo_free_by_after: Vec<Vec<usize>> = vec![Vec::new(); tir.instructions.len()];
            for (idx, pf) in func_sidecar.promotion_frees.iter().enumerate() {
                debug_assert!(
                    !pf.after.is_param(),
                    "promotion-free anchors are never param sentinel refs"
                );
                promo_free_by_after[pf.after.index()].push(idx);
            }
            let pending_promo_sweep: Vec<usize> = (0..func_sidecar.promotion_frees.len()).collect();

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
                module: &mut self.module,
                data_ctx: &mut self.data_ctx,
                string_data: &mut self.string_data,
                int_type,
                pool,
                tir,
                locals,
                locals_undo,
                range_facts: vec![None; pool.string_count()],
                range_facts_undo: Vec::new(),
                assigned_log: Vec::new(),
                func_ids,
                inst_values: vec![None; tir.instructions.len()],
                param_values: vec![None; tir.params.len()],
                freed_at: vec![false; func_sidecar.free_schedule.len()],
                free_by_after,
                pending_sweep,
                loop_stack: Vec::new(),
                fat_locals: fat_param_locals,
                fat_locals_undo,
                fat_mutated,
                view_base_insts,
                promo_slots,
                promo_freed_at: vec![false; func_sidecar.promotion_frees.len()],
                promo_free_by_after,
                pending_promo_sweep,
                view_locals: view_param_locals,
                view_locals_undo,
                struct_locals: struct_param_locals,
                struct_locals_undo,
                free_binding_names,
                free_binding_param_names,
                inout_ptrs,
                sret_ptr,
                sidecar: func_sidecar,
                branch_stack: Vec::new(),
                guard_msg_data: &mut self.guard_msg_data,
                runtime_fns: &mut self.runtime_fns,
                panic_blocks: Vec::new(),
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
                } else if matches!(pool.kind(param.ty), TypeKind::Struct) {
                    let var = Self::read_slot(&ctx.struct_locals, param.name)
                        .expect("every struct param gets a struct_locals entry above");
                    ctx.param_values[idx] = Some(ValueRepr::Struct {
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

            // Hoist string and bytes literals while the entry block is
            // still the current block: one from-literal call per distinct
            // literal per function, dominating every use.
            Self::hoist_str_literals(&mut builder, &mut ctx)?;

            let body_term = Self::emit_body(&mut builder, &mut ctx, &tir.body_stmts())?;

            if body_term == Terminator::None {
                if is_main {
                    let zero = builder.ins().iconst(int_type, 0);
                    Self::emit_return(&mut builder, &mut ctx, &[zero])?;
                } else if has_sret || tir.return_type == pool.void() {
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
                        .enumerate()
                        .any(|(fired, &b)| b && ctx.sidecar.free_schedule[fired].target == target)
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
                Self::emit_due_promo_frees(builder, ctx, stmt_ref)?;
                Self::sweep_due_frees(builder, ctx)?;
                Self::sweep_due_promo_frees(builder, ctx)?;
            }
        }
        Ok(terminator)
    }

    /// Emit `stmts` with the slot tables scoped: every write the body
    /// makes to `locals` / `fat_locals` / `view_locals` /
    /// `struct_locals` (and `range_facts`) is rolled back on exit by
    /// replaying each table's undo log down to the mark taken here.
    /// On `?` error nothing is
    /// restored — the compile is abandoned anyway.
    fn emit_scoped_body(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        stmts: &[TirRef],
    ) -> Result<Terminator, String> {
        let locals_mark = ctx.locals_undo.len();
        let fat_locals_mark = ctx.fat_locals_undo.len();
        let view_locals_mark = ctx.view_locals_undo.len();
        let struct_locals_mark = ctx.struct_locals_undo.len();
        let range_facts_mark = ctx.range_facts_undo.len();
        let terminator = Self::emit_body(builder, ctx, stmts)?;
        Self::restore_slots(&mut ctx.locals, &mut ctx.locals_undo, locals_mark);
        Self::restore_slots(
            &mut ctx.fat_locals,
            &mut ctx.fat_locals_undo,
            fat_locals_mark,
        );
        Self::restore_slots(
            &mut ctx.view_locals,
            &mut ctx.view_locals_undo,
            view_locals_mark,
        );
        Self::restore_slots(
            &mut ctx.struct_locals,
            &mut ctx.struct_locals_undo,
            struct_locals_mark,
        );
        Self::restore_slots(
            &mut ctx.range_facts,
            &mut ctx.range_facts_undo,
            range_facts_mark,
        );
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
        // Iterate by index ascending: identical input must emit
        // identical instruction order (a HashMap's iteration order is
        // randomized per run).
        for idx in 0..ctx.inout_ptrs.len() {
            let Some((ptr, ty)) = ctx.inout_ptrs[idx] else {
                continue;
            };
            let name = StringId::from_raw(u32::try_from(idx).expect("StringId index out of range"));
            if is_fat_type(ty, ctx.pool) {
                // fat pointee: store all three fat-pointer fields.
                let sl = Self::read_slot(&ctx.fat_locals, name).ok_or_else(|| {
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
                let var = Self::read_slot(&ctx.locals, name).ok_or_else(|| {
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
                if is_fat_type(inst.ty, ctx.pool) {
                    // Producer-into-home: a slot-out producer
                    // initializer (runtime producer call, concat, or
                    // fat-returning user call) writes the binding's
                    // canonical 24-byte home slot directly — no temp
                    // slot, no reload-to-SSA, no second spill slot.
                    // All later reads/writes go through the home.
                    let home = if writes_out_slot(ctx.tir, ctx.pool, view.initializer) {
                        Some(builder.create_sized_stack_slot(StackSlotData::new(
                            StackSlotKind::ExplicitSlot,
                            STR_SLOT_SIZE,
                            3,
                        )))
                    } else {
                        None
                    };
                    let repr = Self::eval_inst_fat_slot(builder, ctx, view.initializer, home)?;
                    match repr {
                        ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                            let var_ptr = builder.declare_var(ctx.int_type);
                            let var_len = builder.declare_var(types::I64);
                            let var_cap = builder.declare_var(types::I64);
                            // Home-backed bindings keep the triple in the
                            // slot only; the SSA Variables stay unused.
                            if home.is_none() {
                                builder.def_var(var_ptr, ptr);
                                builder.def_var(var_len, len);
                                builder.def_var(var_cap, cap);
                            }
                            let home_inline = home.is_some()
                                && Self::home_value_provably_inline(
                                    ctx,
                                    builder.func,
                                    view.initializer,
                                    cap,
                                );
                            Self::write_slot(
                                &mut ctx.fat_locals,
                                &mut ctx.fat_locals_undo,
                                view.name,
                                Some(FatLocals {
                                    ptr: var_ptr,
                                    len: var_len,
                                    cap: var_cap,
                                    home,
                                    home_inline,
                                }),
                            );
                        }
                        _ => unreachable!("fat-typed initializer should produce a fat ValueRepr"),
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
                            Self::write_slot(
                                &mut ctx.view_locals,
                                &mut ctx.view_locals_undo,
                                view.name,
                                Some(ViewLocals {
                                    ptr: var_ptr,
                                    len: var_len,
                                }),
                            );
                        }
                        _ => unreachable!("view-typed initializer should produce ValueRepr::View"),
                    }
                    // Same defensive fact removal as the scalar path
                    // below: a same-scope redefinition must not inherit
                    // a stale fact from the shadowed binding.
                    Self::write_slot(
                        &mut ctx.range_facts,
                        &mut ctx.range_facts_undo,
                        view.name,
                        None,
                    );
                    return Ok(Terminator::None);
                }
                if matches!(ctx.pool.kind(inst.ty), TypeKind::Struct) {
                    return Self::emit_struct_var_decl(builder, ctx, r);
                }
                let val = Self::eval_inst(builder, ctx, view.initializer)?;
                // The variable's resolved type lives in the VarDecl
                // inst's `ty` slot directly — no side-table lookup.
                let cl_ty = cranelift_type_for(inst.ty, ctx.pool, ctx.int_type);
                let var = builder.declare_var(cl_ty);
                builder.def_var(var, val);
                // Defensive: a same-scope redefinition must not inherit
                // a stale fact from the shadowed binding. (No seeding
                // from constant initializers — explicit non-goal.)
                Self::write_slot(
                    &mut ctx.range_facts,
                    &mut ctx.range_facts_undo,
                    view.name,
                    None,
                );
                Self::write_slot(&mut ctx.locals, &mut ctx.locals_undo, view.name, Some(var));
                Ok(Terminator::None)
            }
            TirTag::Return => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("Return must carry TirData::UnOp"),
                };
                if is_fat_type(ctx.tir.return_type, ctx.pool) {
                    let sret = ctx.sret_ptr.expect("fat-returning fn must have sret_ptr");
                    let repr = Self::eval_inst_fat(builder, ctx, operand)?;
                    let (ptr, len, cap) = match repr {
                        ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                            (ptr, len, cap)
                        }
                        _ => unreachable!("fat return must produce a fat ValueRepr"),
                    };
                    builder.ins().store(MemFlagsData::trusted(), ptr, sret, 0);
                    builder.ins().store(MemFlagsData::trusted(), len, sret, 8);
                    builder.ins().store(MemFlagsData::trusted(), cap, sret, 16);
                    Self::emit_due_frees(builder, ctx, r)?;
                    Self::emit_due_promo_frees(builder, ctx, r)?;
                    Self::emit_return(builder, ctx, &[])?;
                } else if matches!(ctx.pool.kind(ctx.tir.return_type), TypeKind::Struct) {
                    return Self::emit_struct_return(builder, ctx, r, operand);
                } else {
                    let val = Self::eval_inst(builder, ctx, operand)?;
                    Self::emit_due_frees(builder, ctx, r)?;
                    Self::emit_due_promo_frees(builder, ctx, r)?;
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
                    Self::emit_due_promo_frees(builder, ctx, r)?;
                    Self::emit_return(builder, ctx, &[zero])?;
                } else {
                    Self::emit_due_frees(builder, ctx, r)?;
                    Self::emit_due_promo_frees(builder, ctx, r)?;
                    Self::emit_return(builder, ctx, &[])?;
                }
                Ok(Terminator::Return)
            }
            TirTag::ExprStmt => {
                let operand = match inst.data {
                    TirData::UnOp(o) => o,
                    _ => unreachable!("ExprStmt must carry TirData::UnOp"),
                };
                // Fat-typed operands (bare formatter calls, str(view),
                // user str/bytes-returning calls) go through the fat entry
                // point, which caches the triple for the scheduled temp
                // Free; view-typed operands (bare slices) go through the
                // view entry point; the scalar path rejects both.
                let operand_ty = ctx.tir.inst(operand).ty;
                if is_fat_type(operand_ty, ctx.pool) {
                    let _ = Self::eval_inst_fat(builder, ctx, operand)?;
                } else if ctx.pool.is_view(operand_ty) {
                    let _ = Self::eval_inst_view(builder, ctx, operand)?;
                } else if matches!(ctx.pool.kind(operand_ty), TypeKind::Struct) {
                    // Bare struct-valued statement (e.g. a discarded
                    // struct-returning call): materialize so the call
                    // is emitted; scheduled temp Frees handle the drop.
                    let _ = Self::eval_inst_struct(builder, ctx, operand)?;
                } else {
                    let _ = Self::eval_inst(builder, ctx, operand)?;
                }
                Ok(Terminator::None)
            }
            TirTag::IfStmt => Self::generate_if_stmt(builder, ctx, r),
            TirTag::Assign => {
                let view = ctx.tir.assign_view(r);
                if is_fat_type(inst.ty, ctx.pool) {
                    // Consuming reassign-concat fast path: the ownership
                    // pass proved the lhs binding dies at this reassign, so
                    // codegen appends in place and skips the
                    // free_on_reassign free below by never reaching it.
                    if let Some(concat_ref) = ctx.sidecar.consumed_concat_lhs[r.index()] {
                        return Self::emit_consuming_concat_assign(builder, ctx, r, concat_ref);
                    }
                    // `read_slot` copies the FatLocals out (three Cranelift
                    // `Variable` newtypes + home), so no table borrow
                    // survives into the free declaration below, which
                    // needs &mut ctx.module.
                    let locals = Self::read_slot(&ctx.fat_locals, view.name).ok_or_else(|| {
                        format!(
                            "Undefined fat variable in assign: '{}'",
                            ctx.pool.str(view.name)
                        )
                    })?;
                    let is_bytes = matches!(ctx.pool.kind(inst.ty), TypeKind::Bytes);
                    // Home-backed target whose RHS cannot reference the
                    // binding: free the old buffer FIRST, then let the
                    // producer write the home directly (free-before-
                    // overwrite). An RHS that reads the binding (e.g. a
                    // non-consuming `s = s + "x"`) takes the eval-then-
                    // free-then-store order instead — freeing first
                    // would be a use-after-free.
                    let direct = locals.home.is_some()
                        && writes_out_slot(ctx.tir, ctx.pool, view.value)
                        && !Self::expr_refs_name(ctx.tir, view.value, view.name);
                    let mut old_freed = false;
                    // Elision, same predicate as the scheduled-free
                    // path: when the home's CURRENT contents are
                    // provably free-noop (`locals.home_inline`), the
                    // old-value free is a guaranteed no-op either way.
                    if direct
                        && !locals.home_inline
                        && ctx.sidecar.free_on_reassign[r.index()].is_some()
                    {
                        let (old_ptr, old_cap) =
                            Self::emit_fat_load_ptr_cap(builder, ctx, view.name)
                                .expect("fat_locals entry read above");
                        if !Self::is_static_cap_zero(builder.func, old_cap) {
                            let free_ref = if is_bytes {
                                Self::declare_bytes_free(ctx, builder)?
                            } else {
                                Self::declare_str_free(ctx, builder)?
                            };
                            builder.ins().call(free_ref, &[old_ptr, old_cap]);
                        }
                        old_freed = true;
                    }
                    let repr = Self::eval_inst_fat_slot(
                        builder,
                        ctx,
                        view.value,
                        if direct { locals.home } else { None },
                    )?;
                    let (ptr, len, cap) = match repr {
                        ValueRepr::Str { ptr, len, cap } | ValueRepr::Bytes { ptr, len, cap } => {
                            (ptr, len, cap)
                        }
                        _ => unreachable!("fat-typed assign should produce a fat ValueRepr"),
                    };
                    // Free the old allocation before overwriting locals.
                    // sidecar.free_on_reassign[r] is set whenever the
                    // ownership pass observed a Valid old owner at this
                    // Assign. The old (ptr, cap) live in the binding's
                    // current storage (home slot or FatLocals Variables)
                    // — NOT in inst_values[old_owner], which holds the
                    // literal's original (ptr, cap) at its emission point
                    // and may be stale across reassigns.
                    if !old_freed
                        && ctx.sidecar.free_on_reassign[r.index()].is_some()
                        && !(locals.home.is_some() && locals.home_inline)
                    {
                        let (old_ptr, old_cap) =
                            Self::emit_fat_load_ptr_cap(builder, ctx, view.name)
                                .expect("fat_locals entry read above");
                        if !Self::is_static_cap_zero(builder.func, old_cap) {
                            let free_ref = if is_bytes {
                                Self::declare_bytes_free(ctx, builder)?
                            } else {
                                Self::declare_str_free(ctx, builder)?
                            };
                            builder.ins().call(free_ref, &[old_ptr, old_cap]);
                        }
                    }
                    match locals.home {
                        // In direct mode the producer already wrote the
                        // home; only the aliasing fallback stores here.
                        Some(home) if !direct => {
                            let addr = builder.ins().stack_addr(ctx.int_type, home, 0);
                            builder.ins().store(MemFlagsData::trusted(), ptr, addr, 0);
                            builder.ins().store(MemFlagsData::trusted(), len, addr, 8);
                            builder.ins().store(MemFlagsData::trusted(), cap, addr, 16);
                        }
                        Some(_) => {}
                        None => {
                            builder.def_var(locals.ptr, ptr);
                            builder.def_var(locals.len, len);
                            builder.def_var(locals.cap, cap);
                        }
                    }
                    if locals.home.is_some() {
                        let inline =
                            Self::home_value_provably_inline(ctx, builder.func, view.value, cap);
                        Self::set_home_inline(ctx, view.name, inline);
                    }
                    Self::kill_fact(ctx, view.name);
                    return Ok(Terminator::None);
                }
                if ctx.pool.is_view(inst.ty) {
                    let repr = Self::eval_inst_view(builder, ctx, view.value)?;
                    let ValueRepr::View { ptr, len } = repr else {
                        unreachable!("view-typed assign should produce ValueRepr::View");
                    };
                    let locals = Self::read_slot(&ctx.view_locals, view.name).ok_or_else(|| {
                        format!(
                            "Undefined strview variable in assign: '{}'",
                            ctx.pool.str(view.name)
                        )
                    })?;
                    // Views are borrows — no free-on-reassign; just
                    // reseat the pair.
                    builder.def_var(locals.ptr, ptr);
                    builder.def_var(locals.len, len);
                    Self::kill_fact(ctx, view.name);
                    return Ok(Terminator::None);
                }
                if matches!(ctx.pool.kind(inst.ty), TypeKind::Struct) {
                    return Self::emit_struct_assign(builder, ctx, r);
                }
                let val = Self::eval_inst(builder, ctx, view.value)?;
                // Kill AFTER evaluating the RHS: `x = x + 1` must still
                // see the old fact while its right-hand side is emitted.
                Self::kill_fact(ctx, view.name);
                let var = Self::read_slot(&ctx.locals, view.name).ok_or_else(|| {
                    format!(
                        "Undefined variable in assign: '{}'",
                        ctx.pool.str(view.name)
                    )
                })?;
                builder.def_var(var, val);
                Ok(Terminator::None)
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
                Self::emit_due_promo_frees(builder, ctx, r)?;
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
                Self::emit_due_promo_frees(builder, ctx, r)?;
                let Some(loop_ctx) = ctx.loop_stack.last() else {
                    return Err("codegen reached continue outside loop".to_string());
                };
                builder.ins().jump(loop_ctx.continue_target, &[]);
                Ok(Terminator::Continue)
            }
            // M9 struct field assignment — lowered in codegen/structs.rs.
            TirTag::FieldAssign => Self::emit_field_assign(builder, ctx, r),
            TirTag::CompoundFieldAssign => Self::emit_compound_field_assign(builder, ctx, r),
            other => Err(format!(
                "emit_stmt: instruction at %{} is not a statement (tag={:?})",
                r.index(),
                other
            )),
        }
    }
}

#[cfg(test)]
mod tests;
