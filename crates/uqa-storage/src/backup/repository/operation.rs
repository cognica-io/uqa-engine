//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{self, Read};

mod capture;
mod copy;
mod pack;
pub use capture::{BackupCaptureReport, BackupImageStatistics};
pub use copy::BackupCopyReport;
pub use pack::{BackupPack, PackObject, PackRange};

use cap_std::fs::Dir;

use crate::mvcc::{CommitSequence, DatabaseId};
use crate::read_control::{CancellationToken, StorageReadControl};
use crate::temporary_file::FileBudget;

use super::super::frame::{encoded_bytes, require_end};
use super::super::{
    read_frame, write_frame, BackupCut, BackupError, BackupId, BackupResult, BackupRoot,
    FrameContext, FrameKind, ImageFormat, ImageProvider, InventorySummary, ObjectId, ObjectSpan,
    SnapshotId,
};
use super::directory;
use super::{BackupLease, LocalRepository};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Intent {
    backup: BackupId,
    cut: BackupCut,
    parent: Option<BackupId>,
}

impl Intent {
    fn encode(self) -> [u8; 86] {
        let mut bytes = [0; 86];
        bytes[..8].copy_from_slice(b"UQACAP01");
        bytes[8..24].copy_from_slice(&self.backup.as_bytes());
        bytes[24..40].copy_from_slice(&self.cut.source.as_bytes());
        bytes[40..56].copy_from_slice(&self.cut.snapshot.as_bytes());
        bytes[56..61].copy_from_slice(&self.cut.format.bytes());
        if let Some(sequence) = self.cut.sequence {
            bytes[61] = 1;
            bytes[62..70].copy_from_slice(&sequence.as_u64().to_le_bytes());
        }
        if let Some(parent) = self.parent {
            bytes[70..].copy_from_slice(&parent.as_bytes());
        }
        bytes
    }

    fn decode(bytes: &[u8]) -> BackupResult<Self> {
        if bytes.len() != 86 || &bytes[..8] != b"UQACAP01" {
            return Err(BackupError::Invalid("capture intent"));
        }
        let sequence = u64::from_le_bytes(bytes[62..70].try_into().unwrap());
        let sequence = match (bytes[61], sequence) {
            (0, 0) => None,
            (1, sequence) => Some(CommitSequence::from_u64(sequence)),
            _ => return Err(BackupError::Invalid("capture sequence tag")),
        };
        let parent = bytes[70..].try_into().unwrap();
        let intent = Self {
            backup: BackupId::from_bytes(bytes[8..24].try_into().unwrap())?,
            parent: if parent == [0; 16] {
                None
            } else {
                Some(BackupId::from_bytes(parent)?)
            },
            cut: BackupCut {
                source: DatabaseId::from_bytes(bytes[24..40].try_into().unwrap()),
                snapshot: SnapshotId::from_bytes(bytes[40..56].try_into().unwrap())?,
                format: ImageFormat::new(
                    ImageProvider::decode(bytes[56])?,
                    u32::from_le_bytes(bytes[57..61].try_into().unwrap()),
                )?,
                sequence,
            },
        };
        if intent.cut.source.as_bytes() == [0; 16] || intent.parent == Some(intent.backup) {
            return Err(BackupError::Invalid("capture identity"));
        }
        Ok(intent)
    }
}

#[derive(Clone, Copy)]
enum State {
    Capturing,
    Prepared(BackupRoot),
    Complete(BackupRoot),
    Cancelling,
    Cancelled,
    Failed,
}

/// Repository capture ownership. A failed capture cannot append or publish. A fully prepared image can finish publication after source loss; uncertain publication keeps its exact root for idempotent resolution.
pub struct BackupOperation<'a> {
    repository: &'a LocalRepository,
    directory: Dir,
    intent: Intent,
    state: State,
    used: u64,
    maximum: u64,
    completion: StorageReadControl,
    lease: Option<directory::Lease>,
    parent_lease: Option<BackupLease<'a>>,
}

