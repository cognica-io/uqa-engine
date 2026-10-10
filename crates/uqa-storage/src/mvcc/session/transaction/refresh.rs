//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Advance command visibility without losing private changes or their original write requirements.

use super::{
    PrivateRecordChanges, StorageReadControl, Transaction, VersionResult, VersionedPersistence,
};
use crate::mvcc::resolution::{self, ResolutionMode};

mod validation;

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
            if let Some(monitor) = current.commit_monitor() {
                // The same records under the monitor's current value, which spares the next refresh this capture.
                if !self.committed.adopt_commit_monitor(monitor) {
                    self.committed = current;
                }
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
        let records = resolved.as_ref().unwrap_or(&prepared);
        validation::validate(records, current.as_ref(), control)?;
        let changes = PrivateRecordChanges::from_prepared(
            records,
            persistence.private_revision_scope(),
            control,
        )?;
        changes.inherit_retained_sources(&self.changes, control)?;
        control.check()?;
        self.changes = changes;
        self.committed = current;
        Ok(())
    }
}
