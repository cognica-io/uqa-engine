//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Requested identities reuse ordered readers without scanning gaps between sparse keys.

use std::ops::Bound;

use uqa_core::memory::BudgetedVec;

use super::{PrivateRecordSnapshot, TieredChange, TieredCursor};
use crate::mvcc::{PreparedRecordWrite, RecordMetadata, VersionResult};
use crate::read_control::StorageReadControl;

pub(in crate::mvcc) struct PrivateRecordSelection<'a> {
    snapshot: &'a PrivateRecordSnapshot,
    cursor: Option<TieredCursor<'a>>,
    current: Option<TieredChange<'a>>,
    last: BudgetedVec<u8>,
    started: bool,
    failed: bool,
}

impl<'a> PrivateRecordSelection<'a> {
    pub(in crate::mvcc) fn new(
        snapshot: &'a PrivateRecordSnapshot,
        control: &StorageReadControl,
    ) -> Self {
        Self {
            snapshot,
            cursor: None,
            current: None,
            last: BudgetedVec::new(control.memory()),
            started: false,
            failed: false,
        }
    }

    pub(in crate::mvcc) fn metadata(
        &mut self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordMetadata>> {
        Ok(self.select(key, control)?.map(TieredChange::metadata))
    }

    pub(in crate::mvcc) fn read(
        &mut self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<PreparedRecordWrite>> {
        self.select(key, control)?
            .map(|change| change.write(control))
            .transpose()
    }

    fn select(
        &mut self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<&TieredChange<'a>>> {
        if self.failed {
            return Err(crate::StorageBackendError::Other(
                "private record selection cannot resume after an error".into(),
            )
            .into());
        }
        if let Err(error) = self.advance(key, control) {
            self.cursor = None;
            self.current = None;
            self.failed = true;
            return Err(error);
        }
        Ok(self.current.as_ref().filter(|change| change.key() == key))
    }

    fn advance(&mut self, key: &[u8], control: &StorageReadControl) -> VersionResult<()> {
        control.check()?;
        if self.snapshot.revision.is_none() {
            return Ok(());
        }
        if !self.started || key < &*self.last {
            // Release readers before resetting so a backwards request cannot retain two sets of decoded blocks.
            self.current = None;
            self.cursor = None;
            let mut cursor = TieredCursor::new(
                Some(&self.snapshot.records),
                &self.snapshot.runs,
                Bound::Included(key),
                control,
            )?;
            self.current = cursor.next(control)?;
            self.cursor = Some(cursor);
            self.started = true;
        } else if self
            .current
            .as_ref()
            .is_some_and(|change| change.key() < key)
        {
            self.current = None;
            self.current = self
                .cursor
                .as_mut()
                .expect("started selection")
                .seek_to(key, control)?;
        }
        if self.current.is_none() {
            self.cursor = None;
        }
        self.last.clear();
        self.last.extend_from_slice(key)?;
        Ok(())
    }
}
