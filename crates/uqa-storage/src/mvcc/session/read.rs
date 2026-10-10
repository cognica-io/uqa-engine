//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::key_value::{
    KeyPresenceVisitor, KeyValuePointVisitor, KeyValueRead, KeyValueReadKeyIterator,
    KeyValueReadRevision,
};
use crate::mvcc::{DatabaseId, VersionError};
use crate::mvcc::{MergedRecordSnapshot, VersionResult};
use crate::read_control::{KeyReadVisitor, KeyValueReadVisitor, StorageReadControl};
use crate::{read_control::ValueReadVisitor, StorageBackendResult};

/// Borrowed Key/Value adapter over an already captured committed/private record view. It does not open or advance a session.
pub struct RecordRead<'a> {
    pub(super) view: &'a MergedRecordSnapshot,
    pub(super) database: DatabaseId,
    pub(super) control: &'a StorageReadControl,
}

impl<'a> RecordRead<'a> {
    pub fn new(
        view: &'a MergedRecordSnapshot,
        database: DatabaseId,
        control: &'a StorageReadControl,
    ) -> Self {
        Self {
            view,
            database,
            control,
        }
    }
}

impl KeyValueRead for RecordRead<'_> {
    fn control(&self) -> &StorageReadControl {
        self.control
    }

    fn marker_fingerprint(&self, key: &[u8]) -> StorageBackendResult<Option<[u8; 32]>> {
        self.view
            .marker_fingerprint(self.database, key, self.control)
            .map(Some)
    }

    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.control.check()?;
        let mut private = None;
        for prefix in prefixes {
            let mut cursor = self
                .view
                .private_cursor(prefix, None, self.control)
                .map_err(VersionError::into_storage_error)?;
            while let Some(record) = cursor
                .next(self.control)
                .map_err(VersionError::into_storage_error)?
            {
                private = private.max(Some(record.revision()));
            }
        }
        Ok(KeyValueReadRevision::records(
            self.database,
            self.view.sequence(),
            private,
        ))
    }

    fn record_revision(&self, key: &[u8]) -> StorageBackendResult<Option<KeyValueReadRevision>> {
        self.view
            .record_revision(self.database, key, self.control)
            .map(|revision| revision.map(KeyValueReadRevision::record))
            .map_err(VersionError::into_storage_error)
    }

    fn retained_source(
        &self,
        key: &[u8],
    ) -> StorageBackendResult<Option<std::sync::Arc<dyn KeyValueRead + Send + Sync>>> {
        self.view
            .retained_source(key, self.control)
            .map_err(VersionError::into_storage_error)
    }

    fn retain(
        &self,
        _prefixes: &[&[u8]],
    ) -> StorageBackendResult<std::sync::Arc<dyn KeyValueRead + Send + Sync>> {
        self.control.check()?;
        let memory = self
            .control
            .memory()
            .reserve(std::mem::size_of::<RetainedRecordRead>())?;
        Ok(std::sync::Arc::new(RetainedRecordRead {
            view: self
                .view
                .try_clone()
                .map_err(VersionError::into_storage_error)?,
            database: self.database,
            control: self.control.clone(),
            _memory: memory,
        }))
    }

    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_value_budgeted(key, self.control, visit)
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_prefix_after(prefix, None, usize::MAX, self.control, visit)
    }

    fn visit_value_budgeted(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.view
            .visit_value(key, control, &mut |record| {
                visit(record.and_then(|record| record.value)).map_err(Into::into)
            })
            .map_err(VersionError::into_storage_error)
    }

    fn visit_values(
        &self,
        keys: &mut KeyValueReadKeyIterator<'_>,
        visit: &mut KeyValuePointVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let mut keys = keys.map(|key| key.map_err(VersionError::from));
        self.view
            .visit_values(&mut keys, self.control, &mut |key, record| {
                visit(key, record.and_then(|record| record.value)).map_err(Into::into)
            })
            .map_err(VersionError::into_storage_error)
    }

    fn visit_key_presence(
        &self,
        keys: &mut KeyValueReadKeyIterator<'_>,
        visit: &mut KeyPresenceVisitor<'_>,
    ) -> StorageBackendResult<()> {
        let mut selected = self.view.selected(self.control);
        loop {
            self.control.check()?;
            let Some(key) = keys.next() else {
                return Ok(());
            };
            let key = key?;
            let present = selected
                .metadata(&key, self.control)
                .map_err(VersionError::into_storage_error)?
                .is_some_and(|record| record.live);
            let more = visit(&key, present)?;
            self.control.check()?;
            if !more {
                return Ok(());
            }
        }
    }

    fn visit_value_bounded(
        &self,
        key: &[u8],
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.view
            .visit_value_bounded(key, max_bytes, control, &mut |record| {
                visit(record.and_then(|record| record.value)).map_err(Into::into)
            })
            .map_err(VersionError::into_storage_error)
    }

    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        visit_live(self.view, prefix, after, limit, control, visit)
            .map_err(VersionError::into_storage_error)
    }

    fn visit_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        if limit == 0 {
            return Ok(());
        }
        let mut count = 0;
        self.view
            .visit_keys(prefix, after, usize::MAX, control, &mut |key, record| {
                if record.live {
                    visit(key)?;
                    count += 1;
                }
                Ok(count < limit)
            })
            .map_err(VersionError::into_storage_error)
    }

    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        control.check()?;
        let mut found = false;
        self.view
            .visit_keys(prefix, None, usize::MAX, control, &mut |_, record| {
                found = record.live;
                Ok(!found)
            })
            .map_err(VersionError::into_storage_error)?;
        Ok(found)
    }
}

