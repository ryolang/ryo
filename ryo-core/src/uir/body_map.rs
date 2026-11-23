use super::{
    ExtraRange, InstData, InstRef, InstTag, TypeExprId, TypeExprList, TypeFieldList, Uir,
    UirTypeExprKind, match_extra, var_decl_extra,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodySlot {
    pub body: u32,
    pub local: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyMapError {
    CrossBody {
        inst: InstRef,
        first_body: u32,
        second_body: u32,
    },
    InvalidInstRef {
        body: u32,
        inst: InstRef,
    },
    ReservedInstRef {
        body: u32,
    },
    InvalidExtraRange {
        body: u32,
        inst: Option<InstRef>,
        offset: u32,
        len: u32,
    },
    MalformedWire {
        body: u32,
        inst: InstRef,
    },
    InvalidTypeExprRef {
        raw: u32,
    },
    InvalidTypeExprRange {
        id: super::TypeExprId,
        offset: u32,
        len: u32,
    },
    TypeExprCycle {
        id: super::TypeExprId,
    },
    TooManyBodies,
    TooManyInstructions {
        body: u32,
    },
}

impl std::fmt::Display for BodyMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CrossBody {
                inst,
                first_body,
                second_body,
            } => write!(
                f,
                "instruction {} is reachable from bodies {first_body} and {second_body}",
                inst.raw()
            ),
            Self::InvalidInstRef { body, inst } => write!(
                f,
                "body {body} references invalid instruction {}",
                inst.raw()
            ),
            Self::ReservedInstRef { body } => {
                write!(f, "body {body} references reserved instruction zero")
            }
            Self::InvalidExtraRange {
                body,
                inst,
                offset,
                len,
            } => match inst {
                Some(inst) => write!(
                    f,
                    "instruction {} in body {body} has invalid extra range {offset}..{}",
                    inst.raw(),
                    offset.saturating_add(*len)
                ),
                None => write!(
                    f,
                    "body {body} has invalid extra range {offset}..{}",
                    offset.saturating_add(*len)
                ),
            },
            Self::MalformedWire { body, inst } => write!(
                f,
                "instruction {} in body {body} has a malformed payload",
                inst.raw()
            ),
            Self::InvalidTypeExprRef { raw } => {
                write!(f, "invalid type-expression handle {raw}")
            }
            Self::InvalidTypeExprRange { id, offset, len } => write!(
                f,
                "type-expression {} has invalid child range {offset}..{}",
                id.raw(),
                offset.saturating_add(*len)
            ),
            Self::TypeExprCycle { id } => {
                write!(
                    f,
                    "type-expression {} contains a dependency cycle",
                    id.raw()
                )
            }
            Self::TooManyBodies => f.write_str("UIR body index exceeds u32"),
            Self::TooManyInstructions { body } => {
                write!(f, "instruction count for body {body} exceeds u32")
            }
        }
    }
}

impl std::error::Error for BodyMapError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UirBodyMap {
    slots: Vec<Option<BodySlot>>,
    bodies: Vec<Vec<InstRef>>,
}

impl UirBodyMap {
    pub fn slot(&self, inst: InstRef) -> Option<BodySlot> {
        self.slots.get(inst.index()).copied().flatten()
    }

    pub fn instructions(&self, body: usize) -> &[InstRef] {
        &self.bodies[body]
    }
}

impl Uir {
    pub fn compute_body_map(&self) -> Result<UirBodyMap, BodyMapError> {
        self.validate_type_exprs()?;
        enum Visit {
            Enter(InstRef),
            Exit(InstRef),
        }

        let mut map = UirBodyMap {
            slots: vec![None; self.instructions.len()],
            bodies: Vec::with_capacity(self.func_bodies.len()),
        };
        map.bodies.resize_with(self.func_bodies.len(), Vec::new);

        let mut active = vec![false; self.instructions.len()];
        let mut stack = Vec::new();
        let mut children = Vec::new();
        for body_index in 0..self.func_bodies.len() {
            let body = u32::try_from(body_index).map_err(|_| BodyMapError::TooManyBodies)?;
            let words = checked_extra(self, self.func_bodies[body_index].body, body, None)?;
            children.clear();
            for &raw in words {
                children.push(checked_ref(raw, body, self.instructions.len())?);
            }
            stack.extend(children.iter().rev().map(|&inst| Visit::Enter(inst)));

            while let Some(visit) = stack.pop() {
                let inst = match visit {
                    Visit::Exit(inst) => {
                        active[inst.index()] = false;
                        continue;
                    }
                    Visit::Enter(inst) => inst,
                };
                if active[inst.index()] {
                    return malformed(body, inst);
                }
                if let Some(slot) = map.slots[inst.index()] {
                    if slot.body != body {
                        return Err(BodyMapError::CrossBody {
                            inst,
                            first_body: slot.body,
                            second_body: body,
                        });
                    }
                    continue;
                }

                let local = u32::try_from(map.bodies[body_index].len())
                    .map_err(|_| BodyMapError::TooManyInstructions { body })?;
                map.slots[inst.index()] = Some(BodySlot { body, local });
                map.bodies[body_index].push(inst);
                active[inst.index()] = true;

                children.clear();
                decode_children(self, inst, body, &mut children)?;
                stack.push(Visit::Exit(inst));
                stack.extend(children.iter().rev().map(|&child| Visit::Enter(child)));
            }
        }
        Ok(map)
    }
}

