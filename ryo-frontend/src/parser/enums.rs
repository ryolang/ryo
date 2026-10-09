//! Enum grammar (M11): `enum Name:` declarations with unit, tuple,
//! and named payloads; `EnumName.Variant(...)` / `{...}` construction;
//! and the attribute placement widened to enum definitions (the
//! I-193 slice). Split out of `parser.rs` to stay under the
//! file-length limit (R2). Variant *construction* does not live here:
//! it folds out of the shared expression grammar's postfix
//! method/field ops (see `type_name_ident` in `parser.rs`), so it
//! costs nothing on the plain-identifier hot path.

use super::*;
use chumsky::input::ValueInput;

/// The tail of an `enum` declaration after the `enum` keyword:
/// `Name:` plus an indented block of variant lines (M11). Yields the
/// name and the sealed variant list.
///
/// One payload rule covers both parenthesized kinds: the first element
/// decides — `Name: type` lines are named payloads, bare types are
/// tuple payloads — and mixing them inside one payload is a syntax
/// error. Payload decls push straight into the `struct_field_decls`
/// arena (tuple fields minted `"0"`, `"1"`, …) and variants into
/// `enum_variants` as they parse, so no intermediate `Vec` is
/// collected (I-152). An empty body parses; the empty-enum diagnostic
/// is astgen's, not the parser's.
///
/// The tail embeds `type_expr_parser()` subtrees, so the grammar
/// construction rule applies: the caller builds it once and threads
/// it into both enum-declaration rules (plain and attributed).
pub(super) fn enum_tail_parser<'a, I>()
-> impl Parser<'a, I, (Ident, EnumVariantDeclList), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // Named payload: `(` `name: type` list `)`. Pushed as parsed.
    let named_elem = select! { Token::Ident(name) => name }
        .then_ignore(just(Token::Colon))
        .then(type_expr_parser())
        .map_with(|(name, ty), e: &mut Mx<'a, '_, I>| {
            e.state().push_struct_field_decl(name, ty);
        });
    // `collect` (not `to(())`): an IterParser collection runs its
    // items in Emit mode — a bare `repeated()`-as-parser validates in
    // Check mode, which never runs the push closures above. Same for
    // every `.map(|_| …)` below versus `.to(…)`: `to` validates its
    // inner in Check mode.
    let named_payload = named_elem
        .clone()
        .then(
            just(Token::Comma)
                .ignore_then(named_elem)
                .repeated()
                .collect::<Vec<_>>(),
        )
        .then_ignore(just(Token::Comma).or_not())
        .then_ignore(just(Token::RParen))
        .map(|_| VariantKind::Named);

    // Tuple payload: `(` bare-type list `)`. The first element is
    // `"0"`; the running count rides the fold accumulator so each
    // subsequent element mints its own positional name.
    let tuple_first = type_expr_parser()
        .map_with(|ty, e: &mut Mx<'a, '_, I>| {
            let name = positional_field_name(0, &mut e.state().pool);
            e.state().push_struct_field_decl(name, ty);
        })
        .map(|()| 1usize);
    let tuple_payload = tuple_first
        .foldl_with(
            just(Token::Comma)
                .ignore_then(type_expr_parser())
                .repeated(),
            |next, ty, e: &mut Mx<'a, '_, I>| {
                let index = i64::try_from(next).expect("tuple payload arity fits i64");
                let name = positional_field_name(index, &mut e.state().pool);
                e.state().push_struct_field_decl(name, ty);
                next + 1
            },
        )
        .then_ignore(just(Token::Comma).or_not())
        .then_ignore(just(Token::RParen))
        .map(|_| VariantKind::Tuple);

    // Variant line: `Name` (unit) or `Name(...)`; the payload decls
    // land between the recorded start offset and the seal.
    let payload = just(Token::LParen)
        .ignore_then(choice((named_payload, tuple_payload)))
        .or_not();
    let variant = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, e.span()))
        .then(empty().map_with(|_, e: &mut Mx<'a, '_, I>| e.state().struct_field_decls_len()))
        .then(payload)
        .map_with(|((name, start), kind), e: &mut Mx<'a, '_, I>| {
            let payload = EnumPayload {
                kind: kind.unwrap_or(VariantKind::Unit),
                fields: e.state().struct_field_decl_list_from(start),
            };
            let span = e.span();
            e.state().push_enum_variant(name, payload, span);
        });

    // Variant lines mirror struct field lines: newline-separated, the
    // block-final variant may sit directly against the `Dedent`.
    // `collect` keeps the repetition in Emit mode (see `named_payload`).
    let variant_line = variant.then_ignore(require_newlines().or(peek_terminator(Token::Dedent)));
    let body = skip_newlines()
        .ignore_then(variant_line.repeated().collect::<Vec<_>>())
        .delimited_by(
            skip_newlines().ignore_then(just(Token::Indent)),
            just(Token::Dedent),
        )
        .map(Some);
    let no_body = empty()
        .and_is(require_newlines().then_ignore(just(Token::Indent).not()))
        .to(None);

    select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, e.span()))
        .then_ignore(just(Token::Colon))
        .then(empty().map_with(|_, e: &mut Mx<'a, '_, I>| e.state().enum_variants_len()))
        .then(body.or(no_body))
        .map_with(|((name, start), _), e: &mut Mx<'a, '_, I>| {
            (name, e.state().enum_variant_list_from(start))
        })
        .boxed()
}

