//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lazy requested-key reads reuse private cursors and open a committed window on the first miss.

use std::cell::RefCell;
use uqa_core::memory::BudgetedVec;

use super::{
    BorrowedRecord, MergedRecordSnapshot, RecordKeyIterator, RecordMetadata, RecordPointVisitor,
    RecordValueVisitor,
};
use crate::mvcc::overlay::PrivateRecordSelection;
use crate::mvcc::VersionResult;
use crate::read_control::StorageReadControl;

/// Selected reads on one immutable command view. Ascending keys reuse private spill readers and seek across gaps; backwards keys restart at their own bound. Duplicates, tombstones and missing keys preserve point-read semantics.
pub struct SelectedRecordRead<'a> {
    view: &'a MergedRecordSnapshot,
    private: PrivateRecordSelection<'a>,
}

impl MergedRecordSnapshot {
    /// Prepare lazy selected reads without opening a provider window or reading a key. Each call closes its own physical read window before returning.
    pub fn selected(&self, control: &StorageReadControl) -> SelectedRecordRead<'_> {
        SelectedRecordRead {
            view: self,
            private: PrivateRecordSelection::new(&self.private, control),
        }
    }
}

impl SelectedRecordRead<'_> {
    /// Borrow requested keys in order, stopping before producing another key on a false visitor or error. Private matches go straight to the visitor. All remaining keys share the provider's read window. Key production and visitors must not reenter persistence.
    pub fn visit_values(
        &mut self,
        keys: &mut RecordKeyIterator<'_>,
        control: &StorageReadControl,
        visit: &mut RecordPointVisitor<'_>,
    ) -> VersionResult<()> {
        if self.view.private_revision().is_none() {
            return self.view.committed.visit_values(keys, control, visit);
        }
        // The provider asks for one key and delivers its result before requesting the next. Its two callbacks therefore borrow the same visitor at disjoint times.
        let visit = RefCell::new(visit);
        let mut misses = std::iter::from_fn(|| {
            next_committed_key(&mut self.private, keys, control, *visit.borrow_mut()).transpose()
        })
        .fuse();
        self.view
            .committed
            .visit_values(&mut misses, control, &mut |key, record| {
                visit.borrow_mut()(key, record)
            })
    }

    /// Read only metadata from private entries; providers with key-only access also avoid committed payloads.
    pub fn metadata(
        &mut self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordMetadata>> {
        if let Some(record) = self.private.metadata(key, control)? {
            return Ok(Some(record));
        }
        self.view.committed.metadata(key, control)
    }

    /// Borrow one value for an internal storage visitor. The visitor must not reenter persistence; after this call returns no physical read guard remains.
    pub fn visit_value(
        &mut self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut RecordValueVisitor<'_>,
    ) -> VersionResult<()> {
        if let Some(write) = self.private.read(key, control)? {
            visit(Some(BorrowedRecord {
                revision: write.expected(),
                value: write.value(),
            }))?;
            control.check()?;
            return Ok(());
        }
        self.view.committed.visit_value(key, control, visit)
    }
}

fn next_committed_key(
    private: &mut PrivateRecordSelection<'_>,
    keys: &mut RecordKeyIterator<'_>,
    control: &StorageReadControl,
    visit: &mut RecordPointVisitor<'_>,
) -> VersionResult<Option<BudgetedVec<u8>>> {
    loop {
        control.check()?;
        let Some(key) = keys.next().transpose()? else {
            return Ok(None);
        };
        let Some(write) = private.read(&key, control)? else {
            return Ok(Some(key));
        };
        let more = visit(
            &key,
            Some(BorrowedRecord {
                revision: write.expected(),
                value: write.value(),
            }),
        )?;
        control.check()?;
        if !more {
            return Ok(None);
        }
    }
}