fn checked_type_expr_ref(uir: &Uir, id: TypeExprId) -> Result<TypeExprId, BodyMapError> {
    if id.index() >= uir.type_exprs.len() {
        return Err(BodyMapError::InvalidTypeExprRef { raw: id.raw() });
    }
    Ok(id)
}

fn checked_type_expr_list(
    uir: &Uir,
    id: TypeExprId,
    list: TypeExprList,
) -> Result<&[TypeExprId], BodyMapError> {
    let start = list.offset as usize;
    let end = start.checked_add(list.len as usize);
    end.and_then(|end| uir.type_expr_lists.get(start..end))
        .ok_or(BodyMapError::InvalidTypeExprRange {
            id,
            offset: list.offset,
            len: list.len,
        })
}

fn checked_type_field_list(
    uir: &Uir,
    id: TypeExprId,
    list: TypeFieldList,
) -> Result<&[(super::StringId, TypeExprId)], BodyMapError> {
    let start = list.offset as usize;
    let end = start.checked_add(list.len as usize);
    end.and_then(|end| uir.type_field_lists.get(start..end))
        .ok_or(BodyMapError::InvalidTypeExprRange {
            id,
            offset: list.offset,
            len: list.len,
        })
}

impl Uir {
    fn validate_type_exprs(&self) -> Result<(), BodyMapError> {
        for body in &self.func_bodies {
            for param in &body.params {
                checked_type_expr_ref(self, param.type_expr)?;
            }
            if let Some(return_type) = body.return_type {
                checked_type_expr_ref(self, return_type)?;
            }
        }
        for decl in &self.struct_decls {
            for field in &decl.fields {
                checked_type_expr_ref(self, field.type_expr)?;
            }
        }
        for decl in &self.enum_decls {
            for variant in &decl.variants {
                for field in &variant.fields {
                    checked_type_expr_ref(self, field.type_expr)?;
                }
            }
        }
        for decl in &self.error_decls {
            for field in &decl.payload.fields {
                checked_type_expr_ref(self, field.type_expr)?;
            }
        }

        let mut states = vec![0u8; self.type_exprs.len()];
        let mut stack = Vec::new();
        let mut children = Vec::new();
        for index in 1..self.type_exprs.len() {
            if states[index] != 0 {
                continue;
            }
            let root = TypeExprId::from_raw(index as u32);
            stack.push((root, false));
            while let Some((id, exit)) = stack.pop() {
                if exit {
                    states[id.index()] = 2;
                    continue;
                }
                match states[id.index()] {
                    1 => return Err(BodyMapError::TypeExprCycle { id }),
                    2 => continue,
                    _ => {}
                }
                states[id.index()] = 1;
                children.clear();
                match self.type_exprs[id.index()].kind {
                    UirTypeExprKind::Name { .. } => {}
                    UirTypeExprKind::Anon { fields } => {
                        for &(_, child) in checked_type_field_list(self, id, fields)? {
                            children.push(checked_type_expr_ref(self, child)?);
                        }
                    }
                    UirTypeExprKind::Positional(elements) => {
                        for &child in checked_type_expr_list(self, id, elements)? {
                            children.push(checked_type_expr_ref(self, child)?);
                        }
                    }
                    UirTypeExprKind::ErrorUnion {
                        error_set, success, ..
                    } => {
                        for &child in checked_type_expr_list(self, id, error_set)? {
                            children.push(checked_type_expr_ref(self, child)?);
                        }
                        children.push(checked_type_expr_ref(self, success)?);
                    }
                }
                stack.push((id, true));
                stack.extend(children.iter().rev().map(|&child| (child, false)));
            }
        }
        Ok(())
    }
}

fn checked_ref(raw: u32, body: u32, instruction_count: usize) -> Result<InstRef, BodyMapError> {
    if raw == 0 {
        return Err(BodyMapError::ReservedInstRef { body });
    }
    let inst = InstRef::from_raw(raw);
    if inst.index() >= instruction_count {
        return Err(BodyMapError::InvalidInstRef { body, inst });
    }
    Ok(inst)
}

