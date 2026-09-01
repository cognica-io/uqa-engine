//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent relation statistics and cost estimation for operator joins.

use uqa_planner::{
    CardinalityEstimator, CostEstimator, OperatorKind, GRAPH_AVG_DEGREE_DEFAULT,
    JACCARD_JOIN_SELECTIVITY,
};
use uqa_sql::ast::OperatorJoinRelations;

use super::{
    engine_query_optimizer, lower_operator_join_table_function, operator_tree_paradigm,
    DriverResult, Engine, OperatorTree, SQLError, SQLParam, ScalarExpr,
};

struct OperatorJoinSideEstimate {
    output_rows: f64,
    cost: f64,
    total_docs: f64,
    dimensions: u32,
    graph_stats: Option<uqa_planner::GraphStats>,
}

enum OperatorJoinEstimateKind {
    Text,
    Vector { threshold: f64 },
    Graph { label: Option<String> },
    Hybrid,
    CrossParadigm,
}

struct OperatorJoinEstimateInput {
    left: OperatorTree,
    right: OperatorTree,
    kind: OperatorJoinEstimateKind,
    paradigm: uqa_planner::AccessParadigm,
}

fn split_operator_join_estimate(tree: OperatorTree) -> DriverResult<OperatorJoinEstimateInput> {
    let paradigm = operator_tree_paradigm(&tree);
    let (left, right, kind) = match tree {
        OperatorTree::TextSimilarityJoin {
            left,
            right,
            threshold: _,
        } => (*left, *right, OperatorJoinEstimateKind::Text),
        OperatorTree::VectorSimilarityJoin {
            left,
            right,
            threshold,
        } => (
            *left,
            *right,
            OperatorJoinEstimateKind::Vector { threshold },
        ),
        OperatorTree::GraphJoin {
            left,
            right,
            label,
            graph: _,
        } => (*left, *right, OperatorJoinEstimateKind::Graph { label }),
        OperatorTree::HybridJoin { left, right } => {
            (*left, *right, OperatorJoinEstimateKind::Hybrid)
        }
        OperatorTree::CrossParadigmJoin { left, right } => {
            (*left, *right, OperatorJoinEstimateKind::CrossParadigm)
        }
        _ => {
            return Err(SQLError::Internal(
                "operator join table function lowered to a non-join root".into(),
            ))
        }
    };
    Ok(OperatorJoinEstimateInput {
        left,
        right,
        kind,
        paradigm,
    })
}

fn estimate_operator_join_side(
    engine: &Engine,
    relation: &str,
    tree: OperatorTree,
) -> DriverResult<OperatorJoinSideEstimate> {
    let optimizer = engine_query_optimizer(engine, relation, &tree)?;
    let planned = optimizer.optimize(tree);
    let output_rows = optimizer
        .estimator
        .estimate(&planned, &optimizer.index_stats);
    let total_docs = optimizer.index_stats.total_docs as f64;
    let cost = optimizer
        .cost_model
        .estimate(&planned, &optimizer.index_stats);
    if !output_rows.is_finite() || output_rows < 0.0 {
        return Err(SQLError::Internal(format!(
            "operator join relation `{relation}` produced invalid cardinality {output_rows}"
        )));
    }
    if !cost.is_finite() || cost < 0.0 {
        return Err(SQLError::Internal(format!(
            "operator join relation `{relation}` produced invalid cost {cost}"
        )));
    }
    Ok(OperatorJoinSideEstimate {
        output_rows,
        cost,
        total_docs,
        dimensions: optimizer.index_stats.dimensions,
        graph_stats: optimizer.graph_stats.clone(),
    })
}

