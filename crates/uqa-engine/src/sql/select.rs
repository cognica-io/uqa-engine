//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL SELECT, set-operation, `CtePlan`, ordering, and projection execution.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use smallvec::SmallVec;
use uqa_core::DocId;
use uqa_execution::{
    eval_scalar, ExecResult, ExpressionEvaluator, ScalarEvalContext, ScalarExpr, ScalarFrameBound,
    SharedExpressionEvaluator,
};
use uqa_planner::{
    AccessPathPlan, ComputePlan, CtePlan, ProjectionPlan, QueryBlockPlan, QueryPlan,
    RelationalPlan, SourcePlan, UnifiedPlan,
};

use super::from_rows::execute_lateral_subquery_output;
use super::scalar::{eval_physical_scalar, PhysicalEvalContext, PhysicalSubqueryRunner};
use super::volatility::{expr_contains_volatile_function, query_contains_volatile_function};
use super::{
    contains_aggregate, doc_id_value, engine_func_intercept, execute_function,
    execute_function_with_top_k, execute_mixed_where, expect_column_name, has_aggregate,
    has_window, is_score_provenance_column, optimize_engine_plan, prepare_window_plan,
    projection_label_at, BTreeMap, BTreeSet, BinaryOp, ColumnPrune, Document, Engine,
    PhysicalAggregateExecutor, PhysicalWindowExecutor, QualifierFilters, ResultRow, SQLError,
    SQLParam, SQLResult, ScoredEntry, SetOpKind, Value, DOC_ID_COLUMN, MERGE_ACTION_COLUMN,
    SCORE_COLUMN, SCORE_PROVENANCE_COLUMN,
};

mod cte_execution;
mod evaluation;
mod expression_shape;
mod facet_projection;
mod filter_pushdown;
mod foreign_access;
mod physical_plan;
mod query_block;
mod recursive_cte;
mod schema_binding;
mod scored_input;
mod set_projection;
mod table_access;

pub(in crate::sql) use cte_execution::*;
pub(crate) use evaluation::CteScope;
pub(in crate::sql) use evaluation::{
    expr_contains_subquery, prepare_correlated_exists_predicate, DirectColumnKey,
    EngineExpressionEvaluator, ScopedEngineHook,
};
pub(in crate::sql) use expression_shape::*;
pub(in crate::sql) use facet_projection::*;
pub(in crate::sql) use filter_pushdown::*;
pub(in crate::sql) use foreign_access::*;
pub(in crate::sql) use physical_plan::*;
pub(in crate::sql) use query_block::*;
pub(in crate::sql) use recursive_cte::*;
pub(in crate::sql) use schema_binding::*;
pub(in crate::sql) use scored_input::*;
pub(in crate::sql) use set_projection::*;
pub(in crate::sql) use table_access::*;

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

type PhysicalProjection = (String, ScalarExpr);
type OutputColumnMapping = (String, String);

/// Execute the physical relational plan directly. CTEs, set-operation
/// branches, values, and query blocks recurse through plan children; query
/// blocks select physical access and row operators without reconstructing a
/// parser statement.
pub(super) fn execute_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut ctes = CteScope::new();
    execute_query_plan_with_ctes(engine, plan, params, &mut ctes)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum QueryOutputMode {
    Rows,
    SharedSpill,
    ExistsKeySet,
}

pub(super) enum QueryRows {
    Rows {
        named: Vec<ResultRow>,
        positional: Option<Vec<Vec<Value>>>,
    },
    SharedSpill(uqa_execution::SharedSpill),
    ExistsKeySet(uqa_execution::CanonicalRowHashSet),
}

pub(super) struct QueryOutput {
    pub(super) columns: Vec<String>,
    pub(super) column_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    /// Physical columns include internal row metadata that is available to a
    /// parent query block but never exposed through [`SQLResult`].
    pub(super) internal_columns: Vec<String>,
    pub(super) internal_types: Vec<Option<uqa_sql::ast::ColumnType>>,
    pub(super) rows: QueryRows,
}