/// One element of a parenthesized argument list (a call, or the M11
/// variant-construction parens that share the method-call shape):
/// either an ordinary expression, or a recovered `name = value`
/// named-argument attempt. The marker exists so the enclosing op can
/// emit the targeted diagnostic (`ParseDiag::NamedArgInParens`) with
/// full flavor context — the enum and variant names are only known
/// when the op folds — while passing the VALUE through as a
/// positional argument: the value stays in the tree (its own type
/// errors still surface) and the no-orphan invariant holds.
pub(super) enum ParenArg {
    Expr(ExprId),
    NamedSyntax { span: SimpleSpan, value: ExprId },
}

/// One argument of a parenthesized list. The `name = value` recovery
/// is tried FIRST: no valid Ryo expression starts with `Ident`
/// followed by `=` (assignment is a statement form; `==` is a single
/// token), so the recovery can only claim input the ordinary `expr`
/// alternative would fail on anyway — `expr` parses the bare name
/// and then strands the `=`. The value expression parses normally;
/// the name is consumed onto the marker.
pub(super) fn paren_arg<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, ParenArg, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    field_key()
        .then_ignore(just(Token::Assign))
        .then(expr.clone())
        .map_with(
            |(_name, value), e: &mut Mx<'a, '_, I>| ParenArg::NamedSyntax {
                span: e.span(),
                value,
            },
        )
        .or(expr.map(ParenArg::Expr))
}

/// An `enum` declaration: `enum Name:` plus the variant block (M11).
/// `enum_tail` is threaded in by the caller (grammar construction
/// rule: built once per program grammar, shared with the attributed
/// form).
pub(super) fn enum_decl_parser<'a, I>(
    enum_tail: impl Parser<'a, I, (Ident, EnumVariantDeclList), PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    just(Token::Enum)
        .ignore_then(enum_tail)
        .map_with(|(name, variants), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.state()
                .enum_def(name, variants, EnumAttrs::default(), span)
        })
        .boxed()
}

