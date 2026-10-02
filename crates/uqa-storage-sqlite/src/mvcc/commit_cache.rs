//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The page cache of the connection that writes a commit.

use rusqlite::Connection;
use uqa_storage::mvcc::PreparedRecordCommit;

use super::PhysicalResult;

/// The kibibytes of pages `SQLite` lets a connection keep unless it is told otherwise.
const ORDINARY_KIB: u64 = 2000;
/// The bytes of pages a commit dirties for each byte of its records. A record is stored as a version, its metadata and a head, and a native record once more as a row of its table and of each index of that table, in B-trees whose pages are not full.
const DIRTIED_FOR_EACH_RECORD_BYTE: u64 = 8;
/// The most a commit may keep, in kibibytes.
const LIMIT_KIB: u64 = 256 * 1024;

/// A page cache that holds what a commit dirties, for as long as the commit is written.
///
/// A transaction that has dirtied more pages than its connection may keep writes dirty pages into the log to make room, reads them back when it needs them again and writes them once more after they change. A commit is one physical transaction that writes every record of its logical transaction to the record tables and to the native tables, so a large commit dirties many times the 2 MiB a connection keeps and spends its time on pages it writes more than once. The connection that writes a commit therefore may keep the pages the commit is expected to dirty. The limit allocates nothing by itself, and the connection returns to its ordinary limit and releases what it holds beyond it when the commit is done, whether it was written or not.
pub(super) struct CommitCache<'a> {
    connection: &'a Connection,
    ordinary: i64,
}

impl<'a> CommitCache<'a> {
    /// Raise the cache limit of `connection` for a commit of `prepared`. A commit the ordinary cache holds changes nothing and runs no statement.
    pub(super) fn grow(
        connection: &'a Connection,
        prepared: &PreparedRecordCommit,
    ) -> PhysicalResult<Option<Self>> {
        let records = prepared
            .records()
            .iter()
            .map(|record| record.key().len() + record.value().map_or(0, <[u8]>::len))
            .fold(0_u64, |bytes, record| bytes.saturating_add(record as u64));
        let wanted = (records.saturating_mul(DIRTIED_FOR_EACH_RECORD_BYTE) / 1024).min(LIMIT_KIB);
        if wanted <= ORDINARY_KIB {
            return Ok(None);
        }
        let ordinary: i64 = connection
            .prepare_cached("PRAGMA cache_size")?
            .query_row([], |row| row.get(0))?;
        // A negative size counts kibibytes and a positive one pages, which hold at least half a kibibyte each.
        let kept = match u64::try_from(ordinary) {
            Ok(pages) => pages / 2,
            Err(_) => ordinary.unsigned_abs(),
        };
        if wanted <= kept {
            return Ok(None);
        }
        let limit = i64::try_from(wanted).unwrap_or(i64::MAX);
        connection.pragma_update(None, "cache_size", -limit)?;
        Ok(Some(Self {
            connection,
            ordinary,
        }))
    }
}

impl Drop for CommitCache<'_> {
    fn drop(&mut self) {
        // The limit only bounds memory. A connection that could not lower it keeps the larger bound until the next commit that raises it restores the ordinary one.
        let _ = self
            .connection
            .pragma_update(None, "cache_size", self.ordinary);
        let _ = self.connection.execute_batch("PRAGMA shrink_memory");
    }
}