pub(super) fn visit_live(
    view: &MergedRecordSnapshot,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
    visit: &mut KeyValueReadVisitor<'_>,
) -> VersionResult<()> {
    control.cancellation().check()?;
    if limit == 0 {
        return Ok(());
    }
    let mut count = 0;
    view.visit_prefix(prefix, after, usize::MAX, control, &mut |key, record| {
        if let Some(value) = record.value {
            visit(key, value)?;
            count += 1;
        }
        Ok(count < limit)
    })
}

struct RetainedRecordRead {
    view: MergedRecordSnapshot,
    database: DatabaseId,
    control: StorageReadControl,
    _memory: uqa_core::memory::MemoryReservation,
}
impl RetainedRecordRead {
    fn read(&self) -> RecordRead<'_> {
        RecordRead {
            view: &self.view,
            database: self.database,
            control: &self.control,
        }
    }
}
impl KeyValueRead for RetainedRecordRead {
    fn control(&self) -> &StorageReadControl {
        &self.control
    }
    fn marker_fingerprint(&self, key: &[u8]) -> StorageBackendResult<Option<[u8; 32]>> {
        self.read().marker_fingerprint(key)
    }
    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.read().revision(prefixes)
    }
    fn record_revision(&self, key: &[u8]) -> StorageBackendResult<Option<KeyValueReadRevision>> {
        self.read().record_revision(key)
    }
    fn retained_source(
        &self,
        key: &[u8],
    ) -> StorageBackendResult<Option<std::sync::Arc<dyn KeyValueRead + Send + Sync>>> {
        self.read().retained_source(key)
    }
    fn retain(
        &self,
        prefixes: &[&[u8]],
    ) -> StorageBackendResult<std::sync::Arc<dyn KeyValueRead + Send + Sync>> {
        self.read().retain(prefixes)
    }
    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read().visit_value(key, visit)
    }
    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read().visit_prefix(prefix, visit)
    }
    fn visit_values(
        &self,
        keys: &mut KeyValueReadKeyIterator<'_>,
        visit: &mut KeyValuePointVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read().visit_values(keys, visit)
    }
    fn visit_key_presence(
        &self,
        keys: &mut KeyValueReadKeyIterator<'_>,
        visit: &mut KeyPresenceVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read().visit_key_presence(keys, visit)
    }
    fn visit_value_budgeted(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read().visit_value_budgeted(key, control, visit)
    }
    fn visit_value_bounded(
        &self,
        key: &[u8],
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read()
            .visit_value_bounded(key, max_bytes, control, visit)
    }
    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read()
            .visit_prefix_after(prefix, after, limit, control, visit)
    }
    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.read().contains_prefix_budgeted(prefix, control)
    }

    fn visit_keys_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.read()
            .visit_keys_after(prefix, after, limit, control, visit)
    }
}
