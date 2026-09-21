//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit binding between a sealed SSI participant and an independently allocated durable commit.

#[cfg(test)]
mod tests;

use super::{SerializableGraph, SerializableTransactionId, VersionError, VersionResult};
use crate::{
    mvcc::{CommitFingerprint, CommitReceipt, CommitSequence, CommitStatus, StorageTransactionId},
    read_control::StorageReadControl,
};

/// A sealed association with one physical transaction and its logical mutation fingerprint. Providers must retain the prepared SSI state and this association under shared admission before attempting physical publication. Neither possession of this token nor loss of its process proves a durable outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerializablePublication {
    participant: SerializableTransactionId,
    transaction: StorageTransactionId,
    fingerprint: CommitFingerprint,
}

#[derive(Clone, Copy)]
pub(super) struct PreparedPublication {
    pub(super) allocation: u64,
    pub(super) fingerprint: CommitFingerprint,
    pub(super) outcome: PublicationOutcome,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PublicationOutcome {
    Prepared,
    Committed(CommitSequence),
    Aborted,
}

impl PreparedPublication {
    pub(super) fn committed(self) -> bool {
        matches!(self.outcome, PublicationOutcome::Committed(_))
    }

    pub(super) fn aborted(self) -> bool {
        self.outcome == PublicationOutcome::Aborted
    }

    fn matches(self, publication: SerializablePublication) -> bool {
        self.allocation == publication.transaction.allocation()
            && self.fingerprint == publication.fingerprint
    }
}

impl SerializablePublication {
    pub const fn participant(self) -> SerializableTransactionId {
        self.participant
    }

    pub const fn transaction(self) -> StorageTransactionId {
        self.transaction
    }

    pub const fn fingerprint(self) -> CommitFingerprint {
        self.fingerprint
    }
}

impl SerializableGraph {
    /// Seal the participant against its caller's already allocated durable transaction. Repeated preparation must preserve both identities and the original fingerprint, including during physical candidate re-preparation. A logically read-only participant can publish maintenance records outside logical predicate spaces; logical write observations remain forbidden. Participants without physical changes finish directly through the graph without allocating a durable transaction.
    pub fn prepare_publication(
        &mut self,
        participant: SerializableTransactionId,
        transaction: StorageTransactionId,
        fingerprint: CommitFingerprint,
        control: &StorageReadControl,
    ) -> VersionResult<SerializablePublication> {
        control.check()?;
        let position = self.position(participant)?;
        if transaction.database() != self.database {
            return Err(VersionError::WrongDatabase);
        }
        let publication = SerializablePublication {
            participant,
            transaction,
            fingerprint,
        };
        if let Some(previous) = self.transactions[position].publication {
            if !previous.matches(publication) {
                return Err(VersionError::CommitMismatch);
            }
        } else {
            self.check_active(participant)?;
            for entry in &*self.transactions {
                control.check()?;
                if entry
                    .publication
                    .is_some_and(|previous| previous.allocation == transaction.allocation())
                {
                    return Err(VersionError::CommitMismatch);
                }
            }
        }
        self.prepare_commit(participant, control)?;
        self.checkpoint_changed |= self.transactions[position].publication.is_none();
        self.transactions[position].publication = Some(PreparedPublication {
            allocation: transaction.allocation(),
            fingerprint,
            outcome: PublicationOutcome::Prepared,
        });
        Ok(publication)
    }

    /// Retrieve the physical receipt identity needed for recovery of a retained prepared participant. The provider queries this exact durable transaction; the SSI participant allocation is never a receipt key.
    pub fn publication(
        &self,
        participant: SerializableTransactionId,
    ) -> VersionResult<Option<SerializablePublication>> {
        self.transactions[self.position(participant)?]
            .publication
            .map(|publication| {
                Ok(SerializablePublication {
                    participant,
                    transaction: StorageTransactionId::new(self.database, publication.allocation)?,
                    fingerprint: publication.fingerprint,
                })
            })
            .transpose()
    }

