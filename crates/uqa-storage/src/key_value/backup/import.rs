//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared image decoding preserves raw Key/Value encodings without executing stored expressions.

use crate::{
    backup::{
        BackupError, BackupImageReader, BackupLease, BackupReadStep, BackupResult, BackupRoot,
        ImageProvider, UnitKind,
    },
    mvcc::{
        BackupCursorKey, CommitSequence, NotificationRecordLayout, RECLAMATION_DOMAIN_PREFIX,
        RECLAMATION_EPOCH_NAMESPACE,
    },
    read_control::StorageReadControl,
};

use super::{KeyValueImageStep, HEADER_BYTES, NAMESPACE};

/// The current unit's revision or watermark has already been consumed and checked. Record payloads remain provisional until their end and complete repository verification.
#[derive(Clone, Copy, Debug)]
pub enum KeyValueImportUnit<'a> {
    Identifier {
        namespace: &'a [u8],
        watermark: u64,
    },
    Record {
        key: &'a [u8],
        revision: CommitSequence,
        value_bytes: u64,
    },
}

/// Checks the mapping envelope, ordered header, allocator exclusions and record revisions while sharing the repository's complete integrity verifier. Logical catalog and artifact validation remain with their respective format owners before target publication.
pub struct KeyValueImageImportReader<'a> {
    reader: Option<BackupImageReader<'a>>,
    root: BackupRoot,
    notification: BackupCursorKey,
    control: StorageReadControl,
    header: bool,
    current: Option<u64>,
    complete: bool,
}

impl<'a> KeyValueImageImportReader<'a> {
    pub fn new(
        lease: &'a BackupLease<'_>,
        provider: ImageProvider,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let root = lease.root();
        let cut = root.cut();
        if !matches!(provider, ImageProvider::Redb | ImageProvider::SQLiteKV)
            || cut.format.provider() != provider
            || cut.format.version() != 1
            || cut.sequence.is_none()
        {
            return Err(BackupError::Unsupported("Key/Value import image format"));
        }
        let notification = crate::key_value::KeyValueNotificationRecords.key(control)?;
        Ok(Self {
            reader: Some(lease.read_image(maximum_workspace_bytes, control)?),
            root,
            notification,
            control: control.clone(),
            header: false,
            current: None,
            complete: false,
        })
    }

    pub const fn root(&self) -> BackupRoot {
        self.root
    }

    pub fn current_unit(&self) -> Option<KeyValueImportUnit<'_>> {
        let value = self.current?;
        let entry = self.reader.as_ref()?.current_unit()?;
        let key = entry.address().key;
        Some(if entry.address().family == 2 {
            KeyValueImportUnit::Identifier {
                namespace: key,
                watermark: value,
            }
        } else {
            KeyValueImportUnit::Record {
                key,
                revision: CommitSequence::from_u64(value),
                value_bytes: entry.decoded_bytes() - 8,
            }
        })
    }

    /// Consume one envelope or one repository verification window. Record values must be drained through their verified end before advancing; identifiers have no unread payload.
    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<KeyValueImageStep> {
        let result = self.advance_inner(maximum_object_bytes);
        if result.is_err() && !self.complete {
            self.release(false);
        }
        result
    }

    fn advance_inner(&mut self, maximum: usize) -> BackupResult<KeyValueImageStep> {
        if self.complete {
            return Ok(KeyValueImageStep::Complete);
        }
        let reader = self.reader.as_mut().ok_or(BackupError::Finished)?;
        self.current = None;
        match reader.advance(maximum)? {
            BackupReadStep::Pending => return Ok(KeyValueImageStep::Pending),
            BackupReadStep::Complete => {
                if !self.header || reader.verified().is_none() {
                    return Err(BackupError::Invalid("missing Key/Value image header"));
                }
                self.release(true);
                return Ok(KeyValueImageStep::Complete);
            }
            BackupReadStep::Unit => (),
        }
        let entry = reader.current_unit().expect("selected repository unit");
        let address = entry.address();
        if address.namespace.as_bytes() != NAMESPACE || address.generation != 0 {
            return Err(BackupError::Invalid("Key/Value image address"));
        }
        if !self.header {
            if address.family != 1
                || !address.key.is_empty()
                || entry.kind() != UnitKind::Metadata
                || entry.decoded_bytes() != HEADER_BYTES as u64
            {
                return Err(BackupError::Invalid("Key/Value image header unit"));
            }
            let mut bytes = [0; HEADER_BYTES];
            read_exact(reader, &mut bytes)?;
            require_end(reader)?;
            let cut = self.root.cut();
            if &bytes[..8] != b"UQAKVI01"
                || bytes[8..24] != cut.source.as_bytes()
                || bytes[24..] != cut.sequence.unwrap().as_u64().to_be_bytes()
            {
                return Err(BackupError::Invalid(
                    "Key/Value image header differs from root",
                ));
            }
            self.header = true;
            return Ok(KeyValueImageStep::Pending);
        }
        match address.family {
            2 if entry.kind() == UnitKind::Metadata && entry.decoded_bytes() == 8 => {
                if address.key.is_empty()
                    || address.key == RECLAMATION_EPOCH_NAMESPACE
                    || address.key.starts_with(RECLAMATION_DOMAIN_PREFIX)
                {
                    return Err(BackupError::Invalid("Key/Value image identifier namespace"));
                }
            }
            3 if entry.kind() == UnitKind::Record && entry.decoded_bytes() >= 8 => {
                if address.key == &*self.notification {
                    return Err(BackupError::Invalid(
                        "Key/Value image contains delivery state",
                    ));
                }
            }
            _ => {
                return Err(BackupError::Invalid(
                    "Key/Value image unit family or length",
                ))
            }
        }
        let identifier = address.family == 2;
        let mut bytes = [0; 8];
        read_exact(reader, &mut bytes)?;
        let value = u64::from_be_bytes(bytes);
        if identifier {
            require_end(reader)?;
        } else if value == 0 || value > self.root.cut().sequence.unwrap().as_u64() {
            return Err(BackupError::Invalid("Key/Value image record revision"));
        }
        self.current = Some(value);
        Ok(KeyValueImageStep::Unit)
    }

    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let result = if matches!(self.current_unit(), Some(KeyValueImportUnit::Record { .. })) {
            self.reader.as_mut().unwrap().read_current(output)
        } else {
            Err(BackupError::Finished)
        };
        if result.is_err() && !self.complete {
            self.release(false);
        }
        result
    }

    fn release(&mut self, complete: bool) {
        self.complete = complete;
        self.reader = None;
        self.current = None;
        self.notification = BackupCursorKey::new(self.control.memory());
    }
}

fn read_exact(reader: &mut BackupImageReader<'_>, mut output: &mut [u8]) -> BackupResult<()> {
    while !output.is_empty() {
        let read = reader.read_current(output)?;
        if read == 0 {
            return Err(BackupError::Invalid("truncated Key/Value image prefix"));
        }
        output = &mut output[read..];
    }
    Ok(())
}

fn require_end(reader: &mut BackupImageReader<'_>) -> BackupResult<()> {
    if reader.read_current(&mut [0])? != 0 {
        return Err(BackupError::Invalid("trailing Key/Value image bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
