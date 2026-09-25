//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DiskANN` origins accompany the existing canonical vector keys, without a duplicate corpus.

mod retained;
#[cfg(test)]
mod tests;
pub use retained::{DiskANNCanonicalVectorVisitor, RetainedDiskANNCanonical};

use std::sync::Arc;

use super::KeyValueVectorIndex;
use crate::diskann_index::format::DiskANNVectorVersion;
use crate::key_value::{codec, KeyValueRead};
use crate::mvcc::{DatabaseId, StorageTransactionId, VersionError};
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, StorageBackendResult};
use uqa_core::DocId;

const ROOT: &[u8] = b"\0uqa-diskann-canonical-v1\0";
const MAGIC: &[u8; 8] = b"UQAVORG1";
const BYTES: usize = 56;

/// Canonical tensor mutation owner for the Key/Value layout. Public `DiskANN` catalog routing remains unavailable until publication and recovery are integrated.
pub struct KeyValueDiskANNCanonical {
    index: KeyValueVectorIndex,
}

impl KeyValueDiskANNCanonical {
    pub fn new(
        store: Arc<dyn KeyValueStore>,
        table: impl Into<String>,
        field: impl Into<String>,
        dimensions: u32,
    ) -> StorageBackendResult<Self> {
        if dimensions == 0 || !store.transaction_model().is_versioned() {
            return Err(invalid(
                "canonical origins require dimensions and a versioned store",
            ));
        }
        Ok(Self {
            index: KeyValueVectorIndex::new(store, table, field, dimensions),
        })
    }

    /// Atomically replace every ordinal and its origin; an empty tensor persists an explicit zero-count replacement. The publishing transaction is also used for private origins, without assigning commit order.
    pub fn replace(
        &self,
        document: DocId,
        vectors: &[Vec<f32>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNVectorVersion> {
        control.check()?;
        for vector in vectors {
            crate::vector_index::validate_vector_values_controlled(
                self.index.dimensions,
                vector,
                Some(control),
            )?;
        }
        let count = codec::usize_to_u64(vectors.len(), "canonical vector count")?;
        codec::validate_vector_ordinal_count(count)?;
        let _workspace = control.memory().reserve(self.workspace_bytes(true)?)?;
        let key = key(&self.index.table, &self.index.field, document)?;
        let mut version = None;
        self.index
            .store
            .with_versioned_mutation(&mut |origin, _, batch| {
                control.check()?;
                let current = DiskANNVectorVersion::new(origin.transaction(), origin.revision())?;
                let record = Record {
                    version: current,
                    dimensions: self.index.dimensions,
                    count,
                };
                self.index.stage_replace(batch, document, vectors)?;
                batch.put(&key, &record.encode())?;
                control.check()?;
                version = Some(current);
                Ok(())
            })?;
        version.ok_or_else(|| invalid("canonical mutation did not execute"))
    }

    /// Retain the exact private/committed boundary without enumerating canonical values.
    pub fn retain(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<RetainedDiskANNCanonical> {
        control.check()?;
        let _workspace = control.memory().reserve(self.workspace_bytes(false)?)?;
        let vectors = codec::vector_field_prefix(&self.index.table, &self.index.field)?;
        let origins = prefix(&self.index.table, &self.index.field)?;
        let mut selected = None;
        self.index
            .store
            .with_read_view(&mut |read: &dyn KeyValueRead| {
                selected = Some(RetainedDiskANNCanonical::new(
                    read.retain(&[&vectors, &origins])?,
                    &vectors,
                    &origins,
                    self.index.dimensions,
                    control,
                )?);
                Ok(())
            })?;
        selected.ok_or_else(|| invalid("canonical read did not execute"))
    }

    fn workspace_bytes(&self, writing: bool) -> StorageBackendResult<usize> {
        // Existing key/blob encoders own ordinary Vec buffers; reserve their simultaneous capacity before invoking them.
        self.index
            .table
            .len()
            .checked_add(self.index.field.len())
            .and_then(|bytes| bytes.checked_add(ROOT.len() + 64))
            .and_then(|bytes| bytes.checked_mul(8))
            .and_then(|bytes| {
                bytes.checked_add(
                    usize::try_from(if writing { self.index.dimensions } else { 0 })
                        .ok()?
                        .checked_mul(4)?,
                )
            })
            .ok_or_else(|| uqa_core::memory::MemoryError::SizeOverflow.into())
    }
}

pub(super) fn prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let canonical = codec::vector_field_prefix(table, field)?;
    let mut prefix = Vec::with_capacity(ROOT.len() + canonical.len());
    prefix.extend_from_slice(ROOT);
    prefix.extend_from_slice(&canonical);
    Ok(prefix)
}

pub(super) fn key(table: &str, field: &str, document: DocId) -> StorageBackendResult<Vec<u8>> {
    let mut key = prefix(table, field)?;
    key.extend_from_slice(&document.to_be_bytes());
    Ok(key)
}

#[derive(Clone, Copy)]
struct Record {
    version: DiskANNVectorVersion,
    dimensions: u32,
    count: u64,
}

impl Record {
    fn encode(self) -> [u8; BYTES] {
        let mut bytes = [0; BYTES];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..24].copy_from_slice(&self.version.writer().database().as_bytes());
        bytes[24..32].copy_from_slice(&self.version.writer().allocation().to_le_bytes());
        bytes[32..40].copy_from_slice(&self.version.revision().to_le_bytes());
        bytes[40..44].copy_from_slice(&self.dimensions.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.count.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8], dimensions: u32) -> StorageBackendResult<Self> {
        if bytes.len() != BYTES || &bytes[..8] != MAGIC || bytes[44..48] != [0; 4] {
            return Err(invalid("invalid canonical origin envelope"));
        }
        let read = |offset: usize| -> [u8; 8] {
            bytes[offset..offset + 8]
                .try_into()
                .expect("validated width")
        };
        if bytes[40..44] != dimensions.to_le_bytes() {
            return Err(invalid("canonical origin dimension mismatch"));
        }
        let database = DatabaseId::from_bytes(bytes[8..24].try_into().expect("validated width"));
        let writer = StorageTransactionId::new(database, u64::from_le_bytes(read(24)))
            .map_err(VersionError::into_storage_error)?;
        let version = DiskANNVectorVersion::new(writer, u64::from_le_bytes(read(32)))?;
        let count = u64::from_le_bytes(read(48));
        codec::validate_vector_ordinal_count(count)?;
        Ok(Self {
            version,
            dimensions,
            count,
        })
    }
}

fn invalid(message: &'static str) -> crate::StorageBackendError {
    VersionError::InvalidEncoding(message).into_storage_error()
}
