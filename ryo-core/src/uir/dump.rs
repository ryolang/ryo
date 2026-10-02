//! UIR pretty-printer — split from `uir.rs` to keep that file under
//! the 2000-line CI gate (`scripts/check_file_length.sh`); the listing
//! format is documented there.

use super::{InstData, InstRef, InstTag, InternPool, StringId, Uir};
use std::fmt;
/// Renderable wrapper for `Uir::dump`, modelled on Zig's
/// `Zir.dumpHir` listing format.
pub struct UirDump<'a> {
    pub uir: &'a Uir,
    pub pool: &'a InternPool,
}

impl Uir {
    /// Render a Zig-style listing: `%N = <op> <operands>` per line,
    /// grouped per function. Used by the (forthcoming) `ryo ir
    /// --emit=uir` command and by tests.
    pub fn dump<'a>(&'a self, pool: &'a InternPool) -> UirDump<'a> {
        UirDump { uir: self, pool }
    }
}

impl<'a> fmt::Display for UirDump<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let uir = self.uir;
        let pool = self.pool;

        // Section 0: struct declarations (M9 side table).
        for decl in &uir.struct_decls {
            write!(f, "struct {}:", pool.str(decl.name))?;
            for field in &decl.fields {
                write!(f, " {}: {}", pool.str(field.name), pool.display(field.ty))?;
            }
            writeln!(f)?;
        }

        // Section 1: per-function signature and the ordered list of
        // body-statement refs, so a reader can see what each function
        // actually executes.
        for body in &uir.func_bodies {
            write!(f, "fn {}(", pool.str(body.name))?;
            for (i, p) in body.params.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}: {}", pool.str(p.name), pool.display(p.ty))?;
            }
            writeln!(f, ") -> {}", pool.display(body.return_type))?;

            write!(f, "  body:")?;
            for r in uir.body_stmts(body) {
                write!(f, " %{}", r.index())?;
            }
            writeln!(f)?;
        }

        // Section 2: every instruction in index order, Zig-ZIR-style.
        // Slot 0 is the reserved sentinel (see `Uir::new`); skip it.
        if uir.instructions.len() > 1 {
            writeln!(f, "\ninstructions:")?;
            for idx in 1..uir.instructions.len() {
                let r = InstRef::from_index(idx);
                write_inst(f, uir, pool, r, 0)?;
            }
        }
        Ok(())
    }
}

