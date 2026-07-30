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
struct EngineTableRowSource {
    table_name: String,
    table: std::sync::Arc<crate::TableState>,
    qualifier: String,
    wanted: Option<BTreeSet<String>>,
    schema: Vec<String>,
    after: Option<uqa_core::DocId>,
}

impl uqa_execution::RowSource for EngineTableRowSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn next_row(&mut self) -> uqa_execution::ExecResult<Option<ResultRow>> {
        let store = self.table.document_store.read();
        let Some(doc_id) = store.next_doc_id(self.after).map_err(|error| {
            SQLError::Internal(format!(
                "scan document ids for `{}`: {error}",
                self.table_name
            ))
        })?
        else {
            return Ok(None);
        };
        self.after = Some(doc_id);
        let document = store.get(doc_id).map_err(|error| {
            SQLError::Internal(format!(
                "read `{}` document {doc_id}: {error}",
                self.table_name
            ))
        })?;
        let document = document.ok_or_else(|| {
            SQLError::Internal(format!(
                "table `{}` cursor returned document {doc_id}, but materialization omitted it",
                self.table_name
            ))
        })?;
        let mut row = ResultRow::new();
        for (column, value) in document {
            if self
                .wanted
                .as_ref()
                .is_some_and(|wanted| !wanted.contains(&column))
            {
                continue;
            }
            row.insert(qualified_key(&self.qualifier, &column), value);
        }
        Ok(Some(row))
    }
}

fn try_streaming_local_table_scan<'a>(
    engine: &Engine,
    source: &SourcePlan,
    ctes: &CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Option<Box<dyn uqa_execution::PhysicalOperator + 'a>>, SQLError> {
    let SourcePlan::Table { name, alias } = source else {
        return Ok(None);
    };
    let qualifier = qualifier_for(name, alias.as_deref());
    if let Some(materialized) = ctes.rows.get(name).cloned() {
        if has_filters_for_qualifier(filters, &qualifier) {
            return Ok(None);
        }
        let mapping = materialized
            .schema()
            .iter()
            .filter_map(|source| {
                let column = if is_score_provenance_column(source) {
                    source.as_str()
                } else {
                    source
                        .rsplit_once('.')
                        .map_or(source.as_str(), |(_, column)| column)
                };
                if !is_score_provenance_column(source)
                    && prune
                        .and_then(|prune| prune.get(&qualifier))
                        .is_some_and(|wanted| !wanted.contains(column))
                {
                    return None;
                }
                Some((qualified_key(&qualifier, column), source.clone()))
            })
            .collect();
        let scan: Box<dyn uqa_execution::PhysicalOperator + 'a> =
            Box::new(uqa_execution::SharedSpillScan::new(materialized));
        return Ok(Some(Box::new(
            uqa_execution::ColumnSelection::with_mapping(scan, mapping),
        )));
    }
    if has_filters_for_qualifier(filters, &qualifier)
        || engine.view_plan(name)?.is_some()
        || engine
            .foreign_table(name)
            .map_err(SQLError::Unsupported)?
            .is_some()
    {
        return Ok(None);
    }
    let Some(table) = engine
        .try_table(name)
        .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?
    else {
        return Ok(None);
    };
    let wanted = prune.and_then(|prune| prune.get(&qualifier)).cloned();
    let columns = if let Some(wanted) = wanted.as_ref() {
        wanted.iter().cloned().collect()
    } else {
        engine.try_table_columns(name).map_err(|error| {
            SQLError::Internal(format!("read table columns for `{name}`: {error}"))
        })?
    };
    let schema = columns
        .into_iter()
        .map(|column| qualified_key(&qualifier, &column))
        .collect();
    let source = EngineTableRowSource {
        table_name: name.clone(),
        table,
        qualifier,
        wanted,
        schema,
        after: None,
    };
    Ok(Some(Box::new(uqa_execution::TableScan::new(Box::new(
        source,
    )))))
}