impl QueryOutput {
    pub(super) fn into_cursor(self) -> Result<super::SQLCursor, SQLError> {
        match self.rows {
            QueryRows::SharedSpill(rows) => {
                super::SQLCursor::from_spill(self.columns, self.column_types, rows)
            }
            QueryRows::Rows { .. } | QueryRows::ExistsKeySet(_) => Err(SQLError::Internal(
                "cursor query unexpectedly used unbounded row materialization".into(),
            )),
        }
    }

    pub(super) fn into_sql_result(self) -> Result<SQLResult, SQLError> {
        let (mut rows, positional_rows) = match self.rows {
            QueryRows::Rows { named, positional } => (named, positional),
            QueryRows::SharedSpill(rows) => {
                let mut scan = uqa_execution::SharedSpillScan::new(rows);
                (
                    uqa_execution::physical::run_to_rows(&mut scan)
                        .map_err(physical_exec_error)?
                        .1,
                    None,
                )
            }
            QueryRows::ExistsKeySet(_) => {
                return Err(SQLError::Internal(
                    "EXISTS key-set output cannot become a SQL result".into(),
                ));
            }
        };
        for row in &mut rows {
            row.retain(|column, _| !is_score_provenance_column(column));
        }
        Ok(SQLResult::from_typed_rows_with_positions(
            self.columns,
            self.column_types,
            rows,
            positional_rows,
        ))
    }

    pub(super) fn into_operator<'a>(self) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
        match self.rows {
            QueryRows::Rows { named, .. } => Box::new(uqa_execution::TableScan::from_typed_rows(
                self.internal_columns,
                self.internal_types,
                named,
            )),
            QueryRows::SharedSpill(rows) => Box::new(uqa_execution::SharedSpillScan::new(rows)),
            QueryRows::ExistsKeySet(_) => {
                panic!("EXISTS key-set output cannot become a physical operator")
            }
        }
    }

    fn into_subquery_result(self) -> Result<uqa_execution::SubqueryResult, SQLError> {
        let QueryOutput {
            columns,
            column_types: _,
            internal_columns,
            internal_types,
            rows,
        } = self;
        let rows: Box<
            dyn Iterator<Item = Result<uqa_execution::OwnedPhysicalRow, SQLError>> + Send,
        > = match rows {
            QueryRows::Rows { named, .. } => {
                let schema = uqa_execution::RowSchema::with_types(internal_columns, internal_types);
                Box::new(named.into_iter().map(move |row| {
                    Ok(uqa_execution::OwnedPhysicalRow::new(
                        schema.clone(),
                        uqa_execution::PhysicalRow::from_result_row(&schema, row),
                    ))
                }))
            }
            QueryRows::SharedSpill(rows) => Box::new(
                rows.read_rows()
                    .map_err(physical_exec_error)?
                    .map(|row| row.map_err(physical_exec_error)),
            ),
            QueryRows::ExistsKeySet(_) => {
                return Err(SQLError::Internal(
                    "EXISTS key-set output cannot become a scalar subquery result".into(),
                ));
            }
        };
        Ok(uqa_execution::SubqueryResult { columns, rows })
    }
}

