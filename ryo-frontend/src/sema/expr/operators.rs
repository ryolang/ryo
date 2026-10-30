//! Unary analysis and binary-operator emission. Each path validates the
//! operand kind for its operator family, emits the typed TIR instruction
//! or diagnostic, and returns the TirRef. Split out of `expr/mod.rs` to
//! stay under the file-length limit.

use super::{ConstInt, FuncCtx, Scope, Sema, analyze_expr, const_eval_int};
use crate::sema::errors;
use ryo_core::diag::{Diag, DiagCode};
use ryo_core::tir::{TirRef, TirTag};
use ryo_core::types::{TypeId, TypeKind, ViewKind};
use ryo_core::uir::{InstRef, InstTag, Span};

pub(super) fn analyze_unary_operator(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    scope: &Scope,
    tag: InstTag,
    r: InstRef,
    operand: InstRef,
    span: Span,
) -> TirRef {
    let sub = analyze_expr(sema, fcx, scope, operand);
    let sub_ty = fcx.builder.ty_of(sub);
    if let Some(recovery) = errors::reject_direct_use(
        sema,
        fcx,
        sub_ty,
        span,
        DiagCode::UnsupportedOperator,
        "use an operator on",
    ) {
        return recovery;
    }
    match tag {
        InstTag::Neg => match sema.pool.kind(sub_ty) {
            TypeKind::Int => {
                if matches!(const_eval_int(sema.uir, r), ConstInt::Overflow) {
                    sema.sink.emit(Diag::error(
                        span,
                        DiagCode::ConstEvalFailure,
                        "integer overflow in constant expression".to_string(),
                    ));
                    fcx.builder.unreachable(sema.pool.error_type(), span)
                } else {
                    fcx.builder.unary(TirTag::INeg, sema.pool.int(), sub, span)
                }
            }
            TypeKind::Float => fcx
                .builder
                .unary(TirTag::FNeg, sema.pool.float(), sub, span),
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            _ => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "unary operator '-' not supported for type '{}'",
                        sema.pool.display(sub_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        },
        InstTag::Not => match sema.pool.kind(sub_ty) {
            TypeKind::Bool => fcx
                .builder
                .unary(TirTag::BoolNot, sema.pool.bool_(), sub, span),
            TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
            _ => {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "logical operator 'not' requires 'bool' operand, got '{}'",
                        sema.pool.display(sub_ty),
                    ),
                ));
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        },
        _ => unreachable!("analyze_unary_operator received {tag:?}"),
    }
}

pub(super) fn emit_logical_operator(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    tag: InstTag,
    kind_ty: TypeId,
    lhs: TirRef,
    rhs: TirRef,
    span: Span,
) -> TirRef {
    let kind = sema.pool.kind(kind_ty);
    match kind {
        TypeKind::Bool => {
            let tir_tag = match tag {
                InstTag::And => TirTag::BoolAnd,
                InstTag::Or => TirTag::BoolOr,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
        }
        TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
        _ => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UnsupportedOperator,
                format!(
                    "logical operator '{}' requires 'bool' operands, got '{}'",
                    bin_op_symbol(tag),
                    sema.pool.display(kind_ty),
                ),
            ));
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
    }
}

