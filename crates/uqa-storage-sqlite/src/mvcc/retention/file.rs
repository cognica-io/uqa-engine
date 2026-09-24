//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Snapshot sequences use the shared native lease transport with their original on-disk namespace.

use super::super::leases::{LeaseNamespace, NativeLeaseFile};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, Weak},
};
use uqa_storage::{
    mvcc::{
        CommitSequence, DatabaseId, SnapshotLeaseTransport, SnapshotRegistry, VersionError,
        VersionResult,
    },
    read_control::StorageReadControl,
};

type Registries = HashMap<PathBuf, (DatabaseId, Weak<SnapshotRegistry>)>;
static REGISTRIES: OnceLock<Mutex<Registries>> = OnceLock::new();

#[cfg(test)]
mod tests;

pub(in crate::mvcc) fn lease_file(
    path: &Path,
    identity: DatabaseId,
) -> VersionResult<NativeLeaseFile> {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".uqa-snapshots");
    NativeLeaseFile::open(
        Path::new(&sidecar),
        LeaseNamespace {
            magic: *b"UQASNP01",
            database: identity,
            incarnation: None,
        },
    )
}

pub(super) fn registry(path: &Path, identity: DatabaseId) -> VersionResult<Arc<SnapshotRegistry>> {
    let path = super::database_path(path)?;
    let mut registries = REGISTRIES.get_or_init(Mutex::default).lock();
    registries.retain(|_, (_, registry)| registry.strong_count() != 0);
    if let Some((previous, registry)) = registries.get(&path) {
        if let Some(registry) = registry.upgrade() {
            if *previous != identity {
                return Err(VersionError::WrongDatabase);
            }
            return Ok(registry);
        }
    }
    let registry = Arc::new(SnapshotRegistry::with_transport(Arc::new(Transport(
        lease_file(&path, identity)?,
    ))));
    registries.insert(path, (identity, Arc::downgrade(&registry)));
    Ok(registry)
}

struct Transport(NativeLeaseFile);

impl SnapshotLeaseTransport for Transport {
    fn acquire_admission(&self, control: &StorageReadControl) -> VersionResult<()> {
        self.0.acquire_admission(control)
    }
    fn release_admission(&self) {
        self.0.release_admission();
    }
    fn retain(
        &self,
        sequence: CommitSequence,
        control: &StorageReadControl,
    ) -> VersionResult<Box<dyn Send + Sync>> {
        self.0.retain(sequence.as_u64(), control)
    }
    fn oldest(&self, control: &StorageReadControl) -> VersionResult<Option<CommitSequence>> {
        let mut oldest = None;
        self.0.visit(control, &mut |tag| {
            let sequence = CommitSequence::from_u64(tag);
            oldest = Some(oldest.map_or(sequence, |old: CommitSequence| old.min(sequence)));
            Ok(())
        })?;
        Ok(oldest)
    }
}
