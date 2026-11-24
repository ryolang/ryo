//! Scoped lookup and restoration for backend binding representations.

use super::Codegen;
use cranelift_module::Module;
use ryo_core::tir::BindingId;

impl<M: Module> Codegen<M> {
    /// Read a dense `BindingId`-indexed slot table. Validated backend
    /// input keeps every ID in bounds; an absent entry reads as `None`.
    pub(crate) fn read_slot<T: Copy>(table: &[Option<T>], binding: BindingId) -> Option<T> {
        table.get(binding.index()).copied().flatten()
    }

    /// Write a dense `BindingId`-indexed slot table, pushing the
    /// previous slot value onto the table's undo log so scoped
    /// emitters (`take_slot_marks`/`restore_slot_marks` in
    /// `control.rs`, the if/while join handling) can restore it via
    /// `restore_slots`.
    pub(crate) fn write_slot<T: Copy>(
        table: &mut [Option<T>],
        undo: &mut Vec<(u32, Option<T>)>,
        binding: BindingId,
        value: Option<T>,
    ) {
        let raw = binding.raw();
        // The direct-input boundary validates every binding against the
        // current TIR table used to size these slots. A miss here is a
        // compiler bug; a dropped write would desynchronize the undo log.
        let slot = table
            .get_mut(raw as usize)
            .expect("binding within slot-table range");
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
