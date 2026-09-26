//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The exclusive physical database owner outlives every resource lease.

use redb::ReadableDatabase;
use uqa_storage::{
    mvcc::{
        ResourceLease, ResourceLeaseId, ResourceLeaseProvider, ResourceLeaseRequest, VersionError,
        VersionResult,
    },
    read_control::StorageReadControl,
};

use super::RedbRecordStore;

impl ResourceLeaseProvider for RedbRecordStore {
    fn try_acquire_resource(
        &self,
        id: ResourceLeaseId,
        request: ResourceLeaseRequest,
        control: &StorageReadControl,
    ) -> VersionResult<Option<ResourceLease>> {
        control.check()?;
        if id.database() != self.identity {
            return Err(VersionError::WrongDatabase);
        }
        let read = self.database.begin_read().map_err(super::redb_error)?;
        let metadata = read
            .open_table(super::METADATA)
            .map_err(super::redb_error)?;
        super::codec::validate_metadata(&metadata, self.identity)?;
        self.receipts
            .with_admission(&self.database, control, |leases| {
                uqa_storage::mvcc::retain_local_resource(leases, id, request, control)
            })
    }
}
