//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode stored index metadata and inspect column references in key, included-column, then predicate order.
use super::IndexDefinition;
use crate::{ast::IndexKey, schema::dependencies::schema_expr_references_column};
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
