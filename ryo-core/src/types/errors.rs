use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorSetView {
    pub members: Vec<TypeId>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ErrorUnionView {
    pub error_set: TypeId,
    pub success: TypeId,
}

impl InternPool {
    fn push_error_item(&mut self, tag: Tag, data: u32) -> TypeId {
        let id = TypeId(
            u32::try_from(self.items.len())
                .expect("type pool overflow: more than u32::MAX types interned"),
        );
        self.items.push(Item { tag, data });
        id
    }

    pub fn declare_error(&mut self, name: StringId) -> TypeId {
        if let Some(&id) = self.error_names.get(&name) {
            return id;
        }
        let id = self.push_error_item(Tag::DeclaredError, u32::MAX);
        self.error_names.insert(name, id);
        id
    }

    pub fn define_error(&mut self, id: TypeId, payload: EnumVariantDef) {
        let item = self.items[id.0 as usize];
        debug_assert!(matches!(item.tag, Tag::DeclaredError));
        assert!(item.data == u32::MAX, "define_error: error already defined");
        self.define_enum_layout(
            id,
            payload.name,
            std::slice::from_ref(&payload),
            StructFlags::default(),
        );
    }

    pub fn error_set(&mut self, members: &[TypeId]) -> TypeId {
        let mut canonical = members.to_vec();
        canonical.sort_unstable_by_key(|member| member.raw());
        canonical.dedup();
        for &member in &canonical {
            assert!(
                matches!(self.kind(member), TypeKind::DeclaredError),
                "error set members must be declared errors"
            );
        }
        if canonical.len() == 1 {
            return canonical[0];
        }
        if let Some(&id) = self.error_sets.get(&canonical) {
            return id;
        }

        let name = self.intern_str("");
        if canonical.is_empty() {
            let data = u32::try_from(self.extra.len())
                .expect("extra arena overflow: more than u32::MAX u32 entries");
            let fields_base = u32::try_from(self.enum_fields.len()).expect("enum fields overflow");
            self.extra.extend_from_slice(&[
                name.raw(),
                0,
                0,
                0,
                StructFlags::default().pack(true),
                fields_base,
            ]);
            let id = self.push_error_item(Tag::ErrorSet, data);
            self.error_sets.insert(canonical, id);
            return id;
        }

        let variants: Vec<_> = canonical
            .iter()
            .map(|&member| {
                let view = self.enum_view(member);
                let variant = view
                    .variants()
                    .next()
                    .expect("declared error has one variant");
                EnumVariantDef {
                    name: variant.name,
                    kind: variant.kind,
                    fields: variant
                        .fields
                        .iter()
                        .map(|field| (field.name, field.ty))
                        .collect(),
                }
            })
            .collect();
        let id = self.push_error_item(Tag::ErrorSet, u32::MAX);
        self.define_enum_layout(id, name, &variants, StructFlags::default());
        let member_words: Vec<_> = canonical.iter().map(|member| member.raw()).collect();
        self.extra.extend_from_slice(&member_words);
        self.error_sets.insert(canonical, id);
        id
    }

    pub fn error_union(&mut self, error_set: TypeId, success: TypeId) -> TypeId {
        assert!(
            matches!(
                self.kind(error_set),
                TypeKind::DeclaredError | TypeKind::ErrorSet
            ),
            "error union requires an error set"
        );
        if let Some(&id) = self.error_unions.get(&(error_set, success)) {
            return id;
        }

        let name = self.intern_str("");
        let ok = self.intern_str("Ok");
        let err = self.intern_str("Err");
        let field = self.intern_str("0");
        let variants = [
            EnumVariantDef {
                name: ok,
                kind: VariantKind::Tuple,
                fields: vec![(field, success)],
            },
            EnumVariantDef {
                name: err,
                kind: VariantKind::Tuple,
                fields: vec![(field, error_set)],
            },
        ];
        let id = self.push_error_item(Tag::ErrorUnion, u32::MAX);
        self.define_enum_layout(id, name, &variants, StructFlags::default());
        self.error_unions.insert((error_set, success), id);
        id
    }

    pub fn inferred_error_union(&mut self, success: TypeId) -> TypeId {
        if let Some(&id) = self.inferred_error_unions.get(&success) {
            return id;
        }
        let error_set = self.error_set(&[]);
        let data = u32::try_from(self.extra.len())
            .expect("extra arena overflow: more than u32::MAX u32 entries");
        self.extra
            .extend_from_slice(&[error_set.raw(), success.raw()]);
        let id = self.push_error_item(Tag::InferredErrorUnion, data);
        self.inferred_error_unions.insert(success, id);
        id
    }

    pub fn error_set_view(&self, id: TypeId) -> ErrorSetView {
        let item = self.items[id.0 as usize];
        match item.tag {
            Tag::DeclaredError => ErrorSetView { members: vec![id] },
            Tag::ErrorSet => {
                let start = item.data as usize;
                let n = self.extra[start + 1] as usize;
                let mut cursor = start + 6;
                for _ in 0..n {
                    let n_fields = self.extra[cursor + 3] as usize;
                    cursor += 4 + 2 * n_fields;
                }
                let members = self.extra[cursor..cursor + n]
                    .iter()
                    .map(|&raw| TypeId::from_raw(raw))
                    .collect();
                ErrorSetView { members }
            }
            _ => unreachable!("error_set_view: not an error set"),
        }
    }

    pub fn error_union_view(&self, id: TypeId) -> ErrorUnionView {
        let item = self.items[id.0 as usize];
        match item.tag {
            Tag::ErrorUnion => {
                let view = self.enum_view(id);
                let mut variants = view.variants();
                let ok = variants.next().expect("error union has Ok variant");
                let err = variants.next().expect("error union has Err variant");
                ErrorUnionView {
                    error_set: err.fields[0].ty,
                    success: ok.fields[0].ty,
                }
            }
            Tag::InferredErrorUnion => {
                let start = item.data as usize;
                ErrorUnionView {
                    error_set: TypeId::from_raw(self.extra[start]),
                    success: TypeId::from_raw(self.extra[start + 1]),
                }
            }
            _ => unreachable!("error_union_view: not an error union"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;

    fn error_with_fields(
        pool: &mut InternPool,
        name: &str,
        kind: VariantKind,
        fields: Vec<(StringId, TypeId)>,
    ) -> TypeId {
        let name = pool.intern_str(name);
        let id = pool.declare_error(name);
        pool.define_error(id, EnumVariantDef { name, kind, fields });
        id
    }

    fn unit_error(pool: &mut InternPool, name: &str) -> TypeId {
        error_with_fields(pool, name, VariantKind::Unit, vec![])
    }

    #[test]
    fn union_members_are_canonical_and_single_sets_collapse() {
        let mut pool = InternPool::new();
        let a = unit_error(&mut pool, "A");
        let b = unit_error(&mut pool, "B");

        assert_eq!(pool.kind(a), TypeKind::DeclaredError);
        assert_ne!(a, pool.error_type());
        assert!(!pool.is_error(a));
        let a_name = pool.intern_str("A");
        assert_eq!(pool.declare_error(a_name), a);
        assert_eq!(pool.error_set(&[a]), a);
        assert_eq!(pool.error_set(&[a, a]), a);

        let ab = pool.error_set(&[a, b]);
        let ba = pool.error_set(&[b, a, a]);
        assert_eq!(ab, ba);
        assert_eq!(pool.kind(ab), TypeKind::ErrorSet);
        assert_eq!(pool.error_set_view(a).members, vec![a]);
        assert_eq!(pool.error_set_view(ab).members, vec![a, b]);

        let union = pool.error_union(ab, pool.int());
        let same_union = pool.error_union(ba, pool.int());
        assert_eq!(union, same_union);
        assert_eq!(pool.kind(union), TypeKind::ErrorUnion);
        let view = pool.error_union_view(union);
        assert_eq!(view.error_set, ab);
        assert_eq!(view.success, pool.int());

        let inferred = pool.inferred_error_union(pool.int());
        assert_eq!(inferred, pool.inferred_error_union(pool.int()));
        assert_eq!(pool.kind(inferred), TypeKind::InferredErrorUnion);
        assert_ne!(inferred, union);
        assert_eq!(pool.error_union_view(inferred).success, pool.int());
    }

    #[test]
    fn error_union_layout_has_ok_and_err_and_correct_drop_flags() {
        let mut pool = InternPool::new();
        let message = pool.intern_str("message");
        let str_ = pool.str_();
        let a = error_with_fields(&mut pool, "A", VariantKind::Named, vec![(message, str_)]);
        let union = pool.error_union(a, pool.str_());
        let ok = pool.intern_str("Ok");
        let err = pool.intern_str("Err");

        assert!(!pool.is_copy(a));
        assert!(pool.needs_drop(a));
        assert!(!pool.is_copy(union));
        assert!(pool.needs_drop(union));
        assert_eq!(pool.enum_view(a).len(), 1);
        assert_eq!(pool.size_align(union), (40, 8));
        let variants: Vec<_> = pool.enum_view(union).variants().collect();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].name, ok);
        assert_eq!(variants[0].kind, VariantKind::Tuple);
        assert_eq!(variants[0].fields[0].ty, pool.str_());
        assert_eq!(variants[1].name, err);
        assert_eq!(variants[1].kind, VariantKind::Tuple);
        assert_eq!(variants[1].fields[0].ty, a);

        let b = unit_error(&mut pool, "B");
        let c = unit_error(&mut pool, "C");
        let copy_set = pool.error_set(&[b, c]);
        let copy_union = pool.error_union(copy_set, pool.int());
        assert_eq!(pool.enum_view(copy_set).len(), 2);
        assert!(pool.is_copy(copy_set));
        assert!(!pool.needs_drop(copy_set));
        assert!(pool.is_copy(copy_union));
        assert!(!pool.needs_drop(copy_union));
        assert_eq!(pool.size_align(copy_union), (16, 8));
    }

    #[test]
    fn inferred_empty_error_set_has_no_constructible_variant() {
        let mut pool = InternPool::new();
        let empty = pool.error_set(&[]);
        assert_eq!(pool.kind(empty), TypeKind::ErrorSet);
        assert!(pool.error_set_view(empty).members.is_empty());
        assert!(pool.enum_view(empty).is_empty());
        assert_eq!(pool.size_align(empty), (0, 0));
        assert!(!pool.needs_drop(empty));

        let union = pool.error_union(empty, pool.int());
        assert_eq!(pool.size_align(union), (16, 8));
        let variants: Vec<_> = pool.enum_view(union).variants().collect();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].fields[0].ty, pool.int());
        assert_eq!(variants[1].fields[0].ty, empty);
        assert!(pool.enum_view(variants[1].fields[0].ty).is_empty());

        let inferred = pool.inferred_error_union(pool.int());
        let view = pool.error_union_view(inferred);
        assert_eq!(view.error_set, empty);
        assert_eq!(view.success, pool.int());
        assert!(std::panic::catch_unwind(|| pool.size_align(inferred)).is_err());

        let void_union = pool.error_union(empty, pool.void());
        assert_eq!(pool.size_align(void_union), (4, 4));
    }
}
