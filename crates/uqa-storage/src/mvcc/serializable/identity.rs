//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serializable participation is independent of physical publication and its durable receipts.

use super::{DatabaseId, VersionError, VersionResult};

/// One logical participant in a database and coordinator incarnation. Read-only participants require no physical transaction allocation. This identity must never be passed to a provider's commit-status/abort API; a publishing participant retains its separately allocated `StorageTransactionId` when preparing a durable commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SerializableTransactionId {
    database: DatabaseId,
    coordinator: [u8; 16],
    allocation: u64,
}

impl SerializableTransactionId {
    /// Decode a retained participant reference. Admission allocates these identities through `SerializableGraph::admit`; constructing a reference does not admit a participant.
    pub fn new(
        database: DatabaseId,
        coordinator: [u8; 16],
        allocation: u64,
    ) -> VersionResult<Self> {
        validate_coordinator(coordinator)?;
        if allocation == 0 {
            return Err(VersionError::InvalidTransactionId);
        }
        Ok(Self {
            database,
            coordinator,
            allocation,
        })
    }

    pub const fn database(self) -> DatabaseId {
        self.database
    }

    pub const fn coordinator(self) -> [u8; 16] {
        self.coordinator
    }

    pub const fn allocation(self) -> u64 {
        self.allocation
    }
}

pub(super) fn validate_coordinator(coordinator: [u8; 16]) -> VersionResult<()> {
    if coordinator == [0; 16] {
        return Err(VersionError::InvalidEncoding(
            "serializable coordinator identity must be nonzero",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mvcc::SerializableGraph, read_control::StorageReadControl};

    const DATABASE: DatabaseId = DatabaseId::from_bytes([17; 16]);

    #[test]
    fn read_only_participants_allocate_without_physical_transactions_and_never_reuse_ids() {
        let control = StorageReadControl::with_limit(64 * 1024);
        let mut graph = SerializableGraph::new(DATABASE, [1; 16], control.memory()).unwrap();
        let first = graph.admit(true, &control).unwrap();
        assert_eq!(first.allocation(), 1);
        graph.prepare_commit(first, &control).unwrap();
        graph.commit(first).unwrap();
        graph.reclaim();
        assert_eq!(control.memory().used(), 0);
        let second = graph.admit(true, &control).unwrap();
        assert_eq!(second.allocation(), 2);
        assert!(matches!(
            graph.check_active(first),
            Err(VersionError::UnknownTransaction)
        ));
        graph.check_active(second).unwrap();
    }

    #[test]
    fn a_new_coordinator_rejects_old_participants_even_when_allocations_match() {
        let control = StorageReadControl::with_limit(64 * 1024);
        let mut previous = SerializableGraph::new(DATABASE, [1; 16], control.memory()).unwrap();
        let old = previous.admit(false, &control).unwrap();
        let old_mark = previous.write_mark(old).unwrap();
        let mut replacement = SerializableGraph::new(DATABASE, [2; 16], control.memory()).unwrap();
        let current = replacement.admit(false, &control).unwrap();
        assert_eq!(old.allocation(), current.allocation());
        assert_ne!(old, current);
        assert!(matches!(
            replacement.check_active(old),
            Err(VersionError::WrongSerializableCoordinator)
        ));
        assert!(matches!(
            replacement.rollback(old),
            Err(VersionError::WrongSerializableCoordinator)
        ));
        assert!(matches!(
            replacement.rollback_writes(old_mark),
            Err(VersionError::WrongSerializableCoordinator)
        ));
        assert!(matches!(
            replacement.observe_rw(current, current, old, &control),
            Err(VersionError::WrongSerializableCoordinator)
        ));
        replacement.check_active(current).unwrap();
    }

    #[test]
    fn invalid_identity_or_failed_admission_cannot_consume_a_participant_number() {
        let control = StorageReadControl::with_limit(64 * 1024);
        assert!(matches!(
            SerializableGraph::new(DATABASE, [0; 16], control.memory()),
            Err(VersionError::InvalidEncoding(_))
        ));
        assert!(matches!(
            SerializableTransactionId::new(DATABASE, [0; 16], 1),
            Err(VersionError::InvalidEncoding(_))
        ));
        assert!(matches!(
            SerializableTransactionId::new(DATABASE, [1; 16], 0),
            Err(VersionError::InvalidTransactionId)
        ));
        let mut graph = SerializableGraph::new(DATABASE, [1; 16], control.memory()).unwrap();
        let occupied = control.memory().reserve(control.memory().limit()).unwrap();
        assert!(matches!(
            graph.admit(true, &control),
            Err(VersionError::Memory(_))
        ));
        drop(occupied);
        assert_eq!(graph.admit(true, &control).unwrap().allocation(), 1);
    }

    #[test]
    fn exhausted_participant_numbers_do_not_prevent_prepared_completion() {
        let control = StorageReadControl::with_limit(64 * 1024);
        let mut graph = SerializableGraph::new(DATABASE, [1; 16], control.memory()).unwrap();
        graph.last_allocation = u64::MAX - 1;
        let last = graph.admit(false, &control).unwrap();
        graph.prepare_commit(last, &control).unwrap();
        assert!(matches!(
            graph.admit(false, &control),
            Err(VersionError::SequenceExhausted)
        ));
        graph.commit(last).unwrap();
        graph.reclaim();
        assert!(matches!(
            graph.admit(false, &control),
            Err(VersionError::SequenceExhausted)
        ));
        assert_eq!(control.memory().used(), 0);
    }
}
