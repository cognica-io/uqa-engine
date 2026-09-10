//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construct physical mutation rows, including generated values and tuple identity columns.

pub mod context;
use crate::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema};
use context::MutationRowContext;
use std::collections::BTreeSet;
use uqa_core::{DocId, Value};
use uqa_sql::{
    semantics::{doc_id_value, DOC_ID_COLUMN, TABLE_OID_COLUMN, XMIN_COLUMN},
    ColumnType, SQLError,
};
use uqa_storage::document_store::Document;
fn dml_storage_error(action: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {error}"))
}

pub fn is_virtual_document_id_column(
    column: &str,
    definitions: &[uqa_sql::ast::ColumnDef],
) -> bool {
    column == DOC_ID_COLUMN
        && !definitions
            .iter()
            .any(|definition| definition.name == DOC_ID_COLUMN)
}

pub fn target_row(
    context: MutationRowContext<'_>,
    table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    target_row_for_storage(context, table, table, qualifier, doc_id, document)
}

pub fn target_row_for_storage(
    context: MutationRowContext<'_>,
    table: &str,
    storage_table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    target_row_for_storage_optional(
        context,
        table,
        Some(storage_table),
        qualifier,
        Some(doc_id),
        document,
        None,
    )
}

pub fn target_row_for_storage_optional(
    context: MutationRowContext<'_>,
    table: &str,
    storage_table: Option<&str>,
    qualifier: &str,
    doc_id: Option<DocId>,
    document: &Document,
    selected_columns: Option<&BTreeSet<String>>,
) -> Result<OwnedPhysicalRow, SQLError> {
    let metadata = match (storage_table, doc_id) {
        (Some(storage_table), Some(doc_id)) => context
            .tuples
            .document_metadata(storage_table, doc_id)?
            .unwrap_or_default(),
        _ => uqa_storage::DocumentMetadata::default(),
    };
    target_row_for_storage_optional_with_metadata(
        context,
        table,
        storage_table,
        qualifier,
        doc_id,
        document,
        selected_columns,
        metadata,
    )
}

pub fn existing_tuple_metadata(
    context: MutationRowContext<'_>,
    table: &str,
    doc_id: DocId,
) -> Result<uqa_storage::DocumentMetadata, SQLError> {
    context
        .tuples
        .document_metadata(table, doc_id)?
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "document `{table}` row {doc_id} has no tuple metadata"
            ))
        })
}

pub fn new_tuple_metadata(
    context: MutationRowContext<'_>,
) -> Result<uqa_storage::DocumentMetadata, SQLError> {
    Ok(uqa_storage::DocumentMetadata::with_tuple_xmin(
        context.tuples.tuple_version_xid()?,
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps mutation identity and tuple metadata explicit"
)]
pub fn target_row_for_storage_optional_with_metadata(
    context: MutationRowContext<'_>,
    table: &str,
    storage_table: Option<&str>,
    qualifier: &str,
    doc_id: Option<DocId>,
    document: &Document,
    selected_columns: Option<&BTreeSet<String>>,
    metadata: uqa_storage::DocumentMetadata,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = context
        .relations
        .column_definitions(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut materialized = document.clone();
    if let Some(selected_columns) = selected_columns {
        crate::query::generated::materialize_selected_virtual_generated_columns(
            &definitions,
            &mut materialized,
            selected_columns,
        )?;
    } else {
        crate::query::generated::materialize_virtual_generated_columns(
            &definitions,
            &mut materialized,
        )?;
    }
    let mut columns = if definitions.is_empty() {
        materialized
            .keys()
            .filter(|column| column.as_str() != XMIN_COLUMN)
            .cloned()
            .collect::<Vec<_>>()
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(ColumnType::Oid));
    columns.push(XMIN_COLUMN.into());
    types.push(Some(ColumnType::Xid));
    let values = columns
        .iter()
        .map(|column| {
            if is_virtual_document_id_column(column, &definitions)
                || definitions.iter().any(|definition| {
                    definition.name == *column
                        && definition.primary_key
                        && definition.ty.is_integer()
                })
            {
                doc_id.map_or(Ok(Value::Null), doc_id_value)
            } else if column == TABLE_OID_COLUMN {
                storage_table.map_or(Ok(Value::Null), |storage_table| {
                    Ok(Value::Int(crate::catalog::projection::table_relation_oid(
                        &context.catalog,
                        storage_table,
                    )?))
                })
            } else if column == XMIN_COLUMN {
                Ok(crate::query::document_projection::project_document_column(
                    &materialized,
                    metadata,
                    column,
                    &definitions,
                ))
            } else {
                Ok(materialized.get(column).cloned().unwrap_or(Value::Null))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::from_values(values),
    ))
}

pub fn new_target_row_for_storage(
    context: MutationRowContext<'_>,
    table: &str,
    storage_table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    target_row_for_storage_optional_with_metadata(
        context,
        table,
        Some(storage_table),
        qualifier,
        Some(doc_id),
        document,
        None,
        uqa_storage::DocumentMetadata::with_tuple_xmin(context.tuples.tuple_version_xid()?),
    )
}

pub fn null_target_row(
    context: MutationRowContext<'_>,
    table: &str,
    qualifier: &str,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = context
        .relations
        .column_definitions(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut columns = if definitions.is_empty() {
        context
            .relations
            .column_names(table)
            .map_err(|error| dml_storage_error("DML row schema lookup", error))?
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    columns.push(TABLE_OID_COLUMN.into());
    types.push(Some(ColumnType::Oid));
    columns.push(XMIN_COLUMN.into());
    types.push(Some(ColumnType::Xid));
    let width = columns.len();
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::nulls(width),
    ))
}

pub fn join_rows(left: &OwnedPhysicalRow, right: &OwnedPhysicalRow) -> OwnedPhysicalRow {
    OwnedPhysicalRow::new(
        RowSchema::join(&left.schema, &right.schema, std::iter::empty()),
        PhysicalRow::concat(&left.row, &right.row),
    )
}

pub fn append_hidden_qualified_row(
    base: &OwnedPhysicalRow,
    qualifier: &str,
    columns: &[String],
    types: &[Option<ColumnType>],
    values: Vec<Value>,
) -> OwnedPhysicalRow {
    let hidden_types = columns
        .iter()
        .enumerate()
        .map(|(position, _)| types.get(position).cloned().flatten())
        .collect::<Vec<_>>();
    let schema = RowSchema::append_hidden_typed(&base.schema, &hidden_types);
    let offset = base.schema.physical_width();
    let aliases = columns
        .iter()
        .enumerate()
        .map(|(position, column)| {
            (
                ColumnIdentity::qualified(qualifier, column),
                offset + position,
                hidden_types[position].clone(),
            )
        })
        .collect::<Vec<_>>();
    OwnedPhysicalRow::new(
        RowSchema::with_physical_identity_aliases(&schema, &aliases),
        base.row.clone().append_values(values),
    )
}
