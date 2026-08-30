//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection, ordering, filtering, and relational physical-operator assembly.

use super::{
    build_set_projection, collect_exists_key_operator, collect_query_operator,
    column_prune_for_stmt, expression_may_return_set, final_filter_after_qualifier_pushdown,
    prepare_aggregate_output_projection, prepare_correlated_exists_predicate,
    prepare_distinct_grouping_sets, prepare_group_set_projection, prepare_window_plan,
    projection_columns, projections_may_return_set, qualifier_filters_for_stmt,
    resolve_fetch_limit_with_ties, resolve_limit_offset_with_ctes, should_defer_distinct_limit,
    Arc, ComputePlan, CteScope, Engine, EngineExpressionEvaluator, OutputColumnMapping,
    PhysicalAggregateExecutor, PhysicalProjection, PhysicalWindowExecutor, ProjectionPlan,
    ProjectionTarget, QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError, SQLParam, ScalarExpr,
    ScopedEngineHook, SharedExpressionEvaluator, Value,
};
use uqa_execution::ScalarFrameBound;

#[derive(Default)]
pub(in crate::sql) struct RelationalResjunk {
    distinct_on: Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
    order_by: Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
}

impl RelationalResjunk {
    pub(in crate::sql) fn columns(&self) -> Vec<uqa_sql::ast::InternalColumnRef> {
        self.distinct_on
            .iter()
            .chain(&self.order_by)
            .map(|(_, column)| *column)
            .collect()
    }

    pub(in crate::sql) fn is_empty(&self) -> bool {
        self.distinct_on.is_empty() && self.order_by.is_empty()
    }
}

fn projection_set_batch_size(statement: &QueryBlockPlan, ctes: &CteScope) -> usize {
    if ctes.streams_command_progress()
        || statement.limit.is_some()
            && statement.order_by.is_empty()
            && !statement.distinct
            && statement.distinct_on.is_empty()
    {
        1
    } else {
        uqa_execution::DEFAULT_BATCH_SIZE
    }
}

mod row_at_a_time;
use row_at_a_time::RowAtATime;
mod ordering;
use ordering::{
    append_row_at_time_projection, attach_final_projection_order, distinct_output_target_position,
    one_based_output_position, output_position_error, output_target_position,
    prior_distinct_key_index, split_locking_order_projections, validate_distinct_ordering,
};
mod limit;
pub(in crate::sql) use limit::attach_order_limit;
use limit::resolved_sort_keys;

pub(in crate::sql) fn expand_from_star_columns(
    columns: Vec<String>,
    projections: &[ProjectionPlan],
    source_schema: &uqa_execution::RowSchema,
) -> Result<Vec<String>, SQLError> {
    let mut output = Vec::new();
    for (position, projection) in projections.iter().enumerate() {
        match &projection.expr {
            ScalarExpr::Star => {
                output.extend(
                    source_schema
                        .columns()
                        .iter()
                        .enumerate()
                        .filter(|(position, _)| {
                            visible_projection_source_position(source_schema, *position)
                        })
                        .map(|(source_position, column)| {
                            source_schema
                                .public_name(source_position)
                                .unwrap_or(column)
                                .to_string()
                        }),
                );
            }
            ScalarExpr::QualifiedStar(qualifier) => {
                let qualified_columns = source_schema
                    .qualified_star_position_layout(qualifier)
                    .into_iter()
                    .filter(|(_, logical, _, _)| {
                        logical.is_none_or(|position| {
                            visible_projection_source_position(source_schema, position)
                        })
                    })
                    .map(|(column, _, _, _)| column)
                    .collect::<Vec<_>>();
                if qualified_columns.is_empty() {
                    return Err(SQLError::UnknownTable(qualifier.clone()));
                }
                output.extend(qualified_columns);
            }
            _ => output.push(columns[position].clone()),
        }
    }
    Ok(output)
}

/// Output column names of a user-defined routine used as a FROM
/// source: OUT / INOUT / `RETURNS TABLE` parameter names. `None` when
/// the name is not a user routine or its result is a single unnamed
/// column (which keeps the function-name default).
pub(in crate::sql) fn user_function_output_columns(
    engine: &Engine,
    name: &str,
) -> Option<Vec<String>> {
    let overloads = engine.lookup_sql_functions(name)?;
    for function in &overloads {
        if let Some(columns) = crate::sql::from_rows::user_function_output_columns_for(function) {
            return Some(columns);
        }
    }
    None
}

pub(in crate::sql) fn physical_exec_error(error: uqa_execution::ExecError) -> SQLError {
    match error {
        uqa_execution::ExecError::SQL(error) => error,
        uqa_execution::ExecError::Other(message) => SQLError::Internal(message),
    }
}

