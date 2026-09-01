//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL SELECT, set-operation, `CtePlan`, ordering, and projection execution.

use std::cell::Cell;
use std::fmt::Write as _;
use std::rc::Rc;
use std::sync::Arc;

use smallvec::SmallVec;
use uqa_core::DocId;
pub(in crate::sql) use uqa_execution::ProjectionTarget;
use uqa_execution::{
    eval_scalar, ExecResult, ScalarEvalContext, ScalarExpr, SharedExpressionEvaluator,
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
    has_window, optimize_engine_plan, prepare_window_plan, projection_label_at, BTreeMap, BTreeSet,
    BinaryOp, ColumnPrune, ColumnType, Engine, PhysicalAggregateExecutor, PhysicalWindowExecutor,
    QualifierFilters, ResultRow, SQLError, SQLParam, SQLResult, ScoredEntry, SetOpKind, Value,
    DOC_ID_COLUMN, SCORE_COLUMN, TABLE_OID_COLUMN, XMIN_COLUMN,
};

mod cte_execution;
mod directional_plan;
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
use directional_plan::DirectionalQueryPlanOperator;
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

type PhysicalProjection = (uqa_execution::ProjectionTarget, ScalarExpr);
/// Public output label paired with the bound expression that addresses its physical value after relational binding. Positional expressions preserve repeated labels without inventing SQL-visible names.
type OutputColumnMapping = (String, ScalarExpr);

#[derive(Clone, Copy)]
pub(in crate::sql) struct SingleRelation<'a> {
    pub relation_name: &'a str,
    pub qualifier: &'a str,
}

/// Execute the physical relational plan directly. CTEs, set-operation branches, values, and query blocks recurse through plan children; query blocks select physical access and row operators without reconstructing a parser statement.
pub(crate) fn execute_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut ctes = CteScope::new_for_current_routine(engine);
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

    fn uses_directional_scan(&self) -> bool {
        false
    }

    fn directional_scan_prepared(
        &self,
        _engine: &Engine,
        _support: uqa_execution::BackwardScanSupport,
    ) -> Result<(), SQLError> {
        Ok(())
    }

    fn scan_direction(&self) -> uqa_execution::PhysicalScanDirection {
        uqa_execution::PhysicalScanDirection::Forward
    }

    fn direction_exhausted(&self, _engine: &Engine) -> Result<QueryConsumerControl, SQLError> {
        Ok(QueryConsumerControl::Stop)
    }

    fn rewound(&self, _engine: &Engine) -> Result<QueryConsumerControl, SQLError> {
        Ok(QueryConsumerControl::Continue)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::sql) enum QueryConsumerControl {
    Continue,
    Stop,
    Rewind,
}

