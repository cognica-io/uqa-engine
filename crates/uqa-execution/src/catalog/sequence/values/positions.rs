//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocate a persistent sequence's values at its shared position.
//!
//! The durable record runs ahead of the values handed out, as `PostgreSQL`'s log does, and is written once for the values it covers instead of once for each. The exact position is shared by every session and process of the database, so the values and the log count any of them sees are the ones `PostgreSQL` shows.

use std::collections::HashSet;

use super::{
    allocation::exhausted,
    persistence::{autonomous_value, sequence_storage_error},
    NextvalTarget, SequenceValueContext, SequenceValueError,
};
use crate::catalog::snapshot_read::with_read_transaction;
use crate::row_locks::{
    RowLockManager, SequencePosition, SequencePositionGuard, SequencePositionKey,
};
use uqa_storage::{
    sequence_value_allocation, CatalogFacade, PersistentStorageSession, SequenceLogResult,
    SequenceReservationResult, SequenceValuePosition, SequenceValueReservation,
    StorageBackendResult,
};

/// A reserved block of values and what now holds the sequence's position.
#[derive(Clone, Copy)]
pub(super) struct ReservedBlock {
    pub reservation: SequenceValueReservation,
    /// Whether an independent session allocated the block, outside the caller's transaction.
    pub autonomous: bool,
    /// Whether the shared position holds the sequence's value state. Every reader takes it from there, so the session's registry is left as it is.
    pub positioned: bool,
}

/// A reserved block, or `None` when the sequence's definition changed and the caller binds it again.
pub(super) type Reserved = Option<ReservedBlock>;

impl NextvalTarget {
    pub(super) const fn position_key(&self) -> SequencePositionKey {
        SequencePositionKey {
            object: self.object_id,
            definition: self.state.definition_generation,
        }
    }

    const fn record(&self) -> SequenceValuePosition {
        SequenceValuePosition {
            current: self.state.current,
            called: self.state.called,
            log_count: self.state.log_count,
        }
    }
}