    /// Reconcile unresolved physical publications under shared admission before capturing another data snapshot. A lost commit reply may leave committed data behind a prepared SSI participant; admitting a snapshot first would misclassify that visible writer. Confirmed pending allocations may remain prepared, but a missing authoritative receipt prevents admission and never proves abort. The provider retains confirmed resolutions even if a later lookup fails.
    pub fn reconcile_publications(
        &mut self,
        control: &StorageReadControl,
        mut lookup: impl FnMut(StorageTransactionId) -> VersionResult<CommitStatus>,
    ) -> VersionResult<()> {
        for index in 0..self.transactions.len() {
            control.check()?;
            let entry = self.transactions[index];
            let Some(prepared) = entry.publication else {
                continue;
            };
            if prepared.outcome != PublicationOutcome::Prepared {
                continue;
            }
            let participant =
                SerializableTransactionId::new(self.database, self.coordinator, entry.id)?;
            let transaction = StorageTransactionId::new(self.database, prepared.allocation)?;
            let publication = SerializablePublication {
                participant,
                transaction,
                fingerprint: prepared.fingerprint,
            };
            let status = lookup(transaction)?;
            if self.resolve_publication(publication, status)? == CommitStatus::Unknown {
                return Err(VersionError::UnknownTransaction);
            }
        }
        Ok(())
    }

    /// Apply an authoritative status obtained for this token's physical transaction. Pending/unknown outcomes keep an unresolved participant prepared; a committed receipt must match the transaction and fingerprint. Known terminal outcomes cannot be downgraded by later pending/unknown status. Terminal completion neither allocates nor observes statement cancellation. The provider must retain the resolved state before releasing shared publication admission.
    pub fn resolve_publication(
        &mut self,
        publication: SerializablePublication,
        status: CommitStatus,
    ) -> VersionResult<CommitStatus> {
        let position = self.position(publication.participant)?;
        let Some(prepared) = self.transactions[position].publication else {
            return Err(VersionError::CommitMismatch);
        };
        if !prepared.matches(publication) {
            return Err(VersionError::CommitMismatch);
        }
        match prepared.outcome {
            PublicationOutcome::Committed(sequence) => {
                let receipt = CommitReceipt {
                    transaction: publication.transaction,
                    sequence,
                    fingerprint: publication.fingerprint,
                };
                return match status {
                    CommitStatus::Aborted => Err(VersionError::AlreadyCommitted(receipt)),
                    CommitStatus::Committed(actual) if actual != receipt => {
                        Err(VersionError::AlreadyCommitted(receipt))
                    }
                    _ => Ok(CommitStatus::Committed(receipt)),
                };
            }
            PublicationOutcome::Aborted => {
                return if matches!(status, CommitStatus::Committed(_)) {
                    Err(VersionError::AlreadyAborted(publication.transaction))
                } else {
                    Ok(CommitStatus::Aborted)
                };
            }
            PublicationOutcome::Prepared => {}
        }
        match status {
            CommitStatus::Committed(receipt) => {
                if receipt.transaction != publication.transaction
                    || receipt.fingerprint != publication.fingerprint
                {
                    return Err(VersionError::CommitMismatch);
                }
                self.checkpoint_changed = true;
                self.transactions[position]
                    .publication
                    .as_mut()
                    .expect("validated publication")
                    .outcome = PublicationOutcome::Committed(receipt.sequence);
                self.commit(publication.participant)?;
            }
            CommitStatus::Aborted => {
                self.checkpoint_changed = true;
                self.transactions[position]
                    .publication
                    .as_mut()
                    .expect("validated publication")
                    .outcome = PublicationOutcome::Aborted;
                self.rollback(publication.participant)?;
            }
            CommitStatus::Pending | CommitStatus::Unknown => {}
        }
        Ok(status)
    }
}
