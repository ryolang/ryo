//! Builtin-call emission for the CLI intrinsics — split from `expr.rs`
//! (file-length); the arms stay dispatched from `eval_inst` /
//! `eval_inst_fat_slot`'s interned-id chains, which call these as
//! ordinary `Codegen` methods.

use super::{Codegen, FunctionContext};
use cranelift::codegen::ir::StackSlot;
use cranelift::prelude::*;
use cranelift_module::Module;
use ryo_core::tir::TirRef;

impl<M: Module> Codegen<M> {
    /// `process_exit(code)` → `ryo_exit(code: u64)`, backed by the
    /// runtime's `exit()`. TODO(M24): interim call form — replaced by
    /// `process.exit`. The trap after the call is unreachable in
    /// practice; it keeps Cranelift honest about the never-returns
    /// contract, same as the `__ryo_panic` arm.
    pub(crate) fn emit_process_exit(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let view = ctx.tir.call_view(r);
        debug_assert_eq!(
            view.args.len(),
            1,
            "sema should reject process_exit() arity errors"
        );
        let arg = Self::eval_inst(builder, ctx, view.args[0])?;
        let exit_ref = Self::declare_runtime_fn(ctx, builder, "ryo_exit", &[ctx.int_type], &[])?;
        builder.ins().call(exit_ref, &[arg]);
        builder.ins().trap(
            TrapCode::user(1).expect("user trap code 1 is within Cranelift's encodable range"),
        );
        let dead = builder.create_block();
        builder.seal_block(dead);
        builder.switch_to_block(dead);
        Ok(builder.ins().iconst(types::I8, 0))
    }

    /// `process_argc() -> int` → `ryo_process_argc() -> i64`, the
    /// runtime's argv count (argv[0] included, matching the C
    /// convention). TODO(M22): interim call form — replaced by
    /// `process.args`.
    pub(crate) fn emit_process_argc(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let view = ctx.tir.call_view(r);
        debug_assert!(
            view.args.is_empty(),
            "sema should reject process_argc() arity errors"
        );
        let argc_ref =
            Self::declare_runtime_fn(ctx, builder, "ryo_process_argc", &[], &[ctx.int_type])?;
        let call = builder.ins().call(argc_ref, &[]);
        let results = builder.inst_results(call);
        Ok(results[0])
    }

    /// `process_argv(i: int) -> str` → `ryo_process_argv(i: u64, out:
    /// *mut RyoStrFat)`. NOTE the argument order: unlike every other
    /// slot-out producer (`out` first — see `emit_slot_out_call`), the
    /// runtime's argv accessor takes the index first (its runtime tests
    /// call `ryo_process_argv(3, &mut slot)`), so this lowers through
    /// `emit_slot_out_call_out_last`. The runtime copies argv[i] into
    /// the tagged slot (SSO inline or fresh heap buffer); out-of-range
    /// panics there. TODO(M22): interim call form — replaced by
    /// `process.args`.
    pub(crate) fn emit_process_argv(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        index: Value,
        out_slot: Option<StackSlot>,
    ) -> Result<(Value, Value, Value), String> {
        Self::emit_slot_out_call_out_last(
            builder,
            ctx,
            "ryo_process_argv",
            &[(ctx.int_type, index)],
            out_slot,
        )
    }

    /// `io_eprint(arg)` → `ryo_eprint(ptr, len: u64)` — the exact twin
    /// of the `print` arm, writing fd 2 (stderr) instead of fd 1.
    /// Sema has already rewritten bytes/scalar/struct arguments to their
    /// str reprs, so the argument is either repr here.
    /// TODO(M24): interim call form — replaced by `io.eprint`.
    pub(crate) fn emit_io_eprint(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        r: TirRef,
    ) -> Result<Value, String> {
        let view = ctx.tir.call_view(r);
        debug_assert_eq!(
            view.args.len(),
            1,
            "sema should reject io_eprint() arity errors"
        );
        let (ptr, len) = Self::eval_str_or_view_parts(builder, ctx, view.args[0])?;
        let eprint_ref =
            Self::declare_runtime_fn(ctx, builder, "ryo_eprint", &[ctx.int_type, types::I64], &[])?;
        builder.ins().call(eprint_ref, &[ptr, len]);
        Ok(builder.ins().iconst(ctx.int_type, 0))
    }

    /// `io_read_line() -> str` → `ryo_read_line(out: *mut RyoStrFat)`.
    /// Reads one line from stdin (fd 0) into the tagged str slot: the
    /// runtime strips the trailing `\n` and maps EOF (before any byte)
    /// to "". The out-slot is the call's only parameter, passed last —
    /// the arg-less case of the `process_env` slot pattern. Runtime
    /// read errors panic there (stderr + exit 101) until M13.6's
    /// `IoError!str` shape gives them a channel.
    /// TODO(M13.6): interim call form — replaced by
    /// `io.read_line() -> IoError!str`.
    pub(crate) fn emit_io_read_line(
        builder: &mut FunctionBuilder,
        ctx: &mut FunctionContext<'_, M>,
        out_slot: Option<StackSlot>,
    ) -> Result<(Value, Value, Value), String> {
        Self::emit_slot_out_call_out_last(builder, ctx, "ryo_read_line", &[], out_slot)
    }
}
