use super::{Flow, PlaceId};
use crate::tir::{
    BindingInfo, BindingKind, Discriminant, ScopeId, Span, Tir, TirBuilder, TypedMatchArm,
    TypedMatchBody,
};
use crate::types::InternPool;

fn copy_match_and_local(include_local: bool) -> (Tir, InternPool) {
    let mut pool = InternPool::new();
    let int = pool.int();
    let field = pool.intern_str("field");
    let pair_ty = pool.anon_struct(&[(field, int)]);
    let main = pool.intern_str("main");
    let copy_name = pool.intern_str("copy");
    let root_name = pool.intern_str("root");
    let mut builder = TirBuilder::new(main, vec![], pool.void(), Span::EMPTY);
    let copy_id = builder.register_binding(BindingInfo::new(
        copy_name,
        int,
        BindingKind::Local {
            mutable: false,
            borrowed: false,
        },
        ScopeId::from_index(1),
        Span::EMPTY,
    ));
    let root = include_local.then(|| {
        let root_id = builder.register_binding(BindingInfo::new(
            root_name,
            int,
            BindingKind::Local {
                mutable: false,
                borrowed: false,
            },
            ScopeId::from_index(0),
            Span::EMPTY,
        ));
        let one = builder.int_const(1, int, Span::EMPTY);
        builder.var_decl(root_id, false, int, one, Span::EMPTY)
    });
    let value = builder.int_const(2, int, Span::EMPTY);
    let scrutinee = builder.struct_lit(pair_ty, &[(0, value)], Span::EMPTY);
    let match_ = builder.match_expr(
        scrutinee,
        pool.void(),
        &[TypedMatchArm {
            span: Span::EMPTY,
            discriminant: Discriminant::Wildcard,
            bindings: &[crate::tir::MatchBinding {
                field_index: 0,
                id: copy_id,
                ty: int,
            }],
            guards: &[],
            body: TypedMatchBody::Stmts(&[]),
        }],
        Span::EMPTY,
    );
    let statements = match root {
        Some(root) => vec![root, match_],
        None => vec![match_],
    };
    (builder.finish(&statements), pool)
}

#[test]
fn flow_lookup_stamp_rejects_cross_length_alias() {
    let (tir, pool) = copy_match_and_local(false);
    let flow = Flow::build(&tir, &pool);
    let mut malformed = flow.clone();
    malformed.binding_places = vec![Some(PlaceId(u32::MAX)), Some(PlaceId(u32::MAX))];

    malformed
        .require_body(&tir)
        .expect_err("cross-length lookup alias must fail require_body without restamping");
    malformed.content_stamp = malformed.representation_stamp();
    malformed
        .require_bindings(&tir, &pool)
        .expect_err("dense validation must reject the mismatched lookup length after restamping");
}

#[test]
fn flow_lookup_stamp_rejects_equal_length_alias() {
    let (tir, pool) = copy_match_and_local(true);
    let flow = Flow::build(&tir, &pool);
    let mut original = flow.clone();
    original.binding_places = vec![None, Some(PlaceId(u32::MAX))];
    original.content_stamp = original.representation_stamp();
    let mut alias = original.clone();
    alias.binding_places.reverse();

    alias
        .require_body(&tir)
        .expect_err("equal-length lookup alias must fail require_body without restamping");
    alias.content_stamp = alias.representation_stamp();
    alias
        .require_bindings(&tir, &pool)
        .expect_err("dense validation must reject the out-of-bounds place after restamping");
}