/// Build a complete FROM source as a pull-based physical operator. Unlike the
/// compatibility `build_join_rows_*` entry points below, this is the query
/// executor's primary path and never collects a join, view, CTE, derived table,
/// or table-function result into a cardinality-sized `Vec`.
pub(super) fn build_join_operator_with_ctes<'a>(
    engine: &'a Engine,
    from: &SourcePlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::{HashJoin, LateralJoin, NestedLoopJoin, PhysicalOperator};

    match from {
        SourcePlan::Table { name, alias } => {
            let qualifier = qualifier_for(name, alias.as_deref());
            if let Some(materialized) = ctes.rows.get(name).cloned() {
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(materialized));
                let operator = qualify_source_operator(scan, &qualifier, prune);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(plan) = engine.view_plan(name)? {
                let specialized_plan = filters
                    .and_then(|filters| filters.get(&qualifier))
                    .filter(|filters| !filters.is_empty())
                    .and_then(|filters| combine_filters(filters.iter().cloned()))
                    .map(|filter| {
                        push_output_filter_into_query_plan(engine, &plan, &qualifier, &filter, None)
                    })
                    .transpose()?
                    .flatten();
                let execution_plan = specialized_plan.as_ref().unwrap_or(&plan);
                let local_cte_names = query_cte_names(execution_plan);
                let is_volatile = query_contains_volatile_function(engine, execution_plan)?;
                let output = if is_volatile {
                    let mut scoped = ctes.clone();
                    execute_query_plan_output(
                        engine,
                        execution_plan,
                        params,
                        &mut scoped,
                        QueryOutputMode::SharedSpill,
                    )?
                } else {
                    execute_view_plan_output_with_parent_cache(
                        engine,
                        execution_plan,
                        params,
                        ctes,
                        &local_cte_names,
                    )?
                };
                let columns = output.internal_columns.clone();
                let shared = query_output_shared(output, "view")?;
                if !is_volatile && specialized_plan.is_none() {
                    ctes.insert_shared(name.clone(), shared.clone());
                }
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::SharedSpillScan::new(shared));
                let operator =
                    qualify_source_operator_with_columns(scan, &columns, &qualifier, prune, &[]);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(rows) = build_info_schema_rows(engine, name)? {
                let columns: Vec<String> = rows
                    .first()
                    .map(|row| row.keys().cloned().collect())
                    .unwrap_or_default();
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::TableScan::from_rows(columns.clone(), rows));
                let operator =
                    qualify_source_operator_with_columns(scan, &columns, &qualifier, prune, &[]);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if engine
                .foreign_table(name)
                .map_err(SQLError::Unsupported)?
                .is_some()
            {
                let rows = engine
                    .scan_foreign_table_stream(name, None, &[], None)
                    .map_err(SQLError::Unsupported)?;
                let columns = engine
                    .foreign_table_columns(name)
                    .map_err(SQLError::Unsupported)?;
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::RowIteratorScan::new(
                        columns.clone(),
                        Box::new(rows.map(|row| {
                            row.map_err(SQLError::Unsupported)
                                .map_err(uqa_execution::ExecError::from)
                        })),
                    ));
                let operator =
                    qualify_source_operator_with_columns(scan, &columns, &qualifier, prune, &[]);
                return Ok(attach_qualifier_filter(
                    operator, &qualifier, filters, engine, params, ctes,
                ));
            }

            if let Some(predicate) = qualifier_filter(filters, &qualifier)
                .filter(uqa_planner::optimizer::contains_retrieval)
            {
                let entries = crate::operator_tree_bridge::run_optimised(
                    engine,
                    name,
                    Some(&predicate),
                    params,
                )?
                .ok_or_else(|| {
                    SQLError::Unsupported(format!(
                        "JOIN filter retrieval predicate for `{qualifier}` cannot be represented by the shared operator IR"
                    ))
                })?;
                let table = engine.require_table(name)?;
                let columns = engine.try_table_columns(name).map_err(|error| {
                    SQLError::Internal(format!("read table columns for `{name}`: {error}"))
                })?;
                let source = ScoredDocumentSource::new(
                    name,
                    table,
                    ScoredInput::entries(entries, true),
                    columns,
                );
                let scan: Box<dyn PhysicalOperator + 'a> =
                    Box::new(uqa_execution::TableScan::new(Box::new(source)));
                return Ok(qualify_source_operator(scan, &qualifier, prune));
            }

            let Some(operator) = try_streaming_local_table_scan(engine, from, ctes, prune, None)?
            else {
                return Err(SQLError::Unsupported(format!(
                    "relation `{name}` does not exist"
                )));
            };
            Ok(attach_qualifier_filter(
                operator, &qualifier, filters, engine, params, ctes,
            ))
        }
        SourcePlan::Join {
            left,
            right,
            kind,
            on,
            lateral,
            strategy,
        } => {
            let left_filters = filters
                .and_then(|filters| propagated_join_filters(filters, right, left, on.as_ref()));
            let left_filter_ref = left_filters.as_ref().or(filters);
            let left_operator =
                build_join_operator_with_ctes(engine, left, params, ctes, prune, left_filter_ref)?;
            let implicit_lateral_function = matches!(right.as_ref(), SourcePlan::Function { .. });
            if *lateral || implicit_lateral_function {
                if !matches!(strategy, JoinExecutionStrategy::Auto) {
                    return Err(SQLError::Internal(
                        "optimizer selected a hash strategy for a lateral join".into(),
                    ));
                }
                let left_nulls = null_row_for_schema(left_operator.schema());
                let mut right_nulls = ResultRow::new();
                pad_nulls_for_from(&mut right_nulls, right, engine)?;
                let source = EngineLateralSource {
                    engine,
                    right: (**right).clone(),
                    on: on.clone(),
                    params,
                    ctes: ctes.clone(),
                };
                return Ok(Box::new(LateralJoin::new(
                    left_operator,
                    Box::new(source),
                    *kind,
                    left_nulls,
                    right_nulls,
                )));
            }

            let right_filters = filters
                .and_then(|filters| propagated_join_filters(filters, left, right, on.as_ref()));
            let right_filter_ref = right_filters.as_ref().or(filters);
            let right_operator = build_join_operator_with_ctes(
                engine,
                right,
                params,
                ctes,
                prune,
                right_filter_ref,
            )?;

            let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
            let hash_plan = if matches!(kind, JoinKind::Cross) {
                None
            } else {
                on.as_ref().and_then(|predicate| {
                    let conjuncts = join_conjuncts(predicate);
                    let scoped_hook = ScopedEngineHook::new(engine, ctes);
                    let subquery_arena =
                        PlanSubqueryArena::new(&ctes.scalar_subqueries, Some(&scoped_hook));
                    let left_sample = join_schema_sample(left_operator.schema());
                    let right_sample = join_schema_sample(right_operator.schema());
                    let mut left_keys = Vec::with_capacity(conjuncts.len());
                    let mut right_keys = Vec::with_capacity(conjuncts.len());
                    let mut residual = Vec::new();
                    for conjunct in conjuncts {
                        let ScalarExpr::Binary {
                            op: uqa_sql::ast::BinaryOp::Equal,
                            lhs,
                            rhs,
                        } = conjunct
                        else {
                            residual.push(conjunct.clone());
                            continue;
                        };
                        if let Some((left_key, right_key)) = decide_join_sides(
                            &scoped_hook,
                            &subquery_arena,
                            std::slice::from_ref(&left_sample),
                            std::slice::from_ref(&right_sample),
                            lhs,
                            rhs,
                            params,
                        ) {
                            left_keys.push(left_key.clone());
                            right_keys.push(right_key.clone());
                        } else {
                            residual.push(conjunct.clone());
                        }
                    }
                    if left_keys.is_empty() {
                        return None;
                    }
                    let residual = match residual.len() {
                        0 => None,
                        1 => residual.pop(),
                        _ => Some(ScalarExpr::And(residual)),
                    };
                    Some((left_keys, right_keys, residual))
                })
            };
            let left_nulls = null_row_for_schema(left_operator.schema());
            let right_nulls = null_row_for_schema(right_operator.schema());
            let work_mem = physical_work_mem_bytes(engine)?;
            match (strategy, hash_plan) {
                (
                    JoinExecutionStrategy::Auto | JoinExecutionStrategy::Hash,
                    Some((left_keys, right_keys, residual)),
                ) => Ok(Box::new(HashJoin::new_with_work_mem_and_predicate(
                    left_operator,
                    right_operator,
                    *kind,
                    left_keys,
                    right_keys,
                    residual,
                    evaluator,
                    left_nulls,
                    right_nulls,
                    work_mem,
                ))),
                (JoinExecutionStrategy::Auto, None) => {
                    Ok(Box::new(NestedLoopJoin::new_with_work_mem(
                        left_operator,
                        right_operator,
                        *kind,
                        on.clone(),
                        evaluator,
                        left_nulls,
                        right_nulls,
                        work_mem,
                    )))
                }
                (JoinExecutionStrategy::Hash, None) => Err(SQLError::Internal(
                    "DPccp hash-join strategy has no splittable equality predicate".into(),
                )),
            }
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
        } => {
            let hook = ScopedEngineHook::new(engine, ctes);
            let rows = build_values_rows(
                rows,
                alias.as_deref(),
                column_aliases,
                params,
                &hook,
                &hook,
                &ctes.scalar_subqueries,
            )?;
            let columns = rows.first().map_or_else(
                || {
                    column_aliases
                        .iter()
                        .map(|column| {
                            alias
                                .as_deref()
                                .map_or_else(|| column.clone(), |qual| qualified_key(qual, column))
                        })
                        .collect()
                },
                |row| row.keys().cloned().collect(),
            );
            Ok(Box::new(uqa_execution::TableScan::from_rows(columns, rows)))
        }
        SourcePlan::Function {
            name,
            args,
            alias,
            column_aliases,
            column_types,
        } => {
            let hook = ScopedEngineHook::new(engine, ctes);
            let context = TableFunctionEvalContext::new(
                engine,
                params,
                &hook,
                &hook,
                &ctes.scalar_subqueries,
            );
            let mut rows = build_table_function_row_stream(
                &context,
                name,
                args,
                alias.as_deref(),
                column_aliases,
                column_types,
            )?;
            let first = rows
                .next()
                .transpose()
                .map_err(super::select::physical_exec_error)?;
            let columns = first.as_ref().map_or_else(
                || table_function_empty_schema(name, alias.as_deref(), column_aliases),
                |row| row.keys().cloned().collect(),
            );
            let rows = first.into_iter().map(Ok).chain(rows);
            Ok(Box::new(uqa_execution::RowIteratorScan::new(
                columns,
                Box::new(rows),
            )))
        }
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let local_cte_names = query_cte_names(body);
            let output = execute_view_plan_output_with_parent_cache(
                engine,
                body,
                params,
                ctes,
                &local_cte_names,
            )?;
            let source_columns = output.internal_columns.clone();
            let operator = output.into_operator();
            let qualifier = alias.as_deref().unwrap_or_default();
            let operator = qualify_source_operator_with_columns(
                operator,
                &source_columns,
                qualifier,
                prune,
                column_aliases,
            );
            Ok(attach_qualifier_filter(
                operator, qualifier, filters, engine, params, ctes,
            ))
        }
    }
}

fn query_output_shared(
    output: QueryOutput,
    label: &str,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    let QueryRows::SharedSpill(rows) = output.rows else {
        return Err(SQLError::Internal(format!(
            "{label} execution returned in-memory rows at an internal streaming boundary"
        )));
    };
    Ok(rows)
}

fn execute_view_plan_output_with_parent_cache(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    local_cte_names: &BTreeSet<String>,
) -> Result<QueryOutput, SQLError> {
    let saved = save_and_remove_cte_names(ctes, local_cte_names);
    let result =
        execute_query_plan_output(engine, plan, params, ctes, QueryOutputMode::SharedSpill);
    restore_cte_names(ctes, saved);
    result
}

fn qualify_source_operator<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    qualifier: &str,
    prune: Option<&ColumnPrune>,
) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
    let columns = operator.schema().to_vec();
    qualify_source_operator_with_columns(operator, &columns, qualifier, prune, &[])
}

fn qualify_source_operator_with_columns<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    source_columns: &[String],
    qualifier: &str,
    prune: Option<&ColumnPrune>,
    aliases: &[String],
) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
    let mapping = source_columns
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            let source_base = if is_score_provenance_column(source) {
                source.as_str()
            } else {
                source
                    .rsplit_once('.')
                    .map_or(source.as_str(), |(_, column)| column)
            };
            let column = aliases.get(index).map_or(source_base, String::as_str);
            if !is_score_provenance_column(column)
                && !qualifier.is_empty()
                && prune
                    .and_then(|prune| prune.get(qualifier))
                    .is_some_and(|wanted| !wanted.contains(column))
            {
                return None;
            }
            let output = if qualifier.is_empty() {
                column.to_string()
            } else {
                qualified_key(qualifier, column)
            };
            Some((output, source.clone()))
        })
        .collect();
    Box::new(uqa_execution::ColumnSelection::with_mapping(
        operator, mapping,
    ))
}

fn attach_qualifier_filter<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    qualifier: &str,
    filters: Option<&QualifierFilters>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &CteScope,
) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
    let Some(predicate) = qualifier_filter(filters, qualifier) else {
        return operator;
    };
    Box::new(uqa_execution::Filter::with_evaluator(
        operator,
        predicate,
        EngineExpressionEvaluator::shared(engine, params, ctes),
    ))
}

fn null_row_for_schema(schema: &[String]) -> ResultRow {
    schema
        .iter()
        .map(|column| (column.clone(), Value::Null))
        .collect()
}

