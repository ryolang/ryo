//! Variant construction lowering (M11): `EnumName.Variant(args)` →
//! [`InstTag::EnumLit`]. Split out of `astgen.rs` so the parent file
//! stays under the file-length gate.

use super::*;
use ryo_core::diag::ParseDiag;
use ryo_core::extra::enum_lit_extra;

/// Lower `EnumName.Variant(args)` (M11) to [`InstTag::EnumLit`]: variant and
/// payload field indices come from the declaration-order directory, so named
/// arguments canonicalize against declaration order — pairs stay in source
/// order, each carrying its declaration-order field index.
///
/// An undeclared enum is `UnknownType` (except the parenthesized form,
/// which predates M11 as a method call on an uppercase-led value and is
/// restored to `MethodCall`); an unknown variant is `UnknownVariant`; an
/// unknown payload field reuses `UnknownField`. A declared-but-failed
/// enum lowers quietly to the error type (the struct precedent — its own
/// diagnostic is already in the sink).
///
/// The source form rides the wire via `enum_lit_extra::FLAG_POSITIONAL`:
/// the parenthesized positional form sets it, the braced named form
/// clears it. Named parens are diagnosed here and recovered as positional
/// now that the parser preserves them for error-family construction.
/// Sema needs the distinction to enforce the Brace Law on
/// named payloads (D11): `Rectangle(1.0, 2.0)` on `Rectangle(width: float,
/// height: float)` is rejected with the brace spelling, while the
/// field-identical `Rectangle{width=1.0, height=2.0}` stays legal — in
/// the flat `(field_idx, value)` encoding the two are otherwise
/// indistinguishable. A named field astgen doesn't recognize rides as
/// `enum_lit_extra::ORPHANED_FIELD` so the arg stays reachable for sema.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_variant_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    c: ast::VariantConstruct,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
    depth: u32,
) -> InstRef {
    let mut step = prepare_variant_construct(b, ast, c, types, defined_enums, pool, sink, span);
    loop {
        match step {
            VariantStep::Complete(result) => return result,
            VariantStep::Child(state, arg) => {
                let value = super::expr::gen_expr_d(
                    b,
                    ast,
                    arg,
                    types,
                    defined_enums,
                    pool,
                    sink,
                    depth + 1,
                );
                step = resume_variant_construct(state, value, b, ast, types, pool, sink);
            }
        }
    }
}

pub(super) enum VariantStep {
    Child(VariantState, ast::ExprId),
    Complete(InstRef),
}

pub(super) struct VariantState {
    c: ast::VariantConstruct,
    args: ast::VariantArgs,
    span: Span,
    next: usize,
    kind: VariantStateKind,
}

