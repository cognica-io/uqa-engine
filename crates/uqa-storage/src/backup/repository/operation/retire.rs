//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    directory, io, BackupError, BackupId, BackupResult, BackupRoot, CancellationToken, Dir,
    LocalRepository, StorageReadControl,
};

impl LocalRepository {
    /// Retire a completed root or an abandoned/prepared operation. Active capture,
    /// publication and reader leases return `Busy`. A durable cancellation marker
    /// fences later publication before the root is removed; retrying the same ID
    /// resolves an interrupted removal. Object reclamation is a separate operation.
    pub fn remove_backup(&self, id: BackupId, control: &StorageReadControl) -> BackupResult<()> {
        let _admission = self.admission(control)?;
        let name = directory::name(id.as_bytes());
        let operation = directory::child(&self.operations, directory::text(&name).as_ref())?;
        let _lease = directory::lock(
            directory::open_file(&operation, "lease", true)?,
            false,
            control,
        )?;
        let intent = self.read_intent(&operation, "intent", id, control)?;
        let root = match self.read_root(id, control) {
            Ok(root) => Some(root),
            Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if root.is_some_and(|root| root.cut() != intent.cut || root.parent() != intent.parent) {
            return Err(BackupError::Integrity);
        }
        let retired = self.operation_cancelled(&operation, id, control)?;
        if retired && root.is_none() {
            #[cfg(test)]
            self.before(super::super::Fault::RemovalMarkerSync)?;
            directory::synchronize(&operation)?;
            return directory::synchronize(&self.roots);
        }
        control.cancellation().check()?;
        self.advance_removal_generation(control)?;
        // After retirement starts, cancellation cannot interrupt the durable fence
        // and unlink boundary. I/O errors remain recoverable by this same ID.
        let completion = StorageReadControl::new(control.memory(), &CancellationToken::new());
        if !retired {
            operation.hard_link("intent", &operation, "cancelled")?;
        }
        #[cfg(test)]
        self.before(super::super::Fault::RemovalMarker)?;
        // Authenticate the marker even on an interrupted previous attempt.
        if !self.operation_cancelled(&operation, id, &completion)? {
            return Err(BackupError::Integrity);
        }
        #[cfg(test)]
        self.before(super::super::Fault::RemovalMarkerSync)?;
        directory::synchronize(&operation)?;
        #[cfg(test)]
        self.before(super::super::Fault::RemovalRoot)?;
        if root.is_some() {
            self.roots.remove_file(directory::text(&name))?;
        }
        #[cfg(test)]
        self.before(super::super::Fault::RemovalSync)?;
        directory::synchronize(&self.roots)
    }

    pub(in crate::backup::repository) fn operation_cancelled(
        &self,
        operation: &Dir,
        id: BackupId,
        control: &StorageReadControl,
    ) -> BackupResult<bool> {
        match self.read_intent(operation, "cancelled", id, control) {
            Ok(cancelled) => {
                if cancelled != self.read_intent(operation, "intent", id, control)? {
                    return Err(BackupError::Integrity);
                }
                Ok(true)
            }
            Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(in crate::backup::repository) fn prepared_root(
        &self,
        operation: &Dir,
        id: BackupId,
        control: &StorageReadControl,
    ) -> BackupResult<Option<BackupRoot>> {
        if self.operation_cancelled(operation, id, control)? {
            return Ok(None);
        }
        let mut file = match directory::open_file(operation, "prepared", false) {
            Ok(file) => file,
            Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        let root = self.decode_root(&mut file, id, control)?;
        let intent = self.read_intent(operation, "intent", id, control)?;
        if root.cut() != intent.cut || root.parent() != intent.parent {
            return Err(BackupError::Integrity);
        }
        Ok(Some(root))
    }
}
