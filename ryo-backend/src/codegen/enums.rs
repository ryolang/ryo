//! Enum codegen (M11) — home for all enum/variant emission: enum
//! construction, tag/union slot layout, match dispatch, Eq, and Debug.
//!
//! Like structs (see `structs.rs`), enum values are memory-first
//! aggregates: every value lives in a stack slot, `ValueRepr`-style
//! dispatch goes through the slot address, and copies/drops are
//! field-wise. This module is wired in ahead of the emission work to
//! keep every file under the 2000-line CI cap
//! (`scripts/check_file_length.sh`); the emission arms land here.

use cranelift_module::Module;

use super::Codegen;

impl<M: Module> Codegen<M> {}
