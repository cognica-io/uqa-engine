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
        if let Some(captured_at) = self.committed.commit_monitor() {
            // Nothing was committed since this snapshot was captured, so a new one would be the same.
            if persistence.commit_monitor_version()? == Some(captured_at) {
                control.cancellation().check()?;
                return Ok(());
            }
        }
        let current = persistence.snapshot(control)?;
        if current.sequence() == self.committed.sequence() {
            if current.commit_monitor().is_some() {
                // The same records under the monitor's current value, which spares the next refresh this capture.
                self.committed = current;
            }
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
        changes.inherit_retained_sources(&self.changes, control)?;
        control.check()?;
        self.changes = changes;
        self.committed = current;
        Ok(())
    }
}