fn push_ref(
    children: &mut Vec<InstRef>,
    inst: InstRef,
    body: u32,
    instruction_count: usize,
) -> Result<(), BodyMapError> {
    children.push(checked_ref(inst.raw(), body, instruction_count)?);
    Ok(())
}

fn malformed<T>(body: u32, inst: InstRef) -> Result<T, BodyMapError> {
    Err(BodyMapError::MalformedWire { body, inst })
}

fn checked_extra(
    uir: &Uir,
    range: ExtraRange,
    body: u32,
    inst: Option<InstRef>,
) -> Result<&[u32], BodyMapError> {
    let start = range.offset as usize;
    let end = start.checked_add(range.len as usize);
    end.and_then(|end| uir.extra.get(start..end))
        .ok_or(BodyMapError::InvalidExtraRange {
            body,
            inst,
            offset: range.offset,
            len: range.len,
        })
}

fn expect_extra(data: InstData, body: u32, inst: InstRef) -> Result<ExtraRange, BodyMapError> {
    match data {
        InstData::Extra(range) => Ok(range),
        _ => malformed(body, inst),
    }
}

struct WireCursor<'a> {
    words: &'a [u32],
    pos: usize,
    body: u32,
    inst: InstRef,
    instruction_count: usize,
}

impl<'a> WireCursor<'a> {
    fn new(
        uir: &'a Uir,
        range: ExtraRange,
        body: u32,
        inst: InstRef,
    ) -> Result<Self, BodyMapError> {
        Ok(Self {
            words: checked_extra(uir, range, body, Some(inst))?,
            pos: 0,
            body,
            inst,
            instruction_count: uir.instructions.len(),
        })
    }

    fn word(&mut self) -> Result<u32, BodyMapError> {
        let word = self.words.get(self.pos).copied();
        if word.is_some() {
            self.pos += 1;
        }
        word.ok_or(BodyMapError::MalformedWire {
            body: self.body,
            inst: self.inst,
        })
    }

    fn count(&mut self) -> Result<usize, BodyMapError> {
        Ok(self.word()? as usize)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u32], BodyMapError> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or(BodyMapError::MalformedWire {
                body: self.body,
                inst: self.inst,
            })?;
        let words = self
            .words
            .get(self.pos..end)
            .ok_or(BodyMapError::MalformedWire {
                body: self.body,
                inst: self.inst,
            })?;
        self.pos = end;
        Ok(words)
    }

    fn skip(&mut self, count: usize) -> Result<(), BodyMapError> {
        self.take(count).map(|_| ())
    }

    fn refs(&mut self, count: usize, children: &mut Vec<InstRef>) -> Result<(), BodyMapError> {
        let body = self.body;
        let instruction_count = self.instruction_count;
        for &raw in self.take(count)? {
            children.push(checked_ref(raw, body, instruction_count)?);
        }
        Ok(())
    }

    fn ref_list(&mut self, children: &mut Vec<InstRef>) -> Result<(), BodyMapError> {
        let count = self.count()?;
        self.refs(count, children)
    }

    fn inst(&mut self, children: &mut Vec<InstRef>) -> Result<(), BodyMapError> {
        let raw = self.word()?;
        let inst = checked_ref(raw, self.body, self.instruction_count)?;
        children.push(inst);
        Ok(())
    }

    fn finish(&self) -> Result<(), BodyMapError> {
        if self.pos == self.words.len() {
            Ok(())
        } else {
            malformed(self.body, self.inst)
        }
    }
}