/// Execute a physical query plan while preserving the caller's CTE scope.
pub(super) fn execute_query_plan_with_ctes(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<SQLResult, SQLError> {
    execute_query_plan_output(engine, plan, params, ctes, QueryOutputMode::Rows)?.into_sql_result()
}

pub(super) fn execute_query_plan_output(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    if !plan.ctes.is_empty() {
        let filters = cte_output_filters(engine, plan);
        materialize_plan_ctes_with_filters(engine, &plan.ctes, params, ctes, &filters)?;
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            execute_query_block_output(engine, block, params, ctes, output_mode)
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
            // Materialize each child directly into a disk-backed, repeatable
            // stream before starting the next child. A nested set operation
            // therefore never owns two cardinality-sized `SQLResult.rows`
            // vectors, and its external merge consumes batches under
            // `work_mem`.
            let lhs = execute_query_plan_output(
                engine,
                left,
                params,
                ctes,
                QueryOutputMode::SharedSpill,
            )?;
            let columns = lhs.columns.clone();
            let left: Box<dyn uqa_execution::PhysicalOperator + '_> = lhs.into_operator();
            let rhs = execute_query_plan_output(
                engine,
                right,
                params,
                ctes,
                QueryOutputMode::SharedSpill,
            )?;
            let right: Box<dyn uqa_execution::PhysicalOperator + '_> = rhs.into_operator();
            let operation: Box<dyn uqa_execution::PhysicalOperator + '_> = Box::new(
                uqa_execution::ExternalSetOperation::new(
                    left,
                    right,
                    *kind,
                    *all,
                    physical_work_mem_bytes(engine)?,
                )
                .map_err(physical_exec_error)?,
            );
            if !order_by.is_empty() || limit.is_some() || offset.is_some() {
                let synthetic = QueryBlockPlan {
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
                };
                let ordering_scope = ctes.enter_scalar_subqueries(subqueries);
                let evaluator = EngineExpressionEvaluator::shared(engine, params, &ordering_scope);
                let output = identity_order_columns(&columns);
                let operation = attach_order_limit(
                    operation,
                    &synthetic,
                    &output,
                    engine,
                    params,
                    &ordering_scope,
                    evaluator,
                )?;
                return collect_query_operator(engine, columns, operation, output_mode);
            }
            collect_query_operator(engine, columns, operation, output_mode)
        }
        RelationalPlan::Values { rows, subqueries } => {
            validate_values_set_contexts(engine, rows)?;
            execute_plan_values_output(engine, rows, subqueries, params, ctes, output_mode)
        }
    }
}

pub(super) fn collect_query_operator<'a>(
    engine: &Engine,
    columns: Vec<String>,
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let internal_schema = operator.row_schema().clone();
    let internal_columns = internal_schema.columns().to_vec();
    let internal_types = internal_schema.column_types().to_vec();
    let column_types = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            if internal_schema.columns().get(index) == Some(column) {
                internal_schema.column_type(index).cloned()
            } else {
                internal_schema
                    .position(column)
                    .and_then(|position| internal_schema.column_type(position).cloned())
            }
        })
        .collect();
    let rows = match output_mode {
        QueryOutputMode::Rows => {
            let has_duplicate_labels = {
                let mut seen = std::collections::BTreeSet::new();
                columns.iter().any(|column| !seen.insert(column))
            };
            if has_duplicate_labels {
                let batches = uqa_execution::physical::run_to_batches(operator.as_mut())
                    .map_err(physical_exec_error)?;
                let mut named = Vec::new();
                let mut positional = Vec::new();
                for batch in batches {
                    let columnar =
                        uqa_execution::ColumnarBatch::from_batch(&columns, batch.clone());
                    positional.extend(columnar.into_positional_rows());
                    named.extend(batch.into_result_rows());
                }
                QueryRows::Rows {
                    named,
                    positional: Some(positional),
                }
            } else {
                QueryRows::Rows {
                    named: uqa_execution::physical::run_to_rows(operator.as_mut())
                        .map_err(physical_exec_error)?
                        .1,
                    positional: None,
                }
            }
        }
        QueryOutputMode::SharedSpill => {
            let mut buffer =
                uqa_execution::SpillBuffer::new(physical_work_mem_bytes(engine)?.max(1));
            if let Err(error) = operator.open() {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "open",
                ));
            }
            loop {
                let batch = match operator.next() {
                    Ok(batch) => batch,
                    Err(error) => {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "execution",
                        ));
                    }
                };
                let Some(batch) = batch else {
                    break;
                };
                if let Err(error) = buffer.push(batch) {
                    return Err(close_after_physical_failure(
                        operator.as_mut(),
                        error,
                        "spill buffering",
                    ));
                }
            }
            operator.close().map_err(physical_exec_error)?;
            QueryRows::SharedSpill(
                buffer
                    .into_shared(internal_schema)
                    .map_err(physical_exec_error)?,
            )
        }
        QueryOutputMode::ExistsKeySet => {
            let key_positions = columns
                .iter()
                .map(|column| {
                    operator.row_schema().position(column).ok_or_else(|| {
                        SQLError::Internal(format!(
                            "decorrelated EXISTS result is missing key column `{column}`"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut keys = uqa_execution::CanonicalRowHashSet::new();
            if let Err(error) = operator.open() {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "open EXISTS key input",
                ));
            }
            loop {
                let batch = match operator.next() {
                    Ok(batch) => batch,
                    Err(error) => {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "collect EXISTS keys",
                        ));
                    }
                };
                let Some(batch) = batch else {
                    break;
                };
                for row in &batch.rows {
                    let view = batch.schema.view(row);
                    let mut key = SmallVec::<[&Value; 4]>::with_capacity(key_positions.len());
                    let mut contains_null = false;
                    for position in &key_positions {
                        let Some(value) = view.value_at(*position) else {
                            contains_null = true;
                            break;
                        };
                        if matches!(value, Value::Null) {
                            contains_null = true;
                            break;
                        }
                        key.push(value);
                    }
                    if !contains_null {
                        if let Err(error) = keys.insert_borrowed(&key) {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                error,
                                "hash EXISTS keys",
                            ));
                        }
                    }
                }
            }
            operator.close().map_err(physical_exec_error)?;
            QueryRows::ExistsKeySet(keys)
        }
    };
    Ok(QueryOutput {
        columns,
        column_types,
        internal_columns,
        internal_types,
        rows,
    })
}

