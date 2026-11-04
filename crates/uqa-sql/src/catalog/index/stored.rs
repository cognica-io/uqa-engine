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
        option_namespaces: Vec::new(),
    })
}
pub fn index_definition(definition: Option<&str>) -> Result<IndexDefinition, serde_json::Error> {
    definition.map_or_else(|| Ok(IndexDefinition::default()), serde_json::from_str)
}

/// Observe an index's durable address without decoding predicates, key types or included-column payloads. Full definition validation remains at catalog restoration and publication.
pub fn catalog_object_identity(
    definition: Option<&str>,
) -> Result<Option<uqa_core::catalog_identity::CatalogObjectIdentity>, serde_json::Error> {
    #[derive(serde::Deserialize)]
    struct Definition {
        catalog: Option<Identity>,
    }
    #[derive(serde::Deserialize)]
    struct Identity {
        identity: uqa_core::catalog_identity::CatalogObjectIdentity,
    }
    definition.map_or(Ok(None), |text| {
        serde_json::from_str::<Definition>(text)
            .map(|definition| definition.catalog.map(|catalog| catalog.identity))
    })
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

/// Preserve catalog identities and displayed key names while rebinding column references.
pub fn rename_column_references(
    keys: &mut [IndexKey],
    definition: &mut IndexDefinition,
    from: &str,
    to: &str,
) -> Result<(), String> {
    use crate::schema::dependencies::rewrites::rename_schema_expr_column;
    if definition.key_names.is_empty() {
        definition.key_names = keys
            .iter()
            .filter_map(IndexKey::column)
            .chain(definition.included_columns.iter().map(String::as_str))
            .map(str::to_owned)
            .collect();
    }
    for key in keys {
        match key {
            IndexKey::Column(name) => {
                if name == from {
                    *name = to.into();
                }
            }
            IndexKey::Expression(expression) => {
                rename_schema_expr_column(expression, from, to)?;
            }
        }
    }
    for column in &mut definition.included_columns {
        if column == from {
            *column = to.into();
        }
    }
    if let Some(predicate) = definition.predicate.as_deref_mut() {
        rename_schema_expr_column(predicate, from, to)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