fn decode_children(
    uir: &Uir,
    inst: InstRef,
    body: u32,
    children: &mut Vec<InstRef>,
) -> Result<(), BodyMapError> {
    let data = uir.instructions[inst.index()].data;
    let instruction_count = uir.instructions.len();
    let tag = uir.instructions[inst.index()].tag;
    match tag {
        InstTag::IntLiteral
        | InstTag::FloatLiteral
        | InstTag::StrLiteral
        | InstTag::BytesLiteral
        | InstTag::BoolLiteral
        | InstTag::Var
        | InstTag::TempVar
        | InstTag::ReturnVoid
        | InstTag::Break
        | InstTag::Continue
        | InstTag::Unreachable => {
            let valid = match tag {
                InstTag::IntLiteral => matches!(data, InstData::Int(_)),
                InstTag::FloatLiteral => matches!(data, InstData::Float(_)),
                InstTag::StrLiteral | InstTag::BytesLiteral => {
                    matches!(data, InstData::Str(_))
                }
                InstTag::BoolLiteral => matches!(data, InstData::Bool(_)),
                InstTag::Var | InstTag::TempVar => matches!(data, InstData::Var(_)),
                _ => matches!(data, InstData::None),
            };
            if !valid {
                return malformed(body, inst);
            }
        }
        InstTag::Add
        | InstTag::Sub
        | InstTag::Mul
        | InstTag::Div
        | InstTag::Mod
        | InstTag::Eq
        | InstTag::NotEq
        | InstTag::Lt
        | InstTag::Gt
        | InstTag::LtEq
        | InstTag::GtEq
        | InstTag::And
        | InstTag::Or
        | InstTag::Index => {
            let InstData::BinOp { lhs, rhs } = data else {
                return malformed(body, inst);
            };
            push_ref(children, lhs, body, instruction_count)?;
            push_ref(children, rhs, body, instruction_count)?;
        }
        InstTag::Neg | InstTag::Return | InstTag::ExprStmt | InstTag::Not | InstTag::Try => {
            let InstData::UnOp(operand) = data else {
                return malformed(body, inst);
            };
            push_ref(children, operand, body, instruction_count)?;
        }
        InstTag::Borrow => {
            let InstData::Borrow(operand) = data else {
                return malformed(body, inst);
            };
            push_ref(children, operand, body, instruction_count)?;
        }
        InstTag::Call => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.skip(1)?;
            let argc = cursor.count()?;
            cursor.refs(argc, children)?;
            cursor.finish()?;
        }
        InstTag::VarDecl => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.skip(2)?;
            let type_expr = cursor.word()?;
            if type_expr != var_decl_extra::TYPE_EXPR_NONE_SENTINEL {
                checked_type_expr_ref(uir, TypeExprId::from_raw(type_expr))?;
            }
            cursor.inst(children)?;
            cursor.finish()?;
        }
        InstTag::IfStmt => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.inst(children)?;
            cursor.ref_list(children)?;
            let elif_count = cursor.count()?;
            for _ in 0..elif_count {
                cursor.inst(children)?;
                cursor.ref_list(children)?;
            }
            if cursor.word()? != 0 {
                cursor.ref_list(children)?;
            }
            cursor.finish()?;
        }
        InstTag::AssignOrDecl => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.skip(1)?;
            cursor.inst(children)?;
            cursor.finish()?;
        }
        InstTag::CompoundAssign => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.skip(2)?;
            cursor.inst(children)?;
            cursor.finish()?;
        }
        InstTag::WhileLoop => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.inst(children)?;
            cursor.ref_list(children)?;
            cursor.finish()?;
        }
        InstTag::ForRange => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.skip(1)?;
            cursor.inst(children)?;
            cursor.inst(children)?;
            cursor.ref_list(children)?;
            cursor.finish()?;
        }
        InstTag::MethodCall => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.inst(children)?;
            cursor.skip(1)?;
            let argc = cursor.count()?;
            cursor.refs(argc, children)?;
            cursor.finish()?;
        }
        InstTag::Slice => {
            let InstData::Slice { base, start, end } = data else {
                return malformed(body, inst);
            };
            push_ref(children, base, body, instruction_count)?;
            for bound in [start, end].into_iter().flatten() {
                push_ref(children, bound, body, instruction_count)?;
            }
        }
        InstTag::StructLit => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.skip(1)?;
            let field_count = cursor.count()?;
            for _ in 0..field_count {
                cursor.skip(1)?;
                cursor.inst(children)?;
            }
            cursor.finish()?;
        }
        InstTag::EnumLit => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.skip(2)?;
            decode_variant_args(&mut cursor, 8, children)?;
            cursor.finish()?;
        }
        InstTag::ErrorConstruct => {
            let InstData::ErrorConstruct { args, .. } = data else {
                return malformed(body, inst);
            };
            let mut cursor = WireCursor::new(uir, args, body, inst)?;
            cursor.skip(2)?;
            decode_variant_args(&mut cursor, 0, children)?;
            cursor.finish()?;
        }
        InstTag::FieldAccess => {
            let InstData::FieldAccess { object, .. } = data else {
                return malformed(body, inst);
            };
            push_ref(children, object, body, instruction_count)?;
        }
        InstTag::FieldAssign => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.inst(children)?;
            cursor.inst(children)?;
            cursor.finish()?;
        }
        InstTag::CompoundFieldAssign => {
            let range = expect_extra(data, body, inst)?;
            let mut cursor = WireCursor::new(uir, range, body, inst)?;
            cursor.inst(children)?;
            cursor.skip(1)?;
            cursor.inst(children)?;
            cursor.finish()?;
        }
        InstTag::Destructure => {
            let InstData::Destructure { value, plan, .. } = data else {
                return malformed(body, inst);
            };
            push_ref(children, value, body, instruction_count)?;
            let mut cursor = WireCursor::new(uir, plan, body, inst)?;
            let entry_count = cursor.count()?;
            let words = match entry_count.checked_mul(2) {
                Some(words) => words,
                None => return malformed(body, inst),
            };
            cursor.skip(words)?;
            cursor.finish()?;
        }
        InstTag::Match | InstTag::Catch => {
            let (scrutinee, arms, is_catch) = match (tag, data) {
                (InstTag::Match, InstData::Match { scrutinee, arms }) => (scrutinee, arms, false),
                (InstTag::Catch, InstData::Catch { scrutinee, arms }) => (scrutinee, arms, true),
                _ => return malformed(body, inst),
            };
            push_ref(children, scrutinee, body, instruction_count)?;
            let mut cursor = WireCursor::new(uir, arms, body, inst)?;
            if is_catch {
                cursor.skip(1)?;
            }
            decode_match_arms(&mut cursor, children)?;
            cursor.finish()?;
        }
    }
    Ok(())
}

