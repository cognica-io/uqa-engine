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
    doc_id_value, engine_func_intercept, execute_function, execute_function_with_top_k,
    execute_mixed_where, expect_column_name, has_aggregate, has_window, is_score_provenance_column,
    optimize_engine_plan, prepare_window_plan, projection_label_at, BTreeMap, BTreeSet, BinaryOp,
    ColumnPrune, Document, Engine, PhysicalAggregateExecutor, PhysicalWindowExecutor,
    QualifierFilters, ResultRow, SQLError, SQLParam, SQLResult, ScoredEntry, SetOpKind, Value,
    DOC_ID_COLUMN, MERGE_ACTION_COLUMN, SCORE_COLUMN, SCORE_PROVENANCE_COLUMN,
};

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
}

pub(super) enum QueryRows {
    Rows(Vec<ResultRow>),
    SharedSpill(uqa_execution::SharedSpill),
}

pub(super) struct QueryOutput {
    pub(super) columns: Vec<String>,
    /// Physical columns include internal row metadata that is available to a
    /// parent query block but never exposed through [`SQLResult`].
    pub(super) internal_columns: Vec<String>,
    pub(super) rows: QueryRows,
}

impl QueryOutput {
    pub(super) fn into_sql_result(self) -> Result<SQLResult, SQLError> {
        let mut rows = match self.rows {
            QueryRows::Rows(rows) => rows,
            QueryRows::SharedSpill(rows) => {
                let mut scan = uqa_execution::SharedSpillScan::new(rows);
                uqa_execution::physical::run_to_rows(&mut scan)
                    .map_err(physical_exec_error)?
                    .1
            }
        };
        for row in &mut rows {
            row.retain(|column, _| !is_score_provenance_column(column));
        }
        Ok(SQLResult::from_rows(self.columns, rows))
    }

    pub(super) fn into_operator<'a>(self) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
        match self.rows {
            QueryRows::Rows(rows) => Box::new(uqa_execution::TableScan::from_rows(
                self.internal_columns,
                rows,
            )),
            QueryRows::SharedSpill(rows) => Box::new(uqa_execution::SharedSpillScan::new(rows)),
        }
    }

    fn into_subquery_result(self) -> Result<uqa_execution::SubqueryResult, SQLError> {
        let columns = self.columns;
        let rows: Box<dyn Iterator<Item = Result<ResultRow, SQLError>> + Send> = match self.rows {
            QueryRows::Rows(rows) => Box::new(rows.into_iter().map(|mut row| {
                row.retain(|column, _| !is_score_provenance_column(column));
                Ok(row)
            })),
            QueryRows::SharedSpill(rows) => {
                Box::new(rows.read_rows().map_err(physical_exec_error)?.map(|row| {
                    let mut row = row.map_err(physical_exec_error)?;
                    row.retain(|column, _| !is_score_provenance_column(column));
                    Ok(row)
                }))
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
    let internal_columns = operator.schema().to_vec();
    let rows = match output_mode {
        QueryOutputMode::Rows => QueryRows::Rows(
            uqa_execution::physical::run_to_rows(operator.as_mut())
                .map_err(physical_exec_error)?
                .1,
        ),
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
                    .into_shared(internal_columns.clone())
                    .map_err(physical_exec_error)?,
            )
        }
    };
    Ok(QueryOutput {
        columns,
        internal_columns,
        rows,
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

pub(super) fn materialize_plan_ctes(
    engine: &Engine,
    plans: &[CtePlan],
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<(), SQLError> {
    materialize_plan_ctes_with_filters(engine, plans, params, ctes, &BTreeMap::new())
}

fn materialize_plan_ctes_with_filters(
    engine: &Engine,
    plans: &[CtePlan],
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    for plan in plans {
        if plan.recursive {
            let rows = materialize_recursive_cte(
                engine,
                plan,
                params,
                ctes,
                output_filters.get(&plan.name),
            )?;
            ctes.insert_shared(plan.name.clone(), rows);
            continue;
        }

        let result = execute_query_plan_output(
            engine,
            &plan.query,
            params,
            ctes,
            QueryOutputMode::SharedSpill,
        )?;
        let mut columns = result.columns.clone();
        let source_columns = result.internal_columns.clone();
        let mut operator = result.into_operator();
        if !plan.columns.is_empty() {
            let renamed_columns = columns
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    plan.columns
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| source.clone())
                })
                .collect::<Vec<_>>();
            let mapping = source_columns
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    let output = if is_score_provenance_column(source) {
                        source.clone()
                    } else {
                        renamed_columns
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| source.clone())
                    };
                    (output, source.clone())
                })
                .collect();
            columns = renamed_columns;
            operator = Box::new(uqa_execution::ColumnSelection::with_mapping(
                operator, mapping,
            ));
        }
        let materialized =
            collect_query_operator(engine, columns, operator, QueryOutputMode::SharedSpill)?;
        let QueryRows::SharedSpill(materialized) = materialized.rows else {
            return Err(SQLError::Internal(
                "CTE spill collector returned in-memory rows".into(),
            ));
        };
        ctes.insert_shared(plan.name.clone(), materialized);
    }
    Ok(())
}

/// Render the inner statement as an EXPLAIN-style plan result. Mirrors
/// the canonical UQA implementation's `_explain_plan`: returns a single-column `plan` table with
/// one row per line.
pub(super) struct ExplainAnalysis {
    pub(super) elapsed: std::time::Duration,
    pub(super) rows: u64,
    pub(super) affected_rows: u64,
}

pub(super) fn run_explain(
    body: &UnifiedPlan,
    verbose: bool,
    format: Option<&str>,
    analysis: Option<&ExplainAnalysis>,
) -> Result<SQLResult, SQLError> {
    let mut plan_text = match body {
        UnifiedPlan::Query(query) => format_query_plan(query),
        UnifiedPlan::Command(command) => format!("{}\n  {command:#?}", command.name()),
    };
    if verbose {
        plan_text.push_str("\n  verbose=true");
        write!(plan_text, "\n  physical_plan={body:#?}")
            .map_err(|error| SQLError::Internal(format!("format EXPLAIN plan: {error}")))?;
    }
    if let Some(analysis) = analysis {
        let _ = write!(
            plan_text,
            "\n  actual_rows={}\n  affected_rows={}\n  execution_time_ms={:.3}",
            analysis.rows,
            analysis.affected_rows,
            analysis.elapsed.as_secs_f64() * 1_000.0
        );
    }

    let format = format.unwrap_or("text").to_ascii_lowercase();
    if format == "json" {
        let payload = serde_json::json!({
            "Plan": plan_text.lines().collect::<Vec<_>>(),
            "Analyze": analysis.is_some(),
            "Actual Rows": analysis.map(|value| value.rows),
            "Affected Rows": analysis.map(|value| value.affected_rows),
            "Execution Time (ms)": analysis.map(|value| value.elapsed.as_secs_f64() * 1_000.0),
        });
        let mut row = ResultRow::new();
        row.insert("plan".to_string(), Value::Str(payload.to_string()));
        return Ok(SQLResult {
            columns: vec!["plan".to_string()],
            rows: vec![row],
            affected_rows: 0,
        });
    }
    if format != "text" {
        return Err(SQLError::Unsupported(format!(
            "EXPLAIN format `{format}` is not supported; expected TEXT or JSON"
        )));
    }
    let mut rows: Vec<ResultRow> = Vec::new();
    for line in plan_text.split('\n') {
        let mut r = ResultRow::new();
        r.insert("plan".to_string(), Value::Str(line.to_string()));
        rows.push(r);
    }
    Ok(SQLResult {
        columns: vec!["plan".to_string()],
        rows,
        affected_rows: 0,
    })
}

fn format_query_plan(plan: &QueryPlan) -> String {
    match &plan.root {
        RelationalPlan::QueryBlock(block) => format_select_plan(block),
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => format!(
            "SetOp\n  kind={kind:?}\n  all={all}\n  left=({})\n  right=({})\n  order_by={}\n  limit={}\n  offset={}",
            format_query_plan(left).replace('\n', "\n    "),
            format_query_plan(right).replace('\n', "\n    "),
            order_by.len(),
            limit
                .as_deref()
                .map_or_else(|| "none".into(), explain_int_expr),
            offset
                .as_deref()
                .map_or_else(|| "none".into(), explain_int_expr),
        ),
        RelationalPlan::Values { rows, .. } => format!("Values\n  rows={}", rows.len()),
    }
}

fn format_select_plan(stmt: &QueryBlockPlan) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "Select");
    if !stmt.projections.is_empty() {
        let _ = writeln!(s, "  projections={}", stmt.projections.len());
    }
    if let Some(from) = &stmt.from {
        let _ = writeln!(s, "  from={from:?}");
    }
    if stmt.r#where.is_some() {
        let _ = writeln!(s, "  where=<expr>");
    }
    if !stmt.group_by.is_empty() {
        let _ = writeln!(s, "  group_by={}", stmt.group_by.len());
    }
    if !stmt.grouping_sets.is_empty() {
        let _ = writeln!(s, "  grouping_sets={}", stmt.grouping_sets.len());
    }
    if !stmt.order_by.is_empty() {
        let _ = writeln!(s, "  order_by={}", stmt.order_by.len());
    }
    if let Some(expr) = stmt.limit.as_ref() {
        let _ = writeln!(s, "  limit={}", explain_int_expr(expr));
    }
    if let Some(expr) = stmt.offset.as_ref() {
        let _ = writeln!(s, "  offset={}", explain_int_expr(expr));
    }
    if stmt.distinct {
        let _ = writeln!(s, "  distinct=true");
    }
    s.trim_end().to_string()
}

fn should_defer_distinct_limit(stmt: &QueryBlockPlan) -> bool {
    stmt.distinct && (stmt.limit.is_some() || stmt.offset.is_some())
}

fn select_execution_stmt(stmt: &QueryBlockPlan, defer_distinct_limit: bool) -> QueryBlockPlan {
    if !defer_distinct_limit {
        return stmt.clone();
    }
    let mut exec_stmt = stmt.clone();
    exec_stmt.limit = None;
    exec_stmt.offset = None;
    exec_stmt
}

fn run_select_without_from_output(
    engine: &Engine,
    original: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let row = ResultRow::new();
    let columns = projection_columns(&stmt.projections);
    let hook = ScopedEngineHook::new(engine, ctes);
    // Set-returning functions in the projection list expand to rows
    // (`SELECT generate_series(1, 3)`).
    // They are a one-to-many projection boundary, but their WHERE phase still
    // runs through the common physical Filter.
    if let Some(result) = expand_projection_srf_output(
        engine,
        &hook,
        original,
        stmt,
        &row,
        params,
        ctes,
        output_mode,
    )? {
        return Ok(result);
    }
    let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::TableScan::from_rows(Vec::new(), vec![row]));
    execute_query_block_operator_output(
        engine,
        operator,
        stmt.r#where.clone(),
        stmt,
        original,
        params,
        ctes,
        columns,
        output_mode,
    )
}

/// Expand a projection list that consists of exactly one set-returning
/// function call (`generate_series`, `unnest`, `jsonb_object_keys`,
/// ...) into one result row per element, mirroring `PostgreSQL`'s
/// SRF-in-select-list behavior for the single-SRF case.
#[allow(clippy::too_many_arguments)]
fn expand_projection_srf_output<'a>(
    engine: &'a Engine,
    hook: &'a ScopedEngineHook<'a>,
    original: &'a QueryBlockPlan,
    stmt: &'a QueryBlockPlan,
    row: &ResultRow,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    output_mode: QueryOutputMode,
) -> Result<Option<QueryOutput>, SQLError> {
    if stmt.projections.len() != 1 {
        return Ok(None);
    }
    let projection = &stmt.projections[0];
    let ScalarExpr::Func { name, args, .. } = &projection.expr else {
        return Ok(None);
    };
    let lower = name.to_ascii_lowercase();
    let is_json_keys = matches!(lower.as_str(), "json_object_keys" | "jsonb_object_keys");
    let is_table_srf = matches!(
        lower.as_str(),
        "generate_series"
            | "unnest"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "regexp_split_to_table"
            | "string_to_table"
    );
    if !is_json_keys && !is_table_srf {
        return Ok(None);
    }

    use uqa_execution::{Filter, PhysicalOperator, ProjectRows, ProjectSet};

    let columns = projection_columns(&stmt.projections);
    let label = columns[0].clone();
    let projector = move |input: &ResultRow| -> ExecResult<ProjectRows> {
        if is_json_keys {
            let context = PhysicalEvalContext::new(Some(input), params)
                .with_function_hook(hook)
                .with_subquery_runner(hook);
            let value = eval_physical_scalar(&projection.expr, &stmt.subqueries, &context)?;
            let Value::List(items) = value else {
                return Ok(Box::new(std::iter::empty()));
            };
            return Ok(Box::new(items.into_iter().map({
                let label = label.clone();
                move |item| Ok([(label.clone(), item)].into_iter().collect())
            })));
        }
        let context = super::from_rows::TableFunctionEvalContext::new(
            engine,
            params,
            hook,
            hook,
            &stmt.subqueries,
        );
        let produced = super::from_rows::build_table_function_row_stream(
            &context,
            &lower,
            args,
            None,
            &[],
            &[],
        )?;
        Ok(Box::new(produced.map({
            let label = label.clone();
            move |produced_row| {
                let produced_row = produced_row?;
                let value = produced_row
                    .iter()
                    .next()
                    .map_or(Value::Null, |(_, value)| value.clone());
                Ok([(label.clone(), value)].into_iter().collect())
            }
        })))
    };
    let input_schema = row.keys().cloned().collect();
    let mut child: Box<dyn PhysicalOperator + '_> = Box::new(uqa_execution::TableScan::from_rows(
        input_schema,
        vec![row.clone()],
    ));
    if let Some(predicate) = stmt.r#where.clone() {
        child = Box::new(Filter::with_evaluator(
            child,
            predicate,
            EngineExpressionEvaluator::shared(engine, params, ctes),
        ));
    }
    let mut operator: Box<dyn PhysicalOperator + '_> =
        Box::new(ProjectSet::new(child, columns.clone(), Box::new(projector)));
    let output_columns = identity_order_columns(&columns);
    operator = attach_order_limit(
        operator,
        stmt,
        &output_columns,
        engine,
        params,
        ctes,
        EngineExpressionEvaluator::shared(engine, params, ctes),
    )?;
    Ok(Some(finish_query_block_operator_output(
        engine,
        operator,
        original,
        params,
        ctes,
        columns,
        output_mode,
    )?))
}

