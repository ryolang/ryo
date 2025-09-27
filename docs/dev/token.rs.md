Okay, here is a starting point for `ryo-core/src/token.rs` using the `logos` crate. This defines the basic tokens based on the Ryo specification (Draft v1.6).

**Assumptions:**

*   You have added `logos = "..."` to the `ryo-core/Cargo.toml` dependencies.
*   Ryo uses tabs for indentation, and we need a way to represent indent/dedent events (often handled *after* initial lexing, see notes below).

```rust
// ryo-core/src/token.rs

use logos::{Logos, Span}; // Import Logos derive macro and Span struct
use std::fmt;

// Define the Token enum using the Logos derive macro
#[derive(Logos, Debug, PartialEq, Clone)]
#[logos(error = LexingError)] // Use LexingError enum for errors
pub enum Token {
    // --- Keywords ---
    #[token("fn")]
    KwFn,
    #[token("struct")]
    KwStruct,
    #[token("enum")]
    KwEnum,
    #[token("trait")]
    KwTrait,
    #[token("impl")]
    KwImpl,
    #[token("mut")]
    KwMut,
    #[token("if")]
    KwIf,
    #[token("elif")]
    KwElif,
    #[token("else")]
    KwElse,
    #[token("for")]
    KwFor,
    #[token("in")]
    KwIn,
    #[token("return")]
    KwReturn,
    #[token("break")]
    KwBreak,
    #[token("continue")]
    KwContinue,
    #[token("import")]
    KwImport,
    #[token("match")]
    KwMatch,
    #[token("pub")]
    KwPub,
    #[token("Result")] // Treat built-in type names like keywords initially
    KwResult,         // Alternatively, handle via Ident and check later
    #[token("Optional")]
    KwOptional,
    #[token("Ok")]
    KwOk,
    #[token("Err")]
    KwErr,
    #[token("Some")]
    KwSome,
    // Note: 'None' is Optional.None, not a keyword
    #[token("true")]
    KwTrue,
    #[token("false")]
    KwFalse,
    #[token("comptime")]
    KwComptime,
    #[token("spawn")]
    KwSpawn,
    #[token("chan")]
    KwChan,
    #[token("select")]
    KwSelect,
    #[token("move")]
    KwMove,
    #[token("unsafe")]
    KwUnsafe,

    // --- Operators ---
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("==")]
    EqEq,
    #[token("!=")]
    NotEq,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("<=")]
    LtEq,
    #[token(">=")]
    GtEq,
    #[token("and")] // Logical operators as keywords
    OpAnd,
    #[token("or")]
    OpOr,
    #[token("not")]
    OpNot,
    #[token("=")]
    Eq,
    #[token(".")]
    Dot,
    #[token("?")]
    Question,
    #[token("<-")] // Channel send/receive
    ChanOp,

    // --- Punctuation / Delimiters ---
    #[token(":")]
    Colon,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token(",")]
    Comma,
    #[token("->")]
    Arrow, // For function return types

    // --- Literals ---
    // Integer: Handles decimal, hex, octal, binary, allows underscores
    #[regex(r"0[xX][0-9a-fA-F_]+|0[oO][0-7_]+|0[bB][01_]+|[0-9][0-9_]*", |lex| lex.slice().parse::<i64>().ok())] // Example: parse directly, adjust type as needed (isize?)
    LiteralInt(i64), // Store the parsed value

    // Float: Handles decimal, scientific notation, allows underscores
    #[regex(r"[0-9][0-9_]*(\.[0-9_]+)?([eE][+-]?[0-9_]+)?", |lex| lex.slice().parse::<f64>().ok())]
    LiteralFloat(f64), // Store the parsed value

    // String: Handles basic escapes. More complex logic in callback.
    #[token("\"", lex_string)]
    LiteralString(String), // Store the parsed and unescaped string content

    // F-String Start: Just recognize the start, parsing handled later
    #[token("f\"")]
    LiteralFStringStart,

    // Char: Single quotes, basic escapes
    #[regex(r"'([^'\\]|\\.)'", |lex| lex.slice().chars().nth(1))] // Basic char literal
    LiteralChar(char),

    // --- Identifiers ---
    // Matches typical identifier rules, must come after keywords
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*", |lex| lex.slice().to_string())]
    Ident(String),

    // --- Whitespace and Comments ---
    // Logos skips whitespace defined using #[regex(r"[ \n\r\f]+", logos::skip)]
    // However, we need to handle TABS for indentation explicitly.
    // Comments are skipped.
    #[regex(r"[ \r\f]+", logos::skip)] // Skip spaces, CR, form feed
    #[regex(r"#[^\n]*", logos::skip)] // Skip comments
    #[error] // Mark the error variant
    Error(LexingError),

    // --- Special Tokens (Potentially added by a post-processing step) ---
    // These are often NOT generated directly by Logos but by analyzing newline/tab patterns
    // Indent, // Signifies an increase in indentation level
    // Dedent, // Signifies a decrease in indentation level
    // Newline, // Sometimes significant, sometimes skipped depending on grammar

    // End of File marker
    Eof, // Manually added by the lexer wrapper when input ends
}

// Define the error type for lexing
#[derive(Debug, PartialEq, Clone)]
pub enum LexingError {
    #[error("Invalid character")]
    InvalidCharacter,

    #[error("Invalid integer literal")]
    InvalidIntegerLiteral,

    #[error("Invalid float literal")]
    InvalidFloatLiteral,

    #[error("Unterminated string literal")]
    UnterminatedString,

    #[error("Invalid character literal")]
    InvalidCharLiteral,
    // Add more specific errors as needed
}

// Custom lexing function for string literals to handle escapes
fn lex_string(lex: &mut logos::Lexer<Token>) -> Option<String> {
    let mut content = String::new();
    // Consume the opening quote already matched by the token definition
    let remainder = lex.remainder();
    let mut chars = remainder.char_indices(); // Use char_indices for byte offsets
    let mut end_offset = 0;

    while let Some((offset, ch)) = chars.next() {
        end_offset = offset + ch.len_utf8(); // Update end offset

        match ch {
            '"' => {
                // Found the closing quote
                lex.bump(end_offset); // Consume including the closing quote
                return Some(content);
            }
            '\\' => {
                // Handle escape sequence
                if let Some((_, next_ch)) = chars.next() {
                    end_offset += next_ch.len_utf8(); // Consume escaped char
                    match next_ch {
                        'n' => content.push('\n'),
                        't' => content.push('\t'),
                        'r' => content.push('\r'),
                        '\\' => content.push('\\'),
                        '"' => content.push('"'),
                        // Add hex \xHH, unicode \u{HHHH} escapes if needed
                        // Example (simple hex):
                        // 'x' => { ... read next 2 hex chars ... }
                        _ => {
                            // Invalid escape sequence, potentially push both chars or error
                            content.push('\\');
                            content.push(next_ch);
                        }
                    }
                } else {
                    // Escape at end of input - error
                    // Error handling logic needed here - likely return None and set error state
                    return None; // Indicate error
                }
            }
            _ => {
                content.push(ch);
            }
        }
    }

    // If loop finishes without finding closing quote, it's an error
    // Error handling logic needed here
    None // Indicate error (unterminated string)
}

// Implement Display for nice printing (optional but helpful)
// Note: Logos provides a default Debug impl, Display is custom
impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Keywords
            Token::KwFn => write!(f, "fn"),
            Token::KwStruct => write!(f, "struct"),
            Token::KwEnum => write!(f, "enum"),
            Token::KwTrait => write!(f, "trait"),
            Token::KwImpl => write!(f, "impl"),
            Token::KwMut => write!(f, "mut"),
            Token::KwIf => write!(f, "if"),
            Token::KwElif => write!(f, "elif"),
            Token::KwElse => write!(f, "else"),
            Token::KwFor => write!(f, "for"),
            Token::KwIn => write!(f, "in"),
            Token::KwReturn => write!(f, "return"),
            Token::KwBreak => write!(f, "break"),
            Token::KwContinue => write!(f, "continue"),
            Token::KwImport => write!(f, "import"),
            Token::KwMatch => write!(f, "match"),
            Token::KwPub => write!(f, "pub"),
            Token::KwResult => write!(f, "Result"),
            Token::KwOptional => write!(f, "Optional"),
            Token::KwOk => write!(f, "Ok"),
            Token::KwErr => write!(f, "Err"),
            Token::KwSome => write!(f, "Some"),
            Token::KwTrue => write!(f, "true"),
            Token::KwFalse => write!(f, "false"),
            Token::KwComptime => write!(f, "comptime"),
            Token::KwSpawn => write!(f, "spawn"),
            Token::KwChan => write!(f, "chan"),
            Token::KwSelect => write!(f, "select"),
            Token::KwMove => write!(f, "move"),
            Token::KwUnsafe => write!(f, "unsafe"),
            // Operators
            Token::Plus => write!(f, "+"),
            Token::Minus => write!(f, "-"),
            Token::Star => write!(f, "*"),
            Token::Slash => write!(f, "/"),
            Token::Percent => write!(f, "%"),
            Token::EqEq => write!(f, "=="),
            Token::NotEq => write!(f, "!="),
            Token::Lt => write!(f, "<"),
            Token::Gt => write!(f, ">"),
            Token::LtEq => write!(f, "<="),
            Token::GtEq => write!(f, ">="),
            Token::OpAnd => write!(f, "and"),
            Token::OpOr => write!(f, "or"),
            Token::OpNot => write!(f, "not"),
            Token::Eq => write!(f, "="),
            Token::Dot => write!(f, "."),
            Token::Question => write!(f, "?"),
            Token::ChanOp => write!(f, "<-"),
            // Punctuation
            Token::Colon => write!(f, ":"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            Token::LBracket => write!(f, "["),
            Token::RBracket => write!(f, "]"),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::Comma => write!(f, ","),
            Token::Arrow => write!(f, "->"),
            // Literals
            Token::LiteralInt(i) => write!(f, "{}", i),
            Token::LiteralFloat(fl) => write!(f, "{}", fl),
            Token::LiteralString(s) => write!(f, "\"{}\"", s), // Display with quotes
            Token::LiteralFStringStart => write!(f, "f\""),
            Token::LiteralChar(c) => write!(f, "'{}'", c),
            // Identifier
            Token::Ident(name) => write!(f, "{}", name),
            // Error
            Token::Error(e) => write!(f, "LexingError({:?})", e),
            // Eof
            Token::Eof => write!(f, "EOF"),
        }
    }
}

// --- Indentation Handling Notes ---
// Logos itself doesn't directly handle indentation levels.
// A common approach is:
// 1. Have Logos recognize NEWLINE characters (`\n`) and TAB characters (`\t`).
// 2. Create a wrapper around the Logos lexer.
// 3. This wrapper consumes the raw tokens from Logos.
// 4. It maintains an indentation level stack (based on counts of leading tabs after newlines).
// 5. When the number of leading tabs changes compared to the stack top, the wrapper
//    emits `Indent` or `Dedent` tokens (potentially multiple Dedents).
// 6. It filters out raw NEWLINE and TAB tokens, replacing them with the Indent/Dedent stream
//    that the parser consumes.
// This logic would typically live in `ryo-parser/src/lexer.rs` or a dedicated `indent.rs` module.
```

