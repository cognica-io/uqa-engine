//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consecutive point identities merge one bounded physical cursor with private replacements.

use super::{admit_row, visit_stored_row, LatestDocuments, BODY, INLINE_BODY_BYTES};
use crate::{connection::Result, mvcc::PhysicalResult, read_control::reserve_bindings};
use rusqlite::{params, types::ValueRef};
use uqa_storage::mvcc::VersionError;

const RANGE: &str = "SELECT doc_id, octet_length(body), CASE WHEN octet_length(body) <= ?4 THEN body END, tuple_xmin FROM _documents WHERE table_name = ?1 AND doc_id BETWEEN ?2 AND ?3 ORDER BY doc_id";
type PointVisitor<'a> = dyn FnMut(i64, Option<&[ValueRef<'_>]>) -> Result<bool> + 'a;

impl LatestDocuments<'_> {
    /// Visit every identity in the inclusive range, including missing rows, until the callback stops. The callback must not reenter persistence; large bodies are admitted only after all preceding identities have been accepted.
    pub(crate) fn visit_consecutive(
        &self,
        first: i64,
        last: i64,
        visit: &mut PointVisitor<'_>,
    ) -> Result<()> {
        self.consecutive(first, last, visit)
            .map_err(|error| error.into_version().into())
    }

    fn consecutive(
        &self,
        first: i64,
        last: i64,
        visit: &mut PointVisitor<'_>,
    ) -> PhysicalResult<()> {
        let _bindings = reserve_bindings(self.control, &[self.table.as_bytes()])?;
        let mut private = self.private_documents(first.checked_sub(1))?;
        // Keep the row's allowance until stepping or finalization releases SQLite's buffers.
        let mut admitted = None;
        let mut body = self.connection.prepare_cached(BODY)?;
        let mut statement = self.connection.prepare_cached(RANGE)?;
        let mut rows = statement.query(params![
            self.table,
            first,
            last,
            i64::from(INLINE_BODY_BYTES)
        ])?;
        let mut stored = rows.next()?;
        let mut consumed = false;
        for id in first..=last {
            self.control.check().map_err(VersionError::from)?;
            if consumed {
                stored = rows.next()?;
                consumed = false;
            }
            let stored_id = stored.map(|row| row.get::<_, i64>(0)).transpose()?;
            if let Some(private) = private
                .as_mut()
                .filter(|private| private.peek() == Some(id))
            {
                let more = if private.live()? {
                    private
                        .visit(&mut |row| visit(id, Some(row)))?
                        .expect("live private row")
                } else {
                    visit(id, None)?
                };
                if !more {
                    break;
                }
                private.advance()?;
                consumed = stored_id == Some(id);
            } else if let Some(row) = stored.filter(|_| stored_id == Some(id)) {
                admitted = Some(admit_row(row, self.table, self.control)?);
                if !visit_stored_row(row, self.table, &mut body, &mut |values| {
                    visit(id, Some(values))
                })? {
                    break;
                }
                consumed = true;
            } else if !visit(id, None)? {
                break;
            }
        }
        drop(rows);
        drop(statement);
        drop(body);
        drop(admitted);
        self.control.check().map_err(VersionError::from)?;
        Ok(())
    }
}
