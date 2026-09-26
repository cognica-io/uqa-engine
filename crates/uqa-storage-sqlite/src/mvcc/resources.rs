//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resource owners use their own native admission, separate from receipt allocation and SSI.

use uqa_storage::{
    mvcc::{
        ResourceLease, ResourceLeaseId, ResourceLeaseProvider, ResourceLeaseRequest, VersionError,
        VersionResult,
    },
    read_control::StorageReadControl,
};

use super::SQLiteRecordStore;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(super) fn lease_file(
    path: &std::path::Path,
    database: uqa_storage::mvcc::DatabaseId,
) -> VersionResult<super::leases::NativeLeaseFile> {
    let uqa_storage::PersistentStorageIdentity::File(path) =
        uqa_storage::PersistentStorageIdentity::for_database_path(path)?
    else {
        unreachable!("file resource identity")
    };
    let mut sidecar = path.into_os_string();
    sidecar.push(".uqa-resource-leases");
    super::leases::NativeLeaseFile::open(
        std::path::Path::new(&sidecar),
        super::leases::LeaseNamespace {
            magic: *b"UQARSL01",
            database,
            incarnation: None,
        },
    )
}

impl ResourceLeaseProvider for SQLiteRecordStore {
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
        #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
        if let Some(path) = self.connection.database_path() {
            let file = lease_file(path, self.identity)?;
            let _admission = file.admit(control)?;
            self.with(|connection| {
                super::codec::header(connection, self.identity)?;
                Ok(())
            })?;
            let mut live = false;
            file.visit(control, &mut |tag| {
                live |= tag == id.tag(ResourceLeaseRequest::Recover)
                    || (request != ResourceLeaseRequest::Share
                        && tag == id.tag(ResourceLeaseRequest::Share));
                Ok(())
            })?;
            if live {
                return Ok(None);
            }
            let retained = file.retain_with(id.tag(request), self.connection.clone(), control)?;
            return ResourceLease::retain(id, retained, control).map(Some);
        }
        self.with(|connection| {
            super::codec::header(connection, self.identity)?;
            Ok(())
        })?;
        #[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
        if let Some(path) = self.connection.database_path() {
            let state = super::receipts::local_file_registry(path)?;
            return self
                .connection
                .with_local_receipt_admission(Some(&state), control, |leases| {
                    uqa_storage::mvcc::retain_local_resource(leases, id, request, control)
                });
        }
        self.connection
            .with_local_receipt_admission(None, control, |leases| {
                uqa_storage::mvcc::retain_local_resource(leases, id, request, control)
            })
    }
}
