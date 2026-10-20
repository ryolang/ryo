//! Match-expression grammar (M12): `match scrutinee:` plus one or more
//! `pattern: body` arms, and the match-arm pattern grammar — literals
//! (`42`, `-1`, `"quit"`, `true`), enum variants (`Option.Some(v)`,
//! `Red`, `Coords{x=px}`), wildcards, and the M10 anonymous/positional
//! shapes widened with literal field targets (`{x=0, y=_}`, `(0, _)`).
//! Split out of `grammar.rs` (R2). Match is an EXPRESSION atom: it
//! nests anywhere the shared expression parser is valid.
//!
//! Arm bodies come in three shapes, tried in order:
//!
//! - expression arm (`Some(v): v + 1`) — the expression must reach a
//!   line end, so an `= value` tail (an assignment statement) fails
//!   this alternative and falls through to the statement arm;
//! - single-line statement arm (`Err(e): return e`) — wraps into a
//!   one-element `StmtList`;
//! - block arm (`Some(v):` + indented statements) — normalizes to a
//!   `StmtList` like the single-line form, so downstream lowering
//!   never needs a `return`-as-expression shape.
//!
//! Arm lines mirror `struct_tail_parser`'s field lines: newline-
//! separated, blank lines tolerated, the block-final arm may sit
//! directly against the `Dedent`.
//!
//! Pattern grammar notes:
//!
//! - `_` lexes as an ordinary identifier and is recognized by name
//!   (the M10 rule);
//! - a whole-arm bare lowercase identifier is rejected with a targeted
//!   diagnostic (did-you-mean a variant pattern) — bindings live in
//!   payload positions — while nested binds (`(a, b)`, `{x, y}`)
//!   stay legal, so the rejection lives on the arm, not the pattern;
//! - variant payload literals are deferred with nested patterns:
//!   paren args bind or skip (`Circle(radius)`, `Pair(a, b)`), brace
//!   args also allow puns (`Coords{x}`) but a `name = literal` target
//!   is diagnosed as not-yet-supported and recovered as a wildcard;
//! - a leading `{`/`(` disambiguates to the anonymous/positional
//!   shapes — there is no struct-literal meaning in pattern position.

use super::*;
use chumsky::input::ValueInput;