#[derive(Clone)]
pub(crate) struct CteScope {
    pub(super) rows: BTreeMap<String, uqa_execution::SharedSpill>,
    pub(super) scalar_subqueries: Vec<QueryPlan>,
    scalar_subquery_arena: u64,
    next_scalar_subquery_arena: Arc<AtomicU64>,
    scalar_subquery_cache:
        Arc<parking_lot::Mutex<BTreeMap<(u64, usize), ScalarSubqueryCacheEntry>>>,
}

impl Default for CteScope {
    fn default() -> Self {
        Self {
            rows: BTreeMap::new(),
            scalar_subqueries: Vec::new(),
            scalar_subquery_arena: 0,
            next_scalar_subquery_arena: Arc::new(AtomicU64::new(1)),
            scalar_subquery_cache: Arc::new(parking_lot::Mutex::new(BTreeMap::new())),
        }
    }
}

impl CteScope {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(super) fn insert_shared(&mut self, name: String, rows: uqa_execution::SharedSpill) {
        self.rows.insert(name, rows);
    }

    pub(super) fn remove_materialized(&mut self, name: &str) -> Option<uqa_execution::SharedSpill> {
        self.rows.remove(name)
    }

    /// Bind the scalar-subquery arena owned by one query block. The guard
    /// restores the parent arena on success, error, or panic so nested and
    /// lateral query execution cannot resolve a child slot in its parent.
    pub(super) fn enter_scalar_subqueries(
        &mut self,
        subqueries: &[QueryPlan],
    ) -> ScalarSubqueryScope<'_> {
        let previous = std::mem::replace(&mut self.scalar_subqueries, subqueries.to_vec());
        let next_arena = self
            .next_scalar_subquery_arena
            .fetch_add(1, Ordering::Relaxed);
        let previous_arena = std::mem::replace(&mut self.scalar_subquery_arena, next_arena);
        ScalarSubqueryScope {
            ctes: self,
            previous: Some(previous),
            previous_arena,
        }
    }
}

pub(super) struct ScalarSubqueryScope<'a> {
    ctes: &'a mut CteScope,
    previous: Option<Vec<QueryPlan>>,
    previous_arena: u64,
}

impl std::ops::Deref for ScalarSubqueryScope<'_> {
    type Target = CteScope;

    fn deref(&self) -> &Self::Target {
        self.ctes
    }
}

impl std::ops::DerefMut for ScalarSubqueryScope<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctes
    }
}

impl Drop for ScalarSubqueryScope<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.ctes.scalar_subqueries = previous;
            self.ctes.scalar_subquery_arena = self.previous_arena;
        }
    }
}

#[derive(Clone)]
enum ScalarSubqueryCacheEntry {
    Correlated,
    Materialized(CachedScalarSubquery),
    Scalar(Value),
    Exists(bool),
}

#[derive(Clone)]
struct CachedScalarSubquery {
    columns: Vec<String>,
    rows: uqa_execution::SharedSpill,
}

impl CachedScalarSubquery {
    fn result(&self) -> Result<uqa_execution::SubqueryResult, SQLError> {
        let rows = self
            .rows
            .read_rows()
            .map_err(physical_exec_error)?
            .map(|row| {
                let mut row = row.map_err(physical_exec_error)?;
                row.retain(|column, _| !is_score_provenance_column(column));
                Ok(row)
            });
        Ok(uqa_execution::SubqueryResult {
            columns: self.columns.clone(),
            rows: Box::new(rows),
        })
    }
}

pub(super) struct ScopedEngineHook<'a> {
    engine: &'a Engine,
    ctes: &'a CteScope,
}

impl<'a> ScopedEngineHook<'a> {
    pub(super) fn new(engine: &'a Engine, ctes: &'a CteScope) -> Self {
        Self { engine, ctes }
    }
}

/// Scalar adapter shared by Filter, Project, and Sort. It binds the engine's
/// registered functions and the query block's physical subquery arena without
/// evaluating any row expression outside the operator tree.
pub(super) struct EngineExpressionEvaluator<'a> {
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: CteScope,
}

impl<'a> EngineExpressionEvaluator<'a> {
    pub(super) fn shared(
        engine: &'a Engine,
        params: &'a [SQLParam],
        ctes: &CteScope,
    ) -> SharedExpressionEvaluator<'a> {
        Arc::new(Self {
            engine,
            params,
            ctes: ctes.clone(),
        })
    }
}

impl ExpressionEvaluator for EngineExpressionEvaluator<'_> {
    fn evaluate(&self, expression: &ScalarExpr, row: &ResultRow) -> ExecResult<Value> {
        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let context = PhysicalEvalContext::new(Some(row), self.params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        if let ScalarExpr::Func { name, args, .. } = expression {
            let mut evaluate = |expr: &ScalarExpr| {
                eval_physical_scalar(expr, &self.ctes.scalar_subqueries, &context)
            };
            if let Some(value) =
                engine_func_intercept(Some(self.engine), name, args, row, &mut evaluate)?
            {
                return Ok(value);
            }
        }
        Ok(eval_physical_scalar(
            expression,
            &self.ctes.scalar_subqueries,
            &context,
        )?)
    }

    fn project_star(&self, row: &ResultRow) -> ExecResult<ResultRow> {
        Ok(row
            .iter()
            .filter(|(column, _)| {
                !matches!(
                    column.as_str(),
                    SCORE_COLUMN | DOC_ID_COLUMN | MERGE_ACTION_COLUMN
                ) && !is_score_provenance_column(column)
            })
            .map(|(column, value)| (column.clone(), value.clone()))
            .collect())
    }
}

impl uqa_sql::expr::EngineHook for ScopedEngineHook<'_> {
    fn nextval(&self, name: &str) -> std::result::Result<i64, String> {
        self.engine.nextval(name)
    }

    fn currval(&self, name: &str) -> std::result::Result<i64, String> {
        self.engine.currval(name)
    }

    fn setval(&self, name: &str, value: i64) -> std::result::Result<i64, String> {
        self.engine.setval(name, value)
    }

    fn call_scalar_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<Value, SQLError>> {
        self.engine.call_registered_scalar_function(name, args)
    }

    fn has_scalar_functions(&self) -> bool {
        self.engine.has_registered_scalar_functions()
    }

    fn current_schema(&self) -> std::result::Result<Option<String>, String> {
        self.engine
            .current_schema_name()
            .map_err(|error| error.to_string())
    }

    fn current_schemas(
        &self,
        include_implicit: bool,
    ) -> std::result::Result<Option<Vec<String>>, String> {
        self.engine
            .current_schema_names(include_implicit)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn random_value(&self) -> std::result::Result<Option<f64>, String> {
        Ok(Some(self.engine.next_random_value()))
    }

    fn set_random_seed(&self, seed: f64) -> std::result::Result<bool, String> {
        self.engine.set_random_seed(seed)?;
        Ok(true)
    }

    fn call_user_function(
        &self,
        name: &str,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::sql::call_user_scalar_function(self.engine, name, args)
    }
}

impl PhysicalSubqueryRunner for ScopedEngineHook<'_> {
    fn execute_subquery(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: Option<&ResultRow>,
        params: &[SQLParam],
    ) -> Result<uqa_execution::SubqueryResult, SQLError> {
        let cache_key = (self.ctes.scalar_subquery_arena, subquery);
        if let Some(entry) = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned()
        {
            match entry {
                ScalarSubqueryCacheEntry::Correlated => {
                    return self.execute_correlated_subquery(plan, outer_row, params);
                }
                ScalarSubqueryCacheEntry::Materialized(result) => return result.result(),
                ScalarSubqueryCacheEntry::Scalar(_) | ScalarSubqueryCacheEntry::Exists(_) => {
                    return Err(SQLError::Internal(
                        "scalar subquery slot changed result consumer during execution".into(),
                    ));
                }
            }
        }

        if super::correlation::query_depends_on_outer_row(self.engine, plan)? {
            self.ctes
                .scalar_subquery_cache
                .lock()
                .insert(cache_key, ScalarSubqueryCacheEntry::Correlated);
            return self.execute_correlated_subquery(plan, outer_row, params);
        }

        let result = self.execute_uncorrelated_subquery(plan, params)?;
        self.ctes.scalar_subquery_cache.lock().insert(
            cache_key,
            ScalarSubqueryCacheEntry::Materialized(result.clone()),
        );
        result.result()
    }

    fn scalar_subquery_value(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: Option<&ResultRow>,
        params: &[SQLParam],
    ) -> Result<Value, SQLError> {
        let cache_key = (self.ctes.scalar_subquery_arena, subquery);
        if let Some(entry) = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned()
        {
            return match entry {
                ScalarSubqueryCacheEntry::Correlated => self
                    .execute_correlated_subquery(plan, outer_row, params)?
                    .into_scalar_value(),
                ScalarSubqueryCacheEntry::Scalar(value) => Ok(value),
                ScalarSubqueryCacheEntry::Materialized(result) => {
                    result.result()?.into_scalar_value()
                }
                ScalarSubqueryCacheEntry::Exists(_) => Err(SQLError::Internal(
                    "scalar subquery slot changed result consumer during execution".into(),
                )),
            };
        }
        if super::correlation::query_depends_on_outer_row(self.engine, plan)? {
            self.ctes
                .scalar_subquery_cache
                .lock()
                .insert(cache_key, ScalarSubqueryCacheEntry::Correlated);
            return self
                .execute_correlated_subquery(plan, outer_row, params)?
                .into_scalar_value();
        }
        let value = self
            .execute_uncorrelated_subquery(plan, params)?
            .result()?
            .into_scalar_value()?;
        self.ctes
            .scalar_subquery_cache
            .lock()
            .insert(cache_key, ScalarSubqueryCacheEntry::Scalar(value.clone()));
        Ok(value)
    }

    fn subquery_exists(
        &self,
        subquery: usize,
        plan: &QueryPlan,
        outer_row: Option<&ResultRow>,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let cache_key = (self.ctes.scalar_subquery_arena, subquery);
        if let Some(entry) = self
            .ctes
            .scalar_subquery_cache
            .lock()
            .get(&cache_key)
            .cloned()
        {
            return match entry {
                ScalarSubqueryCacheEntry::Correlated => self
                    .execute_correlated_subquery(plan, outer_row, params)?
                    .into_exists(),
                ScalarSubqueryCacheEntry::Exists(exists) => Ok(exists),
                ScalarSubqueryCacheEntry::Materialized(result) => Ok(result.rows.rows() != 0),
                ScalarSubqueryCacheEntry::Scalar(_) => Err(SQLError::Internal(
                    "scalar subquery slot changed result consumer during execution".into(),
                )),
            };
        }
        if super::correlation::query_depends_on_outer_row(self.engine, plan)? {
            self.ctes
                .scalar_subquery_cache
                .lock()
                .insert(cache_key, ScalarSubqueryCacheEntry::Correlated);
            return self
                .execute_correlated_subquery(plan, outer_row, params)?
                .into_exists();
        }
        let exists = self
            .execute_uncorrelated_subquery(plan, params)?
            .rows
            .rows()
            != 0;
        self.ctes
            .scalar_subquery_cache
            .lock()
            .insert(cache_key, ScalarSubqueryCacheEntry::Exists(exists));
        Ok(exists)
    }
}

impl ScopedEngineHook<'_> {
    fn execute_uncorrelated_subquery(
        &self,
        plan: &QueryPlan,
        params: &[SQLParam],
    ) -> Result<CachedScalarSubquery, SQLError> {
        let mut scoped_ctes = self.ctes.clone();
        let output = execute_query_plan_output(
            self.engine,
            plan,
            params,
            &mut scoped_ctes,
            QueryOutputMode::SharedSpill,
        )?;
        let QueryRows::SharedSpill(rows) = output.rows else {
            return Err(SQLError::Internal(
                "scalar subquery spill collector returned in-memory rows".into(),
            ));
        };
        Ok(CachedScalarSubquery {
            columns: output.columns,
            rows,
        })
    }

    fn execute_correlated_subquery(
        &self,
        plan: &QueryPlan,
        outer_row: Option<&ResultRow>,
        params: &[SQLParam],
    ) -> Result<uqa_execution::SubqueryResult, SQLError> {
        if let Some(outer_row) = outer_row {
            return execute_lateral_subquery_output(
                self.engine,
                plan,
                outer_row,
                params,
                self.ctes,
            )?
            .into_subquery_result();
        }
        let mut scoped_ctes = self.ctes.clone();
        execute_query_plan_output(
            self.engine,
            plan,
            params,
            &mut scoped_ctes,
            QueryOutputMode::SharedSpill,
        )?
        .into_subquery_result()
    }
}

