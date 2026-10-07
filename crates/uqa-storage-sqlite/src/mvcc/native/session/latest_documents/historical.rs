//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current physical rows plus selected historical replacements form an old snapshot.

use super::{
    admit_row, stores_alone, visit_stored_row, NativeRecordIdentity, NativeRecordOwner,
    NativeSnapshot, PrivateRows, BODY, INLINE_BODY_BYTES, ROWS, ROWS_AFTER,
};
use crate::{connection::Result, mvcc::PhysicalResult};
use rusqlite::{params, types::ValueRef, Connection};
use uqa_core::memory::MemoryReservation;
use uqa_storage::{
    mvcc::{CommitSequence, VersionError},
    read_control::StorageReadControl,
};

mod changes;

pub(crate) struct HistoricalDocuments<'a> {
    connection: &'a Connection,
    table: &'a str,
    snapshot: &'a NativeSnapshot,
    documents: NativeRecordIdentity,
    control: &'a StorageReadControl,
}

impl NativeSnapshot {
    /// Current rows with heads no newer than the retained boundary are unchanged.
    /// Resolve newer heads at that boundary, then overlay the captured private rows.
    pub(crate) fn read_historical_documents<T>(
        &self,
        table: &str,
        owner: NativeRecordOwner,
        control: &StorageReadControl,
        read: &mut dyn FnMut(&HistoricalDocuments<'_>) -> Result<T>,
    ) -> Result<Option<T>> {
        self.control.check()?;
        control.check()?;
        let Some(snapshot) = self
            .view
            .committed()
            .provider_snapshot()
            .and_then(|value| value.downcast_ref::<crate::mvcc::read::Snapshot>())
        else {
            return Ok(None);
        };
        let documents = NativeRecordIdentity::new(super::Family::Documents, owner)?;
        let prefix = documents.encode_prefix(&[], control)?;
        let upper = crate::read_control::prefix_upper_bound(&prefix, control)?;
        let result = snapshot.read_historical(|connection| {
            if !stores_alone(connection, table, owner, control)? {
                return Ok(None);
            }
            // A newer compacted run can hide a head needed by the overlay. Keep
            // the common record reader for that representation, including an
            // inserted document which did not exist at the retained boundary.
            let compacted: bool = connection.prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_runs WHERE key_length = ?1 AND first_key < ?2 AND last_key >= ?3 AND sequence > ?4)"
            )?.query_row(params![i64::try_from(prefix.len() + 9).map_err(|_| VersionError::InvalidEncoding("native document key is too large"))?, upper.as_deref(), &*prefix, self.view.sequence().as_u64().to_be_bytes().as_slice()], |row| row.get(0))?;
            if compacted { return Ok(None); }
            let rows = HistoricalDocuments { connection, table, snapshot: self, documents, control };
            Ok(Some(read(&rows)?))
        })?;
        self.control.check()?;
        control.check()?;
        Ok(result)
    }
}

impl HistoricalDocuments<'_> {
    pub(crate) fn visit(
        &self,
        after: Option<i64>,
        visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> Result<()> {
        self.visit_rows(after, visit)
            .map_err(|error| error.into_version().into())
    }

    fn visit_rows(
        &self,
        after: Option<i64>,
        visit: &mut dyn FnMut(&[ValueRef<'_>]) -> Result<bool>,
    ) -> PhysicalResult<()> {
        let prefix = self.documents.encode_prefix(&[], self.control)?;
        let upper = crate::read_control::prefix_upper_bound(&prefix, self.control)?;
        let start = after
            .map(|id| {
                self.documents
                    .encode_key(&[ValueRef::Integer(id)], self.control)
            })
            .transpose()?;
        let lower = start.as_deref().unwrap_or(&prefix);
        let boundary = self.snapshot.view.sequence();
        let private = self
            .snapshot
            .view
            .private_revision()
            .is_some()
            .then(|| {
                PrivateRows::after(
                    &self.snapshot.view,
                    self.documents,
                    &[],
                    after,
                    self.control,
                )
            })
            .transpose()?;
        let _bindings = crate::read_control::reserve_bindings(
            self.control,
            &[
                lower,
                upper.as_deref().unwrap_or_default(),
                self.table.as_bytes(),
            ],
        )?;
        // Only the fixed-width document key is admitted into the head cursor.
        let _key = self
            .control
            .memory()
            .reserve(prefix.len() + 9)
            .map_err(VersionError::from)?;
        let mut admitted: Option<MemoryReservation> = None;
        let mut changed_statement = self.connection.prepare_cached(
            "SELECT CASE WHEN length(key) = ?4 THEN key END FROM _uqa_mvcc_heads WHERE key > ?1 AND key < ?2 AND sequence > ?3 ORDER BY key"
        )?;
        let heads = changed_statement.query(params![
            lower,
            upper.as_deref(),
            boundary.as_u64().to_be_bytes().as_slice(),
            i64::try_from(prefix.len() + 9)
                .map_err(|_| VersionError::InvalidEncoding("native document key is too large"))?
        ])?;
        let mut changes = changes::Changes::new(
            heads,
            self.connection,
            self.documents,
            boundary,
            self.control,
            private,
        )?;
        let mut body = self.connection.prepare_cached(BODY)?;
        let mut statement =
            self.connection
                .prepare_cached(if after.is_some() { ROWS_AFTER } else { ROWS })?;
        let limit = i64::from(INLINE_BODY_BYTES);
        let mut rows = match after {
            Some(after) => statement.query(params![self.table, limit, after])?,
            None => statement.query(params![self.table, limit])?,
        };
        let mut more = true;
        while let Some(row) = rows.next()? {
            self.control.check().map_err(VersionError::from)?;
            let id = row.get(0)?;
            let (keep_reading, replaced) = changes.before(Some(id), visit)?;
            if !keep_reading {
                more = false;
                break;
            }
            if replaced {
                continue;
            }
            admitted = Some(admit_row(row, self.table, self.control)?);
            if !visit_stored_row(row, self.table, &mut body, visit)? {
                more = false;
                break;
            }
        }
        drop(rows);
        drop(statement);
        drop(body);
        drop(admitted);
        if more {
            changes.before(None, visit)?;
        }
        self.control.check().map_err(VersionError::from)?;
        Ok(())
    }
}
