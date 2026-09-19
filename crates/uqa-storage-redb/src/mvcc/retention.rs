//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Every record adapter over one exclusively owned redb database shares snapshot admission.

use redb::Database;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use uqa_storage::mvcc::{DatabaseId, SnapshotRegistry, VersionError, VersionResult};

type Registries = HashMap<usize, (DatabaseId, Weak<SnapshotRegistry>)>;

pub(super) fn registry(
    database: &Arc<Database>,
    identity: DatabaseId,
) -> VersionResult<Arc<SnapshotRegistry>> {
    static REGISTRIES: OnceLock<Mutex<Registries>> = OnceLock::new();
    let key = Arc::as_ptr(database) as usize;
    let mut registries = REGISTRIES
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