struct SetOperationRowConsumer {
    downstream: Rc<dyn QueryRowConsumer>,
    columns: Vec<String>,
    schema: uqa_execution::RowSchema,
    offset: Cell<u64>,
    remaining: Cell<Option<u64>>,
    begun: Cell<bool>,
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
            begun: Cell::new(false),
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
        if self.begun.replace(true) {
            Ok(())
        } else {
            self.downstream.begin(engine, &self.columns, &self.schema)
        }
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
        match control {
            QueryConsumerControl::Continue => {}
            QueryConsumerControl::Stop => {
                self.stopped.set(true);
                return Ok(control);
            }
            QueryConsumerControl::Rewind => {
                return Err(SQLError::Internal(
                    "set-operation consumer received a directional rewind".into(),
                ));
            }
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
    /// Physical columns include internal row metadata that is available to a parent query block but never exposed through [`SQLResult`].
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
        let (rows, positional_rows) = match self.rows {
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
mod execution;
pub(in crate::sql) use execution::collect_exists_key_operator;
pub(super) use execution::{
    collect_query_operator, execute_query_plan_output, execute_query_plan_with_ctes,
};
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
    let mut execution = select_execution_stmt(block, defer_distinct_limit);
    let outer = scoped_ctes.row_lock_outer_row().map(|row| &row.schema);
    if let Some(source) = execution.from.as_mut() {
        bind_source_plan_schema_for_execution(engine, source, params, &scoped_ctes, outer)?;
    }
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
            physical_work_mem_bytes(engine.query_runtime_view())?,
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
            engine.query_runtime_view(),
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
    if matches!(
        &output_mode,
        QueryOutputMode::RowConsumer(consumer) if consumer.uses_directional_scan()
    ) {
        return execute_directional_values_output(
            engine,
            rows,
            subqueries,
            params,
            ctes,
            output_mode,
        );
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

fn execute_directional_values_output(
    engine: &Engine,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let columns: Vec<String> = (0..rows[0].len())
        .map(|index| format!("column{}", index + 1))
        .collect();
    let column_types = values_types_in_scope(engine, rows, subqueries, None, params, ctes)?;
    let empty_schema = uqa_execution::RowSchema::default();
    let source_types = rows
        .iter()
        .map(|source| {
            if source.len() != columns.len() {
                return Err(SQLError::TypeMismatch(format!(
                    "VALUES row width {} does not match first row width {}",
                    source.len(),
                    columns.len()
                )));
            }
            source
                .iter()
                .map(|expression| {
                    uqa_execution::common_context_expression_type(
                        expression,
                        &empty_schema,
                        params,
                        Some(engine),
                    )
                })
                .collect::<Result<Vec<_>, SQLError>>()
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut values_ctes = ctes.clone();
    values_ctes.scalar_subqueries = subqueries.to_vec();
    let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(DirectionalValuesScan::new(
            rows,
            source_types,
            uqa_execution::RowSchema::with_types(columns.clone(), column_types),
            EngineExpressionEvaluator::shared(engine, params, &values_ctes),
        ));
    collect_query_operator(engine, columns, operator, output_mode)
}

#[derive(Clone, Copy)]
enum ValuesScanPosition {
    BeforeFirst,
    OnRow(usize),
    AfterLast,
}

struct DirectionalValuesScan<'a> {
    rows: &'a [Vec<ScalarExpr>],
    source_types: Vec<Vec<Option<ColumnType>>>,
    schema: uqa_execution::RowSchema,
    evaluator: SharedExpressionEvaluator<'a>,
    position: ValuesScanPosition,
}

impl<'a> DirectionalValuesScan<'a> {
    fn new(
        rows: &'a [Vec<ScalarExpr>],
        source_types: Vec<Vec<Option<ColumnType>>>,
        schema: uqa_execution::RowSchema,
        evaluator: SharedExpressionEvaluator<'a>,
    ) -> Self {
        Self {
            rows,
            source_types,
            schema,
            evaluator,
            position: ValuesScanPosition::BeforeFirst,
        }
    }

    fn evaluate(&self, position: usize) -> uqa_execution::ExecResult<uqa_execution::Batch> {
        let empty_schema = uqa_execution::RowSchema::default();
        let empty_row = uqa_execution::PhysicalRow::default();
        let values = self.rows[position]
            .iter()
            .enumerate()
            .map(|(column, expression)| {
                let value =
                    self.evaluator
                        .evaluate_physical(expression, &empty_schema, &empty_row)?;
                coerce_common_context_value(
                    value,
                    self.source_types[position][column].as_ref(),
                    self.schema.column_type(column),
                )
                .map_err(uqa_execution::ExecError::SQL)
            })
            .collect::<uqa_execution::ExecResult<Vec<_>>>()?;
        Ok(uqa_execution::Batch::from_physical_rows(
            self.schema.clone(),
            vec![uqa_execution::PhysicalRow::from_values(values)],
        ))
    }

    fn next_in_direction(
        &mut self,
        direction: uqa_execution::PhysicalScanDirection,
    ) -> uqa_execution::ExecResult<Option<uqa_execution::Batch>> {
        let target = match (direction, self.position) {
            (uqa_execution::PhysicalScanDirection::Forward, ValuesScanPosition::BeforeFirst) => 0,
            (
                uqa_execution::PhysicalScanDirection::Forward,
                ValuesScanPosition::OnRow(position),
            ) => position.saturating_add(1),
            (uqa_execution::PhysicalScanDirection::Forward, ValuesScanPosition::AfterLast)
            | (uqa_execution::PhysicalScanDirection::Backward, ValuesScanPosition::BeforeFirst) => {
                return Ok(None)
            }
            (uqa_execution::PhysicalScanDirection::Backward, ValuesScanPosition::OnRow(0)) => {
                self.position = ValuesScanPosition::BeforeFirst;
                return Ok(None);
            }
            (
                uqa_execution::PhysicalScanDirection::Backward,
                ValuesScanPosition::OnRow(position),
            ) => position - 1,
            (uqa_execution::PhysicalScanDirection::Backward, ValuesScanPosition::AfterLast) => {
                let Some(position) = self.rows.len().checked_sub(1) else {
                    self.position = ValuesScanPosition::BeforeFirst;
                    return Ok(None);
                };
                position
            }
        };
        if target >= self.rows.len() {
            self.position = ValuesScanPosition::AfterLast;
            return Ok(None);
        }
        self.position = ValuesScanPosition::OnRow(target);
        self.evaluate(target).map(Some)
    }
}

impl uqa_execution::PhysicalOperator for DirectionalValuesScan<'_> {
    fn row_schema(&self) -> &uqa_execution::RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        u64::try_from(self.rows.len()).ok()
    }

    fn backward_scan_support(&self) -> uqa_execution::BackwardScanSupport {
        uqa_execution::BackwardScanSupport::Native
    }

    fn open(&mut self) -> uqa_execution::ExecResult<()> {
        self.position = ValuesScanPosition::BeforeFirst;
        Ok(())
    }

    fn next(&mut self) -> uqa_execution::ExecResult<Option<uqa_execution::Batch>> {
        self.next_in_direction(uqa_execution::PhysicalScanDirection::Forward)
    }

    fn next_direction(
        &mut self,
        direction: uqa_execution::PhysicalScanDirection,
    ) -> uqa_execution::ExecResult<Option<uqa_execution::Batch>> {
        self.next_in_direction(direction)
    }

    fn rewind(&mut self) -> uqa_execution::ExecResult<()> {
        self.position = ValuesScanPosition::BeforeFirst;
        Ok(())
    }

    fn close(&mut self) -> uqa_execution::ExecResult<()> {
        self.position = ValuesScanPosition::AfterLast;
        Ok(())
    }
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
