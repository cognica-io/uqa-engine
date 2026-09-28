//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Finite, borrowed registry reads with consumer-owned retention and cancellation.

use std::{num::NonZeroUsize, ops::ControlFlow};

use uqa_storage::{
    notifications::{NotificationQueueEntryRef, NotificationQueueScan},
    read_control::StorageReadControl,
    StorageBackendResult,
};

use super::control;

use super::{nonnegative_u64, registry_error, sqlite_integer, NotificationRegistryTransaction};

impl NotificationRegistryTransaction {
    /// Visit at most `max_entries` ordered rows without materializing their strings or a result vector. The consumer must admit any copies under its own queue limits before retaining them. Existing `SQLite` connection/cache memory remains owned by the registry.
    ///
    /// `Continue` accepts the current row and advances the returned resume boundary; `Break` declines it and stops without advancing past it. `visited` includes a declined row. This method never changes a listener cursor or commits a transaction. Consumer errors and cancellation stop the scan; an error grants no cursor advancement.
    pub fn visit_entries_from(
        &self,
        from_sequence: u64,
        max_entries: NonZeroUsize,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(
            NotificationQueueEntryRef<'_>,
        ) -> StorageBackendResult<ControlFlow<()>>,
    ) -> StorageBackendResult<NotificationQueueScan> {
        let _operation =
            control::operation_with(&self.connection, self.control.as_ref(), Some(control))?;
        control.check()?;
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT sequence, process_id, channel, payload FROM queue_entries WHERE sequence >= ?1 ORDER BY sequence LIMIT ?2",
            )
            .map_err(|error| registry_error("prepare queue scan", &error))?;
        let mut rows = statement
            .query(rusqlite::params![
                sqlite_integer(from_sequence, "scan sequence")?,
                i64::try_from(max_entries.get()).unwrap_or(i64::MAX),
            ])
            .map_err(|error| registry_error("scan queue entries", &error))?;
        let mut progress = NotificationQueueScan {
            visited: 0,
            next_sequence: from_sequence,
            exhausted: false,
        };
        while progress.visited < max_entries.get() {
            control.check()?;
            let Some(row) = rows
                .next()
                .map_err(|error| registry_error("read queue entry", &error))?
            else {
                control.check()?;
                progress.exhausted = true;
                return Ok(progress);
            };
            let entry = read_entry(row)?;
            control.check()?;
            progress.visited += 1;
            let accepted = visit(entry)?;
            control.check()?;
            if accepted.is_break() {
                return Ok(progress);
            }
            // SQLite INTEGER bounds make this addition safe without saturating or skipping a value.
            progress.next_sequence = entry.sequence + 1;
        }
        Ok(progress)
    }
}

fn read_entry<'row>(
    row: &'row rusqlite::Row<'_>,
) -> StorageBackendResult<NotificationQueueEntryRef<'row>> {
    let sequence = row
        .get::<_, i64>(0)
        .map_err(|error| registry_error("read queue entry sequence", &error))?;
    let process_id = row
        .get(1)
        .map_err(|error| registry_error("read queue entry process identifier", &error))?;
    let channel = row
        .get_ref(2)
        .and_then(|value| value.as_str().map_err(rusqlite::Error::from))
        .map_err(|error| registry_error("read queue entry channel", &error))?;
    let payload = row
        .get_ref(3)
        .and_then(|value| value.as_str().map_err(rusqlite::Error::from))
        .map_err(|error| registry_error("read queue entry payload", &error))?;
    Ok(NotificationQueueEntryRef {
        sequence: nonnegative_u64(sequence, "entry sequence")?,
        process_id,
        channel,
        payload,
    })
}
