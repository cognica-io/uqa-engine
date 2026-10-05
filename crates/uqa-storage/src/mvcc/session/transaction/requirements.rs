//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact required keys retain one original revision and their insertion order for undo.

use uqa_core::memory::{BudgetedMap, BudgetedVec, MemoryBudget};

use super::{
    CommitSequence, RecordKey, StorageReadControl, Transaction, VersionError, VersionResult,
};
use crate::mvcc::commit::RecordRequirement;

struct Position {
    index: usize,
    from_view: bool,
}

pub(super) struct Requirements {
    records: BudgetedVec<RecordRequirement>,
    keys: BudgetedMap<RecordKey, Position>,
}

impl Requirements {
    pub(super) fn new(memory: &MemoryBudget) -> Self {
        Self {
            records: BudgetedVec::new(memory),
            keys: BudgetedMap::new(memory),
        }
    }

    fn get(&self, key: &[u8]) -> Option<(&RecordRequirement, bool)> {
        self.keys
            .get(key)
            .map(|position| (&self.records[position.index], position.from_view))
    }

    fn push(&mut self, record: RecordRequirement, from_view: bool) -> VersionResult<()> {
        self.records.reserve(1)?;
        let entry = self.keys.prepare_entry(
            record.key.clone(),
            Position {
                index: self.records.len(),
                from_view,
            },
        )?;
        self.records.push(record)?;
        self.keys.insert_prepared(entry);
        Ok(())
    }

    pub(super) fn invalidate_view(&mut self, key: &[u8]) {
        if let Some(position) = self.keys.get_mut(key) {
            position.from_view = false;
        }
    }

    pub(super) fn truncate(&mut self, len: usize) {
        for record in &self.records[len..] {
            self.keys.remove(record.key.bytes());
        }
        super::truncate_retained(&mut self.records, len);
    }

    #[cfg(test)]
    pub(super) fn capacity(&self) -> usize {
        self.records.capacity()
    }
}

impl std::ops::Deref for Requirements {
    type Target = [RecordRequirement];

    fn deref(&self) -> &Self::Target {
        &self.records
    }
}

impl Transaction {
    pub(in crate::mvcc::session) fn require_unchanged(
        &mut self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.writable()?;
        control.check()?;
        if self
            .requirements
            .get(key)
            .is_some_and(|(_, from_view)| from_view)
        {
            // Ordinary private writes preserve their original revision, observed writes must match this requirement, and refresh validates every requirement before advancing its committed view. Savepoint undo removes requirements together with the writes that introduced them.
            return Ok(());
        }
        let expected = self
            .view()?
            .metadata(key, control)?
            .and_then(|record| record.revision);
        if let Some((requirement, _)) = self.requirements.get(key) {
            return if requirement.expected == expected {
                Ok(())
            } else {
                Err(VersionError::InvalidEncoding(
                    "record requirements disagree",
                ))
            };
        }
        self.requirements.push(
            RecordRequirement {
                key: RecordKey::new(key, control.memory())?,
                expected,
            },
            true,
        )
    }

    pub(in crate::mvcc::session) fn require_observed(
        &mut self,
        key: &RecordKey,
        expected: CommitSequence,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.writable()?;
        control.check()?;
        if self.changes.write_kind(key.bytes(), control)?.is_some() {
            return Err(VersionError::InvalidEncoding(
                "observed metadata already has a private replacement",
            ));
        }
        if let Some((requirement, _)) = self.requirements.get(key.bytes()) {
            return if requirement.expected == Some(expected) {
                Ok(())
            } else {
                Err(VersionError::InvalidEncoding(
                    "observed metadata preconditions disagree",
                ))
            };
        }
        // An externally observed revision may be newer than this view. Do not treat it as a view-derived requirement until its ordinary validation boundary.
        self.requirements.push(
            RecordRequirement {
                key: key.clone(),
                expected: Some(expected),
            },
            false,
        )
    }
}
