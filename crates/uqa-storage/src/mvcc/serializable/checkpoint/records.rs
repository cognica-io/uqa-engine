//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable checkpoint records let providers publish only inserted, replaced and removed state.

mod decode;
mod encode;
mod key;
#[cfg(test)]
mod tests;

pub use encode::SerializableCheckpointRecord;
pub use key::SerializableCheckpointKey;

use super::{io::invalid, SerializableGraph, VersionResult};
use crate::read_control::StorageReadControl;

const MAGIC: &[u8; 8] = b"UQASREC1";

impl SerializableGraph {
    /// Whether keyed checkpoint state needs publication. Graphs restored from a complete legacy checkpoint need initial record publication even without a logical change. Only a validated keyed-record restore establishes the comparison baseline.
    pub fn checkpoint_records_changed(&self) -> bool {
        self.checkpoint_changed || self.checkpoint_records.is_empty()
    }

    /// Stream the exact difference from the keyed checkpoint loaded under this admission. A missing value deletes the old key; a value inserts a new record or replaces the header. Providers apply the complete stream in one durable transaction, including changes preceding an operation error. This borrows existing graph data and allocates no deletion journal or second encoded graph. It does not acknowledge durability or change the baseline.
    pub fn write_checkpoint_changes<'a>(
        &'a self,
        control: &StorageReadControl,
        mut update: impl FnMut(
            SerializableCheckpointKey,
            Option<SerializableCheckpointRecord<'a>>,
        ) -> VersionResult<()>,
    ) -> VersionResult<()> {
        control.check()?;
        if !self.checkpoint_records_changed() {
            return Ok(());
        }
        let current = [
            self.transactions.len(),
            self.outgoing.len(),
            self.predicates.reads.len(),
            self.predicates.writes.len(),
        ]
        .into_iter()
        .try_fold(1_usize, |total, count| {
            total.checked_add(count).ok_or_else(invalid)
        })?;
        let additional = current
            .saturating_sub(self.checkpoint_records.capacity())
            .checked_mul(std::mem::size_of::<SerializableCheckpointKey>())
            .ok_or_else(invalid)?;
        // A successful publication must remain decodable under this allowance. Existing baseline capacity already covers unchanged and removed records; reserve only the additional future key storage, without allocating another index.
        let _future_keys = self.transactions.budget().reserve(additional)?;
        let mut old = self.checkpoint_records.iter().copied().peekable();
        let mut previous = None;
        self.visit_checkpoint_records(|record| {
            control.check()?;
            let key = record.key(control)?;
            if previous.is_some_and(|previous| previous >= key) {
                return Err(invalid());
            }
            previous = Some(key);
            while old.peek().is_some_and(|old| *old < key) {
                update(old.next().expect("peeked checkpoint key"), None)?;
            }
            let present = old.peek() == Some(&key);
            if present {
                old.next();
            }
            if !present || key == SerializableCheckpointKey::HEADER {
                update(key, Some(record))?;
            }
            Ok(())
        })?;
        for key in old {
            control.check()?;
            update(key, None)?;
        }
        Ok(())
    }

    fn visit_checkpoint_records<'a>(
        &'a self,
        mut visit: impl FnMut(SerializableCheckpointRecord<'a>) -> VersionResult<()>,
    ) -> VersionResult<()> {
        visit(SerializableCheckpointRecord::header(self))?;
        for &entry in &*self.transactions {
            visit(SerializableCheckpointRecord::transaction(self, entry))?;
        }
        for &edge in &*self.outgoing {
            visit(SerializableCheckpointRecord::edge(self, edge))?;
        }
        for (writing, entries) in [
            (false, &self.predicates.reads),
            (true, &self.predicates.writes),
        ] {
            for entry in &**entries {
                visit(SerializableCheckpointRecord::predicate(
                    self, entry, writing,
                ))?;
            }
        }
        Ok(())
    }
}
