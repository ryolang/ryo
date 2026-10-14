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

mod arith;
mod builtins;
mod bytes;
mod control;
mod decls;
mod enums;
mod expr;
mod frees;
mod jit;
mod match_;
mod ranges;
mod recursion;
mod str_ops;
mod structs;
mod tail;
mod views;

pub(crate) use structs::{is_fat_type, is_struct_type};

pub(crate) use enums::is_enum_type;

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
/// free bookkeeping (due frees + sweeps), matching the old `emit_body`
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
        // Enums (M11) are slot-allocated aggregates exactly like
        // structs; the memory-first paths gate before scalar mapping.
        TypeKind::Enum => unreachable!("cranelift_type_for: enum TypeId reached codegen"),
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
    /// Interned-name ids for per-function / per-instruction dispatch
    /// (`main` detection, call-form routing, the slot-out producer
    /// chain). Resolved from the pool at the top of `compile_all` and
    /// valid for the duration of that compilation; `Default` (all
    /// `None`) only until the first compile.
    name_ids: CodegenNameIds,
    /// Names of functions whose bodies contain a syntactic
    /// tail-position self-call, collected by the pre-pass in
    /// `prepare_compilation`. `build_signature` compiles marked
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
type DeclaredFunctions = HashMap<StringId, (FuncId, Signature)>;

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
    /// Whether the function being lowered is `main`, resolved by
    /// `compile_function` from [`CodegenNameIds`]. The ReturnVoid arm
    /// needs it to emit the C ABI's int-0 return.
    is_main: bool,
    /// Whether the function being lowered was compiled with
    /// `CallConv::Tail`: its name was in `Codegen`'s
    /// `tail_candidates` when `build_signature` ran. `return_call` is
    /// only legal from a `tail`-convention function, so the tail path
    /// consults this before attempting one.
    tail_conv_candidate: bool,
    /// Interned-name ids resolved once per compilation; the per-call
    /// dispatch in `emit_call_slot`, the literal hoisting exclusion,
    /// and the slot-out exclusion all compare `StringId` equality
    /// against these.
    name_ids: CodegenNameIds,
    /// Scalar binding name → Cranelift `Variable`. Dense table indexed
    /// by `StringId::raw()`, sized once per function from
    /// `pool.string_count()` (codegen never interns, so every name in
    /// the TIR is in bounds). Binding names are not unique per
    /// function — a nested VarDecl shadows the outer slot — so every
    /// write goes through `write_slot`, which records the previous
    /// slot value on `locals_undo`; the Flow-block driver's region
    /// prologues replay the log down to each construct's marks
    /// so sibling regions never observe each other's bindings.
    locals: Vec<Option<Variable>>,
    locals_undo: Vec<(u32, Option<Variable>)>,
    /// Value-range facts for spec §18 guard elision: binding name →
    /// inclusive `[lo, hi]` bounds proven by dominating comparisons
    /// (see `codegen/ranges.rs` for the seeding rules). Dense
    /// `StringId::raw()`-indexed table with an undo log, scoped like
    /// `locals`: the driver's join/exit prologues restore it via
    /// `restore_slots`, and any binding assigned inside a restored
    /// scope is killed at the join via `assigned_log`.
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
    func_ids: &'a DeclaredFunctions,
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
    ///   (b) `BoolAnd`/`BoolOr` merge values via the short-circuit
    ///       join block's I8 param (phi node), so the memoized `Value`
    ///       is the join-block param, which dominates every downstream
    ///       use;
    ///   (c) `IfStmt` is statement-level — no values flow out of
    ///       branches, so no branch-local value is ever read after the
    ///       merge. `TirTag::Match` (M12) is expression-level control
    ///       flow, but its results flow out through a pre-allocated
    ///       result slot (loaded/cached at the join by the MatchJoin
    ///       prologue — never a block param), and arm-body expressions
    ///       are scoped to their arm block (never read across the
    ///       merge).
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
    /// Dense flag per `sidecar.drops` index: true once that entry's
    /// Free has been consumed — either emitted at its Flow fire
    /// position or marked consumed by the lowering-time provably-inline
    /// pre-filter (position-mechanical: elision is a lowering-time no-op-free decision,
    /// not an ownership decision). Each drop lives in exactly one fire
    /// bucket, so consumption is exactly once by construction; the flag
    /// both guards the pre-filter against the fire paths and backs the
    /// debug-only all-consumed assertion.
    freed_at: Vec<bool>,
    /// Debug-only: per-drop flag marking drops consumed AT A FIRE
    /// POINT (the fire-map buckets, including same-target duplicates
    /// consumed there) — as opposed to the lowering-time pre-filter's
    /// marks in `freed_at`. The split lets the place-resolution
    /// assertion check every pre-elided drop against the elision
    /// predicate (a pre-elided drop never resolved storage, so the
    /// "has storage" leg must not excuse it) and every fired drop
    /// against storage resolution.
    #[cfg(debug_assertions)]
    fire_marked: Vec<bool>,
    /// Targets whose destruction has already been emitted in the
    /// CURRENT block visit (position-mechanical) — cleared per block, so the dedup is
    /// per-path, not global: the ownership pass deliberately
    /// double-anchors some frees (the consumer step AND the enclosing
    /// Return's epilogue, both firing in the same block's terminator
    /// handling) — a sweep-era belt-and-suspenders that relied on the
    /// duplicate's anchor being unmaterializable. Fired mechanically
    /// from position, both duplicates would run, so the
    /// once-per-target-per-path invariant is enforced at consumption: a
    /// drop whose target was already destroyed in this block is consumed
    /// as a no-op. Same-target drops in DIFFERENT blocks are different
    /// paths and all fire. Elided (no-op) drops do NOT record their
    /// target here — the buffer was never freed, so a later same-target
    /// drop must still fire.
    freed_targets: std::collections::HashSet<TirRef>,
    /// The drop fire map (position-mechanical): scheduled drops keyed by their Flow
    /// fire position, built once per function before emission. Firing
    /// is mechanical from the Flow position — no cached-value
    /// representation, pending-sweep state, or materialization order
    /// decides whether a drop fires.
    ///
    /// * `fire_before_step[inst]` — `BeforeStep{inst}` drops, fired by
    ///   the block driver immediately before the step emits.
    /// * `fire_after_stmt[inst]` — `AfterStep` drops whose anchor is a
    ///   step of the statement rooted at `inst` (a value-step anchor
    ///   fires at its enclosing statement's end — after the consumer,
    ///   the only sound position for a last-use free), fired by the
    ///   post-statement hook.
    /// * `fire_by_payload[inst]` — `AfterStep` drops anchored on a
    ///   terminator payload (if/loop/match/return/break/continue
    ///   instruction), fired by that construct's handler at its single
    ///   emit position (the join/exit prologue, the terminator arm).
    /// * `fire_at_term[block]` — `PreTerminator{block}` drops plus
    ///   `AfterStep` drops whose anchor step has no enclosing statement
    ///   (match-arm body expressions, deferred return operands), fired
    ///   inside the block's terminator handler at its canonical point
    ///   (after operand/body evaluation, before the jump/return).
    fire_before_step: Vec<Vec<usize>>,
    fire_after_stmt: Vec<Vec<usize>>,
    fire_by_payload: Vec<Vec<usize>>,
    fire_at_term: HashMap<ryo_core::flow::BlockId, Vec<usize>>,
    /// `BlockStart` drops (position-mechanical unification): fired once per
    /// Flow block, when the driver switches into the mapped Cranelift
    /// block, BEFORE the block's steps. Two kinds share the bucket,
    /// told apart at the fire point by target identity: match-arm-start
    /// payload destruction (the block is an arm of a match and the
    /// target is that match's scrutinee — codegen recomputes the arm's
    /// unbound needs-drop fields and drops each) and conditional dead
    /// drops of pre-branch buffers (target = the pre-branch owner,
    /// resolved through the binding tables exactly as the old
    /// block-start emission did).
    fire_at_block_start: HashMap<ryo_core::flow::BlockId, Vec<usize>>,
    /// ---- Flow-block driver state ----
    /// One Cranelift block per Flow IR block, pre-created 1:1 before
    /// the driver runs. The entry Flow block reuses the function's
    /// entry block (index 0). The ONLY block params: a Flow block whose
    /// first step is a `BoolAnd`/`BoolOr` (the short-circuit join) gets
    /// one I8 param — the merged boolean. Every other value merge
    /// (match results) rides a pre-allocated result slot, never a
    /// block param.
    flow_blocks: Vec<Block>,
    /// Per-Flow-block sealing discipline: a block seals once every Flow
    /// predecessor's terminator has been emitted (loop headers
    /// naturally wait for their backedge); unreachable no-predecessor
    /// blocks self-seal right after their own terminator. Entry starts
    /// sealed.
    flow_sealed: Vec<bool>,
    /// Per-Flow-block count of emitted predecessor terminators, for
    /// the sealing rule above.
    flow_preds_emitted: Vec<u32>,
    /// Debug-only visit state per Flow block: low 7 bits count
    /// emissions (the driver must visit each block at most once), bit 7
    /// marks the terminator dispatched (a tail call counts — the
    /// `return_call` terminates the block). Release builds compile this
    /// out entirely.
    #[cfg(debug_assertions)]
    flow_visit_state: Vec<u8>,
    /// The prologue the driver runs when reaching each block (scope
    /// restores / fact seeding / match merge handling).
    block_prologue: Vec<BlockPrologue>,
    /// Dense flag: the block's first step is a `BoolAnd`/`BoolOr` — its
    /// Cranelift block carries the merged-boolean I8 param, and edges
    /// INTO it pass the merge value (constant on the short-circuit
    /// edge, the rhs value on the rhs edge).
    bool_join: Vec<bool>,
    /// Dense flag: the block's last step is an `ExprStmt` calling a
    /// function, in tail position (last statement of the function body
    /// or of an if-cascade arm body, no loop/match arm in between).
    /// The driver defers the operand subtree so `try_emit_tail_call`
    /// sees exactly the cache state the old recursive emitter saw.
    tail_candidate: Vec<bool>,
    /// Dense map: arm fallthrough block → the `IfStmt` it belongs to
    /// (then block, elif body block, or else block). Drives the tail
    /// recursion (`construct_is_last`) and confirms a Goto-target arm
    /// region is an if arm.
    if_arm_owner: Vec<Option<TirRef>>,
    /// Dense map: block → the match whose join block it is. A match
    /// join can coincide with another region entry (an elif whose
    /// condition is a match holds its Branch in the condition's join
    /// block, and a `while match …:` header is the match's join), so
    /// the join handling (result caching + fact/slot restores) runs
    /// from this map BEFORE the block's region prologue, independent
    /// of the `block_prologue` variant.
    match_join: Vec<Option<TirRef>>,
    /// Dense map: a match arm's fallthrough block (the block ending in
    /// `Goto { join }`) → (match, arm). Each falling-through arm
    /// contributes exactly one such edge; the source can be deep inside
    /// the arm (a nested value match's join), so a prologue-based
    /// source check is not enough — the owner arm is found by a
    /// forward walk from each arm block at map time.
    match_fallthrough: Vec<Option<(TirRef, usize)>>,
    /// Construct records, created when the construct's entry block is
    /// emitted (marks taken then), consumed by prologues and handlers.
    if_records: HashMap<TirRef, IfRecord>,
    loop_records: HashMap<TirRef, LoopRecord>,
    /// Per-match state for the Flow-block driver: fact/slot
    /// marks (taken at the pre-step hook), the scrutinee home, the
    /// merge shape, the pre-allocated result slot, and per-arm token
    /// offsets. Created before the scrutinee steps emit, filled by the
    /// Switch terminator handler, consumed by arm prologues, the join
    /// prologue, and arm fallthroughs.
    match_records: HashMap<TirRef, match_::MatchRecord>,
    /// Whether the current function returns through a hidden sret
    /// pointer (fat/struct/enum) — the synthesized fall-off return
    /// needs it, recomputed from the signature per function.
    has_sret: bool,
    /// Owned fat (str/bytes) bindings: three SSA `Variable`s (ptr, len,
    /// cap) per binding name. Dense `StringId::raw()`-indexed table with
    /// an undo log, same scoping discipline as `locals`.
    fat_locals: Vec<Option<FatLocals>>,
    fat_locals_undo: Vec<(u32, Option<FatLocals>)>,
    /// Borrowed-param view bases (Slice/ToView) that were promoted
    /// through a scratch slot instead of the param's FatLocals: base
    /// inst → slot holding flag (offset 0) + promoted triple (8/16/24).
    promo_slots: HashMap<TirRef, StackSlot>,
    /// Bases whose promotion ALLOC ran (the promote-on-view flag=1
    /// store) — the end-of-function pairing assert requires each to
    /// have a fired release (Gap 5).
    promo_alloc_fired: std::collections::HashSet<TirRef>,
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
    /// Binding name → its most recent write in program order (VarDecl
    /// initializer or `Assign` value; fat params map to their sentinel
    /// ref), built alongside `free_binding_names`. `emit_frees` only
    /// takes the binding-path redirect when the drop's target IS
    /// this write — a stale/superseded target would free the binding's
    /// current value instead of its own buffer. Keyed by BINDING
    /// identity (the declaring VarDecl's ref, or a fat param's sentinel
    /// ref), not by name: one name can denote several bindings (a
    /// same-named shadow), and name-keyed lookup lets a later shadow's
    /// writes suppress the branch-divergent-reseat redirect of an outer
    /// binding's Free (the invalid-free + leak in the taken-arm reseat
    /// + shadow composition).
    binding_last_write: HashMap<TirRef, TirRef>,
    /// Free-target (initializer / Assign value) → BINDING identity (the
    /// written Flow place's declaring `VarDecl` ref, for both write
    /// kinds — the single-pass Flow-derived builder never reads
    /// `sidecar.assign_binding`), built alongside `free_binding_names`.
    /// Params need no entry — their binding identity is their own
    /// sentinel ref (see `free_binding_of`). `emit_frees` pairs this
    /// with `binding_last_write` to keep the redirect's lineage tracking
    /// per-binding while `fat_locals` stay name-keyed.
    free_binding_of_insts: Vec<Option<TirRef>>,
    /// Every drop target in this function's schedule. `emit_frees`
    /// consults it to distinguish a superseded redirect target whose
    /// most recent write IS freed elsewhere (loop-carried reassign: do
    /// not redirect — the write's own Free owns the home slot) from the
    /// branch-divergent reseat (the write has no Free: the pre-branch
    /// owner's Free must redirect to cover the path-correct buffer).
    all_free_targets: std::collections::HashSet<TirRef>,
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
    /// threads the resulting per-function entry here. Drops are
    /// consumed mechanically at their anchor instructions; arm-only
    /// drops are gated by block membership — their anchors are only
    /// evaluated on the arm's path — not by any branch predicate.
    sidecar: &'a ryo_core::ownership::FunctionSidecar,
    /// The canonical Flow IR for this body — the backend's emission
    /// driver: one pre-created Cranelift block per Flow block,
    /// walked in DFS order from the entry, with per-step evaluation and
    /// terminator handlers. Positional with the sidecar entry (both
    /// are built once per pipeline run, right after sema).
    flow: &'a ryo_core::flow::Flow,
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
    /// Cold block for the stack-limit check, created by the
    /// first `emit_stack_check` in the function and shared by every
    /// later check; emitted end-of-function beside the deferred panic
    /// blocks. `None` while no check has been emitted.
    stack_cold_block: Option<Block>,
    /// SCC ids of the recursive functions (`Codegen::recursive_sccs`):
    /// a user call gets the stack-limit check iff caller and callee
    /// share an id (a call-graph cycle edge).
    recursive_sccs: &'a HashMap<StringId, u32>,
    /// Block the last stack-limit check fell through into. The stack
    /// pointer is fixed for the whole function body, so a check that
    /// already ran earlier in the current block covers every later
    /// cycle-edge call in it (e.g. both recursive calls of
    /// `fibonacci`).
    stack_checked_block: Option<Block>,
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
            name_ids: CodegenNameIds::default(),
            tail_candidates: HashSet::new(),
            recursive_sccs: HashMap::new(),
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

