//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parameter-independent prepared-plan index costs over canonical catalog metadata.
use super::statistics::CatalogSourceStatistics;
use crate::{
    AccessParadigm, CardinalityEstimator, CostEstimator, LocalAccessEstimate, OperatorKind,
    RelationStats, SourceStatistics,
};
use uqa_sql::{ast::BinaryOp, SQLError, ScalarExpr};
pub(super) fn parameterized_access(
    statistics: &CatalogSourceStatistics<'_>,
    table: &str,
    predicate: &ScalarExpr,
) -> Result<Option<LocalAccessEstimate>, SQLError> {
    let Some(stats) = statistics.relation_statistics(table) else {
        return Ok(None);
    };
    let catalog = statistics.context.catalog;
    let resolved = catalog.resolved_table_name(table)?;
    let indexes = catalog.catalog_indexes()?;
    let mut fields = std::collections::BTreeSet::new();
    for index in indexes {
        if Some(&index.table_name) != resolved.as_ref()
            || !index.index_type.eq_ignore_ascii_case("btree")
        {
            continue;
        }
        let definition =
            uqa_sql::catalog::index::stored::index_definition(index.definition_json.as_deref())
                .map_err(|error| {
                    SQLError::Internal(format!("payload serialization failed: {error}"))
                })?;
        // A parameter cannot establish a partial index's predicate at planning time.
        if definition.predicate.is_some() {
            continue;
        }
        let keys: Vec<uqa_sql::ast::IndexKey> =
            serde_json::from_str(&index.columns_json).map_err(|error| {
                SQLError::Internal(format!("decode index columns for costing: {error}"))
            })?;
        if let Some(column) = keys.first().and_then(uqa_sql::ast::IndexKey::column) {
            fields.insert(column.to_string());
        }
    }
    let costs = CostEstimator::default();
    let rows = stats.row_count as f64;
    let mut cost = costs.estimate_unary(OperatorKind::TableScan, rows).total()
        + costs.estimate_unary(OperatorKind::Filter, rows).total();
    if let Some(candidate_rows) = index_rows(predicate, &stats, &fields) {
        let index_cost = costs
            .estimate_unary(OperatorKind::IndexScan, candidate_rows)
            .total()
            + costs
                .estimate_unary(OperatorKind::Filter, candidate_rows)
                .total();
        cost = cost.min(index_cost);
    }
    Ok(Some(LocalAccessEstimate {
        output_rows: rows
            * CardinalityEstimator::new()
                .scalar_selectivity(predicate, &stats)
                .raw(),
        cost,
        paradigm: AccessParadigm::Relational,
    }))
}

fn index_rows(
    expression: &ScalarExpr,
    stats: &RelationStats,
    fields: &std::collections::BTreeSet<String>,
) -> Option<f64> {
    if let ScalarExpr::And(items) = expression {
        return items
            .iter()
            .filter_map(|item| index_rows(item, stats, fields))
            .reduce(f64::min);
    }
    let ScalarExpr::Binary { op, lhs, rhs } = expression else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    ) {
        return None;
    }
    let ((ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. }, parameter)
    | (parameter, ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. })) =
        (lhs.as_ref(), rhs.as_ref())
    else {
        return None;
    };
    if !fields.contains(column)
        || !matches!(
            parameter,
            ScalarExpr::Param(_) | ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. }
        )
    {
        return None;
    }
    Some(
        stats.row_count as f64
            * CardinalityEstimator::new()
                .scalar_selectivity(expression, stats)
                .raw(),
    )
}
