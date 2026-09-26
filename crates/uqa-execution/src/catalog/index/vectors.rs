//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored vector-method selection and canonical backfill belong to execution.

use uqa_sql::ast::ColumnType;
use uqa_storage::{
    vector_index::{DiskANNIndexParams, HNSWIndexParams, IVFIndexParams, VectorIndexSpec},
    CatalogIndexRow, DocumentStore, StorageBackendError, StorageBackendResult, VectorIndex,
};

pub fn is_vector_method(method: &str) -> bool {
    ["ivf", "hnsw", "diskann"]
        .iter()
        .any(|candidate| method.eq_ignore_ascii_case(candidate))
}

pub fn stored_spec(
    row: &CatalogIndexRow,
    dimensions: u32,
) -> StorageBackendResult<VectorIndexSpec> {
    let parameters = serde_json::from_str(&row.parameters_json)?;
    match row.index_type.to_ascii_lowercase().as_str() {
        "ivf" => IVFIndexParams::from_catalog_map(&parameters).map(VectorIndexSpec::IVF),
        "hnsw" => HNSWIndexParams::from_catalog_map(&parameters).map(VectorIndexSpec::HNSW),
        "diskann" => DiskANNIndexParams::from_catalog_map(dimensions, &parameters)
            .map(VectorIndexSpec::DiskANN),
        _ => Err(StorageBackendError::Other(
            "catalog row is not a physical vector index".into(),
        )),
    }
}

pub fn field_index<'a>(
    rows: impl Iterator<Item = &'a CatalogIndexRow>,
    table: &str,
    field: &str,
) -> StorageBackendResult<Option<&'a CatalogIndexRow>> {
    let mut found = None;
    for row in rows {
        if row.table_name == table
            && is_vector_method(&row.index_type)
            && super::index_references_column(row, field)?
            && found.replace(row).is_some()
        {
            return Err(StorageBackendError::Other(format!(
                "multiple physical vector indexes target `{table}`.`{field}`"
            )));
        }
    }
    Ok(found)
}

/// The column rename keeps a catalog-bound generation and its canonical origins together; other physical methods use their existing rebuild path.
pub struct ColumnVectorRename {
    pub dimensions: u32,
    pub retained: Option<Box<dyn VectorIndex>>,
}

pub fn detach_for_column_rename(
    indexes: &mut uqa_storage::vector_index::VectorIndexes,
    field: &str,
) -> StorageBackendResult<Option<ColumnVectorRename>> {
    let Some(mut index) = indexes.live_mut()?.remove(field) else {
        return Ok(None);
    };
    let dimensions = index.dimensions();
    let retained = if index.index_kind() == "diskann" {
        Some(index)
    } else {
        index.clear()?;
        None
    };
    Ok(Some(ColumnVectorRename {
        dimensions,
        retained,
    }))
}

/// Use the canonical writer with the target dimensions during a type rewrite. The catalog owner must retire any `DiskANN` generation before this call; clearing that retired live handle would incorrectly try to publish another generation.
pub fn prepare_column_rewrite(
    indexes: &mut uqa_storage::vector_index::VectorIndexes,
    field: &str,
    mut canonical: Box<dyn VectorIndex>,
) -> StorageBackendResult<()> {
    let indexes = indexes.live_mut()?;
    if let Some(mut old) = indexes.remove(field) {
        if old.index_kind() != "diskann" {
            old.clear()?;
        }
    }
    canonical.clear()?;
    indexes.insert(field.into(), canonical);
    Ok(())
}

/// Acquire every index definition lock while the original generation is still readable, then remove its canonical data and catalog rows within the caller's transaction.
pub fn remove_for_column_conversion(
    context: &crate::schema::indexes::registry::IndexRegistryContext<'_>,
    table: &str,
    column: &str,
    clear: impl FnOnce() -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    use crate::schema::indexes::registry::{binding, lifecycle};
    let catalog = context.identities.catalog.current_catalog_snapshot();
    let mut rows = Vec::new();
    for row in catalog.catalog_indexes() {
        if row.table_name == table
            && is_vector_method(&row.index_type)
            && super::index_references_column(row, column)?
        {
            rows.push(row.relation.clone());
        }
    }
    for relation in &rows {
        binding::removal(context, relation)?;
    }
    clear()?;
    for relation in rows {
        lifecycle::remove(context, &relation, false)?;
    }
    Ok(())
}

/// Populate a newly selected memory index from one fixed document view, preserving SQL tensor extraction and the index owner's initialization contract.
pub fn populate(
    index: &mut dyn VectorIndex,
    documents: &dyn DocumentStore,
    field: &str,
    ty: Option<&ColumnType>,
) -> StorageBackendResult<()> {
    for (document, values) in documents.iter_all()? {
        let Some(value) = values
            .get(field)
            .filter(|value| !matches!(value, uqa_core::Value::Null))
        else {
            continue;
        };
        let vectors = match ty {
            Some(ty @ (ColumnType::Vector(_) | ColumnType::Tensor(_))) => {
                uqa_sql::assignment::vectors::index_vectors_for_type(value, ty)
            }
            _ => uqa_sql::expr::value_to_vector(value).map(|vector| vec![vector]),
        }
        .map_err(|error| StorageBackendError::backend("vector index backfill", error))?;
        index.add_many(document, vectors)?;
    }
    index.initialize()
}
