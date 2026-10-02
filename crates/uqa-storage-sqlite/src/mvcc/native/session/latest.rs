//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical reads of the native projection at a snapshot that is the database's latest commit.
//!
//! Each record commit materializes the native projection in its own physical transaction and advances the database header in that transaction, so a read whose header sequence equals a snapshot's boundary sees the projection of exactly that boundary. A session's private records are not projected, so the projection stands in for the snapshot only where the session holds none: for every record when it holds none at all, and otherwise for the records under a prefix that none of its private records share.

use rusqlite::Connection;
use uqa_core::memory::BudgetedVec;
use uqa_storage::read_control::StorageReadControl;

use super::NativeSnapshot;
use crate::connection::Result;

impl NativeSnapshot {
    /// Run `read` in one physical read when the native projection holds exactly this snapshot's records. Returns `None` without running it otherwise; the caller then reads the records. `read` must not read this snapshot.
    pub(crate) fn read_latest_projection<T>(
        &self,
        control: &StorageReadControl,
        read: &mut dyn FnMut(&Connection) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        self.control.check()?;
        control.check()?;
        if self.view.private_revision().is_some() {
            return Ok(None);
        }
        self.read_latest_committed(control, read)
    }

    /// As [`Self::read_latest_projection`], for a `read` of the records under `prefix` alone: private records elsewhere are not among them, so only a private record under the prefix sends the caller to the records. `prefix` is encoded only for a session that holds private records.
    pub(crate) fn read_latest_projection_under<T>(
        &self,
        prefix: &dyn Fn() -> Result<BudgetedVec<u8>>,
        control: &StorageReadControl,
        read: &mut dyn FnMut(&Connection) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        self.control.check()?;
        control.check()?;
        if self.view.private_revision().is_some()
            && !self
                .view
                .private_keys(&prefix()?, None, 1, control)?
                .is_empty()
        {
            return Ok(None);
        }
        self.read_latest_committed(control, read)
    }

    fn read_latest_committed<T>(
        &self,
        control: &StorageReadControl,
        read: &mut dyn FnMut(&Connection) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        let Some(snapshot) = self
            .view
            .committed()
            .provider_snapshot()
            .and_then(|snapshot| snapshot.downcast_ref::<crate::mvcc::read::Snapshot>())
        else {
            return Ok(None);
        };
        let result = snapshot.read_latest(|connection| Ok(read(connection)?))?;
        self.control.check()?;
        control.check()?;
        Ok(result)
    }
}