pub(in crate::sql) fn close_after_physical_failure(
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

pub(in crate::sql) fn physical_work_mem_bytes(engine: &Engine) -> Result<usize, SQLError> {
    engine.work_mem_bytes()
}

pub(in crate::sql) fn physical_projections(
    projections: &[ProjectionPlan],
) -> Vec<PhysicalProjection> {
    let labels = projection_columns(projections);
    projections
        .iter()
        .enumerate()
        .map(|(index, projection)| {
            (
                ProjectionTarget::Column(labels[index].clone()),
                projection.expr.clone(),
            )
        })
        .collect()
}

fn projection_target_expression(target: &ProjectionTarget) -> ScalarExpr {
    match target {
        ProjectionTarget::Column(column) => ScalarExpr::Column(column.clone()),
        ProjectionTarget::Internal(column) => ScalarExpr::InternalColumn(*column),
    }
}

fn bound_projection_expression(schema: &uqa_execution::RowSchema, position: usize) -> ScalarExpr {
    let Some(identity) = schema.identity(position) else {
        return ScalarExpr::Position(position);
    };
    if let Some(qualifier) = identity.qualifier() {
        if schema.qualified_position(qualifier, identity.column()) == Some(position) {
            return ScalarExpr::qualified_column(qualifier, identity.column());
        }
    } else if schema.unqualified_position(identity.column()) == Some(position) {
        return ScalarExpr::Column(identity.column().to_string());
    }
    ScalarExpr::Position(position)
}

pub(in crate::sql) fn expand_bound_projection_stars(
    projections: &[ProjectionPlan],
    schema: &uqa_execution::RowSchema,
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let mut expanded = Vec::new();
    for projection in projections {
        match &projection.expr {
            ScalarExpr::Star => {
                for (position, column) in schema.columns().iter().enumerate() {
                    if !visible_projection_source_position(schema, position) {
                        continue;
                    }
                    expanded.push(ProjectionPlan {
                        expr: bound_projection_expression(schema, position),
                        alias: Some(schema.public_name(position).unwrap_or(column).to_string()),
                    });
                }
            }
            ScalarExpr::QualifiedStar(qualifier) => {
                let layout = schema.qualified_star_position_layout(qualifier);
                if layout.is_empty() {
                    return Err(SQLError::UnknownTable(qualifier.clone()));
                }
                for (column, logical, _, _) in layout {
                    if logical.is_some_and(|position| {
                        !visible_projection_source_position(schema, position)
                    }) {
                        continue;
                    }
                    expanded.push(ProjectionPlan {
                        expr: logical.map_or_else(
                            || ScalarExpr::qualified_column(qualifier, &column),
                            |position| bound_projection_expression(schema, position),
                        ),
                        alias: Some(column),
                    });
                }
            }
            _ => expanded.push(projection.clone()),
        }
    }
    Ok(expanded)
}

pub(in crate::sql) fn visible_projection_source_position(
    schema: &uqa_execution::RowSchema,
    position: usize,
) -> bool {
    schema.wildcard_position_visible(position)
}

/// Build collision-free physical target columns for a plain SELECT whose
/// ORDER BY must be able to see both input columns and SELECT-list aliases.
///
/// Public aliases cannot safely be appended directly: `SELECT x + 1 AS x
/// ... ORDER BY x` must order by the output alias, while `ORDER BY x + 1`
/// still resolves `x` against the input namespace. Each non-star target is
/// therefore computed once under an opaque internal attribute and assigned
/// its public label only after Sort/Limit has consumed it.
pub(in crate::sql) fn order_projection(
    projections: &[ProjectionPlan],
    input_schema: &uqa_execution::RowSchema,
) -> Result<(Vec<PhysicalProjection>, Vec<OutputColumnMapping>), SQLError> {
    let labels = projection_columns(projections);
    let mut physical = Vec::new();
    let mut output = Vec::new();
    let internal_relation = uqa_sql::ast::InternalRelationId::allocate();
    let mut next_internal_attribute = 0usize;

    for (index, projection) in projections.iter().enumerate() {
        if matches!(projection.expr, ScalarExpr::Star) {
            for (position, column) in input_schema.columns().iter().enumerate() {
                if visible_projection_source_position(input_schema, position) {
                    output.push((
                        input_schema
                            .public_name(position)
                            .unwrap_or(column)
                            .to_string(),
                        ScalarExpr::Position(position),
                    ));
                }
            }
            continue;
        }
        if let ScalarExpr::QualifiedStar(qualifier) = &projection.expr {
            let columns = input_schema.qualified_star_position_layout(qualifier);
            if columns.is_empty() {
                return Err(SQLError::UnknownTable(qualifier.clone()));
            }
            for (column, logical, _, _) in columns {
                if logical.is_some_and(|position| {
                    !visible_projection_source_position(input_schema, position)
                }) {
                    continue;
                }
                if let Some(logical) = logical {
                    output.push((column, ScalarExpr::Position(logical)));
                    continue;
                }
                let internal = internal_relation.column(next_internal_attribute);
                next_internal_attribute += 1;
                physical.push((
                    ProjectionTarget::Internal(internal),
                    ScalarExpr::qualified_column(qualifier, &column),
                ));
                output.push((column, ScalarExpr::InternalColumn(internal)));
            }
            continue;
        }

        // A bare source column with the same public label does not need a
        // computed shadow slot. Renamed or repeated columns still need their
        // own slot: ColumnSelection consumes each physical input once, so two
        // output mappings cannot safely share the same source column.
        if let ScalarExpr::Column(source) = &projection.expr {
            if &labels[index] == source {
                if let Some(position) = input_schema.unqualified_position(source) {
                    output.push((labels[index].clone(), ScalarExpr::Position(position)));
                    continue;
                }
            }
        }
        if let ScalarExpr::QualifiedColumn { qualifier, column } = &projection.expr {
            if &labels[index] == column {
                if let Some(position) = input_schema.qualified_position(qualifier, column) {
                    output.push((labels[index].clone(), ScalarExpr::Position(position)));
                    continue;
                }
            }
        }

        let internal = internal_relation.column(next_internal_attribute);
        next_internal_attribute += 1;
        physical.push((
            ProjectionTarget::Internal(internal),
            projection.expr.clone(),
        ));
        output.push((labels[index].clone(), ScalarExpr::InternalColumn(internal)));
    }
    Ok((physical, output))
}

pub(in crate::sql) fn identity_order_columns(columns: &[String]) -> Vec<OutputColumnMapping> {
    columns
        .iter()
        .map(|column| (column.clone(), ScalarExpr::Column(column.clone())))
        .collect()
}

pub(in crate::sql) fn output_selection_positions(
    schema: &uqa_execution::RowSchema,
    output: Vec<OutputColumnMapping>,
) -> Result<Vec<(String, usize)>, SQLError> {
    output
        .into_iter()
        .map(|(label, source)| {
            let position = match source {
                ScalarExpr::Position(position) if position < schema.len() => {
                    schema.physical_slot(position)
                }
                ScalarExpr::Column(column) => schema
                    .position(&column)
                    .and_then(|logical| schema.physical_slot(logical)),
                ScalarExpr::InternalColumn(column) => schema.internal_slot(column),
                _ => None,
            }
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "bound output column `{label}` is unavailable in the physical row"
                ))
            })?;
            Ok((label, position))
        })
        .collect()
}

