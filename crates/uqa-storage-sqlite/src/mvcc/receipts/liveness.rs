//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Receipt-owner admission publishes process liveness before the Pending allocation.

use uqa_storage::{
    mvcc::{SerializableLeases, VersionResult},
    read_control::StorageReadControl,
};

use super::super::SQLiteRecordStore;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
use super::super::leases::{LeaseNamespace, NativeLeaseFile};

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(in crate::mvcc) fn lease_file(
    path: &std::path::Path,
    database: uqa_storage::mvcc::DatabaseId,
) -> VersionResult<NativeLeaseFile> {
    let uqa_storage::PersistentStorageIdentity::File(path) =
        uqa_storage::PersistentStorageIdentity::for_database_path(path)?
    else {
        unreachable!("file receipt identity")
    };
    let mut sidecar = path.into_os_string();
    sidecar.push(".uqa-receipt-leases");
    NativeLeaseFile::open(
        std::path::Path::new(&sidecar),
        LeaseNamespace {
            magic: *b"UQARCL01",
            database,
            incarnation: None,
        },
    )
}

impl SQLiteRecordStore {
    pub(in crate::mvcc) fn with_receipt_admission<T>(
        &self,
        control: &StorageReadControl,
        operation: impl FnOnce(&dyn SerializableLeases) -> VersionResult<T>,
    ) -> VersionResult<T> {
        #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
        if let Some(path) = self.connection.database_path() {
            use uqa_core::memory::BudgetedVec;
            let file = lease_file(path, self.identity)?;
            let _admission = file.admit(control)?;
            let mut live = BudgetedVec::new(control.memory());
            file.visit(control, &mut |allocation| {
                live.push(allocation)?;
                Ok(())
            })?;
            live.sort_unstable();
            return operation(&NativeLeases {
                file,
                live,
                store: self,
            });
        }
        #[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
        if let Some(path) = self.connection.database_path() {
            let state = local_file_registry(path)?;
            return self
                .connection
                .with_local_receipt_admission(Some(&state), control, operation);
        }
        self.connection
            .with_local_receipt_admission(None, control, operation)
    }
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
struct NativeLeases<'a> {
    file: NativeLeaseFile,
    live: uqa_core::memory::BudgetedVec<u64>,
    store: &'a SQLiteRecordStore,
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
impl SerializableLeases for NativeLeases<'_> {
    fn retain(
        &self,
        id: uqa_storage::mvcc::SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<uqa_storage::mvcc::SerializableParticipant> {
        uqa_storage::mvcc::SerializableParticipant::retain(
            id,
            self.file
                .retain_with(id.allocation(), self.store.connection.clone(), control)?,
            control,
        )
    }

    fn is_alive(
        &self,
        id: uqa_storage::mvcc::SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        control.check()?;
        Ok(self.live.binary_search(&id.allocation()).is_ok())
    }

    fn reclaim(&self) {}
}

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
fn local_file_registry(
    path: &std::path::Path,
) -> VersionResult<std::sync::Arc<uqa_storage::mvcc::LocalSerializableState>> {
    use parking_lot::Mutex;
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Arc, OnceLock, Weak},
    };
    use uqa_storage::mvcc::LocalSerializableState;
    type Registries = HashMap<PathBuf, Weak<LocalSerializableState>>;
    static REGISTRIES: OnceLock<Mutex<Registries>> = OnceLock::new();
    let uqa_storage::PersistentStorageIdentity::File(path) =
        uqa_storage::PersistentStorageIdentity::for_database_path(path)?
    else {
        unreachable!("file receipt identity")
    };
    let mut registries = REGISTRIES.get_or_init(Mutex::default).lock();
    registries.retain(|_, state| state.strong_count() != 0);
    if let Some(state) = registries.get(&path).and_then(Weak::upgrade) {
        return Ok(state);
    }
    let state = Arc::new(LocalSerializableState::default());
    registries.insert(path, Arc::downgrade(&state));
    Ok(state)
}