fn decode_variant_args(
    cursor: &mut WireCursor<'_>,
    span_words: usize,
    children: &mut Vec<InstRef>,
) -> Result<(), BodyMapError> {
    cursor.skip(1)?;
    let argc = cursor.count()?;
    cursor.skip(span_words)?;
    for _ in 0..argc {
        cursor.skip(1)?;
        cursor.inst(children)?;
    }
    Ok(())
}

fn decode_match_arms(
    cursor: &mut WireCursor<'_>,
    children: &mut Vec<InstRef>,
) -> Result<(), BodyMapError> {
    let arm_count = cursor.count()?;
    for _ in 0..arm_count {
        let arm_len = cursor.count()?;
        let body = cursor.body;
        let inst = cursor.inst;
        let instruction_count = cursor.instruction_count;
        let mut arm = WireCursor {
            words: cursor.take(arm_len)?,
            pos: 0,
            body,
            inst,
            instruction_count,
        };
        decode_match_arm(&mut arm, children)?;
        arm.finish()?;
    }
    Ok(())
}

fn decode_match_arm(
    arm: &mut WireCursor<'_>,
    children: &mut Vec<InstRef>,
) -> Result<(), BodyMapError> {
    arm.skip(match_extra::PATTERN_KIND - 1)?;
    match arm.word()? {
        match_extra::PATTERN_WILDCARD => {}
        match_extra::PATTERN_LITERAL => skip_literal(arm)?,
        match_extra::PATTERN_ANON => {
            arm.skip(1)?;
            let field_count = arm.count()?;
            for _ in 0..field_count {
                arm.skip(1)?;
                match arm.word()? {
                    match_extra::FIELD_WILDCARD => {}
                    match_extra::FIELD_BIND => arm.skip(1)?,
                    match_extra::FIELD_LITERAL => skip_literal(arm)?,
                    _ => return malformed(arm.body, arm.inst),
                }
            }
        }
        match_extra::PATTERN_VARIANT => {
            arm.skip(2)?;
            let flags = arm.word()?;
            if flags & match_extra::FLAG_HAS_ARGS != 0 {
                let argc = arm.count()?;
                let words = match argc.checked_mul(2) {
                    Some(words) => words,
                    None => return malformed(arm.body, arm.inst),
                };
                arm.skip(words)?;
            }
        }
        _ => return malformed(arm.body, arm.inst),
    }

    match arm.word()? {
        match_extra::BODY_EXPR => arm.inst(children)?,
        match_extra::BODY_STMTS => arm.ref_list(children)?,
        _ => return malformed(arm.body, arm.inst),
    }
    Ok(())
}

fn skip_literal(cursor: &mut WireCursor<'_>) -> Result<(), BodyMapError> {
    match cursor.word()? {
        match_extra::LIT_INT => cursor.skip(2),
        match_extra::LIT_STR | match_extra::LIT_BOOL => cursor.skip(1),
        _ => malformed(cursor.body, cursor.inst),
    }
}

#[cfg(test)]
mod tests {
    use super::{BodyMapError, BodySlot};
    use crate::types::InternPool;
    use crate::uir::{InstData, InstRef, InstTag, Span, UirBuilder};

