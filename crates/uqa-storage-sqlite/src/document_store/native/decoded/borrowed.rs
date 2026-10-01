//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline rows stream on one cursor, over the latest committed physical rows when they stand in for the snapshot's records and over the records otherwise; selected BLOBs hydrate after releasing it.

use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedVec, DocId, Value};
use uqa_storage::mvcc::VersionError;

use super::{controlled, sqlite_doc_id, Decoded, Family, NativeDocumentRead, SQLiteResult};
use crate::document_store::document_id_from_sqlite;
use crate::mvcc::native::{decode_record, NativeRecordIdentity};

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
        let available = self.snapshot.visit_latest_documents(
            self.table,
            owner,
            after,
            self.control,
            &mut |row| {
                self.snapshot.control.cancellation().check()?;
                self.control.cancellation().check()?;
                let id = row[1].as_i64().map_err(|_| {
                    VersionError::InvalidEncoding("native document key must be integer")
                })?;
                let id = document_id_from_sqlite(id)?;
                let row = self.decode_body_with_projection(id, row, Some(fields))?;
                if fields.iter().any(|field| {
                    row.fields
                        .get(*field)
                        .is_some_and(|value| controlled::marker(value).is_some())
                }) {
                    pending = Some((id, row));
                    return Ok(false);
                }
                let more = self.visit_decoded_fields(id, &row.fields, fields, visitor)?;
                visited += 1;
                self.snapshot.control.cancellation().check()?;
                self.control.cancellation().check()?;
                Ok(more && visited < limit)
            },
        )?;
        Ok(available.map(|()| (visited, pending)))
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
        let mut projected = BudgetedVec::new(self.control.memory());
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
