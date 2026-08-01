//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM/JOIN row assembly, table functions, and projection intercepts.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_execution::{
    eval_call_arguments, eval_scalar, ScalarEvalContext, ScalarExpr, ScalarSubqueryRunner,
};
use uqa_planner::{
    AccessPathPlan, ComputePlan, JoinExecutionStrategy, QueryBlockPlan, QueryPlan, RelationalPlan,
    SourcePlan,
};
use uqa_sql::ast::JoinKind;
use uqa_sql::{ResultRow, SQLError, SQLParam};
use uqa_storage::document_store::Document;

use crate::{Engine, SQLTableFunctionResult, SQLTableFunctionStream};

use super::scalar::{PhysicalSubqueryRunner, PlanSubqueryArena};
use super::select::{
    execute_query_plan_output, physical_work_mem_bytes, push_output_filter_into_query_plan,
    CteScope, EngineExpressionEvaluator, QueryOutput, QueryOutputMode, QueryRows, ScopedEngineHook,
    ScoredDocumentSource, ScoredInput,
};
use super::volatility::query_contains_volatile_function;
use super::{
    age_cypher, build_info_schema_rows, doc_id_value, execute_tree_entries, expect_column_name,
    expect_optional_graph_value, graph_betweenness_entries, graph_hits_entries,
    graph_pagerank_entries, is_score_provenance_column, json_table_arg, json_table_value_to_text,
    projection_columns, run_age_create_graph_with_evaluator, run_age_drop_graph_with_evaluator,
    run_graph_create_with_evaluator, run_graph_drop_with_evaluator, MERGE_ACTION_COLUMN,
    SCORE_PROVENANCE_COLUMN,
};

pub(super) type ColumnPrune = BTreeMap<String, BTreeSet<String>>;
pub(super) type QualifierFilters = BTreeMap<String, Vec<ScalarExpr>>;

fn checked_integer_value<T>(value: T, label: &str) -> Result<Value, SQLError>
where
    T: Copy + std::fmt::Display,
    i64: TryFrom<T>,
{
    i64::try_from(value).map(Value::Int).map_err(|_| {
        SQLError::TypeMismatch(format!("{label} {value} exceeds the SQL BIGINT range"))
    })
}

fn qualifier_for(name: &str, alias: Option<&str>) -> String {
    alias.unwrap_or(name).to_string()
}

fn qualified_key(qual: &str, column: &str) -> String {
    let mut key = String::with_capacity(qual.len() + 1 + column.len());
    key.push_str(qual);
    key.push('.');
    key.push_str(column);
    key
}

/// Synthesize rows for `information_schema` / `pg_catalog` virtual
/// views. Returns `None` for any unknown name so the caller falls back
/// to the regular table lookup.
pub(super) fn prefix_row(qual: &str, doc: &Document) -> ResultRow {
    let mut out = ResultRow::new();
    for (k, v) in doc {
        out.insert(qualified_key(qual, k), v.clone());
    }
    out
}

fn has_filters_for_qualifier(filters: Option<&QualifierFilters>, qual: &str) -> bool {
    filters
        .and_then(|filters| filters.get(qual))
        .is_some_and(|filters| !filters.is_empty())
}

fn combine_filters(filters: impl IntoIterator<Item = ScalarExpr>) -> Option<ScalarExpr> {
    let mut filters: Vec<ScalarExpr> = filters.into_iter().collect();
    if filters.len() == 1 {
        filters.pop()
    } else if filters.is_empty() {
        None
    } else {
        Some(ScalarExpr::And(filters))
    }
}

fn qualifier_filter(filters: Option<&QualifierFilters>, qualifier: &str) -> Option<ScalarExpr> {
    filters
        .and_then(|filters| filters.get(qualifier))
        .filter(|filters| !filters.is_empty())
        .and_then(|filters| combine_filters(filters.iter().cloned()))
}

fn propagated_join_filters(
    filters: &QualifierFilters,
    source_from: &SourcePlan,
    target_from: &SourcePlan,
    on: Option<&ScalarExpr>,
) -> Option<QualifierFilters> {
    let on = on?;
    let mut out = filters.clone();
    let mut changed = false;
    let source_quals = from_qualifiers(source_from);
    let target_quals = from_qualifiers(target_from);
    for (left, right) in join_column_equalities(on) {
        changed |= propagate_join_filter_pair(
            filters,
            &mut out,
            &source_quals,
            &target_quals,
            &left,
            &right,
        );
        changed |= propagate_join_filter_pair(
            filters,
            &mut out,
            &source_quals,
            &target_quals,
            &right,
            &left,
        );
    }
    changed.then_some(out)
}

