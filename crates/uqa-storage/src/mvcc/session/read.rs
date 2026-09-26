//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::key_value::{KeyValueRead, KeyValueReadRevision};
use crate::mvcc::{DatabaseId, VersionError};
use crate::mvcc::{MergedRecordSnapshot, VersionResult};
use crate::read_control::{KeyReadVisitor, KeyValueReadVisitor, StorageReadControl};
use crate::{read_control::ValueReadVisitor, StorageBackendResult};
use uqa_core::memory::BudgetedVec;

pub(super) struct RecordRead<'a> {
    pub(super) view: &'a MergedRecordSnapshot,
    pub(super) database: DatabaseId,
    pub(super) control: &'a StorageReadControl,
}

impl KeyValueRead for RecordRead<'_> {
    fn control(&self) -> &StorageReadControl {
        self.control
    }

    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.control.check()?;
        let mut private = None;
        for prefix in prefixes {
            let mut after = BudgetedVec::new(self.control.memory());
            loop {
                let page = self
                    .view
                    .private_keys(
                        prefix,
                        (!after.is_empty()).then_some(&*after),
                        64,
                        self.control,
                    )
                    .map_err(VersionError::into_storage_error)?;
                let Some(last) = page.last() else { break };
                after.clear();
                after.extend_from_slice(last.key())?;
                for record in page.iter() {
                    private = private.max(Some(record.revision()));
                }
            }
        }
        Ok(KeyValueReadRevision::records(
            self.database,
            self.view.sequence(),
            private,
        ))
    }

    fn record_revision(&self, key: &[u8]) -> StorageBackendResult<Option<KeyValueReadRevision>> {
        self.control.check()?;
        let metadata = self
            .view
            .metadata(key, self.control)
            .map_err(VersionError::into_storage_error)?;
        let Some(metadata) = metadata.filter(|record| record.live) else {
            self.control.check()?;
            return Ok(None);
        };
        let private = self
            .view
            .private_keys(key, None, 1, self.control)
            .map_err(VersionError::into_storage_error)?;
        let private = private
            .first()
            .filter(|record| record.key() == key)
            .map(crate::mvcc::PrivateRecordKey::revision);
        self.control.check()?;
        Ok(Some(KeyValueReadRevision::records(
            self.database,
            metadata
                .revision
                .unwrap_or(crate::mvcc::CommitSequence::INITIAL),
            private,
        )))
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
    fn revision(&self, prefixes: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.read().revision(prefixes)
    }
    fn record_revision(&self, key: &[u8]) -> StorageBackendResult<Option<KeyValueReadRevision>> {
        self.read().record_revision(key)
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
