//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical memory reads borrow the fixed root and copy only one requested vector.

use super::{MemoryVectorIndex, StorageBackendResult, StorageReadControl};
use crate::vector_index::{copy_vector, VectorRead};
use sha2::{Digest, Sha256};
use std::ops::Bound::{Excluded, Unbounded};
use uqa_core::{memory::BudgetedVec, DocId};

impl MemoryVectorIndex {
    pub(super) fn fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 32]> {
        control.check()?;
        if let Some(fingerprint) = self.state.fingerprint.get() {
            return Ok(*fingerprint);
        }
        let mut digest = Sha256::new();
        digest.update(b"uqa-retained-vector-values-v1\0");
        digest.update(self.dimensions.to_le_bytes());
        for (&document, vectors) in &self.state.vectors {
            for (ordinal, vector) in vectors.iter().enumerate() {
                control.check()?;
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    crate::StorageBackendError::Other(
                        "vector ordinal exceeds u32 index format".into(),
                    )
                })?;
                digest.update(document.to_le_bytes());
                digest.update(ordinal.to_le_bytes());
                digest.update((vector.len() as u64).to_le_bytes());
                for chunk in vector.chunks(1024) {
                    control.check()?;
                    for value in chunk {
                        digest.update(value.to_bits().to_le_bytes());
                    }
                }
            }
        }
        control.check()?;
        let fingerprint = digest.finalize().into();
        let _ = self.state.fingerprint.set(fingerprint);
        Ok(fingerprint)
    }
}

impl VectorRead for MemoryVectorIndex {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        control.check()
    }
    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        self.fingerprint(control).map(Some)
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        control.check()?;
        Ok(self
            .state
            .vectors
            .first_from(after.as_ref().map_or(Unbounded, Excluded))
            .map(|(&document, _)| document))
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        control.check()?;
        Ok(self
            .state
            .vectors
            .get(&document)
            .map_or(0, |vectors| vectors.len() as u64))
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        control.check()?;
        self.state
            .vectors
            .get(&document)
            .and_then(|vectors| vectors.get(ordinal as usize))
            .map(|vector| copy_vector(vector, control))
            .transpose()
    }
}
