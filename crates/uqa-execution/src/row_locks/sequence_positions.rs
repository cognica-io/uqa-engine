//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The exact position of each sequence between its durable records, shared by the sessions and processes of one database.

use std::collections::HashMap;

use parking_lot::MutexGuard;
use uqa_sql::SQLError;

use super::{cross_process::FileLockCoordinator, RowLockManager};

/// One allocation generation of one sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SequencePositionKey {
    pub object: [u8; 16],
    pub definition: [u8; 16],
}

/// What a sequence has handed out, and the durable record that covers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequencePosition {
    /// The value and the called flag of the durable record. Every value handed out is at or before this value.
    pub logged: (i64, bool),
    pub current: i64,
    pub called: bool,
    pub log_count: i64,
}

/// A recorded position and whether it was written in this run of the database. A position kept from an earlier run is exact only while the durable record still is the one it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordedSequencePosition {
    pub position: SequencePosition,
    pub fresh: bool,
}

impl RecordedSequencePosition {
    /// The position when it continues a durable record with this value and called flag.
    #[must_use]
    pub fn continuing(self, record: (i64, bool)) -> Option<SequencePosition> {
        (self.fresh || self.position.logged == record).then_some(self.position)
    }
}

/// What the position store holds for one sequence.
pub(super) struct SequenceSlot {
    pub recorded: Option<RecordedSequencePosition>,
    /// Whether the store has room for the sequence's position.
    pub recordable: bool,
    /// Whether the store should drop the positions of sequences that no longer exist before it takes a new one.
    pub crowded: bool,
}

/// The positions of a database without a position sidecar, which end with this process.
#[derive(Default)]
pub(super) struct LocalSequencePositions {
    positions: HashMap<SequencePositionKey, SequencePosition>,
}

/// The position of one sequence, held so that no other session or process reads or changes it.
pub struct SequencePositionGuard<'a> {
    key: SequencePositionKey,
    local: MutexGuard<'a, LocalSequencePositions>,
    shared: Option<&'a FileLockCoordinator>,
    slot: SequenceSlot,
}

impl SequencePositionGuard<'_> {
    #[must_use]
    pub fn recorded(&self) -> Option<RecordedSequencePosition> {
        self.slot.recorded
    }

    /// Whether a position can be recorded. A sequence without room for one keeps its durable record exact instead.
    #[must_use]
    pub fn recordable(&self) -> bool {
        self.slot.recordable
    }

    /// Whether the positions of sequences that no longer exist should be dropped before this sequence takes a slot.
    #[must_use]
    pub fn crowded(&self) -> bool {
        self.slot.crowded
    }

    pub fn record(&mut self, position: SequencePosition) -> Result<(), SQLError> {
        let recorded = match self.shared {
            Some(coordinator) => coordinator
                .record_sequence_position(&self.key, &position)
                .map_err(SQLError::Internal)?,
            None => {
                self.local.positions.insert(self.key, position);
                true
            }
        };
        if !recorded {
            return Err(SQLError::Internal(
                "the sequence position store has no room for a position it offered".into(),
            ));
        }
        self.slot.recorded = Some(RecordedSequencePosition {
            position,
            fresh: true,
        });
        Ok(())
    }

    /// Forget the position, after which the sequence continues from its durable record.
    pub fn discard(&mut self) -> Result<(), SQLError> {
        if self.slot.recorded.take().is_none() {
            return Ok(());
        }
        match self.shared {
            Some(coordinator) => coordinator
                .remove_sequence_position(&self.key)
                .map_err(SQLError::Internal),
            None => {
                self.local.positions.remove(&self.key);
                Ok(())
            }
        }
    }

    /// Drop every position `keep` does not name. The caller names the allocation generations of the sequences that exist, read while it holds this guard.
    pub fn retain(&mut self, keep: &dyn Fn(&SequencePositionKey) -> bool) -> Result<(), SQLError> {
        match self.shared {
            Some(coordinator) => {
                coordinator
                    .retain_sequence_positions(keep)
                    .map_err(SQLError::Internal)?;
                self.slot = coordinator
                    .read_sequence_slot(&self.key)
                    .map_err(SQLError::Internal)?;
            }
            None => self.local.positions.retain(|key, _| keep(key)),
        }
        // One pass is all the store can free, so a store that is still full is no longer asked again.
        self.slot.crowded = false;
        Ok(())
    }
}

impl Drop for SequencePositionGuard<'_> {
    fn drop(&mut self) {
        if let Some(coordinator) = self.shared {
            coordinator.unlock_sequence_positions();
        }
    }
}

impl RowLockManager {
    fn sequence_position_store(&self) -> Result<Option<&FileLockCoordinator>, SQLError> {
        Ok(self
            .coordinator()?
            .filter(|coordinator| coordinator.shares_sequence_positions()))
    }

    /// Hold the position of one sequence until the guard is dropped. Sessions of this process wait for each other here, and other processes wait at the sidecar.
    pub fn lock_sequence_position(
        &self,
        key: SequencePositionKey,
    ) -> Result<SequencePositionGuard<'_>, SQLError> {
        let shared = self.sequence_position_store()?;
        let local = self.sequence_positions.lock();
        let slot = match shared {
            Some(coordinator) => {
                coordinator
                    .lock_sequence_positions()
                    .map_err(SQLError::Internal)?;
                match coordinator.read_sequence_slot(&key) {
                    Ok(slot) => slot,
                    Err(error) => {
                        coordinator.unlock_sequence_positions();
                        return Err(SQLError::Internal(error));
                    }
                }
            }
            None => SequenceSlot {
                recorded: local
                    .positions
                    .get(&key)
                    .map(|position| RecordedSequencePosition {
                        position: *position,
                        fresh: true,
                    }),
                recordable: true,
                crowded: false,
            },
        };
        Ok(SequencePositionGuard {
            key,
            local,
            shared,
            slot,
        })
    }

    /// The recorded position of one sequence.
    pub fn sequence_position(
        &self,
        key: SequencePositionKey,
    ) -> Result<Option<RecordedSequencePosition>, SQLError> {
        Ok(self.lock_sequence_position(key)?.recorded())
    }

    /// The recorded position of every sequence.
    pub fn sequence_positions(
        &self,
    ) -> Result<HashMap<SequencePositionKey, RecordedSequencePosition>, SQLError> {
        let shared = self.sequence_position_store()?;
        let local = self.sequence_positions.lock();
        let Some(coordinator) = shared else {
            return Ok(local
                .positions
                .iter()
                .map(|(key, position)| {
                    (
                        *key,
                        RecordedSequencePosition {
                            position: *position,
                            fresh: true,
                        },
                    )
                })
                .collect());
        };
        coordinator
            .lock_sequence_positions()
            .map_err(SQLError::Internal)?;
        let positions = coordinator.read_sequence_positions();
        coordinator.unlock_sequence_positions();
        Ok(positions.map_err(SQLError::Internal)?.into_iter().collect())
    }
}

#[cfg(test)]
mod tests;
