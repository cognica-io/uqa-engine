//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Participant lifetime and orphan recovery never infer an uncertain durable outcome from process death.

use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use uqa_core::memory::{BudgetedVec, MemoryBudget, MemoryReservation};

use super::{
    DatabaseId, ParticipantOwner, SerializableGraph, SerializableTransactionId, VersionError,
    VersionResult,
};
use crate::{
    mvcc::{CommitStatus, StorageTransactionId},
    read_control::StorageReadControl,
};

struct Participant {
    id: SerializableTransactionId,
    _retained: Box<dyn Send + Sync>,
    _memory: MemoryReservation,
}

/// All clones retain the original participant's liveness through nested execution and pinned data views. The provider exposes this handle only after admission is retained. Destruction releases its lease without acquiring SSI admission or physical persistence.
#[derive(Clone)]
pub struct SerializableParticipant(Arc<Participant>);

impl SerializableParticipant {
    pub fn retain(
        id: SerializableTransactionId,
        lease: Box<dyn Send + Sync>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let memory = control
            .memory()
            .reserve(std::mem::size_of::<Participant>() + 2 * std::mem::size_of::<usize>())?;
        Ok(Self(Arc::new(Participant {
            id,
            _retained: lease,
            _memory: memory,
        })))
    }

    pub fn id(&self) -> SerializableTransactionId {
        self.0.id
    }
}

struct LocalEntry {
    id: SerializableTransactionId,
    lease: Weak<Participant>,
}

/// Liveness for an exclusively owned database or one browser/in-memory owner. File-backed multiprocess providers must use native leases instead. The entries and their capacity share the owner's allowance; live handles retain the registry through their lease payload.
pub struct LocalSerializableLeases(Mutex<BudgetedVec<LocalEntry>>);

impl LocalSerializableLeases {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self(Mutex::new(BudgetedVec::new(memory)))
    }

    pub fn retain(
        self: &Arc<Self>,
        id: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableParticipant> {
        control.check()?;
        let mut entries = self.0.lock();
        prune(&mut entries);
        if entries.is_empty() {
            *entries = BudgetedVec::new(control.memory());
        }
        let position = entries
            .binary_search_by_key(&key(id), |entry| key(entry.id))
            .err()
            .ok_or(VersionError::InvalidEncoding(
                "duplicate serializable participant lease",
            ))?;
        entries.reserve(1)?;
        let participant = SerializableParticipant::retain(id, Box::new(Arc::clone(self)), control)?;
        entries.push(LocalEntry {
            id,
            lease: Arc::downgrade(&participant.0),
        })?;
        entries[position..].rotate_right(1);
        Ok(participant)
    }

    pub fn is_alive(&self, id: SerializableTransactionId) -> bool {
        let entries = self.0.lock();
        entries
            .binary_search_by_key(&key(id), |entry| key(entry.id))
            .is_ok_and(|position| entries[position].lease.strong_count() != 0)
    }

    pub fn reclaim(&self) {
        prune(&mut self.0.lock());
    }
}

fn prune(entries: &mut BudgetedVec<LocalEntry>) {
    let mut kept = 0;
    for index in 0..entries.len() {
        if entries[index].lease.strong_count() != 0 {
            entries.swap(kept, index);
            kept += 1;
        }
    }
    entries.truncate(kept);
    if entries.is_empty() {
        *entries = BudgetedVec::new(entries.budget());
    }
}

fn key(id: SerializableTransactionId) -> ([u8; 16], [u8; 16], u64) {
    (id.database().as_bytes(), id.coordinator(), id.allocation())
}

impl SerializableGraph {
    pub fn database(&self) -> DatabaseId {
        self.database
    }
    pub fn coordinator(&self) -> [u8; 16] {
        self.coordinator
    }

    /// Bind admission to a retained provider lease before publishing the actor. Failed lease allocation aborts only the new actor and still consumes its allocation when the owner retains this state. Legacy manually owned actors have no lease binding and are never inferred dead by recovery.
    pub fn admit_with_lease(
        &mut self,
        read_only: bool,
        control: &StorageReadControl,
        retain: impl FnOnce(SerializableTransactionId) -> VersionResult<SerializableParticipant>,
    ) -> VersionResult<SerializableParticipant> {
        let id = self.admit(read_only, control)?;
        let retained = retain(id).and_then(|participant| {
            if participant.id() != id {
                return Err(VersionError::InvalidEncoding(
                    "serializable lease identity mismatch",
                ));
            }
            Ok(participant)
        });
        match retained {
            Ok(participant) => {
                let position = self.position(id)?;
                self.transactions[position].owner = ParticipantOwner::Leased;
                Ok(participant)
            }
            Err(error) => {
                self.rollback(id)?;
                Err(error)
            }
        }
    }

    /// Retire only participants whose liveness owner is authoritatively gone. An unbound participant can abort immediately. A prepared physical publication must be resolved or aborted through its exact durable receipt identity; a missing receipt stops recovery and never proves rollback. Earlier confirmed resolutions survive a later error. The caller retains the resulting state before releasing shared admission, including on failure.
    pub fn recover_abandoned(
        &mut self,
        control: &StorageReadControl,
        mut is_alive: impl FnMut(SerializableTransactionId) -> VersionResult<bool>,
        mut finish: impl FnMut(StorageTransactionId) -> VersionResult<CommitStatus>,
    ) -> VersionResult<()> {
        for index in 0..self.transactions.len() {
            control.check()?;
            let entry = self.transactions[index];
            if !entry.live() || entry.owner != ParticipantOwner::Leased {
                continue;
            }
            let participant =
                SerializableTransactionId::new(self.database, self.coordinator, entry.id)?;
            if is_alive(participant)? {
                continue;
            }
            let Some(publication) = self.publication(participant)? else {
                self.rollback(participant)?;
                continue;
            };
            let status = match finish(publication.transaction()) {
                Ok(status) => status,
                Err(VersionError::AlreadyCommitted(receipt)) => CommitStatus::Committed(receipt),
                Err(VersionError::AlreadyAborted(transaction))
                    if transaction == publication.transaction() =>
                {
                    CommitStatus::Aborted
                }
                Err(error) => return Err(error),
            };
            match self.resolve_publication(publication, status)? {
                CommitStatus::Unknown => return Err(VersionError::UnknownTransaction),
                CommitStatus::Pending | CommitStatus::Committed(_) | CommitStatus::Aborted => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
