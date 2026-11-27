use super::{Flow, PlaceId};
use crate::ast::CompoundOp;
use crate::tir::{
    BindingInfo, BindingKind, Discriminant, ScopeId, Span, Tir, TirBuilder, TirTag, TypedMatchArm,
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

#[test]
fn flow_require_bindings_rejects_foreign_pool_without_tokens() {
    let (tir, pool) = copy_match_and_local(false);
    let flow = Flow::build(&tir, &pool);
    assert!(
        flow.places
            .iter()
            .all(|place| !matches!(place, super::Place::Token { .. })),
        "fixture must not contain move tokens"
    );
    let foreign = InternPool::new();
    let error = flow
        .require_bindings(&tir, &foreign)
        .expect_err("a foreign pool must fail before body and binding validation");
    assert!(
        error.contains("different type and layout context"),
        "{error}"
    );
}

enum CopyArmUse {
    Read,
    Assign,
    CompoundAssign,
}

fn copy_match_with_arm(arm_use: CopyArmUse) -> (Tir, InternPool) {
    let mut pool = InternPool::new();
    let int = pool.int();
    let field = pool.intern_str("field");
    let pair_ty = pool.anon_struct(&[(field, int)]);
    let main = pool.intern_str("main");
    let copy_name = pool.intern_str("copy");
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
    let value = builder.int_const(1, int, Span::EMPTY);
    let scrutinee = builder.struct_lit(pair_ty, &[(0, value)], Span::EMPTY);
    let arm_statement = match arm_use {
        CopyArmUse::Read => {
            let read = builder.var(copy_id, int, Span::EMPTY);
            builder.unary(TirTag::ExprStmt, pool.void(), read, Span::EMPTY)
        }
        CopyArmUse::Assign => {
            let value = builder.int_const(2, int, Span::EMPTY);
            builder.assign(copy_id, int, value, Span::EMPTY)
        }
        CopyArmUse::CompoundAssign => {
            let value = builder.int_const(2, int, Span::EMPTY);
            builder.compound_assign(copy_id, CompoundOp::Add, int, value, Span::EMPTY)
        }
    };
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
            body: TypedMatchBody::Stmts(&[arm_statement]),
        }],
        Span::EMPTY,
    );
    (builder.with_layout_context(&pool).finish(&[match_]), pool)
}

#[test]
fn flow_try_build_allows_read_of_placeless_copy_binding() {
    let (tir, pool) = copy_match_with_arm(CopyArmUse::Read);
    Flow::try_build(&tir, &pool).expect("an active place-less Copy read is valid");
}

#[test]
fn flow_try_build_rejects_assign_to_placeless_copy_binding() {
    let (tir, pool) = copy_match_with_arm(CopyArmUse::Assign);
    let error = Flow::try_build(&tir, &pool)
        .expect_err("assignment to a place-less Copy binding must not drop its write");
    assert!(error.contains("no Flow place for write"), "{error}");
}

#[test]
fn flow_try_build_rejects_compound_assign_to_placeless_copy_binding() {
    let (tir, pool) = copy_match_with_arm(CopyArmUse::CompoundAssign);
    let error = Flow::try_build(&tir, &pool)
        .expect_err("compound assignment to a place-less Copy binding must not drop its write");
    assert!(error.contains("no Flow place for write"), "{error}");
}

