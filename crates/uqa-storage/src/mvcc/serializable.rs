//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serializable read/write dependencies, commit ordering and retained conflict summaries.

mod checkpoint;
mod conflicts;
mod identity;
mod observations;
mod predicates;
mod publication;
#[cfg(test)]
mod tests;

use uqa_core::memory::{BudgetedVec, MemoryBudget};

use super::{DatabaseId, VersionError, VersionResult};
use crate::read_control::StorageReadControl;

pub use identity::SerializableTransactionId;
pub use observations::SerializableWriteMark;
pub use predicates::{SerializableKeySpace, SerializablePredicate};
pub use publication::SerializablePublication;

/// Whether a read-only snapshot can execute without predicate observations. An unsafe snapshot must be discarded and captured again; waiting cannot repair it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeSnapshot {
    Pending,
    Safe,
    Unsafe,
}

#[derive(Clone, Copy)]
struct Transaction {
    id: u64,
    snapshot: u64,
    read_only: bool,
    prepared: Option<u64>,
    committed: Option<u64>,
    aborted: bool,
    doomed: bool,
    summarized_out: Option<u64>,
    writes: u64,
    publication: Option<publication::PreparedPublication>,
}

impl Transaction {
    fn live(self) -> bool {
        !self.aborted && self.committed.is_none()
    }

    fn relevant(self) -> bool {
        !self.aborted && !self.doomed
    }
}

/// Both orientations of each edge are sorted, so incident-edge traversal does not scan unrelated dependencies. IDs are non-reused transaction allocations, not vector positions.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Edge(u64, u64);

/// Common SSI algorithm for one database incarnation. Participants, both edge indexes, logical observations and owned predicate keys share a caller-selected retention budget. The owner serializes calls with snapshot admission, predicate registration and durable commit admission; this type does not acquire provider or SQL locks.
///
/// Only SERIALIZABLE transactions participate. Register a read/write edge when the reader cannot see the writer's logical change, including absent keys and predicate ranges. Physical index-sharing conflicts are not such edges. Check existing writer intents when registering reads and existing reads when registering writes; observing only committed changes loses cycles.
///
/// A prepared transaction has passed the final serialization check and cannot be selected as a victim. Keep it prepared across an uncertain physical outcome, and call `commit` or `rollback` only after authoritative completion. Savepoint undo must not discard the transaction's read dependencies.
pub struct SerializableGraph {
    database: DatabaseId,
    coordinator: [u8; 16],
    last_allocation: u64,
    clock: u64,
    pending_finishes: u64,
    transactions: BudgetedVec<Transaction>,
    outgoing: BudgetedVec<Edge>,
    incoming: BudgetedVec<Edge>,
    predicates: observations::Observations,
}

impl SerializableGraph {
    /// Create the graph for a nonzero provider-coordinated incarnation. A provider must retain this incarnation and its admission state for every overlapping participant, including across process handoff.
    pub fn new(
        database: DatabaseId,
        coordinator: [u8; 16],
        memory: &MemoryBudget,
    ) -> VersionResult<Self> {
        identity::validate_coordinator(coordinator)?;
        Ok(Self {
            database,
            coordinator,
            last_allocation: 0,
            clock: 0,
            pending_finishes: 0,
            transactions: BudgetedVec::new(memory),
            outgoing: BudgetedVec::new(memory),
            incoming: BudgetedVec::new(memory),
            predicates: observations::Observations::new(memory),
        })
    }

