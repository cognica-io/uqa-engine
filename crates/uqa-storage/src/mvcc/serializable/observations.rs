//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded logical observations, atomic dependency registration and savepoint write undo.

#[cfg(test)]
mod tests;

use uqa_core::memory::{BudgetedVec, MemoryBudget};

use super::{
    conflicts::DependencyAction, predicates::OwnedPredicate, SerializableGraph,
    SerializablePredicate, SerializableTransactionId, Transaction, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;

/// A transaction-owned position in its successful write observations. Undo removes subsequent write intents while retaining every read observation and previously established dependency.
#[derive(Debug, Clone, Copy)]
pub struct SerializableWriteMark {
    transaction: SerializableTransactionId,
    writes: u64,
}

pub(super) struct Observation {
    pub(super) owner: u64,
    pub(super) write: u64,
    pub(super) predicate: OwnedPredicate,
}

pub(super) struct Observations {
    pub(super) reads: BudgetedVec<Observation>,
    pub(super) writes: BudgetedVec<Observation>,
}

impl Observations {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            reads: BudgetedVec::new(memory),
            writes: BudgetedVec::new(memory),
        }
    }

    pub(super) fn clear(&mut self) -> bool {
        let changed = !self.reads.is_empty() || !self.writes.is_empty();
        self.reads = BudgetedVec::new(self.reads.budget());
        self.writes = BudgetedVec::new(self.writes.budget());
        changed
    }

    pub(super) fn forget(&mut self, owner: u64) -> bool {
        let reads = retain(&mut self.reads, |entry| entry.owner != owner);
        let writes = retain(&mut self.writes, |entry| entry.owner != owner);
        reads || writes
    }

    pub(super) fn reclaim(&mut self, transactions: &[Transaction], oldest: u64) -> bool {
        let retained = |entry: &Observation| {
            transactions
                .binary_search_by_key(&entry.owner, |transaction| transaction.id)
                .is_ok_and(|position| transactions[position].retains_history(oldest))
        };
        let reads = retain(&mut self.reads, retained);
        let writes = retain(&mut self.writes, retained);
        reads || writes
    }

    fn selected(&self, writing: bool) -> &BudgetedVec<Observation> {
        if writing {
            &self.writes
        } else {
            &self.reads
        }
    }

    fn selected_mut(&mut self, writing: bool) -> &mut BudgetedVec<Observation> {
        if writing {
            &mut self.writes
        } else {
            &mut self.reads
        }
    }
}

impl SerializableGraph {
    /// Register before a logical read can race with a write. This includes empty point/range results, index-only and cached reads. Check existing write intents as well as committed writers invisible to the reader's fixed snapshot; later writers inspect the retained read. The caller coordinates this boundary with snapshot capture and publication.
    pub fn observe_read(
        &mut self,
        transaction: SerializableTransactionId,
        predicate: SerializablePredicate<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.observe_predicate(transaction, predicate, false, control)
    }

    /// Register before staging a logical write. Supply the changed row and every affected old/new index key separately, or the entire object for an object-wide change. Physical posting clusters and counters are not logical keys. This checks existing readers and retains an intent for later reads. A failed observation publishes neither its intent nor any dependencies or peer victims.
    pub fn observe_write(
        &mut self,
        transaction: SerializableTransactionId,
        predicate: SerializablePredicate<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.observe_predicate(transaction, predicate, true, control)
    }

    pub fn write_mark(
        &self,
        transaction: SerializableTransactionId,
    ) -> VersionResult<SerializableWriteMark> {
        self.check_active(transaction)?;
        Ok(SerializableWriteMark {
            transaction,
            writes: self.transactions[self.position(transaction)?].writes,
        })
    }