/// A type declaration (`struct` or `enum`) preceded by one or more
/// attribute groups (M9.1; widened to enums in M11 — the I-193 slice).
/// Only `#[derive(Eq)]` and `#[repr(C)]` are known; anything else
/// emits `ParseDiag::UnknownAttribute` naming the attribute.
/// `#[repr(C)]` on an `enum` emits `ParseDiag::ReprCOnEnum` naming the
/// allowed attribute. Attributes followed by something other than a
/// type declaration emit `ParseDiag::MisplacedAttribute` and recover
/// to an `Error` node, so the misplaced line reports once and the
/// following statement still parses.
///
/// `enum_tail` is threaded in by the caller (grammar construction
/// rule: built once per program grammar, shared with the plain
/// `enum_decl_parser`).
pub(super) fn attributed_type_decl_parser<'a, I>(
    enum_tail: impl Parser<'a, I, (Ident, EnumVariantDeclList), PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    #[derive(Clone)]
    enum AttrTarget {
        Struct(Ident, Vec<(StringId, TypeExpr)>),
        Enum(Ident, EnumVariantDeclList),
    }

    // Attribute groups stack vertically (blank lines tolerated); the
    // newlines before each group and before the declaration belong to
    // this alternative only when what follows is another group or a
    // declaration keyword — chumsky rewinds a failed alternative, so
    // on the misplaced path the statement list keeps the line-ending
    // newline and recovers cleanly.
    let attrs = skip_newlines()
        .ignore_then(attr_group_parser())
        .repeated()
        .at_least(1)
        .collect::<Vec<_>>();

    let target = choice((
        skip_newlines()
            .ignore_then(just(Token::Struct))
            .ignore_then(struct_tail_parser())
            .map(|(name, fields)| Some(AttrTarget::Struct(name, fields))),
        skip_newlines()
            .ignore_then(just(Token::Enum))
            .ignore_then(enum_tail)
            .map(|(name, variants)| Some(AttrTarget::Enum(name, variants))),
        empty().to(None),
    ));

    attrs
        .then(target)
        .validate(|(attrs, target), e: &mut Mx<'a, '_, I>, emitter| {
            // The parser is pool-less, so the attribute vocabulary is
            // recognized by the fixed well-known ids (see
            // `StringId::ATTR_*`): exactly `derive(Eq)` / `repr(C)`.
            let mut bits = StructAttrs::default();
            let mut repr_span = None;
            let mut all_known = true;
            for (name, args, span) in &attrs {
                let recognized = if *name == StringId::ATTR_DERIVE
                    && args.as_deref() == Some(&[StringId::ATTR_EQ])
                {
                    bits.derive_eq = true;
                    true
                } else if *name == StringId::ATTR_REPR
                    && args.as_deref() == Some(&[StringId::ATTR_C])
                {
                    bits.repr_c = true;
                    repr_span = Some(*span);
                    true
                } else {
                    false
                };
                if !recognized {
                    all_known = false;
                    emitter.emit(Rich::custom(
                        *span,
                        ParseDiag::UnknownAttribute {
                            name: *name,
                            args: args.clone().unwrap_or_default(),
                        },
                    ));
                }
            }
            // `#[repr(C)]` pins struct layout; enums do not support it
            // in M11 — the diagnostic names what an enum MAY carry.
            if bits.repr_c && matches!(target, Some(AttrTarget::Enum(..))) {
                emitter.emit(Rich::custom(
                    repr_span.unwrap_or_else(|| e.span()),
                    ParseDiag::ReprCOnEnum,
                ));
            }
            // Misplaced only piles on when the attributes themselves
            // were fine — an unknown attribute already explains the
            // line.
            if target.is_none() && all_known {
                emitter.emit(Rich::custom(e.span(), ParseDiag::MisplacedAttribute));
            }
            (bits, target)
        })
        .map_with(|(bits, target), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            match target {
                Some(AttrTarget::Struct(name, fields)) => {
                    e.state().struct_def(name, &fields, bits, span)
                }
                Some(AttrTarget::Enum(name, variants)) => e.state().enum_def(
                    name,
                    variants,
                    EnumAttrs {
                        derive_eq: bits.derive_eq,
                    },
                    span,
                ),
                None => e.state().error_stmt(span),
            }
        })
        .boxed()
}
