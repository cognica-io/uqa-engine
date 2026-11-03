//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native image membership preserves evaluated rows and the stable data namespace.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    backup::{
        BackupCut, BackupError, BackupImageSource, BackupRecordMapping, BackupResult,
        BackupSourceStatistics, BackupSourceStep, BackupSourceUnit, ImageFormat, ImageProvider,
        MappedRecordBackupImage, NamespaceId,
    },
    key_value::diskann_backup_membership,
    mvcc::{CommittedRecordSnapshot, DatabaseId, NotificationRecordLayout, VersionError},
    read_control::StorageReadControl,
};

use super::{
    decode_record, NativeRecordFamily, NativeRecordIdentity, NativeRecordNamespace,
    NativeRecordOwner,
};
use crate::ManagedConnection;

const NAMESPACE: [u8; 16] = *b"UQA-NATIVE-IMG01";
const HEADER_BYTES: usize = 56;

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod import;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub use import::SQLiteNativeImageImportReader;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod restore;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub use restore::SQLiteNativeBackupImport;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(super) mod validation;

#[cfg(all(test, any(windows, all(unix, not(target_os = "emscripten")))))]
mod test_image;
#[cfg(all(test, any(windows, all(unix, not(target_os = "emscripten")))))]
mod tests;

struct Mapping {
    header: [u8; HEADER_BYTES],
    namespace: DatabaseId,
    notification: BudgetedVec<u8>,
}

impl BackupRecordMapping for Mapping {
    fn format(&self) -> ImageFormat {
        ImageFormat::new(ImageProvider::SQLiteNative, 1).expect("fixed nonzero image version")
    }
    fn namespace(&self) -> NamespaceId {
        NamespaceId::from_bytes(NAMESPACE).expect("fixed nonzero namespace")
    }
    fn header(&self) -> &[u8] {
        &self.header
    }

    fn include(
        &self,
        records: &dyn CommittedRecordSnapshot,
        key: &[u8],
        control: &StorageReadControl,
    ) -> BackupResult<(bool, usize)> {
        let identity = NativeRecordIdentity::decode_full(key, control)?;
        if let NativeRecordOwner::Database(namespace) = identity.owner() {
            if namespace != self.namespace {
                return Err(BackupError::Invalid(
                    "native image record namespace mismatch",
                ));
            }
        }
        if key == &*self.notification {
            return Ok((false, 0));
        }
        if identity.family() != NativeRecordFamily::DiskANNRecords {
            return Ok((true, 0));
        }
        let mut logical = BudgetedVec::new(control.memory());
        NativeRecordIdentity::visit_key_components(key, control, |_, value| {
            let ValueRef::Blob(bytes) = value else {
                return Err(VersionError::InvalidEncoding(
                    "native image DiskANN key is not binary",
                ));
            };
            logical.extend_from_slice(bytes)?;
            Ok(())
        })?;
        Ok(diskann_backup_membership(&logical, |state_key, output| {
            let key = identity
                .encode_key(&[ValueRef::Blob(state_key)], control)
                .map_err(VersionError::into_storage_error)?;
            let length = super::binary_pair_limit(state_key.len(), output.len())
                .map_err(VersionError::into_storage_error)?;
            let mut row = BudgetedVec::new(control.memory());
            row.reserve(length)?;
            for _ in 0..length {
                row.push(0)?;
            }
            let chunk = records
                .read_value_chunk(&key, 0, &mut row, control)
                .map_err(VersionError::into_storage_error)?
                .ok_or_else(|| {
                    VersionError::InvalidEncoding("native backup generation has no state")
                        .into_storage_error()
                })?;
            if chunk.total_bytes() != Some(length) || chunk.copied() != length {
                return Err(
                    VersionError::InvalidEncoding("native backup generation state length")
                        .into_storage_error(),
                );
            }
            let (_, values) =
                decode_record(&key, &row, control).map_err(VersionError::into_storage_error)?;
            let [ValueRef::Blob(logical), ValueRef::Blob(state)] = &*values else {
                return Err(
                    VersionError::InvalidEncoding("native backup generation state row")
                        .into_storage_error(),
                );
            };
            if *logical != state_key || state.len() != output.len() {
                return Err(VersionError::InvalidEncoding(
                    "native backup generation state binding",
                )
                .into_storage_error());
            }
            output.copy_from_slice(state);
            Ok(length)
        })?)
    }
}

/// A complete native record-image stream over one committed record/allocator cut. The header preserves the data namespace and native mapping version separately from transaction history. Native rows retain their exact revisions and evaluated bytes; notification delivery and unpublished/retired `DiskANN` payloads are excluded. Import and logical dependency validation are separate owners.
pub struct SQLiteNativeBackupImage(MappedRecordBackupImage<Mapping>);

impl SQLiteNativeBackupImage {
    pub(crate) fn capture(
        connection: &ManagedConnection,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        let (namespace, snapshot) = connection.capture_native_backup_snapshot(control)?;
        let mut header = [0; HEADER_BYTES];
        header[..8].copy_from_slice(b"UQANVI01");
        header[8..24].copy_from_slice(&snapshot.database_id().as_bytes());
        header[24..32].copy_from_slice(&snapshot.records().sequence().as_u64().to_be_bytes());
        header[32..48].copy_from_slice(&namespace.as_bytes());
        header[48..52].copy_from_slice(&super::format::CURRENT_VERSION.to_be_bytes());
        header[52..54]
            .copy_from_slice(&NativeRecordFamily::ForeignServerMetadata.id().to_be_bytes());
        header[54..].copy_from_slice(&49u16.to_be_bytes());
        let mapping = Mapping {
            header,
            namespace,
            notification: NativeRecordNamespace(namespace).key(control)?,
        };
        Ok(Self(MappedRecordBackupImage::from_mapping(
            snapshot, mapping, control,
        )?))
    }

    pub const fn cut(&self) -> BackupCut {
        self.0.cut()
    }
    pub fn advance(&mut self, positions: usize) -> BackupResult<BackupSourceStep> {
        self.0.advance(positions)
    }
    pub fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.0.current_unit()
    }
    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        self.0.read_current(output)
    }
    pub fn statistics(&self) -> BackupSourceStatistics {
        self.0.statistics()
    }
}

impl BackupImageSource for SQLiteNativeBackupImage {
    fn cut(&self) -> BackupCut {
        self.cut()
    }
    fn advance(&mut self, maximum_positions: usize) -> BackupResult<BackupSourceStep> {
        self.advance(maximum_positions)
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.current_unit()
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        self.read_current(output)
    }
    fn statistics(&self) -> BackupSourceStatistics {
        self.statistics()
    }
}
