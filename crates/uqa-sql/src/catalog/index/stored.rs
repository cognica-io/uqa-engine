//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode stored index metadata and inspect column references in key, included-column, then predicate order.
use super::IndexDefinition;
use crate::{
    ast::{CreateIndex, IndexKey},
    schema::dependencies::schema_expr_references_column,
};

/// Restore the complete SQL declaration for validation or a physical rebuild without changing its durable catalog identity.
pub fn declaration(
    row: &uqa_core::catalog_index::CatalogIndexRow,
) -> Result<CreateIndex, serde_json::Error> {
    let definition = index_definition(row.definition_json.as_deref())?;
    let options: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&row.parameters_json)?;
    Ok(CreateIndex {
        name: Some(row.relation.name.clone()),
        table: row.table_name.clone(),
        access_method: row.index_type.clone(),
        columns: serde_json::from_str(&row.columns_json)?,
        included_columns: definition.included_columns,
        column_order: definition.column_order,
        predicate: definition.predicate,
        unique: definition.unique,
        nulls_not_distinct: definition.nulls_not_distinct,
        if_not_exists: false,
        options: options.into_iter().collect(),
    })
}
pub fn index_definition(definition: Option<&str>) -> Result<IndexDefinition, serde_json::Error> {
    definition.map_or_else(|| Ok(IndexDefinition::default()), serde_json::from_str)
}
pub fn references_column(
    keys: &str,
    definition: Option<&str>,
    column: &str,
) -> Result<bool, serde_json::Error> {
    let keys: Vec<IndexKey> = serde_json::from_str(keys)?;
    Ok(keys
        .iter()
        .any(|key| schema_expr_references_column(&key.expression(), column))
        || index_definition(definition)?
            .included_columns
            .iter()
            .any(|name| name == column)
        || index_definition(definition)?
            .predicate
            .as_deref()
            .is_some_and(|expression| schema_expr_references_column(expression, column)))
}