    #[test]
    fn body_map_assigns_dense_owner_and_local_slots() {
        let mut pool = InternPool::new();
        let mut builder = UirBuilder::new();
        let lhs = builder.int_literal(1, Span::EMPTY);
        let rhs = builder.int_literal(2, Span::EMPTY);
        let add = builder.binary(InstTag::Add, lhs, rhs, Span::EMPTY);
        let ret = builder.unary(InstTag::Return, add, Span::EMPTY);
        let first_name = pool.intern_str("first");
        let return_type = builder.type_name(pool.intern_str("int"), false, Span::EMPTY);
        builder.add_function(first_name, vec![], Some(return_type), &[ret], Span::EMPTY);

        let value = builder.int_literal(3, Span::EMPTY);
        let stmt = builder.unary(InstTag::ExprStmt, value, Span::EMPTY);
        let second_name = pool.intern_str("second");
        builder.add_function(second_name, vec![], None, &[stmt], Span::EMPTY);

        let map = builder.finish().compute_body_map().unwrap();
        assert_eq!(map.instructions(0), [ret, add, lhs, rhs]);
        assert_eq!(map.instructions(1), [stmt, value]);
        for (local, inst) in [ret, add, lhs, rhs].into_iter().enumerate() {
            assert_eq!(
                map.slot(inst),
                Some(BodySlot {
                    body: 0,
                    local: local as u32,
                })
            );
        }
        for (local, inst) in [stmt, value].into_iter().enumerate() {
            assert_eq!(
                map.slot(inst),
                Some(BodySlot {
                    body: 1,
                    local: local as u32,
                })
            );
        }
    }

    #[test]
    fn body_map_rejects_cross_body_references() {
        let mut pool = InternPool::new();
        let mut builder = UirBuilder::new();
        let shared = builder.int_literal(1, Span::EMPTY);
        let ret = builder.unary(InstTag::Return, shared, Span::EMPTY);
        let first_name = pool.intern_str("first");
        let return_type = builder.type_name(pool.intern_str("int"), false, Span::EMPTY);
        builder.add_function(first_name, vec![], Some(return_type), &[ret], Span::EMPTY);
        let stmt = builder.unary(InstTag::ExprStmt, shared, Span::EMPTY);
        let second_name = pool.intern_str("second");
        builder.add_function(second_name, vec![], None, &[stmt], Span::EMPTY);

        assert_eq!(
            builder.finish().compute_body_map(),
            Err(BodyMapError::CrossBody {
                inst: shared,
                first_body: 0,
                second_body: 1,
            })
        );
    }

    #[test]
    fn body_map_rejects_invalid_child_without_panicking() {
        let mut pool = InternPool::new();
        let mut builder = UirBuilder::new();
        let value = builder.int_literal(1, Span::EMPTY);
        let ret = builder.unary(InstTag::Return, value, Span::EMPTY);
        let main = pool.intern_str("main");
        let return_type = builder.type_name(pool.intern_str("int"), false, Span::EMPTY);
        builder.add_function(main, vec![], Some(return_type), &[ret], Span::EMPTY);
        let mut uir = builder.finish();
        let invalid = InstRef::from_raw(uir.instructions.len() as u32 + 10);
        uir.instructions[ret.index()].data = InstData::UnOp(invalid);

        assert!(matches!(
            uir.compute_body_map(),
            Err(BodyMapError::InvalidInstRef { inst, .. }) if inst == invalid
        ));

        let mut builder = UirBuilder::new();
        let arg = builder.int_literal(1, Span::EMPTY);
        let callee = pool.intern_str("callee");
        let call = builder.call(callee, &[arg], Span::EMPTY);
        builder.add_function(main, vec![], None, &[call], Span::EMPTY);
        let mut uir = builder.finish();
        let InstData::Extra(range) = &mut uir.instructions[call.index()].data else {
            panic!("call must carry an extra range");
        };
        range.len -= 1;

        assert!(matches!(
            uir.compute_body_map(),
            Err(BodyMapError::MalformedWire { inst, .. }) if inst == call
        ));

        let mut builder = UirBuilder::new();
        let leaf = builder.int_literal(1, Span::EMPTY);
        let first = builder.unary(InstTag::Neg, leaf, Span::EMPTY);
        let second = builder.unary(InstTag::Neg, first, Span::EMPTY);
        let root = builder.unary(InstTag::ExprStmt, second, Span::EMPTY);
        builder.add_function(main, vec![], None, &[root], Span::EMPTY);
        let mut uir = builder.finish();
        uir.instructions[first.index()].data = InstData::UnOp(second);

        assert!(matches!(
            uir.compute_body_map(),
            Err(BodyMapError::MalformedWire { .. })
        ));

        let mut builder = UirBuilder::new();
        let scrutinee = builder.int_literal(1, Span::EMPTY);
        let arm_body = builder.int_literal(2, Span::EMPTY);
        let matched = builder.match_expr(
            scrutinee,
            &[crate::uir::UirMatchArm {
                span: Span::EMPTY,
                pattern: crate::uir::UirPattern::Wildcard,
                body: crate::uir::UirArmBody::Expr(arm_body),
            }],
            Span::EMPTY,
        );
        builder.add_function(main, vec![], None, &[matched], Span::EMPTY);
        let mut uir = builder.finish();
        let InstData::Match { scrutinee, arms } = uir.instructions[matched.index()].data else {
            panic!("match must carry its inline payload");
        };
        uir.instructions[matched.index()].data = InstData::Catch { scrutinee, arms };

        assert!(matches!(
            uir.compute_body_map(),
            Err(BodyMapError::MalformedWire { inst, .. }) if inst == matched
        ));
    }

