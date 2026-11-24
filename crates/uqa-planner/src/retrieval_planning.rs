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
use crate::query_optimizer::{IndexScanCandidate, QueryOptimizer};
pub use catalog::{
    GraphStatisticsSnapshot, RetrievalPlanningCatalog, RetrievalStatisticsTable,
    TextStatisticsRead, VectorStatisticsRead,
};
use indexes::index_candidates;
pub use joins::estimate_cross_relation_operator_join;
pub use paradigm::operator_tree_paradigm;
use statistics::{graph_context, index_stats};
use uqa_core::memory::{ProductionControl, ProductionSmallVec};
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
    let candidates = index_candidates(catalog, table, tree, None)?;
    let resources = catalog.production_resources()?;
    build_optimizer(
        catalog,
        table,
        tree,
        candidates,
        planning_control(resources.as_ref()),
    )
}

fn planning_control(resources: Option<&crate::PlanningResources>) -> ProductionControl<'_> {
    resources.map_or_else(
        ProductionControl::uncontrolled,
        crate::PlanningResources::control,
    )
}

fn build_optimizer(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    tree: &OperatorTree,
    candidates: Vec<IndexScanCandidate>,
    control: ProductionControl<'_>,
) -> PlanningResult<QueryOptimizer> {
    let stats = index_stats(catalog, table, tree, &control)?;
    let column_stats = if table.is_empty() {
        crate::ColumnStatistics::default()
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

struct CostedTree {
    optimizer: QueryOptimizer,
    tree: OperatorTree,
    output_rows: f64,
    cost: f64,
}

fn costed_tree(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    tree: OperatorTree,
) -> PlanningResult<CostedTree> {
    let resources = catalog.production_resources()?;
    let control = planning_control(resources.as_ref());
    let mut observed_rows = ProductionSmallVec::<[usize; 4]>::new(control);
    let candidates = index_candidates(catalog, table, &tree, Some(&mut observed_rows))?;
    let optimizer = build_optimizer(catalog, table, &tree, candidates, control)?;
    let tree = optimizer.optimize(tree);
    let observed = crate::operator_statistics::OperatorStatistics::new(
        &optimizer.index_stats,
        &optimizer.index_candidates,
        &observed_rows,
    );
    let output_rows = optimizer.estimator.estimate_tree(&tree, &observed);
    let cost = optimizer.cost_model.estimate_tree(&tree, &observed);
    Ok(CostedTree {
        optimizer,
        tree,
        output_rows,
        cost,
    })
}

pub fn estimate_operator_tree_access(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    tree: OperatorTree,
    clamp_to_table: bool,
) -> PlanningResult<crate::LocalAccessEstimate> {
    let CostedTree {
        optimizer,
        tree,
        output_rows,
        cost,
    } = costed_tree(catalog, table, tree)?;
    if !output_rows.is_finite() || output_rows < 0.0 {
        return Err(SQLError::Internal(format!(
            "operator access produced invalid cardinality {output_rows}"
        )));
    }
    let output_rows = if clamp_to_table {
        output_rows.min(optimizer.index_stats.total_docs as f64)
    } else {
        output_rows
    };
    if !cost.is_finite() || cost < 0.0 {
        return Err(SQLError::Internal(format!(
            "operator access produced invalid cost {cost}"
        )));
    }
    Ok(crate::LocalAccessEstimate {
        output_rows,
        cost,
        paradigm: operator_tree_paradigm(&tree),
    })
}
