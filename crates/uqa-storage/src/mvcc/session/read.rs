//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::key_value::{KeyValueRead, KeyValueReadRevision};
use crate::mvcc::{DatabaseId, VersionError};
use crate::mvcc::{MergedRecordSnapshot, VersionResult};
use crate::read_control::{KeyValueReadVisitor, StorageReadControl};
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

    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.view
            .visit_value(key, self.control, &mut |record| {
                visit(record.and_then(|record| record.value)).map_err(Into::into)
            })
            .map_err(VersionError::into_storage_error)
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        visit_live(self.view, prefix, None, usize::MAX, self.control, visit)
            .map_err(VersionError::into_storage_error)
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
