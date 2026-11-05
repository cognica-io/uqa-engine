//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pinned local repositories, immutable objects and publication ownership.

mod directory;
mod generation;
mod objects;
mod operation;
mod restore;
mod retention;
mod verify;

pub use operation::{
    BackupCaptureReport, BackupCopyReport, BackupImageStatistics, BackupOperation, BackupPack,
    PackObject, PackRange,
};
pub use restore::{
    BackupRestoreImporter, BackupRestoreOperation, RestoreDirectory, RestoredDatabase,
};
pub use retention::{BackupCollectionReport, BackupListing};
pub use verify::{BackupImageReader, BackupReadStep, VerifiedBackupImage};

use std::io::Read;
use std::path::Path;

use cap_std::fs::Dir;

use crate::read_control::StorageReadControl;

use super::frame::{read_exact, require_end, write_all};
use super::{
    read_frame, write_frame, BackupError, BackupId, BackupLimits, BackupResult, BackupRoot,
    FrameContext, FrameKind, ObjectId, Protection, RepositoryId,
};

const HEADER_BYTES: usize = 41;

/// A local repository whose directory identities remain pinned for its lifetime. Completed roots are immutable; caller-selected protection is independent of source database encryption.
pub struct LocalRepository {
    root: Dir,
    objects: Dir,
    roots: Dir,
    operations: Dir,
    id: RepositoryId,
    protection: Protection,
    limits: BackupLimits,
    #[cfg(test)]
    fault: parking_lot::Mutex<Option<Fault>>,
}

/// A completed root and its live OS lease. Removing a root or reclaiming its operation requires exclusive admission after all readers release this lease.
pub struct BackupLease<'a> {
    repository: &'a LocalRepository,
    root: BackupRoot,
    _lease: directory::Lease,
}

impl BackupLease<'_> {
    pub const fn root(&self) -> BackupRoot {
        self.root
    }

    /// Stream units and complete global verification under this retained root lease. Importers keep their targets private until the returned reader and provider validation complete.
    pub fn read_image(
        &self,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupImageReader<'_>> {
        verify::reader(self.repository, self.root, maximum_workspace_bytes, control)
    }

    /// Authenticate every referenced frame and unit and compare the complete inventory. Provider-specific image validation remains the importing owner's responsibility.
    pub fn verify(
        &self,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.repository
            .verify_root(self.root, maximum_workspace_bytes, control)
    }
}

impl LocalRepository {
    /// Create a new directory; an existing destination is never adopted or overwritten. The repository header is installed only after its owned directories and admission file are synchronized.
    pub fn create(
        path: impl AsRef<Path>,
        protection: Protection,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        validate_limits(limits)?;
        let (parent, name) = directory::parent(path.as_ref())?;
        directory::synchronize(&parent)?;
        directory::create_directory(&parent, name)?;
        let root = directory::child(&parent, name)?;
        for name in ["objects", "roots", "operations"] {
            directory::create_directory(&root, name)?;
        }
        let gate = directory::create_file(&root, "admission")?;
        gate.sync_all()?;
        let repository = Self::from_directory(root, RepositoryId::new()?, protection, limits)?;
        directory::synchronize(&repository.objects)?;
        directory::synchronize(&repository.roots)?;
        directory::synchronize(&repository.operations)?;
        directory::synchronize(&repository.root)?;
        repository.advance_removal_generation(control)?;
        let mut header = directory::create_file(&repository.root, "repository")?;
        let bytes = repository.header();
        write_all(&mut header, &bytes, control)?;
        write_frame(
            &mut header,
            repository.header_context(),
            &repository.protection,
            &bytes,
            limits,
            control,
        )?;
        header.sync_all()?;
        directory::synchronize(&repository.root)?;
        directory::synchronize(&parent)?;
        Ok(repository)
    }

    pub fn open(
        path: impl AsRef<Path>,
        protection: Protection,
        limits: BackupLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        control.cancellation().check()?;
        validate_limits(limits)?;
        let (parent, name) = directory::parent(path.as_ref())?;
        let root = directory::child(&parent, name)?;
        let mut file = directory::open_file(&root, "repository", false)?;
        let mut header = [0; HEADER_BYTES];
        read_exact(&mut file, &mut header, control)?;
        if &header[..8] != b"UQAREP01" {
            return Err(BackupError::Invalid("repository header"));
        }
        let repository = Self::from_directory(
            root,
            RepositoryId::from_bytes(header[8..24].try_into().unwrap())?,
            protection,
            limits,
        )?;
        if repository.header() != header {
            return Err(BackupError::Invalid("repository protection domain"));
        }
        let decoded = read_frame(
            &mut file,
            repository.header_context(),
            &repository.protection,
            limits,
            control,
        )?;
        if *decoded != header {
            return Err(BackupError::Integrity);
        }
        require_end(&mut file, control)?;
        repository.generation(control)?;
        directory::synchronize(&repository.root)?;
        Ok(repository)
    }

