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