/// Collect decorrelated EXISTS keys directly from the filtered input. Direct
/// column expressions stay as borrowed physical values; non-trivial key
/// expressions are evaluated into an inline buffer. In either case there is
/// no projected `PhysicalRow` materialization between the input and hash set.
pub(in crate::sql) fn collect_exists_key_operator<'a>(
    columns: Vec<String>,
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    projections: &[ProjectionPlan],
    evaluator: SharedExpressionEvaluator<'a>,
) -> Result<QueryOutput, SQLError> {
    let internal_columns = operator.schema().to_vec();
    let internal_types = operator.row_schema().column_types().to_vec();
    let column_types = projections
        .iter()
        .map(|projection| {
            uqa_execution::scalar_type(
                &projection.expr,
                operator.row_schema(),
                evaluator.parameters(),
            )
            .ok()
            .flatten()
        })
        .collect();
    let direct_columns = projections
        .iter()
        .map(|projection| DirectColumnKey::compile(&projection.expr))
        .collect::<Option<Vec<_>>>();
    let mut keys = uqa_execution::CanonicalRowHashSet::new();
    if let Err(error) = operator.open() {
        return Err(close_after_physical_failure(
            operator.as_mut(),
            error,
            "open EXISTS key input",
        ));
    }
    loop {
        let batch = match operator.next() {
            Ok(batch) => batch,
            Err(error) => {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "collect EXISTS key input",
                ));
            }
        };
        let Some(batch) = batch else {
            break;
        };
        for row in &batch.rows {
            let view = batch.schema.view(row);
            let inserted = if let Some(direct_columns) = direct_columns.as_ref() {
                let mut key = SmallVec::<[&Value; 4]>::with_capacity(direct_columns.len());
                let mut contains_null = false;
                for column in direct_columns {
                    let Some(value) = column.value(&view) else {
                        contains_null = true;
                        break;
                    };
                    if matches!(value, Value::Null) {
                        contains_null = true;
                        break;
                    }
                    key.push(value);
                }
                if contains_null {
                    Ok(false)
                } else {
                    keys.insert_borrowed(&key)
                }
            } else {
                let mut key = SmallVec::<[Value; 4]>::with_capacity(projections.len());
                let mut contains_null = false;
                for projection in projections {
                    let value = match evaluator.evaluate(&projection.expr, &view) {
                        Ok(value) => value,
                        Err(error) => {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                error,
                                "evaluate EXISTS key",
                            ));
                        }
                    };
                    if matches!(value, Value::Null) {
                        contains_null = true;
                        break;
                    }
                    key.push(value);
                }
                if contains_null {
                    Ok(false)
                } else {
                    keys.insert_values(&key)
                }
            };
            if let Err(error) = inserted {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "hash EXISTS key",
                ));
            }
        }
    }
    operator.close().map_err(physical_exec_error)?;
    Ok(QueryOutput {
        internal_columns,
        internal_types,
        column_types,
        columns,
        rows: QueryRows::ExistsKeySet(keys),
    })
}

