//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM/JOIN row assembly, table functions, and projection intercepts.

use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_execution::{eval_call_arguments, eval_scalar, ScalarEvalContext, ScalarExpr};
use uqa_planner::{
    AccessPathPlan, ComputePlan, JoinExecutionStrategy, QueryBlockPlan, QueryPlan, RelationalPlan,
    SourcePlan,
};
use uqa_sql::ast::JoinKind;
use uqa_sql::{ResultRow, SQLError, SQLParam};

use crate::{Engine, SQLTableFunctionResult, SQLTableFunctionStream};

use super::scalar::{PhysicalSubqueryRunner, PlanSubqueryArena};
use super::select::{
    execute_query_plan_output, physical_work_mem_bytes, push_output_filter_into_query_plan,
    CteScope, EngineExpressionEvaluator, HierarchyScoredDocumentSource, QueryOutput,
    QueryOutputMode, QueryRows, ScopedEngineHook, ScoredDocumentSource, ScoredInput,
    ScoredSourceAttributes,
};
use super::volatility::query_contains_volatile_function;
use super::{
    age_cypher, build_info_schema_rows, doc_id_value, execute_tree_entries, expect_column_name,
    expect_optional_graph_value, graph_betweenness_entries, graph_hits_entries,
    graph_pagerank_entries, json_table_arg, json_table_value_to_text, projection_columns,
    run_age_alter_graph_with_evaluator, run_age_create_elabel_with_evaluator,
    run_age_create_graph_with_evaluator, run_age_create_vlabel_with_evaluator,
    run_age_drop_graph_with_evaluator, run_age_drop_label_with_evaluator,
    run_age_graph_exists_with_evaluator, run_graph_create_with_evaluator,
    run_graph_drop_with_evaluator, TABLE_OID_COLUMN, XMIN_COLUMN,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::sql) struct RelationMetadataProjection(u8);

impl RelationMetadataProjection {
    const DOC_ID: u8 = 1;
    const SCORE: u8 = 2;

    pub(in crate::sql) fn request_doc_id(&mut self) {
        self.0 |= Self::DOC_ID;
    }

    pub(in crate::sql) fn request_score(&mut self) {
        self.0 |= Self::SCORE;
    }

    pub(in crate::sql) fn includes_doc_id(self) -> bool {
        self.0 & Self::DOC_ID != 0
    }

    pub(in crate::sql) fn includes_score(self) -> bool {
        self.0 & Self::SCORE != 0
    }

    pub(in crate::sql) fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, Default)]
pub(in crate::sql) struct SourceProjection {
    columns: BTreeSet<String>,
    retain_all: bool,
    metadata: RelationMetadataProjection,
}

impl SourceProjection {
    pub(super) fn retaining_all() -> Self {
        Self {
            retain_all: true,
            ..Self::default()
        }
    }

    pub(super) fn contains(&self, column: &str) -> bool {
        self.retain_all || self.columns.contains(column)
    }

    pub(super) fn retain_all(&mut self) {
        self.retain_all = true;
    }

    pub(super) fn insert(&mut self, column: String) {
        self.columns.insert(column);
    }

    pub(super) fn extend(&mut self, columns: impl IntoIterator<Item = String>) {
        self.columns.extend(columns);
    }

    pub(in crate::sql) fn explicit_columns(self) -> Option<BTreeSet<String>> {
        (!self.retain_all).then_some(self.columns)
    }

    pub(in crate::sql) fn metadata(&self) -> RelationMetadataProjection {
        self.metadata
    }

    pub(super) fn metadata_mut(&mut self) -> &mut RelationMetadataProjection {
        &mut self.metadata
    }
}

pub(super) type ColumnPrune = BTreeMap<String, SourceProjection>;
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

fn qualifier_for(qualifier: &str, alias: Option<&str>) -> String {
    alias.unwrap_or(qualifier).to_string()
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
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if let Some(alias) = alias {
                out.insert(alias.clone());
            } else {
                collect_from_qualifiers(left, out);
                collect_from_qualifiers(right, out);
            }
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => {
            if let Some(qualifier) = from.visible_qualifier() {
                out.insert(qualifier.to_string());
            }
        }
    }
}

/// Pull-based local-table source used by join leaves. It advances through the
/// document store with `next_doc_id`, so neither ids nor documents are copied
/// into a cardinality-sized staging vector before the physical join sees its
/// first batch.
mod cte_spill;
mod engine_functions;
mod join_predicates;
mod join_using;
mod lateral;
mod local_table;
mod source_qualification;
mod table_function_core;
mod table_function_dispatch;
mod table_function_values;

pub(in crate::sql) use cte_spill::*;
pub(in crate::sql) use engine_functions::*;
pub(in crate::sql) use join_predicates::*;
pub(in crate::sql) use join_using::*;
pub(in crate::sql) use lateral::*;
pub(in crate::sql) use local_table::*;
pub(in crate::sql) use source_qualification::*;
pub(in crate::sql) use table_function_core::*;
pub(in crate::sql) use table_function_dispatch::*;
pub(in crate::sql) use table_function_values::*;

#[cfg(test)]
mod tests;
