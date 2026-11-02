//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native image admission binds the original data namespace and current physical mapping.

use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    backup::{
        BackupCut, BackupError, BackupLease, BackupRecordImportMapping, BackupResult, BackupRoot,
        BackupSourceStep, ImageFormat, ImageProvider, MappedRecordImageImportReader, NamespaceId,
        RecordImageImportUnit,
    },
    mvcc::{DatabaseId, NotificationRecordLayout},
    read_control::StorageReadControl,
};

use super::super::{
    NativeRecordFamily, NativeRecordIdentity, NativeRecordNamespace, NativeRecordOwner,
};
use super::{HEADER_BYTES, NAMESPACE};

struct Mapping {
    namespace: Option<DatabaseId>,
    notification: BudgetedVec<u8>,
}

impl BackupRecordImportMapping for Mapping {
    fn format(&self) -> ImageFormat {
        ImageFormat::new(ImageProvider::SQLiteNative, 1).expect("fixed nonzero image version")
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
        control: &StorageReadControl,
    ) -> BackupResult<()> {
        if &bytes[..8] != b"UQANVI01"
            || bytes[8..24] != cut.source.as_bytes()
            || bytes[24..32] != cut.sequence.unwrap().as_u64().to_be_bytes()
            || bytes[32..48] == [0; 16]
            || bytes[48..52] != super::super::format::CURRENT_VERSION.to_be_bytes()
            || bytes[52..54] != NativeRecordFamily::ForeignServerMetadata.id().to_be_bytes()
            || bytes[54..] != 49u16.to_be_bytes()
        {
            return Err(BackupError::Invalid("native image header or mapping"));
        }
        let namespace = DatabaseId::from_bytes(bytes[32..48].try_into().unwrap());
        self.notification = NativeRecordNamespace(namespace).key(control)?;
        self.namespace = Some(namespace);
        Ok(())
    }
    fn validate_record_key(&self, key: &[u8], control: &StorageReadControl) -> BackupResult<()> {
        let identity = NativeRecordIdentity::decode_full(key, control)?;
        if let NativeRecordOwner::Database(namespace) = identity.owner() {
            if Some(namespace) != self.namespace {
                return Err(BackupError::Invalid("native image record namespace"));
            }
        }
        if key == &*self.notification {
            return Err(BackupError::Invalid("native image contains delivery state"));
        }
        Ok(())
    }
}

/// Validate native image envelope, mapping and record keys through the complete repository verifier. Record values remain provisional until their end and full archive verification. Native row, physical projection and logical dependency validation belong to the destination importer.
pub struct SQLiteNativeImageImportReader<'a>(MappedRecordImageImportReader<'a, Mapping>);

impl<'a> SQLiteNativeImageImportReader<'a> {
    pub fn new(
        lease: &'a BackupLease<'_>,
        maximum_workspace_bytes: u64,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let mapping = Mapping {
            namespace: None,
            notification: BudgetedVec::new(control.memory()),
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
    /// The original data namespace becomes available after header admission and retires with the input on completion or failure.
    pub fn native_namespace(&self) -> Option<DatabaseId> {
        self.0.mapping()?.namespace
    }
    pub fn current_unit(&self) -> Option<RecordImageImportUnit<'_>> {
        self.0.current_unit()
    }
    pub fn advance(&mut self, maximum_object_bytes: usize) -> BackupResult<BackupSourceStep> {
        self.0.advance(maximum_object_bytes)
    }
    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        self.0.read_current(output)
    }
}

#[cfg(test)]
mod tests;
