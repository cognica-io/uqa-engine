//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical reads of the native projection at a snapshot that is the database's latest commit.
//!
//! Each record commit materializes the native projection in its own physical transaction and advances the database header in that transaction, so a read whose header sequence equals a snapshot's boundary sees the projection of exactly that boundary. A session's private records are not projected, so the projection stands in for the snapshot only when the session holds none.

use rusqlite::Connection;
use uqa_storage::read_control::StorageReadControl;

use super::NativeSnapshot;
use crate::connection::Result;

impl NativeSnapshot {
    /// Run `read` in one physical read when the native projection holds exactly this snapshot's committed records. Returns `None` without running it otherwise; the caller then reads the records. `read` must not read this snapshot.
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
