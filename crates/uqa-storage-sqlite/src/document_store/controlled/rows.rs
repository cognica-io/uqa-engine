//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete document reads preserve one physical/native view and the invoking payload allowance.

use rusqlite::{params, Connection};
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId, Value,
};
use uqa_storage::{
    document_store::{decoding::decode_legacy_document_fields_budgeted, Document},
    read_control::StorageReadControl,
    DocumentMetadata, RetainedDocumentFields, RetainedDocumentPage, RetainedStoredDocument,
};

use super::{corrupt, decode_blob, marker, read_error, retention_error, Marker};
use crate::document_store::{sqlite_doc_id, SQLiteDocumentStore, SQLiteError, SQLiteResult};

impl SQLiteDocumentStore {
    pub(in crate::document_store) fn read_rows_controlled(
        &self,
        ids: &[DocId],
        control: &StorageReadControl,
    ) -> SQLiteResult<RetainedDocumentPage> {
        control.check()?;
        if ids.is_empty() {
            return Ok(BudgetedVec::new(control.memory()));
        }
        if let Some(page) =
            self.read_native_with_control(Some(control), |read| read.retained_many(ids))?
        {
            return Ok(page);
        }
        let (page, _) = self.conn.with_snapshot(|connection, _| {
            control.check()?;
            let mut page = BudgetedVec::new(control.memory());
            page.reserve(ids.len())?;
            for id in ids {
                control.check()?;
                page.push(read_legacy(connection, &self.table, *id, control)?)?;
            }
            control.check()?;
            Ok(page)
        })?;
        control.check()?;
        Ok(page)
    }
}

fn read_legacy(
    connection: &Connection,
    table: &str,
    id: DocId,
    control: &StorageReadControl,
) -> SQLiteResult<Option<RetainedStoredDocument>> {
    let mut statement = connection.prepare_cached(
        "SELECT body, tuple_xmin FROM _documents WHERE table_name = ?1 AND doc_id = ?2",
    )?;
    let mut rows = statement.query(params![table, sqlite_doc_id(id)?])?;
    let Some(row) = rows.next()? else {
        control.check()?;
        return Ok(None);
    };
    control.check()?;
    let body = row
        .get_ref(0)?
        .as_str()
        .map_err(|_| SQLiteError::StorageBackend("document body must be text".into()))?;
    let fields = decode_legacy_document_fields_budgeted(body.as_bytes(), control)?;
    let metadata = match row.get::<_, Option<i64>>(1)? {
        None => DocumentMetadata::default(),
        Some(xmin) => DocumentMetadata::with_tuple_xmin(u32::try_from(xmin).map_err(|_| {
            SQLiteError::StorageBackend("document tuple xmin is outside the u32 range".into())
        })?),
    };
    let fields = hydrate_fields(fields, None, control, |field, marker| {
        if marker.field != field {
            return Err(corrupt(
                table,
                id,
                field,
                "JSON marker references a different blob field",
            ));
        }
        let mut statement = connection.prepare_cached(
            "SELECT bytes FROM _document_blobs WHERE table_name = ?1 AND doc_id = ?2 AND field_name = ?3",
        )?;
        let mut rows = statement.query(params![table, sqlite_doc_id(id)?, field])?;
        let Some(row) = rows.next()? else {
            return Err(corrupt(
                table,
                id,
                field,
                "JSON marker references a missing blob row",
            ));
        };
        let bytes = row
            .get_ref(0)?
            .as_blob()
            .map_err(|_| corrupt(table, id, field, "document BLOB must be binary"))?;
        decode_blob(bytes, marker, control).map_err(|error| match error {
            uqa_core::json::JsonReadError::InvalidJson => {
                corrupt(table, id, field, marker.invalid_reason())
            }
            error => read_error(error),
        })
    })?;
    control.check()?;
    Ok(Some(RetainedStoredDocument::with_metadata(
        fields, metadata,
    )))
}

pub(in crate::document_store) fn hydrate_fields(
    fields: Budgeted<Document>,
    projection: Option<&[&str]>,
    control: &StorageReadControl,
    mut load: impl FnMut(&str, Marker<'_>) -> SQLiteResult<Budgeted<Value>>,
) -> SQLiteResult<RetainedDocumentFields> {
    struct Decoded {
        fields: Document,
        memory: uqa_core::memory::MemoryReservation,
    }
    let (fields, memory) = fields.into_parts();
    let mut decoded = Decoded { fields, memory };
    for (field, value) in &mut decoded.fields {
        control.check()?;
        if projection.is_some_and(|fields| !fields.contains(&field.as_str())) {
            continue;
        }
        let Some(marker) = marker(value) else {
            continue;
        };
        let replacement = load(field, marker)?;
        let replaced_bytes = value
            .retained_payload_bytes(control.memory(), control.cancellation())
            .map_err(|error| read_error(retention_error(error)))?;
        let (replacement, retained) = replacement.into_parts();
        *value = replacement;
        drop(decoded.memory.split(replaced_bytes));
        decoded.memory.absorb(retained);
    }
    control.check()?;
    Ok(RetainedDocumentFields::from_budgeted(
        Budgeted::new(decoded.fields, decoded.memory),
        control,
    )?)
}
