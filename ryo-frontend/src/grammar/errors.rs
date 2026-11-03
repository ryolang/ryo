//! Nominal error declarations, construction candidates, and try/catch grammar.

use super::*;
use chumsky::input::ValueInput;

pub(super) fn error_decl_parser<'a, I>() -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    just(Token::ErrorDecl)
        .ignore_then(
            select! { Token::Ident(name) => name }
                .map_with(|name, e: &mut Mx<'a, '_, I>| {
                    Ident::new(name, crate::spans::cspan(e.span()))
                })
                .validate(|name, e: &mut Mx<'a, '_, I>, emitter| {
                    if !is_uppercase_name(name.name, &e.state().pool) {
                        emitter.emit(Rich::custom(
                            e.span(),
                            ParseDiag::Message(
                                "error declaration names must use PascalCase".into(),
                            ),
                        ));
                    }
                    name
                }),
        )
        .then(enums::payload_parser())
        .map_with(|(name, payload), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            e.state().error_def(name, payload, span)
        })
        .boxed()
}

pub(super) fn error_family_decl_parser<'a, I>(
    enum_tail: impl Parser<'a, I, (Ident, EnumVariantDeclList), PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let visibility = choice((
        just(Token::Pub).to(DeclVisibility::Public),
        just(Token::Package).to(DeclVisibility::Package),
    ))
    .or_not()
    .map(|visibility| visibility.unwrap_or(DeclVisibility::Module));

    let family = visibility
        .then_ignore(just(Token::Errors))
        .then(enum_tail)
        .validate(
            |(visibility, (name, members)), e: &mut Mx<'a, '_, I>, emitter| {
                let name_span = SimpleSpan::new((), name.span.start..name.span.end);
                if !is_uppercase_name(name.name, &e.state().pool) {
                    emitter.emit(Rich::custom(
                        name_span,
                        ParseDiag::Message(
                            "error family declaration names must use PascalCase".into(),
                        ),
                    ));
                }
                if e.state().enum_variants(members).is_empty() {
                    emitter.emit(Rich::custom(
                        name_span,
                        ParseDiag::Message(format!(
                            "error family '{}' must declare at least one member",
                            e.state().pool.str(name.name)
                        )),
                    ));
                }
                (visibility, name, members)
            },
        )
        .map_with(|(visibility, name, members), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            e.state().error_family_def(name, members, visibility, span)
        });

    let member_modifier = choice((just(Token::Pub), just(Token::Package)))
        .map_with(|_, e: &mut Mx<'a, '_, I>| e.span());
    let invalid_body = skip_newlines()
        .ignore_then(just(Token::Indent))
        .ignore_then(
            none_of([Token::Pub, Token::Package, Token::Dedent])
                .repeated()
                .ignore_then(member_modifier)
                .then(none_of(Token::Dedent).repeated()),
        )
        .then_ignore(just(Token::Dedent));
    let invalid_member_visibility = visibility
        .then_ignore(just(Token::Errors))
        .then(
            select! { Token::Ident(name) => name }.map_with(|name, e: &mut Mx<'a, '_, I>| {
                Ident::new(name, crate::spans::cspan(e.span()))
            }),
        )
        .then_ignore(just(Token::Colon))
        .then(invalid_body)
        .validate(|parsed, _e: &mut Mx<'a, '_, I>, emitter| {
            emitter.emit(Rich::custom(
                parsed.1.0,
                ParseDiag::Message("error family members cannot have visibility modifiers".into()),
            ));
            parsed
        })
        .map_with(|_, e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            e.state().error_stmt(span)
        });

    family.or(invalid_member_visibility).boxed()
}

fn is_uppercase_name(name: StringId, pool: &InternPool) -> bool {
    pool.str(name)
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_uppercase)
}

fn reject_hidden_alternative<'a, 'b, I>(name: StringId, span: SimpleSpan, e: &mut Mx<'a, 'b, I>)
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    if matches!(e.state().pool.str(name), "Ok" | "Err") {
        e.emit(Rich::custom(
            span,
            ParseDiag::Message(
                "Ok and Err are compiler-internal alternatives, not source constructors".into(),
            ),
        ));
    }
}

