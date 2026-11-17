pub mod astgen;
pub mod builtins;
pub mod grammar;
pub mod indent;
pub mod lexer;
pub mod ownership;
pub mod parse;
pub mod pipeline;
pub mod sema;
pub mod spans;

#[cfg(test)]
mod lexer_diagnostic_tests;
#[cfg(test)]
mod lexer_source_identity_tests;
#[cfg(test)]
mod parse_source_identity_tests;