fn expr_contains_subquery(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_contains_subquery)
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_contains_subquery)
                || order_by
                    .iter()
                    .any(|order| expr_contains_subquery(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|expr| expr_contains_subquery(expr))
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_contains_subquery(lhs) || expr_contains_subquery(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_contains_subquery(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_contains_subquery(expr)
                || expr_contains_subquery(low)
                || expr_contains_subquery(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_contains_subquery(expr) || list.iter().any(expr_contains_subquery)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_contains_subquery)
                || spec.partition_by.iter().any(expr_contains_subquery)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_contains_subquery(&order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_contains_subquery(&frame.start)
                        || frame_bound_contains_subquery(&frame.end)
                })
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_contains_subquery(expr))
                || when.iter().any(|(cond, result)| {
                    expr_contains_subquery(cond) || expr_contains_subquery(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_contains_subquery(expr))
        }
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => false,
    }
}

fn frame_bound_contains_subquery(bound: &ScalarFrameBound) -> bool {
    match bound {
        ScalarFrameBound::Preceding(expr) | ScalarFrameBound::Following(expr) => {
            expr_contains_subquery(expr)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => false,
    }
}

fn run_query_block_with_prepared_exists_output(
    engine: &Engine,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let Some(from) = stmt.from.as_ref() else {
        return run_select_without_from_output(engine, block, stmt, params, ctes, output_mode);
    };

    // Set-op branches, CTEs, and derived-table bodies still need the same
    // search-aware single-table physical access path as top-level queries;
    // otherwise registry-backed predicates such as
    // `pool_positive_evidence(bayesian_match(...), knn_match(...))` fall
    // through to scalar expression evaluation.
    if let SourcePlan::Table { name, alias } = from {
        let foreign_table = engine
            .foreign_table(name)
            .map_err(|err| SQLError::Internal(format!("resolve foreign table `{name}`: {err}")))?;
        if alias.is_none() && foreign_table.is_some() {
            return run_single_foreign_select_output(
                engine,
                name,
                block,
                stmt,
                params,
                ctes,
                output_mode,
            );
        }
        let local_table = engine
            .try_table(name)
            .map_err(|err| SQLError::Internal(format!("resolve table `{name}`: {err}")))?;
        let is_virtual = name.contains('.') || (local_table.is_none() && foreign_table.is_none());
        if alias.is_none() && !is_virtual {
            return run_single_table_select_output(
                engine,
                name,
                block,
                stmt,
                params,
                ctes,
                output_mode,
            );
        }
    }

    if let Some(filter) = stmt.r#where.as_ref() {
        super::validate_joined_expr_text_match_fields(engine, from, filter)?;
    }

    let column_prune = column_prune_for_stmt(engine, stmt, from);
    let qualifier_filters = qualifier_filters_for_stmt(engine, stmt, from);
    let operator = super::from_rows::build_join_operator_with_ctes(
        engine,
        from,
        params,
        ctes,
        column_prune.as_ref(),
        qualifier_filters.as_ref(),
    )?;
    let physical_filter =
        final_filter_after_qualifier_pushdown(engine, stmt, from, qualifier_filters.as_ref());

    let columns = if matches!(block.compute, ComputePlan::Project) {
        expand_from_star_columns(
            engine,
            projection_columns(&stmt.projections),
            &stmt.projections,
            from,
        )
    } else {
        projection_columns(&stmt.projections)
    };
    execute_query_block_operator_output(
        engine,
        operator,
        physical_filter,
        stmt,
        block,
        params,
        ctes,
        columns,
        output_mode,
    )
}

fn column_prune_for_stmt(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
) -> Option<ColumnPrune> {
    if has_window(&stmt.projections)
        || stmt.projections.iter().any(|projection| {
            matches!(projection.expr, ScalarExpr::Star)
                || expr_contains_subquery(&projection.expr)
                || expr_contains_volatile_function(engine, &projection.expr)
        })
    {
        return None;
    }

    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    if qualifiers.is_empty() {
        return None;
    }

    let mut prune: ColumnPrune = qualifiers
        .iter()
        .map(|qualifier| (qualifier.clone(), BTreeSet::new()))
        .collect();
    let mut valid = true;
    collect_from_prune_columns(from, &qualifiers, &mut prune, &mut valid);
    for projection in &stmt.projections {
        collect_expr_prune_columns(&projection.expr, &qualifiers, &mut prune, &mut valid);
    }
    if let Some(filter) = stmt.r#where.as_ref() {
        collect_expr_prune_columns(filter, &qualifiers, &mut prune, &mut valid);
    }
    for expr in &stmt.group_by {
        collect_expr_prune_columns(expr, &qualifiers, &mut prune, &mut valid);
    }
    for set in &stmt.grouping_sets {
        for expr in set {
            collect_expr_prune_columns(expr, &qualifiers, &mut prune, &mut valid);
        }
    }
    if let Some(having) = stmt.having.as_ref() {
        collect_expr_prune_columns(having, &qualifiers, &mut prune, &mut valid);
    }
    for order in &stmt.order_by {
        collect_expr_prune_columns(&order.expr, &qualifiers, &mut prune, &mut valid);
    }
    for expr in &stmt.distinct_on {
        collect_expr_prune_columns(expr, &qualifiers, &mut prune, &mut valid);
    }
    if !valid {
        return None;
    }
    Some(prune)
}

fn collect_from_qualifiers(from: &SourcePlan, out: &mut Vec<String>) {
    match from {
        SourcePlan::Table { name, alias } => {
            out.push(alias.clone().unwrap_or_else(|| name.clone()));
        }
        SourcePlan::Join { left, right, .. } => {
            collect_from_qualifiers(left, out);
            collect_from_qualifiers(right, out);
        }
        SourcePlan::Values { alias, .. }
        | SourcePlan::Function { alias, .. }
        | SourcePlan::Subquery { alias, .. } => {
            if let Some(alias) = alias {
                out.push(alias.clone());
            }
        }
    }
}

fn collect_from_prune_columns(
    from: &SourcePlan,
    qualifiers: &[String],
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match from {
        SourcePlan::Join {
            left, right, on, ..
        } => {
            collect_from_prune_columns(left, qualifiers, prune, valid);
            collect_from_prune_columns(right, qualifiers, prune, valid);
            if let Some(on) = on.as_ref() {
                collect_expr_prune_columns(on, qualifiers, prune, valid);
            }
        }
        SourcePlan::Values { rows, .. } => {
            for row in rows {
                for expr in row {
                    collect_expr_prune_columns(expr, qualifiers, prune, valid);
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            for expr in args {
                collect_expr_prune_columns(expr, qualifiers, prune, valid);
            }
        }
        SourcePlan::Subquery { .. } => {
            *valid = false;
        }
        SourcePlan::Table { .. } => {}
    }
}

fn collect_expr_prune_columns(
    expr: &ScalarExpr,
    qualifiers: &[String],
    prune: &mut ColumnPrune,
    valid: &mut bool,
) {
    match expr {
        ScalarExpr::Column(column) => {
            for qualifier in qualifiers {
                if let Some(columns) = prune.get_mut(qualifier) {
                    columns.insert(column.clone());
                }
            }
        }
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } => {
            if let Some(columns) = prune.get_mut(qualifier) {
                columns.insert(column.clone());
            } else {
                *valid = false;
            }
        }
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => {}
        ScalarExpr::Star
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => {
            *valid = false;
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                collect_expr_prune_columns(item, qualifiers, prune, valid);
            }
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_expr_prune_columns(arg, qualifiers, prune, valid);
            }
            for order in order_by {
                collect_expr_prune_columns(&order.expr, qualifiers, prune, valid);
            }
            if let Some(filter) = filter.as_ref() {
                collect_expr_prune_columns(filter, qualifiers, prune, valid);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_expr_prune_columns(lhs, qualifiers, prune, valid);
            collect_expr_prune_columns(rhs, qualifiers, prune, valid);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            collect_expr_prune_columns(inner, qualifiers, prune, valid);
        }
        ScalarExpr::Between { expr, low, high } => {
            collect_expr_prune_columns(expr, qualifiers, prune, valid);
            collect_expr_prune_columns(low, qualifiers, prune, valid);
            collect_expr_prune_columns(high, qualifiers, prune, valid);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_expr_prune_columns(expr, qualifiers, prune, valid);
            for item in list {
                collect_expr_prune_columns(item, qualifiers, prune, valid);
            }
        }
        ScalarExpr::WindowCall { .. } => {
            *valid = false;
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_expr_prune_columns(base, qualifiers, prune, valid);
            }
            for (cond, result) in when {
                collect_expr_prune_columns(cond, qualifiers, prune, valid);
                collect_expr_prune_columns(result, qualifiers, prune, valid);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_expr_prune_columns(else_branch, qualifiers, prune, valid);
            }
        }
    }
}

fn qualifier_filters_for_stmt(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
) -> Option<QualifierFilters> {
    let filter = stmt.r#where.as_ref()?;
    if expr_contains_subquery(filter) {
        return None;
    }
    let from_quals = from_qualifier_set(from);
    if from_quals.is_empty() {
        return None;
    }
    let single_qualifier = (from_quals.len() == 1)
        .then(|| from_quals.iter().next().cloned())
        .flatten();
    let mut filters = QualifierFilters::new();
    for part in flatten_and_filter_parts(filter) {
        if let Some((qualifier, filter)) =
            qualifier_filter_for_part(engine, part, &from_quals, single_qualifier.as_deref())
        {
            filters.entry(qualifier).or_default().push(filter);
        }
    }
    (!filters.is_empty()).then_some(filters)
}

fn qualifier_filter_for_part(
    engine: &Engine,
    part: &ScalarExpr,
    from_quals: &BTreeSet<String>,
    single_qualifier: Option<&str>,
) -> Option<(String, ScalarExpr)> {
    if expr_contains_subquery(part)
        || (expr_contains_volatile_function(engine, part)
            && !uqa_planner::optimizer::contains_retrieval(part))
    {
        return None;
    }
    let qualifiers = expr_qualifiers(part);
    let has_unqualified = expr_has_unqualified_column(part);
    if qualifiers.len() == 1 && (!has_unqualified || from_quals.len() == 1) {
        let qualifier = qualifiers.iter().next()?;
        if from_quals.contains(qualifier) {
            return Some((qualifier.clone(), part.clone()));
        }
    }
    if qualifiers.is_empty() && has_unqualified {
        if let Some(qualifier) = single_qualifier {
            return Some((
                qualifier.to_string(),
                qualify_unqualified_columns(part, qualifier),
            ));
        }
    }
    None
}

fn final_filter_after_qualifier_pushdown(
    engine: &Engine,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filters: Option<&QualifierFilters>,
) -> Option<ScalarExpr> {
    let filter = stmt.r#where.as_ref()?;
    if filters.is_none() || !qualifier_filter_elision_safe(from) {
        return Some(filter.clone());
    }
    let from_quals = from_qualifier_set(from);
    let single_qualifier = (from_quals.len() == 1)
        .then(|| from_quals.iter().next().cloned())
        .flatten();
    let residual: Vec<ScalarExpr> = flatten_and_filter_parts(filter)
        .into_iter()
        .filter(|part| {
            qualifier_filter_for_part(engine, part, &from_quals, single_qualifier.as_deref())
                .is_none()
        })
        .cloned()
        .collect();
    combine_filter_parts(residual)
}

fn qualifier_filter_elision_safe(from: &SourcePlan) -> bool {
    match from {
        SourcePlan::Join {
            left, right, kind, ..
        } => {
            matches!(
                kind,
                uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross
            ) && qualifier_filter_elision_safe(left)
                && qualifier_filter_elision_safe(right)
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { .. } => true,
    }
}

fn combine_filter_parts(mut parts: Vec<ScalarExpr>) -> Option<ScalarExpr> {
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(ScalarExpr::And(parts)),
    }
}

/// Find predicates on a directly referenced CTE output. The predicate remains
/// on the consumer and is duplicated into the CTE only when that CTE has one
/// reference in this query block. This makes the rewrite semantics-preserving
/// for shared CTE materializations.
fn cte_output_filters(engine: &Engine, plan: &QueryPlan) -> BTreeMap<String, (String, ScalarExpr)> {
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        return BTreeMap::new();
    };
    let (Some(from), Some(filter)) = (block.from.as_ref(), block.r#where.as_ref()) else {
        return BTreeMap::new();
    };
    if expr_contains_subquery(filter) || expr_contains_volatile_function(engine, filter) {
        return BTreeMap::new();
    }

    let cte_names: BTreeSet<&str> = plan.ctes.iter().map(|cte| cte.name.as_str()).collect();
    let mut references: BTreeMap<String, Vec<String>> = BTreeMap::new();
    collect_cte_source_references(from, &cte_names, &mut references);
    let qualifier_to_cte: BTreeMap<String, String> = references
        .into_iter()
        .filter_map(|(cte, qualifiers)| {
            (qualifiers.len() == 1).then(|| (qualifiers[0].clone(), cte))
        })
        .collect();
    if qualifier_to_cte.is_empty() {
        return BTreeMap::new();
    }

    let from_qualifiers = from_qualifier_set(from);
    let single_qualifier = (from_qualifiers.len() == 1)
        .then(|| from_qualifiers.iter().next().cloned())
        .flatten();
    let mut grouped: BTreeMap<String, (String, Vec<ScalarExpr>)> = BTreeMap::new();
    for part in flatten_and_filter_parts(filter) {
        let Some((qualifier, predicate)) =
            qualifier_filter_for_part(engine, part, &from_qualifiers, single_qualifier.as_deref())
        else {
            continue;
        };
        let Some(cte_name) = qualifier_to_cte.get(&qualifier) else {
            continue;
        };
        let entry = grouped
            .entry(cte_name.clone())
            .or_insert_with(|| (qualifier, Vec::new()));
        entry.1.push(predicate);
    }

    grouped
        .into_iter()
        .filter_map(|(name, (qualifier, predicates))| {
            combine_filter_parts(predicates).map(|predicate| (name, (qualifier, predicate)))
        })
        .collect()
}

