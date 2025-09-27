Okay, creating a full `parser.rs` using `chumsky` is a significant undertaking. I'll provide a well-structured starting point covering the minimal Ryo subset we defined earlier (e.g., `fn main(): return <integer>;`) and outline how to expand it for other features from the spec (Draft v1.6).

This example assumes:

*   You have `chumsky = "..."` and `logos = "..."` in `ryo-parser/Cargo.toml`.
*   You have `ryo-core = { path = "../ryo-core" }` and `ryo-errors = { path = "../ryo-errors" }`.
*   `ryo-core` contains `token.rs` (as defined previously, potentially without Indent/Dedent yet) and `ast.rs` (basic AST nodes).
*   We will **ignore indentation** for this *initial minimal example* and assume simple sequences or require braces `{}` temporarily for blocks, focusing on parsing logic first. **Proper indentation parsing is complex and needs to be added later.**

```rust
// ryo-parser/src/parser.rs

use crate::lexer::lexer; // Assuming lexer function is in lexer.rs
use chumsky::prelude::*;
use chumsky::Parser;
use ryo_core::ast::{Expression, FunctionDef, Literal, Program, Statement, TypeExpr}; // Basic AST nodes
use ryo_core::token::Token; // Use Token enum from ryo-core

// Define the type for the parser's extra state/context if needed (e.g., for error recovery)
// type ParserExtra<'a, T> = chumsky::extra::Err<Rich<'a, T>>; // Example using Rich errors

// --- Parser Definition ---

// Entry point: Parses a stream of tokens into a Program AST node
pub fn program_parser<'a>() -> impl Parser<'a, &'a [(Token, Span)], Program, extra::Err<Rich<'a, Token>>>
{
    // Define parsers for individual components first

    // Parser for identifiers
    let ident = select! { Token::Ident(name) => name }.labelled("identifier");

    // Parser for integer literals
    let int_literal = select! { Token::LiteralInt(val) => Literal::Int(val) }.labelled("integer literal");

    // Parser for basic type expressions (just identifier for now)
    // TODO: Expand for List[T], Map[K,V], Optional[T], Result[T,E], tuple types etc.
    let type_expr = ident.map(|name| TypeExpr::Name(name)).labelled("type");

    // --- Expressions (Minimal) ---
    // For now, only integer literals are expressions
    // TODO: Expand significantly for operators, function calls, etc.
    let expression = int_literal
        .map(|lit| Expression::Literal(lit))
        .labelled("expression");

    // --- Statements (Minimal) ---
    // Parser for a return statement
    let return_stmt = just(Token::KwReturn)
        .ignore_then(expression) // Expect an expression after 'return'
        .map(|expr| Statement::Return { value: expr })
        .labelled("return statement");

    // Combine different statement types
    // TODO: Add variable declarations, if/for, match, etc.
    let statement = return_stmt; // Only return for now

    // --- Blocks (Simplified - Ignoring Indentation for now) ---
    // Assume statements are just sequenced, or use {} for temporary blocks
    // Proper indentation parsing requires a pre-processing step or more complex parser state
    let block = statement // For now, a block is just one statement
        .repeated()
        .collect::<Vec<_>>()
        .labelled("block");
    /* // Example using braces if indentation is ignored initially:
    let block = statement
        .repeated()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
        .labelled("block");
    */

    // --- Function Definition (Minimal main) ---
    let function_def = just(Token::KwFn)
        .ignore_then(ident.filter(|name| name == "main")) // Only 'main' for now
        .then_ignore(just(Token::LParen))
        .then_ignore(just(Token::RParen)) // No parameters for now
        // TODO: Add parameter parsing: separated_by(param_parser, Token::Comma)
        // TODO: Add return type parsing: .then_ignore(just(Token::Arrow)).then(type_expr)
        .then_ignore(just(Token::Colon))
        .then(block) // Expect a block after ':'
        .map(|(name, body_stmts)| FunctionDef {
            name,
            params: Vec::new(),   // Empty params for now
            return_type: None, // No return type specified for now
            body: body_stmts,
        })
        .labelled("function definition");

    // --- Program ---
    // A program is currently just a single function definition (main)
    // TODO: Allow multiple functions, imports, structs, enums etc. at the top level
    let program = function_def
        .map(|func| Program {
            functions: vec![func],
            // Add other top-level items here (structs, enums, imports...)
        })
        .then_ignore(end()); // Ensure the entire input is consumed

    program
}

// --- Main Parsing Function ---

// This function takes the source code string, lexes it, and then parses it.
// It returns the Program AST or a list of parsing errors.
pub fn parse_source(source: &str) -> (Option<Program>, Vec<Rich<Token>>) {
    let tokens = match lexer(source) {
        Ok(t) => t,
        Err(lex_errors) => {
            // Convert lexer errors to Rich errors for consistency (implementation needed)
            // For now, just return empty program and dummy errors
            eprintln!("Lexing errors occurred!"); // Replace with proper reporting
            return (None, Vec::new()); // Placeholder
        }
    };

    // Convert Vec<(Token, Span)> to the slice format Chumsky expects
    let token_slice = tokens.as_slice();
    // Create a stream for Chumsky
    let token_stream = chumsky::Stream::from_iter(token_slice.iter().cloned())
                                    .spanned(Span::new(source.len(), source.len())); // Span for EOF

    program_parser().parse_recovery(token_stream) // Use parse_recovery for better errors
}

// --- Helper Types (Spans, Errors) ---
// Re-export or define Span and Rich error type for convenience
// Often defined in lib.rs or a shared error module

// Example Span type (assuming you use Chumsky's SimpleSpan)
pub type Span = SimpleSpan<usize>;

// Example Rich error type from Chumsky
// use chumsky::error::Rich;
use chumsky::{error::Rich, span::SimpleSpan}; // Import Rich error type
                                              // Assuming you'll use the default 'Rich' error type from Chumsky
                                              // pub type ParseError<'a> = Rich<'a, Token>;
                                              // Let's use the full path for clarity for now:
                                              // Vec<chumsky::error::Rich<'a, Token>>
```

