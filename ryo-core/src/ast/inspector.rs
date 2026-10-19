//! Chumsky parser-state integration for [`Ast`]: the rewind-truncation
//! no-op `Inspector` hooks.
//!
//! Split out of `ast.rs` to stay under the file-length limit (R2).

use super::Ast;

/// Chumsky parser-state implementation: the parser builds directly
/// into the arenas via `map_with`/`foldl_with` closures that call
/// `e.state()`; the `Ast` is the arena half of the parser state
/// (see `ParseState` in `ryo-frontend`'s parser, which pairs it
/// with the intern pool).
///
/// The `Inspector` hooks are deliberately no-ops. A failed
/// alternative (`or`/`choice` backtracking, `repeated` iteration
/// rollbacks, error recovery) can push nodes before it fails, and
/// those nodes stay in the arenas as unreachable orphans: the arenas
/// are append-only and ids are stable, so a rewind can never
/// invalidate a node that a kept id still references — the failed
/// branch's ids simply never escape into a kept node. Orphans cost
/// nothing downstream: consumers reach nodes only by following ids
/// out of `top_level`, and the whole `Ast` is dropped once astgen
/// has lowered it. On valid inputs speculation almost never pushes
/// before failing (alternatives die on their first token), so the
/// orphan count is zero in practice; `ryo-frontend`'s parser tests
/// assert that over the example corpus.
///
/// The alternative — snapshotting the arena lengths in `on_save` and
/// truncating in `on_rewind`, chumsky's `TruncateState` pattern —
/// was measured to cost ~20% of total parse time
/// (`parse_large` in `ryo-frontend/benches/frontend.rs`): chumsky
/// checkpoints at every `choice`/`repeated` boundary, so the hooks
/// fire constantly even though they almost always truncate nothing.
/// Exact arenas are not worth that; the `Ast` is a transient
/// parse-to-astgen buffer, not a long-lived structure.
impl<'src, I: chumsky::input::Input<'src>> chumsky::inspector::Inspector<'src, I> for Ast {
    type Checkpoint = ();

    fn on_token(&mut self, _: &I::Token) {}

    fn on_save<'parse>(&self, _: &chumsky::input::Cursor<'src, 'parse, I>) -> Self::Checkpoint {}

    fn on_rewind<'parse>(
        &mut self,
        _: &chumsky::input::Checkpoint<'src, 'parse, I, Self::Checkpoint>,
    ) {
    }
}