fn collect_cte_source_references(
    source: &SourcePlan,
    cte_names: &BTreeSet<&str>,
    references: &mut BTreeMap<String, Vec<String>>,
) {
    match source {
        SourcePlan::Table { name, alias } if cte_names.contains(name.as_str()) => {
            references
                .entry(name.clone())
                .or_default()
                .push(alias.clone().unwrap_or_else(|| name.clone()));
        }
        SourcePlan::Join { left, right, .. } => {
            collect_cte_source_references(left, cte_names, references);
            collect_cte_source_references(right, cte_names, references);
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::Subquery { .. } => {}
    }
}

/// Specialize a physical query plan with a predicate on its output columns.
/// The caller keeps the original predicate as a residual check; this function
/// only returns a plan when pushing the predicate below the output boundary is
/// provably safe.
pub(super) fn push_output_filter_into_query_plan(
    engine: &Engine,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Result<Option<QueryPlan>, SQLError> {
    if expr_contains_subquery(filter)
        || expr_contains_volatile_function(engine, filter)
        || query_contains_volatile_function(engine, plan)?
    {
        return Ok(None);
    }
    let Some(specialized) =
        specialize_query_output_filter(engine, plan, qualifier, filter, output_columns_override)
    else {
        return Ok(None);
    };
    match optimize_engine_plan(engine, UnifiedPlan::Query(Box::new(specialized)))? {
        UnifiedPlan::Query(plan) => Ok(Some(*plan)),
        UnifiedPlan::Command(_) => Err(SQLError::Internal(
            "query optimizer changed a query into a command plan".into(),
        )),
    }
}

fn specialize_query_output_filter(
    engine: &Engine,
    plan: &QueryPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<QueryPlan> {
    let mut specialized = plan.clone();
    specialize_relational_output_filter(
        engine,
        &mut specialized.root,
        qualifier,
        filter,
        output_columns_override,
    )?;
    Some(specialized)
}

fn specialize_relational_output_filter(
    engine: &Engine,
    root: &mut RelationalPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<()> {
    match root {
        RelationalPlan::QueryBlock(block) => specialize_query_block_output_filter(
            engine,
            block,
            qualifier,
            filter,
            output_columns_override,
        ),
        RelationalPlan::SetOp {
            left,
            right,
            limit,
            offset,
            ..
        } => {
            if limit.is_some() || offset.is_some() {
                return None;
            }
            let output_columns = match output_columns_override {
                Some(columns) => columns.to_vec(),
                None => query_plan_output_columns(left)?,
            };
            let specialized_left = specialize_query_output_filter(
                engine,
                left,
                qualifier,
                filter,
                Some(&output_columns),
            )?;
            let specialized_right = specialize_query_output_filter(
                engine,
                right,
                qualifier,
                filter,
                Some(&output_columns),
            )?;
            **left = specialized_left;
            **right = specialized_right;
            Some(())
        }
        RelationalPlan::Values { .. } => None,
    }
}

fn query_plan_output_columns(plan: &QueryPlan) -> Option<Vec<String>> {
    match &plan.root {
        RelationalPlan::QueryBlock(block) => Some(projection_columns(&block.projections)),
        RelationalPlan::SetOp { left, .. } => query_plan_output_columns(left),
        RelationalPlan::Values { rows, .. } => rows.first().map(|row| {
            (1..=row.len())
                .map(|index| format!("column{index}"))
                .collect()
        }),
    }
}

fn specialize_query_block_output_filter(
    engine: &Engine,
    block: &mut QueryBlockPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    output_columns_override: Option<&[String]>,
) -> Option<()> {
    if block.limit.is_some()
        || block.offset.is_some()
        || matches!(block.compute, ComputePlan::Window)
        || !block.distinct_on.is_empty()
        || !block.grouping_sets.is_empty()
    {
        return None;
    }

    let output_columns = output_columns_override.map_or_else(
        || projection_columns(&block.projections),
        <[String]>::to_vec,
    );
    if output_columns.len() != block.projections.len() {
        return None;
    }
    let mut used = BTreeSet::new();
    let rewritten = rewrite_output_filter(
        filter,
        qualifier,
        &output_columns,
        &block.projections,
        &mut used,
    )?;
    if used.is_empty() {
        return None;
    }

    for index in &used {
        let expression = &block.projections[*index].expr;
        if matches!(expression, ScalarExpr::Star)
            || expression.contains_window()
            || expr_contains_subquery(expression)
            || expr_contains_volatile_function(engine, expression)
        {
            return None;
        }
        if matches!(block.compute, ComputePlan::Aggregate)
            && !block.group_by.iter().any(|group| group == expression)
        {
            return None;
        }
    }
    if block.distinct
        && block
            .projections
            .iter()
            .enumerate()
            .any(|(index, projection)| {
                !used.contains(&index) && expr_contains_function(&projection.expr)
            })
    {
        return None;
    }

    block.r#where = match block.r#where.take() {
        Some(existing) => Some(ScalarExpr::And(vec![existing, rewritten])),
        None => Some(rewritten),
    };
    Some(())
}

fn rewrite_output_filter(
    expression: &ScalarExpr,
    qualifier: &str,
    output_columns: &[String],
    projections: &[ProjectionPlan],
    used: &mut BTreeSet<usize>,
) -> Option<ScalarExpr> {
    let map_column = |column: &str, used: &mut BTreeSet<usize>| {
        let index = output_columns
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(column))?;
        used.insert(index);
        Some(projections[index].expr.clone())
    };
    let recur = |expression: &ScalarExpr, used: &mut BTreeSet<usize>| {
        rewrite_output_filter(expression, qualifier, output_columns, projections, used)
    };

    Some(match expression {
        ScalarExpr::Column(column) => map_column(column, used)?,
        ScalarExpr::QualifiedColumn {
            qualifier: expression_qualifier,
            column,
            ..
        } if expression_qualifier.eq_ignore_ascii_case(qualifier) => map_column(column, used)?,
        ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Star
        | ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => return None,
        ScalarExpr::Literal(_) | ScalarExpr::Param(_) => expression.clone(),
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| recur(arg, used))
                .collect::<Option<Vec<_>>>()?,
            distinct: *distinct,
            order_by: order_by
                .iter()
                .map(|order| {
                    Some(uqa_execution::ScalarOrder {
                        expr: recur(&order.expr, used)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            filter: match filter.as_deref() {
                Some(filter) => Some(Box::new(recur(filter, used)?)),
                None => None,
            },
        },
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(recur(lhs, used)?),
            rhs: Box::new(recur(rhs, used)?),
        },
        ScalarExpr::Not(inner) => ScalarExpr::Not(Box::new(recur(inner, used)?)),
        ScalarExpr::And(items) => ScalarExpr::And(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::Or(items) => ScalarExpr::Or(
            items
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
        ),
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(recur(expr, used)?),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(recur(expr, used)?),
            low: Box::new(recur(low, used)?),
            high: Box::new(recur(high, used)?),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(recur(expr, used)?),
            list: list
                .iter()
                .map(|item| recur(item, used))
                .collect::<Option<Vec<_>>>()?,
            negated: *negated,
        },
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: match base.as_deref() {
                Some(base) => Some(Box::new(recur(base, used)?)),
                None => None,
            },
            when: when
                .iter()
                .map(|(condition, result)| Some((recur(condition, used)?, recur(result, used)?)))
                .collect::<Option<Vec<_>>>()?,
            else_branch: match else_branch.as_deref() {
                Some(branch) => Some(Box::new(recur(branch, used)?)),
                None => None,
            },
        },
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(recur(expr, used)?),
            ty: ty.clone(),
        },
    })
}

fn expr_contains_function(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Func { .. } | ScalarExpr::WindowCall { .. } => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_contains_function)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_contains_function(lhs) || expr_contains_function(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_contains_function(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_contains_function(expr)
                || expr_contains_function(low)
                || expr_contains_function(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_contains_function(expr) || list.iter().any(expr_contains_function)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(expr_contains_function)
                || when.iter().any(|(condition, result)| {
                    expr_contains_function(condition) || expr_contains_function(result)
                })
                || else_branch.as_deref().is_some_and(expr_contains_function)
        }
        ScalarExpr::InSubquery { expr, .. } => expr_contains_function(expr),
        ScalarExpr::Star
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn flatten_and_filter_parts(expr: &ScalarExpr) -> Vec<&ScalarExpr> {
    match expr {
        ScalarExpr::And(items) => items.iter().flat_map(flatten_and_filter_parts).collect(),
        other => vec![other],
    }
}

fn from_qualifier_set(from: &SourcePlan) -> BTreeSet<String> {
    let mut qualifiers = Vec::new();
    collect_from_qualifiers(from, &mut qualifiers);
    qualifiers.into_iter().collect()
}

fn expr_qualifiers(expr: &ScalarExpr) -> BTreeSet<String> {
    let mut qualifiers = BTreeSet::new();
    collect_expr_qualifiers(expr, &mut qualifiers);
    qualifiers
}

fn collect_expr_qualifiers(expr: &ScalarExpr, qualifiers: &mut BTreeSet<String>) {
    match expr {
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            qualifiers.insert(qualifier.clone());
        }
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            for item in items {
                collect_expr_qualifiers(item, qualifiers);
            }
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for arg in args {
                collect_expr_qualifiers(arg, qualifiers);
            }
            for order in order_by {
                collect_expr_qualifiers(&order.expr, qualifiers);
            }
            if let Some(filter) = filter.as_ref() {
                collect_expr_qualifiers(filter, qualifiers);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_expr_qualifiers(lhs, qualifiers);
            collect_expr_qualifiers(rhs, qualifiers);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            collect_expr_qualifiers(inner, qualifiers);
        }
        ScalarExpr::Between { expr, low, high } => {
            collect_expr_qualifiers(expr, qualifiers);
            collect_expr_qualifiers(low, qualifiers);
            collect_expr_qualifiers(high, qualifiers);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_expr_qualifiers(expr, qualifiers);
            for item in list {
                collect_expr_qualifiers(item, qualifiers);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for arg in args {
                collect_expr_qualifiers(arg, qualifiers);
            }
            for expr in &spec.partition_by {
                collect_expr_qualifiers(expr, qualifiers);
            }
            for order in &spec.order_by {
                collect_expr_qualifiers(&order.expr, qualifiers);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base.as_ref() {
                collect_expr_qualifiers(base, qualifiers);
            }
            for (cond, result) in when {
                collect_expr_qualifiers(cond, qualifiers);
                collect_expr_qualifiers(result, qualifiers);
            }
            if let Some(else_branch) = else_branch.as_ref() {
                collect_expr_qualifiers(else_branch, qualifiers);
            }
        }
        ScalarExpr::InSubquery { expr, .. } => collect_expr_qualifiers(expr, qualifiers),
        ScalarExpr::Column(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => {}
    }
}

fn expr_has_unqualified_column(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Column(_) => true,
        ScalarExpr::Array(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(expr_has_unqualified_column)
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_has_unqualified_column)
                || order_by
                    .iter()
                    .any(|order| expr_has_unqualified_column(&order.expr))
                || filter
                    .as_ref()
                    .is_some_and(|filter| expr_has_unqualified_column(filter))
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_has_unqualified_column(lhs) || expr_has_unqualified_column(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_has_unqualified_column(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_has_unqualified_column(expr)
                || expr_has_unqualified_column(low)
                || expr_has_unqualified_column(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_has_unqualified_column(expr) || list.iter().any(expr_has_unqualified_column)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(expr_has_unqualified_column)
                || spec.partition_by.iter().any(expr_has_unqualified_column)
                || spec
                    .order_by
                    .iter()
                    .any(|order| expr_has_unqualified_column(&order.expr))
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref()
                .is_some_and(|expr| expr_has_unqualified_column(expr))
                || when.iter().any(|(cond, result)| {
                    expr_has_unqualified_column(cond) || expr_has_unqualified_column(result)
                })
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_has_unqualified_column(expr))
        }
        ScalarExpr::InSubquery { expr, .. } => expr_has_unqualified_column(expr),
        ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn qualify_unqualified_columns(expr: &ScalarExpr, qualifier: &str) -> ScalarExpr {
    match expr {
        ScalarExpr::Column(column) => ScalarExpr::qualified_column(qualifier, column),
        ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::Star => expr.clone(),
        ScalarExpr::Array(items) => ScalarExpr::Array(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::And(items) => ScalarExpr::And(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::Or(items) => ScalarExpr::Or(
            items
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
        ),
        ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(qualify_unqualified_columns(lhs, qualifier)),
            rhs: Box::new(qualify_unqualified_columns(rhs, qualifier)),
        },
        ScalarExpr::Not(inner) => {
            ScalarExpr::Not(Box::new(qualify_unqualified_columns(inner, qualifier)))
        }
        ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            negated: *negated,
        },
        ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            low: Box::new(qualify_unqualified_columns(low, qualifier)),
            high: Box::new(qualify_unqualified_columns(high, qualifier)),
        },
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => ScalarExpr::InList {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            list: list
                .iter()
                .map(|item| qualify_unqualified_columns(item, qualifier))
                .collect(),
            negated: *negated,
        },
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => ScalarExpr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| qualify_unqualified_columns(arg, qualifier))
                .collect(),
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_ref()
                .map(|filter| Box::new(qualify_unqualified_columns(filter, qualifier))),
        },
        ScalarExpr::WindowCall { name, args, spec } => ScalarExpr::WindowCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| qualify_unqualified_columns(arg, qualifier))
                .collect(),
            spec: spec.clone(),
        },
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => ScalarExpr::Case {
            base: base
                .as_ref()
                .map(|expr| Box::new(qualify_unqualified_columns(expr, qualifier))),
            when: when
                .iter()
                .map(|(cond, result)| {
                    (
                        qualify_unqualified_columns(cond, qualifier),
                        qualify_unqualified_columns(result, qualifier),
                    )
                })
                .collect(),
            else_branch: else_branch
                .as_ref()
                .map(|expr| Box::new(qualify_unqualified_columns(expr, qualifier))),
        },
        ScalarExpr::Cast { expr, ty } => ScalarExpr::Cast {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            ty: ty.clone(),
        },
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => ScalarExpr::InSubquery {
            expr: Box::new(qualify_unqualified_columns(expr, qualifier)),
            subquery: *subquery,
            negated: *negated,
        },
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } => expr.clone(),
    }
}

fn expand_from_star_columns(
    engine: &Engine,
    columns: Vec<String>,
    projections: &[ProjectionPlan],
    from: &SourcePlan,
) -> Vec<String> {
    let has_star = projections
        .iter()
        .any(|p| matches!(p.expr, ScalarExpr::Star));
    if !has_star {
        return columns;
    }
    let source_cols = from_clause_output_columns(engine, from);
    if source_cols.is_empty() {
        return columns;
    }
    let mut out = Vec::with_capacity(columns.len() + source_cols.len());
    for column in columns {
        if column == "*" {
            out.extend(source_cols.iter().cloned());
        } else {
            out.push(column);
        }
    }
    out
}

fn from_clause_output_columns(engine: &Engine, from: &SourcePlan) -> Vec<String> {
    match from {
        SourcePlan::Function {
            name,
            alias,
            column_aliases,
            ..
        } => {
            let cols = if column_aliases.is_empty() {
                user_function_output_columns(engine, name).unwrap_or_else(|| vec![name.clone()])
            } else {
                column_aliases.clone()
            };
            qualify_output_columns(alias.as_deref(), cols)
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
        } => {
            let cols = if column_aliases.is_empty() {
                let width = rows.first().map_or(0, Vec::len);
                (0..width).map(|idx| format!("column{}", idx + 1)).collect()
            } else {
                column_aliases.clone()
            };
            qualify_output_columns(alias.as_deref(), cols)
        }
        SourcePlan::Subquery {
            alias,
            column_aliases,
            ..
        } => qualify_output_columns(alias.as_deref(), column_aliases.clone()),
        SourcePlan::Join { left, right, .. } => {
            let mut cols = from_clause_output_columns(engine, left);
            cols.extend(from_clause_output_columns(engine, right));
            cols
        }
        SourcePlan::Table { .. } => Vec::new(),
    }
}

/// Output column names of a user-defined routine used as a FROM
/// source: OUT / INOUT / `RETURNS TABLE` parameter names. `None` when
/// the name is not a user routine or its result is a single unnamed
/// column (which keeps the function-name default).
fn user_function_output_columns(engine: &Engine, name: &str) -> Option<Vec<String>> {
    let overloads = engine.lookup_sql_functions(name)?;
    for function in &overloads {
        let outs = function.def.output_params();
        if !outs.is_empty() {
            return Some(
                outs.iter()
                    .enumerate()
                    .map(|(idx, p)| {
                        if p.name.is_empty() {
                            format!("column{}", idx + 1)
                        } else {
                            p.name.clone()
                        }
                    })
                    .collect(),
            );
        }
    }
    None
}

fn qualify_output_columns(alias: Option<&str>, columns: Vec<String>) -> Vec<String> {
    match alias {
        Some(a) => columns
            .into_iter()
            .map(|column| format!("{a}.{column}"))
            .collect(),
        None => columns,
    }
}

pub(super) fn physical_exec_error(error: uqa_execution::ExecError) -> SQLError {
    match error {
        uqa_execution::ExecError::SQL(error) => error,
        uqa_execution::ExecError::Other(message) => SQLError::Internal(message),
    }
}

fn close_after_physical_failure(
    operator: &mut dyn uqa_execution::PhysicalOperator,
    error: uqa_execution::ExecError,
    stage: &str,
) -> SQLError {
    match operator.close() {
        Ok(()) => physical_exec_error(error),
        Err(close_error) => SQLError::Internal(format!(
            "{error}; operator close after {stage} failure also failed: {close_error}"
        )),
    }
}

pub(super) fn physical_work_mem_bytes(engine: &Engine) -> Result<usize, SQLError> {
    engine.work_mem_bytes()
}

fn source_columns(rows: &[ResultRow]) -> Vec<String> {
    rows.first()
        .map(|row| row.keys().cloned().collect())
        .unwrap_or_default()
}

fn physical_projections(projections: &[ProjectionPlan]) -> Vec<PhysicalProjection> {
    let labels = projection_columns(projections);
    projections
        .iter()
        .enumerate()
        .map(|(index, projection)| (labels[index].clone(), projection.expr.clone()))
        .collect()
}

fn score_provenance_columns(schema: &[String]) -> Vec<String> {
    schema
        .iter()
        .filter(|column| is_score_provenance_column(column))
        .cloned()
        .collect()
}

fn append_score_provenance_projections(
    projections: &mut Vec<PhysicalProjection>,
    schema: &[String],
) {
    for column in score_provenance_columns(schema) {
        if !projections.iter().any(|(name, _)| name == &column) {
            projections.push((column.clone(), ScalarExpr::Column(column)));
        }
    }
}

fn append_score_provenance_mappings(mappings: &mut Vec<OutputColumnMapping>, schema: &[String]) {
    for column in score_provenance_columns(schema) {
        if !mappings.iter().any(|(name, _)| name == &column) {
            mappings.push((column.clone(), column));
        }
    }
}

fn visible_projection_source_column(column: &str) -> bool {
    !matches!(column, SCORE_COLUMN | DOC_ID_COLUMN | MERGE_ACTION_COLUMN)
        && !is_score_provenance_column(column)
}

/// Build collision-free physical target columns for a plain SELECT whose
/// ORDER BY must be able to see both input columns and SELECT-list aliases.
///
/// Public aliases cannot safely be appended directly: `SELECT x + 1 AS x
/// ... ORDER BY x` must order by the output alias, while `ORDER BY x + 1`
/// still resolves `x` against the input namespace. Each non-star target is
/// therefore computed once under an internal name and renamed only after
/// Sort/Limit has consumed it.
fn order_projection(
    projections: &[ProjectionPlan],
    input_columns: &[String],
) -> (Vec<PhysicalProjection>, Vec<OutputColumnMapping>) {
    let labels = projection_columns(projections);
    let mut physical = Vec::new();
    let mut output = Vec::new();
    let mut occupied: HashSet<String> = input_columns.iter().cloned().collect();

    for (index, projection) in projections.iter().enumerate() {
        if matches!(projection.expr, ScalarExpr::Star) {
            for column in input_columns {
                if visible_projection_source_column(column)
                    && !output
                        .iter()
                        .any(|(name, _): &(String, String)| name == column)
                {
                    output.push((column.clone(), column.clone()));
                }
            }
            continue;
        }

        let mut internal = format!("__uqa_projection_{index}");
        let mut suffix = 0usize;
        while occupied.contains(&internal) {
            suffix += 1;
            internal = format!("__uqa_projection_{index}_{suffix}");
        }
        occupied.insert(internal.clone());
        physical.push((internal.clone(), projection.expr.clone()));
        output.push((labels[index].clone(), internal));
    }
    (physical, output)
}

fn identity_order_columns(columns: &[String]) -> Vec<OutputColumnMapping> {
    columns
        .iter()
        .map(|column| (column.clone(), column.clone()))
        .collect()
}

fn resolve_order_expression(
    expression: &ScalarExpr,
    output_columns: &[OutputColumnMapping],
) -> Result<ScalarExpr, SQLError> {
    match expression {
        ScalarExpr::Literal(Value::Int(position)) => {
            let index = usize::try_from(*position)
                .ok()
                .and_then(|position| position.checked_sub(1))
                .filter(|index| *index < output_columns.len())
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!(
                        "ORDER BY position {position} is not in the select list"
                    ))
                })?;
            Ok(ScalarExpr::Column(output_columns[index].1.clone()))
        }
        // SQL output aliases are visible only as a bare ORDER BY name. A
        // name embedded in a larger expression continues to bind to the
        // input row, which is why this rewrite deliberately is not recursive.
        ScalarExpr::Column(name) => Ok(output_columns
            .iter()
            .find(|(output, _)| output == name)
            .map_or_else(
                || expression.clone(),
                |(_, physical)| ScalarExpr::Column(physical.clone()),
            )),
        _ => Ok(expression.clone()),
    }
}

