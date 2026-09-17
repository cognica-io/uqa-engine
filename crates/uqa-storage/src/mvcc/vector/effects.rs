//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained immutable document inputs and their sealed transaction fingerprint.

use super::{IndexKind, Mutation};
use crate::mvcc::{key::RecordKey, CommitSequence, PreparedRecordCommit, VersionResult};
use crate::read_control::StorageReadControl;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryError},
    DocId,
};

#[derive(Clone)]
pub(in crate::mvcc) struct OwnedVectorMutation {
    pub(in crate::mvcc) kind: IndexKind,
    pub(in crate::mvcc) metadata: RecordKey,
    pub(in crate::mvcc) document: DocId,
    vectors: Option<Arc<Budgeted<Vec<Vec<f32>>>>>,
}

impl OwnedVectorMutation {
    pub(in crate::mvcc) fn retain(
        kind: IndexKind,
        metadata: &[u8],
        mutation: Mutation<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        control.cancellation().check()?;
        let (document, vectors) = match mutation {
            Mutation::Replace { document, vectors } => {
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
            Mutation::Delete(document) => (document, None),
        };
        Ok(Self {
            kind,
            metadata: RecordKey::new(metadata, control.memory())?,
            document,
            vectors,
        })
    }
    pub(in crate::mvcc) fn borrowed(&self) -> Mutation<'_> {
        self.vectors
            .as_ref()
            .map_or(Mutation::Delete(self.document), |vectors| {
                Mutation::Replace {
                    document: self.document,
                    vectors,
                }
            })
    }
}

pub(in crate::mvcc) struct VectorEffects {
    pub(in crate::mvcc) operations: BudgetedVec<OwnedVectorMutation>,
}

impl PreparedRecordCommit {
    pub(in crate::mvcc) fn with_vector_effects(
        mut self,
        base: CommitSequence,
        operations: &[OwnedVectorMutation],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        if operations.is_empty() {
            return Ok(self);
        }
        let mut owned = BudgetedVec::new(control.memory());
        owned.reserve(operations.len())?;
        let mut digest = Sha256::new();
        digest.update(b"UQA prepared vector effects 1");
        digest.update(self.fingerprint());
        digest.update(base.as_u64().to_be_bytes());
        digest.update((operations.len() as u64).to_be_bytes());
        for operation in operations {
            control.cancellation().check()?;
            digest.update([operation.kind.fingerprint_tag()]);
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
        self.seal_vector_effects(
            digest.finalize().into(),
            VectorEffects { operations: owned },
        );
        Ok(self)
    }
}