    /// Allocate and admit a logical participant at the same boundary as its fixed data snapshot, without allocating a physical transaction or writing user data. The owner coordinates and retains admission across every provider process. Allocations survive graph reclamation and are never reused within the coordinator incarnation.
    pub fn admit(
        &mut self,
        read_only: bool,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableTransactionId> {
        control.check()?;
        let allocation = self
            .last_allocation
            .checked_add(1)
            .ok_or(VersionError::SequenceExhausted)?;
        let transaction =
            SerializableTransactionId::new(self.database, self.coordinator, allocation)?;
        self.begin(transaction, read_only, control)?;
        Ok(transaction)
    }

    fn begin(
        &mut self,
        transaction: SerializableTransactionId,
        read_only: bool,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        self.validate_identity(transaction)?;
        let position = self
            .transactions
            .binary_search_by_key(&transaction.allocation(), |entry| entry.id)
            .err()
            .ok_or(VersionError::InvalidEncoding(
                "duplicate serializable transaction",
            ))?;
        self.reserve_order(false)?;
        self.transactions.push(Transaction {
            id: transaction.allocation(),
            snapshot: self.clock + 1,
            read_only,
            prepared: None,
            committed: None,
            aborted: false,
            doomed: false,
            summarized_out: None,
            writes: 0,
            publication: None,
        })?;
        self.transactions[position..].rotate_right(1);
        self.clock += 1;
        self.last_allocation = self.last_allocation.max(transaction.allocation());
        Ok(())
    }

    /// Fail a doomed transaction before another statement or commit. Prepared state is sealed, including while its durable outcome is unknown.
    pub fn check_active(&self, transaction: SerializableTransactionId) -> VersionResult<()> {
        let entry = self.transactions[self.position(transaction)?];
        if entry.doomed {
            return Err(VersionError::SerializationConflict { transaction });
        }
        if !entry.live() {
            return Err(VersionError::TransactionFinished);
        }
        if entry.prepared.is_some() {
            return Err(VersionError::TransactionSealed);
        }
        Ok(())
    }

    /// Register a dependency directed from reader to writer. The observing transaction must be one endpoint and still active. A peer may be marked doomed; its next active check or preparation then reports a serialization failure. A rejected local observation publishes no edge and does not doom its transaction: statement/savepoint recovery can undo that operation while retaining earlier reads. Nonoverlapping and duplicate edges are ignored.
    pub fn observe_rw(
        &mut self,
        observer: SerializableTransactionId,
        reader: SerializableTransactionId,
        writer: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let Some(action) = self.plan_dependency(observer, reader, writer, control)? else {
            return Ok(());
        };
        // Reserve both orientations before publishing either, so exhaustion cannot hide half an edge.
        self.reserve_dependencies(usize::from(action.victim.is_none()))?;
        self.publish_dependency(action)
    }

    /// Check incoming dangerous structures before durable publication, preferring an unprepared pivot as the victim. Preparation is idempotent and reserves ordering space for authoritative completion even if other transactions continue meanwhile.
    pub fn prepare_commit(
        &mut self,
        transaction: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.check()?;
        let position = self.position(transaction)?;
        let entry = self.transactions[position];
        if entry.prepared.is_some() && entry.live() && !entry.doomed {
            return Ok(());
        }
        self.check_active(transaction)?;
        self.reserve_order(true)?;
        let incoming = edge_range(&self.incoming, entry.id);
        // Inspect every candidate, including cancellation, before dooming any peer.
        for index in incoming.clone() {
            control.check()?;
            let pivot = self.node(self.incoming[index].1);
            if self.dangerous_pivot(entry.id, pivot, Some(control))? && pivot.prepared.is_some() {
                self.transactions[position].doomed = true;
                return Err(VersionError::SerializationConflict { transaction });
            }
        }
        for index in incoming {
            let pivot = self.node(self.incoming[index].1);
            if self.dangerous_pivot(entry.id, pivot, None)? {
                let position = self.allocation_position(pivot.id)?;
                self.transactions[position].doomed = true;
            }
        }
        self.clock += 1;
        self.pending_finishes += 1;
        self.transactions[position].prepared = Some(self.clock);
        Ok(())
    }

    /// Publish a confirmed committed outcome without allocation or cancellation. Never call this on a merely attempted or uncertain provider commit. Repeating the same confirmed outcome is harmless.
    pub fn commit(&mut self, transaction: SerializableTransactionId) -> VersionResult<()> {
        let position = self.position(transaction)?;
        let entry = self.transactions[position];
        if entry.committed.is_some() {
            return Ok(());
        }
        if entry.aborted || entry.doomed || entry.prepared.is_none() {
            return Err(VersionError::InvalidEncoding(
                "serializable completion requires a prepared transaction",
            ));
        }
        if entry
            .publication
            .is_some_and(|publication| !publication.committed())
        {
            return Err(VersionError::InvalidEncoding(
                "durable serializable publication requires a confirmed receipt",
            ));
        }
        // Each prepared transaction owns a completion slot; admission cannot consume it.
        self.clock += 1;
        self.pending_finishes -= 1;
        self.transactions[position].committed = Some(self.clock);
        Ok(())
    }

    /// Remove a confirmed aborted participant from dependency decisions. This cleanup does not allocate or observe statement cancellation. Uncertain prepared outcomes must remain retained.
    pub fn rollback(&mut self, transaction: SerializableTransactionId) -> VersionResult<()> {
        let position = self.position(transaction)?;
        let entry = &mut self.transactions[position];
        if entry.committed.is_some() {
            return Err(VersionError::TransactionFinished);
        }
        if entry
            .publication
            .is_some_and(|publication| !publication.aborted())
        {
            return Err(VersionError::InvalidEncoding(
                "durable serializable publication requires a confirmed abort",
            ));
        }
        if !entry.aborted {
            if entry.prepared.is_some() {
                self.pending_finishes -= 1;
            }
            entry.aborted = true;
        }
        self.predicates.forget(transaction.allocation());
        Ok(())
    }

    /// A deferrable reader waits only for read/write transactions overlapping its snapshot admission. A committed overlap with a conflict to an earlier prepared transaction makes this snapshot unsafe; the owner must capture a new snapshot before executing it.
    pub fn safe_snapshot(
        &self,
        transaction: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<SafeSnapshot> {
        control.check()?;
        self.check_active(transaction)?;
        let reader = self.transactions[self.position(transaction)?];
        if !reader.read_only {
            return Err(VersionError::InvalidEncoding(
                "safe-snapshot check requires a read-only transaction",
            ));
        }
        let mut pending = false;
        for entry in &*self.transactions {
            control.check()?;
            if entry.read_only
                || !entry.relevant()
                || entry.snapshot > reader.snapshot
                || entry.committed.is_some_and(|end| end <= reader.snapshot)
            {
                continue;
            }
            if entry.live() {
                pending = true;
            } else if self
                .earliest_out(*entry, control)?
                .is_some_and(|order| order <= reader.snapshot)
            {
                return Ok(SafeSnapshot::Unsafe);
            }
        }
        Ok(if pending {
            SafeSnapshot::Pending
        } else {
            SafeSnapshot::Safe
        })
    }

    /// Reclaim participants that cannot overlap any live snapshot, preserving earlier conflict-out order on every retained reader. Dropping those summaries would allow a late read of a committed pivot to miss a serialization anomaly. No allocation or cancellation is needed for cleanup.
    pub fn reclaim(&mut self) {
        let oldest = self
            .transactions
            .iter()
            .filter(|entry| entry.live())
            .map(|entry| entry.snapshot)
            .min();
        let Some(oldest) = oldest else {
            self.transactions = BudgetedVec::new(self.transactions.budget());
            self.outgoing = BudgetedVec::new(self.outgoing.budget());
            self.incoming = BudgetedVec::new(self.incoming.budget());
            self.predicates.clear();
            return;
        };
        for index in 0..self.transactions.len() {
            let entry = self.transactions[index];
            let mut earliest = entry.summarized_out;
            for edge in &self.outgoing[edge_range(&self.outgoing, entry.id)] {
                let target = self.node(edge.1);
                if target.relevant() && target.committed.is_some_and(|end| end <= oldest) {
                    if let Some(order) = target.prepared {
                        earliest = Some(earliest.map_or(order, |old| old.min(order)));
                    }
                }
            }
            self.transactions[index].summarized_out = earliest;
        }
        retain_copy(&mut self.transactions, |entry| {
            !entry.aborted && entry.committed.is_none_or(|end| end > oldest)
        });
        let retained = &self.transactions;
        let retain_edge = |edge: Edge| {
            retained
                .binary_search_by_key(&edge.0, |entry| entry.id)
                .is_ok()
                && retained
                    .binary_search_by_key(&edge.1, |entry| entry.id)
                    .is_ok()
        };
        retain_copy(&mut self.outgoing, retain_edge);
        retain_copy(&mut self.incoming, retain_edge);
        self.predicates.reclaim(&self.transactions);
    }

    fn reserve_order(&self, preparing: bool) -> VersionResult<()> {
        self.clock
            .checked_add(self.pending_finishes)
            .and_then(|order| order.checked_add(1 + u64::from(preparing)))
            .ok_or(VersionError::SequenceExhausted)?;
        Ok(())
    }

    fn validate_identity(&self, transaction: SerializableTransactionId) -> VersionResult<()> {
        if transaction.database() != self.database {
            return Err(VersionError::WrongDatabase);
        }
        if transaction.coordinator() != self.coordinator {
            return Err(VersionError::WrongSerializableCoordinator);
        }
        Ok(())
    }

    fn position(&self, transaction: SerializableTransactionId) -> VersionResult<usize> {
        self.validate_identity(transaction)?;
        self.allocation_position(transaction.allocation())
    }

    fn allocation_position(&self, allocation: u64) -> VersionResult<usize> {
        self.transactions
            .binary_search_by_key(&allocation, |entry| entry.id)
            .map_err(|_| VersionError::UnknownTransaction)
    }

    fn node(&self, allocation: u64) -> Transaction {
        self.transactions[self
            .allocation_position(allocation)
            .expect("retained SSI edge must name a retained transaction")]
    }
}

fn edge_range(edges: &[Edge], allocation: u64) -> std::ops::Range<usize> {
    edges.partition_point(|edge| edge.0 < allocation)
        ..edges.partition_point(|edge| edge.0 <= allocation)
}

fn insert_edge(edges: &mut BudgetedVec<Edge>, edge: Edge) -> VersionResult<()> {
    if let Err(position) = edges.binary_search(&edge) {
        edges.push(edge)?;
        edges[position..].rotate_right(1);
    }
    Ok(())
}

fn retain_copy<T: Copy>(values: &mut BudgetedVec<T>, mut retain: impl FnMut(T) -> bool) {
    let mut kept = 0;
    for index in 0..values.len() {
        if retain(values[index]) {
            values[kept] = values[index];
            kept += 1;
        }
    }
    values.truncate(kept);
}