    /// Cancel successful write intents after a statement/savepoint mark. This allocation-free cleanup remains usable for cancelled or peer-doomed transactions, but cannot erase their reads or serialization failure. Prepared or finished transactions cannot undo their publication inputs.
    pub fn rollback_writes(&mut self, mark: SerializableWriteMark) -> VersionResult<()> {
        let entry = self.transactions[self.position(mark.transaction)?];
        if !entry.live() {
            return Err(VersionError::TransactionFinished);
        }
        if entry.prepared.is_some() {
            return Err(VersionError::TransactionSealed);
        }
        if mark.writes > entry.writes {
            return Err(VersionError::InvalidEncoding(
                "invalid serializable write checkpoint",
            ));
        }
        self.checkpoint_changed |= retain(&mut self.predicates.writes, |observed| {
            observed.owner != entry.id || observed.write <= mark.writes
        });
        Ok(())
    }

    fn observe_predicate(
        &mut self,
        transaction: SerializableTransactionId,
        predicate: SerializablePredicate<'_>,
        writing: bool,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        self.check_active(transaction)?;
        predicate.validate(writing)?;
        let position = self.position(transaction)?;
        let entry = self.transactions[position];
        if writing && entry.read_only {
            return Err(VersionError::InvalidEncoding(
                "read-only serializable transaction cannot observe writes",
            ));
        }
        if predicate.is_empty() {
            return Ok(());
        }
        for observed in object_entries(self.predicates.selected(writing), predicate.object) {
            control.check()?;
            if observed.owner == entry.id && observed.predicate.borrowed() == predicate {
                return Ok(());
            }
        }
        let write = if writing {
            entry
                .writes
                .checked_add(1)
                .ok_or(VersionError::SequenceExhausted)?
        } else {
            0
        };
        let owned = OwnedPredicate::new(predicate, self.transactions.budget())?;
        let mut actions = BudgetedVec::<DependencyAction>::new(self.transactions.budget());
        for observed in object_entries(self.predicates.selected(!writing), predicate.object) {
            control.check()?;
            if observed.owner == entry.id {
                continue;
            }
            let other = observed.predicate.borrowed();
            let matches = if writing {
                other.overlaps_write(predicate)
            } else {
                predicate.overlaps_write(other)
            };
            if !matches {
                continue;
            }
            let peer =
                SerializableTransactionId::new(self.database, self.coordinator, observed.owner)?;
            let (reader, writer) = if writing {
                (peer, transaction)
            } else {
                (transaction, peer)
            };
            if let Some(action) = self.plan_dependency(transaction, reader, writer, control)? {
                if let Err(index) =
                    actions.binary_search_by_key(&action.edge, |planned| planned.edge)
                {
                    actions.push(action)?;
                    actions[index..].rotate_right(1);
                }
            }
        }
        // Every action has the same reader or writer; new edges cannot change another action's incoming/outgoing checks. Validate and reserve the complete observation before publishing any action.
        self.predicates.selected_mut(writing).reserve(1)?;
        self.reserve_dependencies(
            actions
                .iter()
                .filter(|action| action.victim.is_none())
                .count(),
        )?;
        control.check()?;
        let observations = self.predicates.selected_mut(writing);
        let index =
            observations.partition_point(|observed| observed.predicate.object <= predicate.object);
        observations.push(Observation {
            owner: entry.id,
            write,
            predicate: owned,
        })?;
        self.checkpoint_changed = true;
        observations[index..].rotate_right(1);
        for &action in &*actions {
            self.publish_dependency(action)?;
        }
        if writing {
            self.transactions[position].writes = write;
        }
        Ok(())
    }
}

fn object_entries(observations: &[Observation], object: [u8; 16]) -> &[Observation] {
    let start = observations.partition_point(|entry| entry.predicate.object < object);
    let end = observations.partition_point(|entry| entry.predicate.object <= object);
    &observations[start..end]
}

fn retain(
    values: &mut BudgetedVec<Observation>,
    mut predicate: impl FnMut(&Observation) -> bool,
) -> bool {
    let previous = values.len();
    let mut kept = 0;
    for index in 0..values.len() {
        if predicate(&values[index]) {
            values.swap(kept, index);
            kept += 1;
        }
    }
    values.truncate(kept);
    if values.is_empty() {
        *values = BudgetedVec::new(values.budget());
    }
    kept != previous
}
