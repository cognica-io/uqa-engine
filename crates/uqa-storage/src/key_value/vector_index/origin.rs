//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DiskANN` origins accompany the existing canonical vector keys, without a duplicate corpus.

pub(in crate::key_value) mod journal;
mod retained;
#[cfg(test)]
mod tests;
pub use retained::{DiskANNCanonicalVectorVisitor, RetainedDiskANNCanonical};

use std::sync::Arc;

use super::KeyValueVectorIndex;
use crate::diskann_index::format::{
    DiskANNCanonicalOrigin as Record, DiskANNChangeIdentity, DiskANNVectorVersion,
    CANONICAL_ORIGIN_BYTES as BYTES,
};
use crate::key_value::{codec, KeyValueRead};
use crate::mvcc::VersionError;
use crate::read_control::StorageReadControl;
use crate::{KeyValueStore, RelationIdentity, StorageBackendResult};
use uqa_core::DocId;

const ROOT: &[u8] = b"\0uqa-diskann-canonical-v1\0";

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
                let record = Record::new(current, self.index.dimensions, count)?;
                self.index.stage_replace(batch, document, vectors)?;
                batch.put(&key, &record.encode())?;
                batch.put(
                    &journal::key(
                        &self.index.table,
                        &self.index.field,
                        DiskANNChangeIdentity::new(document, current),
                    )?,
                    &record.encode(),
                )?;
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
        self.retain_selected(None, control)
    }

    /// Capture the real table/index definitions with canonical input on one fixed view. Ordinary unbound sources cannot authorize a later catalog publication.
    pub fn retain_for_index(
        &self,
        index: &RelationIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<RetainedDiskANNCanonical> {
        self.retain_selected(Some(index), control)
    }

    fn retain_selected(
        &self,
        index: Option<&RelationIdentity>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<RetainedDiskANNCanonical> {
        control.check()?;
        let _workspace = control.memory().reserve(self.workspace_bytes(false)?)?;
        let vectors = codec::vector_field_prefix(&self.index.table, &self.index.field)?;
        let origins = prefix(&self.index.table, &self.index.field)?;
        let changes = journal::prefix(&self.index.table, &self.index.field)?;
        let mut selected = None;
        self.index
            .store
            .with_read_view(&mut |read: &dyn KeyValueRead| {
                let binding = index
                    .map(|index| {
                        crate::key_value::catalog::diskann::Binding::capture(
                            read,
                            &self.index.table,
                            &self.index.field,
                            self.index.dimensions,
                            index,
                            control,
                        )
                    })
                    .transpose()?;
                let mut prefixes = uqa_core::memory::BudgetedVec::new(control.memory());
                prefixes.extend_from_slice(&[&*vectors, &*origins, &*changes])?;
                if let Some(binding) = &binding {
                    prefixes.extend_from_slice(&binding.prefixes())?;
                    prefixes.push(crate::key_value::diskann::READ_PREFIX)?;
                }
                let mut source = RetainedDiskANNCanonical::new(
                    read.retain(&prefixes)?,
                    &vectors,
                    &origins,
                    &changes,
                    self.index.dimensions,
                    control,
                )?;
                drop(prefixes);
                source.binding = binding;
                selected = Some(source);
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
            .and_then(|bytes| bytes.checked_add(ROOT.len() + 128))
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

pub(in crate::key_value) fn table_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    Ok(namespaced(&codec::vector_key_prefix(table)?))
}

pub(in crate::key_value) fn prefix(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    Ok(namespaced(&codec::vector_field_prefix(table, field)?))
}

fn namespaced(canonical: &[u8]) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(ROOT.len() + canonical.len());
    prefix.extend_from_slice(ROOT);
    prefix.extend_from_slice(canonical);
    prefix
}

pub(super) fn key(table: &str, field: &str, document: DocId) -> StorageBackendResult<Vec<u8>> {
    let mut key = prefix(table, field)?;
    key.extend_from_slice(&document.to_be_bytes());
    Ok(key)
}

fn invalid(message: &'static str) -> crate::StorageBackendError {
    VersionError::InvalidEncoding(message).into_storage_error()
}
