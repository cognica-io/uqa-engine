//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered private entries retain their exact location and load values only on request.

use std::ops::Bound;

use super::{PrivateRecordRevision, PrivateRecordSnapshot, TieredChange, TieredCursor};
use crate::mvcc::{PreparedRecordWrite, RecordMetadata, VersionResult};
use crate::read_control::StorageReadControl;
use uqa_core::memory::BudgetedVec;

/// An ordered cursor on one immutable private root, including tombstones. Advancing shares spill readers instead of looking each key up again.
pub struct PrivateRecordCursor<'a> {
    changes: Option<TieredCursor<'a>>,
    prefix: BudgetedVec<u8>,
    failed: bool,
}

/// One private entry, borrowed from its resident root or retaining its spill location. Metadata access never loads its payload.
pub struct PrivateRecordEntry<'a> {
    change: TieredChange<'a>,
}

impl PrivateRecordSnapshot {
    /// Select this prefix strictly after `after` on the retained private root. The cursor and entries cannot outlive the root; a later command or undo never changes their records.
    pub fn cursor(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        control: &StorageReadControl,
    ) -> VersionResult<PrivateRecordCursor<'_>> {
        control.check()?;
        if self.records.is_empty() && self.runs.is_empty() {
            return Ok(PrivateRecordCursor {
                changes: None,
                prefix: BudgetedVec::new(control.memory()),
                failed: false,
            });
        }
        let start = after
            .filter(|after| *after >= prefix)
            .map_or(Bound::Included(prefix), Bound::Excluded);
        let changes =
            TieredCursor::for_prefix(Some(&self.records), &self.runs, prefix, start, control)?;
        let mut retained_prefix = BudgetedVec::new(control.memory());
        retained_prefix.extend_from_slice(prefix)?;
        Ok(PrivateRecordCursor {
            changes: Some(changes),
            prefix: retained_prefix,
            failed: false,
        })
    }
}

impl<'a> PrivateRecordCursor<'a> {
    /// Advance once, preserving newest-private precedence without reading values. Exhaustion or error releases active spill readers even when the cursor remains alive. An error is terminal; later calls reject reuse rather than skipping a partially consumed entry.
    pub fn next(
        &mut self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<PrivateRecordEntry<'a>>> {
        if self.failed {
            return Err(crate::StorageBackendError::Other(
                "private record cursor cannot resume after an error".into(),
            )
            .into());
        }
        let next = (|| {
            control.check()?;
            self.changes
                .as_mut()
                .map_or(Ok(None), |changes| changes.next(control))
        })();
        match next {
            Ok(Some(change)) if change.key().starts_with(&self.prefix) => {
                Ok(Some(PrivateRecordEntry { change }))
            }
            Ok(_) => {
                self.changes = None;
                Ok(None)
            }
            Err(error) => {
                self.changes = None;
                self.failed = true;
                Err(error)
            }
        }
    }
}

impl PrivateRecordEntry<'_> {
    pub fn key(&self) -> &[u8] {
        self.change.key()
    }

    pub fn revision(&self) -> PrivateRecordRevision {
        self.change.identity()
    }

    pub fn metadata(&self) -> RecordMetadata {
        self.change.metadata()
    }

    /// Read this entry's value under the caller's allowance, without seeking its identity again. Resident payloads retain their original reservation.
    pub fn read(&self, control: &StorageReadControl) -> VersionResult<PreparedRecordWrite> {
        control.check()?;
        self.change.write(control)
    }
}
