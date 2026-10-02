//! TIR pretty-printer — split from `tir.rs` to keep that file under
//! the 2000-line CI gate (`scripts/check_file_length.sh`); the listing
//! format is documented there.

use super::{InternPool, ParamMode, Tir, TirData, TirRef, TirTag};
use std::fmt;
/// Renderable wrapper for `Tir::dump`, modelled on Zig's
/// `Air.dumpAir` listing format. One section per function.
pub struct TirDump<'a> {
    pub tirs: &'a [Tir],
    pub pool: &'a InternPool,
}

pub fn dump<'a>(tirs: &'a [Tir], pool: &'a InternPool) -> TirDump<'a> {
    TirDump { tirs, pool }
}

impl<'a> fmt::Display for TirDump<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for tir in self.tirs {
            write!(f, "fn {}(", self.pool.str(tir.name))?;
            for (i, p) in tir.params.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                let prefix = match p.mode {
                    ParamMode::Move => "move ",
                    ParamMode::Inout => "inout ",
                    ParamMode::Borrow => "",
                };
                write!(f, "{prefix}")?;
                write!(f, "{}: {}", self.pool.str(p.name), self.pool.display(p.ty))?;
            }
            writeln!(f, ") -> {}", self.pool.display(tir.return_type))?;

            write!(f, "  body:")?;
            for r in tir.body_stmts() {
                write!(f, " %{}", r.index())?;
            }
            writeln!(f)?;

            // Skip slot 0 (reserved sentinel).
            for idx in 1..tir.instructions.len() {
                let r = TirRef::from_index(idx);
                write_inst(f, tir, self.pool, r)?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

fn write_inst(f: &mut fmt::Formatter<'_>, tir: &Tir, pool: &InternPool, r: TirRef) -> fmt::Result {
    let inst = tir.inst(r);
    write!(f, "  %{} : {} = ", r.index(), pool.display(inst.ty))?;
    match (inst.tag, inst.data) {
        (TirTag::IntConst, TirData::Int(v)) => writeln!(f, "iconst {}", v),
        (TirTag::FloatConst, TirData::Float(v)) => writeln!(f, "fconst {}", v),
        (TirTag::BoolConst, TirData::Bool(b)) => writeln!(f, "bconst {}", b),
        (TirTag::StrConst, TirData::Str(s)) => writeln!(f, "sconst {:?}", pool.str(s)),
        (TirTag::BytesConst, TirData::Str(s)) => {
            writeln!(
                f,
                "bytes_const \"{}\"",
                pool.bytes_payload(s).escape_ascii()
            )
        }
        (TirTag::Var, TirData::Var(s)) => writeln!(f, "var {}", pool.str(s)),
        (TirTag::Slice, TirData::Slice { base, start, end }) => {
            let bound = |b: Option<TirRef>| match b {
                Some(b) => format!("%{}", b.index()),
                None => "_".to_string(),
            };
            writeln!(
                f,
                "slice %{}, {}..{}",
                base.index(),
                bound(start),
                bound(end)
            )
        }
        (op, TirData::BinOp { lhs, rhs }) => {
            writeln!(f, "{} %{}, %{}", bin_op_name(op), lhs.index(), rhs.index())
        }
        (op, TirData::UnOp(operand)) => writeln!(f, "{} %{}", un_op_name(op), operand.index()),
        (TirTag::ReturnVoid, TirData::None) => writeln!(f, "ret_void"),
        (TirTag::Unreachable, TirData::None) => writeln!(f, "unreachable"),
        (TirTag::Call, TirData::Extra(_)) => {
            let view = tir.call_view(r);
            write!(f, "call {}(", pool.str(view.name))?;
            for (i, a) in view.args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "%{}", a.index())?;
            }
            writeln!(f, ")")
        }
        (TirTag::VarDecl, TirData::Extra(_)) => {
            let view = tir.var_decl_view(r);
            let kw = if view.mutable { "mut " } else { "" };
            writeln!(
                f,
                "var_decl {}{} = %{}",
                kw,
                pool.str(view.name),
                view.initializer.index()
            )
        }
        (TirTag::Assign, TirData::Extra(_)) => {
            let v = tir.assign_view(r);
            writeln!(f, "assign {} = %{}", pool.str(v.name), v.value.index())
        }
        (TirTag::CompoundAssign, TirData::Extra(_)) => {
            let v = tir.compound_assign_view(r);
            writeln!(
                f,
                "compound_assign {} {} %{}",
                pool.str(v.name),
                v.op,
                v.value.index()
            )
        }
        (TirTag::FieldAssign, TirData::Extra(_)) => {
            let v = tir.field_assign_view(r);
            writeln!(
                f,
                "field_assign %{} = %{}",
                v.target.index(),
                v.value.index()
            )
        }
        (TirTag::CompoundFieldAssign, TirData::Extra(_)) => {
            let v = tir.compound_field_assign_view(r);
            writeln!(
                f,
                "compound_field_assign %{} {} %{}",
                v.target.index(),
                v.op,
                v.value.index()
            )
        }
        (TirTag::IfStmt, TirData::Extra(_)) => {
            let view = tir.if_stmt_view(r);
            write!(f, "if_stmt cond=%{}", view.cond.index())?;
            write!(f, " then=[{}]", view.then_stmts.len())?;
            for elif in &view.elif_branches {
                write!(
                    f,
                    " elif(cond=%{}, body=[{}])",
                    elif.cond.index(),
                    elif.body.len()
                )?;
            }
            if let Some(else_s) = &view.else_stmts {
                write!(f, " else=[{}]", else_s.len())?;
            }
            writeln!(f)
        }
        (TirTag::WhileLoop, TirData::Extra(_)) => {
            let v = tir.while_loop_view(r);
            let body_refs: Vec<_> = v.body.iter().map(|b| format!("%{}", b.index())).collect();
            writeln!(
                f,
                "while_loop cond=%{} body=[{}]",
                v.cond.index(),
                body_refs.join(", ")
            )
        }
        (TirTag::ForRange, TirData::Extra(_)) => {
            let v = tir.for_range_view(r);
            let body_refs: Vec<_> = v.body.iter().map(|b| format!("%{}", b.index())).collect();
            writeln!(
                f,
                "for_range {} in range(%{}, %{}) body=[{}]",
                pool.str(v.var_name),
                v.start.index(),
                v.end.index(),
                body_refs.join(", ")
            )
        }
        (TirTag::Break, TirData::None) => writeln!(f, "break"),
        (TirTag::Continue, TirData::None) => writeln!(f, "continue"),
        (TirTag::StructLit, TirData::Extra(_)) => {
            let view = tir.struct_lit_view(r);
            write!(f, "struct_lit {} {{", pool.display(view.ty))?;
            for (i, (idx, v)) in view.fields.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{idx}: %{}", v.index())?;
            }
            writeln!(f, "}}")
        }
        (
            TirTag::FieldAccess,
            TirData::FieldAccess {
                object,
                field_index,
            },
        ) => writeln!(f, "field_access %{}.{}", object.index(), field_index),
        (TirTag::Destructure, TirData::Extra(_)) => {
            let view = tir.destructure_view(r);
            write!(f, "destructure %{} {{", view.rhs.index())?;
            for (i, field) in view.fields.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                match field.bind {
                    Some(name) => write!(
                        f,
                        "{}: {}: {}",
                        field.field_index,
                        pool.str(name),
                        pool.display(field.ty)
                    )?,
                    None => write!(f, "{}: _", field.field_index)?,
                }
            }
            writeln!(f, "}}")
        }
        (tag, data) => writeln!(f, "<malformed: {:?} / {:?}>", tag, data),
    }
}

