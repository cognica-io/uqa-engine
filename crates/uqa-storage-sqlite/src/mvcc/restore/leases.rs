//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exclude legacy snapshot and participant leases in their ordinary acquisition order.

use uqa_storage::{
    mvcc::{DatabaseId, SerializableGraph, VersionResult},
    read_control::StorageReadControl,
};

use crate::ManagedConnection;

pub(super) struct Exclusion {
    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    _snapshots: crate::mvcc::leases::NativeLeaseAdmission,
    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    _participants: crate::mvcc::leases::NativeLeaseAdmission,
    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    _receipts: crate::mvcc::leases::NativeLeaseAdmission,
}

pub(super) fn exclude(
    connection: &ManagedConnection,
    identity: DatabaseId,
    graph: &SerializableGraph,
    control: &StorageReadControl,
) -> VersionResult<Exclusion> {
    control.check()?;
    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    {
        let path = connection.database_path().expect("file restore owner");
        let participants = crate::mvcc::serializable::lease_file(path, graph)?;
        let participants = empty(&participants, control)?;
        let receipts = crate::mvcc::receipts::lease_file(path, identity)?;
        let receipts = empty(&receipts, control)?;
        let uqa_storage::PersistentStorageIdentity::File(path) =
            uqa_storage::PersistentStorageIdentity::for_database_path(path)?
        else {
            unreachable!("file restore identity")
        };
        let snapshots = crate::mvcc::retention::lease_file(&path, identity)?;
        let snapshots = empty(&snapshots, control)?;
        Ok(Exclusion {
            _snapshots: snapshots,
            _participants: participants,
            _receipts: receipts,
        })
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
    {
        // A local runtime cannot retain objects from a previous binary. Every current retained object owns the primary pool's physical lifetime lease.
        let _ = (connection, identity, graph);
        Ok(Exclusion {})
    }
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
fn empty(
    file: &crate::mvcc::leases::NativeLeaseFile,
    control: &StorageReadControl,
) -> VersionResult<crate::mvcc::leases::NativeLeaseAdmission> {
    let admission = file.admit(control)?;
    let mut live = false;
    file.visit(control, &mut |_| {
        live = true;
        Ok(())
    })?;
    if live {
        return Err(uqa_storage::mvcc::VersionError::Storage(
            crate::SQLiteError::DatabaseRestoreBusy.into(),
        ));
    }
    Ok(admission)
}
