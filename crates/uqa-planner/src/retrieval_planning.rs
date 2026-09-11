//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retrieval optimizer setup and checked access estimates over retained catalog inputs.

mod access;
mod catalog;
mod top_k;
pub use access::accelerated_tree;
pub use top_k::{plan_bound_text_top_k, plan_text_top_k_tree};
mod indexes;
mod joins;
mod paradigm;
mod statistics;
use crate::query_optimizer::QueryOptimizer;
pub use catalog::{
    GraphStatisticsSnapshot, RetrievalPlanningCatalog, RetrievalStatisticsTable,
    TextStatisticsRead, VectorStatisticsRead,
};
use indexes::index_candidates;
pub use joins::estimate_cross_relation_operator_join;
pub use paradigm::operator_tree_paradigm;
use statistics::{graph_context, index_stats};
use std::collections::BTreeMap;
use uqa_operators::OperatorTree;
use uqa_sql::SQLError;
type PlanningResult<T> = Result<T, SQLError>;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;
fn operator_execution_error(operator: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("execute {operator}: {error}"))
}

pub fn query_optimizer(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    tree: &OperatorTree,
) -> PlanningResult<QueryOptimizer> {
    let candidates = index_candidates(catalog, table, tree)?;
    let stats = index_stats(catalog, table, tree)?;
    let column_stats = if table.is_empty() {
        BTreeMap::new()
    } else {
        catalog.try_query_column_stats(table).map_err(|error| {
            SQLError::Internal(format!(
                "read optimizer column statistics for `{table}`: {error}"
            ))
        })?
    };
    let mut optimizer = QueryOptimizer::new()
        .with_index_stats(stats)
        .with_column_stats(column_stats)
        .with_index_candidates(candidates, table);
    if let Some((graph_stats, graph_store)) = graph_context(catalog, tree)? {
        optimizer = optimizer
            .with_graph_stats(graph_stats)
            .with_graph_store(graph_store);
    }
    Ok(optimizer)
}

pub fn estimate_operator_tree_access(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    tree: OperatorTree,
    clamp_to_table: bool,
) -> PlanningResult<crate::LocalAccessEstimate> {
    let optimizer = query_optimizer(catalog, table, &tree)?;
    let planned_tree = optimizer.optimize(tree);
    let total_docs = optimizer.index_stats.total_docs as f64;
    let output_rows = optimizer
        .estimator
        .estimate(&planned_tree, &optimizer.index_stats);
    if !output_rows.is_finite() || output_rows < 0.0 {
        return Err(SQLError::Internal(format!(
            "operator access produced invalid cardinality {output_rows}"
        )));
    }
    let output_rows = if clamp_to_table {
        output_rows.min(total_docs)
    } else {
        output_rows
    };
    let cost = optimizer
        .cost_model
        .estimate(&planned_tree, &optimizer.index_stats);
    if !cost.is_finite() || cost < 0.0 {
        return Err(SQLError::Internal(format!(
            "operator access produced invalid cost {cost}"
        )));
    }
    Ok(crate::LocalAccessEstimate {
        output_rows,
        cost,
        paradigm: operator_tree_paradigm(&planned_tree),
    })
}
