//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Every record adapter over one exclusively owned redb database shares snapshot admission.

use redb::Database;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use uqa_storage::mvcc::{
    DatabaseId, MetadataEpoch, SnapshotBoundary, SnapshotRegistry, VersionError, VersionResult,
};

type Registries<Boundary> = HashMap<usize, (DatabaseId, Weak<SnapshotRegistry<Boundary>>)>;

pub(super) fn registry(
    database: &Arc<Database>,
    identity: DatabaseId,
) -> VersionResult<Arc<SnapshotRegistry>> {
    static REGISTRIES: OnceLock<Mutex<Registries<uqa_storage::mvcc::CommitSequence>>> =
        OnceLock::new();
    shared_registry(&REGISTRIES, database, identity)
}

pub(super) fn metadata_registry(
    database: &Arc<Database>,
    identity: DatabaseId,
) -> VersionResult<Arc<SnapshotRegistry<MetadataEpoch>>> {
    static REGISTRIES: OnceLock<Mutex<Registries<MetadataEpoch>>> = OnceLock::new();
    shared_registry(&REGISTRIES, database, identity)
}

fn shared_registry<Boundary: SnapshotBoundary>(
    cache: &OnceLock<Mutex<Registries<Boundary>>>,
    database: &Arc<Database>,
    identity: DatabaseId,
) -> VersionResult<Arc<SnapshotRegistry<Boundary>>> {
    let key = Arc::as_ptr(database) as usize;
    let mut registries = cache
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registries.retain(|_, (_, registry)| registry.strong_count() != 0);
    if let Some((old_identity, registry)) = registries.get(&key) {
        if let Some(registry) = registry.upgrade() {
            if *old_identity != identity {
                return Err(VersionError::WrongDatabase);
            }
            return Ok(registry);
        }
    }
    let registry = Arc::new(SnapshotRegistry::default());
    registries.insert(key, (identity, Arc::downgrade(&registry)));
    Ok(registry)
}