/// `match scrutinee:` + indented arm block (M12). Yields the
/// expression id. Built once inside `expression_parser`'s atom choice
/// and handed the shared `expr` handle — no new
/// `expression_parser()` call site. The arm statement grammar is
/// `body_statement_parser` over the same handle, so statement arms
/// (return/break/assignments, if/while/for, nested matches) parse
/// exactly as they do in a function body.
pub(super) fn match_expression_parser<'a, I>(
    expr: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    let stmt = body_statement_parser(expr.clone());

    // ---- Patterns ----
    //
    // `Recursive` because positional elements are full patterns
    // (`(a, (b, c))`).
    let mut pat = Recursive::declare();

    let ident = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| (name, e.span()));

    // `_` — the wildcard, recognized by name (gate: the identifier
    // must be spelled `_`; the error rides a discarded choice
    // alternative, like the M11 receiver gates).
    let wildcard = ident
        .try_map_with(|(name, span), e: &mut Mx<'a, '_, I>| {
            if e.state().pool.str(name) == "_" {
                Ok(())
            } else {
                Err(Rich::custom(span, ParseDiag::ExpectedEnumTypeName))
            }
        })
        .map_with(|_, e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            e.state().pattern_wildcard(span)
        });

    // Literal values: `-` folds into the int at parse time (the lexer
    // marks `i64::MIN` as `IntLitMin`, grammatical only after `-`).
    let neg_int = just(Token::Sub)
        .ignore_then(choice((
            just(Token::IntLitMin).to(i64::MIN),
            select! { Token::IntLit(n) => n }.map(|n| {
                n.checked_neg()
                    .expect("lexer emits i64::MIN as IntLitMin, so a plain IntLit always negates")
            }),
        )))
        .map(LitPattern::Int);
    let lit_value = choice((
        neg_int,
        select! { Token::IntLit(n) => n }.map(LitPattern::Int),
        select! { Token::StrLit(id) => id }.map(LitPattern::Str),
        choice((
            just(Token::True).to(LitPattern::Bool(true)),
            just(Token::False).to(LitPattern::Bool(false)),
        )),
    ));
    let literal = lit_value.map_with(|lit, e: &mut Mx<'a, '_, I>| {
        let span = crate::spans::cspan(e.span());
        e.state().literal_pattern(lit, span)
    });

    // One field of an anonymous pattern: a pun (`q`), `name = ident`
    // (rename/bind), `name = literal` (M12), or `name = _` (wildcard).
    // A field spelled `_` binds nothing; renaming it (`{_ = x}`) is
    // rejected — the M10 rule.
    let field_target = choice((
        ident.map_with(|(name, _), e: &mut Mx<'a, '_, I>| {
            if e.state().pool.str(name) == "_" {
                PatternFieldKind::Wildcard
            } else {
                PatternFieldKind::Rename(name)
            }
        }),
        lit_value.map(PatternFieldKind::Literal),
    ));
    let field = ident
        .then(just(Token::Assign).ignore_then(field_target).or_not())
        .try_map_with(|((name, span), target), e: &mut Mx<'a, '_, I>| {
            let pool = &e.state().pool;
            if pool.str(name) == "_" && target.is_some() {
                return Err(Rich::custom(
                    span,
                    ParseDiag::Message("wildcard field `_` binds nothing; drop the `= ...`".into()),
                ));
            }
            let kind = match target {
                None if pool.str(name) == "_" => PatternFieldKind::Wildcard,
                None => PatternFieldKind::Pun,
                Some(kind) => kind,
            };
            Ok(PatternField { name, kind })
        });
    let anon = field
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .at_least(1)
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
        .map_with(|fields, e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            e.state().pattern_anon(&fields, span)
        });

    // Positional pattern: the comma after the first element is
    // mandatory — `(a)` is never a pattern, the same rule as M10.
    let positional = pat
        .clone()
        .then_ignore(just(Token::Comma))
        .then(
            pat.clone()
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>(),
        )
        .delimited_by(just(Token::LParen), just(Token::RParen))
        .map_with(|(first, rest), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            let mut elems = Vec::with_capacity(rest.len() + 1);
            elems.push(first);
            elems.extend(rest);
            e.state().pattern_positional(&elems, span)
        });

    // Variant patterns. Paren args bind or skip positionally; brace
    // args bind by name (pun, `= ident`, `= _`). A `= literal` target
    // in braces is payload-literal work deferred with nested patterns:
    // diagnosed and recovered as a wildcard so the arm still parses.
    let paren_arg = ident.map_with(|(name, _), e: &mut Mx<'a, '_, I>| {
        let target = if e.state().pool.str(name) == "_" {
            PatternVariantArgTarget::Wildcard
        } else {
            PatternVariantArgTarget::Bind(name)
        };
        PatternVariantArg { name: None, target }
    });
    let paren_args = paren_arg
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LParen), just(Token::RParen))
        .map_with(|args, e: &mut Mx<'a, '_, I>| e.state().variant_pattern_args(true, &args));

    let named_target = choice((
        lit_value.map_with(|_, e: &mut Mx<'a, '_, I>| {
            let span = e.span();
            e.emit(Rich::custom(
                span,
                ParseDiag::Message(
                    "literal field patterns inside variant braces are not supported yet".into(),
                ),
            ));
            PatternVariantArgTarget::Wildcard
        }),
        ident.map_with(|(name, _), e: &mut Mx<'a, '_, I>| {
            if e.state().pool.str(name) == "_" {
                PatternVariantArgTarget::Wildcard
            } else {
                PatternVariantArgTarget::Bind(name)
            }
        }),
    ));
    let brace_arg = ident
        .then(just(Token::Assign).ignore_then(named_target).or_not())
        .map(|((name, _), target)| {
            // A pun binds the field's own name.
            let target = target.unwrap_or(PatternVariantArgTarget::Bind(name));
            PatternVariantArg {
                name: Some(name),
                target,
            }
        });
    let brace_args = brace_arg
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
        .map_with(|args, e: &mut Mx<'a, '_, I>| e.state().variant_pattern_args(false, &args));

    let args = choice((paren_args, brace_args)).or_not();

    // `A.B` (qualified) or `B` (unqualified, gate: uppercase-led per
    // the spec §1 PascalCase convention — a lowercase-led bare
    // identifier is a bind for nested positions and never reaches
    // here whole). The gate error rides a discarded alternative.
    let variant = choice((
        ident
            .then_ignore(just(Token::Dot))
            .then(ident)
            .map(|((enum_name, _), (variant, _))| (Some(enum_name), variant)),
        ident.try_map_with(|(name, span), e: &mut Mx<'a, '_, I>| {
            let is_variant = e
                .state()
                .pool
                .str(name)
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_uppercase);
            if is_variant {
                Ok((None, name))
            } else {
                Err(Rich::custom(span, ParseDiag::ExpectedEnumTypeName))
            }
        }),
    ))
    .then(args)
    .map_with(|((enum_name, variant), args), e: &mut Mx<'a, '_, I>| {
        let span = crate::spans::cspan(e.span());
        e.state().variant_pattern(enum_name, variant, args, span)
    });

    // A bare binding — legal in NESTED positions (positional
    // elements, field targets go through their own sub-parsers), so
    // the whole-arm rejection lives on the arm line, not here.
    let bind = ident.map_with(|(name, span), e: &mut Mx<'a, '_, I>| {
        e.state().pattern_bind(
            Ident::new(name, crate::spans::cspan(span)),
            crate::spans::cspan(span),
        )
    });

    pat.define(choice((wildcard, literal, anon, positional, variant, bind)).boxed());

    // ---- Arms ----
    //
    // A whole-arm bare binding (`foo: 1`) is the classic mistake:
    // patterns test shapes, bindings live in payload positions.
    // Diagnose with a did-you-mean variant note and recover as the
    // binding so the rest of the arm still parses (the diagnostic
    // fails the build).
    let arm_pattern = pat.validate(|pat_id, e: &mut Mx<'a, '_, I>, emitter| {
        if let PatternKind::Bind(ident) = e.state().pattern(pat_id).kind {
            let name = e.state().pool.str(ident.name).to_string();
            emitter.emit(Rich::custom(
                e.span(),
                ParseDiag::Message(format!(
                    "`{name}` is not a pattern — did you mean a variant pattern like \
                     `Some({name})`? (use `_` to match any value)"
                )),
            ));
        }
        pat_id
    });

    let tail = require_newlines().or(peek_terminator(Token::Dedent));
    let arm_body = choice((
        // Expression arm — a call like `io.print(...)` lands here.
        expr.clone()
            .then_ignore(tail.clone())
            .map(MatchArmBody::Expr),
        // Single-line statement arm (`return e`, assignments) — wraps
        // into a one-element StmtList.
        stmt.clone()
            .then_ignore(tail.clone())
            .map_with(|s, e: &mut Mx<'a, '_, I>| {
                MatchArmBody::Stmts(e.state().push_stmt_list(&[s]))
            }),
        // Block arm — the indented statement list. The newline run
        // after the inner block's `Dedent` always separates this arm
        // from the next arm (or the arm block's own `Dedent`), so it
        // is absorbed here: the single-line arms' `tail` handles it
        // via `require_newlines`, but this arm has no line tail of
        // its own.
        skip_newlines()
            .ignore_then(statement_list(stmt.clone(), peek_terminator(Token::Dedent)))
            .delimited_by(
                skip_newlines().ignore_then(just(Token::Indent)),
                just(Token::Dedent),
            )
            .then_ignore(skip_newlines())
            .map_with(|stmts, e: &mut Mx<'a, '_, I>| {
                MatchArmBody::Stmts(e.state().push_stmt_list(&stmts))
            }),
    ));
    let arm_line = arm_pattern
        .then_ignore(just(Token::Colon))
        .then(arm_body)
        .map_with(|(pattern, body), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            e.state().match_arm(pattern, body, span)
        });

    // Arm lines mirror `struct_tail_parser`'s field lines: at least
    // one arm, newline-separated, blank lines tolerated, the
    // block-final arm may sit directly against the `Dedent`.
    let arms = skip_newlines()
        .ignore_then(arm_line.repeated().at_least(1).collect::<Vec<_>>())
        .delimited_by(
            skip_newlines().ignore_then(just(Token::Indent)),
            just(Token::Dedent),
        );

    // A match nested as a call argument closes its arm block one line
    // before the call's `)`: the preprocessor emits the arm block's
    // `Dedent` before that line's newline, leaving `<Dedent> <Newline>
    // )`. Absorb the newline run only when a `)` directly follows
    // (verified zero-width via `rewind`); a failed first alternative
    // rewinds (consuming nothing) so statement-position matches keep
    // their line tail.
    let absorb_close_paren_newline = choice((
        skip_newlines().then_ignore(just(Token::RParen).rewind()),
        empty(),
    ));

    just(Token::Match)
        .ignore_then(expr.clone())
        .then_ignore(just(Token::Colon))
        .then(arms)
        .then_ignore(absorb_close_paren_newline)
        .map_with(|(scrutinee, arms), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            e.state().match_expr(scrutinee, &arms, span)
        })
        .boxed()
}
