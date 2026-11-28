//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-ordered point projections share one read window until a selected BLOB needs hydration.

use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedVec, DocId, Value};
use uqa_storage::{document_store::decoding::ProjectedDocumentFields, mvcc::VersionError};

use super::positional::has_external_values;
use super::{sqlite_doc_id, Family, NativeDocumentRead, SQLiteResult};
use crate::mvcc::native::{decode_record, NativeRecordIdentity, NativeRecordOwner};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PointAccess {
    Borrowed,
    Reentrant,
}

impl NativeDocumentRead<'_> {
    pub(in crate::document_store) fn visit_borrowed_points(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> SQLiteResult<usize> {
        self.visit_points(ids, fields, PointAccess::Borrowed, visitor)
    }

    pub(super) fn visit_points(
        &self,
        ids: &[DocId],
        fields: &[&str],
        access: PointAccess,
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> SQLiteResult<usize> {
        self.snapshot.control.check()?;
        self.control.check()?;
        let mut slots = BudgetedVec::new(self.control.memory());
        let Some(owner) = self.owner else {
            let mut visited = 0;
            for id in ids {
                self.snapshot.control.check()?;
                self.control.check()?;
                sqlite_doc_id(*id)?;
                visited += 1;
                if !self.visit_point_fields(*id, None, fields, &mut slots, visitor)? {
                    break;
                }
            }
            return Ok(visited);
        };
        let mut visited = 0;
        if access == PointAccess::Borrowed {
            let (count, finished) =
                self.visit_latest_points(owner, ids, fields, &mut slots, visitor)?;
            if finished {
                return Ok(count);
            }
            visited = count;
        }
        let identity = NativeRecordIdentity::new(Family::Documents, owner)?;
        let mut keys = ids[visited..].iter().map(|id| {
            let id = sqlite_doc_id(*id).map_err(|error| VersionError::Storage(error.into()))?;
            identity.encode_key(&[ValueRef::Integer(id)], self.control)
        });
        let mut selected = self.snapshot.view.selected(self.control);
        while visited < ids.len() {
            let mut pending = None;
            selected.visit_values(&mut keys, self.control, &mut |key, record| {
                self.snapshot.control.check()?;
                self.control.check()?;
                let id = ids[visited];
                let row = if let Some(bytes) = record.and_then(|record| record.value) {
                    let (_, row) = decode_record(key, bytes, self.control)?;
                    Some(
                        self.decode_positional_body(id, &row, fields)
                            .map_err(|error| VersionError::Storage(error.into()))?,
                    )
                } else {
                    None
                };
                if access == PointAccess::Reentrant
                    || row.as_deref().is_some_and(has_external_values)
                {
                    pending = Some((id, row));
                    return Ok(false);
                }
                let more = self
                    .visit_point_fields(id, row.as_deref(), fields, &mut slots, visitor)
                    .map_err(|error| VersionError::Storage(error.into()))?;
                visited += 1;
                Ok(more)
            })?;
            let Some((id, row)) = pending else { break };
            // Release the physical window for BLOB hydration and for callbacks that may reenter the same database.
            let row = row
                .map(|row| self.hydrate_positional(id, row, fields))
                .transpose()?;
            let more = self.visit_point_fields(id, row.as_deref(), fields, &mut slots, visitor)?;
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
        slots: &mut BudgetedVec<&'static Value>,
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
                                .map(|row| self.decode_positional_body(id, row, fields))
                                .transpose()?;
                            if row.as_ref().is_some_and(|row| has_external_values(row)) {
                                pending = row;
                                return Ok(false);
                            }
                            let more = self.visit_point_fields(
                                id,
                                row.as_deref(),
                                fields,
                                slots,
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
            let row = self.hydrate_positional(id, row, fields)?;
            let more = self.visit_point_fields(id, Some(&row), fields, slots, visitor)?;
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
        row: Option<&ProjectedDocumentFields>,
        fields: &[&str],
        slots: &mut BudgetedVec<&'static Value>,
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> SQLiteResult<bool> {
        self.snapshot.control.check()?;
        self.control.check()?;
        let mut projected = std::mem::replace(slots, BudgetedVec::new(self.control.memory()));
        projected.reserve(fields.len())?;
        for position in 0..fields.len() {
            projected.push(
                row.and_then(|row| row.get(position))
                    .unwrap_or(&Value::Null),
            )?;
        }
        let more = visitor(id, row.is_some(), &projected);
        *slots = projected.clear_references();
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok(more)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod dense_tests;
