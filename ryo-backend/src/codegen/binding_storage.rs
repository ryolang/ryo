//! Scoped lookup and restoration for backend binding representations.

use super::Codegen;
use cranelift_module::Module;
use ryo_core::types::StringId;

impl<M: Module> Codegen<M> {
    /// Read a dense `StringId`-indexed slot table. Out-of-range ids
    /// read as absent — codegen never interns, so every name in the
    /// TIR is in bounds, but stay tolerant like `free_binding_name`.
    pub(crate) fn read_slot<T: Copy>(table: &[Option<T>], name: StringId) -> Option<T> {
        table.get(name.raw() as usize).copied().flatten()
    }

    /// Write a dense `StringId`-indexed slot table, pushing the
    /// previous slot value onto the table's undo log so scoped
    /// emitters (`emit_scoped_body`, the if/while join handling) can
    /// restore it via `restore_slots`.
    pub(crate) fn write_slot<T: Copy>(
        table: &mut [Option<T>],
        undo: &mut Vec<(u32, Option<T>)>,
        name: StringId,
        value: Option<T>,
    ) {
        let raw = name.raw();
        // Invariant: every name in the TIR comes from the pool the
        // tables were sized from (codegen never interns), so a miss
        // here is a compiler bug — a dropped write would also
        // desynchronize the undo log.
        let slot = table
            .get_mut(raw as usize)
            .expect("binding name within slot-table range");
        undo.push((raw, *slot));
        *slot = value;
    }

    /// Restore a slot table to `mark` (a previous `undo.len()`):
    /// replay the undo log in reverse down to the mark, writing each
    /// saved old slot value back, then truncate the log.
    pub(crate) fn restore_slots<T: Copy>(
        table: &mut [Option<T>],
        undo: &mut Vec<(u32, Option<T>)>,
        mark: usize,
    ) {
        while undo.len() > mark {
            let (raw, old) = undo.pop().expect("len > mark");
            table[raw as usize] = old;
        }
    }
}
