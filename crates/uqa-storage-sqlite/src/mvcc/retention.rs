//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select shared logical snapshot retention for the `SQLite` database owner.

use std::sync::Arc;
use uqa_storage::mvcc::{DatabaseId, SnapshotRegistry, VersionResult};

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod file;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(super) use file::lease_file;

pub(super) fn registry(
    connection: &crate::ManagedConnection,
    identity: DatabaseId,
) -> VersionResult<Arc<SnapshotRegistry>> {
    #[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
    if let Some(path) = connection.database_path() {
        return file::registry(path, identity);
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
    if let Some(path) = connection.database_path() {
        return local_file_registry(path, identity);
    }
    connection.snapshot_registry(identity)
}

fn database_path(path: &std::path::Path) -> VersionResult<std::path::PathBuf> {
    match uqa_storage::PersistentStorageIdentity::for_database_path(path)? {
        uqa_storage::PersistentStorageIdentity::File(path) => Ok(path),
        uqa_storage::PersistentStorageIdentity::Opaque(_) => unreachable!("file path identity"),
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "emscripten")))))]
fn local_file_registry(
    path: &std::path::Path,
    identity: DatabaseId,
) -> VersionResult<Arc<SnapshotRegistry>> {
    use parking_lot::Mutex;
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{OnceLock, Weak},
    };
    type Registries = HashMap<PathBuf, (DatabaseId, Weak<SnapshotRegistry>)>;
    static REGISTRIES: OnceLock<Mutex<Registries>> = OnceLock::new();
    let path = database_path(path)?;
    let mut registries = REGISTRIES.get_or_init(Mutex::default).lock();
    registries.retain(|_, (_, registry)| registry.strong_count() != 0);
    if let Some((old_identity, registry)) = registries.get(&path) {
        if let Some(registry) = registry.upgrade() {
            if *old_identity != identity {
                return Err(uqa_storage::mvcc::VersionError::WrongDatabase);
            }
            return Ok(registry);
        }
    }
    let registry = Arc::new(SnapshotRegistry::default());
    registries.insert(path, (identity, Arc::downgrade(&registry)));
    Ok(registry)
}
