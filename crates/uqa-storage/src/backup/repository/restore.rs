//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable restore identities and exclusive directory publication. Importers own physical import, logical validation and the new transaction-history transition.

mod record;
mod target;

use std::fs::File;
use std::io;
use std::path::Path;

use cap_std::fs::Dir;

use crate::backup::{BackupError, BackupId, BackupResult, FrameKind, ImageDigest, RestoreId};
use crate::mvcc::{DatabaseId, DatabaseRestore};
use crate::native_file::directory as native;
use crate::read_control::{CancellationToken, StorageReadControl};

use super::{directory, BackupLease, LocalRepository};
use target::Destination;

const MARKER: &str = ".uqa-restore";

/// Provider access to a private, pinned directory. File names come from installed provider code, never from archive unit keys. All returned handles must close before an importer returns success.
pub struct RestoreDirectory<'a> {
    directory: &'a Dir,
}

impl RestoreDirectory<'_> {
    pub fn create_file(&self, name: &str) -> BackupResult<File> {
        Self::validate_name(name)?;
        directory::create_file(self.directory, name)
    }

    pub fn open_file(&self, name: &str, write: bool) -> BackupResult<File> {
        Self::validate_name(name)?;
        directory::open_file(self.directory, name, write)
    }

    fn validate_name(name: &str) -> BackupResult<()> {
        native::component(name.as_ref())?;
        if name == MARKER {
            return Err(BackupError::Invalid("reserved restore marker"));
        }
        Ok(())
    }
}

/// A consuming provider adapter. Success certifies complete archive and owner-format validation, durable imported data and a completed `DatabaseRestore` transition. Import and recovery must close every provider handle before returning; recovery resolves the exact persisted request and must never reimport over an existing image.
pub trait BackupRestoreImporter: Sized {
    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: &BackupLease<'_>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()>;

    fn recover(
        self,
        directory: &RestoreDirectory<'_>,
        backup: &BackupLease<'_>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<()>;
}

/// A completed publication receipt, not a certificate of the target's future contents. Resolving this operation does not undo later transactions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestoredDatabase {
    pub restore: RestoreId,
    pub backup: BackupId,
    pub database: DatabaseId,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Intent {
    id: RestoreId,
    backup: BackupId,
    root: ImageDigest,
    request: DatabaseRestore,
    parent: [u8; 24],
    name: [u8; 32],
}

impl Intent {
    fn encode(self) -> [u8; 160] {
        let mut bytes = [0; 160];
        bytes[..8].copy_from_slice(b"UQARES01");
        bytes[8..24].copy_from_slice(&self.id.as_bytes());
        bytes[24..40].copy_from_slice(&self.backup.as_bytes());
        bytes[40..72].copy_from_slice(&self.root.as_bytes());
        bytes[72..88].copy_from_slice(&self.request.source().as_bytes());
        bytes[88..104].copy_from_slice(&self.request.target().as_bytes());
        bytes[104..128].copy_from_slice(&self.parent);
        bytes[128..].copy_from_slice(&self.name);
        bytes
    }

    fn decode(bytes: &[u8]) -> BackupResult<Self> {
        if bytes.len() != 160 || &bytes[..8] != b"UQARES01" {
            return Err(BackupError::Invalid("restore intent"));
        }
        let source = DatabaseId::from_bytes(bytes[72..88].try_into().unwrap());
        let target = DatabaseId::from_bytes(bytes[88..104].try_into().unwrap());
        if source.as_bytes() == [0; 16] || target.as_bytes() == [0; 16] {
            return Err(BackupError::Invalid("zero restore incarnation"));
        }
        Ok(Self {
            id: RestoreId::from_bytes(bytes[8..24].try_into().unwrap())?,
            backup: BackupId::from_bytes(bytes[24..40].try_into().unwrap())?,
            root: ImageDigest::from_bytes(bytes[40..72].try_into().unwrap()),
            request: DatabaseRestore::from_identities(source, target)?,
            parent: bytes[104..128].try_into().unwrap(),
            name: bytes[128..].try_into().unwrap(),
        })
    }