fn table_function_empty_schema(
    name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Vec<String> {
    let lower = super::builtin_function_dispatch_name(&name.to_ascii_lowercase());
    let columns = if column_aliases.is_empty() {
        match lower.as_str() {
            "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
                vec!["key".into(), "value".into()]
            }
            "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
            | "graph_betweenness" => vec!["_doc_id".into(), "_score".into()],
            "rpq" => vec!["vertex_id".into()],
            _ => vec![scalar_table_function_default_column(
                &lower,
                alias,
                column_aliases,
            )],
        }
    } else {
        column_aliases.to_vec()
    };
    match alias {
        Some(alias) => columns
            .into_iter()
            .map(|column| qualified_key(alias, &column))
            .collect(),
        None => columns,
    }
}

fn is_json_array_table_function(name: &str) -> bool {
    matches!(
        name,
        "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
    )
}

fn scalar_table_function_default_column(
    normalized_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
) -> String {
    column_aliases.first().cloned().unwrap_or_else(|| {
        if is_json_array_table_function(normalized_name) {
            "value".into()
        } else {
            alias.unwrap_or(normalized_name).to_string()
        }
    })
}

/// Materialize a repeatable FROM input under the session work-memory budget.
/// DML statements may need to rescan their source for each target row; the
/// shared spill keeps that requirement without retaining the full source in a
/// cardinality-sized vector.
pub(super) fn build_join_spill_with_ctes(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    let operator = build_join_operator_with_ctes(engine, from, params, ctes, None, None)?;
    let columns = operator.schema().to_vec();
    let output = super::select::collect_query_operator(
        engine,
        columns,
        operator,
        QueryOutputMode::SharedSpill,
    )?;
    query_output_shared(output, "DML FROM")
}

fn save_and_remove_cte_names(
    ctes: &mut CteScope,
    names: &BTreeSet<String>,
) -> Vec<(String, Option<uqa_execution::SharedSpill>)> {
    names
        .iter()
        .map(|name| (name.clone(), ctes.remove_materialized(name)))
        .collect()
}

fn restore_cte_names(
    ctes: &mut CteScope,
    saved: Vec<(String, Option<uqa_execution::SharedSpill>)>,
) {
    for (name, rows) in saved {
        match rows {
            Some(rows) => {
                ctes.rows.insert(name.clone(), rows);
            }
            None => {
                ctes.remove_materialized(&name);
            }
        }
    }
}

fn query_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_query_cte_names(plan, &mut names);
    names
}

fn collect_query_cte_names(plan: &QueryPlan, names: &mut BTreeSet<String>) {
    for cte in &plan.ctes {
        names.insert(cte.name.clone());
        collect_query_cte_names(&cte.query, names);
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_source_query_cte_names(source, names);
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            collect_query_cte_names(left, names);
            collect_query_cte_names(right, names);
        }
        RelationalPlan::Values { .. } => {}
    }
}

fn collect_source_query_cte_names(source: &SourcePlan, names: &mut BTreeSet<String>) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            collect_source_query_cte_names(left, names);
            collect_source_query_cte_names(right, names);
        }
        SourcePlan::Subquery { body, .. } => collect_query_cte_names(body, names),
        SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => {}
    }
}

fn build_values_rows(
    rows: &[Vec<ScalarExpr>],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn PhysicalSubqueryRunner,
    subqueries: &[QueryPlan],
) -> Result<Vec<ResultRow>, SQLError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let n_cols = rows[0].len();
    let columns: Vec<String> = (0..n_cols)
        .map(|i| {
            column_aliases
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("column{}", i + 1))
        })
        .collect();
    let subquery_arena = PlanSubqueryArena::new(subqueries, Some(subquery_runner));
    let ctx = ScalarEvalContext::new(None, params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(&subquery_arena);
    let mut out: Vec<ResultRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut r = ResultRow::new();
        for (i, expr) in row.iter().enumerate() {
            let v = eval_scalar(expr, &ctx)?;
            r.insert(columns[i].clone(), v);
        }
        let r = match alias {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    }
    Ok(out)
}

struct EngineLateralSource<'a> {
    engine: &'a Engine,
    right: SourcePlan,
    on: Option<ScalarExpr>,
    params: &'a [SQLParam],
    ctes: CteScope,
}

impl uqa_execution::LateralSource for EngineLateralSource<'_> {
    fn rows_for(
        &mut self,
        left_row: &ResultRow,
    ) -> uqa_execution::ExecResult<uqa_execution::LateralRows> {
        if let SourcePlan::Function {
            name,
            args,
            alias,
            column_aliases,
            column_types,
        } = &self.right
        {
            let hook = ScopedEngineHook::new(self.engine, &self.ctes);
            let context = TableFunctionEvalContext::new(
                self.engine,
                self.params,
                &hook,
                &hook,
                &self.ctes.scalar_subqueries,
            );
            return Ok(build_table_function_row_stream_with_row(
                &context,
                name,
                args,
                alias.as_deref(),
                column_aliases,
                column_types,
                Some(left_row),
            )?);
        }
        match &self.right {
            SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
            } => {
                let output = execute_lateral_subquery_output(
                    self.engine,
                    body,
                    left_row,
                    self.params,
                    &self.ctes,
                )?;
                let source_columns = output.internal_columns.clone();
                let rows = query_output_shared(output, "lateral subquery")?;
                let reader = rows.read_rows()?;
                let alias = alias.clone();
                let aliases = column_aliases.clone();
                Ok(Box::new(reader.map(move |row| {
                    let row = row?;
                    Ok(remap_subquery_row(
                        row,
                        &source_columns,
                        alias.as_deref(),
                        &aliases,
                    ))
                })))
            }
            SourcePlan::Function { .. } => Err(uqa_execution::ExecError::SQL(SQLError::Internal(
                "function source reached the relational-source fallback".into(),
            ))),
            source => {
                let operator = build_join_operator_with_ctes(
                    self.engine,
                    source,
                    self.params,
                    &mut self.ctes,
                    None,
                    None,
                )?;
                let columns = operator.schema().to_vec();
                let output = super::select::collect_query_operator(
                    self.engine,
                    columns,
                    operator,
                    QueryOutputMode::SharedSpill,
                )?;
                let rows = query_output_shared(output, "lateral source")?;
                Ok(Box::new(rows.read_rows()?))
            }
        }
    }

    fn matches(&mut self, joined: &ResultRow) -> uqa_execution::ExecResult<bool> {
        let Some(filter) = self.on.as_ref() else {
            return Ok(true);
        };
        let scoped_hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let subquery_arena =
            PlanSubqueryArena::new(&self.ctes.scalar_subqueries, Some(&scoped_hook));
        let context = ScalarEvalContext::new(Some(joined), self.params)
            .with_function_hook(&scoped_hook)
            .with_subquery_runner(&subquery_arena);
        Ok(uqa_sql::expr::truthy(&eval_scalar(filter, &context)?))
    }
}

