//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Advance command visibility without losing private changes or their original write requirements.

use super::{
    PrivateRecordChanges, StorageReadControl, Transaction, VersionError, VersionResult,
    VersionedPersistence,
};
use crate::mvcc::resolution::{self, ResolutionMode};

impl Transaction {
    pub(in crate::mvcc::session) fn refresh(
        &mut self,
        persistence: &dyn VersionedPersistence,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        self.unsealed()?;
        let current = persistence.snapshot(control)?;
        if current.sequence() == self.committed.sequence() {
            return Ok(());
        }
        let prepared = self.prepare(control)?;
        let resolved = resolution::resolve(
            &prepared,
            &*self.committed,
            &current,
            persistence,
            ResolutionMode::Command,
            control,
        )?;
        let records = resolved.as_ref().unwrap_or(&prepared).records();
        for (mutation, write) in records.iter().enumerate() {
            control.cancellation().check()?;
            let actual = current
                .metadata(write.key(), control)?
                .and_then(|row| row.revision);
            if write.expected() != actual {
                return Err(VersionError::WriteConflict {
                    mutation,
                    expected: write.expected(),
                    actual,
                });
            }
        }
        let changes = PrivateRecordChanges::new(control.memory());
        changes.apply_owned(records, control)?;
        control.check()?;
        self.changes = changes;
        self.committed = current;
        Ok(())
    }
}
