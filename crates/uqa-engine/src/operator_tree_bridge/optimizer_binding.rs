//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine catalog binding for optimizer costs and index candidates.

use super::{
    operator_execution_error, BTreeMap, DriverResult, Engine, IndexScanCandidate, OperatorTree,
    PathSegment, Predicate, QueryOptimizer, SQLError, Value,
};

pub(super) fn engine_query_optimizer(
    engine: &Engine,
    table: &str,
    tree: &OperatorTree,
) -> DriverResult<QueryOptimizer> {
    let candidates = engine_index_candidates(engine, table, tree)?;
    let row_count = if table.is_empty() {
        0
    } else {
        engine.table_doc_count(table)?
    };
    Ok(QueryOptimizer::new()
        .with_row_count(row_count)
        .with_index_candidates(candidates, table))
}

pub(super) fn engine_index_candidates(
    engine: &Engine,
    table: &str,
    tree: &OperatorTree,
) -> DriverResult<Vec<IndexScanCandidate>> {
    if table.is_empty()
        || !engine
            .has_table(table)
            .map_err(|error| operator_execution_error("resolve index candidate table", error))?
    {
        return Ok(Vec::new());
    }
    let resolved_table = engine
        .resolve_table_name(table)
        .map_err(|error| operator_execution_error("resolve index candidate table", error))?
        .unwrap_or_else(|| table.to_string());
    let mut indexes_by_field = BTreeMap::new();
    for index in engine
        .list_catalog_indexes()
        .map_err(|error| operator_execution_error("list index candidates", error))?
    {
        if index.table_name != resolved_table || !index.index_type.eq_ignore_ascii_case("btree") {
            continue;
        }
        let columns =
            serde_json::from_str::<Vec<String>>(&index.columns_json).map_err(|error| {
                SQLError::Internal(format!(
                    "decode catalog index `{}` columns: {error}",
                    index.name
                ))
            })?;
        if let Some(field) = columns.first() {
            indexes_by_field
                .entry(field.clone())
                .or_insert(index.name.clone());
        }
    }

    let mut predicates = Vec::new();
    tree.visit(&mut |node| {
        let OperatorTree::Filter {
            field,
            predicate,
            source: None,
        } = node
        else {
            return;
        };
        predicates.push((field.clone(), predicate.clone()));
    });

    let mut candidates = Vec::new();
    for (field, predicate) in predicates {
        let Some(index_name) = indexes_by_field.get(&field) else {
            continue;
        };
        let Some(cardinality) = engine.value_index_cardinality(table, &field, &predicate)? else {
            continue;
        };
        let cardinality = cardinality as f64;
        let scan_cost = match predicate {
            Predicate::Equals(_) => 1.0 + cardinality * 0.1,
            _ => cardinality.max(1.0),
        };
        candidates.push(IndexScanCandidate {
            index_name: index_name.clone(),
            table_name: table.to_string(),
            field,
            predicate,
            scan_cost,
        });
    }
    Ok(candidates)
}

/// Number of score-contributing text terms in a bound BM25 query tree.
/// Set operations merge payloads by summing scores, so the raw query
/// score scales with this count and the calibration must be translated
/// to it. Complements filter without contributing score.
pub(super) fn scored_term_count(tree: &OperatorTree) -> usize {
    match tree {
        OperatorTree::Term { .. } => 1,
        OperatorTree::Intersect(children)
        | OperatorTree::Union(children)
        | OperatorTree::Composed(children) => children.iter().map(scored_term_count).sum(),
        OperatorTree::Filter { source, .. } => source.as_deref().map_or(0, scored_term_count),
        OperatorTree::BayesianScore { source, .. } | OperatorTree::Score { source, .. } => {
            scored_term_count(source)
        }
        _ => 0,
    }
}

// `eval_path` lives in storage; expose a shim so we don't pull in the
// trait at the lowering layer just for this helper.
#[allow(dead_code)]
pub(super) fn lookup_path(value: &Value, path: &[PathSegment]) -> Option<Value> {
    let mut current = value.clone();
    for seg in path {
        current = match (current, seg) {
            (Value::Map(m), PathSegment::Key(k)) => m.get(k)?.clone(),
            (Value::List(items), PathSegment::Index(i)) => items.get(*i)?.clone(),
            _ => return None,
        };
    }
    Some(current)
}