/// Build the engine-specific correlated source and execute it through the
/// common physical `LateralJoin` operator.
#[allow(clippy::too_many_arguments)]
pub(super) fn execute_lateral_subquery_output(
    engine: &Engine,
    plan: &QueryPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<QueryOutput, SQLError> {
    let mut scoped_ctes = ctes.clone();
    super::select::materialize_plan_ctes(engine, &plan.ctes, params, &mut scoped_ctes)?;
    execute_lateral_relational_root_output(engine, &plan.root, outer_row, params, &mut scoped_ctes)
}

fn execute_lateral_relational_root_output(
    engine: &Engine,
    root: &RelationalPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<QueryOutput, SQLError> {
    match root {
        RelationalPlan::QueryBlock(block) => {
            execute_lateral_query_block_output(engine, block, outer_row, params, ctes)
        }
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
        } => {
            let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
            let lhs =
                execute_lateral_subquery_output(engine, left, outer_row, params, &scoped_ctes)?;
            let columns = lhs.columns.clone();
            let lhs = query_output_shared(lhs, "lateral set left")?;
            let rhs =
                execute_lateral_subquery_output(engine, right, outer_row, params, &scoped_ctes)?;
            let rhs = query_output_shared(rhs, "lateral set right")?;
            let order_plan =
                (!order_by.is_empty() || limit.is_some() || offset.is_some()).then(|| {
                    QueryBlockPlan {
                        projections: Vec::new(),
                        from: None,
                        r#where: None,
                        compute: ComputePlan::Project,
                        group_by: Vec::new(),
                        grouping_sets: Vec::new(),
                        having: None,
                        order_by: order_by.clone(),
                        limit: limit.as_deref().cloned(),
                        offset: offset.as_deref().cloned(),
                        distinct: false,
                        distinct_on: Vec::new(),
                        subqueries: subqueries.clone(),
                        access: AccessPathPlan::Row,
                    }
                });
            let execution = super::select::SetSpillExecution::new(
                *kind,
                *all,
                columns,
                lhs,
                rhs,
                order_plan.as_ref(),
                QueryOutputMode::SharedSpill,
            );
            super::select::combine_set_spills_with_order_output(
                engine,
                execution,
                params,
                &scoped_ctes,
            )
        }
        RelationalPlan::Values { rows, subqueries } => {
            let columns: Vec<String> = rows
                .first()
                .map(|row| {
                    (0..row.len())
                        .map(|index| format!("column{}", index + 1))
                        .collect()
                })
                .unwrap_or_default();
            let hook = ScopedEngineHook::new(engine, ctes);
            let context = super::scalar::PhysicalEvalContext::new(Some(outer_row), params)
                .with_function_hook(&hook)
                .with_subquery_runner(&hook);
            let rows = rows
                .iter()
                .map(|values| {
                    let mut row = ResultRow::new();
                    for (index, expression) in values.iter().enumerate() {
                        row.insert(
                            columns[index].clone(),
                            super::scalar::eval_physical_scalar(expression, subqueries, &context)?,
                        );
                    }
                    Ok(row)
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
                Box::new(uqa_execution::TableScan::from_rows(columns.clone(), rows));
            super::select::collect_query_operator(
                engine,
                columns,
                operator,
                QueryOutputMode::SharedSpill,
            )
        }
    }
}

fn execute_lateral_query_block_output(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    outer_row: &ResultRow,
    params: &[SQLParam],
    scoped_ctes: &mut CteScope,
) -> Result<QueryOutput, SQLError> {
    let mut scoped_ctes = scoped_ctes.enter_scalar_subqueries(&stmt.subqueries);
    let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
        if let Some(from) = stmt.from.as_ref() {
            let child =
                build_join_operator_with_ctes(engine, from, params, &mut scoped_ctes, None, None)?;
            let mut schema = outer_row.keys().cloned().collect::<Vec<_>>();
            for column in child.schema() {
                if !schema.contains(column) {
                    schema.push(column.clone());
                }
                if let Some((_, unqualified)) = column.rsplit_once('.') {
                    if !schema.iter().any(|existing| existing == unqualified) {
                        schema.push(unqualified.to_string());
                    }
                }
            }
            let outer = outer_row.clone();
            Box::new(uqa_execution::MapRows::new(
                child,
                schema,
                std::sync::Arc::new(move |inner| Ok(merge_lateral_scope_rows(&outer, &inner))),
            ))
        } else {
            Box::new(uqa_execution::TableScan::from_rows(
                outer_row.keys().cloned().collect(),
                vec![outer_row.clone()],
            ))
        };
    let columns = projection_columns(&stmt.projections);
    super::select::execute_query_block_operator_output(
        engine,
        operator,
        stmt.r#where.clone(),
        stmt,
        stmt,
        params,
        &scoped_ctes,
        columns,
        QueryOutputMode::SharedSpill,
    )
}

fn remap_subquery_row(
    mut row: ResultRow,
    source_columns: &[String],
    alias: Option<&str>,
    column_aliases: &[String],
) -> ResultRow {
    let mut output = ResultRow::new();
    for (index, source) in source_columns.iter().enumerate() {
        let target = column_aliases
            .get(index)
            .cloned()
            .unwrap_or_else(|| source.clone());
        let value = row.remove(source).unwrap_or(Value::Null);
        output.insert(target, value);
    }
    match alias {
        Some(alias) => prefix_row(alias, &output),
        None => output,
    }
}

fn merge_lateral_scope_rows(outer_row: &ResultRow, inner_row: &ResultRow) -> ResultRow {
    let mut merged = outer_row.clone();
    for (key, value) in inner_row {
        merged.insert(key.clone(), value.clone());
        if let Some((_, column)) = key.rsplit_once('.') {
            merged.insert(column.to_string(), value.clone());
        }
    }
    merged
}

pub(super) struct TableFunctionEvalContext<'a> {
    engine: &'a Engine,
    params: &'a [SQLParam],
    eval_hook: &'a dyn uqa_sql::expr::EngineHook,
    subquery_runner: &'a dyn PhysicalSubqueryRunner,
    subqueries: &'a [QueryPlan],
}

impl<'a> TableFunctionEvalContext<'a> {
    pub(super) fn new(
        engine: &'a Engine,
        params: &'a [SQLParam],
        eval_hook: &'a dyn uqa_sql::expr::EngineHook,
        subquery_runner: &'a dyn PhysicalSubqueryRunner,
        subqueries: &'a [QueryPlan],
    ) -> Self {
        Self {
            engine,
            params,
            eval_hook,
            subquery_runner,
            subqueries,
        }
    }
}

/// Build a table-function result as a fallible owned row stream. Built-in
/// cardinality-producing functions are evaluated lazily; registered/user
/// functions keep their existing vector-valued API and are adapted at this
/// explicit extension boundary.
pub(super) fn build_table_function_row_stream(
    context: &TableFunctionEvalContext<'_>,
    name: &str,
    args: &[ScalarExpr],
    alias: Option<&str>,
    column_aliases: &[String],
    column_types: &[String],
) -> Result<uqa_execution::ProjectRows, SQLError> {
    build_table_function_row_stream_with_row(
        context,
        name,
        args,
        alias,
        column_aliases,
        column_types,
        None,
    )
}

#[allow(clippy::similar_names)]
fn build_table_function_row_stream_with_row(
    context: &TableFunctionEvalContext<'_>,
    name: &str,
    args: &[ScalarExpr],
    alias: Option<&str>,
    column_aliases: &[String],
    column_types: &[String],
    row: Option<&ResultRow>,
) -> Result<uqa_execution::ProjectRows, SQLError> {
    let identity = name.to_ascii_lowercase();
    let lower = super::builtin_function_dispatch_name(&identity);
    if matches!(
        lower.as_str(),
        "generate_series"
            | "unnest"
            | "regexp_split_to_table"
            | "string_to_table"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_each"
            | "jsonb_each"
            | "json_each_text"
            | "jsonb_each_text"
    ) {
        let subquery_arena =
            PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
        let scalar_context = ScalarEvalContext::new(row, context.params)
            .with_function_hook(context.eval_hook)
            .with_subquery_runner(&subquery_arena);
        let call_args = eval_call_arguments(args, &scalar_context)?;
        if call_args.iter().any(|(name, _)| name.is_some()) {
            return Err(uqa_sql::expr::unknown_function_error(&lower, &call_args));
        }
        let evaluated: Vec<Value> = call_args.into_iter().map(|(_, value)| value).collect();
        let json_array_function = is_json_array_table_function(&lower);
        let default_col = scalar_table_function_default_column(&lower, alias, column_aliases);
        let row_builder = ScalarFunctionRowBuilder {
            default_col,
            function_name: lower.clone(),
            qualifier: alias.map(str::to_string),
            preserve_function_name: !json_array_function
                && column_aliases.is_empty()
                && alias.is_some(),
        };

        let values: Box<dyn Iterator<Item = Value> + Send> = match lower.as_str() {
            "generate_series" => generate_series_values(evaluated)?,
            "unnest" => Box::new(evaluated.into_iter().flat_map(|value| match value {
                Value::List(items) => items,
                value => vec![value],
            })),
            "regexp_split_to_table" => regexp_split_values(evaluated)?,
            "string_to_table" => string_to_table_values(evaluated)?,
            "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text" => json_array_values(&lower, evaluated)?,
            "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
                return json_each_row_stream(&lower, evaluated, alias, column_aliases);
            }
            _ => {
                return Err(SQLError::Internal(format!(
                    "streaming table function `{lower}` reached an unsupported dispatch branch"
                )));
            }
        };
        return Ok(Box::new(
            values.map(move |value| Ok(row_builder.row(value))),
        ));
    }

    if context.engine.has_registered_table_function(&identity) {
        let subquery_arena =
            PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
        let scalar_context = ScalarEvalContext::new(row, context.params)
            .with_function_hook(context.eval_hook)
            .with_subquery_runner(&subquery_arena);
        let call_args = eval_call_arguments(args, &scalar_context)?;
        if call_args.iter().any(|(name, _)| name.is_some()) {
            return Err(uqa_sql::expr::unknown_function_error(&lower, &call_args));
        }
        let evaluated = call_args
            .into_iter()
            .map(|(_, value)| value)
            .collect::<Vec<_>>();
        let result = context
            .engine
            .call_registered_table_function_stream(&identity, &evaluated)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "registered table function `{name}` disappeared during execution"
                ))
            })??;
        return registered_table_function_row_stream(name, result, alias, column_aliases);
    }

    let rows = build_table_function_rows_with_row(
        context,
        name,
        args,
        alias,
        column_aliases,
        column_types,
        row,
    )?;
    Ok(Box::new(rows.into_iter().map(Ok)))
}

#[derive(Clone)]
struct ScalarFunctionRowBuilder {
    default_col: String,
    function_name: String,
    qualifier: Option<String>,
    preserve_function_name: bool,
}

