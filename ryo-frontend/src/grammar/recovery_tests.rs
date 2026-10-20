//! Parser error-recovery tests: broken statements recover to `Error`
//! placeholder statements in a partial program, broken block headers
//! swallow their indented body, and trailing garbage at EOF keeps the
//! well-formed prefix. Split out of `grammar/tests.rs` to stay under the
//! file-length limit; the shared parse helpers live there as `pub(super)`.

use super::tests::*;
use super::*;

/// The statement is a parser-recovery placeholder.
pub(super) fn is_error_stmt(ast: &Ast, stmt: StmtId) -> bool {
    matches!(ast.stmt(stmt).kind, StmtKind::Error)
}

/// The statement is a variable declaration of either form.
fn is_decl_stmt(ast: &Ast, stmt: StmtId) -> bool {
    matches!(
        ast.stmt(stmt).kind,
        StmtKind::VarDecl(_) | StmtKind::AssignOrDecl { .. }
    )
}

#[test]
fn recovers_from_bad_statement_between_good_ones() {
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = 1\ny = = 2\nz = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 3);
    assert!(is_decl_stmt(&ast, stmts[0]));
    assert!(is_error_stmt(&ast, stmts[1]));
    assert!(is_decl_stmt(&ast, stmts[2]));
}

#[test]
fn recovers_inside_function_body() {
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2\n\tz = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let func = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 3);
    assert!(is_error_stmt(&ast, body[1]));
    assert!(matches!(
        ast.stmt(body[2]).kind,
        StmtKind::AssignOrDecl { .. }
    ));
}

#[test]
fn reports_multiple_parse_errors_in_one_file() {
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = = 1\ny = 2\nz = = 3\nw = 4\n");
    assert_eq!(errs.len(), 2, "expected two parse errors: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 4);
    assert!(is_error_stmt(&ast, stmts[0]));
    assert!(is_decl_stmt(&ast, stmts[1]));
    assert!(is_error_stmt(&ast, stmts[2]));
    assert!(is_decl_stmt(&ast, stmts[3]));
}

#[test]
fn recovers_from_trailing_garbage_without_newline_at_eof() {
    // File does not end with a newline: the last line is broken.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = 1\ny = = 2");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 2);
    assert!(is_decl_stmt(&ast, stmts[0]));
    assert!(is_error_stmt(&ast, stmts[1]));
}

#[test]
fn recovers_from_broken_block_final_statement_before_dedent() {
    // The last body line is broken and the following top-level
    // statement triggers the block's `Dedent`.
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2\nz = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    let func = fn_def(&ast, stmts[0]);
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 2);
    assert!(is_error_stmt(&ast, body[1]));
    assert_eq!(stmts.len(), 2);
}

#[test]
fn recovers_from_broken_block_final_statement_at_eof() {
    // The broken final body line sits directly against the
    // zero-width end-of-input `Dedent` (no terminating newline of
    // its own) — the `peek_terminator(Dedent)` terminator path.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main():\n\tx = 1\n\ty = = 2");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let func = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 2);
    assert!(is_error_stmt(&ast, body[1]));
    assert_eq!(ast.top_level_stmts().len(), 1);
}

#[test]
fn parses_unterminated_final_statement_in_block() {
    // The block-final statement has no terminating newline: it
    // ends directly at the list terminator (the end-of-input
    // `Dedent`). Valid input, so no error node and no diagnostic.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main():\n\tx = 1");
    assert!(errs.is_empty(), "expected a clean parse: {errs:?}");
    assert!(ok, "expected a program");
    let func = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 1);
    assert!(is_decl_stmt(&ast, body[0]));
}

#[test]
fn two_statements_on_one_line_still_error_with_recovery() {
    // The no-two-statements-per-line rule must survive recovery:
    // this is one parse error, not two silent statements.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("x = 1 y = 2\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 1);
    assert!(is_error_stmt(&ast, stmts[0]));
}

#[test]
fn broken_block_header_swallows_body_at_top_level() {
    // The `fn` header is unparseable; its indented body must be
    // swallowed as one region — not parse silently as top-level
    // statements, and not leave a dangling `Dedent` behind.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main(:\n\tx = 1\n\ty = 2\nz = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 2);
    assert!(is_error_stmt(&ast, stmts[0]));
    assert!(is_decl_stmt(&ast, stmts[1]));
}

#[test]
fn broken_block_header_inside_block_swallows_body() {
    // Same mis-nesting one level down: the broken `if` header must
    // swallow its body inside the enclosing `fn` block, and the
    // following body line must still parse in that block.
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("fn main():\n\tif x(:\n\t\ty = 1\n\tz = 2\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let func = fn_def(&ast, only_stmt(&ast));
    let body = fn_body(&ast, func);
    assert_eq!(body.len(), 2);
    assert!(is_error_stmt(&ast, body[0]));
    assert!(matches!(
        ast.stmt(body[1]).kind,
        StmtKind::AssignOrDecl { .. }
    ));
    assert_eq!(ast.top_level_stmts().len(), 1);
}

#[test]
fn broken_block_header_swallows_nested_blocks() {
    // The swallowed region must be balanced: the inner block's
    // `Dedent` does not end the swallow early.
    let (ok, ast, errs, _pool) =
        lex_and_parse_recovering("while x(:\n\tif y:\n\t\tz = 1\n\tw = 2\nv = 3\n");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    let stmts = ast.top_level_stmts();
    assert_eq!(stmts.len(), 2);
    assert!(is_error_stmt(&ast, stmts[0]));
    assert!(is_decl_stmt(&ast, stmts[1]));
}

#[test]
fn broken_block_header_with_body_at_eof() {
    // No trailing statement: the swallowed body runs into the
    // end-of-input `Dedent`, and that is still a single error.
    let (ok, ast, errs, _pool) = lex_and_parse_recovering("fn main(:\n\tx = 1");
    assert_eq!(errs.len(), 1, "expected one parse error: {errs:?}");
    assert!(ok, "recovery must produce a partial program");
    assert_eq!(ast.top_level_stmts().len(), 1);
    assert!(is_error_stmt(&ast, ast.top_level_stmts()[0]));
}
