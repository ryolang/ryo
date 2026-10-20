//! Destructuring-statement grammar (M10): `pattern = value` in its
//! paren-less, parenthesized, and braced forms. Split out of
//! `grammar.rs` to stay under the file-length limit (R2). The pattern
//! shapes themselves are shared with match arms (M12) through the
//! `PatternKind`/`PatternField` AST nodes — this module binds them
//! only (no literals or variants: those parse solely inside match
//! arms, see `grammar/match_.rs`).

use super::*;

/// Push a bind-or-wildcard pattern node: `_` lexes as an ordinary
/// identifier, so the wildcard is recognized by name here.
fn bind_pattern(state: &mut ParseState, name: StringId, span: ryo_core::span::Span) -> PatternId {
    if state.pool.str(name) == "_" {
        state.pattern_wildcard(span)
    } else {
        state.pattern_bind(Ident::new(name, span), span)
    }
}

/// A destructuring statement `pattern = value` (M10). One parse per
/// form, ordered choice, no `attempt()` — the alternatives are
/// disjoint after their first token and a failing alternative rewinds
/// without pushing arena nodes (the same mechanism as the existing
/// Ident-led statements):
///
/// - paren-less `a, b = e` — bindings only; the comma after the first
///   binding distinguishes the form from plain `a = e`, which stays
///   `assign_or_decl` (`destructure` is tried first in the statement
///   dispatch, so the paren-less form claims `Ident ,` lines before
///   `assign_or_decl` sees them);
/// - parenthesized `(a, b) = e`, `(a,) = e` — a positional pattern;
///   the mandatory comma inside the parens distinguishes the form
///   from a parenthesized-expression statement, and nested elements
///   like `(a, (b, c))` parse through the recursive pattern handle;
/// - braced `{q, r} = e`, `{x = quot} = e` — an anonymous field
///   pattern; the `= value` after the closing brace distinguishes it
///   from an anonymous struct literal expression statement, so bare
///   `{x=1}` keeps its meaning (a failed `shaped` alternative rewinds
///   before the `=` and `expr_stmt` claims the line);
/// - `(a) = x` — a one-element parenthesized pattern without the
///   trailing comma is unspellable (`(a)` is an expression), so the
///   shape parses whole and reports the targeted
///   `SingleElemDestructuring` diagnostic, recovering the line to an
///   `Error` statement — the `MisplacedAttribute` shape, so the
///   message stands alone instead of degrading into a generic
///   "expected newline" tail error.
///
/// Note: when `shaped` parses a well-formed pattern that turns out
/// not to be followed by `=` (a bare `(a, b)` or `{x=1}` expression
/// statement), the pattern nodes pushed before the failure stay in
/// the arenas as unreachable orphans — deliberate (the `Inspector`
/// hooks on `ParseState` are no-ops; see `statement_list`).
///
/// `value` is the right-hand-side expression grammar: the caller's
/// shared expression parser (see `top_level_statement_parser`), so
/// this rule does not construct — and later drop — full expression
/// grammars of its own on every `program_parser()` call.
pub(super) fn destructure_stmt_parser<'a, I>(
    value: impl Parser<'a, I, ExprId, PExtra<'a>> + Clone + 'a,
) -> impl Parser<'a, I, StmtId, PExtra<'a>> + Clone + 'a
where
    I: ValueInput<'a, Token = Token, Span = SimpleSpan>,
{
    // The recursive pattern handle. `Recursive` (declare/define, what
    // `recursive()` is sugar for) is what lets the anon and
    // positional sub-parsers be shared between the full pattern
    // grammar — positional elements recurse through `pat` — and the
    // statement forms below.
    let mut pat = Recursive::declare();

    // A bare binding target: an identifier, or `_` (by name). Yields
    // the name with its span; the pattern node is pushed by the
    // consumer, which decides between `bind` and the paren-less form.
    let bind_target = select! { Token::Ident(name) => name }
        .map_with(|name, e: &mut Mx<'a, '_, I>| (name, e.span()));

    let bind = bind_target.map_with(|(name, span), e: &mut Mx<'a, '_, I>| {
        bind_pattern(e.state(), name, crate::spans::cspan(span))
    });

    // One field of an anonymous pattern: a pun (`q`), a wildcard
    // field (`_` or `y = _`), or a rename (`x = quot`). Renaming a
    // field spelled `_` (`{_ = x}`) is rejected: `_` binds nothing.
    // Yields the scalar `PatternField`; the arena push happens at
    // `pattern_anon`.
    let field = bind_target
        .then(just(Token::Assign).ignore_then(bind_target).or_not())
        .try_map_with(|((name, span), rename), e: &mut Mx<'a, '_, I>| {
            let pool = &e.state().pool;
            if pool.str(name) == "_" && rename.is_some() {
                return Err(Rich::custom(
                    span,
                    ParseDiag::Message("wildcard field `_` binds nothing; drop the `= ...`".into()),
                ));
            }
            let kind = match rename {
                None if pool.str(name) == "_" => PatternFieldKind::Wildcard,
                None => PatternFieldKind::Pun,
                Some((bind_name, _)) if pool.str(bind_name) == "_" => PatternFieldKind::Wildcard,
                Some((bind_name, _)) => PatternFieldKind::Rename(bind_name),
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
    // mandatory — exactly like the value-side tuple sugar and the
    // positional type sugar, `(a)` never parses here.
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

    pat.define(choice((positional.clone(), anon, bind)).boxed());

    // Paren-less form: `a, b = e`. Bindings only — nesting belongs to
    // the parenthesized form.
    let paren_less = bind_target
        .then_ignore(just(Token::Comma))
        .then(
            bind_target
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .then_ignore(just(Token::Assign))
        .then(value.clone())
        .map_with(|((first, rest), value), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            let start = first.1.start;
            let end = rest.last().map(|(_, s)| s.end).unwrap_or(first.1.end);
            let mut elems = Vec::with_capacity(rest.len() + 1);
            elems.push(bind_pattern(
                e.state(),
                first.0,
                crate::spans::cspan(first.1),
            ));
            for (name, sp) in rest {
                elems.push(bind_pattern(e.state(), name, crate::spans::cspan(sp)));
            }
            let target = e
                .state()
                .pattern_positional(&elems, crate::spans::cspan(SimpleSpan::new((), start..end)));
            e.state().destructure(target, value, span)
        });

    // Braced / parenthesized forms: a self-delimiting pattern followed
    // by `= value`.
    let shaped = choice((anon, positional))
        .then_ignore(just(Token::Assign))
        .then(value.clone())
        .map_with(|(target, value), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            e.state().destructure(target, value, span)
        });

    // `(a) = x`: one binding, no trailing comma, followed by `=`. The
    // shape is a targeted error (E0112), but recover as a PLAIN
    // binding of `a` rather than an error statement: downstream uses
    // of `a` then resolve instead of cascading one 'undefined
    // variable' per use — E0112 already fails the build.
    let single_no_comma = just(Token::LParen)
        .ignore_then(bind_target)
        .then_ignore(just(Token::RParen))
        .then_ignore(just(Token::Assign))
        .then(value)
        .validate(
            |((name, name_span), value), e: &mut Mx<'a, '_, I>, emitter| {
                emitter.emit(Rich::custom(e.span(), ParseDiag::SingleElemDestructuring));
                ((name, name_span), value)
            },
        )
        .map_with(|((name, name_span), value), e: &mut Mx<'a, '_, I>| {
            let span = crate::spans::cspan(e.span());
            let target = Ident::new(name, crate::spans::cspan(name_span));
            e.state().assign_or_decl(target, value, span)
        });

    choice((paren_less, shaped, single_no_comma)).boxed()
}