pub(super) fn execute_filter_rows(
    engine: &Engine,
    rows: Vec<ResultRow>,
    predicate: ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_execution::scan::TableScan;
    use uqa_execution::{physical::run_to_rows, Filter, PhysicalOperator};

    let columns = source_columns(&rows);
    let scan: Box<dyn PhysicalOperator + '_> = Box::new(TableScan::from_rows(columns, rows));
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    let mut filter = Filter::with_evaluator(scan, predicate, evaluator);
    let (_, rows) = run_to_rows(&mut filter).map_err(physical_exec_error)?;
    Ok(rows)
}

fn attach_order_limit<'a>(
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    statement: &'a QueryBlockPlan,
    output_columns: &[(String, String)],
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::{ExternalSort, Limit, SortKey};

    let offset =
        resolve_limit_offset_with_ctes(statement.offset.as_ref(), engine, params, "OFFSET", ctes)?;
    let limit =
        resolve_limit_offset_with_ctes(statement.limit.as_ref(), engine, params, "LIMIT", ctes)?;
    if !statement.order_by.is_empty() {
        let work_mem_bytes = physical_work_mem_bytes(engine)?;
        let keys = statement
            .order_by
            .iter()
            .map(|order| {
                Ok(SortKey {
                    expr: resolve_order_expression(&order.expr, output_columns)?,
                    descending: order.descending,
                    nulls_first: order
                        .nulls
                        .map(|nulls| matches!(nulls, uqa_sql::ast::NullsOrder::First)),
                })
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let keep = if let Some(limit) = limit {
            let keep = offset
                .unwrap_or(0)
                .checked_add(limit)
                .ok_or_else(|| SQLError::TypeMismatch("OFFSET + LIMIT overflow".into()))?;
            Some(usize::try_from(keep).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "OFFSET + LIMIT {keep} exceeds the platform row-count range"
                ))
            })?)
        } else {
            None
        };
        operator = Box::new(ExternalSort::new(
            operator,
            keys,
            evaluator,
            keep,
            work_mem_bytes,
        ));
    }
    if offset.is_some() || limit.is_some() {
        operator = Box::new(Limit::new(operator, offset.unwrap_or(0), limit));
    }
    Ok(operator)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_query_block_operator_output<'a>(
    engine: &'a Engine,
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    statement: &'a QueryBlockPlan,
    original: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    columns: Vec<String>,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let operator = build_relational_operator(engine, operator, predicate, statement, params, ctes)?;
    finish_query_block_operator_output(
        engine,
        operator,
        original,
        params,
        ctes,
        columns,
        output_mode,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_query_block_operator_output<'a>(
    engine: &'a Engine,
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    original: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    columns: Vec<String>,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    use uqa_execution::{Distinct, Limit};

    if original.distinct {
        let work_mem_bytes = physical_work_mem_bytes(engine)?;
        operator = if original.distinct_on.is_empty() {
            if operator
                .schema()
                .iter()
                .any(|column| is_score_provenance_column(column))
            {
                Box::new(Distinct::on_with_work_mem(
                    operator,
                    columns.iter().cloned().map(ScalarExpr::Column).collect(),
                    EngineExpressionEvaluator::shared(engine, params, ctes),
                    work_mem_bytes,
                ))
            } else {
                Box::new(Distinct::all_with_work_mem(operator, work_mem_bytes))
            }
        } else {
            Box::new(Distinct::on_with_work_mem(
                operator,
                original.distinct_on.clone(),
                EngineExpressionEvaluator::shared(engine, params, ctes),
                work_mem_bytes,
            ))
        };
    }
    if should_defer_distinct_limit(original) {
        let offset = resolve_limit_offset_with_ctes(
            original.offset.as_ref(),
            engine,
            params,
            "OFFSET",
            ctes,
        )?;
        let limit =
            resolve_limit_offset_with_ctes(original.limit.as_ref(), engine, params, "LIMIT", ctes)?;
        operator = Box::new(Limit::new(operator, offset.unwrap_or(0), limit));
    }
    collect_query_operator(engine, columns, operator, output_mode)
}

