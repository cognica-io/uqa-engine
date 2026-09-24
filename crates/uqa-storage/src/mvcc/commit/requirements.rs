//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Commit preconditions share definition revisions without manufacturing writes to those definitions.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use uqa_core::memory::BudgetedVec;
use uqa_core::CancellationToken;

use crate::mvcc::key::RecordKey;
use crate::mvcc::{CommitSequence, VersionError, VersionResult};
use crate::read_control::StorageReadControl;

use super::{hash_bytes, PreparedRecordCommit};

#[derive(Clone)]
pub(in crate::mvcc) struct RecordRequirement {
    pub(in crate::mvcc) key: RecordKey,
    pub(in crate::mvcc) expected: Option<CommitSequence>,
}

impl PreparedRecordCommit {
    pub(in crate::mvcc) fn with_requirements(
        mut self,
        requirements: &[RecordRequirement],
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        if requirements.is_empty() {
            return Ok(self);
        }
        let mut owned = BudgetedVec::new(control.memory());
        owned.reserve(requirements.len())?;
        let mut digest = Sha256::new();
        digest.update(b"UQA prepared record requirements 1");
        digest.update(self.fingerprint);
        digest.update((requirements.len() as u64).to_be_bytes());
        for requirement in requirements {
            control.cancellation().check()?;
            digest.update((requirement.key.bytes().len() as u64).to_be_bytes());
            hash_bytes(&mut digest, requirement.key.bytes(), control)?;
            digest.update([u8::from(requirement.expected.is_some())]);
            digest.update(
                requirement
                    .expected
                    .map_or(0, CommitSequence::as_u64)
                    .to_be_bytes(),
            );
            owned.push(requirement.clone())?;
        }
        self.fingerprint = digest.finalize().into();
        self.requirements = Some(Arc::new(owned));
        Ok(self)
    }

    pub(crate) fn has_requirements(&self) -> bool {
        self.requirements.is_some()
    }

    /// Keys validated without being written. Providers must charge any temporary driver bindings for these keys as well as replacement records.
    pub fn required_keys(&self) -> impl Iterator<Item = &[u8]> {
        self.requirements.iter().flat_map(|requirements| {
            requirements
                .iter()
                .map(|requirement| requirement.key.bytes())
        })
    }

    pub(crate) fn validate_requirements(
        &self,
        cancellation: &CancellationToken,
        mut head_revision: impl FnMut(&[u8]) -> VersionResult<Option<CommitSequence>>,
    ) -> VersionResult<()> {
        cancellation.check()?;
        if let Some(requirements) = &self.requirements {
            for (index, requirement) in requirements.iter().enumerate() {
                cancellation.check()?;
                let actual = head_revision(requirement.key.bytes())?;
                if requirement.expected != actual {
                    return Err(VersionError::ReadConflict {
                        dependency: index,
                        expected: requirement.expected,
                        actual,
                    });
                }
            }
        }
        Ok(())
    }
}