    fn from_directory(
        root: Dir,
        id: RepositoryId,
        protection: Protection,
        limits: BackupLimits,
    ) -> BackupResult<Self> {
        let objects = directory::child(&root, "objects".as_ref())?;
        let roots = directory::child(&root, "roots".as_ref())?;
        let operations = directory::child(&root, "operations".as_ref())?;
        Ok(Self {
            root,
            objects,
            roots,
            operations,
            id,
            protection,
            limits,
            #[cfg(test)]
            fault: parking_lot::Mutex::new(None),
        })
    }

    pub const fn id(&self) -> RepositoryId {
        self.id
    }

    fn header(&self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0; HEADER_BYTES];
        bytes[..8].copy_from_slice(b"UQAREP01");
        bytes[8..24].copy_from_slice(&self.id.as_bytes());
        if let Protection::Encrypted { domain, .. } = &self.protection {
            bytes[24] = 1;
            bytes[25..].copy_from_slice(&domain.as_bytes());
        }
        bytes
    }

    fn header_context(&self) -> FrameContext {
        FrameContext {
            repository: self.id,
            object: ObjectId::from_bytes(self.id.as_bytes()).expect("nonzero repository identity"),
            offset: HEADER_BYTES as u64,
            kind: FrameKind::Repository,
        }
    }

    fn root_context(&self, id: BackupId) -> FrameContext {
        FrameContext {
            repository: self.id,
            object: ObjectId::from_bytes(id.as_bytes()).expect("nonzero backup identity"),
            offset: 0,
            kind: FrameKind::Root,
        }
    }

    fn admission(&self, control: &StorageReadControl) -> BackupResult<directory::Lease> {
        directory::admission(
            directory::open_file(&self.root, "admission", true)?,
            control,
        )
    }

    fn read_root(&self, id: BackupId, control: &StorageReadControl) -> BackupResult<BackupRoot> {
        let name = directory::name(id.as_bytes());
        let mut input = directory::open_file(&self.roots, directory::text(&name), false)?;
        self.decode_root(&mut input, id, control)
    }

    fn decode_root(
        &self,
        input: &mut impl Read,
        id: BackupId,
        control: &StorageReadControl,
    ) -> BackupResult<BackupRoot> {
        let bytes = read_frame(
            input,
            self.root_context(id),
            &self.protection,
            self.limits,
            control,
        )?;
        let root = BackupRoot::decode(&bytes)?;
        if root.backup() != id || root.repository() != self.id {
            return Err(BackupError::Invalid("foreign repository root"));
        }
        require_end(input, control)?;
        Ok(root)
    }

    /// Pin a discoverable completed root before releasing repository admission. Incomplete operation directories are not backups.
    pub fn open_backup(
        &self,
        id: BackupId,
        control: &StorageReadControl,
    ) -> BackupResult<BackupLease<'_>> {
        let _admission = self.admission(control)?;
        let root = self.read_root(id, control)?;
        let name = directory::name(id.as_bytes());
        let operation = directory::child(&self.operations, directory::text(&name).as_ref())?;
        let lease = directory::lock(
            directory::open_file(&operation, "lease", true)?,
            true,
            control,
        )?;
        directory::synchronize(&self.roots)?;
        Ok(BackupLease {
            repository: self,
            root,
            _lease: lease,
        })
    }
}

fn validate_limits(limits: BackupLimits) -> BackupResult<()> {
    if (limits.max_frame_bytes() as usize) < BackupRoot::ENCODED_BYTES {
        return Err(BackupError::Invalid(
            "repository frames cannot hold a root descriptor",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    ObjectSync,
    ObjectInstall,
    PreparedSync,
    RootInstall,
    RootSync,
    CancelSync,
    RestoreStarted,
    RestorePrepared,
    RestoreInstall,
    RestoreSync,
    RestoreComplete,
    RestoreCancel,
    RestoreRecordSync,
    RemovalMarker,
    RemovalMarkerSync,
    RemovalRoot,
    RemovalSync,
    CollectionUnlink,
    CollectionSync,
}

#[cfg(test)]
impl LocalRepository {
    fn before(&self, point: Fault) -> BackupResult<()> {
        let mut fault = self.fault.lock();
        if *fault == Some(point) {
            *fault = None;
            return Err(std::io::Error::other("injected repository I/O failure").into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
