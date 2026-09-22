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
    document_store::{decoding::decode_legacy_document_fields_budgeted, Document},
    DocumentMetadata, RetainedDocumentFields, RetainedStoredDocument,
};

use super::NativeDocumentRead;
use crate::document_store::{controlled, sqlite_doc_id, DocId, SQLiteError, SQLiteResult};
use crate::mvcc::native::NativeRecordFamily as Family;

struct Decoded {
    fields: Document,
    metadata: DocumentMetadata,
    memory: MemoryReservation,
}

impl NativeDocumentRead<'_> {
    fn decoded_body(&self, id: DocId) -> SQLiteResult<Option<Decoded>> {
        let key = sqlite_doc_id(id)?;
        let Some(owner) = self.owner else {
            return Ok(None);
        };
        self.snapshot
            .read_row(Family::Documents, owner, &[ValueRef::Integer(key)], |row| {
                let body = row[2].as_str().map_err(|_| {
                    SQLiteError::StorageBackend("native document body must be text".into())
                })?;
                let fields = decode_legacy_document_fields_budgeted(
                    body.as_bytes(),
                    &self.snapshot.control,
                )?;
                let metadata = super::read::metadata(row[3], self.table, id)?;
                let (fields, memory) = fields.into_parts();
                Ok(Decoded {
                    fields,
                    metadata,
                    memory,
                })
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
        let Some(mut row) = self.decoded_body(id)? else {
            return Ok(None);
        };
        for (field, value) in &mut row.fields {
            self.snapshot.control.check()?;
            if projection.is_some_and(|fields| !fields.contains(&field.as_str())) {
                continue;
            }
            let Some(marker) = controlled::marker(value) else {
                continue;
            };
            let replacement = self.hydrate_retained(id, field, marker)?;
            let replaced_bytes = value
                .retained_payload_bytes(
                    self.snapshot.control.memory(),
                    self.snapshot.control.cancellation(),
                )
                .map_err(|error| {
                    controlled::read_error(match error {
                        uqa_core::ValueRetentionError::Memory(error) => {
                            JsonReadError::Memory(error)
                        }
                        uqa_core::ValueRetentionError::Cancelled(error) => {
                            JsonReadError::Cancelled(error)
                        }
                    })
                })?;
            let (replacement, memory) = replacement.into_parts();
            *value = replacement;
            drop(row.memory.split(replaced_bytes));
            row.memory.absorb(memory);
        }
        self.finish(row).map(Some)
    }

    fn finish(&self, row: Decoded) -> SQLiteResult<RetainedStoredDocument> {
        let fields = RetainedDocumentFields::from_budgeted(
            Budgeted::new(row.fields, row.memory),
            &self.snapshot.control,
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
            .read_row(
                Family::DocumentBlobs,
                owner,
                &[
                    ValueRef::Integer(sqlite_doc_id(id)?),
                    ValueRef::Text(field.as_bytes()),
                ],
                |row| {
                    let bytes = row[3].as_blob().map_err(|_| {
                        SQLiteError::StorageBackend("native document BLOB must be binary".into())
                    })?;
                    controlled::decode_blob(bytes, marker, &self.snapshot.control).map_err(
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
        for id in ids {
            self.snapshot.control.check()?;
            let document = if fields.is_empty() {
                None
            } else {
                self.retained(*id, Some(fields))?
            };
            let present = if fields.is_empty() {
                self.contains(*id)?
            } else {
                document.is_some()
            };
            let mut projected = BudgetedVec::new(self.snapshot.control.memory());
            projected.reserve(fields.len())?;
            for field in fields {
                projected.push(
                    document
                        .as_ref()
                        .and_then(|row| row.fields().get(*field))
                        .unwrap_or(&Value::Null),
                )?;
            }
            // Native row visitors have returned, so the callback can safely reenter persistence while this immutable snapshot and its decoded row stay alive.
            let keep_going = visitor(*id, present, &projected);
            self.snapshot.control.check()?;
            if !keep_going {
                break;
            }
        }
        Ok(())
    }
}