fn build_relational_operator<'a>(
    engine: &'a Engine,
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    statement: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::{ColumnSelection, Filter, HashAggregate, Project, Window};

    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    if let Some(predicate) = predicate {
        operator = Box::new(Filter::with_evaluator(
            operator,
            predicate,
            Arc::clone(&evaluator),
        ));
    }

    match statement.compute {
        ComputePlan::Project => {
            if statement.order_by.is_empty() {
                // Without ordering, Limit may stop the child before unused
                // target expressions are evaluated.
                operator = attach_order_limit(
                    operator,
                    statement,
                    &[],
                    engine,
                    params,
                    ctes,
                    Arc::clone(&evaluator),
                )?;
                let mut projections = physical_projections(&statement.projections);
                append_score_provenance_projections(&mut projections, operator.schema());
                operator = Box::new(Project::with_evaluator(operator, projections, evaluator));
            } else {
                let (physical, mut output) =
                    order_projection(&statement.projections, operator.schema());
                // SQL ordinals and aliases are resolved only against the
                // visible SELECT list. Score provenance is carried through
                // the final column selection for parent query blocks, but it
                // is not itself a selectable output position.
                let order_output = output.clone();
                append_score_provenance_mappings(&mut output, operator.schema());
                operator = Box::new(Project::appending_with_evaluator(
                    operator,
                    physical,
                    Arc::clone(&evaluator),
                ));
                operator = attach_order_limit(
                    operator,
                    statement,
                    &order_output,
                    engine,
                    params,
                    ctes,
                    evaluator,
                )?;
                operator = Box::new(ColumnSelection::with_mapping(operator, output));
            }
        }
        ComputePlan::Aggregate => {
            let schema = projection_columns(&statement.projections);
            let input_schema = operator.schema().to_vec();
            let work_mem_bytes = physical_work_mem_bytes(engine)?;
            operator = Box::new(HashAggregate::with_executor(
                operator,
                schema.clone(),
                Box::new(PhysicalAggregateExecutor::new(
                    engine,
                    statement,
                    params,
                    ctes,
                    input_schema,
                    work_mem_bytes,
                )),
            ));
            let output = identity_order_columns(&schema);
            operator = attach_order_limit(
                operator, statement, &output, engine, params, ctes, evaluator,
            )?;
        }
        ComputePlan::Window => {
            let source_schema = operator.schema().to_vec();
            let work_mem_bytes = physical_work_mem_bytes(engine)?;
            let window_plan = prepare_window_plan(&statement.projections);
            let mut projections = physical_projections(window_plan.projections());
            let schema = window_plan.output_columns(operator.schema());
            operator = Box::new(Window::with_executor(
                operator,
                schema,
                Box::new(PhysicalWindowExecutor::new(
                    engine,
                    window_plan,
                    params,
                    ctes,
                    source_schema.clone(),
                    work_mem_bytes,
                )),
            ));
            append_score_provenance_projections(&mut projections, operator.schema());
            operator = Box::new(Project::with_evaluator(
                operator,
                projections,
                Arc::clone(&evaluator),
            ));
            let output_columns = order_projection(&statement.projections, &source_schema)
                .1
                .into_iter()
                .map(|(output, _)| (output.clone(), output))
                .collect::<Vec<_>>();
            operator = attach_order_limit(
                operator,
                statement,
                &output_columns,
                engine,
                params,
                ctes,
                evaluator,
            )?;
        }
    }

    Ok(operator)
}

fn walk_expr<F: FnMut(&ScalarExpr)>(expr: &ScalarExpr, f: &mut F) {
    f(expr);
    match expr {
        ScalarExpr::And(parts) | ScalarExpr::Or(parts) => {
            for p in parts {
                walk_expr(p, f);
            }
        }
        ScalarExpr::Not(inner) => walk_expr(inner, f),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        ScalarExpr::IsNull { expr, .. } => walk_expr(expr, f),
        ScalarExpr::Between { expr, low, high } => {
            walk_expr(expr, f);
            walk_expr(low, f);
            walk_expr(high, f);
        }
        ScalarExpr::InList { expr, list, .. } => {
            walk_expr(expr, f);
            for p in list {
                walk_expr(p, f);
            }
        }
        ScalarExpr::Func { args, .. } | ScalarExpr::WindowCall { args, .. } => {
            for p in args {
                walk_expr(p, f);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(b) = base {
                walk_expr(b, f);
            }
            for (c, r) in when {
                walk_expr(c, f);
                walk_expr(r, f);
            }
            if let Some(e) = else_branch {
                walk_expr(e, f);
            }
        }
        ScalarExpr::Cast { expr, .. } => walk_expr(expr, f),
        ScalarExpr::Array(items) => {
            for p in items {
                walk_expr(p, f);
            }
        }
        _ => {}
    }
}

fn expr_contains_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |part| {
        if expr_is_jsonpath_fts_match(part) {
            found = true;
        }
    });
    found
}

fn expr_is_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
    matches!(
        expr,
        ScalarExpr::Func { name, args, .. }
            if name.eq_ignore_ascii_case("fts_match")
                && matches!(
                    args.get(1),
                    Some(ScalarExpr::Literal(Value::Str(path))) if path.trim_start().starts_with('$')
                )
    )
}

/// Iterate the recursive `CtePlan`: take the anchor (LHS of UNION ALL) as
/// the initial row set, then repeatedly evaluate the recursive step
/// (RHS) with the `CtePlan` bound to the *new rows from the previous
/// iteration* (working set), unioning the result back into the total.
/// Caps at 1024 iterations to keep buggy queries from running away.
fn materialize_recursive_cte(
    engine: &Engine,
    cte: &CtePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_filter: Option<&(String, ScalarExpr)>,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    if !cte.query.ctes.is_empty() {
        materialize_plan_ctes(engine, &cte.query.ctes, params, ctes)?;
    }

    let RelationalPlan::SetOp {
        kind,
        all,
        left,
        right,
        order_by,
        limit,
        offset,
        subqueries,
    } = &cte.query.root
    else {
        return Err(SQLError::Unsupported(
            "recursive CTE requires a UNION query".into(),
        ));
    };
    if *kind != SetOpKind::Union {
        return Err(SQLError::Unsupported(
            "recursive CTE only supports UNION".into(),
        ));
    }

    let declared_columns = (!cte.columns.is_empty()).then_some(cte.columns.as_slice());
    let (anchor_plan, step_plan) = if let Some((qualifier, filter)) = output_filter {
        let output_columns = declared_columns
            .map(<[String]>::to_vec)
            .or_else(|| query_plan_output_columns(left));
        match output_columns {
            Some(output_columns) => {
                let specialized_anchor = push_output_filter_into_query_plan(
                    engine,
                    left,
                    qualifier,
                    filter,
                    Some(&output_columns),
                )?;
                let specialized_step = push_output_filter_into_query_plan(
                    engine,
                    right,
                    qualifier,
                    filter,
                    Some(&output_columns),
                )?;
                match (specialized_anchor, specialized_step) {
                    (Some(anchor), Some(step)) => (anchor, step),
                    _ => ((**left).clone(), (**right).clone()),
                }
            }
            None => ((**left).clone(), (**right).clone()),
        }
    } else {
        ((**left).clone(), (**right).clone())
    };

    let anchor = execute_query_plan_output(
        engine,
        &anchor_plan,
        params,
        ctes,
        QueryOutputMode::SharedSpill,
    )?;
    let anchor_columns = if cte.columns.is_empty() {
        anchor.columns.clone()
    } else {
        cte.columns.clone()
    };
    let mut working = alias_query_output_to_shared(engine, anchor, &anchor_columns)?;

    let work_mem = physical_work_mem_bytes(engine)?.max(1);
    // The accumulated rows and UNION duplicate state are live together. Give
    // each at most half of work_mem; SharedSpill working sets are disk-only.
    let state_budget = (work_mem / 2).max(1);
    let mut accumulated = uqa_execution::SpillBuffer::new(state_budget);
    let mut seen = (!*all).then(|| uqa_execution::ExactRowSet::new(state_budget));
    if let Some(seen) = seen.as_mut() {
        working = filter_new_recursive_rows(&working, &anchor_columns, seen)?;
    }

    const MAX_ITERATIONS: usize = 1024;
    let mut iterations = 0usize;
    while working.rows() != 0 {
        if iterations == MAX_ITERATIONS {
            return Err(SQLError::Unsupported(format!(
                "recursive CTE `{}` exceeded {MAX_ITERATIONS} iterations",
                cte.name
            )));
        }
        iterations += 1;

        append_shared_spill(&mut accumulated, &working)?;
        ctes.insert_shared(cte.name.clone(), working);
        let step_result = execute_query_plan_output(
            engine,
            &step_plan,
            params,
            ctes,
            QueryOutputMode::SharedSpill,
        );
        ctes.remove_materialized(&cte.name);
        let step = step_result?;
        working = alias_query_output_to_shared(engine, step, &anchor_columns)?;
        if let Some(seen) = seen.as_mut() {
            working = filter_new_recursive_rows(&working, &anchor_columns, seen)?;
        }
    }

    let rows = accumulated
        .into_shared(anchor_columns.clone())
        .map_err(physical_exec_error)?;

    if order_by.is_empty() && limit.is_none() && offset.is_none() {
        return Ok(rows);
    }
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
    let operation: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::SharedSpillScan::new(rows));
    let output = identity_order_columns(&anchor_columns);
    let operation = attach_order_limit(
        operation,
        &synthetic,
        &output,
        engine,
        params,
        &ordering_scope,
        EngineExpressionEvaluator::shared(engine, params, &ordering_scope),
    )?;
    let output = collect_query_operator(
        engine,
        anchor_columns,
        operation,
        QueryOutputMode::SharedSpill,
    )?;
    let QueryRows::SharedSpill(rows) = output.rows else {
        return Err(SQLError::Internal(
            "recursive CTE collector returned in-memory rows".into(),
        ));
    };
    Ok(rows)
}

fn alias_query_output_to_shared(
    engine: &Engine,
    output: QueryOutput,
    aliases: &[String],
) -> Result<uqa_execution::SharedSpill, SQLError> {
    let visible_source_columns = output.columns.clone();
    let source_columns = output.internal_columns.clone();
    let columns = visible_source_columns
        .iter()
        .enumerate()
        .map(|(index, source)| {
            aliases
                .get(index)
                .cloned()
                .unwrap_or_else(|| source.clone())
        })
        .collect::<Vec<_>>();
    let mut operator = output.into_operator();
    if source_columns != columns {
        let mapping = source_columns
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let output = if is_score_provenance_column(source) {
                    source.clone()
                } else {
                    columns
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| source.clone())
                };
                (output, source.clone())
            })
            .collect::<Vec<_>>();
        operator = Box::new(uqa_execution::ColumnSelection::with_mapping(
            operator, mapping,
        ));
    }
    let output = collect_query_operator(engine, columns, operator, QueryOutputMode::SharedSpill)?;
    let QueryRows::SharedSpill(rows) = output.rows else {
        return Err(SQLError::Internal(
            "recursive term collector returned in-memory rows".into(),
        ));
    };
    Ok(rows)
}

fn append_shared_spill(
    output: &mut uqa_execution::SpillBuffer,
    rows: &uqa_execution::SharedSpill,
) -> Result<(), SQLError> {
    let reader = rows.reader().map_err(physical_exec_error)?;
    for batch in reader {
        output
            .push(batch.map_err(physical_exec_error)?)
            .map_err(physical_exec_error)?;
    }
    Ok(())
}

fn filter_new_recursive_rows(
    input: &uqa_execution::SharedSpill,
    columns: &[String],
    seen: &mut uqa_execution::ExactRowSet,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    // The source is already disk-backed. Retain no cardinality-sized tail
    // while constructing the next working set.
    let mut output = uqa_execution::SpillBuffer::new(1);
    let schema = uqa_execution::RowSchema::new(columns.to_vec());
    let reader = input.reader().map_err(physical_exec_error)?;
    for batch in reader {
        let batch = batch.map_err(physical_exec_error)?;
        let mut rows = Vec::with_capacity(batch.rows.len().min(uqa_execution::DEFAULT_BATCH_SIZE));
        for row in batch.rows {
            if seen
                .insert_row(&row, columns)
                .map_err(physical_exec_error)?
            {
                rows.push(row);
            }
        }
        if !rows.is_empty() {
            output
                .push(uqa_execution::Batch::new(schema.clone(), rows))
                .map_err(physical_exec_error)?;
        }
    }
    output
        .into_shared(columns.to_vec())
        .map_err(physical_exec_error)
}

pub(super) enum ScoredInput {
    All,
    Entries {
        entries: Vec<ScoredEntry>,
        score_bearing: bool,
    },
}

impl ScoredInput {
    pub(super) fn entries(entries: Vec<ScoredEntry>, score_bearing: bool) -> Self {
        Self::Entries {
            entries,
            score_bearing,
        }
    }
}

