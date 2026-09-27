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
mod selection;
pub(crate) use fingerprint::selected_fingerprint;
pub use selection::SelectedVectorRead;

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
    for (position, bytes) in bytes.chunks_exact(4).enumerate() {
        if position.is_multiple_of(1024) {
            control.check()?;
        }
        vector.push(f32::from_le_bytes(
            bytes.try_into().expect("four-byte float"),
        ))?;
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
