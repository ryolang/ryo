//! Shared test fixtures and helpers for smoke testing.

// Each integration-test binary pulls in this module via `mod common;`
// but uses only a subset of the shared helpers; the rest would trip
// `dead_code` (an error under CI's `-Dwarnings`).
#![allow(dead_code)]

mod assertions;
mod fixtures;
mod runner;

// Each binary uses only a subset of the re-exported helpers, which
// trips `unused_imports` on the globs (same rationale as `dead_code`
// above).
#[allow(unused_imports)]
pub use assertions::*;
#[allow(unused_imports)]
pub use fixtures::*;
#[allow(unused_imports)]
pub use runner::*;