pub(super) fn emit_equality_operator(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    tag: InstTag,
    kind_ty: TypeId,
    lhs: TirRef,
    rhs: TirRef,
    span: Span,
) -> TirRef {
    let kind = sema.pool.kind(kind_ty);
    match kind {
        TypeKind::Int | TypeKind::Bool => {
            let tir_tag = match tag {
                InstTag::Eq => TirTag::ICmpEq,
                InstTag::NotEq => TirTag::ICmpNe,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
        }
        TypeKind::Float => {
            let tir_tag = match tag {
                InstTag::Eq => TirTag::FCmpEq,
                InstTag::NotEq => TirTag::FCmpNe,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
        }
        TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
        TypeKind::Str => {
            let tir_tag = match tag {
                InstTag::Eq => TirTag::StrCmpEq,
                InstTag::NotEq => TirTag::StrCmpNe,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
        }
        TypeKind::Bytes => {
            let tir_tag = match tag {
                InstTag::Eq => TirTag::BytesCmpEq,
                InstTag::NotEq => TirTag::BytesCmpNe,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
        }
        // M8.4 §3.3: view equality compares viewed contents. Same
        // `StrCmpEq`/`StrCmpNe` (or M8.4.2 `BytesCmpEq`/`BytesCmpNe`)
        // tags — operands are `{ptr, len}` pairs instead of full fat
        // pointers (codegen Task 13).
        TypeKind::View(ViewKind::Str) => {
            let tir_tag = match tag {
                InstTag::Eq => TirTag::StrCmpEq,
                InstTag::NotEq => TirTag::StrCmpNe,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
        }
        TypeKind::View(ViewKind::Bytes) => {
            let tir_tag = match tag {
                InstTag::Eq => TirTag::BytesCmpEq,
                InstTag::NotEq => TirTag::BytesCmpNe,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
        }
        // M9.1: memberwise struct equality is opt-in via
        // `#[derive(Eq)]` (sema gate; codegen lands separately).
        // `is_eq` is only meaningful on an error-free program —
        // a rejected derive still leaves the flag set, but the
        // driver short-circuits before codegen on any error.
        TypeKind::Struct => {
            if sema.pool.struct_view(kind_ty).is_eq() {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::StructEq,
                    InstTag::NotEq => TirTag::StructNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            } else {
                let name = sema.pool.display(kind_ty).to_string();
                sema.sink.emit(
                    Diag::error(
                        span,
                        DiagCode::EqDeriveRequired,
                        format!(
                            "binary operator `{}` requires `{}` to be `Eq`",
                            bin_op_symbol(tag),
                            name,
                        ),
                    )
                    .with_help(format!("add `#[derive(Eq)]` to `{name}`")),
                );
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
        // M10: anonymous struct equality is structural — a shape
        // is Eq-capable exactly when every field is
        // (`is_eq_capable`, computed recursively; there is no
        // stored flag and no opt-in attribute). Same memberwise
        // lowering as named structs: the shared StructEq/StructNe
        // codegen path reads the fields through `struct_view`,
        // which anon structs populate too. One diagnostic per
        // offending field, mirroring M9.1's derive validation.
        TypeKind::AnonStruct => {
            let view = sema.pool.struct_view(kind_ty);
            let mut capable = true;
            for f in &view.fields {
                if sema.pool.is_eq_capable(f.ty) {
                    continue;
                }
                capable = false;
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::AnonFieldNotEq,
                    format!(
                        "binary operator `{}` requires field '{}' of type '{}' to be Eq-capable",
                        bin_op_symbol(tag),
                        sema.pool.str(f.name),
                        sema.pool.display(f.ty),
                    ),
                ));
            }
            if capable {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::StructEq,
                    InstTag::NotEq => TirTag::StructNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            } else {
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
        // M11: enum equality is opt-in via `#[derive(Eq)]`, exactly
        // like M9.1 structs — the same EqDeriveRequired gate and
        // fix-it note. The derive flag is sufficient: astgen's
        // DeriveFieldNotEq check already rejected any payload field
        // that is not Eq-capable (per variant) before the enum was
        // defined, so a defined enum with the flag compares safely.
        // Codegen lowers EnumEq/EnumNe as discriminant compare plus
        // a tag-switch field-wise payload compare.
        TypeKind::Enum => {
            if sema.pool.enum_view(kind_ty).is_eq() {
                let tir_tag = match tag {
                    InstTag::Eq => TirTag::EnumEq,
                    InstTag::NotEq => TirTag::EnumNe,
                    _ => unreachable!(),
                };
                fcx.builder
                    .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
            } else {
                let name = sema.pool.display(kind_ty).to_string();
                sema.sink.emit(
                    Diag::error(
                        span,
                        DiagCode::EqDeriveRequired,
                        format!(
                            "binary operator `{}` requires `{}` to be `Eq`",
                            bin_op_symbol(tag),
                            name,
                        ),
                    )
                    .with_help(format!("add `#[derive(Eq)]` to `{name}`")),
                );
                fcx.builder.unreachable(sema.pool.error_type(), span)
            }
        }
        TypeKind::Void
        | TypeKind::Never
        | TypeKind::View(_)
        | TypeKind::DeclaredError
        | TypeKind::ErrorSet
        | TypeKind::ErrorUnion
        | TypeKind::InferredErrorUnion => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UnsupportedOperator,
                format!(
                    "equality operator '{}' not supported for type '{}'",
                    bin_op_symbol(tag),
                    sema.pool.display(kind_ty),
                ),
            ));
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
    }
}

pub(super) fn emit_ordering_operator(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    tag: InstTag,
    kind_ty: TypeId,
    lhs: TirRef,
    rhs: TirRef,
    span: Span,
) -> TirRef {
    let kind = sema.pool.kind(kind_ty);
    match kind {
        TypeKind::Int => {
            let tir_tag = match tag {
                InstTag::Lt => TirTag::ICmpLt,
                InstTag::LtEq => TirTag::ICmpLe,
                InstTag::Gt => TirTag::ICmpGt,
                InstTag::GtEq => TirTag::ICmpGe,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
        }
        TypeKind::Float => {
            let tir_tag = match tag {
                InstTag::Lt => TirTag::FCmpLt,
                InstTag::LtEq => TirTag::FCmpLe,
                InstTag::Gt => TirTag::FCmpGt,
                InstTag::GtEq => TirTag::FCmpGe,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.bool_(), lhs, rhs, span)
        }
        TypeKind::Str => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UnsupportedOperator,
                format!(
                    "ordering operator '{}' not supported for type 'str' (yet)",
                    bin_op_symbol(tag),
                ),
            ));
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
        TypeKind::Bool
        | TypeKind::Void
        | TypeKind::Never
        | TypeKind::AnonStruct
        | TypeKind::Struct
        | TypeKind::Bytes
        | TypeKind::Enum
        | TypeKind::DeclaredError
        | TypeKind::ErrorSet
        | TypeKind::ErrorUnion
        | TypeKind::InferredErrorUnion
        | TypeKind::View(_) => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UnsupportedOperator,
                format!(
                    "ordering operator '{}' not supported for type '{}'",
                    bin_op_symbol(tag),
                    sema.pool.display(kind_ty),
                ),
            ));
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
        TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
    }
}

pub(super) fn emit_modulo_operator(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    tag: InstTag,
    kind_ty: TypeId,
    lhs: TirRef,
    rhs: TirRef,
    span: Span,
) -> TirRef {
    let kind = sema.pool.kind(kind_ty);
    match kind {
        TypeKind::Int => fcx
            .builder
            .binary(TirTag::IMod, sema.pool.int(), lhs, rhs, span),
        TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
        _ => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UnsupportedOperator,
                format!(
                    "modulo operator '{}' not supported for type '{}'",
                    bin_op_symbol(tag),
                    sema.pool.display(kind_ty),
                ),
            ));
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
    }
}

pub(super) fn emit_arithmetic_operator(
    sema: &mut Sema<'_>,
    fcx: &mut FuncCtx,
    tag: InstTag,
    kind_ty: TypeId,
    lhs: TirRef,
    rhs: TirRef,
    span: Span,
) -> TirRef {
    let kind = sema.pool.kind(kind_ty);
    // Arithmetic: +, -, *, /
    match kind {
        TypeKind::Int => {
            let tir_tag = match tag {
                InstTag::Add => TirTag::IAdd,
                InstTag::Sub => TirTag::ISub,
                InstTag::Mul => TirTag::IMul,
                InstTag::Div => TirTag::ISDiv,
                _ => unreachable!(),
            };
            fcx.builder.binary(tir_tag, sema.pool.int(), lhs, rhs, span)
        }
        TypeKind::Float => {
            let tir_tag = match tag {
                InstTag::Add => TirTag::FAdd,
                InstTag::Sub => TirTag::FSub,
                InstTag::Mul => TirTag::FMul,
                InstTag::Div => TirTag::FDiv,
                _ => unreachable!(),
            };
            fcx.builder
                .binary(tir_tag, sema.pool.float(), lhs, rhs, span)
        }
        TypeKind::Str => {
            if tag != InstTag::Add {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "arithmetic operator '{}' not supported for type 'str'",
                        bin_op_symbol(tag),
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            fcx.builder
                .binary(TirTag::StrConcat, sema.pool.str_(), lhs, rhs, span)
        }
        TypeKind::Bytes => {
            if tag != InstTag::Add {
                sema.sink.emit(Diag::error(
                    span,
                    DiagCode::UnsupportedOperator,
                    format!(
                        "arithmetic operator '{}' not supported for type 'bytes'",
                        bin_op_symbol(tag),
                    ),
                ));
                return fcx.builder.unreachable(sema.pool.error_type(), span);
            }
            fcx.builder
                .binary(TirTag::BytesConcat, sema.pool.bytes(), lhs, rhs, span)
        }
        TypeKind::Error => fcx.builder.unreachable(sema.pool.error_type(), span),
        _ => {
            sema.sink.emit(Diag::error(
                span,
                DiagCode::UnsupportedOperator,
                format!(
                    "arithmetic operator '{}' not supported for type '{}'",
                    bin_op_symbol(tag),
                    sema.pool.display(kind_ty),
                ),
            ));
            fcx.builder.unreachable(sema.pool.error_type(), span)
        }
    }
}

pub(super) fn bin_op_symbol(tag: InstTag) -> &'static str {
    match tag {
        InstTag::Add => "+",
        InstTag::Sub => "-",
        InstTag::Mul => "*",
        InstTag::Div => "/",
        InstTag::Mod => "%",
        InstTag::Eq => "==",
        InstTag::NotEq => "!=",
        InstTag::Lt => "<",
        InstTag::Gt => ">",
        InstTag::LtEq => "<=",
        InstTag::GtEq => ">=",
        InstTag::And => "and",
        InstTag::Or => "or",
        _ => "?",
    }
}