impl ScalarFunctionRowBuilder {
    fn row(&self, value: Value) -> ResultRow {
        let mut row = ResultRow::new();
        row.insert(self.default_col.clone(), value.clone());
        if self.preserve_function_name && self.default_col != self.function_name {
            row.insert(self.function_name.clone(), value);
        }
        self.qualifier
            .as_deref()
            .map_or(row.clone(), |qualifier| prefix_row(qualifier, &row))
    }
}

fn generate_series_values(
    evaluated: Vec<Value>,
) -> Result<Box<dyn Iterator<Item = Value> + Send>, SQLError> {
    if !(2..=3).contains(&evaluated.len()) {
        return Err(SQLError::TypeMismatch(
            "generate_series requires 2-3 args".into(),
        ));
    }
    let start = generate_series_integer(&evaluated[0], "start")?;
    let end = generate_series_integer(&evaluated[1], "stop")?;
    let increment = evaluated
        .get(2)
        .map_or(Ok(1), |value| generate_series_integer(value, "step"))?;
    if increment == 0 {
        return Err(SQLError::TypeMismatch(
            "generate_series step cannot be 0".into(),
        ));
    }
    let mut current = Some(start);
    Ok(Box::new(std::iter::from_fn(move || {
        let value = current?;
        if (increment > 0 && value > end) || (increment < 0 && value < end) {
            current = None;
            return None;
        }
        current = value.checked_add(increment);
        Some(Value::Int(value))
    })))
}

fn generate_series_integer(value: &Value, label: &str) -> Result<i64, SQLError> {
    match value {
        Value::Int(value) => Ok(*value),
        Value::Float(value)
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= i64::MIN as f64
                && *value < -(i64::MIN as f64) =>
        {
            Ok(*value as i64)
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "generate_series {label} must be an integer"
        ))),
    }
}

struct RegexSplitValues {
    regex: regex::Regex,
    source: String,
    piece_start: usize,
    search_start: usize,
    tail_pending: bool,
    done: bool,
}

impl Iterator for RegexSplitValues {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.tail_pending {
            self.done = true;
            return Some(Value::Str(self.source[self.piece_start..].to_string()));
        }
        let Some(found) = self.regex.find_at(&self.source, self.search_start) else {
            self.done = true;
            return Some(Value::Str(self.source[self.piece_start..].to_string()));
        };
        let piece = self.source[self.piece_start..found.start()].to_string();
        self.piece_start = found.end();
        if found.start() == found.end() {
            if found.end() == self.source.len() {
                self.tail_pending = true;
            } else {
                let advance = self.source[found.end()..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
                self.search_start = found.end().saturating_add(advance);
            }
        } else {
            self.search_start = found.end();
        }
        Some(Value::Str(piece))
    }
}

fn regexp_split_values(
    evaluated: Vec<Value>,
) -> Result<Box<dyn Iterator<Item = Value> + Send>, SQLError> {
    if evaluated.len() != 2 {
        return Err(SQLError::TypeMismatch(
            "regexp_split_to_table requires 2 args".into(),
        ));
    }
    let source = match &evaluated[0] {
        Value::Str(value) => value.clone(),
        _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 1".into())),
    };
    let pattern = match &evaluated[1] {
        Value::Str(value) => value,
        _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 2".into())),
    };
    let regex = regex::Regex::new(pattern)
        .map_err(|error| SQLError::TypeMismatch(format!("invalid regex: {error}")))?;
    Ok(Box::new(RegexSplitValues {
        regex,
        source,
        piece_start: 0,
        search_start: 0,
        tail_pending: false,
        done: false,
    }))
}

fn string_to_table_values(
    evaluated: Vec<Value>,
) -> Result<Box<dyn Iterator<Item = Value> + Send>, SQLError> {
    if evaluated.len() != 2 {
        return Err(SQLError::TypeMismatch(
            "string_to_table requires 2 args".into(),
        ));
    }
    let source = match &evaluated[0] {
        Value::Str(value) => value.clone(),
        _ => return Err(SQLError::TypeMismatch("string_to_table arg 1".into())),
    };
    let delimiter = match &evaluated[1] {
        Value::Str(value) => value.clone(),
        _ => return Err(SQLError::TypeMismatch("string_to_table arg 2".into())),
    };
    Ok(Box::new(LiteralSplitValues {
        source,
        delimiter,
        cursor: 0,
        done: false,
    }))
}

struct LiteralSplitValues {
    source: String,
    delimiter: String,
    cursor: usize,
    done: bool,
}

impl Iterator for LiteralSplitValues {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.delimiter.is_empty() {
            let value = self.source[self.cursor..].chars().next()?;
            self.cursor += value.len_utf8();
            if self.cursor == self.source.len() {
                self.done = true;
            }
            return Some(Value::Str(value.to_string()));
        }
        if let Some(relative) = self.source[self.cursor..].find(&self.delimiter) {
            let delimiter_start = self.cursor + relative;
            let piece = self.source[self.cursor..delimiter_start].to_string();
            self.cursor = delimiter_start + self.delimiter.len();
            return Some(Value::Str(piece));
        }
        self.done = true;
        Some(Value::Str(self.source[self.cursor..].to_string()))
    }
}

fn json_array_values(
    name: &str,
    evaluated: Vec<Value>,
) -> Result<Box<dyn Iterator<Item = Value> + Send>, SQLError> {
    if evaluated.len() != 1 {
        return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
    }
    let parsed = json_table_arg(&evaluated[0], name)?;
    let serde_json::Value::Array(items) = parsed else {
        return Err(SQLError::TypeMismatch(format!(
            "{name}: argument is not an array"
        )));
    };
    Ok(Box::new(
        items
            .into_iter()
            .map(|value| json_table_value_to_text(&value)),
    ))
}

fn json_each_row_stream(
    name: &str,
    evaluated: Vec<Value>,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<uqa_execution::ProjectRows, SQLError> {
    if evaluated.len() != 1 {
        return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
    }
    let parsed = json_table_arg(&evaluated[0], name)?;
    let serde_json::Value::Object(object) = parsed else {
        return Err(SQLError::TypeMismatch(format!(
            "{name}: argument is not an object"
        )));
    };
    let key_column = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| "key".into());
    let value_column = column_aliases
        .get(1)
        .cloned()
        .unwrap_or_else(|| "value".into());
    let qualifier = alias.map(str::to_string);
    Ok(Box::new(object.into_iter().map(move |(key, value)| {
        let mut row = ResultRow::new();
        row.insert(key_column.clone(), Value::Str(key));
        row.insert(value_column.clone(), json_table_value_to_text(&value));
        Ok(qualifier
            .as_deref()
            .map_or(row.clone(), |qualifier| prefix_row(qualifier, &row)))
    })))
}