pub(super) struct ScoredDocumentSource {
    table_name: String,
    table: Arc<crate::TableState>,
    input: ScoredInputCursor,
    schema: Vec<String>,
    score_bearing: bool,
}

enum ScoredInputCursor {
    All { after: Option<DocId> },
    Entries(std::vec::IntoIter<ScoredEntry>),
}

impl ScoredDocumentSource {
    pub(super) fn new(
        table_name: &str,
        table: Arc<crate::TableState>,
        input: ScoredInput,
        mut schema: Vec<String>,
    ) -> Self {
        for hidden in [DOC_ID_COLUMN, SCORE_COLUMN, SCORE_PROVENANCE_COLUMN] {
            if !schema.iter().any(|column| column == hidden) {
                schema.push(hidden.to_string());
            }
        }
        let (input, score_bearing) = match input {
            ScoredInput::All => (ScoredInputCursor::All { after: None }, false),
            ScoredInput::Entries {
                entries,
                score_bearing,
            } => (
                ScoredInputCursor::Entries(entries.into_iter()),
                score_bearing,
            ),
        };
        Self {
            table_name: table_name.to_string(),
            table,
            input,
            schema,
            score_bearing,
        }
    }

    fn next_entry(&mut self) -> Result<Option<ScoredEntry>, SQLError> {
        match &mut self.input {
            ScoredInputCursor::Entries(entries) => Ok(entries.next()),
            ScoredInputCursor::All { after } => {
                let next = self
                    .table
                    .document_store
                    .read()
                    .next_doc_id(*after)
                    .map_err(|error| {
                        SQLError::Internal(format!(
                            "scan document ids for `{}`: {error}",
                            self.table_name
                        ))
                    })?;
                *after = next;
                Ok(next.map(|doc_id| ScoredEntry { doc_id, score: 0.0 }))
            }
        }
    }
}

impl uqa_execution::RowSource for ScoredDocumentSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
        let Some(entry) = self.next_entry()? else {
            return Ok(None);
        };
        let mut document = self
            .table
            .document_store
            .read()
            .get(entry.doc_id)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "read `{}` document {}: {error}",
                    self.table_name, entry.doc_id
                ))
            })?
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "access path returned document {}, but table `{}` omitted it",
                    entry.doc_id, self.table_name
                ))
            })?;
        document.insert(DOC_ID_COLUMN.into(), doc_id_value(entry.doc_id)?);
        document.insert(SCORE_COLUMN.into(), Value::Float(entry.score));
        document.insert(
            SCORE_PROVENANCE_COLUMN.into(),
            if self.score_bearing {
                Value::Float(entry.score)
            } else {
                Value::Null
            },
        );
        Ok(Some(document))
    }
}

fn run_single_table_select_output(
    engine: &Engine,
    table: &str,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    if let Some(filter) = stmt.r#where.as_ref() {
        super::validate_expr_text_match_fields(engine, table, filter)?;
    }
    let score_top_k = if matches!(
        block.access,
        AccessPathPlan::OperatorTree {
            score_limit_pushdown: true
        }
    ) {
        score_order_top_k(stmt, engine, params, ctes)?
            .filter(|_| score_limited_text_filter(stmt.r#where.as_ref()))
    } else {
        None
    };
    let has_jsonpath_fts_filter = stmt
        .r#where
        .as_ref()
        .is_some_and(expr_contains_jsonpath_fts_match);
    // Try the operator-tree pipeline first: lower the WHERE clause to
    // an `OperatorTree`, run `QueryOptimizer` (10 algebraic / graph-
    // aware / fusion-reordering passes - compatibility), then execute
    // through `PlanExecutor` against an `EngineDriver`. The bridge
    // returns `None` for shapes that are not posting-list access paths
    // (arithmetic across columns, subqueries, window calls, ...); those
    // remain scalar predicates in this relational filter node.
    let optimised = if has_jsonpath_fts_filter
        || !matches!(block.access, AccessPathPlan::OperatorTree { .. })
    {
        None
    } else if let (Some(top_k), Some(ScalarExpr::Func { name, args, .. })) =
        (score_top_k, stmt.r#where.as_ref())
    {
        Some(execute_function_with_top_k(
            engine,
            table,
            name,
            args,
            params,
            Some(top_k),
        )?)
    } else {
        crate::operator_tree_bridge::run_optimised(engine, table, stmt.r#where.as_ref(), params)?
    };
    let score_bearing_filter = stmt
        .r#where
        .as_ref()
        .is_some_and(uqa_planner::optimizer::contains_retrieval);
    let (scored, mut physical_filter) = if let Some(rows) = optimised {
        (ScoredInput::entries(rows, score_bearing_filter), None)
    } else {
        match &block.access {
            AccessPathPlan::Row => (ScoredInput::All, stmt.r#where.clone()),
            AccessPathPlan::Hybrid => {
                let rows = match stmt.r#where.as_ref() {
                    Some(filter) => ScoredInput::entries(
                        execute_mixed_where(engine, table, filter, params)?,
                        uqa_planner::optimizer::contains_retrieval(filter),
                    ),
                    None => ScoredInput::All,
                };
                (rows, None)
            }
            AccessPathPlan::OperatorTree { .. } => {
                let rows = match stmt.r#where.as_ref() {
                    Some(filter_expr @ ScalarExpr::Func { name, args, .. })
                        if uqa_sql::registry::is_registered(name)
                            && !expr_is_jsonpath_fts_match(filter_expr) =>
                    {
                        ScoredInput::entries(
                            execute_function(engine, table, name, args, params)?,
                            uqa_planner::optimizer::contains_retrieval(filter_expr),
                        )
                    }
                    // The planner may optimistically choose the operator-tree
                    // access class for a predicate that the posting-list IR
                    // cannot represent (for example `IS NULL`, arithmetic, or
                    // a subquery). Keep it inside the same physical query
                    // pipeline as a relational Filter over the table scan.
                    Some(_) => ScoredInput::All,
                    None => ScoredInput::All,
                };
                let filter = matches!(rows, ScoredInput::All)
                    .then(|| stmt.r#where.clone())
                    .flatten();
                (rows, filter)
            }
        }
    };

    if let Some(facet_fields) = facet_projection_fields(&stmt.projections)? {
        let execution = FacetExecution {
            fields: &facet_fields,
            params,
            ctes,
            output_mode,
        };
        return build_facet_output(engine, table, scored, physical_filter.take(), execution);
    }

    let table_state = engine.require_table(table)?;
    let source_schema = engine.try_table_columns(table).map_err(|error| {
        SQLError::Internal(format!("read table columns for `{table}`: {error}"))
    })?;
    let source = ScoredDocumentSource::new(table, table_state, scored, source_schema);
    let source: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::TableScan::new(Box::new(source)));
    let columns = if matches!(block.compute, ComputePlan::Project) {
        expand_star_columns(
            projection_columns(&stmt.projections),
            &stmt.projections,
            engine,
            Some(table),
        )?
    } else {
        projection_columns(&stmt.projections)
    };
    execute_query_block_operator_output(
        engine,
        source,
        physical_filter,
        stmt,
        block,
        params,
        ctes,
        columns,
        output_mode,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_single_foreign_select_output(
    engine: &Engine,
    table: &str,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let predicates = fdw_predicates_from_where(stmt.r#where.as_ref(), params);
    let scanned = engine
        .scan_foreign_table_stream(table, None, &predicates, None)
        .map_err(SQLError::Unsupported)?;
    let columns = if matches!(block.compute, ComputePlan::Project) {
        expand_star_columns(
            projection_columns(&stmt.projections),
            &stmt.projections,
            engine,
            Some(table),
        )?
    } else {
        projection_columns(&stmt.projections)
    };
    let source_columns = engine
        .foreign_table_columns(table)
        .map_err(SQLError::Unsupported)?;
    let source: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::RowIteratorScan::new(
            source_columns,
            Box::new(scanned.map(|row| {
                row.map_err(SQLError::Unsupported)
                    .map_err(uqa_execution::ExecError::from)
            })),
        ));
    execute_query_block_operator_output(
        engine,
        source,
        stmt.r#where.clone(),
        stmt,
        block,
        params,
        ctes,
        columns,
        output_mode,
    )
}

fn fdw_predicates_from_where(
    expr: Option<&ScalarExpr>,
    params: &[SQLParam],
) -> Vec<uqa_fdw::FDWPredicate> {
    let Some(expr) = expr else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_fdw_predicates(expr, params, &mut out);
    out
}

fn collect_fdw_predicates(
    expr: &ScalarExpr,
    params: &[SQLParam],
    out: &mut Vec<uqa_fdw::FDWPredicate>,
) {
    match expr {
        ScalarExpr::And(parts) => {
            for part in parts {
                collect_fdw_predicates(part, params, out);
            }
        }
        _ => {
            if let Some(predicate) = fdw_predicate(expr, params) {
                out.push(predicate);
            }
        }
    }
}

fn fdw_predicate(expr: &ScalarExpr, params: &[SQLParam]) -> Option<uqa_fdw::FDWPredicate> {
    match expr {
        ScalarExpr::Binary { op, lhs, rhs } => {
            if let Some(column) = fdw_column_name(lhs) {
                let value = fdw_const_value(rhs, params)?;
                return Some(uqa_fdw::FDWPredicate {
                    column,
                    operator: fdw_binary_op(*op)?,
                    value,
                });
            }
            if let Some(column) = fdw_column_name(rhs) {
                let value = fdw_const_value(lhs, params)?;
                return Some(uqa_fdw::FDWPredicate {
                    column,
                    operator: fdw_reversed_binary_op(*op)?,
                    value,
                });
            }
            None
        }
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } if !negated => {
            let column = fdw_column_name(expr)?;
            let values = list
                .iter()
                .map(|item| fdw_const_value(item, params))
                .collect::<Option<Vec<_>>>()?;
            Some(uqa_fdw::FDWPredicate {
                column,
                operator: uqa_fdw::PredicateOp::In,
                value: Value::List(values),
            })
        }
        ScalarExpr::IsNull { expr, negated } => Some(uqa_fdw::FDWPredicate {
            column: fdw_column_name(expr)?,
            operator: if *negated {
                uqa_fdw::PredicateOp::NotEq
            } else {
                uqa_fdw::PredicateOp::Eq
            },
            value: Value::Null,
        }),
        ScalarExpr::Func { name, args, .. } => fdw_like_predicate(name, args, false, params),
        ScalarExpr::Not(inner) => match inner.as_ref() {
            ScalarExpr::Func { name, args, .. } => fdw_like_predicate(name, args, true, params),
            _ => None,
        },
        _ => None,
    }
}

fn fdw_like_predicate(
    name: &str,
    args: &[ScalarExpr],
    negated: bool,
    params: &[SQLParam],
) -> Option<uqa_fdw::FDWPredicate> {
    if args.len() != 2 {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let operator = match (lower.as_str(), negated) {
        ("like", false) => uqa_fdw::PredicateOp::Like,
        ("like", true) => uqa_fdw::PredicateOp::NotLike,
        ("ilike", false) => uqa_fdw::PredicateOp::ILike,
        ("ilike", true) => uqa_fdw::PredicateOp::NotILike,
        _ => return None,
    };
    Some(uqa_fdw::FDWPredicate {
        column: fdw_column_name(&args[0])?,
        operator,
        value: fdw_const_value(&args[1], params)?,
    })
}

fn fdw_column_name(expr: &ScalarExpr) -> Option<String> {
    match expr {
        ScalarExpr::Column(name) => Some(name.clone()),
        ScalarExpr::QualifiedColumn { column, .. } => Some(column.clone()),
        _ => None,
    }
}

fn fdw_const_value(expr: &ScalarExpr, params: &[SQLParam]) -> Option<Value> {
    let ctx = ScalarEvalContext::new(None, params);
    eval_scalar(expr, &ctx).ok()
}

fn fdw_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
    Some(match op {
        BinaryOp::Equal => uqa_fdw::PredicateOp::Eq,
        BinaryOp::NotEqual => uqa_fdw::PredicateOp::NotEq,
        BinaryOp::Less => uqa_fdw::PredicateOp::Lt,
        BinaryOp::LessEqual => uqa_fdw::PredicateOp::LtEq,
        BinaryOp::Greater => uqa_fdw::PredicateOp::Gt,
        BinaryOp::GreaterEqual => uqa_fdw::PredicateOp::GtEq,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => return None,
    })
}

fn fdw_reversed_binary_op(op: BinaryOp) -> Option<uqa_fdw::PredicateOp> {
    Some(match op {
        BinaryOp::Equal => uqa_fdw::PredicateOp::Eq,
        BinaryOp::NotEqual => uqa_fdw::PredicateOp::NotEq,
        BinaryOp::Less => uqa_fdw::PredicateOp::Gt,
        BinaryOp::LessEqual => uqa_fdw::PredicateOp::GtEq,
        BinaryOp::Greater => uqa_fdw::PredicateOp::Lt,
        BinaryOp::GreaterEqual => uqa_fdw::PredicateOp::LtEq,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => return None,
    })
}

fn facet_projection_fields(
    projections: &[ProjectionPlan],
) -> Result<Option<Vec<String>>, SQLError> {
    if projections.len() != 1 {
        return Ok(None);
    }
    let ScalarExpr::Func { name, args, .. } = &projections[0].expr else {
        return Ok(None);
    };
    if !name.eq_ignore_ascii_case("uqa_facets") {
        return Ok(None);
    }
    let mut fields = Vec::with_capacity(args.len());
    for arg in args {
        fields.push(expect_column_name(arg, "uqa_facets.field")?);
    }
    Ok(Some(fields))
}

struct FacetExecution<'a> {
    fields: &'a [String],
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    output_mode: QueryOutputMode,
}

fn build_facet_output(
    engine: &Engine,
    table: &str,
    scored: ScoredInput,
    predicate: Option<ScalarExpr>,
    execution: FacetExecution<'_>,
) -> Result<QueryOutput, SQLError> {
    use uqa_execution::{
        AggregateKind, AggregateSpec, ExternalSort, Filter, HashAggregate, PhysicalOperator,
        ProjectSet, SortKey,
    };

    let include_field = execution.fields.len() > 1;
    let table_state = engine.require_table(table)?;
    let source_schema = engine.try_table_columns(table).map_err(|error| {
        SQLError::Internal(format!("read table columns for `{table}`: {error}"))
    })?;
    let source = ScoredDocumentSource::new(table, table_state, scored, source_schema);
    let mut source: Box<dyn PhysicalOperator + '_> =
        Box::new(uqa_execution::TableScan::new(Box::new(source)));
    if let Some(predicate) = predicate {
        source = Box::new(Filter::with_evaluator(
            source,
            predicate,
            EngineExpressionEvaluator::shared(engine, execution.params, execution.ctes),
        ));
    }

    let facet_fields = execution.fields.to_vec();
    let facet_columns = if include_field {
        vec!["facet_field".into(), "facet_value".into()]
    } else {
        vec!["facet_value".into()]
    };
    let facet_rows: Box<dyn PhysicalOperator + '_> = Box::new(ProjectSet::new(
        source,
        facet_columns.clone(),
        Box::new(move |document: &ResultRow| {
            let mut rows = Vec::with_capacity(facet_fields.len());
            for field in &facet_fields {
                let Some(value) = document.get(field) else {
                    continue;
                };
                if matches!(value, Value::Null) {
                    continue;
                }
                let mut row = ResultRow::new();
                if include_field {
                    row.insert("facet_field".into(), Value::Str(field.clone()));
                }
                row.insert("facet_value".into(), value.clone());
                rows.push(row);
            }
            Ok(Box::new(rows.into_iter().map(Ok)) as uqa_execution::ProjectRows)
        }),
    ));

    // End the document/evaluator borrow phase in a bounded spill. The generic
    // external aggregate can then own a static scan while its group map and
    // final ordering independently obey work_mem.
    let facet_input = collect_query_operator(
        engine,
        facet_columns.clone(),
        facet_rows,
        QueryOutputMode::SharedSpill,
    )?;
    let QueryRows::SharedSpill(facet_input) = facet_input.rows else {
        return Err(SQLError::Internal(
            "facet input collector returned in-memory rows".into(),
        ));
    };
    let group_keys = facet_columns
        .iter()
        .map(|column| (column.clone(), ScalarExpr::Column(column.clone())))
        .collect::<Vec<_>>();
    let work_mem = physical_work_mem_bytes(engine)?;
    let aggregate: Box<dyn PhysicalOperator + '_> = Box::new(HashAggregate::new_with_work_mem(
        Box::new(uqa_execution::SharedSpillScan::new(facet_input)),
        group_keys,
        vec![AggregateSpec {
            kind: AggregateKind::CountStar,
            arg: None,
            alias: "facet_count".into(),
            distinct: false,
        }],
        Vec::new(),
        work_mem,
    ));
    let sort_keys = facet_columns
        .iter()
        .map(|column| SortKey {
            expr: ScalarExpr::Column(column.clone()),
            descending: false,
            nulls_first: None,
        })
        .collect();
    let sorted: Box<dyn PhysicalOperator + '_> = Box::new(ExternalSort::new(
        aggregate,
        sort_keys,
        EngineExpressionEvaluator::shared(engine, execution.params, execution.ctes),
        None,
        work_mem,
    ));
    let mut columns = facet_columns;
    columns.push("facet_count".into());
    collect_query_operator(engine, columns, sorted, execution.output_mode)
}