fn match_with_wrong_field_token() -> (Tir, InternPool) {
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let first = pool.intern_str("first");
    let second = pool.intern_str("second");
    let pair_ty = pool.anon_struct(&[(first, str_ty), (second, str_ty)]);
    let main = pool.intern_str("main");
    let binding_name = pool.intern_str("payload");
    let mut builder = TirBuilder::new(main, vec![], pool.void(), Span::EMPTY);
    let binding_id = builder.register_binding(BindingInfo::new(
        binding_name,
        str_ty,
        BindingKind::Local {
            mutable: false,
            borrowed: false,
        },
        ScopeId::from_index(1),
        Span::EMPTY,
    ));
    let first_value = builder.str_const(first, str_ty, Span::EMPTY);
    let second_value = builder.str_const(second, str_ty, Span::EMPTY);
    let scrutinee =
        builder.struct_lit(pair_ty, &[(0, first_value), (1, second_value)], Span::EMPTY);
    builder.field_access(scrutinee, 0, str_ty, Span::EMPTY);
    let match_ = builder.match_expr(
        scrutinee,
        pool.void(),
        &[TypedMatchArm {
            span: Span::EMPTY,
            discriminant: Discriminant::Wildcard,
            bindings: &[crate::tir::MatchBinding {
                field_index: 1,
                id: binding_id,
                ty: str_ty,
            }],
            guards: &[],
            body: TypedMatchBody::Stmts(&[]),
        }],
        Span::EMPTY,
    );
    (builder.with_layout_context(&pool).finish(&[match_]), pool)
}

#[test]
fn flow_try_build_rejects_match_token_with_wrong_field_index() {
    let (tir, pool) = match_with_wrong_field_token();
    let error = Flow::try_build(&tir, &pool)
        .expect_err("owner token field index must match its binding's canonical field");
    assert!(error.contains("field index"), "{error}");
}

#[derive(Clone, Copy)]
enum TokenFailure {
    MatchMissing,
    MatchWrongType,
    MatchTagDataMismatch,
    DestructureMissing,
    DestructureWrongType,
    DestructureTagDataMismatch,
}

fn malformed_token_case(failure: TokenFailure) -> (Tir, InternPool) {
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let int = pool.int();
    let field = pool.intern_str("field");
    let pair_ty = pool.anon_struct(&[(field, str_ty)]);
    let main = pool.intern_str("main");
    let binding_name = pool.intern_str("payload");
    let mut builder = TirBuilder::new(main, vec![], pool.void(), Span::EMPTY);
    let binding_id = builder.register_binding(BindingInfo::new(
        binding_name,
        str_ty,
        BindingKind::Local {
            mutable: false,
            borrowed: false,
        },
        ScopeId::from_index(1),
        Span::EMPTY,
    ));
    let value = builder.str_const(field, str_ty, Span::EMPTY);
    let scrutinee = builder.struct_lit(pair_ty, &[(0, value)], Span::EMPTY);
    let token = match failure {
        TokenFailure::MatchWrongType | TokenFailure::DestructureWrongType => {
            Some(builder.field_access(scrutinee, 0, int, Span::EMPTY))
        }
        TokenFailure::MatchTagDataMismatch | TokenFailure::DestructureTagDataMismatch => {
            Some(builder.field_access(scrutinee, 0, str_ty, Span::EMPTY))
        }
        _ => None,
    };
    let instruction = match failure {
        TokenFailure::MatchMissing
        | TokenFailure::MatchWrongType
        | TokenFailure::MatchTagDataMismatch => builder.match_expr(
            scrutinee,
            pool.void(),
            &[TypedMatchArm {
                span: Span::EMPTY,
                discriminant: Discriminant::Wildcard,
                bindings: &[crate::tir::MatchBinding {
                    field_index: 0,
                    id: binding_id,
                    ty: str_ty,
                }],
                guards: &[],
                body: TypedMatchBody::Stmts(&[]),
            }],
            Span::EMPTY,
        ),
        TokenFailure::DestructureMissing
        | TokenFailure::DestructureWrongType
        | TokenFailure::DestructureTagDataMismatch => {
            builder.destructure(scrutinee, &[(0, Some(binding_id), str_ty)], Span::EMPTY)
        }
    };
    let mut tir = builder.with_layout_context(&pool).finish(&[instruction]);
    if matches!(
        failure,
        TokenFailure::MatchTagDataMismatch | TokenFailure::DestructureTagDataMismatch
    ) {
        let token = token.expect("tag/data mismatch fixture has a token");
        tir.instructions[token.index()].data = crate::tir::TirData::None;
    }
    (tir, pool)
}

