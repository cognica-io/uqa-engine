//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dimension-dependent option resolution and catalog-before-build scheduling.

use uqa_sql::{
    ast::{ColumnType, CreateIndex},
    schema::indexes::{
        keys::require_column_key,
        options::parse_diskann_index_options,
        vectors::{resolve_vector_index_target, VectorIndexCatalog},
    },
    SQLError,
};
use uqa_storage::{
    vector_index::{DiskANNAlpha, DiskANNIndexParams},
    CatalogIndexRow, StorageBackendError, StorageBackendResult,
};

pub(super) fn prepare(
    catalog: &dyn VectorIndexCatalog,
    statement: &mut CreateIndex,
) -> Result<(), SQLError> {
    let parsed = parse_diskann_index_options(&statement.options)?;
    let target = resolve_vector_index_target(catalog, statement, "diskann")?;
    if target.fields.len() != 1 {
        return Err(SQLError::Unsupported(
            "CREATE INDEX USING diskann requires exactly one vector or tensor column".into(),
        ));
    }
    let dimensions = target.fields[0].1;
    let mut parameters =
        DiskANNIndexParams::for_dimensions(dimensions).map_err(parameters_error)?;
    if let Some(value) = parsed.max_degree {
        parameters.max_degree = value;
    }
    if let Some(value) = parsed.build_list_size {
        parameters.build_list_size = value;
    }
    if let Some(value) = parsed.search_list_size {
        parameters.search_list_size = value;
    }
    if let Some(value) = parsed.alpha {
        parameters.alpha = DiskANNAlpha::new(value).map_err(parameters_error)?;
    }
    if let Some(value) = parsed.beam_width {
        parameters.beam_width = value;
    }
    if let Some(value) = parsed.pq_bytes {
        parameters.pq_bytes = value;
    }
    if let Some(value) = parsed.seed {
        parameters.seed = value;
    }
    statement.options = parameters
        .to_catalog_map(dimensions)
        .map_err(parameters_error)?
        .into_iter()
        .collect();
    Ok(())
}

pub(super) fn build(
    vectors: &dyn VectorIndexCatalog,
    publication: &dyn super::creation::IndexCreationPublication,
    row: &CatalogIndexRow,
) -> StorageBackendResult<()> {
    let (field, dimensions, parameters) = target(vectors, row)?;
    publication
        .create_diskann_field(row, &field, dimensions, parameters)
        .map_err(storage_error)
}

fn target(
    vectors: &dyn VectorIndexCatalog,
    row: &CatalogIndexRow,
) -> StorageBackendResult<(String, u32, DiskANNIndexParams)> {
    let statement = uqa_sql::catalog::index::stored::declaration(row)?;
    if statement.columns.len() != 1 {
        return Err(StorageBackendError::Other(
            "stored DiskANN index requires exactly one field".into(),
        ));
    }
    let field = require_column_key(&statement.columns[0], "diskann").map_err(storage_error)?;
    let dimensions = match vectors
        .column_type(&row.table_name, field)
        .map_err(storage_error)?
    {
        Some(ColumnType::Vector(dimensions) | ColumnType::Tensor(dimensions)) => dimensions,
        _ => {
            return Err(StorageBackendError::Other(
                "stored DiskANN index has no vector or tensor column".into(),
            ))
        }
    };
    let parameters = DiskANNIndexParams::from_catalog_map(
        dimensions,
        &serde_json::from_str(&row.parameters_json)?,
    )?;
    Ok((field.to_owned(), dimensions, parameters))
}

/// Retain actual catalog rows and retire their selected generations before the caller replaces the table's storage incarnation. The caller's transaction owns undo across both operations.
pub fn retire_table(
    context: &super::registry::IndexRegistryContext<'_>,
    table: &str,
) -> StorageBackendResult<Vec<CatalogIndexRow>> {
    retire_fields(context, table, None)
}

/// Retire the field's publication while its original column and index definitions still exist.
pub fn retire_column(
    context: &super::registry::IndexRegistryContext<'_>,
    table: &str,
    column: &str,
) -> StorageBackendResult<()> {
    retire_fields(context, table, Some(column)).map(|_| ())
}

/// Reconstruct the field after its canonical rewrite and new table schema have been persisted. The original catalog identity and effective parameters remain authoritative.
pub fn create_column(
    context: &super::registry::IndexRegistryContext<'_>,
    table: &str,
    column: &str,
) -> StorageBackendResult<bool> {
    let catalog = context.identities.catalog.current_catalog_snapshot();
    let row = crate::catalog::index::vectors::field_index(
        catalog.snapshot().definitions.catalog_indexes.values(),
        table,
        column,
    )?;
    let Some(row) = row.filter(|row| row.index_type.eq_ignore_ascii_case("diskann")) else {
        return Ok(false);
    };
    build(context.vectors, context.builds, row)?;
    Ok(true)
}

fn retire_fields(
    context: &super::registry::IndexRegistryContext<'_>,
    table: &str,
    column: Option<&str>,
) -> StorageBackendResult<Vec<CatalogIndexRow>> {
    let catalog = context.identities.catalog.current_catalog_snapshot();
    let rows = catalog
        .snapshot()
        .definitions
        .catalog_indexes
        .values()
        .filter(|row| row.table_name == table && row.index_type.eq_ignore_ascii_case("diskann"))
        .cloned()
        .collect::<Vec<_>>();
    let mut retired = Vec::new();
    for row in rows {
        if let Some(column) = column {
            if !crate::catalog::index::index_references_column(&row, column)? {
                continue;
            }
        }
        let (field, dimensions, _) = target(context.vectors, &row)?;
        context
            .retirement
            .retire_diskann_index(&row, &field, dimensions)?;
        retired.push(row);
    }
    Ok(retired)
}

/// Build under the newly persisted table incarnation, without changing index identities or reinterpreting persisted algorithm defaults.
fn create_table(
    context: &super::registry::IndexRegistryContext<'_>,
    rows: &[CatalogIndexRow],
) -> StorageBackendResult<()> {
    for row in rows {
        build(context.vectors, context.builds, row)?;
    }
    Ok(())
}

/// Coordinate index retirement and reconstruction around one table-storage incarnation replacement in the caller's transaction.
pub fn replace_table_storage(
    context: &super::registry::IndexRegistryContext<'_>,
    table: &str,
    replace: impl FnOnce() -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    let rows = retire_table(context, table)?;
    replace()?;
    create_table(context, &rows)
}

fn parameters_error(error: StorageBackendError) -> SQLError {
    SQLError::TypeMismatch(format!("CREATE INDEX USING diskann: {error}"))
}

fn storage_error(error: SQLError) -> StorageBackendError {
    StorageBackendError::backend("DiskANN index lifecycle", error)
}
