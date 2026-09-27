//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain complete build-origin evidence with evaluated generation lifecycle effects and the original commit fingerprint.

use crate::diskann_index::pages::DiskANNOriginReader;
use crate::mvcc::{
    key::RecordKey, CommitSequence, PreparedRecordCommit, SharedRecordValue, VersionResult,
};
use crate::read_control::StorageReadControl;
use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;

#[derive(Clone)]
pub(in crate::mvcc) enum OwnedPopulationMutation {
    Publish {
        key: RecordKey,
        template: SharedRecordValue,
        origins: DiskANNOriginReader,
    },
    Retire {
        key: RecordKey,
    },
}

impl OwnedPopulationMutation {
    pub(in crate::mvcc) fn key(&self) -> &RecordKey {
        match self {
            Self::Publish { key, .. } | Self::Retire { key } => key,
        }
    }
}

pub(in crate::mvcc) struct PopulationEffects {
    pub(in crate::mvcc) operations: BudgetedVec<OwnedPopulationMutation>,
}

impl PreparedRecordCommit {
    pub(in crate::mvcc) fn with_population_effects(
        mut self,
        base: CommitSequence,
        operations: &[OwnedPopulationMutation],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        if operations.is_empty() {
            return Ok(self);
        }
        let mut owned = BudgetedVec::new(control.memory());
        owned.reserve(operations.len())?;
        let mut digest = Sha256::new();
        digest.update(b"UQA prepared DiskANN population effects 1");
        digest.update(self.fingerprint());
        digest.update(base.as_u64().to_be_bytes());
        digest.update((operations.len() as u64).to_be_bytes());
        for operation in operations {
            control.check()?;
            hash(&mut digest, operation.key().bytes(), control)?;
            match operation {
                OwnedPopulationMutation::Publish {
                    template, origins, ..
                } => {
                    digest.update([1]);
                    hash(&mut digest, template, control)?;
                    let input = origins.manifest().input();
                    let generation = input.generation;
                    digest.update(generation.database());
                    digest.update(generation.table().to_be_bytes());
                    digest.update(generation.index().to_be_bytes());
                    digest.update(generation.generation().to_be_bytes());
                    let summary = origins
                        .manifest()
                        .origins()
                        .expect("validated complete origins");
                    digest.update(summary.documents().to_be_bytes());
                    digest.update(summary.digest());
                    digest.update(input.dimensions.to_be_bytes());
                    digest.update(input.coverage.vector_count().to_be_bytes());
                }
                OwnedPopulationMutation::Retire { .. } => digest.update([0]),
            }
            owned.push(operation.clone())?;
        }
        self.seal_population_effects(
            digest.finalize().into(),
            PopulationEffects { operations: owned },
        );
        Ok(self)
    }
}

fn hash(digest: &mut Sha256, bytes: &[u8], control: &StorageReadControl) -> VersionResult<()> {
    digest.update((bytes.len() as u64).to_be_bytes());
    for chunk in bytes.chunks(4096) {
        control.check()?;
        digest.update(chunk);
    }
    Ok(())
}