fn execute_query_block_output(
    engine: &Engine,
    block: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let mut scoped_ctes = ctes.enter_scalar_subqueries(&block.subqueries);
    let defer_distinct_limit = should_defer_distinct_limit(block);
    let execution = select_execution_stmt(block, defer_distinct_limit);
    run_query_block_with_prepared_exists_output(
        engine,
        block,
        &execution,
        params,
        &mut scoped_ctes,
        output_mode,
    )
}

pub(super) struct SetSpillExecution<'a> {
    kind: SetOpKind,
    all: bool,
    columns: Vec<String>,
    lhs: uqa_execution::SharedSpill,
    rhs: uqa_execution::SharedSpill,
    order_plan: Option<&'a QueryBlockPlan>,
    output_mode: QueryOutputMode,
}

impl<'a> SetSpillExecution<'a> {
    pub(super) fn new(
        kind: SetOpKind,
        all: bool,
        columns: Vec<String>,
        lhs: uqa_execution::SharedSpill,
        rhs: uqa_execution::SharedSpill,
        order_plan: Option<&'a QueryBlockPlan>,
        output_mode: QueryOutputMode,
    ) -> Self {
        Self {
            kind,
            all,
            columns,
            lhs,
            rhs,
            order_plan,
            output_mode,
        }
    }
}

pub(super) fn combine_set_spills_with_order_output(
    engine: &Engine,
    execution: SetSpillExecution<'_>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<QueryOutput, SQLError> {
    use uqa_execution::{ExternalSetOperation, PhysicalOperator};

    let left: Box<dyn PhysicalOperator> =
        Box::new(uqa_execution::SharedSpillScan::new(execution.lhs));
    let right: Box<dyn PhysicalOperator> =
        Box::new(uqa_execution::SharedSpillScan::new(execution.rhs));
    let mut operation: Box<dyn PhysicalOperator + '_> = Box::new(
        ExternalSetOperation::new(
            left,
            right,
            execution.kind,
            execution.all,
            physical_work_mem_bytes(engine)?,
        )
        .map_err(physical_exec_error)?,
    );
    if let Some(order_plan) = execution.order_plan {
        let output = identity_order_columns(&execution.columns);
        operation = attach_order_limit(
            operation,
            order_plan,
            &output,
            engine,
            params,
            ctes,
            EngineExpressionEvaluator::shared(engine, params, ctes),
        )?;
    }
    collect_query_operator(engine, execution.columns, operation, execution.output_mode)
}

fn execute_plan_values_output(
    engine: &Engine,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    if rows.is_empty() {
        let scan: Box<dyn uqa_execution::PhysicalOperator + '_> =
            Box::new(uqa_execution::TableScan::from_rows(Vec::new(), Vec::new()));
        return collect_query_operator(engine, Vec::new(), scan, output_mode);
    }
    let columns: Vec<String> = (0..rows[0].len())
        .map(|index| format!("column{}", index + 1))
        .collect();
    let hook = ScopedEngineHook::new(engine, ctes);
    let context = PhysicalEvalContext::new(None, params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    let mut output = Vec::with_capacity(rows.len());
    for source in rows {
        if source.len() != columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "VALUES row width {} does not match first row width {}",
                source.len(),
                columns.len()
            )));
        }
        let mut row = ResultRow::new();
        for (index, expression) in source.iter().enumerate() {
            row.insert(
                columns[index].clone(),
                eval_physical_scalar(expression, subqueries, &context)?,
            );
        }
        output.push(row);
    }
    let scan: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::TableScan::from_rows(columns.clone(), output));
    collect_query_operator(engine, columns, scan, output_mode)
}

#[cfg(test)]
mod physical_failure_tests;
