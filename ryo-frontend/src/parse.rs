//! Source text → AST + diagnostics: lex, `parse_with_state`, and the `Rich` → `Diag` conversion. The driver consumes this through `parse_source` and owns rendering.

use chumsky::error::{Rich, RichPattern, RichReason};
use chumsky::{Parser, prelude::*};
use ryo_core::ast;
use ryo_core::diag::{Diag, DiagCode, DiagSink, ParseDiag};
use ryo_core::types::InternPool;

use crate::lexer::{self, Token};
use crate::parser::{ParseState, program_parser};
use crate::spans::cspan;

/// Parse source text into an AST plus every diagnostic lexing and parsing
/// produced. `Ok` always carries the diagnostics (possibly empty; a
/// recovered parse still yields a partial AST). `Err` means
/// unrecoverable and carries the raw diagnostics — rendering is the
/// caller's job.
pub fn parse_source(
    input: &str,
    pool: &mut InternPool,
) -> Result<(ast::Ast, Vec<Diag>), Vec<Diag>> {
    // `lexer::lex` runs logos + indent processing + string and
    // integer interning in a single pass and never fails hard: it
    // emits structured `Diag`s into the sink and recovers so the
    // parser still sees a well-formed stream. Lex and parse
    // diagnostics accumulate in the same sink so both can surface in
    // one run, rendered through the same Ariadne pipeline as the
    // middle-end diagnostics.
    let mut sink = DiagSink::new();
    let tokens = lexer::lex(input, pool, &mut sink);

    // Indentation failure: the lexer returned an empty stream because
    // without Indent/Dedent markers parsing is meaningless. Skip the
    // parser and report the lex diagnostics as-is.
    if tokens.is_empty() && sink.has_errors() {
        return Err(sink.into_diags());
    }

    // chumsky 0.12 added `Input::split_token_span`, which collapses the previous
    // `Stream::from_iter(...).map(eoi, |(t, s)| (t, s))` boilerplate that we used to
    // pull `(Token, Span)` slices into a parser-friendly shape.
    let token_stream = tokens[..].split_token_span((0..input.len()).into());

    // R10: the parser recovers at statement boundaries, so a syntax
    // error yields a diagnostic AND a partial program (with `Error`
    // placeholder nodes). Callers thread the diagnostics into the
    // middle-end sink and keep analyzing, so one bad statement never
    // suppresses semantic diagnostics elsewhere in the file (R9).
    // The parser builds directly into the AST arenas, threaded as
    // chumsky parser state (the state also carries the intern pool
    // so positional field keys can mint their canonical names).
    let mut state = ParseState::new(std::mem::take(pool));
    let (out, errs) = program_parser()
        .parse_with_state(token_stream, &mut state)
        .into_output_errors();
    let (ast, parsed_pool) = state.into_parts();
    *pool = parsed_pool;
    for e in &errs {
        let span = cspan(*e.span());
        match e.reason() {
            // Parser-emitted custom errors carry their own `DiagCode`.
            // Most are self-contained; `UnknownAttribute` carries
            // interned ids, so resolve its spelling through the pool
            // here (same pattern as `rich_error_message` below), and
            // `MisplacedAttribute` rides its mandated explanation as a
            // structured note on the headline message.
            RichReason::Custom(pd) => {
                let diag = Diag::error(span, pd.code(), pd.message(pool));
                let diag = match pd {
                    ParseDiag::MisplacedAttribute => diag.with_note(
                        None,
                        "attributes are only supported on struct and enum definitions",
                    ),
                    _ => diag,
                };
                sink.emit(diag);
            }
            RichReason::ExpectedFound { .. } => sink.emit(Diag::error(
                span,
                DiagCode::ParseError,
                rich_error_message(e, pool),
            )),
        }
    }
    match out {
        Some(()) => Ok((ast, sink.into_diags())),
        // Unrecoverable parse (no output even after recovery — e.g.
        // trailing garbage that swallows the end-of-input marker).
        None => Err(sink.into_diags()),
    }
}

/// Render a token with the intern pool available: identifiers and
/// string literals show their actual text, everything else falls back
/// to the token's pool-free `Display` (which renders those payloads
/// as opaque `<id#N>` / `<str#N>` handles).
fn render_token_with_pool(tok: &Token, pool: &InternPool) -> String {
    match tok {
        Token::Ident(id) => pool.str(*id).to_string(),
        Token::StrLit(id) => format!("\"{}\"", pool.str(*id)),
        other => other.to_string(),
    }
}