impl LocalRepository {
    pub fn begin_backup(
        &self,
        cut: BackupCut,
        parent: Option<BackupId>,
        maximum_encoded_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupOperation<'_>> {
        control.cancellation().check()?;
        if cut.source.as_bytes() == [0; 16] {
            return Err(BackupError::Invalid("zero source incarnation"));
        }
        let retained_parent = parent.map(|id| self.open_backup(id, control)).transpose()?;
        if retained_parent.as_ref().is_some_and(|parent| {
            parent.root.cut().source != cut.source || parent.root.cut().format != cut.format
        }) {
            return Err(BackupError::Invalid("incompatible incremental parent"));
        }
        let intent = Intent {
            backup: BackupId::new()?,
            cut,
            parent,
        };
        let context = FrameContext {
            kind: FrameKind::Intent,
            ..self.root_context(intent.backup)
        };
        let required = encoded_bytes(context, &self.protection, intent.encode().len())?;
        if required > maximum_encoded_bytes {
            return Err(BackupError::Limit {
                resource: "encoded staging",
                required,
                limit: maximum_encoded_bytes,
            });
        }
        let _admission = self.admission(control)?;
        let name = directory::name(intent.backup.as_bytes());
        directory::create_directory(&self.operations, directory::text(&name))?;
        let directory = directory::child(&self.operations, directory::text(&name).as_ref())?;
        let lease = directory::lock(directory::create_file(&directory, "lease")?, false, control)?;
        lease.sync_all()?;
        let mut file = directory::create_file(&directory, "intent")?;
        write_frame(
            &mut file,
            context,
            &self.protection,
            &intent.encode(),
            self.limits,
            control,
        )?;
        file.sync_all()?;
        directory::synchronize(&directory)?;
        directory::synchronize(&self.operations)?;
        Ok(BackupOperation {
            repository: self,
            directory,
            intent,
            state: State::Capturing,
            used: required,
            maximum: maximum_encoded_bytes,
            completion: StorageReadControl::new(control.memory(), &CancellationToken::new()),
            lease: Some(lease),
            parent_lease: retained_parent,
        })
    }

    /// Resume only a durable prepared image. An interrupted capture without a prepared descriptor cannot attach a new source snapshot to its old operation ID.
    pub fn resume_publication(
        &self,
        id: BackupId,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<BackupOperation<'_>> {
        let admission = self.admission(control)?;
        let name = directory::name(id.as_bytes());
        let directory = directory::child(&self.operations, directory::text(&name).as_ref())?;
        let lease = directory::lock(
            directory::open_file(&directory, "lease", true)?,
            false,
            control,
        )?;
        drop(admission);
        let intent = self.read_intent(&directory, "intent", id, control)?;
        match self.read_intent(&directory, "cancelled", id, control) {
            Ok(cancelled) if cancelled == intent => return Err(BackupError::Finished),
            Ok(_) => return Err(BackupError::Integrity),
            Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut prepared = directory::open_file(&directory, "prepared", true)?;
        let root = self.decode_root(&mut prepared, id, control)?;
        if root.cut() != intent.cut || root.parent() != intent.parent {
            return Err(BackupError::Invalid("prepared capture differs from intent"));
        }
        self.verify_root(root, maximum_workspace_bytes, control)?;
        prepared.sync_all()?;
        directory::synchronize(&directory)?;
        Ok(BackupOperation {
            repository: self,
            directory,
            intent,
            state: State::Prepared(root),
            used: 0,
            maximum: 0,
            completion: StorageReadControl::new(control.memory(), &CancellationToken::new()),
            lease: Some(lease),
            parent_lease: None,
        })
    }

    fn read_intent(
        &self,
        directory: &Dir,
        name: &str,
        id: BackupId,
        control: &StorageReadControl,
    ) -> BackupResult<Intent> {
        let mut file = directory::open_file(directory, name, false)?;
        let bytes = read_frame(
            &mut file,
            FrameContext {
                kind: FrameKind::Intent,
                ..self.root_context(id)
            },
            &self.protection,
            self.limits,
            control,
        )?;
        let intent = Intent::decode(&bytes)?;
        require_end(&mut file, control)?;
        if intent.backup != id {
            return Err(BackupError::Invalid("capture operation identity"));
        }
        Ok(intent)
    }
}

impl BackupOperation<'_> {
    #[cfg(test)]
    pub(super) fn duplicate_lease_for_test(&self) -> std::fs::File {
        self.lease.as_ref().unwrap().duplicate()
    }

    pub const fn id(&self) -> BackupId {
        self.intent.backup
    }

    pub const fn cut(&self) -> BackupCut {
        self.intent.cut
    }

    /// Stream an object containing one unit or inventory, with bounded frames.
    pub fn write_object(
        &mut self,
        kind: FrameKind,
        input: &mut impl Read,
        control: &StorageReadControl,
    ) -> BackupResult<ObjectSpan> {
        let mut pack = self.begin_pack(control)?;
        let range = pack.append(kind, input, control)?;
        pack.finish(control)?.span(range)
    }

    fn reserve_staging(&mut self, bytes: u64) -> BackupResult<()> {
        let required = self
            .used
            .checked_add(bytes)
            .ok_or(BackupError::Invalid("encoded staging overflow"))?;
        if required > self.maximum {
            return Err(BackupError::Limit {
                resource: "encoded staging",
                required,
                limit: self.maximum,
            });
        }
        self.used = required;
        Ok(())
    }