enum VariantStateKind {
    Method {
        recv: InstRef,
        args: Vec<InstRef>,
    },
    Declared {
        declared: TypeId,
        ty: TypeId,
        vidx: usize,
        positional: bool,
        pairs: Vec<(u32, InstRef)>,
    },
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_variant_construct(
    b: &mut UirBuilder,
    ast: &ast::Ast,
    c: ast::VariantConstruct,
    types: &TypeResolver,
    defined_enums: &HashSet<TypeId>,
    pool: &mut InternPool,
    sink: &mut DiagSink,
    span: Span,
) -> VariantStep {
    let enum_name = c.enum_name.name;
    let Some(&declared) = types.enum_types.get(&enum_name) else {
        let args = c.args.expect("parser always passes Some");
        if args.named_parens {
            sink.emit(Diag::error(
                c.variant.span,
                DiagCode::NamedArgInParens,
                ParseDiag::NamedArgInParens {
                    enum_name: None,
                    variant: None,
                }
                .message(pool),
            ));
            let recv = b.var_ref(enum_name, c.enum_name.span);
            return next_variant_step(
                VariantState {
                    c,
                    args,
                    span,
                    next: 0,
                    kind: VariantStateKind::Method {
                        recv,
                        args: Vec::new(),
                    },
                },
                b,
                ast,
            );
        }
        if args.positional.is_some() {
            // The parser promotes every `Uppercase.name(args)` to
            // VariantConstruct; when the receiver names no enum, the
            // paren form may be a method call on an uppercase-led
            // value (`S = "hi"; S.len()`), which had exactly this
            // MethodCall shape before M11. Restore it and let sema
            // resolve the receiver: a value gets its method back, an
            // unknown name gets the ordinary "undefined variable", a
            // struct name gets the not-a-value error. Braces keep
            // UnknownType below — `Point.new{x=1}` has no
            // method-call meaning.
            let recv = b.var_ref(enum_name, c.enum_name.span);
            return next_variant_step(
                VariantState {
                    c,
                    args,
                    span,
                    next: 0,
                    kind: VariantStateKind::Method {
                        recv,
                        args: Vec::new(),
                    },
                },
                b,
                ast,
            );
        }
        sink.emit(Diag::error(
            c.enum_name.span,
            DiagCode::UnknownType,
            format!("unknown type: '{}'", pool.str(enum_name)),
        ));
        return VariantStep::Complete(b.enum_lit(pool.error_type(), 0, false, &[], span));
    };
    let ty = if defined_enums.contains(&declared) {
        declared
    } else {
        pool.error_type()
    };
    let args = c.args.expect("parser always passes Some");
    if args.named_parens {
        sink.emit(Diag::error(
            c.variant.span,
            DiagCode::NamedArgInParens,
            ParseDiag::NamedArgInParens {
                enum_name: Some(enum_name),
                variant: Some(c.variant.name),
            }
            .message(pool),
        ));
    }
    let variants = &types.enum_variant_fields[&declared];
    let Some(vidx) = variants
        .iter()
        .position(|&(vname, _, _)| vname == c.variant.name)
    else {
        sink.emit(Diag::error(
            c.variant.span,
            DiagCode::UnknownVariant,
            format!(
                "enum '{}' has no variant '{}'",
                pool.str(enum_name),
                pool.str(c.variant.name),
            ),
        ));
        // Poison the recovery with the error type, matching the
        // undeclared-enum fallback above: the real type would make this
        // node byte-identical to a legitimate empty construct of variant
        // 0, and sema would pile a misleading missing-fields error for
        // that variant on top of the accurate one just emitted.
        return VariantStep::Complete(b.enum_lit(pool.error_type(), 0, false, &[], span));
    };
    let positional = args.positional.is_some() || args.named_parens;
    next_variant_step(
        VariantState {
            c,
            args,
            span,
            next: 0,
            kind: VariantStateKind::Declared {
                declared,
                ty: if args.mixed_fields {
                    pool.error_type()
                } else {
                    ty
                },
                vidx,
                positional,
                pairs: Vec::new(),
            },
        },
        b,
        ast,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resume_variant_construct(
    mut state: VariantState,
    value: InstRef,
    b: &mut UirBuilder,
    ast: &ast::Ast,
    types: &TypeResolver,
    pool: &mut InternPool,
    sink: &mut DiagSink,
) -> VariantStep {
    match &mut state.kind {
        VariantStateKind::Method { args, .. } => args.push(value),
        VariantStateKind::Declared {
            declared,
            vidx,
            positional,
            pairs,
            ..
        } => {
            if *positional {
                pairs.push((state.next as u32, value));
            } else {
                let inits = state
                    .args
                    .named
                    .expect("braced variant carries named fields");
                let &(fname, arg) = &ast.struct_field_inits(inits)[state.next];
                let variants = &types.enum_variant_fields[declared];
                let kind = variants[*vidx].1;
                let field_names = &variants[*vidx].2;
                let Some(fidx) = field_names.iter().position(|&n| n == fname) else {
                    // A tuple variant under the brace form: the name
                    // is unknown because the form is wrong — append
                    // the paren spelling.
                    let hint = if kind == VariantKind::Tuple {
                        format!(
                            "; '{}' is a tuple variant: {}.{}(...)",
                            pool.str(state.c.variant.name),
                            pool.str(state.c.enum_name.name),
                            pool.str(state.c.variant.name),
                        )
                    } else {
                        String::new()
                    };
                    sink.emit(Diag::error(
                        ast.expr_span(arg),
                        DiagCode::UnknownField,
                        format!(
                            "variant '{}' of enum '{}' has no field '{}'{}",
                            pool.str(state.c.variant.name),
                            pool.str(state.c.enum_name.name),
                            pool.str(fname),
                            hint,
                        ),
                    ));
                    // Keep the arg reachable for sema under the
                    // ORPHANED_FIELD sentinel so its own diagnostics
                    // still surface; sema analyzes it and suppresses
                    // the derived missing-fields error (I-206's
                    // sibling — a dropped pair must not read as a
                    // valueless declared field).
                    pairs.push((enum_lit_extra::ORPHANED_FIELD, value));
                    state.next += 1;
                    return next_variant_step(state, b, ast);
                };
                pairs.push((fidx as u32, value));
            }
        }
    }
    state.next += 1;
    next_variant_step(state, b, ast)
}

fn next_variant_step(state: VariantState, b: &mut UirBuilder, ast: &ast::Ast) -> VariantStep {
    let child = if let Some(list) = state.args.positional {
        ast.expr_list(list).get(state.next).copied()
    } else if let Some(inits) = state.args.named {
        ast.struct_field_inits(inits)
            .get(state.next)
            .map(|&(_, arg)| arg)
    } else {
        None
    };
    if let Some(child) = child {
        return VariantStep::Child(state, child);
    }

    let result = match state.kind {
        VariantStateKind::Method { recv, args } => {
            b.method_call(recv, state.c.variant.name, &args, state.span)
        }
        VariantStateKind::Declared {
            ty,
            vidx,
            positional,
            pairs,
            ..
        } => b.enum_lit(ty, vidx as u32, positional, &pairs, state.span),
    };
    VariantStep::Complete(result)
}