/// Rebuild chumsky's `Rich` error message with identifier and
/// string-literal payloads resolved through the pool. Mirrors
/// chumsky's own phrasing ("found 'X' expected A, B, or C") so the
/// text stays familiar, but a parse error on `x = foo(` shows `foo`
/// instead of the opaque `<id#0>` handle. Only called for
/// `ExpectedFound` errors — `Custom` payloads carry their own code
/// and self-contained message, converted by the caller.
fn rich_error_message(e: &Rich<'_, Token, SimpleSpan, ParseDiag>, pool: &InternPool) -> String {
    match e.reason() {
        RichReason::Custom(_) => {
            unreachable!("custom parse errors are converted by the caller")
        }
        RichReason::ExpectedFound { .. } => {
            // Unclosed argument list: the parser stopped at a line break
            // while `)` was still a valid continuation. A token dump
            // ("found '<newline>' expected '.', '[', '*', …") sends the
            // user to the glossary instead of the paren — name the
            // problem and the two tokens that would continue the call.
            let at_line_break = matches!(e.found(), Some(Token::Newline | Token::Dedent));
            if at_line_break
                && e.expected().any(|p| match p {
                    RichPattern::Token(tok) => **tok == Token::RParen,
                    _ => false,
                })
            {
                return "unclosed argument list: expected ',' or ')' before the end of the line"
                    .to_string();
            }
            let found = match e.found() {
                Some(tok) => format!("found '{}'", render_token_with_pool(tok, pool)),
                None => "found end of input".to_string(),
            };
            let expected: Vec<String> = e
                .expected()
                .map(|p| match p {
                    RichPattern::Token(tok) => {
                        format!("'{}'", render_token_with_pool(tok, pool))
                    }
                    other => other.to_string(),
                })
                .collect();
            let expected_part = match expected.len() {
                0 => "something else".to_string(),
                1 => expected[0].clone(),
                _ => format!(
                    "{}, or {}",
                    expected[..expected.len() - 1].join(", "),
                    expected
                        .last()
                        .expect("this arm only runs when expected.len() >= 2")
                ),
            };
            format!("{} expected {}", found, expected_part)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_source;
    use ryo_core::ast;
    use ryo_core::diag::{DiagCode, Severity};
    use ryo_core::types::InternPool;

    #[test]
    fn parse_error_renders_identifiers_through_pool() {
        // The token's pool-free `Display` renders identifiers as
        // opaque `<id#N>` handles; the parse-error conversion must
        // re-render them through the pool so the user sees the
        // actual identifier text.
        let mut pool = InternPool::new();
        let (_program, diags) =
            parse_source("x foo = 1", &mut pool).expect("recovery should yield a partial program");
        assert!(!diags.is_empty());
        let msg = &diags[0].message;
        assert!(
            msg.contains("foo"),
            "message should name the identifier text: {msg}"
        );
        assert!(
            !msg.contains("<id#"),
            "message must not leak opaque handle ids: {msg}"
        );
    }

    #[test]
    fn unclosed_call_gets_a_human_message() {
        let mut pool = InternPool::new();
        let (_program, diags) = parse_source(
            "fn main():\n\ttotal = \"hello\"\n\tprint(int_to_str(total.len())\n\tprint(\"done\")\n",
            &mut pool,
        )
        .expect("recovery should yield a partial program");
        let e0100: Vec<_> = diags
            .iter()
            .filter(|d| d.code == DiagCode::ParseError)
            .collect();
        assert_eq!(e0100.len(), 1, "expected one parse error: {diags:?}");
        let msg = &e0100[0].message;
        assert!(
            msg.contains("unclosed") && msg.contains("')'"),
            "message should name the problem and the fix: {msg}"
        );
        assert!(
            !msg.contains("<newline>") && !msg.contains("something else"),
            "message must not leak parser internals: {msg}"
        );
    }

    #[test]
    fn top_level_assignment_gets_targeted_message() {
        let mut pool = InternPool::new();
        let (_program, diags) = parse_source(
            "struct Point:\n\tx: int\n\ty: int\n\nfn main():\n\tq = Point{x=1, y=2}\n\nq.x = 7\n",
            &mut pool,
        )
        .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        assert_eq!(diags[0].code, DiagCode::ParseError);
        assert!(
            diags[0].message.contains("inside a function body"),
            "message should explain the rule: {}",
            diags[0].message
        );
    }

    #[test]
    fn top_level_assignment_message_covers_positional_and_compound() {
        let mut pool = InternPool::new();
        let (_p, diags) =
            parse_source("q.0 = 7\n", &mut pool).expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "positional target: {diags:?}");
        assert!(
            diags
                .iter()
                .all(|d| d.message.contains("inside a function body")),
            "positional target: {diags:?}"
        );

        let mut pool = InternPool::new();
        let (_p, diags) =
            parse_source("q.x += 1\n", &mut pool).expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "compound assign: {diags:?}");
        assert!(
            diags
                .iter()
                .all(|d| d.message.contains("inside a function body")),
            "compound assign: {diags:?}"
        );
    }

    #[test]
    fn top_level_plain_decl_still_parses() {
        let mut pool = InternPool::new();
        parse_source("q = 7\nprint(q)\n", &mut pool)
            .expect("top-level var decls and flat scripts stay valid");
    }

    #[test]
    fn one_line_if_body_gets_targeted_message() {
        let mut pool = InternPool::new();
        let (_p, diags) = parse_source(
            "fn factorial(n: int) -> int:\n\tif (n <= 1): return 1\n\treturn n * factorial(n - 1)\n",
            &mut pool,
        )
        .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        assert_eq!(diags[0].code, DiagCode::ParseError);
        assert!(
            diags[0].message.contains("one-line") && diags[0].message.contains("own line"),
            "message should steer to the fix: {}",
            diags[0].message
        );
        assert!(
            !diags[0].message.contains("<indent>"),
            "message must not leak parser internals: {}",
            diags[0].message
        );
    }

    #[test]
    fn missing_colon_after_condition_gets_targeted_message() {
        let mut pool = InternPool::new();
        let (_p, diags) = parse_source(
            "fn f(n: int) -> int:\n\tif (n <= 1) return 1\n\treturn n\n",
            &mut pool,
        )
        .expect("recovery should yield a partial program");
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("expected ':' after the condition")),
            "missing-colon shape should be named: {diags:?}"
        );
    }

    #[test]
    fn accepted_block_form_stays_silent() {
        let mut pool = InternPool::new();
        parse_source(
            "fn factorial(n: int) -> int:\n\tif (n <= 1):\n\t\treturn 1\n\treturn n * factorial(n - 1)\n\nfn main():\n\tprint(factorial(5))\n",
            &mut pool,
        )
        .expect("newline + indented block must parse clean");
    }

    #[test]
    fn blank_line_between_header_and_body_is_not_a_one_line_body() {
        let mut pool = InternPool::new();
        parse_source(
            "fn f(n: int) -> int:\n\tif (n <= 1):\n\n\t\treturn 1\n\treturn n\n",
            &mut pool,
        )
        .expect("blank-line tolerance must survive");
    }

    #[test]
    fn one_line_body_message_covers_all_block_headers() {
        // Same Python-transcription hazard on every header — the
        // shared helper must fire for each, with the keyword
        // interpolated.
        for (src, kw) in [
            (
                "fn f(n: int) -> int:\n\twhile n > 0: n -= 1\n\treturn n\n",
                "while",
            ),
            (
                "fn f(n: int) -> int:\n\tfor i in range(0, 2): print(i)\n\treturn n\n",
                "for",
            ),
        ] {
            let mut pool = InternPool::new();
            let (_p, diags) =
                parse_source(src, &mut pool).expect("recovery should yield a partial program");
            assert!(
                diags
                    .iter()
                    .any(|d| d.message.contains(&format!("one-line '{kw}'"))),
                "{kw}: targeted message missing: {diags:?}"
            );
        }
    }

    #[test]
    fn header_recovery_preserves_indented_structure() {
        // The line after a one-line-body error is deeper-indented.
        // The recovery swallow must stop at <indent> (the indent
        // preprocessor emits it before the newline) so the block
        // parser can claim the indented body — eating the <indent>
        // orphans its <dedent> and cascades extra parse errors.
        for src in [
            "fn f(n: int) -> int:\n\tif n <= 1: return 1\n\t\tprint(n)\n\treturn n\n",
            "fn f(n: int) -> int:\n\tif n <= 1 return 1\n\t\tprint(n)\n\treturn n\n",
        ] {
            let mut pool = InternPool::new();
            let (_p, diags) =
                parse_source(src, &mut pool).expect("recovery should yield a partial program");
            assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        }
    }

    #[test]
    fn unknown_attribute_diagnostic_names_the_attribute() {
        // The parser is pool-less, so it reports the unknown attribute
        // as interned ids; the parse-error conversion must render the
        // spelling (and the known set) through the pool.
        let mut pool = InternPool::new();
        let (_program, diags) = parse_source("#[derive(Bogus)] struct P:\n\tx: int\n", &mut pool)
            .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        let diag = &diags[0];
        assert_eq!(diag.code, DiagCode::UnknownAttribute);
        assert!(
            diag.message.contains("derive(Bogus)"),
            "message should name the attribute: {}",
            diag.message
        );
        assert!(
            diag.message.contains("derive(Eq), repr(C)"),
            "message should list the known set: {}",
            diag.message
        );
    }

    #[test]
    fn misplaced_attribute_diagnostic_carries_the_note() {
        // The mandated explanation rides as a structured `DiagNote`
        // under the headline message, not as message text.
        let mut pool = InternPool::new();
        let (_program, diags) =
            parse_source("#[derive(Eq)]\nfn f():\n\tpass_through = 1\n", &mut pool)
                .expect("recovery should yield a partial program");
        assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
        let diag = &diags[0];
        assert_eq!(diag.code, DiagCode::UnknownAttribute);
        assert!(
            diag.message.contains("unexpected attribute"),
            "headline should name the misplaced attribute: {}",
            diag.message
        );
        assert!(
            diag.notes.iter().any(
                |n| n.message == "attributes are only supported on struct and enum definitions"
            ),
            "note should explain that attributes need a struct or enum definition: {:?}",
            diag.notes
        );
    }

    #[test]
    fn parse_source_recovers_and_returns_partial_program() {
        // R10: one syntax error must not discard the rest of the
        // file. The parser synchronizes at the next statement
        // boundary, reports the error, and yields a partial AST
        // with an `Error` placeholder node.
        let mut pool = InternPool::new();
        let (program, diags) = parse_source("x = 1\ny = = 2\nz = 3\n", &mut pool)
            .expect("recovery should yield a partial program");
        assert_eq!(
            diags
                .iter()
                .filter(|d| d.code == DiagCode::ParseError)
                .count(),
            1,
            "expected exactly one parse diagnostic: {diags:?}"
        );
        let stmts = program.top_level_stmts();
        assert_eq!(stmts.len(), 3);
        assert!(matches!(
            program.stmt(stmts[0]).kind,
            ast::StmtKind::VarDecl(_)
        ));
        assert!(matches!(program.stmt(stmts[1]).kind, ast::StmtKind::Error));
        assert!(matches!(
            program.stmt(stmts[2]).kind,
            ast::StmtKind::VarDecl(_)
        ));
    }

    #[test]
    fn named_arg_in_parens_enum_flavor_message_is_targeted() {
        // `Shape.Circle(radius=5.0)` (Circle is a tuple variant) used
        // to render the generic E0100 expected-token dump; the
        // targeted E0124 names the variant and shows both correct
        // spellings, as the single error.
        let mut pool = InternPool::new();
        let input = "enum Shape:\n\tCircle(float)\n\tRectangle(width: float, height: float)\n\nfn main():\n\tprint(Shape.Circle(radius=5.0))\n";
        let (_program, diags) =
            parse_source(input, &mut pool).expect("recovery should yield a partial program");
        let errors = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "expected exactly one error: {diags:?}");
        let diag = errors[0];
        assert_eq!(diag.code, DiagCode::NamedArgInParens);
        assert_eq!(
            diag.message,
            "unexpected '=' — 'Circle' takes positional arguments: \
             Shape.Circle(...); named fields use braces on named variants: \
             Shape.Variant{field=value}"
        );
        assert!(
            !diag.message.contains("expected '('"),
            "message must not be a token dump: {}",
            diag.message
        );
    }

    #[test]
    fn named_arg_in_parens_method_flavor_message() {
        // The plain-call mirror: `f(a=b)` names no variant and points
        // at the positional spelling.
        let mut pool = InternPool::new();
        let input = "fn main():\n\tf(a=b)\n";
        let (_program, diags) =
            parse_source(input, &mut pool).expect("recovery should yield a partial program");
        let diag = diags
            .iter()
            .find(|d| d.code == DiagCode::NamedArgInParens)
            .expect("named-arg diagnostic must surface");
        assert_eq!(
            diag.message,
            "named arguments are not supported — pass arguments positionally"
        );
    }

    #[test]
    fn missing_parens_construct_message_is_targeted() {
        // `x = Shape.Circle 5.0`: one E0126 — the trailing
        // expression is recovered as the payload, so the statement
        // still declares `x` and no undefined-variable cascade
        // follows.
        let mut pool = InternPool::new();
        let input =
            "enum Shape:\n\tCircle(float)\n\nfn main():\n\tx = Shape.Circle 5.0\n\tprint(x)\n";
        let (_program, diags) =
            parse_source(input, &mut pool).expect("recovery should yield a partial program");
        let errors = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "expected exactly one error: {diags:?}");
        assert_eq!(errors[0].code, DiagCode::MissingArgListOnVariant);
        assert_eq!(
            errors[0].message,
            "missing argument list — 'Circle' takes a positional payload: Shape.Circle(...)"
        );
    }

    #[test]
    fn positional_values_in_braces_message_is_targeted() {
        // `Shape.Rectangle{1.0, 2.0}`: E0127 names the field syntax
        // and the paren form; the named variant then gets its own
        // E0123 — two one-liners, no token dump.
        let mut pool = InternPool::new();
        let input = "enum Shape:\n\tRectangle(width: float, height: float)\n\nfn main():\n\tprint(Shape.Rectangle{1.0, 2.0})\n";
        let (_program, diags) =
            parse_source(input, &mut pool).expect("recovery should yield a partial program");
        // (The named variant then gets its own E0123 from sema — the
        // Brace Law, covered by the sema suite.)
        let errors = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "expected exactly one error: {diags:?}");
        assert_eq!(errors[0].code, DiagCode::PositionalArgsInBraces);
        assert_eq!(
            errors[0].message,
            "fields are written name=value (width=1.0); for a positional payload use parens: Shape.Variant(...)"
        );
    }

    #[test]
    fn comma_separated_variants_message_is_targeted() {
        // `enum Color: Red, Green, Blue`: one E0128; the variants
        // are declared anyway, so the rest of the file type-checks.
        let mut pool = InternPool::new();
        let input = "enum Color: Red, Green, Blue\n\nfn main():\n\tc = Color.Red\n\tprint(c)\n";
        let (_program, diags) =
            parse_source(input, &mut pool).expect("recovery should yield a partial program");
        let errors = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1, "expected exactly one error: {diags:?}");
        assert_eq!(errors[0].code, DiagCode::CommaSeparatedVariants);
        assert_eq!(
            errors[0].message,
            "enum variants are declared one per line (block form); there is no comma form"
        );
    }

    #[test]
    fn derive_debug_message_notes_it_is_automatic() {
        // `#[derive(Debug)]`: Debug needs no attribute — the E0108
        // message says so, alongside the known-attribute list.
        let mut pool = InternPool::new();
        let input = "#[derive(Debug)]\nenum Color:\n\tRed\n\nfn main():\n\tpass_through = 1\n";
        let (_program, diags) =
            parse_source(input, &mut pool).expect("recovery should yield a partial program");
        let diag = diags
            .iter()
            .find(|d| d.code == DiagCode::UnknownAttribute)
            .expect("unknown-attribute diagnostic must surface");
        assert_eq!(
            diag.message,
            "unknown attribute 'derive(Debug)'; Debug is automatic for every enum; known attributes: derive(Eq), repr(C)"
        );
    }

    #[test]
    fn indentation_failure_returns_raw_lex_diags_without_parsing() {
        // Mixed tab/space indentation makes the lexer return an empty
        // stream; the parse must bail before the parser runs and hand
        // back the lex diagnostics as-is (the early-bail path).
        let mut pool = InternPool::new();
        let diags = match parse_source("fn f():\n\tx = 1\n        y = 2\n", &mut pool) {
            Err(diags) => diags,
            Ok(_) => panic!("space-indented line must not parse"),
        };
        let diag = diags
            .iter()
            .find(|d| d.code == DiagCode::ParseError)
            .expect("the indentation diagnostic must surface");
        assert!(
            diag.message
                .contains("spaces are not allowed for indentation"),
            "unexpected message: {}",
            diag.message
        );
    }
}
