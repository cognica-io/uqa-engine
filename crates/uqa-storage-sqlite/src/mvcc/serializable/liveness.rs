//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained SSI participants use native process leases or their exclusively owned local registry.

use std::sync::Arc;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    mvcc::{
        admit_serializable, LocalSerializableLeases, SerializableCoordinator, SerializableGraph,
        SerializableLeases, SerializableOperation, SerializableParticipant,
        SerializableTransactionId, VersionResult,
    },
    read_control::{CancellationToken, StorageReadControl},
};

use super::SQLiteRecordStore;
use crate::connection::ownership::DatabaseOwner;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
use super::super::leases::{LeaseNamespace, NativeLeaseAdmission, NativeLeaseFile};

enum Liveness {
    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    Native {
        file: NativeLeaseFile,
        _admission: NativeLeaseAdmission,
        live: BudgetedVec<u64>,
        owner: Option<Arc<DatabaseOwner>>,
    },
    Local {
        leases: Arc<LocalSerializableLeases>,
        owner: Option<Arc<DatabaseOwner>>,
    },
}

impl Liveness {
    fn open(
        store: &SQLiteRecordStore,
        graph: &SerializableGraph,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
        if let Some(path) = store.connection.database_path() {
            let file = lease_file(path, graph)?;
            let admission = file.admit(control)?;
            let mut live = BudgetedVec::new(control.memory());
            file.visit(control, &mut |allocation| {
                live.push(allocation)?;
                Ok(())
            })?;
            live.sort_unstable();
            return Ok(Self::Native {
                file,
                _admission: admission,
                live,
                owner: store.connection.database_owner(),
            });
        }
        #[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
        if let Some(path) = store.connection.database_path() {
            return Ok(Self::Local {
                leases: local_file_registry(path, control)?,
                owner: store.connection.database_owner(),
            });
        }
        let _ = graph;
        Ok(Self::Local {
            leases: store.connection.serializable_local_leases(control),
            owner: store.connection.database_owner(),
        })
    }
}

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(in crate::mvcc) fn lease_file(
    path: &std::path::Path,
    graph: &SerializableGraph,
) -> VersionResult<NativeLeaseFile> {
    let uqa_storage::PersistentStorageIdentity::File(path) =
        uqa_storage::PersistentStorageIdentity::for_database_path(path)?
    else {
        unreachable!("database file identity")
    };
    let mut sidecar = path.into_os_string();
    sidecar.push(".uqa-serializable-leases");
    NativeLeaseFile::open(
        std::path::Path::new(&sidecar),
        LeaseNamespace {
            magic: *b"UQASSL01",
            database: graph.database(),
            incarnation: Some(graph.coordinator()),
        },
    )
}

impl SerializableLeases for Liveness {
    fn is_alive(
        &self,
        id: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        control.cancellation().check()?;
        Ok(match self {
            #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
            Self::Native { live, .. } => live.binary_search(&id.allocation()).is_ok(),
            Self::Local { leases, .. } => leases.is_alive(id),
        })
    }

    fn retain(
        &self,
        id: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableParticipant> {
        match self {
            #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
            Self::Native { file, owner, .. } => SerializableParticipant::retain(
                id,
                file.retain_with(id.allocation(), owner.clone(), control)?,
                control,
            ),
            Self::Local { leases, owner } => leases.retain_with(id, owner.clone(), control),
        }
    }

    fn reclaim(&self) {
        match self {
            #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
            Self::Native { .. } => {}
            Self::Local { leases, .. } => leases.reclaim(),
        }
    }
}

impl SQLiteRecordStore {
    /// Reconcile abandoned participants, retain a new participant and capture its fixed data view under shared admission. The returned handle must outlive every operation and retained nested/pinned view attributed to it. All publishers must use the same shared admission; this does not by itself enable SQL SSI.
    pub fn admit_serializable<T>(
        &self,
        read_only: bool,
        control: &StorageReadControl,
        capture: impl FnOnce() -> VersionResult<T>,
    ) -> VersionResult<(SerializableParticipant, T)> {
        admit_serializable(self, read_only, control, capture)
    }

    /// Reconcile receipts and retire dead retained participants without admitting a new one. Untracked legacy actors remain manually owned. A missing prepared receipt is an error, not evidence of abort.
    pub fn recover_serializable(&self, control: &StorageReadControl) -> VersionResult<()> {
        self.recover_serializable_participants(control)
    }
}

impl SerializableCoordinator for SQLiteRecordStore {
    fn with_serializable_admission(
        &self,
        control: &StorageReadControl,
        operation: &mut SerializableOperation<'_>,
    ) -> VersionResult<()> {
        let mut held = self.serializable_admission(control)?;
        let liveness = Liveness::open(self, held.graph(), control)?;
        let result = operation(held.graph_mut(), &liveness);
        // A later cancellation/error cannot discard earlier confirmed physical outcomes. Serialization itself allocates no second encoded buffer; no SQL/callback is retried.
        let finish = StorageReadControl::new(control.memory(), &CancellationToken::new());
        held.persist(&finish)?;
        result
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
fn local_file_registry(
    path: &std::path::Path,
    control: &StorageReadControl,
) -> VersionResult<Arc<LocalSerializableLeases>> {
    use parking_lot::Mutex;
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{OnceLock, Weak},
    };
    type Registries = HashMap<PathBuf, Weak<LocalSerializableLeases>>;
    static REGISTRIES: OnceLock<Mutex<Registries>> = OnceLock::new();
    let uqa_storage::PersistentStorageIdentity::File(path) =
        uqa_storage::PersistentStorageIdentity::for_database_path(path)?
    else {
        unreachable!("database file identity")
    };
    let mut registries = REGISTRIES.get_or_init(Mutex::default).lock();
    registries.retain(|_, leases| leases.strong_count() != 0);
    if let Some(leases) = registries.get(&path).and_then(Weak::upgrade) {
        return Ok(leases);
    }
    let leases = Arc::new(LocalSerializableLeases::new(control.memory()));
    registries.insert(path, Arc::downgrade(&leases));
    Ok(leases)
}