#[allow(clippy::similar_names)]
fn build_table_function_rows_with_row(
    context: &TableFunctionEvalContext<'_>,
    name: &str,
    args: &[ScalarExpr],
    alias: Option<&str>,
    column_aliases: &[String],
    column_types: &[String],
    row: Option<&ResultRow>,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_sql::expr::unknown_function_error;
    let engine = context.engine;
    let subquery_arena = PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
    let ctx = ScalarEvalContext::new(row, context.params)
        .with_function_hook(context.eval_hook)
        .with_subquery_runner(&subquery_arena);
    let identity = name.to_ascii_lowercase();
    let lower = super::builtin_function_dispatch_name(&identity);
    let call_args = eval_call_arguments(args, &ctx)?;
    let has_named_args = call_args.iter().any(|(name, _)| name.is_some());
    let evaluated: Vec<Value> = call_args.iter().map(|(_, value)| value.clone()).collect();
    let default_col = column_aliases
        .first()
        .cloned()
        .unwrap_or_else(|| alias.unwrap_or(name).to_string());
    let qual = alias;
    let mut out: Vec<ResultRow> = Vec::new();
    let push_scalar = |out: &mut Vec<ResultRow>, value: Value| {
        let mut r = ResultRow::new();
        r.insert(default_col.clone(), value.clone());
        if column_aliases.is_empty() && alias.is_some() && default_col != name {
            r.insert(name.to_string(), value);
        }
        let r = match qual {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    };
    if !has_named_args {
        if let Some(result) = engine.call_registered_table_function(&identity, &evaluated) {
            return registered_table_function_rows(name, result?, qual, column_aliases);
        }
    }
    if let Some(result) =
        crate::sql::plpgsql_exec::call_user_table_function(engine, &identity, &call_args)
    {
        return registered_table_function_rows(name, result?, qual, column_aliases);
    }
    if has_named_args {
        return Err(unknown_function_error(&lower, &call_args));
    }
    match lower.as_str() {
        "generate_series" => {
            for value in generate_series_values(evaluated)? {
                push_scalar(&mut out, value);
            }
            Ok(out)
        }
        "unnest" => {
            for value in &evaluated {
                if let Value::List(items) = value {
                    for item in items {
                        push_scalar(&mut out, item.clone());
                    }
                } else {
                    push_scalar(&mut out, value.clone());
                }
            }
            Ok(out)
        }
        "regexp_split_to_table" => {
            if evaluated.len() != 2 {
                return Err(SQLError::TypeMismatch(
                    "regexp_split_to_table requires 2 args".into(),
                ));
            }
            let s = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 1".into())),
            };
            let pat = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("regexp_split_to_table arg 2".into())),
            };
            let re = regex::Regex::new(&pat)
                .map_err(|e| SQLError::TypeMismatch(format!("invalid regex: {e}")))?;
            for piece in re.split(&s) {
                push_scalar(&mut out, Value::Str(piece.to_string()));
            }
            Ok(out)
        }
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let parsed = json_table_arg(&evaluated[0], &lower)?;
            let serde_json::Value::Object(obj) = parsed else {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower}: argument is not an object"
                )));
            };
            let key_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "key".into());
            let val_col = column_aliases
                .get(1)
                .cloned()
                .unwrap_or_else(|| "value".into());
            for (k, v) in obj {
                let mut r = ResultRow::new();
                r.insert(key_col.clone(), Value::Str(k));
                r.insert(val_col.clone(), json_table_value_to_text(&v));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "json_array_elements"
        | "jsonb_array_elements"
        | "json_array_elements_text"
        | "jsonb_array_elements_text" => {
            if evaluated.len() != 1 {
                return Err(SQLError::TypeMismatch(format!("{lower} takes 1 arg")));
            }
            let parsed = json_table_arg(&evaluated[0], &lower)?;
            let serde_json::Value::Array(arr) = parsed else {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower}: argument is not an array"
                )));
            };
            let col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "value".into());
            for v in arr {
                let mut r = ResultRow::new();
                r.insert(col.clone(), json_table_value_to_text(&v));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        // -------------------------------------------------------------
        // Analyzer DDL exposed as table-functions. Mirror the canonical UQA implementation's
        // _build_create_analyzer / _build_drop_analyzer /
        // _build_list_analyzers / _build_set_table_analyzer.
        // -------------------------------------------------------------
        "create_analyzer" => {
            if evaluated.len() != 2 {
                return Err(SQLError::BadArity {
                    name: "create_analyzer".into(),
                    expected: "2".into(),
                    actual: evaluated.len(),
                });
            }
            let analyzer_name = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("create_analyzer arg 1".into())),
            };
            let config_json = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("create_analyzer arg 2".into())),
            };
            engine
                .register_named_analyzer(&analyzer_name, &config_json)
                .map_err(SQLError::Unsupported)?;
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "create_analyzer".into()),
                Value::Str(format!("analyzer '{analyzer_name}' created")),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "drop_analyzer" => {
            if evaluated.len() != 1 {
                return Err(SQLError::BadArity {
                    name: "drop_analyzer".into(),
                    expected: "1".into(),
                    actual: evaluated.len(),
                });
            }
            let analyzer_name = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("drop_analyzer arg 1".into())),
            };
            let removed = engine
                .drop_named_analyzer(&analyzer_name)
                .map_err(SQLError::Internal)?;
            if !removed {
                return Err(SQLError::Unsupported(format!(
                    "analyzer `{analyzer_name}` does not exist"
                )));
            }
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "drop_analyzer".into()),
                Value::Str(format!("analyzer '{analyzer_name}' dropped")),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "list_analyzers" => {
            if !evaluated.is_empty() {
                return Err(SQLError::BadArity {
                    name: "list_analyzers".into(),
                    expected: "0".into(),
                    actual: evaluated.len(),
                });
            }
            // Match UQA behavior for: include the four built-in analyzers
            // (`whitespace`, `standard`, `standard_cjk`, `keyword`) on
            // top of every user-registered named analyzer.
            let mut names: std::collections::BTreeSet<String> = engine
                .list_named_analyzers()
                .map_err(SQLError::Unsupported)?
                .into_iter()
                .collect();
            for builtin in ["whitespace", "standard", "standard_cjk", "keyword"] {
                names.insert(builtin.to_string());
            }
            let key = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "analyzer_name".into());
            for n in names {
                let mut r = ResultRow::new();
                r.insert(key.clone(), Value::Str(n));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "fts_index_stats" => {
            if evaluated.len() > 1 {
                return Err(SQLError::TypeMismatch(
                    "fts_index_stats accepts optional table name".into(),
                ));
            }
            let table_filter = match evaluated.first() {
                Some(Value::Str(s)) => Some(s.as_str()),
                Some(_) => return Err(SQLError::TypeMismatch("fts_index_stats arg 1".into())),
                None => None,
            };
            for stat in engine.fts_index_stats(table_filter)? {
                let mut r = ResultRow::new();
                r.insert("table_name".into(), Value::Str(stat.table_name));
                r.insert("field".into(), Value::Str(stat.field));
                r.insert("analyzer".into(), Value::Str(stat.analyzer));
                r.insert(
                    "posting_count".into(),
                    checked_integer_value(stat.posting_count, "posting count")?,
                );
                r.insert(
                    "doc_length_count".into(),
                    checked_integer_value(stat.doc_length_count, "document-length count")?,
                );
                r.insert(
                    "indexed_doc_count".into(),
                    checked_integer_value(stat.indexed_doc_count, "indexed-document count")?,
                );
                r.insert(
                    "term_count".into(),
                    checked_integer_value(stat.term_count, "term count")?,
                );
                r.insert(
                    "total_field_length".into(),
                    checked_integer_value(stat.total_field_length, "total field length")?,
                );
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "set_table_analyzer" => {
            if !(3..=4).contains(&evaluated.len()) {
                return Err(SQLError::BadArity {
                    name: "set_table_analyzer".into(),
                    expected: "3 or 4".into(),
                    actual: evaluated.len(),
                });
            }
            let target_table = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 1".into())),
            };
            let field = match &evaluated[1] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 2".into())),
            };
            let analyzer_name = match &evaluated[2] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("set_table_analyzer arg 3".into())),
            };
            let phase = if evaluated.len() > 3 {
                match &evaluated[3] {
                    Value::Str(s) => s.clone(),
                    _ => {
                        return Err(SQLError::TypeMismatch(
                            "set_table_analyzer phase must be a string".into(),
                        ));
                    }
                }
            } else {
                "both".into()
            };
            engine
                .set_table_field_analyzer(&target_table, &field, &analyzer_name, &phase)
                .map_err(SQLError::Unsupported)?;
            let mut msg = format!("analyzer '{analyzer_name}' assigned to {target_table}.{field}");
            if phase != "both" {
                use std::fmt::Write as _;
                let _ = write!(msg, " (phase={phase})");
            }
            let mut r = ResultRow::new();
            r.insert(
                column_aliases
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "set_table_analyzer".into()),
                Value::Str(msg),
            );
            let r = match qual {
                Some(a) => prefix_row(a, &r),
                None => r,
            };
            Ok(vec![r])
        }
        "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
        | "graph_betweenness" => {
            if evaluated.len() > 1 {
                return Err(SQLError::TypeMismatch(format!(
                    "{lower} accepts at most one graph argument"
                )));
            }
            let graph = expect_optional_graph_value(engine, evaluated.first(), &lower)?;
            let entries = match lower.as_str() {
                "pagerank" | "graph_pagerank" => graph_pagerank_entries(engine, &graph)?,
                "hits" | "graph_hits" => graph_hits_entries(engine, &graph)?,
                "betweenness" | "graph_betweenness" => graph_betweenness_entries(engine, &graph)?,
                _ => {
                    return Err(SQLError::Internal(format!(
                        "graph centrality function `{lower}` reached an unsupported dispatch branch"
                    )));
                }
            };
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "_doc_id".into());
            let score_col = column_aliases
                .get(1)
                .cloned()
                .unwrap_or_else(|| "_score".into());
            for entry in entries {
                let mut r = ResultRow::new();
                r.insert(id_col.clone(), doc_id_value(entry.doc_id)?);
                r.insert(score_col.clone(), Value::Float(entry.score));
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        "cypher" => {
            age_cypher::build_rows(engine, args, &evaluated, qual, column_aliases, column_types)
        }
        "rpq" => {
            if !(2..=3).contains(&evaluated.len()) {
                return Err(SQLError::TypeMismatch(
                    "rpq requires 2 or 3 args (expr, start [, graph])".into(),
                ));
            }
            let expr_str = match &evaluated[0] {
                Value::Str(s) => s.clone(),
                _ => return Err(SQLError::TypeMismatch("rpq.expr must be string".into())),
            };
            let start = match &evaluated[1] {
                Value::Int(n) => u64::try_from(*n).map_err(|_| {
                    SQLError::TypeMismatch("rpq.start must be a non-negative integer".into())
                })?,
                _ => return Err(SQLError::TypeMismatch("rpq.start must be integer".into())),
            };
            let graph = expect_optional_graph_value(engine, evaluated.get(2), "rpq")?;
            let entries = execute_tree_entries(
                engine,
                &uqa_operators::OperatorTree::RegularPathQuery {
                    rpq_source: expr_str,
                    start_vertex: start,
                    graph,
                },
            )?;
            let id_col = column_aliases
                .first()
                .cloned()
                .unwrap_or_else(|| "vertex_id".into());
            for entry in entries {
                let mut r = ResultRow::new();
                r.insert(id_col.clone(), doc_id_value(entry.doc_id)?);
                let r = match qual {
                    Some(a) => prefix_row(a, &r),
                    None => r,
                };
                out.push(r);
            }
            Ok(out)
        }
        other => Err(SQLError::Unsupported(format!(
            "table function `{other}` in FROM"
        ))),
    }
}

