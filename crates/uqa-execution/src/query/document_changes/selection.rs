//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered private selections retain their allocation leases with every shared view.

use super::{Arc, Change, DocId, DocumentChanges, StorageBackendResult};
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryReservation};
use uqa_storage::read_control::StorageReadControl;

pub(super) struct Selection {
    pub(super) rows: BudgetedVec<(DocId, Change)>,
    // Release the shared payload charge after its rows and their source owners.
    allocation: MemoryReservation,
}

impl Selection {
    pub(super) fn into_parts(self) -> (Vec<(DocId, Change)>, MemoryReservation, MemoryReservation) {
        let (rows, capacity) = self.rows.into_parts();
        (rows, capacity, self.allocation)
    }

    fn new(memory: &MemoryBudget, capacity: usize) -> StorageBackendResult<Self> {
        let allocation = memory.reserve(size_of::<Self>())?;
        let mut rows = BudgetedVec::new(memory);
        rows.reserve(capacity)?;
        Ok(Self { rows, allocation })
    }
}

impl DocumentChanges {
    pub(super) fn retention_control(&self, control: &StorageReadControl) -> StorageReadControl {
        let memory = self
            .selection
            .as_ref()
            .map_or(control.memory(), |selection| selection.rows.budget());
        StorageReadControl::new(memory, control.cancellation())
    }

    pub(super) fn rows(&self) -> &[(DocId, Change)] {
        self.selection
            .as_ref()
            .map_or(&[], |selection| &selection.rows)
    }

    /// The selected change of `id`, below any rows that commands staged.
    pub(super) fn selected(&self, id: DocId) -> Option<&Change> {
        let rows = self.rows();
        rows.binary_search_by_key(&id, |(id, _)| *id)
            .ok()
            .map(|index| &rows[index].1)
    }

    fn writable(
        &mut self,
        additional: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<&mut Selection> {
        control.check()?;
        if self.staged.is_some() || self.identities.is_some() {
            return Err(staged_below());
        }
        if self
            .selection
            .as_ref()
            .is_none_or(|rows| Arc::strong_count(rows) != 1)
        {
            let memory = self
                .selection
                .as_ref()
                .map_or(control.memory(), |selection| selection.rows.budget());
            let capacity = self
                .rows()
                .len()
                .checked_add(additional)
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            let mut selection = Selection::new(memory, capacity)?;
            for row in self.rows() {
                control.check()?;
                selection.rows.push(row.clone())?;
            }
            control.check()?;
            self.selection = Some(Arc::new(selection));
        }
        let selection = Arc::get_mut(self.selection.as_mut().expect("selection initialized"))
            .expect("selection uniquely owned");
        selection.rows.reserve(additional)?;
        Ok(selection)
    }

    pub(super) fn insert(
        &mut self,
        id: DocId,
        change: Change,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let position = self.rows().binary_search_by_key(&id, |(id, _)| *id);
        let selection = self.writable(usize::from(position.is_err()), control)?;
        match position {
            Ok(index) => selection.rows[index].1 = change,
            Err(index) => {
                selection.rows.push((id, change))?;
                selection.rows[index..].rotate_right(1);
            }
        }
        Ok(())
    }

    /// Merge complete ordered views before publication. Failure leaves both the original selection and its charged capacity unchanged.
    pub fn extend(
        &mut self,
        newer: Self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if self.staged.is_some()
            || newer.staged.is_some()
            || self.identities.is_some()
            || newer.identities.is_some()
        {
            return Err(staged_below());
        }
        if !newer.has_changes() {
            return Ok(());
        }
        let Some(original) = &self.selection else {
            *self = newer;
            return Ok(());
        };
        let mut count = original.rows.len();
        for (id, _) in newer.rows() {
            control.check()?;
            if self.selected(*id).is_none() {
                count = count
                    .checked_add(1)
                    .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
            }
        }
        let mut selection = Selection::new(original.rows.budget(), count)?;
        let mut old = self.rows().iter().peekable();
        for row in newer.rows() {
            while old.peek().is_some_and(|(id, _)| *id < row.0) {
                control.check()?;
                selection
                    .rows
                    .push(old.next().expect("peeked row").clone())?;
            }
            control.check()?;
            if old.peek().is_some_and(|(id, _)| *id == row.0) {
                old.next();
            }
            selection.rows.push(row.clone())?;
        }
        for row in old {
            control.check()?;
            selection.rows.push(row.clone())?;
        }
        control.check()?;
        self.selection = Some(Arc::new(selection));
        Ok(())
    }
}

/// Selected changes are older than the lazy layers, the rows a transaction changed and the rows running commands staged, so they cannot be added above those rows.
pub(super) fn staged_below() -> uqa_storage::StorageBackendError {
    uqa_storage::StorageBackendError::Other(
        "selected changes cannot be added above the rows a transaction changed or commands staged"
            .into(),
    )
}
