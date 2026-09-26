//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A prepared absence observation cannot cross physical tombstone retirement.

use sha2::{Digest, Sha256};
use uqa_core::CancellationToken;

use crate::mvcc::{CommittedRecordSnapshot, RecordWrite, VersionError, VersionResult};
use crate::read_control::StorageReadControl;

use super::PreparedRecordCommit;

impl PreparedRecordCommit {
    /// Preserve the reclamation epoch of the actual snapshot used to evaluate these replacements. The snapshot must belong to the same provider/history as the transaction. A raw `new` batch has no such evidence and cannot assert absence in a prefix whose tombstones have been retired.
    pub fn new_at_snapshot(
        writes: &[RecordWrite<'_>],
        snapshot: &dyn CommittedRecordSnapshot,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        Ok(Self::new(writes, control)?.with_reclamation_epoch(snapshot.reclamation_epoch()))
    }

    pub(in crate::mvcc) fn with_reclamation_epoch(mut self, epoch: Option<u64>) -> Self {
        if let Some(epoch) = epoch {
            let mut digest = Sha256::new();
            digest.update(b"UQA prepared reclamation epoch 1");
            digest.update(self.fingerprint);
            digest.update(epoch.to_be_bytes());
            self.fingerprint = digest.finalize().into();
        }
        self.reclamation_epoch = epoch;
        self
    }

    /// Under the physical commit boundary, after resolving an original receipt, reject observations preceding this prefix's last tombstone retirement. The provider publishes the floor atomically with deletion and captures epochs under snapshot admission. Other prefixes retain their ordinary revision contract.
    pub fn validate_reclamation_epoch(
        &self,
        prefix: &[u8],
        minimum: u64,
        current: u64,
        cancellation: &CancellationToken,
    ) -> VersionResult<()> {
        cancellation.check()?;
        if prefix.is_empty() || minimum == 0 || minimum > current {
            return Err(VersionError::InvalidEncoding(
                "invalid tombstone reclamation domain",
            ));
        }
        if self
            .reclamation_epoch
            .is_some_and(|epoch| epoch >= minimum && epoch <= current)
        {
            return Ok(());
        }
        let absent = self
            .records()
            .iter()
            .filter(|write| write.expected().is_none())
            .map(super::PreparedRecordWrite::key)
            .chain(self.requirements.iter().flat_map(|requirements| {
                requirements
                    .iter()
                    .filter(|requirement| requirement.expected.is_none())
                    .map(|requirement| requirement.key.bytes())
            }));
        for key in absent {
            cancellation.check()?;
            if key.starts_with(prefix) {
                return Err(VersionError::ReclaimedObservation {
                    observed: self.reclamation_epoch,
                    minimum,
                    current,
                });
            }
        }
        Ok(())
    }
}
