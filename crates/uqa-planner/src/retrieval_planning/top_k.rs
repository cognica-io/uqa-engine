//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Plan physical text limits from one retained analyzer/index-statistics snapshot.

use super::RetrievalPlanningCatalog;
use uqa_operators::{OperatorTree, TextScoringMode};
use uqa_sql::SQLError;

#[cfg(test)]
mod tests;

pub fn plan_text_top_k_tree(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    field: &str,
    query: &str,
    scoring: TextScoringMode,
    top_k: usize,
) -> Result<OperatorTree, SQLError> {
    let capabilities = catalog.text_top_k_capabilities(table, field, query)?;
    Ok(crate::plan_text_top_k(
        OperatorTree::Term {
            query: query.to_string(),
            field: Some(field.to_string()),
            scoring: Some(scoring),
            top_k: None,
        },
        top_k,
        capabilities,
    ))
}

pub fn plan_bound_text_top_k(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    tree: uqa_operators::OperatorTree,
    top_k: usize,
) -> Result<uqa_operators::OperatorTree, SQLError> {
    let (query, field, scoring) = match tree {
        uqa_operators::OperatorTree::Term {
            query,
            field: Some(field),
            scoring: Some(scoring),
            top_k: None,
        } => (query, field, scoring),
        other => return Ok(other),
    };
    plan_text_top_k_tree(catalog, table, &field, &query, scoring, top_k)
}