    #[test]
    fn body_map_allows_sharing_orphans_and_deep_chains() {
        let mut pool = InternPool::new();
        let mut builder = UirBuilder::new();
        let lhs = builder.int_literal(1, Span::EMPTY);
        let rhs = builder.int_literal(2, Span::EMPTY);
        let shared = builder.binary(InstTag::Add, lhs, rhs, Span::EMPTY);
        let root = builder.binary(InstTag::Mul, shared, shared, Span::EMPTY);
        let ret = builder.unary(InstTag::Return, root, Span::EMPTY);
        let orphan = builder.int_literal(99, Span::EMPTY);
        let main = pool.intern_str("main");
        let return_type = builder.type_name(pool.intern_str("int"), false, Span::EMPTY);
        builder.add_function(main, vec![], Some(return_type), &[ret], Span::EMPTY);

        let map = builder.finish().compute_body_map().unwrap();
        assert_eq!(map.instructions(0), [ret, root, shared, lhs, rhs]);
        assert_eq!(map.slot(shared).unwrap().local, 2);
        assert_eq!(map.slot(orphan), None);

        let mut builder = UirBuilder::new();
        let leaf = builder.int_literal(0, Span::EMPTY);
        let mut cursor = leaf;
        for _ in 0..10_001 {
            cursor = builder.unary(InstTag::Neg, cursor, Span::EMPTY);
        }
        let root = builder.unary(InstTag::ExprStmt, cursor, Span::EMPTY);
        builder.add_function(main, vec![], None, &[root], Span::EMPTY);

        let map = builder.finish().compute_body_map().unwrap();
        assert_eq!(map.instructions(0).len(), 10_003);
        assert_eq!(map.slot(root).unwrap().local, 0);
        assert_eq!(map.slot(leaf).unwrap().local, 10_002);
    }
}

#[cfg(test)]
mod unresolved_variant_tests {
    use super::BodyMapError;
    use crate::types::InternPool;
    use crate::uir::{InstData, InstTag, Span, UirBuilder, UirVariantForm};

    #[test]
    fn body_map_owns_unresolved_variant_arguments() {
        let mut pool = InternPool::new();
        let shape = pool.intern_str("Shape");
        let rectangle = pool.intern_str("Rectangle");
        let height = pool.intern_str("height");
        let named = pool.intern_str("Named");
        let code = pool.intern_str("code");
        let main = pool.intern_str("main");
        let mut builder = UirBuilder::new();
        let height_value = builder.float_literal(2.0, Span::EMPTY);
        let variant = builder.enum_lit(
            shape,
            rectangle,
            Span::EMPTY,
            Span::EMPTY,
            UirVariantForm {
                positional: false,
                named_parens: false,
                mixed_fields: false,
            },
            &[(height.raw(), height_value)],
            Span::EMPTY,
        );
        let variant_stmt = builder.unary(InstTag::ExprStmt, variant, Span::EMPTY);
        let code_value = builder.int_literal(1, Span::EMPTY);
        let error = builder.error_construct(
            named,
            None,
            None,
            UirVariantForm {
                positional: false,
                named_parens: true,
                mixed_fields: false,
            },
            &[(code.raw(), code_value)],
            Span::EMPTY,
        );
        let error_stmt = builder.unary(InstTag::ExprStmt, error, Span::EMPTY);
        builder.add_function(main, vec![], None, &[variant_stmt, error_stmt], Span::EMPTY);
        let mut uir = builder.finish();

        let map = uir.compute_body_map().expect("valid unresolved wires");
        assert_eq!(
            map.instructions(0),
            [
                variant_stmt,
                variant,
                height_value,
                error_stmt,
                error,
                code_value,
            ]
        );

        let original_len = {
            let InstData::Extra(range) = &mut uir.instructions[variant.index()].data else {
                panic!("variant must carry an extra range");
            };
            let original_len = range.len;
            range.len -= 1;
            original_len
        };
        assert!(matches!(
            uir.compute_body_map(),
            Err(BodyMapError::MalformedWire { inst, .. }) if inst == variant
        ));
        let InstData::Extra(range) = &mut uir.instructions[variant.index()].data else {
            panic!("variant must carry an extra range");
        };
        range.len = original_len;

        let InstData::ErrorConstruct { args, .. } = &mut uir.instructions[error.index()].data
        else {
            panic!("error candidate must carry its name and argument range");
        };
        args.len -= 1;
        assert!(matches!(
            uir.compute_body_map(),
            Err(BodyMapError::MalformedWire { inst, .. }) if inst == error
        ));
    }
}