fn propagate_join_filter_pair(
    filters: &QualifierFilters,
    out: &mut QualifierFilters,
    source_quals: &BTreeSet<String>,
    target_quals: &BTreeSet<String>,
    source: &(String, String),
    target: &(String, String),
) -> bool {
    if !source_quals.contains(&source.0) || !target_quals.contains(&target.0) {
        return false;
    }
    let mut changed = false;
    if let Some(source_filters) = filters.get(&source.0) {
        for filter in source_filters {
            if let Some(value) = constant_equality_for_column(filter, &source.0, &source.1) {
                let propagated = ScalarExpr::Binary {
                    op: uqa_sql::ast::BinaryOp::Equal,
                    lhs: Box::new(ScalarExpr::qualified_column(&target.0, &target.1)),
                    rhs: Box::new(value),
                };
                out.entry(target.0.clone()).or_default().push(propagated);
                changed = true;
            }
        }
    }
    changed
}

fn constant_equality_for_column(expr: &ScalarExpr, qual: &str, column: &str) -> Option<ScalarExpr> {
    let ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::Equal,
        lhs,
        rhs,
    } = expr
    else {
        return None;
    };
    if expr_is_qualified_column(lhs, qual, column) && expr_is_constant(rhs) {
        return Some((**rhs).clone());
    }
    if expr_is_qualified_column(rhs, qual, column) && expr_is_constant(lhs) {
        return Some((**lhs).clone());
    }
    None
}

fn expr_is_qualified_column(expr: &ScalarExpr, qual: &str, column: &str) -> bool {
    matches!(
        expr,
        ScalarExpr::QualifiedColumn {
            qualifier,
            column: col,
            ..
        } if qualifier == qual && col == column
    )
}

fn expr_is_constant(expr: &ScalarExpr) -> bool {
    matches!(expr, ScalarExpr::Literal(_) | ScalarExpr::Param(_))
}

fn join_column_equalities(expr: &ScalarExpr) -> Vec<((String, String), (String, String))> {
    match expr {
        ScalarExpr::And(items) => items.iter().flat_map(join_column_equalities).collect(),
        ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Equal,
            lhs,
            rhs,
        } => {
            let Some(left) = qualified_column_pair(lhs) else {
                return Vec::new();
            };
            let Some(right) = qualified_column_pair(rhs) else {
                return Vec::new();
            };
            vec![(left, right)]
        }
        _ => Vec::new(),
    }
}

fn qualified_column_pair(expr: &ScalarExpr) -> Option<(String, String)> {
    match expr {
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } => Some((qualifier.clone(), column.clone())),
        _ => None,
    }
}

fn from_qualifiers(from: &SourcePlan) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_from_qualifiers(from, &mut out);
    out
}

fn collect_from_qualifiers(from: &SourcePlan, out: &mut BTreeSet<String>) {
    match from {
        SourcePlan::Table { name, alias } => {
            out.insert(alias.clone().unwrap_or_else(|| name.clone()));
        }
        SourcePlan::Join { left, right, .. } => {
            collect_from_qualifiers(left, out);
            collect_from_qualifiers(right, out);
        }
        SourcePlan::Values { alias, .. }
        | SourcePlan::Function { alias, .. }
        | SourcePlan::Subquery { alias, .. } => {
            if let Some(alias) = alias {
                out.insert(alias.clone());
            }
        }
    }
}

fn null_row_for(table: &str, alias: Option<&str>, engine: &Engine) -> Result<ResultRow, SQLError> {
    let qual = qualifier_for(table, alias);
    let mut out = ResultRow::new();
    if engine
        .try_table(table)
        .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
        .is_none()
    {
        for column in engine
            .foreign_table_columns(table)
            .map_err(SQLError::Unsupported)?
        {
            out.insert(qualified_key(&qual, &column), Value::Null);
        }
        return Ok(out);
    }
    // Emit NULLs for any column that ever appeared in the table; for an
    // empty table we still know the keys via document_count, but the
    // safe default is just an empty row - a missing key resolves to
    // NULL through ScalarExpr::Column / QualifiedColumn lookup anyway.
    let mut sample_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for id in engine.table_doc_ids(table)? {
        if let Some(doc) = engine.get_document(table, id)? {
            for k in doc.keys() {
                sample_keys.insert(k.clone());
            }
        }
        if sample_keys.len() > 16 {
            break;
        }
    }
    for k in sample_keys {
        out.insert(qualified_key(&qual, &k), Value::Null);
    }
    Ok(out)
}

/// Pull-based local-table source used by join leaves. It advances through the
/// document store with `next_doc_id`, so neither ids nor documents are copied
/// into a cardinality-sized staging vector before the physical join sees its
/// first batch.
mod cte_spill;
mod engine_functions;
mod join_predicates;
mod lateral;
mod local_table;
mod source_qualification;
mod table_function_core;
mod table_function_dispatch;
mod table_function_values;

pub(in crate::sql) use cte_spill::*;
pub(in crate::sql) use engine_functions::*;
pub(in crate::sql) use join_predicates::*;
pub(in crate::sql) use lateral::*;
pub(in crate::sql) use local_table::*;
pub(in crate::sql) use source_qualification::*;
pub(in crate::sql) use table_function_core::*;
pub(in crate::sql) use table_function_dispatch::*;
pub(in crate::sql) use table_function_values::*;

#[cfg(test)]
mod tests;