/// Parse a plain call or an uppercase-led error-construction candidate.
/// Named parens preserve field names only for the candidate form.
pub(super) fn call_or_error_construct_parser<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| Ident::new(name, crate::spans::cspan(e.span())))
        .then(
            enums::paren_arg(expr)
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LParen), just(Token::RParen)),
        )
        .map_with(|(name, args), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            if !is_uppercase_name(name.name, &e.state().pool) {
                let args = enums::unwrap_paren_args(args, e);
                return e.state().call(name.name, &args, span);
            }

            reject_hidden_alternative(
                name.name,
                SimpleSpan::new((), name.span.start..name.span.end),
                e,
            );
            let positional = args
                .iter()
                .all(|arg| matches!(arg, enums::ParenArg::Expr(_)));
            let named = args
                .iter()
                .all(|arg| matches!(arg, enums::ParenArg::NamedSyntax { .. }));
            if !positional && !named {
                e.emit(Rich::custom(
                    SimpleSpan::new((), span.start..span.end),
                    ParseDiag::Message(
                        "positional and named error payload fields cannot be mixed".into(),
                    ),
                ));
            }

            if named && !positional {
                let start = e.state().struct_field_inits_len();
                for arg in args {
                    if let enums::ParenArg::NamedSyntax { name, value, .. } = arg {
                        e.state().push_struct_field_init_item(name, value);
                    }
                }
                let fields = e.state().struct_field_init_list_from(start);
                e.state().error_construct(
                    name,
                    Some(VariantArgs {
                        positional: None,
                        named: Some(fields),
                        named_parens: true,
                        mixed_fields: false,
                    }),
                    span,
                )
            } else {
                let values = args.into_iter().map(|arg| match arg {
                    enums::ParenArg::Expr(value) | enums::ParenArg::NamedSyntax { value, .. } => {
                        value
                    }
                });
                let start = e.state().expr_lists_len();
                for value in values {
                    e.state().push_expr_list_item(value);
                }
                let list = e.state().expr_list_from(start);
                e.state().error_construct(
                    name,
                    Some(VariantArgs {
                        positional: Some(list),
                        named: None,
                        named_parens: false,
                        mixed_fields: !positional && !named,
                    }),
                    span,
                )
            }
        })
}

/// Convert a finished bare uppercase-led identifier into a unit error
/// candidate. Postfix expressions remain ordinary field/method chains.
pub(super) fn finish_error_candidate<'a, 'b, I>(expr: ExprId, e: &mut Mx<'a, 'b, I>) -> ExprId
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let Some(name) = type_name_ident(expr, e) else {
        return expr;
    };
    reject_hidden_alternative(
        name.name,
        SimpleSpan::new((), name.span.start..name.span.end),
        e,
    );
    e.state().promote_ident_to_error_construct(expr)
}

pub(super) fn try_parser<'a, I>(
    operand: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    just(Token::Try)
        .ignore_then(operand)
        .map_with(|operand, e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            e.state().try_expr(operand, span)
        })
}

pub(super) struct CatchTail {
    pub binding: Option<Ident>,
    pub body: CatchBody,
    pub span: SimpleSpan,
}

/// `catch [as name]:` plus direct arms or a handler body. `arms` and
/// `arm_body` are the expression grammar's shared match-arm parsers.
pub(super) fn catch_tail_parser<'a, I>(
    arms: impl Parser<'a, I, MatchArmList, PExtra<'a>> + Clone + 'a,
    arm_body: impl Parser<'a, I, MatchArmBody, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, CatchTail, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let as_keyword =
        select! { Token::Ident(name) => name }.try_map_with(|name, e: &mut Mx<'a, '_, I>| {
            if e.state().pool.str(name) == "as" {
                Ok(())
            } else {
                Err(Rich::custom(
                    e.span(),
                    ParseDiag::Message("expected 'as <name>:' or ':' after 'catch'".into()),
                ))
            }
        });
    let binding = as_keyword
        .ignore_then(select! { Token::Ident(name) => name }.map_with(
            |name, e: &mut Mx<'a, '_, I>| Ident::new(name, crate::spans::cspan(e.span())),
        ))
        .or_not();
    let arms = arms.map(CatchBody::Arms);
    let handler = skip_newlines()
        .ignore_then(arm_body)
        .delimited_by(
            skip_newlines().ignore_then(just(Token::Indent)),
            just(Token::Dedent),
        )
        .map(CatchBody::Handler);

    just(Token::Catch)
        .ignore_then(binding)
        .then_ignore(just(Token::Colon))
        .then(arms.or(handler))
        .validate(|(binding, body), e: &mut Mx<'a, '_, I>, emitter| {
            if binding.is_none() && matches!(body, CatchBody::Handler(_)) {
                emitter.emit(Rich::custom(
                    e.span(),
                    ParseDiag::Message(
                        "bare catch requires direct error arms. Use a wildcard direct arm (`_: 0`) for binding-free handling, or `catch as err:` when shared error access is needed"
                            .into(),
                    ),
                ));
            }
            (binding, body)
        })
        .map_with(|(binding, body), e: &mut Mx<'a, '_, I>| CatchTail {
            binding,
            body,
            span: e.span(),
        })
        .boxed()
}
