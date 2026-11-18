//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned Key/Value images preserve owner encodings and project out delivery and build state.

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
mod import;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub use import::{KeyValueImageImportReader, KeyValueImportUnit};

use std::sync::Arc;

use crate::{
    backup::{
        BackupCut, BackupError, BackupImageSource, BackupRecordMapping, BackupResult,
        BackupSourceStatistics, BackupViewQueryInput, ImageFormat, ImageProvider,
        MappedRecordBackupImage, NamespaceId, RecordImageCursor,
    },
    catalog::input::CatalogInputLimits,
    mvcc::{
        BackupCursorKey, CommittedBackupSnapshot, CommittedRecordSnapshot, NotificationRecordLayout,
    },
    read_control::StorageReadControl,
};

pub use crate::backup::{
    BackupSourceStep as KeyValueImageStep, BackupSourceUnit as KeyValueImageUnit,
};

const NAMESPACE: [u8; 16] = *b"UQA-KV-IMAGE-001";
const HEADER_BYTES: usize = 32;

struct Mapping {
    provider: ImageProvider,
    header: [u8; HEADER_BYTES],
    notification: BackupCursorKey,
}

impl BackupRecordMapping for Mapping {
    fn format(&self) -> ImageFormat {
        ImageFormat::new(self.provider, 1).expect("fixed nonzero image version")
    }
    fn namespace(&self) -> NamespaceId {
        NamespaceId::from_bytes(NAMESPACE).expect("fixed nonzero namespace")
    }
    fn header(&self) -> &[u8] {
        &self.header
    }
    fn stored_view_queries(
        &self,
        records: &RecordImageCursor,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Option<BackupViewQueryInput>> {
        BackupViewQueryInput::from_key_value_records(records, &[super::TAG_VIEW], limits, control)
            .map(Some)
    }
    fn stored_metadata_values(
        &self,
        records: &RecordImageCursor,
        prefix: &str,
        limits: crate::catalog::input::CatalogInputLimits,
        control: &crate::read_control::StorageReadControl,
    ) -> BackupResult<Option<crate::backup::BackupCatalogTextInput>> {
        crate::backup::BackupCatalogTextInput::from_filtered_metadata_records(
            records,
            &[super::TAG_METADATA],
            prefix,
            super::catalog::select_metadata,
            limits,
            control,
        )
        .map(Some)
    }
    fn include(
        &self,
        records: &dyn CommittedRecordSnapshot,
        key: &[u8],
        control: &StorageReadControl,
    ) -> BackupResult<(bool, usize)> {
        if key == &*self.notification {
            return Ok((false, 0));
        }
        Ok(super::diskann::backup::include(records, key, control)?)
    }
}

/// Online Key/Value mapping exporter over one captured joint cut. The shared stream owns consumption and retirement; this mapping excludes notification delivery and unpublished/retired `DiskANN` payloads while preserving evaluated owner encodings.
pub struct KeyValueBackupImage(MappedRecordBackupImage<Mapping>);

impl KeyValueBackupImage {
    /// The provider supplies its complete Key/Value cut. Native `SQLite` and the actual memory Engine have separate mapping owners.
    pub fn new(
        source: Arc<dyn CommittedBackupSnapshot>,
        provider: ImageProvider,
        control: &StorageReadControl,
    ) -> BackupResult<Self> {
        if !matches!(provider, ImageProvider::SQLiteKV | ImageProvider::Redb) {
            return Err(BackupError::Unsupported("Key/Value image provider"));
        }
        control.cancellation().check()?;
        let mut header = [0; HEADER_BYTES];
        header[..8].copy_from_slice(b"UQAKVI01");
        header[8..24].copy_from_slice(&source.database_id().as_bytes());
        header[24..].copy_from_slice(&source.records().sequence().as_u64().to_be_bytes());
        let mapping = Mapping {
            provider,
            header,
            notification: super::KeyValueNotificationRecords.key(control)?,
        };
        Ok(Self(MappedRecordBackupImage::from_mapping(
            source, mapping, control,
        )?))
    }
    pub const fn cut(&self) -> BackupCut {
        self.0.cut()
    }
    pub const fn visited_positions(&self) -> u64 {
        self.0.visited_positions()
    }
    pub const fn copied_source_bytes(&self) -> u64 {
        self.0.copied_source_bytes()
    }
    pub const fn metadata_read_bytes(&self) -> u64 {
        self.0.metadata_read_bytes()
    }
    pub const fn excluded_units(&self) -> u64 {
        self.0.excluded_units()
    }
    pub fn current_unit(&self) -> Option<KeyValueImageUnit<'_>> {
        self.0.current_unit()
    }
    pub fn advance(&mut self, positions: usize) -> BackupResult<KeyValueImageStep> {
        self.0.advance(positions)
    }
    pub fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        self.0.read_current(output)
    }
}

impl BackupImageSource for KeyValueBackupImage {
    fn reuse_current(&mut self, parent: crate::backup::UnitFingerprint) -> BackupResult<bool> {
        self.0.reuse_current(parent)
    }
    fn cut(&self) -> BackupCut {
        self.cut()
    }
    fn stored_view_queries(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> BackupResult<Option<BackupViewQueryInput>> {
        self.0.stored_view_queries(limits, control)
    }
    fn stored_metadata_values(
        &self,
        prefix: &str,
        limits: crate::catalog::input::CatalogInputLimits,
        control: &crate::read_control::StorageReadControl,
    ) -> BackupResult<Option<crate::backup::BackupCatalogTextInput>> {
        self.0.stored_metadata_values(prefix, limits, control)
    }

    fn advance(&mut self, maximum_positions: usize) -> BackupResult<KeyValueImageStep> {
        self.advance(maximum_positions)
    }
    fn current_unit(&self) -> Option<KeyValueImageUnit<'_>> {
        self.current_unit()
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        self.read_current(output)
    }
    fn statistics(&self) -> BackupSourceStatistics {
        self.0.statistics()
    }
}