fn estimate_graph_operator_join(
    left: &OperatorJoinSideEstimate,
    right: &OperatorJoinSideEstimate,
    label: Option<&str>,
    physical: &CostEstimator,
) -> (f64, f64) {
    let edge_probability = left.graph_stats.as_ref().map_or_else(
        || (GRAPH_AVG_DEGREE_DEFAULT / left.total_docs.max(1.0)).min(1.0),
        |stats| {
            let vertices = (stats.num_vertices as f64).max(1.0);
            (stats.avg_out_degree * stats.label_selectivity(label) / vertices).min(1.0)
        },
    );
    let candidate_edges = left.output_rows
        * left
            .graph_stats
            .as_ref()
            .map_or(GRAPH_AVG_DEGREE_DEFAULT, |stats| {
                stats.avg_out_degree * stats.label_selectivity(label)
            });
    let join_cost = candidate_edges
        + physical
            .estimate_join(
                OperatorKind::HashJoinInner,
                candidate_edges,
                right.output_rows,
            )
            .total();
    (
        left.output_rows * right.output_rows * edge_probability,
        join_cost,
    )
}

fn estimate_operator_join_result(
    kind: &OperatorJoinEstimateKind,
    left: &OperatorJoinSideEstimate,
    right: &OperatorJoinSideEstimate,
) -> (f64, f64) {
    let left_rows = left.output_rows;
    let right_rows = right.output_rows;
    let dimensions = left.dimensions.max(right.dimensions).max(1);
    let domain = left.total_docs.max(right.total_docs).max(1.0);
    let physical = CostEstimator::default();
    match kind {
        OperatorJoinEstimateKind::Text => (
            left_rows * right_rows * JACCARD_JOIN_SELECTIVITY,
            physical
                .estimate_join(OperatorKind::NestedLoopJoin, left_rows, right_rows)
                .total(),
        ),
        OperatorJoinEstimateKind::Vector { threshold } => (
            left_rows
                * right_rows
                * CardinalityEstimator::vector_selectivity(*threshold, dimensions),
            physical
                .estimate_join(OperatorKind::NestedLoopJoin, left_rows, right_rows)
                .total()
                * f64::from(dimensions),
        ),
        OperatorJoinEstimateKind::Graph { label } => {
            estimate_graph_operator_join(left, right, label.as_deref(), &physical)
        }
        OperatorJoinEstimateKind::Hybrid => {
            let equality_candidates = left_rows * right_rows / domain;
            (
                equality_candidates * CardinalityEstimator::vector_selectivity(0.5, dimensions),
                physical
                    .estimate_join(OperatorKind::HashJoinInner, left_rows, right_rows)
                    .total()
                    + physical
                        .estimate_join(OperatorKind::NestedLoopJoin, equality_candidates, 1.0)
                        .total()
                        * f64::from(dimensions),
            )
        }
        OperatorJoinEstimateKind::CrossParadigm => (
            left_rows * right_rows / domain,
            physical
                .estimate_join(OperatorKind::HashJoinInner, left_rows, right_rows)
                .total(),
        ),
    }
}

fn estimate_cross_relation_operator_join(
    engine: &Engine,
    relations: &OperatorJoinRelations,
    tree: OperatorTree,
) -> DriverResult<uqa_planner::LocalAccessEstimate> {
    let input = split_operator_join_estimate(tree)?;
    let left = estimate_operator_join_side(engine, &relations.left, input.left)?;
    let right = estimate_operator_join_side(engine, &relations.right, input.right)?;
    let (output_rows, join_cost) = estimate_operator_join_result(&input.kind, &left, &right);
    let cost = left.cost + right.cost + join_cost;
    if !output_rows.is_finite() || output_rows < 0.0 || !cost.is_finite() || cost < 0.0 {
        return Err(SQLError::Internal(format!(
            "cross-relation operator join produced invalid estimate rows={output_rows}, cost={cost}"
        )));
    }
    Ok(uqa_planner::LocalAccessEstimate {
        output_rows,
        cost,
        paradigm: input.paradigm,
    })
}

pub(crate) fn estimate_operator_join_table_function(
    engine: &Engine,
    name: &str,
    relations: Option<&OperatorJoinRelations>,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<uqa_planner::LocalAccessEstimate> {
    let (relations, tree) =
        lower_operator_join_table_function(engine, name, relations, args, params)?;
    estimate_cross_relation_operator_join(engine, &relations, tree)
}