**Key Points and Considerations:**

1.  **`logos` Crate:** This code relies heavily on the `logos` crate for defining tokens using attributes (`#[token(...)]`, `#[regex(...)]`).
2.  **Error Handling:** A basic `LexingError` enum is defined. You'll need to enhance the error variants and potentially the callbacks (`lex_string`) to return specific errors when parsing fails (e.g., for unterminated strings, invalid escapes, invalid numbers).
3.  **Literal Parsing:** The regex callbacks attempt to parse literals directly using `lex.slice().parse::<Type>().ok()`. This is convenient but might need refinement for error handling (e.g., using `Result` instead of `Option`) and potentially supporting larger integer types (`i128`?) or specific float parsing libraries if needed. The integer type is set to `i64` as an example; adjust to `isize` or your chosen default integer representation.
4.  **String Escapes:** The `lex_string` function handles basic escapes (`\n`, `\t`, `\\`, `\"`). You'll need to add logic for hex (`\xHH`) and Unicode (`\u{HHHH}`) escapes if required. Error handling for invalid escapes or unterminated strings needs to be robust.
5.  **F-Strings:** Only the start `f"` is tokenized here. The actual parsing of the f-string content (interleaving literal parts and expressions within `{}`) needs to happen in the *parser*, which will consume tokens until the closing `"`.
6.  **Indentation (CRITICAL):** As noted in the comments, `logos` doesn't handle indentation logic directly. You **must** implement a separate layer (a lexer wrapper) that processes the raw token stream (including newline and tab characters, which `logos` *can* produce if not skipped) and generates the `Indent` and `Dedent` tokens required by the parser to understand Ryo's block structure. This is a non-trivial but essential step for tab-based indentation.
7.  **`Eof` Token:** The `Token::Eof` variant isn't generated by `logos`. The lexer wrapper needs to append this token once the input stream is exhausted.
8.  **Built-in Types:** `Result`, `Optional`, `Ok`, `Err`, `Some` are included as keywords here for simplicity. An alternative is to lex them as `Token::Ident` and have the type checker/resolver identify them as built-in types later. Treating them as keywords is often easier initially.

This `token.rs` provides a strong starting point for Ryo's lexical definition. Remember that the indentation handling wrapper is the next crucial step before feeding this token stream to the parser.