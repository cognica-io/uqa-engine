//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate original conditions in one ordered metadata window before publishing a refreshed view.

use crate::mvcc::{
    CommittedRecordSnapshot, PreparedRecordCommit, PreparedWriteCursor, PreparedWriteMetadata,
    RecordMetadata, RecordMetadataRequests, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;

#[cfg(test)]
mod tests;

pub(super) fn validate(
    records: &PreparedRecordCommit,
    current: &dyn CommittedRecordSnapshot,
    control: &StorageReadControl,
) -> VersionResult<()> {
    current.visit_metadata(
        &mut Conditions {
            input: records.writes(),
            pending: None,
            mutation: 0,
            control,
        },
        control,
    )
}

struct Conditions<'a> {
    input: PreparedWriteCursor<'a>,
    pending: Option<PreparedWriteMetadata>,
    mutation: usize,
    control: &'a StorageReadControl,
}

impl RecordMetadataRequests for Conditions<'_> {
    fn advance(&mut self) -> VersionResult<bool> {
        self.pending = self.input.next_metadata(self.control)?;
        Ok(self.pending.is_some())
    }

    fn key(&self) -> &[u8] {
        self.pending.as_ref().expect("an active condition").key()
    }

    fn accept(&mut self, metadata: Option<RecordMetadata>) -> VersionResult<()> {
        let expected = self.pending.take().expect("an active condition").expected();
        let actual = metadata.and_then(|record| record.revision);
        if actual != expected {
            return Err(VersionError::WriteConflict {
                mutation: self.mutation,
                expected,
                actual,
            });
        }
        self.mutation += 1;
        Ok(())
    }
}