#[cfg(test)]
mod type_expr_tests {
    use super::BodyMapError;
    use crate::types::InternPool;
    use crate::uir::{
        Span, TypeExprId, TypeExprList, Uir, UirBuilder, UirStructDecl, UirStructField,
        UirTypeExpr, UirTypeExprKind,
    };

    #[test]
    fn type_expr_validation_rejects_out_of_range_return_annotation() {
        let mut pool = InternPool::new();
        let main = pool.intern_str("main");
        let mut builder = UirBuilder::new();
        builder.add_function(
            main,
            vec![],
            Some(TypeExprId::from_raw(99)),
            &[],
            Span::EMPTY,
        );

        assert_eq!(
            builder.finish().compute_body_map(),
            Err(BodyMapError::InvalidTypeExprRef { raw: 99 })
        );
    }

    #[test]
    fn type_expr_validation_rejects_out_of_range_variable_annotation() {
        let mut pool = InternPool::new();
        let main = pool.intern_str("main");
        let value_name = pool.intern_str("value");
        let mut builder = UirBuilder::new();
        let value = builder.int_literal(1, Span::EMPTY);
        let declaration = builder.var_decl(
            value_name,
            false,
            Some(TypeExprId::from_raw(99)),
            value,
            Span::EMPTY,
        );
        builder.add_function(main, vec![], None, &[declaration], Span::EMPTY);

        assert_eq!(
            builder.finish().compute_body_map(),
            Err(BodyMapError::InvalidTypeExprRef { raw: 99 })
        );
    }

    #[test]
    fn type_expr_validation_rejects_out_of_range_nominal_field() {
        let mut pool = InternPool::new();
        let name = pool.intern_str("Point");
        let field = pool.intern_str("x");
        let mut builder = UirBuilder::new();
        builder.add_struct_decl(UirStructDecl {
            name,
            name_span: Span::EMPTY,
            attrs: crate::ast::StructAttrs::default(),
            fields: vec![UirStructField {
                name: field,
                type_expr: TypeExprId::from_raw(99),
                span: Span::EMPTY,
            }],
            span: Span::EMPTY,
            source_index: 0,
        });

        assert_eq!(
            builder.finish().compute_body_map(),
            Err(BodyMapError::InvalidTypeExprRef { raw: 99 })
        );
    }

    #[test]
    fn type_expr_validation_rejects_out_of_range_list_range() {
        let mut uir = Uir::new();
        uir.type_exprs.push(UirTypeExpr {
            kind: UirTypeExprKind::Positional(TypeExprList { offset: 0, len: 1 }),
            span: Span::EMPTY,
        });

        assert_eq!(
            uir.compute_body_map(),
            Err(BodyMapError::InvalidTypeExprRange {
                id: TypeExprId::from_raw(1),
                offset: 0,
                len: 1,
            })
        );
    }

    #[test]
    fn type_expr_validation_rejects_out_of_range_child() {
        let mut uir = Uir::new();
        uir.type_expr_lists.push(TypeExprId::from_raw(99));
        uir.type_exprs.push(UirTypeExpr {
            kind: UirTypeExprKind::Positional(TypeExprList { offset: 0, len: 1 }),
            span: Span::EMPTY,
        });

        assert_eq!(
            uir.compute_body_map(),
            Err(BodyMapError::InvalidTypeExprRef { raw: 99 })
        );
    }

    #[test]
    fn type_expr_validation_rejects_cycles() {
        let mut uir = Uir::new();
        let first = TypeExprId::from_raw(1);
        let second = TypeExprId::from_raw(2);
        uir.type_expr_lists.extend([first, second]);
        uir.type_exprs.extend([
            UirTypeExpr {
                kind: UirTypeExprKind::Positional(TypeExprList { offset: 0, len: 2 }),
                span: Span::EMPTY,
            },
            UirTypeExpr {
                kind: UirTypeExprKind::Positional(TypeExprList { offset: 0, len: 2 }),
                span: Span::EMPTY,
            },
        ]);

        assert_eq!(
            uir.compute_body_map(),
            Err(BodyMapError::TypeExprCycle { id: first })
        );
    }

    #[test]
    fn type_expr_validation_accepts_shared_and_deep_graphs() {
        let mut pool = InternPool::new();
        let shared_name = pool.intern_str("int");
        let field_name = pool.intern_str("value");
        let mut builder = UirBuilder::new();
        let shared = builder.type_name(shared_name, false, Span::EMPTY);
        let mut cursor = shared;
        for _ in 0..10_000 {
            cursor = builder.type_positional(&[cursor], Span::EMPTY);
        }
        builder.type_anon(&[(field_name, cursor), (field_name, shared)], Span::EMPTY);

        builder.finish().compute_body_map().unwrap();
    }
}