    fn receipt(self) -> RestoredDatabase {
        RestoredDatabase {
            restore: self.id,
            backup: self.backup,
            database: self.request.target(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    intent: Intent,
    holder: [u8; 24],
    image: [u8; 24],
}

impl Target {
    fn encode(self) -> [u8; 208] {
        let mut bytes = [0; 208];
        bytes[..160].copy_from_slice(&self.intent.encode());
        bytes[160..184].copy_from_slice(&self.holder);
        bytes[184..].copy_from_slice(&self.image);
        bytes
    }

    fn decode(bytes: &[u8]) -> BackupResult<Self> {
        if bytes.len() != 208 {
            return Err(BackupError::Invalid("restore directory identity"));
        }
        Ok(Self {
            intent: Intent::decode(&bytes[..160])?,
            holder: bytes[160..184].try_into().unwrap(),
            image: bytes[184..].try_into().unwrap(),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    New,
    Started,
    Prepared,
    Cancelling,
    Cancelled,
    Complete,
}

/// Repository-owned restore admission and recovery. The destination is absent until a completely prepared private directory is atomically installed without replacement. Dropping this owner releases admission, while its durable intent remains available for recovery.
pub struct BackupRestoreOperation<'a> {
    repository: &'a LocalRepository,
    directory: Dir,
    target: Target,
    destination: Option<Destination>,
    lease: Option<directory::Lease>,
    backup: Option<BackupLease<'a>>,
    state: State,
    control: StorageReadControl,
    completion: StorageReadControl,
}

impl LocalRepository {
    fn restore_directory(&self, create: bool, control: &StorageReadControl) -> BackupResult<Dir> {
        let _admission = self.admission(control)?;
        match directory::child(&self.root, "restores".as_ref()) {
            Err(BackupError::Io(error)) if create && error.kind() == io::ErrorKind::NotFound => {
                directory::create_directory(&self.root, "restores")?;
                directory::synchronize(&self.root)?;
                directory::child(&self.root, "restores".as_ref())
            }
            result => result,
        }
    }

    /// Persist a fresh history request outside the destination before creating any provider files. The selected parent is pinned; the destination must not exist, including as an empty directory or symlink.
    pub fn begin_restore(
        &self,
        backup: BackupId,
        path: impl AsRef<Path>,
        control: &StorageReadControl,
    ) -> BackupResult<BackupRestoreOperation<'_>> {
        control.cancellation().check()?;
        let backup = self.open_backup(backup, control)?;
        let destination = Destination::open(path.as_ref(), control)?;
        destination.require_absent()?;
        directory::synchronize(&destination.parent)?;
        let root = backup.root();
        let intent = Intent {
            id: RestoreId::new()?,
            backup: root.backup(),
            root: ImageDigest::of_bytes(&root.encode()),
            request: DatabaseRestore::new(root.cut().source)?,
            parent: native::identity(&destination.parent)?,
            name: destination.name_digest(),
        };
        let restores = self.restore_directory(true, control)?;
        let name = directory::name(intent.id.as_bytes());
        directory::create_directory(&restores, directory::text(&name))?;
        let operation = directory::child(&restores, directory::text(&name).as_ref())?;
        let lease = directory::lock(directory::create_file(&operation, "lease")?, false, control)?;
        lease.sync_all()?;
        record::install(
            self,
            &operation,
            intent.id,
            "intent",
            FrameKind::RestoreIntent,
            &intent.encode(),
            control,
        )?;
        directory::synchronize(&restores)?;
        let holder_name = target::holder_name(intent.id);
        directory::create_directory(
            &destination.parent,
            std::str::from_utf8(&holder_name).unwrap(),
        )?;
        let holder = target::holder(&destination, intent.id)?;
        directory::create_directory(&holder, "image")?;
        let image = directory::child(&holder, "image".as_ref())?;
        let target = Target {
            intent,
            holder: native::identity(&holder)?,
            image: native::identity(&image)?,
        };
        directory::synchronize(&image)?;
        directory::synchronize(&holder)?;
        directory::synchronize(&destination.parent)?;
        record::install(
            self,
            &operation,
            intent.id,
            "initialized",
            FrameKind::RestoreInitialized,
            &target.encode(),
            control,
        )?;
        Ok(BackupRestoreOperation {
            repository: self,
            directory: operation,
            target,
            destination: Some(destination),
            lease: Some(lease),
            backup: Some(backup),
            state: State::New,
            control: control.clone(),
            completion: StorageReadControl::new(control.memory(), &CancellationToken::new()),
        })
    }

    /// Resolve one persisted request. A prepared or published image does not need its source archive. An interrupted importer can only enter its recovery method, never fresh import over existing files.
    pub fn resume_restore(
        &self,
        id: RestoreId,
        path: impl AsRef<Path>,
        control: &StorageReadControl,
    ) -> BackupResult<BackupRestoreOperation<'_>> {
        let completion = StorageReadControl::new(control.memory(), &CancellationToken::new());
        let restores = self.restore_directory(false, &completion)?;
        let name = directory::name(id.as_bytes());
        let operation = directory::child(&restores, directory::text(&name).as_ref())?;
        let lease = directory::lock(
            directory::open_file(&operation, "lease", true)?,
            false,
            &completion,
        )?;
        let bytes = record::read(
            self,
            &operation,
            id,
            "intent",
            FrameKind::RestoreIntent,
            &completion,
        )?
        .ok_or(BackupError::Invalid("missing restore intent"))?;
        let intent = Intent::decode(&bytes)?;
        drop(bytes);
        if intent.id != id {
            return Err(BackupError::Integrity);
        }
        let destination = Destination::open(path.as_ref(), &completion)?;
        if intent.parent != native::identity(&destination.parent)?
            || intent.name != destination.name_digest()
        {
            return Err(BackupError::Invalid("restore destination identity"));
        }
        let bytes = record::read(
            self,
            &operation,
            id,
            "initialized",
            FrameKind::RestoreInitialized,
            &completion,
        )?
        .ok_or(BackupError::Invalid("uninitialized restore directory"))?;
        let target = Target::decode(&bytes)?;
        drop(bytes);
        if target.intent != intent {
            return Err(BackupError::Integrity);
        }
        let mut restore = BackupRestoreOperation {
            repository: self,
            directory: operation,
            target,
            destination: Some(destination),
            lease: Some(lease),
            backup: None,
            state: State::New,
            control: control.clone(),
            completion,
        };
        restore.resume()?;
        Ok(restore)
    }
}

impl BackupRestoreOperation<'_> {
    pub const fn id(&self) -> RestoreId {
        self.target.intent.id
    }
    pub const fn request(&self) -> DatabaseRestore {
        self.target.intent.request
    }

    pub fn completed(&self) -> Option<RestoredDatabase> {
        (self.state == State::Complete).then(|| self.target.intent.receipt())
    }

    fn has_record(&self, name: &str, kind: FrameKind) -> BackupResult<bool> {
        record::matches(
            self.repository,
            &self.directory,
            self.id(),
            name,
            kind,
            &self.target.encode(),
            &self.completion,
        )
    }

    fn install_record(
        &self,
        name: &str,
        kind: FrameKind,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        record::install(
            self.repository,
            &self.directory,
            self.id(),
            name,
            kind,
            &self.target.encode(),
            control,
        )
    }

    fn stage(&self) -> BackupResult<(Dir, Dir)> {
        let destination = self.destination.as_ref().ok_or(BackupError::Finished)?;
        let holder = target::holder(destination, self.id())?;
        if native::identity(&holder)? != self.target.holder {
            return Err(BackupError::Integrity);
        }
        let image = directory::child(&holder, "image".as_ref())?;
        if native::identity(&image)? != self.target.image {
            return Err(BackupError::Integrity);
        }
        Ok((holder, image))
    }

    fn prepared_marker(&self, image: &Dir) -> BackupResult<bool> {
        record::matches(
            self.repository,
            image,
            self.id(),
            MARKER,
            FrameKind::RestorePrepared,
            &self.target.encode(),
            &self.completion,
        )
    }

    /// Recognize our exact physical directory and certificate. Foreign destinations are never adopted, even if their contents happen to equal this backup.
    fn published(&self) -> BackupResult<bool> {
        let destination = self.destination.as_ref().ok_or(BackupError::Finished)?;
        match destination.parent.symlink_metadata(&destination.name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
            Ok(metadata) if !metadata.is_dir() => return Err(target::occupied()),
            Ok(_) => {}
        }
        let image = directory::child(&destination.parent, &destination.name)?;
        if native::identity(&image)? != self.target.image {
            return Err(target::occupied());
        }
        if !self.prepared_marker(&image)? {
            return Err(BackupError::Integrity);
        }
        directory::synchronize(&image)?;
        Ok(true)
    }

    fn resume(&mut self) -> BackupResult<()> {
        let complete = self.has_record("complete", FrameKind::RestoreComplete)?;
        if self.has_record("cancelled", FrameKind::RestoreCancelled)? {
            if complete {
                return Err(BackupError::Integrity);
            }
            self.install_record("cancelled", FrameKind::RestoreCancelled, &self.completion)?;
            self.retire(State::Cancelled);
            return Ok(());
        }
        match self.published() {
            Ok(true) => return self.finish(),
            Err(BackupError::Io(error))
                if error.kind() == io::ErrorKind::AlreadyExists && !complete => {}
            result => {
                result?;
            }
        }
        if complete {
            return Err(BackupError::Invalid("published restore target is missing"));
        }
        let (_, image) = self.stage()?;
        if self.prepared_marker(&image)? {
            directory::synchronize(&image)?;
            self.install_record("prepared", FrameKind::RestorePrepared, &self.completion)?;
            self.state = State::Prepared;
            return Ok(());
        }
        if self.has_record("prepared", FrameKind::RestorePrepared)? {
            return Err(BackupError::Integrity);
        }
        self.control.cancellation().check()?;
        let lease = self
            .repository
            .open_backup(self.target.intent.backup, &self.control)?;
        if ImageDigest::of_bytes(&lease.root().encode()) != self.target.intent.root {
            return Err(BackupError::Integrity);
        }
        self.backup = Some(lease);
        self.state = if self.has_record("started", FrameKind::RestoreStarted)? {
            State::Started
        } else {
            State::New
        };
        Ok(())
    }

    /// Complete import under the original allowance. The provider supplies its existing restore state machine; an uncertain call is routed to recovery by the durable started record.
    pub fn prepare(&mut self, importer: impl BackupRestoreImporter) -> BackupResult<()> {
        if matches!(self.state, State::Prepared | State::Complete) {
            return Ok(());
        }
        if matches!(self.state, State::Cancelling | State::Cancelled) {
            return Err(BackupError::Finished);
        }
        self.control.cancellation().check()?;
        let (_, image) = self.stage()?;
        if self.prepared_marker(&image)? {
            directory::synchronize(&image)?;
            self.install_record("prepared", FrameKind::RestorePrepared, &self.completion)?;
            self.state = State::Prepared;
            self.backup.take();
            return Ok(());
        }
        let fresh = self.state == State::New;
        self.install_record("started", FrameKind::RestoreStarted, &self.control)?;
        self.state = State::Started;
        #[cfg(test)]
        self.repository.before(super::Fault::RestoreStarted)?;
        let directory = RestoreDirectory { directory: &image };
        let backup = self.backup.as_ref().ok_or(BackupError::Finished)?;
        if fresh {
            importer.import(&directory, backup, self.request(), &self.control)?;
        } else {
            importer.recover(&directory, backup, self.request(), &self.control)?;
        }
        directory::synchronize(&image)?;
        record::install(
            self.repository,
            &image,
            self.id(),
            MARKER,
            FrameKind::RestorePrepared,
            &self.target.encode(),
            &self.control,
        )?;
        #[cfg(test)]
        self.repository.before(super::Fault::RestorePrepared)?;
        self.install_record("prepared", FrameKind::RestorePrepared, &self.control)?;
        self.state = State::Prepared;
        self.backup.take();
        Ok(())
    }

    /// Publish once, resolving a previous rename before checking cancellation. Existing targets are never replaced; after a lost reply this method preserves all subsequent writes.
    pub fn publish(&mut self) -> BackupResult<RestoredDatabase> {
        if self.state == State::Complete {
            return Ok(self.target.intent.receipt());
        }
        if self.state != State::Prepared {
            return Err(BackupError::Finished);
        }
        if self.published()? {
            self.finish()?;
            return Ok(self.target.intent.receipt());
        }
        self.control.cancellation().check()?;
        let (holder, image) = self.stage()?;
        if !self.prepared_marker(&image)? {
            return Err(BackupError::Integrity);
        }
        directory::synchronize(&image)?;
        drop(image);
        let destination = self.destination.as_ref().ok_or(BackupError::Finished)?;
        #[cfg(test)]
        self.repository.before(super::Fault::RestoreInstall)?;
        native::rename_new(
            &holder,
            "image".as_ref(),
            &destination.parent,
            &destination.name,
        )?;
        #[cfg(test)]
        self.repository.before(super::Fault::RestoreSync)?;
        directory::synchronize(&holder)?;
        self.finish()?;
        Ok(self.target.intent.receipt())
    }

    fn finish(&mut self) -> BackupResult<()> {
        let destination = self.destination.as_ref().ok_or(BackupError::Finished)?;
        match target::holder(destination, self.id()) {
            Ok(holder) => {
                if native::identity(&holder)? != self.target.holder {
                    return Err(BackupError::Integrity);
                }
                directory::synchronize(&holder)?;
            }
            Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        directory::synchronize(&destination.parent)?;
        #[cfg(test)]
        self.repository.before(super::Fault::RestoreComplete)?;
        self.install_record("complete", FrameKind::RestoreComplete, &self.completion)?;
        self.retire(State::Complete);
        Ok(())
    }

    /// Persist cancellation before acknowledging it. A directory already published by this request remains completed even if its publication reply was lost.
    pub fn cancel(&mut self) -> BackupResult<Option<RestoredDatabase>> {
        if self.state == State::Complete {
            return Ok(self.completed());
        }
        if self.state == State::Cancelled {
            return Ok(None);
        }
        match self.published() {
            Ok(true) => {
                self.finish()?;
                return Ok(self.completed());
            }
            Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {}
            result => {
                result?;
            }
        }
        if self.has_record("complete", FrameKind::RestoreComplete)? {
            return Err(BackupError::Invalid("published restore target is missing"));
        }
        self.state = State::Cancelling;
        #[cfg(test)]
        self.repository.before(super::Fault::RestoreCancel)?;
        self.install_record("cancelled", FrameKind::RestoreCancelled, &self.completion)?;
        self.retire(State::Cancelled);
        Ok(None)
    }

    fn retire(&mut self, state: State) {
        self.state = state;
        self.backup.take();
        self.destination.take();
        self.lease.take();
    }
}

#[cfg(test)]
mod tests;
