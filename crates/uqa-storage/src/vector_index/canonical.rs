//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable raw vectors are independent of a physical index's mutation provenance.

use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};

mod fingerprint;
mod ordered;
mod selection;
pub(crate) use fingerprint::selected_fingerprint;
pub(crate) use ordered::{copy_vector, ordinal_count};
pub use selection::SelectedVectorRead;

#[cfg(test)]
mod tests;

/// A fixed canonical field view. Owners retain their original visibility, allowance and cancellation. Metadata enumeration never decodes coordinates; a point read owns at most one vector and releases provider locks before returning. This interface makes no claim about physical index lineage or mutation origins.
pub trait VectorRead: Send + Sync {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()>;
    fn dimensions(&self) -> u32;
    /// Trustworthy identity of this complete fixed field, obtained from retained metadata. None means fixed-model reuse cannot be verified; callers must not replace it with a user-supplied label.
    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        self.check_control(control)?;
        Ok(None)
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>>;
    /// Validate contiguous ordinal keys and return their count. Missing and explicitly empty tensors both have zero vectors.
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64>;
    /// Read one ordinal without invoking a caller while a provider read lock is held. Absent ordinals return None; malformed values fail.
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>>;
}

pub type VectorReadSnapshot = Arc<dyn VectorRead>;

/// Compare a complete replacement with the selected canonical document, keeping at most one existing vector in memory. Float bits, ordinal order and tensor length all matter; normalized equality is insufficient for a canonical replacement.
pub(crate) fn canonical_vectors_equal(
    source: &dyn VectorRead,
    document: DocId,
    vectors: &[Vec<f32>],
    control: &StorageReadControl,
) -> StorageBackendResult<bool> {
    source.check_control(control)?;
    for vector in vectors {
        super::validate_vector_values_controlled(source.dimensions(), vector, Some(control))?;
    }
    if source.document_vector_count(document, control)? != vectors.len() as u64 {
        return Ok(false);
    }
    for (ordinal, expected) in vectors.iter().enumerate() {
        let ordinal =
            u32::try_from(ordinal).map_err(|_| uqa_core::memory::MemoryError::SizeOverflow)?;
        let Some(actual) = source.read_vector(document, ordinal, control)? else {
            return Ok(false);
        };
        if actual.len() != expected.len() {
            return Ok(false);
        }
        for (actual, expected) in actual.chunks(1024).zip(expected.chunks(1024)) {
            source.check_control(control)?;
            if actual
                .iter()
                .zip(expected)
                .any(|(a, b)| a.to_bits() != b.to_bits())
            {
                return Ok(false);
            }
        }
    }
    source.check_control(control)?;
    Ok(true)
}

/// Decode one little-endian canonical float vector under the invoking read allowance.
pub fn decode_vector_bytes(
    bytes: &[u8],
    dimensions: u32,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<f32>> {
    control.check()?;
    let expected = usize::try_from(dimensions)
        .map_err(|_| uqa_core::memory::MemoryError::SizeOverflow)?
        .checked_mul(4)
        .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
    if bytes.len() != expected {
        return Err(
            crate::mvcc::VersionError::InvalidEncoding("canonical vector width mismatch")
                .into_storage_error(),
        );
    }
    let mut vector = BudgetedVec::new(control.memory());
    vector.reserve(bytes.len() / 4)?;
    for (position, bytes) in bytes.as_chunks::<4>().0.iter().enumerate() {
        if position.is_multiple_of(1024) {
            control.check()?;
        }
        vector.push(f32::from_le_bytes(*bytes))?;
    }
    super::validate_vector_values_controlled(dimensions, &vector, Some(control))?;
    Ok(vector)
}

impl<T: VectorRead> VectorRead for Budgeted<T> {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        (**self).check_control(control)
    }
    fn dimensions(&self) -> u32 {
        (**self).dimensions()
    }
    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        (**self).corpus_fingerprint(control)
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        (**self).next_document_after(after, control)
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        (**self).document_vector_count(document, control)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        (**self).read_vector(document, ordinal, control)
    }
}
