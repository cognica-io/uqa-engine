//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Project stored fields and transaction metadata through a SQL row type.

use std::collections::BTreeMap;
use uqa_core::{DocId, Value};
use uqa_sql::semantics::XMIN_COLUMN;
use uqa_sql::{ast::ColumnDef, SQLError};
use uqa_storage::{document_store::Document, DocumentMetadata, DocumentStore, StoredDocument};

/// Read only requested fields and tuple metadata. Complete rows are needed only when a requested virtual generated expression must be evaluated.
pub fn read_document_projection(
    source: &dyn DocumentStore,
    ids: &[DocId],
    fields: &[&str],
    definitions: &[ColumnDef],
) -> Result<BTreeMap<DocId, Vec<Value>>, SQLError> {
    let requested = fields
        .iter()
        .map(|field| (*field).to_string())
        .collect::<Vec<_>>();
    if super::generated::projection_contains_virtual_generated_column(definitions, &requested) {
        let documents = source.get_stored_many(ids).map_err(|error| {
            crate::storage_errors::storage_error("read generated document projection", &error)
        })?;
        return documents
            .into_iter()
            .map(|(id, mut document)| {
                super::generated::materialize_projected_virtual_generated_columns(
                    definitions,
                    document.fields_mut(),
                    &requested,
                )?;
                let values = fields
                    .iter()
                    .map(|field| project_stored_document_column(&document, field, definitions))
                    .collect();
                Ok((id, values))
            })
            .collect();
    }
    let mut rows = source.get_fields_multi(ids, fields).map_err(|error| {
        crate::storage_errors::storage_error("read document projection", &error)
    })?;
    if !projections_use_tuple_xmin(&requested, definitions) {
        return Ok(rows);
    }
    for (id, values) in &mut rows {
        if values.len() != fields.len() {
            return Err(SQLError::Internal(format!(
                "document {id} returned {} fields for a {}-field projection",
                values.len(),
                fields.len(),
            )));
        }
        let explicit = if definitions.is_empty() {
            source.get_field(*id, XMIN_COLUMN).map_err(|error| {
                crate::storage_errors::storage_error("read dynamic xmin field", &error)
            })?
        } else {
            None
        };
        let xmin = if let Some(value) = explicit {
            value
        } else {
            source
                .get_metadata(*id)
                .map_err(|error| {
                    crate::storage_errors::storage_error("read projected tuple metadata", &error)
                })?
                .and_then(DocumentMetadata::tuple_xmin)
                .map_or(Value::Null, |xmin| Value::Int(i64::from(xmin)))
        };
        for (field, value) in fields.iter().zip(values) {
            if *field == XMIN_COLUMN {
                *value = xmin.clone();
            }
        }
    }
    Ok(rows)
}

pub fn projection_uses_tuple_xmin(column: &str, definitions: &[uqa_sql::ast::ColumnDef]) -> bool {
    column == XMIN_COLUMN
        && !definitions
            .iter()
            .any(|definition| definition.name == XMIN_COLUMN)
}

pub fn projections_use_tuple_xmin(
    columns: &[String],
    definitions: &[uqa_sql::ast::ColumnDef],
) -> bool {
    columns
        .iter()
        .any(|column| projection_uses_tuple_xmin(column, definitions))
}

pub fn project_document_column(
    document: &Document,
    metadata: DocumentMetadata,
    column: &str,
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Value {
    if !projection_uses_tuple_xmin(column, definitions) {
        return document.get(column).cloned().unwrap_or(Value::Null);
    }
    if definitions.is_empty() {
        if let Some(value) = document.get(column) {
            return value.clone();
        }
    }
    metadata
        .tuple_xmin()
        .map_or(Value::Null, |xmin| Value::Int(i64::from(xmin)))
}

pub fn project_stored_document_column(
    document: &StoredDocument,
    column: &str,
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Value {
    project_document_column(document.fields(), document.metadata(), column, definitions)
}
