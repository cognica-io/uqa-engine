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
use crate::mvcc::native::{decode_record, NativeRecordIdentity, NativeRecordOwner};

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
        let (mut visited, finished) = self.visit_latest_points(owner, ids, fields, visitor)?;
        if finished {
            return Ok(visited);
        }
        let identity = NativeRecordIdentity::new(Family::Documents, owner)?;
        let mut keys = ids[visited..].iter().map(|id| {
            let id = sqlite_doc_id(*id).map_err(|error| VersionError::Storage(error.into()))?;
            identity.encode_key(&[ValueRef::Integer(id)], self.control)
        });
        let mut selected = self.snapshot.view.selected(self.control);
        while visited < ids.len() {
            let mut pending: Option<Decoded> = None;
            selected.visit_values(&mut keys, self.control, &mut |key, record| {
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
                    .visit_point_fields(id, row.as_ref().map(|row| &row.fields), fields, visitor)
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

    /// A consecutive selection can stream one bounded physical cursor. Other orders and old snapshots keep the lazy selected-record reader.
    fn visit_latest_points(
        &self,
        owner: NativeRecordOwner,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> SQLiteResult<(usize, bool)> {
        if ids.len() < 8
            || ids[ids.len() - 1] > i64::MAX as u64
            || ids
                .windows(2)
                .any(|pair| pair[0].checked_add(1) != Some(pair[1]))
        {
            return Ok((0, false));
        }
        let mut visited = 0;
        while visited < ids.len() {
            let mut pending = None;
            let latest = self.snapshot.read_latest_documents(
                self.table,
                owner,
                self.control,
                &mut |latest| {
                    latest.visit_consecutive(
                        ids[visited] as i64,
                        ids[ids.len() - 1] as i64,
                        &mut |id, row| {
                            self.snapshot.control.check()?;
                            self.control.check()?;
                            let id = id as DocId;
                            let row = row
                                .map(|row| self.decode_body_with_projection(id, row, Some(fields)))
                                .transpose()?;
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
                            let more = self.visit_point_fields(
                                id,
                                row.as_ref().map(|row| &row.fields),
                                fields,
                                visitor,
                            )?;
                            visited += 1;
                            Ok(more)
                        },
                    )
                },
            )?;
            if latest.is_none() {
                return Ok((visited, false));
            }
            let Some(row) = pending else {
                return Ok((visited, true));
            };
            // BLOB hydration releases the physical cursor. Recheck the snapshot boundary before the next segment.
            let id = ids[visited];
            let row = self.hydrate_decoded(id, row, Some(fields))?;
            let more = self.visit_point_fields(id, Some(row.fields()), fields, visitor)?;
            visited += 1;
            if !more {
                break;
            }
        }
        Ok((visited, true))
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

#[cfg(test)]
mod dense_tests;