fn registered_table_function_rows(
    name: &str,
    result: SQLTableFunctionResult,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<Vec<ResultRow>, SQLError> {
    if result.columns.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "table function `{name}` returned no columns"
        )));
    }
    let columns: Vec<String> = result
        .columns
        .iter()
        .enumerate()
        .map(|(idx, column)| {
            column_aliases
                .get(idx)
                .cloned()
                .unwrap_or_else(|| column.clone())
        })
        .collect();
    let mut out = Vec::with_capacity(result.rows.len());
    for values in result.rows {
        if values.len() != result.columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "table function `{name}` row has {} values for {} columns",
                values.len(),
                result.columns.len()
            )));
        }
        let mut row = ResultRow::new();
        for (column, value) in columns.iter().zip(values) {
            row.insert(column.clone(), value);
        }
        let row = match alias {
            Some(alias) => prefix_row(alias, &row),
            None => row,
        };
        out.push(row);
    }
    Ok(out)
}

fn registered_table_function_row_stream(
    name: &str,
    result: SQLTableFunctionStream,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<uqa_execution::ProjectRows, SQLError> {
    if result.columns.is_empty() {
        return Err(SQLError::TypeMismatch(format!(
            "table function `{name}` returned no columns"
        )));
    }
    let expected_width = result.columns.len();
    let columns = result
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            column_aliases
                .get(index)
                .cloned()
                .unwrap_or_else(|| column.clone())
        })
        .collect::<Vec<_>>();
    let function_name = name.to_string();
    let qualifier = alias.map(str::to_string);
    Ok(Box::new(result.rows.map(
        move |values| -> uqa_execution::ExecResult<ResultRow> {
            let values = values.map_err(uqa_execution::ExecError::from)?;
            if values.len() != expected_width {
                return Err(SQLError::TypeMismatch(format!(
                "table function `{function_name}` row has {} values for {expected_width} columns",
                values.len()
            ))
                .into());
            }
            let mut row = ResultRow::new();
            for (column, value) in columns.iter().zip(values) {
                row.insert(column.clone(), value);
            }
            Ok(qualifier
                .as_deref()
                .map_or(row.clone(), |qualifier| prefix_row(qualifier, &row)))
        },
    )))
}

fn join_conjuncts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
    match expr {
        ScalarExpr::And(items) => {
            let mut conjuncts = Vec::with_capacity(items.len());
            for item in items {
                conjuncts.extend(join_conjuncts(item));
            }
            conjuncts
        }
        _ => vec![expr],
    }
}

/// Pick which expression evaluates over the left side and which over
/// the right by sampling the first row of each side. Returns
/// `(left_key_expr, right_key_expr)` when one direction works,
/// `None` when the predicate isn't separable across sides.
fn decide_join_sides<'a>(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    left_rows: &[ResultRow],
    right_rows: &[ResultRow],
    lhs: &'a ScalarExpr,
    rhs: &'a ScalarExpr,
    params: &[SQLParam],
) -> Option<(&'a ScalarExpr, &'a ScalarExpr)> {
    if left_rows.is_empty() || right_rows.is_empty() {
        return None;
    }
    let l_sample = &left_rows[0];
    let r_sample = &right_rows[0];
    let lhs_on_left = eval_yields_value(eval_hook, subquery_runner, l_sample, lhs, params);
    let rhs_on_right = eval_yields_value(eval_hook, subquery_runner, r_sample, rhs, params);
    if lhs_on_left && rhs_on_right {
        return Some((lhs, rhs));
    }
    let rhs_on_left = eval_yields_value(eval_hook, subquery_runner, l_sample, rhs, params);
    let lhs_on_right = eval_yields_value(eval_hook, subquery_runner, r_sample, lhs, params);
    if rhs_on_left && lhs_on_right {
        return Some((rhs, lhs));
    }
    None
}

fn eval_yields_value(
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn ScalarSubqueryRunner,
    row: &ResultRow,
    expr: &ScalarExpr,
    params: &[SQLParam],
) -> bool {
    let ctx = ScalarEvalContext::new(Some(row), params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(subquery_runner);
    matches!(eval_scalar(expr, &ctx), Ok(v) if v != uqa_core::Value::Null)
}

fn pad_nulls_for_from(
    row: &mut ResultRow,
    from: &SourcePlan,
    engine: &Engine,
) -> Result<(), SQLError> {
    let mut tables = Vec::new();
    from.collect_tables(&mut tables);
    for (name, alias) in &tables {
        let null_keys = null_row_for(name, alias.as_deref(), engine)?;
        for (k, v) in null_keys {
            row.entry(k).or_insert(v);
        }
    }
    Ok(())
}

fn join_schema_sample(columns: &[String]) -> ResultRow {
    columns
        .iter()
        .map(|column| (column.clone(), Value::Int(1)))
        .collect()
}

pub(super) fn engine_func_intercept(
    engine: Option<&Engine>,
    name: &str,
    args: &[ScalarExpr],
    row: &ResultRow,
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Option<Value>, SQLError> {
    let lower = super::builtin_function_dispatch_name(name);
    match lower.as_str() {
        "uqa_highlight" => Ok(Some(run_uqa_highlight(row, args, evaluate)?)),
        "score_bm25" | "score_bayesian_bm25" => {
            validate_score_projection_args(&lower, args, evaluate)?;
            let score = score_projection_value(&lower, args, row)?;
            Ok(Some(score))
        }
        "deep_learn" => Ok(Some(run_deep_learn_projection(
            require_projection_engine(engine, "deep_learn")?,
            args,
            evaluate,
        )?)),
        "merge_action" => {
            if !args.is_empty() {
                return Err(SQLError::BadArity {
                    name: "merge_action".into(),
                    expected: "0".into(),
                    actual: args.len(),
                });
            }
            let action = row.get(MERGE_ACTION_COLUMN).cloned().ok_or_else(|| {
                SQLError::Unsupported("merge_action() is only valid in MERGE RETURNING".into())
            })?;
            Ok(Some(action))
        }
        // UQA-native helpers keep their lenient semantics.
        "graph_create" => {
            let eng = require_projection_engine(engine, "graph_create")?;
            Ok(Some(Value::Bool(run_graph_create_with_evaluator(
                eng, args, evaluate,
            )?)))
        }
        "graph_drop" => {
            let eng = require_projection_engine(engine, "graph_drop")?;
            Ok(Some(Value::Bool(run_graph_drop_with_evaluator(
                eng, args, evaluate,
            )?)))
        }
        // Apache AGE-compatible functions: strict name validation and
        // a void (SQL NULL) return value.
        "create_graph" => Ok(Some(run_age_create_graph_with_evaluator(
            require_projection_engine(engine, "create_graph")?,
            args,
            evaluate,
        )?)),
        "drop_graph" => Ok(Some(run_age_drop_graph_with_evaluator(
            require_projection_engine(engine, "drop_graph")?,
            args,
            evaluate,
        )?)),
        _ => Ok(None),
    }
}

fn score_projection_value(
    function: &str,
    args: &[ScalarExpr],
    row: &ResultRow,
) -> Result<Value, SQLError> {
    if args.len() == 2 {
        if let ScalarExpr::QualifiedColumn { qualifier, .. } = &args[0] {
            let prefix = format!("{qualifier}.");
            let mut scores = row.iter().filter_map(|(column, value)| {
                (column.starts_with(&prefix) && is_score_provenance_column(column))
                    .then_some(value)
                    .and_then(|value| match value {
                        Value::Float(score) => Some(*score),
                        _ => None,
                    })
            });
            let Some(score) = scores.next() else {
                return Err(score_projection_context_error(function));
            };
            if scores.next().is_some() {
                return Err(SQLError::Unsupported(format!(
                    "{function}() has multiple score-bearing retrieval rows for `{qualifier}`"
                )));
            }
            return Ok(Value::Float(score));
        }
    }

    if let Some(Value::Float(score)) = row.get(SCORE_PROVENANCE_COLUMN) {
        return Ok(Value::Float(*score));
    }
    let mut scores = row.iter().filter_map(|(column, value)| {
        (is_score_provenance_column(column) && column != SCORE_PROVENANCE_COLUMN)
            .then_some(value)
            .and_then(|value| match value {
                Value::Float(score) => Some(*score),
                _ => None,
            })
    });
    let Some(score) = scores.next() else {
        return Err(score_projection_context_error(function));
    };
    if scores.next().is_some() {
        return Err(SQLError::Unsupported(format!(
            "{function}() has multiple score-bearing retrieval rows; qualify its field argument"
        )));
    }
    Ok(Value::Float(score))
}

fn score_projection_context_error(function: &str) -> SQLError {
    SQLError::Unsupported(format!(
        "{function}() requires a score-bearing retrieval row"
    ))
}

fn require_projection_engine<'a>(
    engine: Option<&'a Engine>,
    function: &str,
) -> Result<&'a Engine, SQLError> {
    engine.ok_or_else(|| {
        SQLError::Unsupported(format!("{function} requires an engine-backed projection"))
    })
}

