//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-ordered point projections share one read window until a selected BLOB needs hydration.

use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedVec, DocId, Value};
use uqa_storage::{document_store::Document, mvcc::VersionError};

use super::{controlled, sqlite_doc_id, Decoded, Family, NativeDocumentRead, SQLiteResult};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};

impl NativeDocumentRead<'_> {
    pub(in crate::document_store) fn visit_borrowed_points(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> SQLiteResult<usize> {
        self.snapshot.control.check()?;
        self.control.check()?;
        let Some(owner) = self.owner else {
            let mut visited = 0;
            self.visit_projection(ids, fields, &mut |id, present, values| {
                visited += 1;
                visitor(id, present, values)
            })?;
            return Ok(visited);
        };
        let identity = NativeRecordIdentity::new(Family::Documents, owner)?;
        let mut keys = ids.iter().map(|id| {
            let id = sqlite_doc_id(*id).map_err(|error| VersionError::Storage(error.into()))?;
            identity.encode_key(&[ValueRef::Integer(id)], self.control)
        });
        let mut visited = 0;
        while visited < ids.len() {
            let mut pending: Option<Decoded> = None;
            self.snapshot
                .view
                .visit_values(&mut keys, self.control, &mut |key, record| {
                    self.snapshot.control.check()?;
                    self.control.check()?;
                    let id = ids[visited];
                    let row = if let Some(bytes) = record.and_then(|record| record.value) {
                        let (_, row) = decode_record(key, bytes, self.control)?;
                        Some(
                            self.decode_body_with_projection(id, &row, Some(fields))
                                .map_err(|error| VersionError::Storage(error.into()))?,
                        )
                    } else {
                        None
                    };
                    if row.as_ref().is_some_and(|row| {
                        fields.iter().any(|field| {
                            row.fields
                                .get(*field)
                                .is_some_and(|value| controlled::marker(value).is_some())
                        })
                    }) {
                        pending = row;
                        return Ok(false);
                    }
                    let more = self
                        .visit_point_fields(
                            id,
                            row.as_ref().map(|row| &row.fields),
                            fields,
                            visitor,
                        )
                        .map_err(|error| VersionError::Storage(error.into()))?;
                    visited += 1;
                    Ok(more)
                })?;
            let Some(row) = pending else {
                break;
            };
            // Release the physical read window before resolving compound BLOB records on the same immutable view.
            let id = ids[visited];
            let row = self.hydrate_decoded(id, row, Some(fields))?;
            let more = self.visit_point_fields(id, Some(row.fields()), fields, visitor)?;
            visited += 1;
            if !more {
                break;
            }
        }
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok(visited)
    }

    fn visit_point_fields(
        &self,
        id: DocId,
        row: Option<&Document>,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> SQLiteResult<bool> {
        self.snapshot.control.check()?;
        self.control.check()?;
        let mut projected = BudgetedVec::new(self.control.memory());
        projected.reserve(fields.len())?;
        for field in fields {
            projected.push(row.and_then(|row| row.get(*field)).unwrap_or(&Value::Null))?;
        }
        let more = visitor(id, row.is_some(), &projected);
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok(more)
    }
}

#[cfg(test)]
mod tests;
