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
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, Weak},
};
use uqa_storage::{
    mvcc::{
        CommitSequence, DatabaseId, MetadataEpoch, SnapshotBoundary, SnapshotLeaseTransport,
        SnapshotRegistry, VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};

type Registries<B = CommitSequence> = HashMap<PathBuf, (DatabaseId, Weak<SnapshotRegistry<B>>)>;
static REGISTRIES: OnceLock<Mutex<Registries>> = OnceLock::new();
static METADATA_REGISTRIES: OnceLock<Mutex<Registries<MetadataEpoch>>> = OnceLock::new();

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
    registry_for(path, identity, &REGISTRIES, lease_file)
}

pub(super) fn metadata_registry(
    path: &Path,
    identity: DatabaseId,
) -> VersionResult<Arc<SnapshotRegistry<MetadataEpoch>>> {
    registry_for(path, identity, &METADATA_REGISTRIES, |path, identity| {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(".uqa-backup-snapshots");
        NativeLeaseFile::open(
            Path::new(&sidecar),
            LeaseNamespace {
                magic: *b"UQAMET01",
                database: identity,
                incarnation: None,
            },
        )
    })
}

fn registry_for<B: NativeBoundary>(
    path: &Path,
    identity: DatabaseId,
    registries: &OnceLock<Mutex<Registries<B>>>,
    open: impl FnOnce(&Path, DatabaseId) -> VersionResult<NativeLeaseFile>,
) -> VersionResult<Arc<SnapshotRegistry<B>>> {
    let path = super::database_path(path)?;
    let mut registries = registries.get_or_init(Mutex::default).lock();
    registries.retain(|_, (_, registry)| registry.strong_count() != 0);
    if let Some((previous, registry)) = registries.get(&path) {
        if let Some(registry) = registry.upgrade() {
            if *previous != identity {
                return Err(VersionError::WrongDatabase);
            }
            return Ok(registry);
        }
    }
    let registry = Arc::new(SnapshotRegistry::with_transport(Arc::new(Transport {
        file: open(&path, identity)?,
        boundary: PhantomData,
    })));
    registries.insert(path, (identity, Arc::downgrade(&registry)));
    Ok(registry)
}

trait NativeBoundary: SnapshotBoundary {
    fn encode(self) -> u64;
    fn decode(value: u64) -> Self;
}

impl NativeBoundary for CommitSequence {
    fn encode(self) -> u64 {
        self.as_u64()
    }
    fn decode(value: u64) -> Self {
        Self::from_u64(value)
    }
}

impl NativeBoundary for MetadataEpoch {
    fn encode(self) -> u64 {
        self.as_u64()
    }
    fn decode(value: u64) -> Self {
        Self::from_u64(value)
    }
}

struct Transport<B> {
    file: NativeLeaseFile,
    boundary: PhantomData<B>,
}

impl<B: NativeBoundary> SnapshotLeaseTransport<B> for Transport<B> {
    fn acquire_admission(&self, control: &StorageReadControl) -> VersionResult<()> {
        self.file.acquire_admission(control)
    }
    fn release_admission(&self) {
        self.file.release_admission();
    }
    fn retain(
        &self,
        sequence: B,
        control: &StorageReadControl,
    ) -> VersionResult<Box<dyn Send + Sync>> {
        self.file.retain(sequence.encode(), control)
    }
    fn oldest(&self, control: &StorageReadControl) -> VersionResult<Option<B>> {
        let mut oldest = None;
        self.file.visit(control, &mut |tag| {
            let sequence = B::decode(tag);
            oldest = Some(oldest.map_or(sequence, |old: B| old.min(sequence)));
            Ok(())
        })?;
        Ok(oldest)
    }
}