/// When a projection list contains `ScalarExpr::Star`, replace the synthetic
/// `*` placeholder in the result column list with the source schema.
/// Empty result sets still report the correct column shape, matching
/// `PostgreSQL`'s behaviour of `SELECT * FROM empty_table`.
pub(super) fn expand_star_columns(
    columns: Vec<String>,
    projections: &[ProjectionPlan],
    engine: &Engine,
    table: Option<&str>,
) -> Result<Vec<String>, SQLError> {
    let has_star = projections
        .iter()
        .any(|p| matches!(p.expr, ScalarExpr::Star));
    if !has_star {
        return Ok(columns);
    }
    let schema_cols: Vec<String> = match table {
        Some(t) => {
            let cols = engine.try_table_columns(t).map_err(|error| {
                SQLError::Internal(format!("read table columns for `{t}`: {error}"))
            })?;
            if cols.is_empty() {
                engine
                    .foreign_table_columns(t)
                    .map_err(SQLError::Unsupported)?
            } else {
                cols
            }
        }
        None => Vec::new(),
    };
    if schema_cols.is_empty() {
        return Ok(columns);
    }
    let mut out: Vec<String> = Vec::with_capacity(columns.len() + schema_cols.len());
    for c in columns {
        if c == "*" {
            for sc in &schema_cols {
                if !out.iter().any(|x| x == sc) {
                    out.push(sc.clone());
                }
            }
        } else if !out.iter().any(|x| x == &c) {
            out.push(c);
        }
    }
    Ok(out)
}

fn order_by_references_field(stmt: &QueryBlockPlan) -> bool {
    stmt.order_by.iter().any(|o| match &o.expr {
        ScalarExpr::Column(name) => name != SCORE_COLUMN,
        _ => true,
    })
}

/// Collect bare column names referenced by an ORDER BY expression.
/// Returns `false` (ineligible) when the expression contains anything
/// that cannot be resolved against a stored document alone: function
/// calls, subqueries, window calls, `*`, or a bare literal (which
/// `PostgreSQL` would treat as an output-ordinal reference).
fn score_limited_text_filter(expr: Option<&ScalarExpr>) -> bool {
    let Some(ScalarExpr::Func { name, .. }) = expr else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "text_match" | "bayesian_match"
    )
}

fn score_order_top_k(
    stmt: &QueryBlockPlan,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<usize>, SQLError> {
    if stmt.distinct
        || !stmt.distinct_on.is_empty()
        || stmt.order_by.is_empty()
        || order_by_references_field(stmt)
        || stmt.order_by.iter().any(|order| !order.descending)
        || has_aggregate(engine, &stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        return Ok(None);
    }
    let Some(limit) =
        resolve_limit_offset_with_ctes(stmt.limit.as_ref(), engine, params, "LIMIT", ctes)?
    else {
        return Ok(None);
    };
    let offset =
        resolve_limit_offset_with_ctes(stmt.offset.as_ref(), engine, params, "OFFSET", ctes)?
            .unwrap_or(0);
    let requested = limit.checked_add(offset).ok_or_else(|| {
        SQLError::TypeMismatch("LIMIT plus OFFSET exceeds the u64 execution range".into())
    })?;
    let top_k = usize::try_from(requested).map_err(|_| {
        SQLError::TypeMismatch("LIMIT plus OFFSET exceeds the platform usize range".into())
    })?;
    Ok(Some(top_k))
}

fn explain_int_expr(expr: &ScalarExpr) -> String {
    match expr {
        ScalarExpr::Literal(Value::Int(n)) => n.to_string(),
        _ => "<expr>".to_string(),
    }
}

/// Evaluate a `LIMIT` / `OFFSET` expression to a non-negative `u64`.
/// Mirrors the canonical UQA implementation's `_extract_int_value` - accepts integer constants,
/// `$N` parameter references, and any expression that the row-evaluator
/// can fold to an integer at execute time. Returns `None` when the
/// clause was absent.
fn resolve_limit_offset_with_ctes(
    expr: Option<&ScalarExpr>,
    engine: &Engine,
    params: &[SQLParam],
    label: &str,
    ctes: &CteScope,
) -> Result<Option<u64>, SQLError> {
    let Some(expr) = expr else {
        return Ok(None);
    };
    let hook = ScopedEngineHook::new(engine, ctes);
    let ctx = PhysicalEvalContext::new(None, params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    let value = eval_physical_scalar(expr, &ctes.scalar_subqueries, &ctx)?;
    match value {
        Value::Null => Ok(None),
        Value::Int(n) if n >= 0 => Ok(Some(u64::try_from(n).map_err(|_| {
            SQLError::TypeMismatch(format!("{label} exceeds the u64 execution range"))
        })?)),
        Value::Int(_) => Err(SQLError::TypeMismatch(format!(
            "{label} must be non-negative"
        ))),
        Value::Float(value) => float_limit_offset(value, label).map(Some),
        other => Err(SQLError::TypeMismatch(format!(
            "{label} must be a non-negative integer, got {other:?}"
        ))),
    }
}

fn float_limit_offset(value: f64, label: &str) -> Result<u64, SQLError> {
    const U64_UPPER_EXCLUSIVE: f64 = 18_446_744_073_709_551_616.0;
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value >= U64_UPPER_EXCLUSIVE {
        return Err(SQLError::TypeMismatch(format!(
            "{label} must be a finite non-negative integer within the u64 execution range, got {value}"
        )));
    }
    Ok(value as u64)
}

pub(super) fn projection_columns(projections: &[ProjectionPlan]) -> Vec<String> {
    let mut out = Vec::with_capacity(projections.len());
    for proj in projections {
        let base = projection_label_at(proj);
        let mut label = base.clone();
        let mut suffix = 1usize;
        while out.iter().any(|existing: &String| existing == &label) {
            label = format!("{base}_{suffix}");
            suffix += 1;
        }
        out.push(label);
    }
    out
}

pub(super) fn build_projection_row_with_ctes(
    engine: &Engine,
    document: &Document,
    projections: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<ResultRow, SQLError> {
    use uqa_execution::physical::run_to_rows;
    use uqa_execution::scan::TableScan;
    use uqa_execution::{PhysicalOperator, Project};

    let source = document.clone();
    let columns = source.keys().cloned().collect();
    let scan: Box<dyn PhysicalOperator + '_> =
        Box::new(TableScan::from_rows(columns, vec![source]));
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    let mut project = Project::with_evaluator(scan, physical_projections(projections), evaluator);
    let (_, mut rows) = run_to_rows(&mut project).map_err(physical_exec_error)?;
    rows.pop().ok_or_else(|| {
        SQLError::Internal("physical projection produced no row for a single-row input".into())
    })
}

#[cfg(test)]
mod physical_failure_tests {
    use super::*;
    use uqa_execution::{Batch, ExecError, ExecResult, PhysicalOperator};
    use uqa_planner::UnifiedPlan;

    struct CloseOperator {
        schema: Vec<String>,
        close_error: Option<&'static str>,
    }

    impl PhysicalOperator for CloseOperator {
        fn schema(&self) -> &[String] {
            &self.schema
        }

        fn open(&mut self) -> ExecResult<()> {
            unreachable!("the cleanup helper must not open the operator")
        }

        fn next(&mut self) -> ExecResult<Option<Batch>> {
            unreachable!("the cleanup helper must not pull the operator")
        }

        fn close(&mut self) -> ExecResult<()> {
            match self.close_error {
                Some(message) => Err(ExecError::Other(message.into())),
                None => Ok(()),
            }
        }
    }

    #[test]
    fn physical_failure_preserves_the_original_error_when_close_succeeds() {
        let mut operator = CloseOperator {
            schema: Vec::new(),
            close_error: None,
        };
        let original = SQLError::TypeMismatch("primary".into());
        let original_message = original.to_string();
        let error =
            close_after_physical_failure(&mut operator, ExecError::SQL(original), "execution");
        assert_eq!(error.to_string(), original_message);
        assert!(matches!(error, SQLError::TypeMismatch(_)));
    }

    #[test]
    fn physical_failure_reports_both_execution_and_close_errors() {
        let mut operator = CloseOperator {
            schema: Vec::new(),
            close_error: Some("cleanup"),
        };
        let error = close_after_physical_failure(
            &mut operator,
            ExecError::Other("primary".into()),
            "spill buffering",
        );
        let message = error.to_string();
        assert!(message.contains("primary"));
        assert!(message.contains("spill buffering"));
        assert!(message.contains("cleanup"));
    }

    #[test]
    fn floating_limit_rejects_non_finite_fractional_and_out_of_range_values() {
        assert_eq!(float_limit_offset(42.0, "LIMIT").unwrap(), 42);
        for value in [
            f64::NAN,
            f64::INFINITY,
            -1.0,
            1.5,
            18_446_744_073_709_551_616.0,
        ] {
            assert!(float_limit_offset(value, "LIMIT").is_err(), "{value}");
        }
    }

    #[test]
    fn scalar_subquery_scope_restores_the_parent_arena_after_unwind() {
        let statement = uqa_sql::compile("SELECT 1")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
            panic!("SELECT must lower to a query plan");
        };
        let query = *query;
        let mut scope = CteScope::new();
        scope.scalar_subqueries.push(query.clone());

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = scope.enter_scalar_subqueries(&[query.clone(), query]);
            panic!("exercise scalar-subquery scope cleanup");
        }));

        assert!(unwind.is_err());
        assert_eq!(scope.scalar_subqueries.len(), 1);
    }
}
