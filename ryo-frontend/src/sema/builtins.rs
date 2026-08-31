//! Builtin-call emission and validation — split from `mod.rs`.

use super::{FuncCtx, Scope, Sema, borrow_target_reason};
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{ParamMode, TirRef, TirTag};
use ryo_core::types::{StringId, TypeKind, ViewKind};
use ryo_core::uir::{CallView, InstData, InstRef, InstTag, Span};

/// Front-end validation for builtin calls.
pub(crate) fn emit_builtin_call(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    view: &CallView,
    arg_tirs: &[TirRef],
    span: Span,
    builtin: &'static crate::builtins::BuiltinFunction,
) -> TirRef {
    let name = sema.pool.str(view.name);
    // Builtins never take ownership of their arguments — every arg
    // is borrowed regardless of declared type.
    let modes = vec![ParamMode::Borrow; arg_tirs.len()];
    // --- M8.3: `&`/`inout` agreement for builtin calls (builtins
    // bypass `check_call`, so the check is replayed here). `&` is only
    // valid on an `inout` parameter — today only the first argument of
    // str_push/bytes_push — everywhere else it is rejected, exactly
    // like user-function calls. The push arms additionally validate
    // the mutable lvalue of that first argument.
    let builtin_modes: Vec<ParamMode> = if name == "str_push" || name == "bytes_push" {
        vec![ParamMode::Inout, ParamMode::Borrow]
    } else {
        modes.clone()
    };
    for (idx, arg_uir) in view.args.iter().enumerate() {
        let arg_is_borrow = matches!(sema.uir.inst(*arg_uir).tag, InstTag::Borrow);
        let param_is_inout =
            builtin_modes.get(idx).copied().unwrap_or(ParamMode::Borrow) == ParamMode::Inout;
        if arg_is_borrow && !param_is_inout {
            sema.sink.emit(
                Diag::error(
                    sema.uir.span(*arg_uir),
                    DiagCode::BorrowMismatch,
                    format!(
                        "argument {} is passed by `&` but parameter is not `inout`",
                        idx + 1
                    ),
                )
                .with_help("remove the `&`, or declare the parameter `inout`"),
            );
        }
    }
    match name {
        "print" => {
            if !check_print_args(sema, fcx, view, arg_tirs, span) {
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            // M8.4.2: print(bytes/bytesview) renders the escaped repr —
            // rewrite to `print(__ryo_bytes_repr(arg))` at the TIR level
            // so the repr temp is a normal ownership-tracked str
            // producer (a codegen-synthesized temp would never be freed).
            let arg_ty = fcx.builder.ty_of(arg_tirs[0]);
            let owned_args;
            let effective: &[TirRef] = if matches!(
                sema.pool.kind(arg_ty),
                TypeKind::Bytes | TypeKind::View(ViewKind::Bytes)
            ) {
                let callee = sema.pool.intern_str("__ryo_bytes_repr");
                let repr = fcx.builder.call(
                    callee,
                    &[arg_tirs[0]],
                    &[ParamMode::Borrow],
                    sema.pool.str_(),
                    span,
                );
                owned_args = vec![repr];
                &owned_args
            } else {
                arg_tirs
            };
            // W0003 case A: `print` takes `strview` directly.
            warn_redundant_materialize_builtin_arg(sema, fcx, view.args[0], effective[0], "print");
            let ret_ty = builtin.return_type(sema.pool);
            fcx.builder.call(view.name, effective, &modes, ret_ty, span)
        }
        "panic" => emit_panic(sema, fcx, view, span),
        "assert" => emit_assert(sema, fcx, view, arg_tirs, span),
        "int_to_str" => {
            if view.args.len() != 1 {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ArityMismatch,
                    format!(
                        "int_to_str() takes exactly 1 argument, got {}",
                        view.args.len()
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let arg_ty = fcx.builder.ty_of(arg_tirs[0]);
            if sema.pool.is_error(arg_ty) {
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            if !matches!(sema.pool.kind(arg_ty), TypeKind::Int) {
                sema.sink.emit(Diag::error(
                    sema.uir.span(view.args[0]),
                    DiagCode::TypeMismatch,
                    format!(
                        "int_to_str() argument must be int, got {}",
                        sema.pool.display(arg_ty)
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let ret_ty = builtin.return_type(sema.pool);
            fcx.builder.call(view.name, arg_tirs, &modes, ret_ty, span)
        }
        "float_to_str" => {
            if view.args.len() != 1 {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ArityMismatch,
                    format!(
                        "float_to_str() takes exactly 1 argument, got {}",
                        view.args.len()
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let arg_ty = fcx.builder.ty_of(arg_tirs[0]);
            if sema.pool.is_error(arg_ty) {
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            if !matches!(sema.pool.kind(arg_ty), TypeKind::Float) {
                sema.sink.emit(Diag::error(
                    sema.uir.span(view.args[0]),
                    DiagCode::TypeMismatch,
                    format!(
                        "float_to_str() argument must be float, got {}",
                        sema.pool.display(arg_ty)
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let ret_ty = builtin.return_type(sema.pool);
            fcx.builder.call(view.name, arg_tirs, &modes, ret_ty, span)
        }
        "bool_to_str" => {
            if view.args.len() != 1 {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ArityMismatch,
                    format!(
                        "bool_to_str() takes exactly 1 argument, got {}",
                        view.args.len()
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let arg_ty = fcx.builder.ty_of(arg_tirs[0]);
            if sema.pool.is_error(arg_ty) {
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            if !matches!(sema.pool.kind(arg_ty), TypeKind::Bool) {
                sema.sink.emit(Diag::error(
                    sema.uir.span(view.args[0]),
                    DiagCode::TypeMismatch,
                    format!(
                        "bool_to_str() argument must be bool, got {}",
                        sema.pool.display(arg_ty)
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let ret_ty = builtin.return_type(sema.pool);
            fcx.builder.call(view.name, arg_tirs, &modes, ret_ty, span)
        }
        "str_push" => {
            // str_push(s: inout str, suffix: strview) -> void. Builtins
            // bypass `check_call`, so the `&`/`inout` agreement +
            // mutable-lvalue checks are replayed here against arg 0.
            // M8.4: the suffix is a view — an owned `str` passes as
            // today (codegen reads its ptr+len), a slice passes
            // directly. No `ToView` wrap here: builtins bypass
            // check_call's §3.4 conversion, and wrapping an owned
            // `str` would leave an inst codegen can't lower yet
            // (Task 7) in previously-valid programs.
            if view.args.len() != 2 {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ArityMismatch,
                    format!(
                        "str_push() takes exactly 2 arguments, got {}",
                        view.args.len()
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let a0 = view.args[0];
            let t0 = fcx.builder.ty_of(arg_tirs[0]);
            let t1 = fcx.builder.ty_of(arg_tirs[1]);
            if !matches!(sema.pool.kind(t0), TypeKind::Str)
                || !matches!(sema.pool.kind(t1), TypeKind::Str | TypeKind::View(_))
            {
                sema.sink.emit(Diag::error(
                    sema.uir.span(a0),
                    DiagCode::TypeMismatch,
                    "str_push(s: inout str, suffix: strview): first argument must be str, suffix must be str or strview".to_string(),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            // arg 0 must be `&<mut str>`: a Borrow whose target is an
            // assignable lvalue (Task 6's helper).
            let inner = match sema.uir.inst(a0).data {
                InstData::Borrow(i) => i,
                _ => {
                    sema.sink.emit(Diag::error(
                        sema.uir.span(a0),
                        DiagCode::BorrowMismatch,
                        "str_push's first argument is `inout str` and requires `&`".to_string(),
                    ));
                    return fcx.builder.unreachable(sema.pool.error_type(), span);
                }
            };
            if let Some(reason) = borrow_target_reason(sema, scope, inner) {
                sema.sink.emit(Diag::error(
                    sema.uir.span(a0),
                    DiagCode::BorrowMismatch,
                    format!("cannot borrow this expression as mutable: {}", reason),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            // W0003 case A: the suffix parameter is `strview` —
            // materializing a slice to feed it is a redundant copy.
            warn_redundant_materialize_builtin_arg(
                sema,
                fcx,
                view.args[1],
                arg_tirs[1],
                "str_push",
            );
            let modes = vec![ParamMode::Inout, ParamMode::Borrow];
            let ret_ty = builtin.return_type(sema.pool);
            fcx.builder.call(view.name, arg_tirs, &modes, ret_ty, span)
        }
        "bytes_push" => {
            // bytes_push(b: inout bytes, x: int) -> void (M8.4.2
            // stopgap: the byte is an `int` range-checked 0-255 at
            // runtime; becomes `u8` at M17.1). Mirrors the str_push
            // arm's inout/lvalue machinery; appends a SINGLE byte.
            if view.args.len() != 2 {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::ArityMismatch,
                    format!(
                        "bytes_push() takes exactly 2 arguments, got {}",
                        view.args.len()
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let a0 = view.args[0];
            let t0 = fcx.builder.ty_of(arg_tirs[0]);
            let t1 = fcx.builder.ty_of(arg_tirs[1]);
            if !matches!(sema.pool.kind(t0), TypeKind::Bytes)
                || !matches!(sema.pool.kind(t1), TypeKind::Int)
            {
                sema.sink.emit(Diag::error(
                    sema.uir.span(a0),
                    DiagCode::TypeMismatch,
                    "bytes_push(b: inout bytes, x: int): first argument must be bytes, second must be int".to_string(),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            // arg 0 must be `&<mut bytes>`: a Borrow whose target is an
            // assignable lvalue.
            let inner = match sema.uir.inst(a0).data {
                InstData::Borrow(i) => i,
                _ => {
                    sema.sink.emit(Diag::error(
                        sema.uir.span(a0),
                        DiagCode::BorrowMismatch,
                        "bytes_push's first argument is `inout bytes` and requires `&`".to_string(),
                    ));
                    return fcx.builder.unreachable(sema.pool.error_type(), span);
                }
            };
            if let Some(reason) = borrow_target_reason(sema, scope, inner) {
                sema.sink.emit(Diag::error(
                    sema.uir.span(a0),
                    DiagCode::BorrowMismatch,
                    format!("cannot borrow this expression as mutable: {}", reason),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            let modes = vec![ParamMode::Inout, ParamMode::Borrow];
            let ret_ty = builtin.return_type(sema.pool);
            fcx.builder.call(view.name, arg_tirs, &modes, ret_ty, span)
        }
        _ => {
            let ret_ty = builtin.return_type(sema.pool);
            fcx.builder.call(view.name, arg_tirs, &modes, ret_ty, span)
        }
    }
}

/// W0003 case A (M8.4.1.2): is this UIR argument syntactically a
/// `str(view)` materialize call? Mirrors the intercept condition in
/// `check_call` — a call-form `str` with no user declaration carrying
/// the name (a user-defined `fn str` shadows the intercept, so its
/// calls never warn). Callers pair this with a `ty_of == str` check on
/// the argument's TIR so error paths stay warning-free.
pub(crate) fn is_str_materialize_arg(sema: &Sema<'_>, arg_uir: InstRef) -> bool {
    if sema.uir.inst(arg_uir).tag != InstTag::Call {
        return false;
    }
    let name = sema.uir.call_view(arg_uir).name;
    sema.pool.str(name) == "str" && !sema.name_to_decl.contains_key(&name)
}

/// W0003 case A for view-accepting builtins (M8.4.1.2): `print` and
/// str_push's suffix take `strview` arguments directly, so a
/// `str(view)` materialize call in that position is a redundant
/// allocation.
pub(crate) fn warn_redundant_materialize_builtin_arg(
    sema: &mut Sema<'_>,
    fcx: &FuncCtx,
    arg_uir: InstRef,
    arg_tir: TirRef,
    builtin: &str,
) {
    if fcx.builder.ty_of(arg_tir) == sema.pool.str_() && is_str_materialize_arg(sema, arg_uir) {
        sema.sink.emit(Diag::warning(
            sema.uir.span(arg_uir),
            DiagCode::RedundantMaterialize,
            format!(
                "redundant `str(...)` — `{builtin}` accepts `strview` arguments directly, with no allocation (drop the `str(...)` call)"
            ),
        ));
    }
}

/// M8.4.1.2 `str(view)` materialization: validate the single `strview`
/// argument and emit the str-returning call to the synthesized
/// `__ryo_str_from_view` runtime callee (mirrors the `int_to_str` arm of
/// `emit_builtin_call`). The ownership pass seeds the str-returning Call
/// as a fresh owner by construction; codegen lowers it via the same sret
/// stack-slot pattern as `int_to_str`.
pub(crate) fn emit_str_materialize(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    view: &CallView,
    arg_tirs: &[TirRef],
    span: Span,
) -> TirRef {
    if view.args.len() != 1 {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!("str() takes exactly 1 argument, got {}", view.args.len()),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    // Builtins never take `inout`: `&` is rejected exactly like the
    // table builtins (mirrors `int_to_str(&c)`).
    if matches!(sema.uir.inst(view.args[0]).tag, InstTag::Borrow) {
        sema.sink.emit(
            Diag::error(
                sema.uir.span(view.args[0]),
                DiagCode::BorrowMismatch,
                "argument 1 is passed by `&` but parameter is not `inout`".to_string(),
            )
            .with_help("remove the `&`, or declare the parameter `inout`"),
        );
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    let arg_ty = fcx.builder.ty_of(arg_tirs[0]);
    if sema.pool.is_error(arg_ty) {
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if !sema.pool.is_view(arg_ty) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.args[0]),
            DiagCode::TypeMismatch,
            format!(
                "str() argument must be strview, got {}",
                sema.pool.display(arg_ty)
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    // The synthesized callee name is unshadowable — user code cannot
    // declare `__ryo_`-prefixed identifiers (ReservedIdentifier) — so
    // codegen's name-match on the callee is unambiguous, and a
    // user-defined `fn str` (which wins in `check_call`) still lowers
    // as an ordinary user call.
    let callee = sema.pool.intern_str("__ryo_str_from_view");
    fcx.builder.call(
        callee,
        arg_tirs,
        &[ParamMode::Borrow],
        sema.pool.str_(),
        span,
    )
}

pub(crate) fn emit_panic(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    view: &CallView,
    span: Span,
) -> TirRef {
    if view.args.len() != 1 {
        // panic takes exactly one message argument (spec §7.6)
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!("panic() takes exactly 1 argument, got {}", view.args.len()),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    if !matches!(sema.uir.inst(view.args[0]).tag, InstTag::StrLiteral) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.args[0]),
            DiagCode::BuiltinArgKind,
            "panic() argument must be a string literal",
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }
    let user_msg_id = match sema.uir.inst(view.args[0]).data {
        InstData::Str(id) => id,
        _ => unreachable!("StrLiteral tag implies Str data"),
    };
    build_panic_call(sema, fcx, user_msg_id, "panicked", span)
}

pub(crate) fn emit_assert(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    view: &CallView,
    arg_tirs: &[TirRef],
    span: Span,
) -> TirRef {
    if view.args.len() != 2 {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!(
                "assert() takes exactly 2 arguments (condition, message), got {}",
                view.args.len()
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let cond_ty = fcx.builder.ty_of(arg_tirs[0]);
    if !sema.pool.compatible(cond_ty, sema.pool.bool_()) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.args[0]),
            DiagCode::TypeMismatch,
            format!(
                "assert() condition must be 'bool', got '{}'",
                sema.pool.display(cond_ty),
            ),
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    if !matches!(sema.uir.inst(view.args[1]).tag, InstTag::StrLiteral) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.args[1]),
            DiagCode::BuiltinArgKind,
            "assert() message must be a string literal",
        ));
        return fcx.builder.unreachable(sema.pool.error_type(), span);
    }

    let user_msg_id = match sema.uir.inst(view.args[1]).data {
        InstData::Str(id) => id,
        _ => unreachable!("StrLiteral tag implies Str data"),
    };

    let neg_cond = fcx
        .builder
        .unary(TirTag::BoolNot, sema.pool.bool_(), arg_tirs[0], span);

    let panic_call = build_panic_call(sema, fcx, user_msg_id, "assertion failed", span);
    let panic_stmt = fcx
        .builder
        .unary(TirTag::ExprStmt, sema.pool.void(), panic_call, span);

    fcx.builder
        .if_stmt(neg_cond, &[panic_stmt], &[], None, sema.pool.void(), span)
}

pub(crate) fn build_panic_call(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    user_msg_id: StringId,
    prefix: &str,
    span: Span,
) -> TirRef {
    let user_msg = sema.pool.str(user_msg_id).to_string();
    let (line, col) = byte_offset_to_line_col(sema.source, span.start);
    let func_name = sema.pool.str(fcx.builder.name());
    let formatted = format!(
        "{} at {}:{}:{} in {}(): {}\n",
        prefix,
        sema.file_path.display(),
        line,
        col,
        func_name,
        user_msg
    );
    let msg_len = formatted.len() as i64;
    let formatted_id = sema.pool.intern_str(&formatted);

    let str_ref = fcx.builder.str_const(formatted_id, sema.pool.str_(), span);
    let len_ref = fcx.builder.int_const(msg_len, sema.pool.int(), span);
    let panic_name = sema.pool.intern_str("__ryo_panic");
    fcx.builder.call(
        panic_name,
        &[str_ref, len_ref],
        &[ParamMode::Borrow, ParamMode::Borrow],
        sema.pool.never(),
        span,
    )
}

pub(crate) fn check_print_args(
    sema: &mut Sema<'_>,
    fcx: &FuncCtx,
    view: &CallView,
    arg_tirs: &[TirRef],
    span: Span,
) -> bool {
    if view.args.len() != 1 {
        sema.sink.emit(Diag::error(
            span,
            DiagCode::ArityMismatch,
            format!("print() takes exactly 1 argument, got {}", view.args.len()),
        ));
        return false;
    }
    let arg_ty = fcx.builder.ty_of(arg_tirs[0]);
    if sema.pool.is_error(arg_ty) {
        return false;
    }
    if !matches!(
        sema.pool.kind(arg_ty),
        TypeKind::Str | TypeKind::Bytes | TypeKind::View(_)
    ) {
        sema.sink.emit(Diag::error(
            sema.uir.span(view.args[0]),
            DiagCode::TypeMismatch,
            format!(
                "print() argument must be str, strview, bytes, or bytesview, got {}",
                sema.pool.display(arg_ty)
            ),
        ));
        return false;
    }
    true
}

// Column counts unicode codepoints, not bytes — matches editor conventions.
pub(crate) fn byte_offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
