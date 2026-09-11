//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index-backed relational access selection after retrieval optimization.

use super::{operator_execution_error, query_optimizer, RetrievalPlanningCatalog};
use uqa_operators::OperatorTree;
use uqa_sql::{SQLError, ScalarExpr};

#[cfg(test)]
mod tests;

pub fn accelerated_tree(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    expression: &ScalarExpr,
    tree: OperatorTree,
) -> Result<Option<OperatorTree>, SQLError> {
    let optimized = query_optimizer(catalog, table, &tree)?.optimize(tree);
    if supports_acceleration(catalog, table, expression, &optimized)? {
        Ok(Some(optimized))
    } else {
        Ok(None)
    }
}

fn supports_acceleration(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    expression: &ScalarExpr,
    optimized: &OperatorTree,
) -> Result<bool, SQLError> {
    let mut has_index_scan = false;
    optimized.visit(&mut |node| has_index_scan |= matches!(node, OperatorTree::IndexScan { .. }));
    if !has_index_scan && !crate::optimizer::contains_retrieval(expression) {
        let mut filters = Vec::new();
        optimized.visit(&mut |node| {
            if let OperatorTree::Filter {
                field, predicate, ..
            } = node
            {
                filters.push((field.clone(), predicate.clone()));
            }
        });
        let mut all_value_indexed = !filters.is_empty();
        for (field, predicate) in filters {
            if !catalog
                .value_index_supports(table, &field, &predicate)
                .map_err(|error| operator_execution_error("prepare value index", error))?
            {
                all_value_indexed = false;
                break;
            }
        }
        if !all_value_indexed {
            return Ok(false);
        }
    }
    Ok(true)
}
