//! Enum grammar (M11): `enum Name:` declarations with unit, tuple,
//! and named payloads; `EnumName.Variant(...)` / `{...}` construction;
//! and the attribute placement widened to enum definitions (the
//! I-193 slice). Split out of `grammar.rs` to stay under the
//! file-length limit (R2). Variant *construction* does not live here:
//! it folds out of the shared expression grammar's postfix
//! method/field ops (see `type_name_ident` in `grammar.rs`), so it
//! costs nothing on the plain-identifier hot path.

use super::*;
use chumsky::input::ValueInput;

/// Unwrap a parsed paren-arg list for a plain call: recovered
/// `name = value` args emit the method-flavor diagnostic (a call has
/// no variant to name) and pass their value through as a positional
/// argument.
pub(super) fn unwrap_paren_args<'a, 'b, I>(
    args: Vec<ParenArg>,
    e: &mut Mx<'a, 'b, I>,
) -> Vec<ExprId>
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    args.into_iter()
        .map(|arg| match arg {
            ParenArg::Expr(id) => id,
            ParenArg::NamedSyntax { span, value, .. } => {
                e.emit(Rich::custom(
                    span,
                    ParseDiag::NamedArgInParens {
                        enum_name: None,
                        variant: None,
                    },
                ));
                value
            }
        })
        .collect()
}

/// `EnumName.Variant{...}` — named variant construction (M11).
/// Sits between the method op and `field_op` in the postfix choice:
/// like the method op it requires more than `.name`, so `field_op`
/// backtracks here when no `{` follows. The receiver gate reads
/// `postfix_head_enum` (set by the postfix fold) BEFORE the `{` is
/// consumed: for a lowercase receiver the alternative fails after
/// `.name` and `field_op` claims the field access, leaving `{...}`
/// unconsumed — `obj.field{...}` keeps its historical
/// field-access-then-garbage parse (the lowercase pin in
/// `grammar/enum_tests.rs`). `try_map_with` runs the gate in both
/// emit and check mode.
///
/// The content has one recovery: `Shape.Rectangle{1.0, 2.0}` —
/// values without `name =` prefixes. Named content is tried first
/// (an init list must open with a field key followed by `=`, so the
/// positional reading can only claim input the named reading fails
/// on), and `delimited_by` sits inside each alternative so a failed
/// named reading backtracks the whole `{...}`. The fold emits E0127
/// (naming the enum from the receiver itself — nested expressions
/// overwrite the hint slot) and promotes the values as positional
/// arguments.
///
/// Like the other `.`-led ops this parses the tail after the `.`; the
/// postfix fold consumes the shared `.` (see `postfix_parser`).
pub(super) fn named_construct_op<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
    field_init: impl Parser<'a, I, (StringId, ExprId), PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, PostfixOp, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    enum BraceContent {
        Named(Vec<(StringId, ExprId)>),
        Positional(Vec<ExprId>),
    }
    let named_content = field_init
        .clone()
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
        .map(BraceContent::Named);
    let positional_content = expr
        .clone()
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<ExprId>>()
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
        .map(BraceContent::Positional);
    select! { Token::Ident(name) => name }
        .try_map_with(|name, e: &mut Mx<'a, '_, I>| {
            if e.state().postfix_head_enum.is_some() {
                Ok(Ident::new(name, crate::spans::cspan(e.span())))
            } else {
                Err(Rich::custom(e.span(), ParseDiag::ExpectedEnumTypeName))
            }
        })
        .then(named_content.or(positional_content))
        .map_with(|(variant, content), e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            match content {
                BraceContent::Named(inits) => PostfixOp::NamedConstruct(variant, inits, span),
                BraceContent::Positional(values) => {
                    PostfixOp::RecoveredPositionalBraces(variant, values, span)
                }
            }
        })
}

/// `Shape.Circle 5.0` — a variant construction with the argument
/// list's parens missing: an expression-start token follows the
/// variant name on the same line (a Newline/Dedent token would sit
/// between otherwise, failing the trailing `expr`). Gated on the
/// enum-candidate receiver hint so `obj.field <anything>` keeps its
/// historical parse; sits before `field_op` so the recovery claims
/// the field access when it fires.
///
/// The payload may not begin with `-`: that token continues a binary
/// expression (`CFG.size - 1` on an uppercase-named value), and the
/// recovery claiming it would rewrite a subtraction into a phantom
/// construct — the guard fails the alternative and `field_op` keeps
/// the field access, so the binary op parses unchanged.
pub(super) fn missing_args_op<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, PostfixOp, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(name) => name }
        .try_map_with(|name, e: &mut Mx<'a, '_, I>| {
            if e.state().postfix_head_enum.is_some() {
                Ok(Ident::new(name, crate::spans::cspan(e.span())))
            } else {
                Err(Rich::custom(e.span(), ParseDiag::ExpectedEnumTypeName))
            }
        })
        .then(just(Token::Sub).not().ignore_then(expr))
        .map_with(|(variant, arg), e: &mut Mx<'a, '_, I>| {
            PostfixOp::RecoveredMissingParens(variant, arg, e.span())
        })
}