fn bin_op_name(t: TirTag) -> &'static str {
    match t {
        TirTag::IAdd => "iadd",
        TirTag::ISub => "isub",
        TirTag::IMul => "imul",
        TirTag::ISDiv => "isdiv",
        TirTag::IMod => "imod",
        TirTag::ICmpEq => "icmp_eq",
        TirTag::ICmpNe => "icmp_ne",
        TirTag::ICmpLt => "icmp_lt",
        TirTag::ICmpLe => "icmp_le",
        TirTag::ICmpGt => "icmp_gt",
        TirTag::ICmpGe => "icmp_ge",
        TirTag::FAdd => "fadd",
        TirTag::FSub => "fsub",
        TirTag::FMul => "fmul",
        TirTag::FDiv => "fdiv",
        TirTag::FCmpEq => "fcmp_eq",
        TirTag::FCmpNe => "fcmp_ne",
        TirTag::FCmpLt => "fcmp_lt",
        TirTag::FCmpLe => "fcmp_le",
        TirTag::FCmpGt => "fcmp_gt",
        TirTag::FCmpGe => "fcmp_ge",
        TirTag::StrConcat => "str_concat",
        TirTag::StrCmpEq => "str_eq",
        TirTag::StrCmpNe => "str_ne",
        TirTag::BytesConcat => "bytes_concat",
        TirTag::BytesCmpEq => "bytes_eq",
        TirTag::BytesCmpNe => "bytes_ne",
        TirTag::BytesIndex => "bytes_index",
        TirTag::StructEq => "struct_eq",
        TirTag::StructNe => "struct_ne",
        TirTag::BoolAnd => "bool_and",
        TirTag::BoolOr => "bool_or",
        _ => "?bin",
    }
}

fn un_op_name(t: TirTag) -> &'static str {
    match t {
        TirTag::INeg => "ineg",
        TirTag::FNeg => "fneg",
        TirTag::BoolNot => "bool_not",
        TirTag::Return => "ret",
        TirTag::ExprStmt => "expr_stmt",
        TirTag::StrLen => "str_len",
        TirTag::ToView => "to_view",
        TirTag::ViewAsOwner => "view_as_owner",
        TirTag::DebugRepr => "debug_repr",
        _ => "?un",
    }
}