pub(in crate::sql) fn resolve_order_expression(
    expression: &ScalarExpr,
    output_columns: &[OutputColumnMapping],
) -> Result<ScalarExpr, SQLError> {
    match expression {
        ScalarExpr::Literal(Value::Int(position)) => {
            let index = usize::try_from(*position)
                .ok()
                .and_then(|position| position.checked_sub(1))
                .filter(|index| *index < output_columns.len())
                .ok_or_else(|| output_position_error("ORDER BY", *position))?;
            Ok(output_columns[index].1.clone())
        }
        // SQL output aliases are visible only as a bare ORDER BY name. A
        // name embedded in a larger expression continues to bind to the
        // input row, which is why this rewrite deliberately is not recursive.
        ScalarExpr::Column(name) => {
            let mut matches = output_columns.iter().filter(|(output, _)| output == name);
            let Some((_, physical)) = matches.next() else {
                return Ok(expression.clone());
            };
            if matches.next().is_some() {
                return Err(SQLError::AmbiguousColumn(name.clone()));
            }
            Ok(physical.clone())
        }
        _ => Ok(expression.clone()),
    }
}

type PreparedOrderSetProjections = (
    Option<QueryBlockPlan>,
    Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
);

fn prepare_order_set_projections(
    engine: &Engine,
    type_resolver: &dyn uqa_execution::FunctionTypeResolver,
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    projections: &mut Vec<PhysicalProjection>,
    schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Result<PreparedOrderSetProjections, SQLError> {
    let mut prepared: Option<QueryBlockPlan> = None;
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let mut resjunk = Vec::new();
    for (index, order) in statement.order_by.iter().enumerate() {
        if let Some(target) = output_target_position(statement, &order.expr, output_columns)? {
            if !target.direct {
                prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                    one_based_output_position(target.position)?;
            }
            continue;
        }
        let expression = resolve_order_expression(&order.expr, output_columns)?;
        if statement.with_ties {
            let column = relation.column(resjunk.len());
            projections.push((ProjectionTarget::Internal(column), expression));
            resjunk.push((index, column));
            prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                ScalarExpr::InternalColumn(column);
            continue;
        }
        if let Some((target, _)) = projections
            .iter()
            .find(|(_, projected)| crate::sql::aggregates::exprs_match(projected, &expression))
        {
            prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                projection_target_expression(target);
        } else if expression_may_return_set(engine, type_resolver, &expression, schema, params)? {
            let column = relation.column(resjunk.len());
            projections.push((ProjectionTarget::Internal(column), expression));
            resjunk.push((index, column));
            prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                ScalarExpr::InternalColumn(column);
        }
    }
    Ok((prepared, resjunk))
}

fn append_distinct_set_projections(
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    projections: &mut Vec<PhysicalProjection>,
) -> Result<Vec<(usize, uqa_sql::ast::InternalColumnRef)>, SQLError> {
    let mut columns = Vec::new();
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    for (index, expression) in statement.distinct_on.iter().enumerate() {
        if prior_distinct_key_index(statement, index, expression, output_columns)?.is_some() {
            continue;
        }
        if distinct_output_target_position(statement, expression, output_columns)?.is_some() {
            continue;
        }
        let expression = resolve_order_expression(expression, output_columns)?;
        let column = relation.column(columns.len());
        projections.push((ProjectionTarget::Internal(column), expression));
        columns.push((index, column));
    }
    Ok(columns)
}

struct AggregateKeyStatement {
    statement: QueryBlockPlan,
    targets: Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
    distinct_on: Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
    order_by: Vec<(usize, uqa_sql::ast::InternalColumnRef)>,
}

fn prepare_aggregate_key_statement(
    statement: &QueryBlockPlan,
) -> Result<Option<AggregateKeyStatement>, SQLError> {
    let output = identity_order_columns(&projection_columns(&statement.projections));
    let mut prepared: Option<QueryBlockPlan> = None;
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let mut targets = Vec::new();
    let mut distinct_on = Vec::new();
    let mut order_by = Vec::new();
    for (index, expression) in statement.distinct_on.iter().enumerate() {
        if prior_distinct_key_index(statement, index, expression, &output)?.is_some() {
            continue;
        }
        if distinct_output_target_position(statement, expression, &output)?.is_some() {
            continue;
        }
        let expression = resolve_order_expression(expression, &output)?;
        let prepared = prepared.get_or_insert_with(|| statement.clone());
        let position = prepared.projections.len();
        let column = relation.column(targets.len());
        prepared.projections.push(ProjectionPlan {
            expr: expression,
            alias: None,
        });
        targets.push((position, column));
        distinct_on.push((index, column));
    }
    for (index, order) in statement.order_by.iter().enumerate() {
        if let Some(target) = output_target_position(statement, &order.expr, &output)? {
            if !target.direct {
                prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
                    one_based_output_position(target.position)?;
            }
            continue;
        }
        let expression = resolve_order_expression(&order.expr, &output)?;
        let current = prepared.as_ref().unwrap_or(statement);
        let existing = current
            .projections
            .iter()
            .enumerate()
            .find(|(_, projection)| {
                crate::sql::aggregates::exprs_match(&projection.expr, &expression)
            })
            .map(|(position, _)| position);
        let prepared = prepared.get_or_insert_with(|| statement.clone());
        let key = if let Some(position) = existing {
            if let Some((_, column)) = targets
                .iter()
                .find(|(target_position, _)| *target_position == position)
            {
                order_by.push((index, *column));
                ScalarExpr::InternalColumn(*column)
            } else {
                ScalarExpr::Position(position)
            }
        } else {
            let position = prepared.projections.len();
            let column = relation.column(targets.len());
            prepared.projections.push(ProjectionPlan {
                expr: expression,
                alias: None,
            });
            targets.push((position, column));
            order_by.push((index, column));
            ScalarExpr::InternalColumn(column)
        };
        prepared.order_by[index].expr = key;
    }
    Ok(prepared.map(|statement| AggregateKeyStatement {
        statement,
        targets,
        distinct_on,
        order_by,
    }))
}

pub(in crate::sql) fn execute_filter_physical_rows(
    engine: &Engine,
    schema: uqa_execution::RowSchema,
    rows: Vec<uqa_execution::PhysicalRow>,
    predicate: ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<uqa_execution::OwnedPhysicalRow>, SQLError> {
    use uqa_execution::scan::TableScan;
    use uqa_execution::{physical::run_to_batches, Filter, PhysicalOperator};

    let scan: Box<dyn PhysicalOperator + '_> =
        Box::new(TableScan::from_physical_rows(schema, rows));
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    let mut filter = Filter::with_evaluator(scan, predicate, evaluator);
    Ok(run_to_batches(&mut filter)
        .map_err(physical_exec_error)?
        .into_iter()
        .flat_map(uqa_execution::Batch::into_owned_rows)
        .collect())
}

/// Rebuild the plan below one `LockRows` boundary for a tuple-local recheck. The construction replays the same source, filter, and scalar target projection below the original `LockRows` boundary, with the recheck pins active in `ctes` so every lock-target base scan emits only the candidate's tuples while unmarked relations rescan under the statement snapshot. Sorting, locking, and `LIMIT` never run here: the candidate keeps its original position in the outer stream.
#[cold]
#[inline(never)]
pub(in crate::sql) fn build_row_lock_recheck_operator<'a>(
    engine: &'a Engine,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    _ordered: bool,
    projections: &[PhysicalProjection],
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::Project;

    let Some(from) = statement.from.as_ref() else {
        return Err(SQLError::Internal(
            "row-lock recheck requires a FROM clause".into(),
        ));
    };
    let column_prune = column_prune_for_stmt(engine, statement, from);
    let qualifier_filters = qualifier_filters_for_stmt(engine, statement, from);
    let source_row_locks = crate::sql::select::resolve_row_locks(
        engine,
        from,
        &statement.locking,
        statement.r#where.as_ref(),
        params,
        ctes,
    )?;
    let mut operator = {
        let mut scoped_ctes = ctes.enter_source_row_locks(source_row_locks);
        crate::sql::from_rows::build_join_operator_with_recheck_pins(
            engine,
            from,
            params,
            &mut scoped_ctes,
            column_prune.as_ref(),
            qualifier_filters.as_ref(),
        )?
    };
    if let Some(outer_row) = ctes.row_lock_outer_row() {
        operator = Box::new(uqa_execution::ScopeOverlay::new(
            operator,
            outer_row.clone(),
        ));
    }
    let predicate =
        final_filter_after_qualifier_pushdown(engine, statement, from, qualifier_filters.as_ref());
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    if let Some(predicate) = predicate {
        operator = match prepare_correlated_exists_predicate(engine, &predicate, params, ctes)? {
            Some(prepared) => Box::new(uqa_execution::Filter::with_row_predicate(
                operator, prepared,
            )),
            None => Box::new(uqa_execution::Filter::with_evaluator(
                operator,
                predicate,
                Arc::clone(&evaluator),
            )),
        };
    }
    operator = if projections.is_empty() {
        operator
    } else {
        Box::new(Project::appending_target_evaluator(
            operator,
            projections.to_vec(),
            evaluator,
        )) as Box<dyn uqa_execution::PhysicalOperator + 'a>
    };
    Ok(operator)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::sql) fn execute_query_block_operator_output<'a>(
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
    let type_resolver = ScopedEngineHook::new(engine, ctes);
    if matches!(&output_mode, QueryOutputMode::ExistsKeySet)
        && matches!(statement.compute, ComputePlan::Project)
        && statement.order_by.is_empty()
        && statement.limit.is_none()
        && statement.offset.is_none()
        && !statement.distinct
        && statement.distinct_on.is_empty()
        && !projections_may_return_set(
            engine,
            &type_resolver,
            &physical_projections(&statement.projections),
            operator.row_schema(),
            params,
        )?
        && matches!(original.compute, ComputePlan::Project)
        && original.order_by.is_empty()
        && original.limit.is_none()
        && original.offset.is_none()
        && !original.distinct
        && original.distinct_on.is_empty()
    {
        let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
        let operator =
            attach_relational_filter(engine, operator, predicate, params, ctes, &evaluator)?;
        return collect_exists_key_operator(columns, operator, &statement.projections, evaluator);
    }
    let (operator, resjunk) =
        build_relational_operator(engine, operator, predicate, statement, params, ctes)?;
    finish_query_block_operator_output(
        engine,
        operator,
        original,
        params,
        ctes,
        columns,
        output_mode,
        resjunk,
    )
}

