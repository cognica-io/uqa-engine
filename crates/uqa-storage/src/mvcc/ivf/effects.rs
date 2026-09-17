//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained immutable document inputs and their sealed transaction fingerprint.

use crate::mvcc::{
    key::RecordKey, CommitSequence, PreparedRecordCommit, VersionError, VersionResult,
};
use crate::{ivf_index::IVFMutation, read_control::StorageReadControl};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryError},
    DocId,
};

#[derive(Clone)]
pub(in crate::mvcc) struct OwnedIVFMutation {
    pub(in crate::mvcc) metadata: RecordKey,
    pub(in crate::mvcc) document: DocId,
    vectors: Option<Arc<Budgeted<Vec<Vec<f32>>>>>,
}

impl OwnedIVFMutation {
    pub(in crate::mvcc) fn retain(
        metadata: &[u8],
        mutation: IVFMutation<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        control.cancellation().check()?;
        let (document, vectors) = match mutation {
            IVFMutation::Replace { document, vectors } => {
                let mut bytes = vectors
                    .len()
                    .checked_mul(size_of::<Vec<f32>>())
                    .ok_or(MemoryError::SizeOverflow)?;
                for vector in vectors {
                    control.cancellation().check()?;
                    bytes = bytes
                        .checked_add(
                            vector
                                .len()
                                .checked_mul(size_of::<f32>())
                                .ok_or(MemoryError::SizeOverflow)?,
                        )
                        .ok_or(MemoryError::SizeOverflow)?;
                }
                let memory = control.memory().reserve(bytes)?;
                let mut owned = Vec::with_capacity(vectors.len());
                for vector in vectors {
                    control.cancellation().check()?;
                    owned.push(vector.clone());
                }
                (document, Some(Budgeted::new(owned, memory).into_shared()?))
            }
            IVFMutation::Delete(document) => (document, None),
            IVFMutation::Clear | IVFMutation::Train => {
                return Err(VersionError::InvalidEncoding(
                    "structural IVF changes require conditional publication",
                ))
            }
        };
        Ok(Self {
            metadata: RecordKey::new(metadata, control.memory())?,
            document,
            vectors,
        })
    }
    pub(in crate::mvcc) fn borrowed(&self) -> IVFMutation<'_> {
        self.vectors
            .as_ref()
            .map_or(IVFMutation::Delete(self.document), |vectors| {
                IVFMutation::Replace {
                    document: self.document,
                    vectors,
                }
            })
    }
}

pub(in crate::mvcc) struct IVFEffects {
    pub(in crate::mvcc) operations: BudgetedVec<OwnedIVFMutation>,
}

impl PreparedRecordCommit {
    pub(in crate::mvcc) fn with_ivf_effects(
        mut self,
        base: CommitSequence,
        operations: &[OwnedIVFMutation],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        if operations.is_empty() {
            return Ok(self);
        }
        let mut owned = BudgetedVec::new(control.memory());
        owned.reserve(operations.len())?;
        let mut digest = Sha256::new();
        digest.update(b"UQA prepared IVF effects 1");
        digest.update(self.fingerprint());
        digest.update(base.as_u64().to_be_bytes());
        digest.update((operations.len() as u64).to_be_bytes());
        for operation in operations {
            control.cancellation().check()?;
            let key = operation.metadata.bytes();
            digest.update((key.len() as u64).to_be_bytes());
            for part in key.chunks(4096) {
                control.cancellation().check()?;
                digest.update(part);
            }
            digest.update(operation.document.to_be_bytes());
            digest.update([u8::from(operation.vectors.is_some())]);
            if let Some(vectors) = &operation.vectors {
                digest.update((vectors.len() as u64).to_be_bytes());
                for vector in vectors.iter() {
                    digest.update((vector.len() as u64).to_be_bytes());
                    for part in vector.chunks(1024) {
                        control.cancellation().check()?;
                        for value in part {
                            digest.update(value.to_bits().to_be_bytes());
                        }
                    }
                }
            }
            owned.push(operation.clone())?;
        }
        self.seal_ivf_effects(digest.finalize().into(), IVFEffects { operations: owned });
        Ok(self)
    }
}
