//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key/Value image admission preserves its mapping and excludes delivery authority.

use crate::{
    backup::{
        BackupCut, BackupError, BackupLease, BackupRecordImportMapping, BackupResult, BackupRoot,
        ImageFormat, ImageProvider, MappedRecordImageImportReader, NamespaceId,
    },
    mvcc::{BackupCursorKey, NotificationRecordLayout},
    read_control::StorageReadControl,
};

use super::{KeyValueImageStep, HEADER_BYTES, NAMESPACE};

pub use crate::backup::RecordImageImportUnit as KeyValueImportUnit;

struct Mapping {
    provider: ImageProvider,
    notification: BackupCursorKey,
}

impl BackupRecordImportMapping for Mapping {
    fn format(&self) -> ImageFormat {
        ImageFormat::new(self.provider, 1).expect("fixed nonzero image version")
    }
    fn namespace(&self) -> NamespaceId {
        NamespaceId::from_bytes(NAMESPACE).expect("fixed nonzero namespace")
    }
    fn header_bytes(&self) -> usize {
        HEADER_BYTES
    }
    fn decode_header(
        &mut self,
        bytes: &[u8],
        cut: BackupCut,
        _control: &StorageReadControl,
    ) -> BackupResult<()> {
        if &bytes[..8] != b"UQAKVI01"
            || bytes[8..24] != cut.source.as_bytes()
            || bytes[24..] != cut.sequence.unwrap().as_u64().to_be_bytes()
        {
            return Err(BackupError::Invalid(
                "Key/Value image header differs from root",
            ));
        }
        Ok(())
    }
    fn validate_record_key(&self, key: &[u8], _control: &StorageReadControl) -> BackupResult<()> {
        if key == &*self.notification {
            return Err(BackupError::Invalid(
                "Key/Value image contains delivery state",
            ));
        }
        Ok(())
    }
}

/// Checks the Key/Value header and key domain through the shared complete image verifier. Logical catalog and artifact validation remain with their respective owners before target publication.
pub struct KeyValueImageImportReader<'a>(MappedRecordImageImportReader<'a, Mapping>);

impl<'a> KeyValueImageImportReader<'a> {
    pub fn new(
        lease: &'a BackupLease<'_>,
        provider: ImageProvider,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        if !matches!(provider, ImageProvider::Redb | ImageProvider::SQLiteKV) {
            return Err(BackupError::Unsupported("Key/Value import image format"));
        }
        let mapping = Mapping {
            provider,
            notification: crate::key_value::KeyValueNotificationRecords.key(control)?,
        };
        Ok(Self(MappedRecordImageImportReader::new(
            lease,
            mapping,
            maximum_workspace_bytes,
            control,
        )?))
    }
    pub const fn root(&self) -> BackupRoot {
        self.0.root()
    }
    pub fn current_unit(&self) -> Option<KeyValueImportUnit<'_>> {
        self.0.current_unit()
    }
    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<KeyValueImageStep> {
        self.0.advance(maximum_object_bytes)
    }
    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        self.0.read_current(output)
    }
}

#[cfg(test)]
mod tests;
