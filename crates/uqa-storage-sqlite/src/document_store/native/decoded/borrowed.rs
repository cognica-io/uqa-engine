//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline rows stream on one cursor, over the latest committed physical rows when they stand in for the snapshot's records and over the records otherwise; selected BLOBs hydrate after releasing it.

use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedSmallVec, DocId, Value};
use uqa_storage::mvcc::VersionError;

use super::{controlled, sqlite_doc_id, Decoded, Family, NativeDocumentRead, SQLiteResult};
use crate::document_store::decoded_columns::{CachedColumns, ColumnBuilder};
use crate::document_store::document_id_from_sqlite;
use crate::mvcc::native::{decode_record, NativeRecordIdentity, PrivateDocuments};

/// The rows one latest-commit read visited, and the row that ended it to hydrate fields stored outside its body.
type LatestSegment = (usize, Option<(DocId, Decoded)>);

impl NativeDocumentRead<'_> {
    pub(in crate::document_store) fn visit_borrowed_fields(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> SQLiteResult<usize> {
        self.snapshot.control.check()?;
        self.control.check()?;
        if fields.is_empty() {
            return self.visit_next_ids(after, limit, visitor);
        }
        if limit == 0 {
            return Ok(0);
        }
        let mut after = after.map(sqlite_doc_id).transpose()?;
        let Some(owner) = self.owner else {
            return Ok(0);
        };
        let mut visited = 0;
        while let Some((count, pending)) =
            self.latest_fields_segment(owner, after, limit - visited, fields, visitor)?
        {
            visited += count;
            let Some((id, row)) = pending else {
                self.snapshot.control.check()?;
                self.control.check()?;
                return Ok(visited);
            };
            // Compound reads and the callback run outside the physical read; the scan resumes in a new one.
            let row = self.hydrate_decoded(id, row, Some(fields))?;
            let more = self.visit_decoded_fields(id, row.fields(), fields, visitor)?;
            visited += 1;
            self.snapshot.control.check()?;
            self.control.check()?;
            if !more || visited >= limit {
                return Ok(visited);
            }
            after = Some(sqlite_doc_id(id)?);
        }
        let identity = NativeRecordIdentity::new(Family::Documents, owner)?;
        let prefix = identity.encode_prefix(&[], self.control)?;
        let mut after_key = after
            .map(|id| identity.encode_key(&[ValueRef::Integer(id)], self.control))
            .transpose()?;
        let mut more = true;
        while more && visited < limit {
            let mut pending: Option<(DocId, Decoded)> = None;
            self.snapshot.view.visit_prefix(
                &prefix,
                after_key.as_deref(),
                usize::MAX,
                self.control,
                &mut |key, record| {
                    self.snapshot.control.cancellation().check()?;
                    self.control.cancellation().check()?;
                    let Some(bytes) = record.value else {
                        return Ok(true);
                    };
                    let (_, row) = decode_record(key, bytes, self.control)?;
                    let id = row[1].as_i64().map_err(|_| {
                        VersionError::InvalidEncoding("native document key must be integer")
                    })?;
                    let id = document_id_from_sqlite(id)
                        .map_err(|error| VersionError::Storage(error.into()))?;
                    let row = self
                        .decode_body_with_projection(id, &row, Some(fields))
                        .map_err(|error| VersionError::Storage(error.into()))?;
                    if fields.iter().any(|field| {
                        row.fields
                            .get(*field)
                            .is_some_and(|value| controlled::marker(value).is_some())
                    }) {
                        pending = Some((id, row));
                        return Ok(false);
                    }
                    more = self
                        .visit_decoded_fields(id, &row.fields, fields, visitor)
                        .map_err(|error| VersionError::Storage(error.into()))?;
                    visited += 1;
                    self.snapshot.control.cancellation().check()?;
                    self.control.cancellation().check()?;
                    Ok(more && visited < limit)
                },
            )?;
            let Some((id, row)) = pending else {
                break;
            };
            // Compound reads and the callback now run outside the physical cursor. The immutable record boundary remains unchanged.
            let row = self.hydrate_decoded(id, row, Some(fields))?;
            more = self.visit_decoded_fields(id, row.fields(), fields, visitor)?;
            visited += 1;
            self.snapshot.control.check()?;
            self.control.check()?;
            if more && visited < limit {
                after_key = Some(
                    identity.encode_key(&[ValueRef::Integer(sqlite_doc_id(id)?)], self.control)?,
                );
            }
        }
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok(visited)
    }

    /// One physical read of the latest committed rows after `after`, or `None` when they cannot stand in for the snapshot's records. Returns the rows visited and the row that ended the read because its selected fields are stored outside its body.
    fn latest_fields_segment(
        &self,
        owner: crate::mvcc::native::NativeRecordOwner,
        after: Option<i64>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> SQLiteResult<Option<LatestSegment>> {
        let mut visited = 0;
        let mut pending = None;
        let available = self.snapshot.read_latest_documents(
            self.table,
            owner,
            self.control,
            &mut |latest| {
                let generation = latest.generation();
                let private = latest.private_documents(after)?;
                if let Some(cached) = self
                    .columns
                    .and_then(|cache| cache.get(self.table, owner, generation, fields))
                {
                    // Decoded columns hold the stored rows; the session's private records of the table are merged into them.
                    (visited, pending) = match private {
                        Some(private) => self.visit_cached_columns_merged(
                            &cached, private, after, limit, fields, visitor,
                        )?,
                        None => (
                            self.visit_cached_columns(&cached, after, limit, visitor)?,
                            None,
                        ),
                    };
                    return Ok(());
                }
                // A build starts at the table's first row and continues only with the page that resumes exactly where it stopped. Rows merged with private records are the session's own and are never recorded.
                let cache = self.columns.filter(|_| private.is_none());
                drop(private);
                let mut builder = cache.and_then(|cache| match after {
                    None => Some(ColumnBuilder::start(
                        cache, self.table, owner, generation, fields,
                    )),
                    Some(_) => cache.resume(self.table, owner, generation, fields, after),
                });
                let mut exhausted = true;
                let mut paged = false;
                latest.visit(after, &mut |row| {
                    self.snapshot.control.cancellation().check()?;
                    self.control.cancellation().check()?;
                    let stored_id = row[1].as_i64().map_err(|_| {
                        VersionError::InvalidEncoding("native document key must be integer")
                    })?;
                    let id = document_id_from_sqlite(stored_id)?;
                    let mut row = self.decode_body_with_projection(id, row, Some(fields))?;
                    if fields.iter().any(|field| {
                        row.fields
                            .get(*field)
                            .is_some_and(|value| controlled::marker(value).is_some())
                    }) {
                        pending = Some((id, row));
                        exhausted = false;
                        return Ok(false);
                    }
                    let more = self.visit_decoded_fields(id, &row.fields, fields, visitor)?;
                    visited += 1;
                    if let (Some(cache), Some(build)) = (self.columns, builder.as_mut()) {
                        if !build.push(cache, stored_id, &mut row.fields) {
                            builder = None;
                        }
                    }
                    self.snapshot.control.cancellation().check()?;
                    self.control.cancellation().check()?;
                    if !more {
                        exhausted = false;
                        return Ok(false);
                    }
                    if visited >= limit {
                        exhausted = false;
                        paged = true;
                        return Ok(false);
                    }
                    Ok(true)
                })?;
                if let (Some(cache), Some(build)) = (self.columns, builder) {
                    if exhausted {
                        cache.finish(build);
                    } else if paged {
                        cache.park(build);
                    }
                }
                Ok(())
            },
        )?;
        Ok(available.map(|()| (visited, pending)))
    }

    /// Serve a projection from columns decoded at the snapshot's own data generation, without reading or decoding rows.
    fn visit_cached_columns(
        &self,
        (ids, columns): &CachedColumns,
        after: Option<i64>,
        limit: usize,
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> SQLiteResult<usize> {
        let start = after.map_or(0, |after| ids.partition_point(|id| *id <= after));
        let mut row = BudgetedSmallVec::<[&Value; 8]>::new(self.control.memory());
        row.reserve(columns.len())?;
        let mut visited = 0;
        for (position, id) in ids.iter().enumerate().skip(start) {
            if visited >= limit {
                break;
            }
            self.snapshot.control.cancellation().check()?;
            self.control.cancellation().check()?;
            row.clear();
            for column in columns {
                row.push(&column[position])?;
            }
            visited += 1;
            if !visitor(document_id_from_sqlite(*id)?, &row) {
                break;
            }
        }
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok(visited)
    }

    /// Serve a projection from the stored rows' decoded columns, merging the session's private records of the table in document order: a private record before a cached row inserts a document, and one at a cached row replaces or deletes it. Returns the rows visited and a private row whose selected fields are stored outside its body, which ends the visit to hydrate outside the physical read.
    fn visit_cached_columns_merged(
        &self,
        (ids, columns): &CachedColumns,
        mut private: PrivateDocuments<'_>,
        after: Option<i64>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> SQLiteResult<LatestSegment> {
        let mut position = after.map_or(0, |after| ids.partition_point(|id| *id <= after));
        let mut row = BudgetedSmallVec::<[&Value; 8]>::new(self.control.memory());
        row.reserve(columns.len())?;
        let mut visited = 0;
        while visited < limit {
            self.snapshot.control.cancellation().check()?;
            self.control.cancellation().check()?;
            let stored = ids.get(position).copied();
            let next = private.peek();
            let Some(private_id) = next.filter(|id| stored.is_none_or(|stored| *id <= stored))
            else {
                let Some(stored) = stored else {
                    break;
                };
                row.clear();
                for column in columns {
                    row.push(&column[position])?;
                }
                position += 1;
                visited += 1;
                if !visitor(document_id_from_sqlite(stored)?, &row) {
                    break;
                }
                continue;
            };
            if stored == Some(private_id) {
                position += 1;
            }
            let id = document_id_from_sqlite(private_id)?;
            let mut decoded = None;
            private.visit(&mut |values| {
                decoded = Some(self.decode_body_with_projection(id, values, Some(fields))?);
                Ok(true)
            })?;
            private.advance()?;
            // A deletion has no row.
            let Some(decoded) = decoded else {
                continue;
            };
            if fields.iter().any(|field| {
                decoded
                    .fields
                    .get(*field)
                    .is_some_and(|value| controlled::marker(value).is_some())
            }) {
                return Ok((visited, Some((id, decoded))));
            }
            visited += 1;
            if !self.visit_decoded_fields(id, &decoded.fields, fields, visitor)? {
                break;
            }
        }
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok((visited, None))
    }

    fn visit_decoded_fields(
        &self,
        id: DocId,
        row: &uqa_storage::document_store::Document,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> SQLiteResult<bool> {
        self.snapshot.control.check()?;
        self.control.check()?;
        // Ordinary projections keep their value references inline.
        let mut projected = BudgetedSmallVec::<[&Value; 8]>::new(self.control.memory());
        projected.reserve(fields.len())?;
        for field in fields {
            projected.push(row.get(*field).unwrap_or(&Value::Null))?;
        }
        let more = visitor(id, &projected);
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok(more)
    }
}

#[cfg(test)]
mod tests;