impl<M: Module> Codegen<M> {
    fn prepare_compilation(
        &mut self,
        tirs: &[Tir],
        pool: &InternPool,
    ) -> Result<DeclaredFunctions, String> {
        // Tail-call pre-pass: mark functions whose bodies contain a
        // syntactic tail-position self-call, so `build_signature`
        // compiles them with CallConv::Tail (see
        // `tail::scan_tail_call_candidates` for the scope).
        self.tail_candidates = tail::scan_tail_call_candidates(tirs, self.name_ids.main);
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
        debug_assert!(
            bytes::no_unreachable_in(tirs),
            "codegen requires sema to have produced TIR with no Unreachable instructions"
        );
        debug_assert_eq!(tirs.len(), flows.len(), "one Flow IR per TIR body");
        self.name_ids = CodegenNameIds::resolve(pool);
        let func_ids = self.prepare_compilation(tirs, pool)?;

        let mut ir_output = String::new();
        for (i, tir) in tirs.iter().enumerate() {
            let text = self.compile_function(tir, &func_ids, pool, sidecar, flows, i, dump_ir)?;
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
            let sig = self.build_signature(tir, pool, is_main);
            let name_str = pool.str(tir.name);
            let linkage = if is_main {
                Linkage::Export
            } else {
                Linkage::Local
            };
            let func_id = self
                .module
                .declare_function(name_str, linkage, &sig)
                .map_err(|e| format!("Failed to declare function '{}': {}", name_str, e))?;
            // Keep the signature: `compile_function` installs it into
            // `ctx.func` instead of rebuilding it.
            func_ids.insert(tir.name, (func_id, sig));
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
        free_binding_names: &[Option<StringId>],
        free_binding_param_names: &[Option<StringId>],
        free_binding_of_insts: &[Option<TirRef>],
        binding_last_write: &HashMap<TirRef, TirRef>,
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
        for (idx, drop) in func_sidecar.drops.iter().enumerate() {
            match drop.position {
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
                            fire_points.resolve(drop.position),
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
                            fire_points.resolve(drop.position),
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
                            fire_points.resolve(drop.position),
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
        Self::pre_elide_drops(
            tir,
            pool,
            fat_mutated,
            view_base_insts,
            free_binding_names,
            free_binding_param_names,
            free_binding_of_insts,
            binding_last_write,
            func_sidecar,
            &mut freed_at,
        );
        DropFireMap(
            fire_before_step,
            fire_after_stmt,
            fire_by_payload,
            fire_at_term,
            fire_at_block_start,
            freed_at,
        )
    }

    /// Promotion-free anchor tables: `promo_free_by_after` keys each
    /// promotion free by its anchor instruction (the promo family keeps
    /// its own anchor hook — I-216), `pending_promo_sweep` starts as
    /// every index for the end-of-statement sweep.
    fn build_promo_fire_tables(
        tir: &Tir,
        func_sidecar: &ryo_core::ownership::FunctionSidecar,
    ) -> (Vec<Vec<usize>>, Vec<usize>) {
        let mut promo_free_by_after: Vec<Vec<usize>> = vec![Vec::new(); tir.instructions.len()];
        for (idx, pf) in func_sidecar.promotion_frees.iter().enumerate() {
            let anchor = pf
                .position
                .anchor_inst()
                .expect("promotion frees are step-anchored (AfterStep)");
            debug_assert!(
                !anchor.is_param(),
                "promotion-free anchors are never param sentinel refs"
            );
            promo_free_by_after[anchor.index()].push(idx);
        }
        let pending_promo_sweep: Vec<usize> = (0..func_sidecar.promotion_frees.len()).collect();
        (promo_free_by_after, pending_promo_sweep)
    }

    #[allow(clippy::too_many_arguments)]
    fn compile_function(
        &mut self,
        tir: &Tir,
        func_ids: &DeclaredFunctions,
        pool: &InternPool,
        sidecar: &ryo_core::ownership::OwnershipSidecar,
        flows: &[ryo_core::flow::Flow],
        sidecar_index: usize,
        dump_ir: bool,
    ) -> Result<String, String> {
        // Interned-name ids for this compilation (resolved by
        // `compile_all`); copied out so the block below can read them
        // while `builder` holds mutable borrows of `self`.
        let ids = self.name_ids;
        let (func_id, sig) = func_ids
            .get(&tir.name)
            .ok_or_else(|| format!("Function '{}' not declared", pool.str(tir.name)))?;
        let func_id = *func_id;

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

        // The signature built at declaration time; cloning two small
        // Vecs beats rebuilding the signature (pool queries + per-param
        // ABI branching) a second time.
        self.ctx.func.signature = sig.clone();

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

            let is_main = ids.main == Some(tir.name);
            let returns_fat = !is_main && is_fat_type(tir.return_type, pool);
            let returns_struct = !is_main && is_struct_type(tir.return_type, pool);
            let returns_enum = !is_main && is_enum_type(tir.return_type, pool);
            let has_sret = returns_fat || returns_struct || returns_enum;
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
                    } else if is_struct_type(param.ty, pool) || is_enum_type(param.ty, pool) {
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
                } else if is_struct_type(param.ty, pool) || is_enum_type(param.ty, pool) {
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

            // The canonical Flow IR for this body — positional with the
            // sidecar entry (both are built once per pipeline run,
            // right after sema).
            let flow = flows.get(sidecar_index).ok_or_else(|| {
                format!(
                    "no Flow built for '{}' (index {} of {})",
                    pool.str(tir.name),
                    sidecar_index,
                    flows.len()
                )
            })?;
            let (
                free_binding_names,
                free_binding_param_names,
                free_binding_of_insts,
                binding_last_write,
            ) = Self::build_free_binding_names(tir, flow, pool);
            let (fat_mutated, view_base_insts) = Self::build_fat_mutation_tables(tir, pool, &ids);
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
                &free_binding_names,
                &free_binding_param_names,
                &free_binding_of_insts,
                &binding_last_write,
                func_sidecar,
            );

            let (promo_free_by_after, pending_promo_sweep) =
                Self::build_promo_fire_tables(tir, func_sidecar);

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
                is_main,
                tail_conv_candidate: self.tail_candidates.contains(&tir.name),
                name_ids: ids,
                locals,
                locals_undo,
                range_facts: vec![None; pool.string_count()],
                range_facts_undo: Vec::new(),
                assigned_log: Vec::new(),
                func_ids,
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
                fat_locals: fat_param_locals,
                fat_locals_undo,
                promo_slots,
                promo_freed_at: vec![false; func_sidecar.promotion_frees.len()],
                promo_alloc_fired: std::collections::HashSet::new(),
                promo_free_by_after,
                pending_promo_sweep,
                view_locals: view_param_locals,
                view_locals_undo,
                struct_locals: struct_param_locals,
                struct_locals_undo,
                free_binding_names,
                free_binding_param_names,
                free_binding_of_insts,
                binding_last_write,
                all_free_targets: func_sidecar.drops.iter().map(|d| d.target).collect(),
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
            Self::hoist_str_literals(&mut builder, &mut ctx)?;

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

    /// Emit one Flow IR STEP: a statement-level instruction, or the
    /// materialization of a value instruction whose consumers come
    /// later in the block (steps are in evaluation order, so operand
    /// steps populate the value caches before their consumers).
    /// Statement-level steps also fire the end-of-statement free
    /// bookkeeping (due frees + sweeps) — the Flow-block driver's
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
                } else if matches!(ctx.pool.kind(ty), TypeKind::Enum) {
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
            // only sound position for a last-use free. The promotion
            // family keeps its own anchor hook + sweep.
            Self::fire_after_statement(builder, ctx, r)?;
            Self::emit_due_promo_frees(builder, ctx, r)?;
            Self::sweep_due_promo_frees(builder, ctx)?;
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
        } else if matches!(ctx.pool.kind(operand_ty), TypeKind::Enum) {
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