/// A unit, positional, or named payload. The first parenthesized element
/// decides the payload kind; fields are pushed directly into the shared
/// declaration arena and sealed after the list finishes.
pub(super) fn payload_parser<'a, I>() -> impl Parser<'a, I, EnumPayload, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let named_elem = select! { Token::Ident(name) => name }
        .then_ignore(just(Token::Colon))
        .then(type_expr_parser())
        .map_with(|(name, ty), e: &mut Mx<'a, '_, I>| {
            e.state().push_struct_field_decl(name, ty);
        });
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

    let payload = just(Token::LParen)
        .ignore_then(choice((named_payload, tuple_payload)))
        .or_not();
    empty()
        .map_with(|_, e: &mut Mx<'a, '_, I>| e.state().struct_field_decls_len())
        .then(payload)
        .map_with(|(start, kind), e: &mut Mx<'a, '_, I>| EnumPayload {
            kind: kind.unwrap_or(VariantKind::Unit),
            fields: e.state().struct_field_decl_list_from(start),
        })
}

/// The tail of an `enum` declaration after the `enum` keyword:
/// `Name:` plus an indented block of variant lines (M11). Yields the
/// name and the sealed variant list. An empty body parses; the
/// empty-enum diagnostic is astgen's, not the parser's.
pub(super) fn enum_tail_parser<'a, I>()
-> impl Parser<'a, I, (Ident, EnumVariantDeclList), PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let variant = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, crate::spans::cspan(e.span())))
        .then(payload_parser())
        .map_with(|(name, payload), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
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

    // `enum Color: Red, Green, Blue` — the comma form. Variants are
    // declared one per line in the indented block; there is no comma
    // form. Recover the line by declaring the variants anyway (unit,
    // no payloads) with one targeted diagnostic. Requires at least
    // one comma, so `enum Color: Red` — a single variant name on the
    // header line — keeps its historical (failing) parse. The token
    // stream itself proves single-line: a Newline between the names
    // breaks the `Comma`-separated repetition.
    let comma_variant = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, crate::spans::cspan(e.span())))
        .map_with(|name, e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            let start = e.state().struct_field_decls_len();
            let fields = e.state().struct_field_decl_list_from(start);
            e.state().push_enum_variant(
                name,
                EnumPayload {
                    kind: VariantKind::Unit,
                    fields,
                },
                span,
            );
        });
    let comma_variants = comma_variant
        .then(
            just(Token::Comma)
                .ignore_then(comma_variant)
                .repeated()
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .map_with(|_, e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.emit(Rich::custom(span, ParseDiag::CommaSeparatedVariants));
            // Same shape as `no_body`: the variants were pushed onto
            // the arena above; the seal below reads the window.
            None
        });

    select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, crate::spans::cspan(e.span())))
        .then_ignore(just(Token::Colon))
        .then(empty().map_with(|_, e: &mut Mx<'a, '_, I>| e.state().enum_variants_len()))
        .then(body.or(no_body).or(comma_variants))
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
    NamedSyntax {
        name: StringId,
        span: SimpleSpan,
        value: ExprId,
    },
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
            |(name, value), e: &mut Mx<'a, '_, I>| ParenArg::NamedSyntax {
                name,
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
            let span = crate::spans::cspan(e.span());
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
            let span = crate::spans::cspan(e.span());
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

impl PostfixOp {
    /// Re-stamp the op's span. The `.`-led ops parse as tails after a
    /// shared `.` (see `postfix_parser`); the outer parse supplies the
    /// `.`-inclusive span they carried when each op owned its dot.
    pub(super) fn with_span(self, new: SimpleSpan) -> Self {
        match self {
            PostfixOp::Method(name, args, _) => PostfixOp::Method(name, args, new),
            PostfixOp::NamedConstruct(name, inits, _) => {
                PostfixOp::NamedConstruct(name, inits, new)
            }
            PostfixOp::RecoveredPositionalBraces(name, values, _) => {
                PostfixOp::RecoveredPositionalBraces(name, values, new)
            }
            PostfixOp::RecoveredMissingParens(name, arg, _) => {
                PostfixOp::RecoveredMissingParens(name, arg, new)
            }
            PostfixOp::Field(field, _) => PostfixOp::Field(field, new),
            PostfixOp::Missing => PostfixOp::Missing,
            PostfixOp::Slice(lo, hi, _) => PostfixOp::Slice(lo, hi, new),
            PostfixOp::Index(index, _) => PostfixOp::Index(index, new),
        }
    }
}
