//! Variant construction lowering (M11): `EnumName.Variant(args)` →
//! [`InstTag::EnumLit`]. Split out of `astgen.rs` so the parent file
//! stays under the file-length gate.

use super::*;

/// Lower `EnumName.Variant(args)` (M11) to [`InstTag::EnumLit`]: variant and
/// payload field indices come from the declaration-order directory, so named
/// arguments canonicalize against declaration order — pairs stay in source
/// order, each carrying its declaration-order field index.
///
/// An undeclared enum is `UnknownType`; an unknown variant is
/// `UnknownVariant`; an unknown payload field reuses `UnknownField`. A
/// declared-but-failed enum lowers quietly to the error type (the
/// struct precedent — its own diagnostic is already in the sink).
///
/// The source form rides the wire via `enum_lit_extra::FLAG_POSITIONAL`:
/// the parenthesized positional form sets it, the braced named form
/// clears it. Sema needs the distinction to enforce the Brace Law on
/// named payloads (D11): `Rectangle(1.0, 2.0)` on `Rectangle(width: float,
/// height: float)` is rejected with the brace spelling, while the
/// field-identical `Rectangle{width=1.0, height=2.0}` stays legal — in
/// the flat `(field_idx, value)` encoding the two are otherwise
/// indistinguishable.
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
) -> InstRef {
    let enum_name = c.enum_name.name;
    let Some(&declared) = types.enum_types.get(&enum_name) else {
        sink.emit(Diag::error(
            c.enum_name.span,
            DiagCode::UnknownType,
            format!("unknown type: '{}'", pool.str(enum_name)),
        ));
        return b.enum_lit(pool.error_type(), 0, false, &[], span);
    };
    let ty = if defined_enums.contains(&declared) {
        declared
    } else {
        pool.error_type()
    };
    let args = c.args.expect("parser always passes Some");
    let variants = &types.enum_variant_fields[&declared];
    let Some(vidx) = variants
        .iter()
        .position(|&(vname, _)| vname == c.variant.name)
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
        return b.enum_lit(ty, 0, false, &[], span);
    };
    let mut pairs: Vec<(u32, InstRef)> = Vec::new();
    let positional = match (args.positional, args.named) {
        (Some(list), None) => {
            for (i, &arg) in ast.expr_list(list).iter().enumerate() {
                pairs.push((
                    i as u32,
                    gen_expr(b, ast, arg, types, defined_enums, pool, sink),
                ));
            }
            true
        }
        (None, Some(inits)) => {
            let field_names = &variants[vidx].1;
            for &(fname, arg) in ast.struct_field_inits(inits) {
                let Some(fidx) = field_names.iter().position(|&n| n == fname) else {
                    sink.emit(Diag::error(
                        ast.expr_span(arg),
                        DiagCode::UnknownField,
                        format!(
                            "variant '{}' of enum '{}' has no field '{}'",
                            pool.str(c.variant.name),
                            pool.str(enum_name),
                            pool.str(fname),
                        ),
                    ));
                    continue;
                };
                pairs.push((
                    fidx as u32,
                    gen_expr(b, ast, arg, types, defined_enums, pool, sink),
                ));
            }
            false
        }
        _ => unreachable!("parser guarantees exactly one variant-arg form"),
    };
    b.enum_lit(ty, vidx as u32, positional, &pairs, span)
}