#[test]
fn flow_try_build_rejects_missing_match_token_without_panic() {
    let (tir, pool) = malformed_token_case(TokenFailure::MatchMissing);
    let error = Flow::try_build(&tir, &pool).expect_err("missing match token must be an error");
    assert!(error.contains("field access"), "{error}");
}

#[test]
fn flow_try_build_rejects_wrongly_typed_match_token_without_panic() {
    let (tir, pool) = malformed_token_case(TokenFailure::MatchWrongType);
    let error = Flow::try_build(&tir, &pool).expect_err("wrong token type must be an error");
    assert!(error.contains("type"), "{error}");
}

#[test]
fn flow_try_build_rejects_missing_destructure_token_without_panic() {
    let (tir, pool) = malformed_token_case(TokenFailure::DestructureMissing);
    let error =
        Flow::try_build(&tir, &pool).expect_err("missing destructure token must be an error");
    assert!(error.contains("field access"), "{error}");
}

#[test]
fn flow_try_build_rejects_wrongly_typed_destructure_token_without_panic() {
    let (tir, pool) = malformed_token_case(TokenFailure::DestructureWrongType);
    let error = Flow::try_build(&tir, &pool).expect_err("wrong token type must be an error");
    assert!(error.contains("type"), "{error}");
}

#[test]
fn flow_try_build_rejects_match_token_tag_data_mismatch_without_panic() {
    let (tir, pool) = malformed_token_case(TokenFailure::MatchTagDataMismatch);
    let error = Flow::try_build(&tir, &pool)
        .expect_err("a FieldAccess tag with inconsistent data must be an ordinary error");
    assert!(error.contains("field access"), "{error}");
}

#[test]
fn flow_try_build_rejects_destructure_token_tag_data_mismatch_without_panic() {
    let (tir, pool) = malformed_token_case(TokenFailure::DestructureTagDataMismatch);
    let error = Flow::try_build(&tir, &pool)
        .expect_err("a FieldAccess tag with inconsistent data must be an ordinary error");
    assert!(error.contains("field access"), "{error}");
}

#[test]
fn flow_try_build_accepts_non_copy_parameter_scrutinee_tokens() {
    let mut pool = InternPool::new();
    let str_ty = pool.str_();
    let field = pool.intern_str("field");
    let pair_ty = pool.anon_struct(&[(field, str_ty)]);
    let main = pool.intern_str("main");
    let param_name = pool.intern_str("input");
    let binding_name = pool.intern_str("payload");
    let param_id = crate::tir::BindingId::from_index(0);
    let param = crate::tir::TirParam::new(
        param_id,
        param_name,
        pair_ty,
        crate::tir::ParamMode::Move,
        Span::EMPTY,
    );
    let mut builder = TirBuilder::new(main, vec![param], pool.void(), Span::EMPTY);
    let binding_id = builder.register_binding(BindingInfo::new(
        binding_name,
        str_ty,
        BindingKind::Local {
            mutable: false,
            borrowed: false,
        },
        ScopeId::from_index(1),
        Span::EMPTY,
    ));
    let scrutinee = builder.var(param_id, pair_ty, Span::EMPTY);
    let token = builder.field_access(scrutinee, 0, str_ty, Span::EMPTY);
    let match_ = builder.match_expr(
        scrutinee,
        pool.void(),
        &[TypedMatchArm {
            span: Span::EMPTY,
            discriminant: Discriminant::Wildcard,
            bindings: &[crate::tir::MatchBinding {
                field_index: 0,
                id: binding_id,
                ty: str_ty,
            }],
            guards: &[],
            body: TypedMatchBody::Stmts(&[]),
        }],
        Span::EMPTY,
    );
    let tir = builder.with_layout_context(&pool).finish(&[match_]);
    let tokens = tir
        .match_owner_tokens(match_, &pool)
        .expect("a non-Copy parameter-scrutinee token is valid");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].owner, token);
    Flow::try_build(&tir, &pool).expect("parameter-scrutinee match must build Flow");
}
