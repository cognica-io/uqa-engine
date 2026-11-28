//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document bodies and selected BLOBs retain one decoding allowance through their readers.

use rusqlite::types::ValueRef;
use uqa_core::{
    json::JsonReadError,
    memory::{Budgeted, BudgetedVec, MemoryReservation},
    Value,
};
use uqa_storage::{
    document_store::{decoding::decode_legacy_document_text_fields_budgeted, Document},
    DocumentMetadata, RetainedDocumentFields, RetainedStoredDocument,
};

use super::NativeDocumentRead;
use crate::document_store::{controlled, sqlite_doc_id, DocId, SQLiteError, SQLiteResult};
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordIdentity};

mod borrowed;
mod points;
mod positional;
mod presence;

#[cfg(test)]
mod tests;

struct Decoded {
    fields: Document,
    metadata: DocumentMetadata,
    memory: MemoryReservation,
}

impl NativeDocumentRead<'_> {
    pub(in crate::document_store) fn retained_many(
        &self,
        ids: &[DocId],
        projection: Option<&[&str]>,
    ) -> SQLiteResult<uqa_storage::RetainedDocumentPage> {
        self.snapshot.control.check()?;
        self.control.check()?;
        let mut page = BudgetedVec::new(self.control.memory());
        page.reserve(ids.len())?;
        for id in ids {
            self.snapshot.control.check()?;
            self.control.check()?;
            page.push(self.retained(*id, projection)?)?;
        }
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok(page)
    }

    fn decoded_body(&self, id: DocId) -> SQLiteResult<Option<Decoded>> {
        let key = sqlite_doc_id(id)?;
        let Some(owner) = self.owner else {
            return Ok(None);
        };
        self.snapshot.borrow_row_controlled(
            Family::Documents,
            owner,
            &[ValueRef::Integer(key)],
            self.control,
            |row| self.decode_body(id, row),
        )
    }

    fn decode_body(&self, id: DocId, row: &[ValueRef<'_>]) -> SQLiteResult<Decoded> {
        let body = row[2]
            .as_str()
            .map_err(|_| SQLiteError::StorageBackend("native document body must be text".into()))?;
        let fields = decode_legacy_document_text_fields_budgeted(body, self.control)?;
        let metadata = super::read::metadata(row[3], self.table, id)?;
        let (fields, memory) = fields.into_parts();
        Ok(Decoded {
            fields,
            metadata,
            memory,
        })
    }

    pub(super) fn retained_body(&self, id: DocId) -> SQLiteResult<Option<RetainedStoredDocument>> {
        self.decoded_body(id)?
            .map(|row| self.finish(row))
            .transpose()
    }

    pub(super) fn retained(
        &self,
        id: DocId,
        projection: Option<&[&str]>,
    ) -> SQLiteResult<Option<RetainedStoredDocument>> {
        let Some(row) = self.decoded_body(id)? else {
            return Ok(None);
        };
        self.hydrate_decoded(id, row, projection).map(Some)
    }

    fn selection_key(&self, id: DocId) -> SQLiteResult<Option<BudgetedVec<u8>>> {
        let id = sqlite_doc_id(id)?;
        let Some(owner) = self.owner else {
            return Ok(None);
        };
        Ok(Some(
            NativeRecordIdentity::new(Family::Documents, owner)?
                .encode_key(&[ValueRef::Integer(id)], self.control)?,
        ))
    }

    fn hydrate_decoded(
        &self,
        id: DocId,
        row: Decoded,
        projection: Option<&[&str]>,
    ) -> SQLiteResult<RetainedStoredDocument> {
        let fields = controlled::rows::hydrate_fields(
            Budgeted::new(row.fields, row.memory),
            projection,
            self.control,
            |field, marker| self.hydrate_retained(id, field, marker),
        )?;
        self.snapshot.control.check()?;
        self.control.check()?;
        Ok(RetainedStoredDocument::with_metadata(fields, row.metadata))
    }

    fn finish(&self, row: Decoded) -> SQLiteResult<RetainedStoredDocument> {
        let fields = RetainedDocumentFields::from_budgeted(
            Budgeted::new(row.fields, row.memory),
            self.control,
        )?;
        Ok(RetainedStoredDocument::with_metadata(fields, row.metadata))
    }

    fn hydrate_retained(
        &self,
        id: DocId,
        field: &str,
        marker: controlled::Marker<'_>,
    ) -> SQLiteResult<Budgeted<Value>> {
        if marker.field != field {
            return Err(controlled::corrupt(
                self.table,
                id,
                field,
                "JSON marker references a different blob field",
            ));
        }
        let owner = self.owner.expect("a decoded body has a native owner");
        self.snapshot
            .read_row_controlled(
                Family::DocumentBlobs,
                owner,
                &[
                    ValueRef::Integer(sqlite_doc_id(id)?),
                    ValueRef::Text(field.as_bytes()),
                ],
                self.control,
                |row| {
                    let bytes = row[3].as_blob().map_err(|_| {
                        SQLiteError::StorageBackend("native document BLOB must be binary".into())
                    })?;
                    controlled::decode_blob(bytes, marker, self.control).map_err(
                        |error| match error {
                            JsonReadError::InvalidJson => {
                                controlled::corrupt(self.table, id, field, marker.invalid_reason())
                            }
                            error => controlled::read_error(error),
                        },
                    )
                },
            )?
            .ok_or_else(|| {
                controlled::corrupt(
                    self.table,
                    id,
                    field,
                    "JSON marker references a missing blob row",
                )
            })
    }

    pub(in crate::document_store) fn visit_projection(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> SQLiteResult<()> {
        self.snapshot.control.check()?;
        self.control.check()?;
        if fields.is_empty() {
            return self.visit_presence(ids, visitor);
        }
        self.visit_points(ids, fields, points::PointAccess::Reentrant, visitor)?;
        Ok(())
    }
}
