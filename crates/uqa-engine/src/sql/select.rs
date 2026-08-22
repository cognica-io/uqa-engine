//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL SELECT, set-operation, `CtePlan`, ordering, and projection execution.

use std::cell::Cell;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::rc::Rc;
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
use super::scalar::{
    eval_physical_scalar, PhysicalEvalContext, PhysicalOuterRow, PhysicalSubqueryRunner,
};
use super::volatility::{expr_contains_volatile_function, query_contains_volatile_function};
use super::{
    contains_aggregate, doc_id_value, engine_func_intercept, execute_function,
    execute_function_with_top_k, execute_mixed_where, expect_column_name, has_aggregate,
    has_window, is_score_provenance_column, optimize_engine_plan, prepare_window_plan,
    projection_label_at, BTreeMap, BTreeSet, BinaryOp, ColumnPrune, ColumnType, Engine,
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
mod grouping_sets;
mod physical_plan;
mod query_block;
mod recursive_cte;
mod row_lock_recheck;
mod row_lock_retry_cache;
mod row_locking;
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
pub(in crate::sql) use grouping_sets::*;
pub(in crate::sql) use physical_plan::*;
pub(in crate::sql) use query_block::*;
pub(in crate::sql) use recursive_cte::*;
pub(in crate::sql) use row_lock_recheck::*;
pub(crate) use row_lock_retry_cache::{RetryRowOverride, RowLockRetryCache};
pub(in crate::sql) use row_locking::*;
pub(in crate::sql) use schema_binding::*;
pub(in crate::sql) use scored_input::*;
pub(in crate::sql) use set_projection::*;
pub(in crate::sql) use table_access::*;

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

type PhysicalProjection = (String, ScalarExpr);
/// Public output label paired with the bound expression that addresses its physical value after relational binding. Positional expressions preserve repeated labels without inventing SQL-visible names.
type OutputColumnMapping = (String, ScalarExpr);

#[derive(Clone, Copy)]
pub(in crate::sql) struct SingleRelation<'a> {
    pub storage_name: &'a str,
    pub qualifier: &'a str,
}

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

pub(in crate::sql) trait QueryRowConsumer {
    fn begin(
        &self,
        engine: &Engine,
        columns: &[String],
        schema: &uqa_execution::RowSchema,
    ) -> Result<(), SQLError>;

    fn consume(
        &self,
        engine: &Engine,
        row: uqa_execution::OwnedPhysicalRow,
    ) -> Result<QueryConsumerControl, SQLError>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::sql) enum QueryConsumerControl {
    Continue,
    Stop,
}

struct SetOperationRowConsumer {
    downstream: Rc<dyn QueryRowConsumer>,
    columns: Vec<String>,
    schema: uqa_execution::RowSchema,
    offset: Cell<u64>,
    remaining: Cell<Option<u64>>,
    stopped: Cell<bool>,
}

impl SetOperationRowConsumer {
    fn new(
        downstream: Rc<dyn QueryRowConsumer>,
        schema: uqa_execution::RowSchema,
        offset: u64,
        limit: Option<u64>,
    ) -> Self {
        Self {
            columns: schema.columns().to_vec(),
            downstream,
            schema,
            offset: Cell::new(offset),
            remaining: Cell::new(limit),
            stopped: Cell::new(limit == Some(0)),
        }
    }

    fn stopped(&self) -> bool {
        self.stopped.get()
    }
}

