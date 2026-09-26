//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    maintenance::{index, scores},
    restore::{verify_diskann_restore_source, verify_diskann_restored},
};
use crate::{
    diskann_index::{build::DiskANNTemporaryBudget, format::DiskANNGeneration},
    key_value::{conformance::expect, diskann::conformance::verify_selected_corruption},
    mvcc::VersionError,
    read_control::StorageReadControl,
    KeyValueStore, PersistentStorageBackend, StorageBackendError, StorageBackendResult,
    VectorIndexOpenMode,
};

/// Seed a disposable backend and corrupt each selected physical artifact in its mapped record store. Fresh runtime restore/search must fail, old snapshots must retain their complete results, and no query may repair or replace the damaged generation.
pub fn verify_diskann_selected_corruption(
    backend: &dyn PersistentStorageBackend,
    records: &dyn KeyValueStore,
) -> StorageBackendResult<DiskANNGeneration> {
    let generation = verify_diskann_restore_source(backend)?;
    let control = StorageReadControl::with_limit(1 << 22);
    let temporary = DiskANNTemporaryBudget::new(1 << 20);
    let session = backend.open_controlled_session(&control)?;
    let live = index(
        &*session.backend,
        &temporary,
        &control,
        VectorIndexOpenMode::Restore,
    )?;
    let retained = live.snapshot()?;
    verify_selected_corruption(records, generation, &mut || {
        let attempt = backend.open_controlled_session(&control)?;
        let result = index(
            &*attempt.backend,
            &temporary,
            &control,
            VectorIndexOpenMode::Restore,
        )
        .and_then(|index| index.search_knn(&[1.0, 0.0], 10));
        let rejected = match &result {
            Err(StorageBackendError::Other(message)) => message.starts_with("invalid DiskANN "),
            Err(StorageBackendError::Backend { source, .. }) => matches!(
                source.downcast_ref::<VersionError>(),
                Some(VersionError::InvalidEncoding(_))
            ),
            _ => false,
        };
        expect(
            rejected,
            &format!(
                "a fresh restore or graph query must report invalid selected data: {:?}",
                result.as_ref().map(|_| ())
            ),
        )?;
        scores(&*retained, &[(1, -1.0), (3, -1.0), (4, 0.0), (5, 1.0)])
    })?;
    verify_diskann_restored(backend, generation)?;
    Ok(generation)
}