**Explanation and Next Steps:**

1.  **Dependencies:** Make sure `chumsky`, `logos`, `ryo-core`, and `ryo-errors` are correctly set up in `ryo-parser/Cargo.toml`.
2.  **Core Imports:** Imports necessary components from `chumsky`, `ryo_core::ast`, and `ryo_core::token`.
3.  **Parser Primitives:**
    *   `select!`: Extracts data from specific token variants (e.g., `Token::Ident(name)`).
    *   `just(Token)`: Matches a specific token exactly.
    *   `.ignore_then(...)`: Parses something but discards its result, then parses the next thing.
    *   `.then(...)`: Parses two things sequentially, returning both as a tuple.
    *   `.map(|...)`: Transforms the result of a parser.
    *   `.labelled("...")`: Assigns a label for better error messages.
    *   `.repeated().collect()`: Parses zero or more occurrences of something and collects into a `Vec`.
    *   `end()`: Ensures the parser consumes the entire input.
4.  **Structure:** The code builds parsers bottom-up: literals/identifiers -> expressions -> statements -> blocks -> function definitions -> program.
5.  **Minimal Subset:** This code *only* parses `fn main(): return <integer>;`. Expanding it involves:
    *   **Expressions:** Adding parsers for binary operators (handling precedence with `left_assoc`, `right_assoc`), unary operators, function calls, tuple/list/map literals, variable access (`ident`).
    *   **Statements:** Adding parsers for variable declarations (`let`/`mut`), `if`/`elif`/`else`, `for`, `match`, expression statements (like a function call on its own).
    *   **Types:** Expanding `type_expr` to handle `List[T]`, `Map[K,V]`, `(T1, T2)`, `Optional[T]`, `Result[T,E]`.
    *   **Function Definitions:** Parsing parameters `(name: Type, ...)` and return types `-> Type`. Allowing functions other than `main`.
    *   **Top Level:** Allowing multiple functions, `struct`, `enum`, `trait`, `impl`, `import` statements at the top level of a file within the `program` parser.
6.  **Indentation:** This is the **biggest missing piece** here. You need a pre-processing step (lexer wrapper) to convert newline/tab sequences into `Indent`/`Dedent`/`Newline` tokens. The parser then uses these tokens to structure blocks instead of relying on `{}` or simple sequencing. Chumsky has examples/strategies for handling indentation-sensitive languages, but it adds significant complexity.
7.  **Error Handling:** `parse_recovery` is used for better error reporting than `parse`. You'll need to integrate this properly with `ryo-errors` and `codespan-reporting` to show user-friendly messages based on the `Rich` errors Chumsky produces. The `lex_errors` conversion also needs implementation.
8.  **`parse_source` Function:** This is the main entry point that ties the lexer and parser together.

**To Expand This:**

*   **Add Expression Parsing:** Start with simple binary operators, carefully considering precedence using Chumsky's features.
*   **Add Statement Parsing:** Implement variable declarations (`let`/`mut name: type = expr`).
*   **Tackle Indentation:** Implement the lexer wrapper to generate `Indent`/`Dedent` tokens. Modify the `block` parser to use these instead of simple repetition or braces. This is crucial for Ryo's syntax.
*   **Incrementally Add Features:** Add other statements, top-level items, and type expressions one by one, writing tests for each.

This provides a foundational parser structure using Chumsky. Remember that building a full parser for a language, especially one with significant indentation, is a complex task requiring careful, incremental development and thorough testing.