fn write_inst(
    f: &mut fmt::Formatter<'_>,
    uir: &Uir,
    pool: &InternPool,
    r: InstRef,
    depth: usize,
) -> fmt::Result {
    // Print the instruction itself; sub-expressions are referenced by
    // `%idx` rather than recursively expanded — this is the whole
    // point of a flat IR. The depth parameter is reserved for future
    // block / control-flow nesting.
    let _ = depth;
    let inst = uir.inst(r);
    write!(f, "  %{} = ", r.index())?;
    match (inst.tag, inst.data) {
        (InstTag::IntLiteral, InstData::Int(v)) => writeln!(f, "int {}", v),
        (InstTag::FloatLiteral, InstData::Float(v)) => writeln!(f, "float {}", v),
        (InstTag::StrLiteral, InstData::Str(s)) => writeln!(f, "str {:?}", pool.str(s)),
        (InstTag::BytesLiteral, InstData::Str(s)) => {
            writeln!(f, "bytes \"{}\"", pool.bytes_payload(s).escape_ascii())
        }
        (InstTag::BoolLiteral, InstData::Bool(b)) => writeln!(f, "bool {}", b),
        (InstTag::Var, InstData::Var(s)) => writeln!(f, "var {}", pool.str(s)),
        (op, InstData::BinOp { lhs, rhs }) => {
            writeln!(f, "{} %{}, %{}", bin_op_name(op), lhs.index(), rhs.index())
        }
        (op, InstData::UnOp(operand)) => writeln!(f, "{} %{}", un_op_name(op), operand.index()),
        (InstTag::ReturnVoid, InstData::None) => writeln!(f, "ret_void"),
        (InstTag::Call, InstData::Extra(_)) => {
            let view = uir.call_view(r);
            write!(f, "call {}(", pool.str(view.name))?;
            for (i, a) in view.args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "%{}", a.index())?;
            }
            writeln!(f, ")")
        }
        (InstTag::VarDecl, InstData::Extra(_)) => {
            let view = uir.var_decl_view(r);
            let kw = if view.mutable { "mut " } else { "" };
            match view.ty {
                Some(t) => writeln!(
                    f,
                    "var_decl {}{}: {} = %{}",
                    kw,
                    pool.str(view.name),
                    pool.display(t),
                    view.initializer.index()
                ),
                None => writeln!(
                    f,
                    "var_decl {}{} = %{}",
                    kw,
                    pool.str(view.name),
                    view.initializer.index()
                ),
            }
        }
        (InstTag::IfStmt, InstData::Extra(_)) => {
            let view = uir.if_stmt_view(r);
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
        (InstTag::AssignOrDecl, InstData::Extra(_)) => {
            let v = uir.assign_or_decl_view(r);
            writeln!(
                f,
                "assign_or_decl {} = %{}",
                pool.str(v.name),
                v.value.index()
            )
        }
        (InstTag::CompoundAssign, InstData::Extra(_)) => {
            let v = uir.compound_assign_view(r);
            writeln!(
                f,
                "compound_assign {} {} %{}",
                pool.str(v.name),
                v.op,
                v.value.index()
            )
        }
        (InstTag::FieldAssign, InstData::Extra(_)) => {
            let v = uir.field_assign_view(r);
            writeln!(
                f,
                "field_assign %{} = %{}",
                v.target.index(),
                v.value.index()
            )
        }
        (InstTag::CompoundFieldAssign, InstData::Extra(_)) => {
            let v = uir.compound_field_assign_view(r);
            writeln!(
                f,
                "compound_field_assign %{} {} %{}",
                v.target.index(),
                v.op,
                v.value.index()
            )
        }
        (InstTag::WhileLoop, InstData::Extra(_)) => {
            let v = uir.while_loop_view(r);
            let body_refs: Vec<_> = v.body.iter().map(|b| format!("%{}", b.index())).collect();
            writeln!(
                f,
                "while_loop cond=%{} body=[{}]",
                v.cond.index(),
                body_refs.join(", ")
            )
        }
        (InstTag::ForRange, InstData::Extra(_)) => {
            let v = uir.for_range_view(r);
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
        (InstTag::MethodCall, InstData::Extra(_)) => {
            let view = uir.method_call_view(r);
            write!(
                f,
                "method_call %{}.{}(",
                view.receiver.index(),
                pool.str(view.name)
            )?;
            for (i, a) in view.args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "%{}", a.index())?;
            }
            writeln!(f, ")")
        }
        (InstTag::Break, InstData::None) => writeln!(f, "break"),
        (InstTag::Continue, InstData::None) => writeln!(f, "continue"),
        (InstTag::StructLit, InstData::Extra(_)) => {
            let view = uir.struct_lit_view(r);
            match view.name {
                Some(name) => write!(f, "struct_lit {} {{", pool.str(name))?,
                None => write!(f, "struct_lit <anon> {{")?,
            }
            for (i, (fname, v)) in view.fields.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}: %{}", pool.str(*fname), v.index())?;
            }
            writeln!(f, "}}")
        }
        (InstTag::FieldAccess, InstData::FieldAccess { object, field }) => {
            writeln!(f, "field_access %{}.{}", object.index(), pool.str(field))
        }
        (InstTag::Destructure, InstData::Destructure { value, .. }) => {
            let view = uir.destructure_view(r);
            write!(
                f,
                "destructure %{} {} [",
                value.index(),
                if view.by_position {
                    "positional"
                } else {
                    "named"
                }
            )?;
            for (i, (selector, bind)) in view.plan.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                if view.by_position {
                    write!(f, "{}", selector)?;
                } else {
                    write!(f, "{}", pool.str(StringId::from_raw(*selector)))?;
                }
                match bind {
                    Some(name) => write!(f, ": {}", pool.str(*name))?,
                    None => write!(f, ": _")?,
                }
            }
            writeln!(f, "]")
        }
        (InstTag::Borrow, InstData::Borrow(inner)) => {
            writeln!(f, "borrow %{}", inner.index())
        }
        (InstTag::Slice, InstData::Slice { base, start, end }) => {
            let bound = |b: Option<InstRef>| match b {
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
        (tag, data) => writeln!(f, "<malformed: {:?} / {:?}>", tag, data),
    }
}

fn bin_op_name(t: InstTag) -> &'static str {
    match t {
        InstTag::Add => "add",
        InstTag::Sub => "sub",
        InstTag::Mul => "mul",
        InstTag::Div => "div",
        InstTag::Mod => "mod",
        InstTag::Eq => "icmp_eq",
        InstTag::NotEq => "icmp_ne",
        InstTag::Lt => "icmp_lt",
        InstTag::Gt => "icmp_gt",
        InstTag::LtEq => "icmp_le",
        InstTag::GtEq => "icmp_ge",
        InstTag::And => "bool_and",
        InstTag::Or => "bool_or",
        InstTag::Index => "index",
        _ => "?bin",
    }
}

fn un_op_name(t: InstTag) -> &'static str {
    match t {
        InstTag::Neg => "neg",
        InstTag::Not => "bool_not",
        InstTag::Return => "ret",
        InstTag::ExprStmt => "expr_stmt",
        _ => "?un",
    }
}