impl QueryRowConsumer for SetOperationRowConsumer {
    fn begin(
        &self,
        engine: &Engine,
        columns: &[String],
        _schema: &uqa_execution::RowSchema,
    ) -> Result<(), SQLError> {
        if columns.len() != self.columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "set-operation input width {} does not match output width {}",
                columns.len(),
                self.columns.len()
            )));
        }
        self.downstream.begin(engine, &self.columns, &self.schema)
    }

    fn consume(
        &self,
        engine: &Engine,
        row: uqa_execution::OwnedPhysicalRow,
    ) -> Result<QueryConsumerControl, SQLError> {
        if self.stopped() {
            return Ok(QueryConsumerControl::Stop);
        }
        if self.offset.get() > 0 {
            self.offset.set(self.offset.get() - 1);
            return Ok(QueryConsumerControl::Continue);
        }
        if self.remaining.get() == Some(0) {
            self.stopped.set(true);
            return Ok(QueryConsumerControl::Stop);
        }
        let projections = {
            let view = row.view();
            self.schema
                .column_types()
                .iter()
                .enumerate()
                .map(|(position, target_type)| {
                    let source_type = row.schema.column_type(position);
                    if target_type
                        .as_ref()
                        .is_some_and(|target_type| source_type != Some(target_type))
                    {
                        let value = view.value_at(position).cloned().unwrap_or(Value::Null);
                        return coerce_common_context_value(
                            value,
                            source_type,
                            target_type.as_ref(),
                        )
                        .map(uqa_execution::RowProjectionValue::Owned);
                    }
                    Ok(row.schema.physical_slot(position).map_or(
                        uqa_execution::RowProjectionValue::Owned(Value::Null),
                        uqa_execution::RowProjectionValue::InputSlot,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?
        };
        let control = self.downstream.consume(
            engine,
            uqa_execution::OwnedPhysicalRow::new(
                self.schema.clone(),
                row.row
                    .project_with_values(projections)
                    .without_lock_origins(),
            ),
        )?;
        if matches!(control, QueryConsumerControl::Stop) {
            self.stopped.set(true);
            return Ok(control);
        }
        if let Some(remaining) = self.remaining.get() {
            let remaining = remaining - 1;
            self.remaining.set(Some(remaining));
            if remaining == 0 {
                self.stopped.set(true);
                return Ok(QueryConsumerControl::Stop);
            }
        }
        Ok(QueryConsumerControl::Continue)
    }
}

#[derive(Clone)]
pub(super) enum QueryOutputMode {
    Rows,
    SharedSpill,
    ExistsKeySet,
    RowConsumer(Rc<dyn QueryRowConsumer>),
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
        if self
            .internal_columns
            .iter()
            .any(|column| is_score_provenance_column(column))
        {
            for row in &mut rows {
                row.retain(|column, _| !is_score_provenance_column(column));
            }
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

    pub(super) fn into_public_operator<'a>(self) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
        let columns = self.columns.clone();
        let public_width = columns.len();
        let operator = self.into_operator();
        let positions = columns
            .into_iter()
            .enumerate()
            .map(|(position, column)| (column, position))
            .collect();
        debug_assert!(operator.row_schema().len() >= public_width);
        Box::new(uqa_execution::ColumnSelection::with_positions(
            operator, positions,
        ))
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
    let mut visible_ctes = ctes.enter_visible_ctes(plan.ctes.iter().map(|cte| cte.name.as_str()));
    let ctes = &mut *visible_ctes;
    if !plan.ctes.is_empty() {
        let ordered_ctes = ordered_plan_ctes(plan)?;
        let reachable = reachable_plan_cte_names(plan);
        let single_reference = single_reference_plan_cte_names(plan);
        let recursive = ordered_ctes
            .iter()
            .copied()
            .filter(|cte| cte_references_own_name(cte))
            .map(|cte| cte.name.as_str())
            .collect::<BTreeSet<_>>();
        for cte in ordered_ctes.iter().copied().filter(|cte| {
            !recursive.contains(cte.name.as_str())
                && reachable.contains(&cte.name)
                && single_reference.contains(&cte.name)
                && matches!(
                    query_contains_volatile_function(engine, &cte.query),
                    Ok(false)
                )
        }) {
            ctes.insert_deferred(cte.clone());
        }
        let filters = cte_output_filters(engine, plan);
        materialize_plan_ctes_with_filters(
            engine,
            ordered_ctes.into_iter().filter(|cte| {
                reachable.contains(&cte.name)
                    && (recursive.contains(cte.name.as_str())
                        || !single_reference.contains(&cte.name)
                        || !matches!(
                            query_contains_volatile_function(engine, &cte.query),
                            Ok(false)
                        ))
            }),
            params,
            ctes,
            &filters,
        )?;
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
            with_ties,
            offset,
            subqueries,
        } => {
            let set_schema = bind_query_plan_schema(engine, plan, params, ctes, None)?;
            let streaming_consumer = match &output_mode {
                QueryOutputMode::RowConsumer(downstream)
                    if matches!((*kind, *all), (SetOpKind::Union, true))
                        && order_by.is_empty()
                        && !*with_ties =>
                {
                    Some(Rc::clone(downstream))
                }
                _ => None,
            };
            if let Some(downstream) = streaming_consumer {
                let columns = set_schema.columns().to_vec();
                let column_types = set_schema.column_types().to_vec();
                let (resolved_offset, resolved_limit) = {
                    let scoped_ctes = ctes.enter_scalar_subqueries(subqueries);
                    (
                        resolve_limit_offset_with_ctes(
                            offset.as_deref(),
                            engine,
                            params,
                            "OFFSET",
                            &scoped_ctes,
                        )?
                        .unwrap_or(0),
                        resolve_limit_offset_with_ctes(
                            limit.as_deref(),
                            engine,
                            params,
                            "LIMIT",
                            &scoped_ctes,
                        )?,
                    )
                };
                let consumer = Rc::new(SetOperationRowConsumer::new(
                    Rc::clone(&downstream),
                    set_schema.clone(),
                    resolved_offset,
                    resolved_limit,
                ));
                if consumer.stopped() {
                    downstream.begin(engine, &columns, &set_schema)?;
                } else {
                    let mut child_ctes = ctes.enter_lock_identity_emission(false);
                    execute_query_plan_output(
                        engine,
                        left,
                        params,
                        &mut child_ctes,
                        QueryOutputMode::RowConsumer(consumer.clone()),
                    )?;
                    if !consumer.stopped() {
                        execute_query_plan_output(
                            engine,
                            right,
                            params,
                            &mut child_ctes,
                            QueryOutputMode::RowConsumer(consumer),
                        )?;
                    }
                }
                return Ok(QueryOutput {
                    columns: columns.clone(),
                    column_types: column_types.clone(),
                    internal_columns: columns,
                    internal_types: column_types,
                    rows: QueryRows::Rows {
                        named: Vec::new(),
                        positional: None,
                    },
                });
            }
            // Materialize each child directly into a disk-backed, repeatable
            // stream before starting the next child. A nested set operation
            // therefore never owns two cardinality-sized `SQLResult.rows`
            // vectors, and its external merge consumes batches under
            // `work_mem`.
            let (lhs, rhs) = {
                let mut child_ctes = ctes.enter_lock_identity_emission(false);
                let lhs = execute_query_plan_output(
                    engine,
                    left,
                    params,
                    &mut child_ctes,
                    QueryOutputMode::SharedSpill,
                )?;
                let rhs = execute_query_plan_output(
                    engine,
                    right,
                    params,
                    &mut child_ctes,
                    QueryOutputMode::SharedSpill,
                )?;
                (lhs, rhs)
            };
            let columns = lhs.columns.clone();
            let left: Box<dyn uqa_execution::PhysicalOperator + '_> = lhs.into_public_operator();
            let right: Box<dyn uqa_execution::PhysicalOperator + '_> = rhs.into_public_operator();
            let operation: Box<dyn uqa_execution::PhysicalOperator + '_> = Box::new(
                uqa_execution::ExternalSetOperation::new_with_types(
                    left,
                    right,
                    *kind,
                    *all,
                    set_schema.column_types().to_vec(),
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
                    group_distinct: false,
                    having: None,
                    order_by: order_by.clone(),
                    limit: limit.as_deref().cloned(),
                    with_ties: *with_ties,
                    offset: offset.as_deref().cloned(),
                    distinct: false,
                    distinct_on: Vec::new(),
                    subqueries: subqueries.clone(),
                    access: AccessPathPlan::Row,
                    locking: Vec::new(),
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
                    None,
                )?;
                return collect_query_operator(engine, columns, operation, output_mode);
            }
            collect_query_operator(engine, columns, operation, output_mode)
        }
        RelationalPlan::Values { rows, subqueries } => {
            validate_values_set_contexts(
                engine,
                rows,
                &uqa_execution::RowSchema::default(),
                params,
            )?;
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
        QueryOutputMode::RowConsumer(consumer) => {
            consumer.begin(engine, &columns, &internal_schema)?;
            if let Err(error) = operator.open() {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "open row consumer input",
                ));
            }
            'consume: loop {
                let batch = match operator.next() {
                    Ok(batch) => batch,
                    Err(error) => {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "execute row consumer input",
                        ));
                    }
                };
                let Some(batch) = batch else {
                    break;
                };
                let uqa_execution::Batch { schema, rows } = batch;
                for row in rows {
                    let row = uqa_execution::OwnedPhysicalRow::new(schema.clone(), row);
                    match consumer.consume(engine, row) {
                        Ok(QueryConsumerControl::Continue) => {}
                        Ok(QueryConsumerControl::Stop) => break 'consume,
                        Err(error) => {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                uqa_execution::ExecError::SQL(error),
                                "consume query row",
                            ));
                        }
                    }
                }
            }
            operator.close().map_err(physical_exec_error)?;
            QueryRows::Rows {
                named: Vec::new(),
                positional: None,
            }
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
                    let value =
                        match evaluator.evaluate_physical(&projection.expr, &batch.schema, row) {
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
    let inherited_lock_identities = ctes.lock_identities.emit;
    let mut scoped_ctes = ctes.enter_scalar_subqueries(&block.subqueries);
    let row_identity_barrier = block.distinct
        || !block.distinct_on.is_empty()
        || matches!(block.compute, ComputePlan::Aggregate | ComputePlan::Window);
    scoped_ctes.lock_identities.emit =
        !block.locking.is_empty() || (inherited_lock_identities && !row_identity_barrier);
    scoped_ctes.lock_identities.retain_after_lock =
        inherited_lock_identities && !row_identity_barrier;
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

    let public_positions = || {
        execution
            .columns
            .iter()
            .cloned()
            .enumerate()
            .map(|(position, column)| (column, position))
            .collect::<Vec<_>>()
    };
    let left: Box<dyn PhysicalOperator> = Box::new(uqa_execution::ColumnSelection::with_positions(
        Box::new(uqa_execution::SharedSpillScan::new(execution.lhs)),
        public_positions(),
    ));
    let right: Box<dyn PhysicalOperator> =
        Box::new(uqa_execution::ColumnSelection::with_positions(
            Box::new(uqa_execution::SharedSpillScan::new(execution.rhs)),
            public_positions(),
        ));
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
            None,
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
            Box::new(uqa_execution::TableScan::from_physical_rows(
                uqa_execution::RowSchema::default(),
                Vec::new(),
            ));
        return collect_query_operator(engine, Vec::new(), scan, output_mode);
    }
    let columns: Vec<String> = (0..rows[0].len())
        .map(|index| format!("column{}", index + 1))
        .collect();
    let column_types = values_types_in_scope(engine, rows, subqueries, None, params, ctes)?;
    let empty_schema = uqa_execution::RowSchema::default();
    let hook = ScopedEngineHook::new(engine, ctes);
    let context = PhysicalEvalContext::new(None, params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    let schema = uqa_execution::RowSchema::with_types(columns.clone(), column_types.clone());
    let consumer = match &output_mode {
        QueryOutputMode::RowConsumer(consumer) => {
            consumer.begin(engine, &columns, &schema)?;
            Some(consumer)
        }
        QueryOutputMode::Rows | QueryOutputMode::SharedSpill | QueryOutputMode::ExistsKeySet => {
            None
        }
    };
    let mut output = consumer.is_none().then(|| Vec::with_capacity(rows.len()));
    for source in rows {
        if source.len() != columns.len() {
            return Err(SQLError::TypeMismatch(format!(
                "VALUES row width {} does not match first row width {}",
                source.len(),
                columns.len()
            )));
        }
        let mut values = Vec::with_capacity(source.len());
        for (index, expression) in source.iter().enumerate() {
            let source_type = uqa_execution::common_context_expression_type(
                expression,
                &empty_schema,
                params,
                Some(engine),
            )?;
            let value = eval_physical_scalar(expression, subqueries, &context)?;
            values.push(coerce_common_context_value(
                value,
                source_type.as_ref(),
                column_types[index].as_ref(),
            )?);
        }
        let row = uqa_execution::PhysicalRow::from_values(values);
        if let Some(consumer) = consumer {
            if matches!(
                consumer.consume(
                    engine,
                    uqa_execution::OwnedPhysicalRow::new(schema.clone(), row),
                )?,
                QueryConsumerControl::Stop
            ) {
                break;
            }
        } else if let Some(output) = output.as_mut() {
            output.push(row);
        }
    }
    if consumer.is_some() {
        return Ok(QueryOutput {
            columns,
            column_types,
            internal_columns: schema.columns().to_vec(),
            internal_types: schema.column_types().to_vec(),
            rows: QueryRows::Rows {
                named: Vec::new(),
                positional: None,
            },
        });
    }
    let scan: Box<dyn uqa_execution::PhysicalOperator + '_> = Box::new(
        uqa_execution::TableScan::from_physical_rows(schema, output.unwrap_or_default()),
    );
    collect_query_operator(engine, columns, scan, output_mode)
}

pub(in crate::sql) fn coerce_common_context_value(
    value: Value,
    source_type: Option<&ColumnType>,
    target_type: Option<&ColumnType>,
) -> Result<Value, SQLError> {
    let Some(target_type) = target_type else {
        return Ok(value);
    };
    if source_type == Some(target_type) {
        return Ok(value);
    }
    let cast_target = match target_type {
        ColumnType::Domain { base, .. } => base.as_ref(),
        target => target,
    };
    let source_name = source_type.map(ColumnType::sql_name);
    uqa_sql::expr::cast_value_from(&value, &cast_target.sql_name(), source_name.as_deref())
}

#[cfg(test)]
mod physical_failure_tests;
