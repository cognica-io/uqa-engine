//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog index candidates and physical scan costs.

use super::{operator_execution_error, PlanningResult, RetrievalPlanningCatalog};
use crate::{query_optimizer::IndexScanCandidate, CostEstimator, OperatorKind};
use std::collections::BTreeMap;
use uqa_operators::OperatorTree;
use uqa_sql::SQLError;

pub(super) fn index_candidates(
    catalog: &dyn RetrievalPlanningCatalog,
    table: &str,
    tree: &OperatorTree,
) -> PlanningResult<Vec<IndexScanCandidate>> {
    if table.is_empty()
        || !catalog
            .has_table(table)
            .map_err(|error| operator_execution_error("resolve index candidate table", error))?
    {
        return Ok(Vec::new());
    }
    let resolved_table = catalog
        .resolve_table_name(table)
        .map_err(|error| operator_execution_error("resolve index candidate table", error))?
        .unwrap_or_else(|| table.to_string());
    let mut indexes_by_field = BTreeMap::new();
    let indexes = catalog
        .list_catalog_indexes()
        .map_err(|error| operator_execution_error("list index candidates", error))?;
    for index in indexes.iter() {
        if index.table_name != resolved_table || !index.index_type.eq_ignore_ascii_case("btree") {
            continue;
        }
        let columns = serde_json::from_str::<Vec<uqa_sql::ast::IndexKey>>(&index.columns_json)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "decode catalog index `{}` columns: {error}",
                    index.relation.qualified_name()
                ))
            })?;
        if let Some(field) = columns.first().and_then(uqa_sql::ast::IndexKey::column) {
            indexes_by_field
                .entry(field.to_string())
                .or_insert_with(|| index.relation.qualified_name());
        }
    }
    drop(indexes);

    let mut candidates = Vec::new();
    tree.try_visit(&mut |node| {
        let OperatorTree::Filter {
            field,
            predicate,
            source: None,
        } = node
        else {
            return Ok(());
        };
        let Some(index_name) = indexes_by_field.get(field) else {
            return Ok(());
        };
        let Some(cardinality) = catalog.value_index_cardinality(table, field, predicate)? else {
            return Ok(());
        };
        let cardinality = cardinality as f64;
        let scan_cost = CostEstimator::default()
            .estimate_unary(OperatorKind::IndexScan, cardinality)
            .total();
        candidates.push(IndexScanCandidate {
            index_name: index_name.clone(),
            table_name: table.to_string(),
            field: field.clone(),
            predicate: predicate.clone(),
            scan_cost,
        });
        Ok::<(), SQLError>(())
    })?;
    Ok(candidates)
}