fn run_deep_learn_projection(
    engine: &Engine,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    if args.len() != 2 {
        return Err(SQLError::BadArity {
            name: "deep_learn".into(),
            expected: "2".into(),
            actual: args.len(),
        });
    }
    let model_name = match evaluate(&args[0])? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.model must be a string, got {other:?}"
            )));
        }
    };
    let training_source = match evaluate(&args[1])? {
        Value::Str(s) => s,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "deep_learn.training_set must be a table name or JSON string, got {other:?}"
            )));
        }
    };
    let trimmed = training_source.trim();
    let output = if trimmed.starts_with('{') {
        engine.deep_learn_json(&model_name, trimmed, &uqa_ml::LearnOptions::default())?
    } else {
        engine.deep_learn_table(
            &model_name,
            &training_source,
            &uqa_ml::LearnOptions::default(),
        )?
    };
    let mut report = BTreeMap::new();
    report.insert("model".into(), Value::Str(model_name));
    report.insert(
        "examples".into(),
        checked_integer_value(output.report.examples, "training example count")?,
    );
    report.insert(
        "feature_dimensions".into(),
        checked_integer_value(output.report.feature_dimensions, "feature dimension count")?,
    );
    report.insert(
        "class_count".into(),
        checked_integer_value(output.report.class_count, "class count")?,
    );
    Ok(Value::Map(report))
}

fn validate_score_projection_args(
    name: &str,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<(), SQLError> {
    if !(1..=2).contains(&args.len()) {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "1..=2".into(),
            actual: args.len(),
        });
    }
    let query_idx = args.len() - 1;
    if args.len() == 2 {
        let _ = expect_column_name(&args[0], &format!("{name}.field"))?;
    }
    match evaluate(&args[query_idx])? {
        Value::Str(_) => Ok(()),
        other => Err(SQLError::TypeMismatch(format!(
            "{name}.query must be a string, got {other:?}"
        ))),
    }
}

/// Evaluate a `uqa_highlight(field, query[, start_tag, end_tag,
/// max_fragments, fragment_size])` projection. Matches UQA
/// reference's `_run_uqa_highlight` shape: field can be either a
/// bare column reference (looked up on the row) or a literal string;
/// the rest of the args are scalar literals after evaluation.
fn run_uqa_highlight(
    row: &ResultRow,
    args: &[ScalarExpr],
    evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
) -> Result<Value, SQLError> {
    if args.len() < 2 || args.len() > 6 {
        return Err(SQLError::BadArity {
            name: "uqa_highlight".into(),
            expected: "2..=6".into(),
            actual: args.len(),
        });
    }
    let text = match &args[0] {
        ScalarExpr::Column(c) => match super::row_column_value(row, c) {
            Some(Value::Str(s)) => s.clone(),
            Some(Value::Null) => return Ok(Value::Null),
            Some(other) => format!("{other:?}"),
            None => return Ok(Value::Null),
        },
        ScalarExpr::QualifiedColumn {
            qualifier,
            column,
            key,
        } => {
            let fallback_key;
            let lookup_key = if key.is_empty() {
                fallback_key = qualified_key(qualifier, column);
                fallback_key.as_str()
            } else {
                key.as_str()
            };
            match row
                .get(lookup_key)
                .or_else(|| uqa_sql::expr::unqualified_fallback(row, column))
            {
                Some(Value::Str(s)) => s.clone(),
                Some(Value::Null) => return Ok(Value::Null),
                Some(other) => format!("{other:?}"),
                None => return Ok(Value::Null),
            }
        }
        other => match evaluate(other)? {
            Value::Str(s) => s,
            Value::Null => return Ok(Value::Null),
            v => format!("{v:?}"),
        },
    };
    let query_str = match evaluate(&args[1])? {
        Value::Str(s) => s,
        Value::Null => return Ok(Value::Str(text)),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "uqa_highlight query must be string, got {other:?}"
            )));
        }
    };
    let start_tag = match args.get(2) {
        Some(e) => match evaluate(e)? {
            Value::Str(s) => s,
            Value::Null => "<b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight start_tag must be string, got {other:?}"
                )));
            }
        },
        None => "<b>".into(),
    };
    let end_tag = match args.get(3) {
        Some(e) => match evaluate(e)? {
            Value::Str(s) => s,
            Value::Null => "</b>".into(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight end_tag must be string, got {other:?}"
                )));
            }
        },
        None => "</b>".into(),
    };
    let max_fragments = match args.get(4) {
        Some(e) => match evaluate(e)? {
            Value::Int(n) if n >= 0 => usize::try_from(n).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "uqa_highlight max_fragments {n} exceeds the platform usize range"
                ))
            })?,
            Value::Null => 0,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight max_fragments must be non-negative integer, got {other:?}"
                )));
            }
        },
        None => 0,
    };
    let fragment_size = match args.get(5) {
        Some(e) => match evaluate(e)? {
            Value::Int(n) if n > 0 => usize::try_from(n).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "uqa_highlight fragment_size {n} exceeds the platform usize range"
                ))
            })?,
            Value::Null => 150,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "uqa_highlight fragment_size must be positive integer, got {other:?}"
                )));
            }
        },
        None => 150,
    };
    let opts = uqa_analysis::HighlightOptions {
        start_tag,
        end_tag,
        max_fragments,
        fragment_size,
    };
    // Pull every whitespace-separated token out of the query string
    // as a candidate match term. The canonical UQA behavior parses the FTS
    // query, but a simple split is what callers reach for in
    // practice and matches what the test fixture exercises.
    let terms: Vec<String> = query_str
        .split_whitespace()
        .filter(|t| !matches!(t.to_ascii_lowercase().as_str(), "and" | "or" | "not"))
        .map(std::string::ToString::to_string)
        .collect();
    let analyzer = uqa_analysis::standard_analyzer("english");
    let out = uqa_analysis::highlight(&text, &terms, Some(&analyzer), &opts)
        .map_err(|error| SQLError::Internal(format!("highlight analysis failed: {error}")))?;
    Ok(Value::Str(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_filters_handles_empty_and_single_inputs_without_panicking() {
        assert!(combine_filters(Vec::<ScalarExpr>::new()).is_none());
        let combined = combine_filters([ScalarExpr::Literal(Value::Bool(true))]);
        assert!(matches!(
            combined,
            Some(ScalarExpr::Literal(Value::Bool(true)))
        ));
    }

    #[test]
    fn engine_backed_projection_functions_reject_a_missing_engine_context() {
        let row = ResultRow::new();
        for function in [
            "deep_learn",
            "graph_create",
            "graph_drop",
            "create_graph",
            "drop_graph",
        ] {
            let mut evaluate = |_: &ScalarExpr| Ok(Value::Null);
            let error = engine_func_intercept(None, function, &[], &row, &mut evaluate)
                .expect_err("engine-backed functions must not report success without an engine");
            assert!(
                matches!(
                    &error,
                    SQLError::Unsupported(message)
                        if message == &format!("{function} requires an engine-backed projection")
                ),
                "unexpected {function} error: {error:?}"
            );
        }
    }

    #[test]
    fn score_projection_uses_explicit_provenance_even_for_zero() {
        let args = [ScalarExpr::Literal(Value::Str("query".into()))];
        let mut evaluate = |expr: &ScalarExpr| match expr {
            ScalarExpr::Literal(value) => Ok(value.clone()),
            _ => Ok(Value::Null),
        };
        let scored_row = ResultRow::from([
            (super::super::SCORE_COLUMN.into(), Value::Float(99.0)),
            (SCORE_PROVENANCE_COLUMN.into(), Value::Float(0.0)),
        ]);
        assert_eq!(
            engine_func_intercept(None, "score_bm25", &args, &scored_row, &mut evaluate).unwrap(),
            Some(Value::Float(0.0))
        );

        let unscored_row = ResultRow::from([
            (super::super::SCORE_COLUMN.into(), Value::Float(0.0)),
            (SCORE_PROVENANCE_COLUMN.into(), Value::Null),
        ]);
        let error = engine_func_intercept(None, "score_bm25", &args, &unscored_row, &mut evaluate)
            .unwrap_err();
        assert!(error.to_string().contains("score-bearing"), "{error}");
    }
}