    /// Finish and verify capture before creating a durable descriptor that can be published without the source. The descriptor is installed once and is never rewritten with another cut.
    pub fn prepare(
        &mut self,
        inventory: ObjectSpan,
        summary: InventorySummary,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        self.prepare_with_files(inventory, summary, None, control)
    }

    fn prepare_with_files(
        &mut self,
        inventory: ObjectSpan,
        summary: InventorySummary,
        files: Option<&FileBudget>,
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        if !matches!(self.state, State::Capturing) {
            return Err(BackupError::Finished);
        }
        let result = self.prepare_inner(inventory, summary, files, control);
        match result {
            Ok(root) => {
                self.state = State::Prepared(root);
                Ok(())
            }
            Err(error) => {
                self.state = State::Failed;
                Err(error)
            }
        }
    }

    fn prepare_inner(
        &mut self,
        inventory: ObjectSpan,
        summary: InventorySummary,
        files: Option<&FileBudget>,
        control: &StorageReadControl,
    ) -> BackupResult<BackupRoot> {
        let root = BackupRoot::new(
            self.repository.id,
            self.intent.backup,
            self.intent.parent,
            self.intent.cut,
            inventory,
            summary,
        )?;
        let context = self.repository.root_context(self.intent.backup);
        self.reserve_staging(encoded_bytes(
            context,
            &self.repository.protection,
            BackupRoot::ENCODED_BYTES,
        )?)?;
        let remaining = FileBudget::new(self.maximum - self.used);
        self.repository
            .verify_root_with_files(root, files.unwrap_or(&remaining), control)?;
        let mut prepared = directory::create_file(&self.directory, "prepared")?;
        write_frame(
            &mut prepared,
            context,
            &self.repository.protection,
            &root.encode(),
            self.repository.limits,
            control,
        )?;
        #[cfg(test)]
        self.repository.before(super::Fault::PreparedSync)?;
        prepared.sync_all()?;
        directory::synchronize(&self.directory)?;
        Ok(root)
    }

    /// Resolve an existing matching root before consulting cancellation. A lost completion reply cannot turn a durable backup into a cancelled operation.
    pub fn publish(&mut self, control: &StorageReadControl) -> BackupResult<BackupRoot> {
        let root = match self.state {
            State::Complete(root) => return Ok(root),
            State::Prepared(root) => root,
            _ => return Err(BackupError::Finished),
        };
        let _admission = self.repository.admission(&self.completion)?;
        match self
            .repository
            .read_root(self.intent.backup, &self.completion)
        {
            Ok(existing) => {
                if existing != root {
                    return Err(BackupError::Integrity);
                }
                directory::synchronize(&self.repository.roots)?;
                self.state = State::Complete(root);
                self.lease.take();
                self.parent_lease.take();
                return Ok(root);
            }
            Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        control.cancellation().check()?;
        self.repository.advance_generation(control)?;
        let name = directory::name(self.intent.backup.as_bytes());
        #[cfg(test)]
        self.repository.before(super::Fault::RootInstall)?;
        self.directory
            .hard_link("prepared", &self.repository.roots, directory::text(&name))?;
        #[cfg(test)]
        self.repository.before(super::Fault::RootSync)?;
        directory::synchronize(&self.repository.roots)?;
        self.state = State::Complete(root);
        self.lease.take();
        self.parent_lease.take();
        Ok(root)
    }

    /// Cancellation before publication leaves no completed root. If a publication already installed its root, resolve that durable result instead.
    pub fn cancel(&mut self) -> BackupResult<Option<BackupRoot>> {
        if let State::Complete(root) = self.state {
            return Ok(Some(root));
        }
        if matches!(self.state, State::Cancelled) {
            return Ok(None);
        }
        self.state = State::Cancelling;
        let _admission = self.repository.admission(&self.completion)?;
        match self
            .repository
            .read_root(self.intent.backup, &self.completion)
        {
            Ok(root) => {
                if root.cut() != self.intent.cut || root.parent() != self.intent.parent {
                    return Err(BackupError::Integrity);
                }
                directory::synchronize(&self.repository.roots)?;
                self.state = State::Complete(root);
                self.lease.take();
                self.parent_lease.take();
                Ok(Some(root))
            }
            Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                match self
                    .directory
                    .hard_link("intent", &self.directory, "cancelled")
                {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        if self.repository.read_intent(
                            &self.directory,
                            "cancelled",
                            self.intent.backup,
                            &self.completion,
                        )? != self.intent
                        {
                            return Err(BackupError::Integrity);
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
                #[cfg(test)]
                self.repository.before(super::Fault::CancelSync)?;
                directory::synchronize(&self.directory)?;
                self.state = State::Cancelled;
                self.lease.take();
                self.parent_lease.take();
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}