impl SequenceValueContext<'_> {
    /// The position store of a sequence whose values are allocated outside the caller's transaction.
    pub(super) fn shared_positions(
        &self,
        target: &NextvalTarget,
        private: bool,
    ) -> Option<&RowLockManager> {
        if target.temporary || private || self.storage.is_none() {
            return None;
        }
        self.runtime.sequence_positions()
    }

    fn independent_session(&self) -> Result<PersistentStorageSession, SequenceValueError> {
        self.runtime
            .open_nontransactional_sequence_session()
            .map_err(|error| sequence_storage_error("open sequence session", error))?
            .ok_or_else(|| {
                SequenceValueError::Internal(
                    "sequence positions require an independent catalog session".into(),
                )
            })
    }

    fn independent_value<T>(
        &self,
        action: &str,
        operation: impl Fn(&dyn CatalogFacade) -> StorageBackendResult<T>,
    ) -> Result<T, SequenceValueError> {
        autonomous_value(
            &self.independent_session()?,
            self.runtime.cancellation(),
            &operation,
        )
        .map_err(|error| sequence_storage_error(action, error))
    }

    /// Reserve the next block at the sequence's shared position, moving the durable record first when it does not cover the block.
    pub(super) fn reserve_at_position(
        &self,
        positions: &RowLockManager,
        target: &NextvalTarget,
    ) -> Result<Reserved, SequenceValueError> {
        let mut guard = positions.lock_sequence_position(target.position_key())?;
        let mut record = target.record();
        let mut position = match guard.recorded() {
            Some(recorded) => {
                let position = recorded.continuing((record.current, record.called));
                if position.is_none() {
                    // Kept from an earlier run of a database whose record is no longer the one it continues.
                    guard.discard()?;
                }
                position
            }
            None => None,
        };
        if position.is_none() {
            if guard.crowded() {
                self.drop_positions_of_missing_sequences(&mut guard)?;
            }
            if !guard.recordable() {
                return self.reserve_exactly(target);
            }
        }
        let state = target.state;
        loop {
            self.runtime.cancellation().check()?;
            let base = position.map_or(record, |position| SequenceValuePosition {
                current: position.current,
                called: position.called,
                log_count: position.log_count,
            });
            let allocation = sequence_value_allocation(
                base,
                state.increment,
                state.min_value,
                state.max_value,
                state.cycle,
                state.cache_size,
            )
            .ok_or_else(|| exhausted(&target.relation.name, state))?;
            let mut logged = position.map_or((record.current, record.called), |held| held.logged);
            if let Some(value) = allocation.record_value(
                base,
                position.map(|held| held.logged.0),
                state.increment,
                state.min_value,
                state.max_value,
            ) {
                let result = self.independent_value("log sequence values", |catalog| {
                    catalog.log_sequence_values(
                        &target.name,
                        target.object_id,
                        state.definition_generation,
                        logged,
                        SequenceValuePosition {
                            current: value,
                            called: true,
                            log_count: 0,
                        },
                    )
                })?;
                match result {
                    SequenceLogResult::Logged => logged = (value, true),
                    SequenceLogResult::Changed(actual) => {
                        // Something that keeps no position moved the record, and its values lie past the record it wrote.
                        guard.discard()?;
                        position = None;
                        record = actual;
                        continue;
                    }
                    SequenceLogResult::DefinitionChanged => return Ok(None),
                    SequenceLogResult::Missing => {
                        return Err(SequenceValueError::Undefined(target.name.clone()))
                    }
                }
            }
            let reservation = allocation.reservation;
            guard.record(SequencePosition {
                logged,
                current: reservation.last_value,
                called: true,
                log_count: reservation.log_count,
            })?;
            return Ok(Some(ReservedBlock {
                reservation,
                autonomous: true,
                positioned: true,
            }));
        }
    }

    /// Reserve through the durable record alone, which then holds the exact position. A sequence the position store has no room for allocates this way.
    fn reserve_exactly(&self, target: &NextvalTarget) -> Result<Reserved, SequenceValueError> {
        let result = self.independent_value("reserve sequence values", |catalog| {
            catalog.reserve_sequence_values(
                &target.name,
                target.object_id,
                target.state.definition_generation,
            )
        })?;
        match result {
            SequenceReservationResult::Reserved(reservation) => Ok(Some(ReservedBlock {
                reservation,
                autonomous: true,
                positioned: false,
            })),
            SequenceReservationResult::DefinitionChanged => Ok(None),
            SequenceReservationResult::Missing => {
                Err(SequenceValueError::Undefined(target.name.clone()))
            }
            SequenceReservationResult::Exhausted => {
                Err(exhausted(&target.relation.name, target.state))
            }
        }
    }

    /// Drop the positions of sequences that were dropped or whose allocation generation was replaced. The committed sequences are read while the positions are held, so every position recorded so far belongs to a sequence this read sees or to one that is gone.
    fn drop_positions_of_missing_sequences(
        &self,
        guard: &mut SequencePositionGuard<'_>,
    ) -> Result<(), SequenceValueError> {
        let session = self.independent_session()?;
        let existing = with_read_transaction(&session, |catalog| catalog.load_sequence_rows())
            .map_err(|error| sequence_storage_error("load sequence catalog", error))?
            .into_iter()
            .map(|row| SequencePositionKey {
                object: row.object_id,
                definition: row.definition_generation,
            })
            .collect::<HashSet<_>>();
        guard.retain(&|key| existing.contains(key))?;
        Ok(())
    }

    /// Forget the position of a sequence whose record a value assignment replaces.
    pub(super) fn discard_position(
        &self,
        target: &NextvalTarget,
    ) -> Result<Option<SequencePositionGuard<'_>>, SequenceValueError> {
        if target.temporary || self.storage.is_none() {
            return Ok(None);
        }
        let Some(positions) = self.runtime.sequence_positions() else {
            return Ok(None);
        };
        Ok(Some(
            positions.lock_sequence_position(target.position_key())?,
        ))
    }
}