fn attach_relational_filter<'a>(
    engine: &'a Engine,
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    params: &'a [SQLParam],
    ctes: &CteScope,
    evaluator: &SharedExpressionEvaluator<'a>,
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::Filter;

    if let Some(predicate) = predicate {
        operator = match prepare_correlated_exists_predicate(engine, &predicate, params, ctes)? {
            Some(prepared) => Box::new(Filter::with_row_predicate(operator, prepared)),
            None => Box::new(Filter::with_evaluator(
                operator,
                predicate,
                Arc::clone(evaluator),
            )),
        };
    }
    Ok(operator)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::sql) fn finish_query_block_operator_output<'a>(
    engine: &'a Engine,
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    original: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope,
    columns: Vec<String>,
    output_mode: QueryOutputMode,
    resjunk: RelationalResjunk,
) -> Result<QueryOutput, SQLError> {
    use uqa_execution::{ColumnSelection, Distinct, Limit};

    if original.distinct {
        let work_mem_bytes = physical_work_mem_bytes(engine)?;
        operator = if original.distinct_on.is_empty() {
            Box::new(Distinct::all_with_work_mem(operator, work_mem_bytes))
        } else {
            let output = identity_order_columns(&columns);
            let mut distinct_on: Vec<ScalarExpr> = Vec::with_capacity(original.distinct_on.len());
            for (index, expression) in original.distinct_on.iter().enumerate() {
                let key = if let Some((_, column)) = resjunk
                    .distinct_on
                    .iter()
                    .find(|(key_index, _)| *key_index == index)
                {
                    ScalarExpr::InternalColumn(*column)
                } else if let Some(prior) =
                    prior_distinct_key_index(original, index, expression, &output)?
                {
                    distinct_on[prior].clone()
                } else if let Some(target) =
                    distinct_output_target_position(original, expression, &output)?
                {
                    ScalarExpr::Position(target.position)
                } else {
                    expression.clone()
                };
                distinct_on.push(key);
            }
            Box::new(Distinct::on_with_work_mem(
                operator,
                distinct_on,
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
        if original.with_ties {
            let limit =
                resolve_fetch_limit_with_ties(original.limit.as_ref(), engine, params, ctes)?;
            let output = identity_order_columns(&columns);
            let mut ordering = original.clone();
            for (index, column) in &resjunk.order_by {
                ordering.order_by[*index].expr = ScalarExpr::InternalColumn(*column);
            }
            let keys = resolved_sort_keys(&ordering, &output, Some(operator.row_schema()))?;
            operator = Box::new(Limit::with_ties(
                operator,
                offset.unwrap_or(0),
                limit,
                keys,
                EngineExpressionEvaluator::shared(engine, params, ctes),
            ));
        } else {
            let limit = resolve_limit_offset_with_ctes(
                original.limit.as_ref(),
                engine,
                params,
                "LIMIT",
                ctes,
            )?;
            operator = Box::new(Limit::new(operator, offset.unwrap_or(0), limit));
        }
    }
    let resjunk_columns = resjunk.columns();
    if !resjunk_columns.is_empty() {
        operator = Box::new(ColumnSelection::dropping_internal_attributes(
            operator,
            &resjunk_columns,
        ));
    }
    if operator.schema().len() < columns.len() {
        return Err(SQLError::Internal(format!(
            "query output schema width {} is smaller than public output width {}",
            operator.schema().len(),
            columns.len()
        )));
    }
    if operator.schema()[..columns.len()] != columns {
        let mut positions = columns
            .iter()
            .cloned()
            .enumerate()
            .map(|(position, output)| (output, position))
            .collect::<Vec<_>>();
        positions.extend(
            operator.schema()[columns.len()..]
                .iter()
                .cloned()
                .enumerate()
                .map(|(offset, output)| (output, columns.len() + offset)),
        );
        operator = Box::new(ColumnSelection::with_positions(operator, positions));
    }
    collect_query_operator(engine, columns, operator, output_mode)
}

pub(in crate::sql) fn build_relational_operator<'a>(
    engine: &'a Engine,
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    predicate: Option<ScalarExpr>,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &CteScope,
) -> Result<
    (
        Box<dyn uqa_execution::PhysicalOperator + 'a>,
        RelationalResjunk,
    ),
    SQLError,
> {
    use uqa_execution::{ColumnSelection, HashAggregate, Project, Window};

    let type_resolver = ScopedEngineHook::new(engine, ctes);
    let mut resjunk = RelationalResjunk::default();
    let expanded_statement = statement
        .projections
        .iter()
        .any(|projection| {
            matches!(
                projection.expr,
                ScalarExpr::Star | ScalarExpr::QualifiedStar(_)
            )
        })
        .then(|| {
            let mut expanded = statement.clone();
            expanded.projections =
                expand_bound_projection_stars(&statement.projections, operator.row_schema())?;
            Ok::<_, SQLError>(expanded)
        })
        .transpose()?;
    let statement = expanded_statement.as_ref().unwrap_or(statement);
    validate_distinct_ordering(statement)?;
    if ctes.streams_command_progress() {
        operator = Box::new(RowAtATime::new(operator));
    }
    if let Some(clause) = statement.locking.first() {
        let projections = physical_projections(&statement.projections);
        let (_, output) = order_projection(&statement.projections, operator.row_schema())?;
        let order_returns_set = statement.order_by.iter().try_fold(false, |found, order| {
            if found {
                return Ok(true);
            }
            let expression = resolve_order_expression(&order.expr, &output)?;
            expression_may_return_set(
                engine,
                &type_resolver,
                &expression,
                operator.row_schema(),
                params,
            )
        })?;
        if projections_may_return_set(
            engine,
            &type_resolver,
            &projections,
            operator.row_schema(),
            params,
        )? || order_returns_set
        {
            return Err(SQLError::Unsupported(format!(
                "{} is not allowed with set-returning functions in the target list",
                clause.strength.sql_name()
            )));
        }
    }
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    operator = attach_relational_filter(engine, operator, predicate, params, ctes, &evaluator)?;
    let distinct_group_statement = if matches!(statement.compute, ComputePlan::Aggregate) {
        prepare_distinct_grouping_sets(engine, statement, operator.row_schema(), params)?
    } else {
        None
    };
    let statement = distinct_group_statement.as_ref().unwrap_or(statement);
    let mut group_statement = None;
    if matches!(statement.compute, ComputePlan::Aggregate) {
        if let Some(plan) = prepare_group_set_projection(
            engine,
            &type_resolver,
            statement,
            operator.row_schema(),
            params,
        )? {
            operator = build_set_projection(
                operator,
                engine,
                params,
                ctes,
                Arc::clone(&evaluator),
                plan.projections,
                true,
                uqa_execution::DEFAULT_BATCH_SIZE,
            )?;
            group_statement = Some(plan.statement);
        }
    }
    let statement = group_statement.as_ref().unwrap_or(statement);

    match statement.compute {
        ComputePlan::Project => {
            if statement.order_by.is_empty() {
                let mut projections = physical_projections(&statement.projections);
                let distinct_output =
                    identity_order_columns(&projection_columns(&statement.projections));
                resjunk.distinct_on.extend(append_distinct_set_projections(
                    statement,
                    &distinct_output,
                    &mut projections,
                )?);
                if !statement.locking.is_empty() {
                    let (physical, output) =
                        order_projection(&statement.projections, operator.row_schema())?;
                    let recheck_projections = physical.clone();
                    operator =
                        append_row_at_time_projection(operator, physical, Arc::clone(&evaluator));
                    operator = attach_order_limit(
                        operator,
                        statement,
                        &[],
                        engine,
                        params,
                        ctes,
                        Arc::clone(&evaluator),
                        Some(crate::sql::select::LockRowsRecheckSource::with_projections(
                            statement,
                            ctes,
                            false,
                            recheck_projections,
                        )),
                    )?;
                    let output = output_selection_positions(operator.row_schema(), output)?;
                    operator = Box::new(ColumnSelection::with_physical_positions(operator, output));
                } else if projections_may_return_set(
                    engine,
                    &type_resolver,
                    &projections,
                    operator.row_schema(),
                    params,
                )? {
                    // PostgreSQL evaluates set-returning target expressions above LockRows, so a rechecked tuple re-expands its rows from the substituted base tuple. Locks therefore attach below the set projection, and LIMIT is applied to the expanded output above.
                    operator = crate::sql::select::attach_lock_rows(
                        engine,
                        operator,
                        statement,
                        params,
                        ctes,
                        None,
                        Some(crate::sql::select::LockRowsRecheckSource::new(
                            statement, ctes, false,
                        )),
                    )?;
                    operator = build_set_projection(
                        operator,
                        engine,
                        params,
                        ctes,
                        Arc::clone(&evaluator),
                        projections,
                        false,
                        projection_set_batch_size(statement, ctes),
                    )?;
                    let unlocked_statement = (!statement.locking.is_empty()).then(|| {
                        let mut unlocked = statement.clone();
                        unlocked.locking.clear();
                        unlocked
                    });
                    operator = attach_order_limit(
                        operator,
                        unlocked_statement.as_ref().unwrap_or(statement),
                        &[],
                        engine,
                        params,
                        ctes,
                        evaluator,
                        None,
                    )?;
                } else {
                    // Without ordering or row expansion, Limit may stop the
                    // child before unused target expressions are evaluated.
                    operator = attach_order_limit(
                        operator,
                        statement,
                        &[],
                        engine,
                        params,
                        ctes,
                        Arc::clone(&evaluator),
                        Some(crate::sql::select::LockRowsRecheckSource::new(
                            statement, ctes, false,
                        )),
                    )?;
                    operator = Box::new(Project::with_target_evaluator(
                        operator,
                        projections,
                        evaluator,
                    ));
                }
            } else {
                let (mut physical, output) =
                    order_projection(&statement.projections, operator.row_schema())?;
                // SQL ordinals and aliases are resolved only against the
                // visible SELECT list. Score provenance is carried through
                // the final column selection for parent query blocks, but it
                // is not itself a selectable output position.
                let order_output = output.clone();
                let distinct_columns =
                    append_distinct_set_projections(statement, &order_output, &mut physical)?;
                resjunk.distinct_on.extend(distinct_columns);
                let (order_statement, order_columns) = prepare_order_set_projections(
                    engine,
                    &type_resolver,
                    statement,
                    &order_output,
                    &mut physical,
                    operator.row_schema(),
                    params,
                )?;
                resjunk.order_by.extend(order_columns);
                if statement.locking.is_empty() && !ctes.streams_command_progress() {
                    operator = if projections_may_return_set(
                        engine,
                        &type_resolver,
                        &physical,
                        operator.row_schema(),
                        params,
                    )? {
                        build_set_projection(
                            operator,
                            engine,
                            params,
                            ctes,
                            Arc::clone(&evaluator),
                            physical,
                            true,
                            uqa_execution::DEFAULT_BATCH_SIZE,
                        )?
                    } else {
                        Box::new(Project::appending_target_evaluator(
                            operator,
                            physical,
                            Arc::clone(&evaluator),
                        ))
                    };
                    operator = attach_order_limit(
                        operator,
                        order_statement.as_ref().unwrap_or(statement),
                        &order_output,
                        engine,
                        params,
                        ctes,
                        evaluator,
                        None,
                    )?;
                } else if statement.locking.is_empty() {
                    let effective_order_statement = order_statement.as_ref().unwrap_or(statement);
                    let (sort_statement, before_sort, after_sort) =
                        split_locking_order_projections(
                            effective_order_statement,
                            &order_output,
                            physical,
                        )?;
                    if !before_sort.is_empty() {
                        operator = Box::new(Project::appending_target_evaluator(
                            operator,
                            before_sort,
                            Arc::clone(&evaluator),
                        ));
                    }
                    if projections_may_return_set(
                        engine,
                        &type_resolver,
                        &after_sort,
                        operator.row_schema(),
                        params,
                    )? {
                        operator = build_set_projection(
                            operator,
                            engine,
                            params,
                            ctes,
                            Arc::clone(&evaluator),
                            after_sort,
                            true,
                            projection_set_batch_size(statement, ctes),
                        )?;
                        operator = attach_order_limit(
                            operator,
                            &sort_statement,
                            &order_output,
                            engine,
                            params,
                            ctes,
                            Arc::clone(&evaluator),
                            None,
                        )?;
                    } else {
                        operator = attach_order_limit(
                            operator,
                            &sort_statement,
                            &order_output,
                            engine,
                            params,
                            ctes,
                            Arc::clone(&evaluator),
                            None,
                        )?;
                        operator = append_row_at_time_projection(
                            operator,
                            after_sort,
                            Arc::clone(&evaluator),
                        );
                    }
                } else {
                    let effective_order_statement = order_statement.as_ref().unwrap_or(statement);
                    let recheck_projections = physical.clone();
                    let (mut sort_statement, before_sort, after_sort) =
                        split_locking_order_projections(
                            effective_order_statement,
                            &order_output,
                            physical,
                        )?;
                    if !before_sort.is_empty() {
                        operator = Box::new(Project::appending_target_evaluator(
                            operator,
                            before_sort,
                            Arc::clone(&evaluator),
                        ));
                    }
                    sort_statement.locking.clear();
                    sort_statement.limit = None;
                    sort_statement.with_ties = false;
                    sort_statement.offset = None;
                    operator = attach_order_limit(
                        operator,
                        &sort_statement,
                        &order_output,
                        engine,
                        params,
                        ctes,
                        Arc::clone(&evaluator),
                        None,
                    )?;
                    operator =
                        append_row_at_time_projection(operator, after_sort, Arc::clone(&evaluator));
                    let mut lock_statement = statement.clone();
                    lock_statement.order_by = sort_statement.order_by;
                    operator = attach_order_limit(
                        operator,
                        &lock_statement,
                        &order_output,
                        engine,
                        params,
                        ctes,
                        Arc::clone(&evaluator),
                        Some(crate::sql::select::LockRowsRecheckSource::with_projections(
                            statement,
                            ctes,
                            true,
                            recheck_projections,
                        )),
                    )?;
                }
                let output = output_selection_positions(operator.row_schema(), output)?;
                operator = Box::new(ColumnSelection::with_physical_positions(operator, output));
            }
        }
        ComputePlan::Aggregate => {
            let public_projection_count = statement.projections.len();
            let key_statement = prepare_aggregate_key_statement(statement)?;
            if let Some(keys) = &key_statement {
                resjunk.distinct_on.extend(keys.distinct_on.iter().copied());
                resjunk.order_by.extend(keys.order_by.iter().copied());
            }
            let internal_targets = key_statement
                .as_ref()
                .map_or(&[][..], |keys| keys.targets.as_slice());
            let statement = key_statement
                .as_ref()
                .map_or(statement, |keys| &keys.statement);
            let schema = projection_columns(&statement.projections[..public_projection_count]);
            let input_schema = operator.row_schema().clone();
            let work_mem_bytes = physical_work_mem_bytes(engine)?;
            let output_plan =
                prepare_aggregate_output_projection(engine, statement, internal_targets);
            let aggregate_schema = projection_columns(&output_plan.statement.projections);
            let aggregate_types = output_plan
                .statement
                .projections
                .iter()
                .map(|projection| {
                    evaluator
                        .expression_type(&projection.expr, &input_schema)
                        .ok()
                        .flatten()
                })
                .collect::<Vec<_>>();
            let aggregate_row_schema = uqa_execution::RowSchema::with_types(
                aggregate_schema.clone(),
                aggregate_types.clone(),
            );
            let aggregate_executor = PhysicalAggregateExecutor::new(
                engine,
                &output_plan.statement,
                params,
                ctes,
                input_schema,
                aggregate_row_schema,
                work_mem_bytes,
            )?;
            operator = Box::new(HashAggregate::with_typed_executor(
                operator,
                aggregate_schema,
                aggregate_types,
                Box::new(aggregate_executor),
            ));
            let output = identity_order_columns(&schema);
            operator = attach_final_projection_order(
                operator,
                (statement, &output),
                output_plan.projections,
                engine,
                params,
                ctes,
                evaluator,
            )?;
        }
        ComputePlan::Window => {
            let source_row_schema = operator.row_schema().clone();
            let work_mem_bytes = physical_work_mem_bytes(engine)?;
            let window_plan = prepare_window_plan(&statement.projections);
            let mut projections = physical_projections(window_plan.projections());
            let schema = window_plan.output_schema(engine, &source_row_schema, params)?;
            let output_columns = order_projection(&statement.projections, &source_row_schema)?
                .1
                .into_iter()
                .enumerate()
                .map(|(position, (output, _))| (output, ScalarExpr::Position(position)))
                .collect::<Vec<_>>();
            resjunk.distinct_on.extend(append_distinct_set_projections(
                statement,
                &output_columns,
                &mut projections,
            )?);
            let (order_statement, order_columns) = prepare_order_set_projections(
                engine,
                &type_resolver,
                statement,
                &output_columns,
                &mut projections,
                &schema,
                params,
            )?;
            resjunk.order_by.extend(order_columns);
            operator = Box::new(Window::with_row_schema_executor(
                operator,
                schema.clone(),
                Box::new(PhysicalWindowExecutor::new(
                    engine,
                    window_plan,
                    params,
                    ctes,
                    source_row_schema,
                    work_mem_bytes,
                )),
            ));
            let effective_order_statement = order_statement.as_ref().unwrap_or(statement);
            operator = attach_final_projection_order(
                operator,
                (effective_order_statement, &output_columns),
                projections,
                engine,
                params,
                ctes,
                evaluator,
            )?;
        }
    }

    Ok((operator, resjunk))
}

pub(in crate::sql) fn walk_expr<F: FnMut(&ScalarExpr)>(expr: &ScalarExpr, f: &mut F) {
    f(expr);
    match expr {
        ScalarExpr::And(parts) | ScalarExpr::Or(parts) => {
            for p in parts {
                walk_expr(p, f);
            }
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::InSubquery { expr: inner, .. } => walk_expr(inner, f),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
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
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for p in args {
                walk_expr(p, f);
            }
            for order in order_by {
                walk_expr(&order.expr, f);
            }
            if let Some(filter) = filter {
                walk_expr(filter, f);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for p in args {
                walk_expr(p, f);
            }
            for partition in &spec.partition_by {
                walk_expr(partition, f);
            }
            for order in &spec.order_by {
                walk_expr(&order.expr, f);
            }
            if let Some(frame) = &spec.frame {
                for bound in [&frame.start, &frame.end] {
                    match bound {
                        ScalarFrameBound::Preceding(expression)
                        | ScalarFrameBound::Following(expression) => walk_expr(expression, f),
                        ScalarFrameBound::UnboundedPreceding
                        | ScalarFrameBound::UnboundedFollowing
                        | ScalarFrameBound::CurrentRow => {}
                    }
                }
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
        ScalarExpr::Array(items) | ScalarExpr::Row(items) => {
            for p in items {
                walk_expr(p, f);
            }
        }
        _ => {}
    }
}

pub(in crate::sql) fn expr_contains_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
    let mut found = false;
    walk_expr(expr, &mut |part| {
        if expr_is_jsonpath_fts_match(part) {
            found = true;
        }
    });
    found
}

pub(in crate::sql) fn expr_is_jsonpath_fts_match(expr: &ScalarExpr) -> bool {
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

#[cfg(test)]
mod tests;
