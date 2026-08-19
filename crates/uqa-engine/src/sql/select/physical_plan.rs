//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection, ordering, filtering, and relational physical-operator assembly.

use super::{
    build_set_projection, collect_exists_key_operator, collect_query_operator,
    column_prune_for_stmt, expression_may_return_set, final_filter_after_qualifier_pushdown,
    is_score_provenance_column, prepare_aggregate_set_projection,
    prepare_correlated_exists_predicate, prepare_group_set_projection, prepare_window_plan,
    projection_columns, projections_may_return_set, qualifier_filters_for_stmt,
    resolve_limit_offset_with_ctes, should_defer_distinct_limit,
    validate_query_block_expression_types, validate_query_set_contexts, Arc, ComputePlan, CteScope,
    Engine, EngineExpressionEvaluator, HashSet, OutputColumnMapping, PhysicalAggregateExecutor,
    PhysicalProjection, PhysicalWindowExecutor, ProjectionPlan, QueryBlockPlan, QueryOutput,
    QueryOutputMode, ResultRow, SQLError, SQLParam, ScalarExpr, SharedExpressionEvaluator, Value,
    DOC_ID_COLUMN, MERGE_ACTION_COLUMN, SCORE_COLUMN,
};

const ORDER_SET_COLUMN_PREFIX: &str = "\0uqa.order_set_value.";
const DISTINCT_SET_COLUMN_PREFIX: &str = "\0uqa.distinct_set_value.";

fn projection_set_batch_size(statement: &QueryBlockPlan) -> usize {
    if statement.limit.is_some()
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
                        .filter(|(_, column)| visible_projection_source_column(column))
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
                    .qualified_star_layout(qualifier)
                    .into_iter()
                    .filter(|(column, _, _)| visible_projection_source_column(column))
                    .map(|(column, _, _)| column)
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
        .map(|(index, projection)| (labels[index].clone(), projection.expr.clone()))
        .collect()
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

fn expand_bound_projection_stars(
    projections: &[ProjectionPlan],
    schema: &uqa_execution::RowSchema,
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let mut expanded = Vec::new();
    for projection in projections {
        match &projection.expr {
            ScalarExpr::Star => {
                for (position, column) in schema.columns().iter().enumerate() {
                    if !visible_projection_source_column(column) {
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
                    if !visible_projection_source_column(&column) {
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

pub(in crate::sql) fn score_provenance_columns(schema: &[String]) -> Vec<String> {
    schema
        .iter()
        .filter(|column| is_score_provenance_column(column))
        .cloned()
        .collect()
}

pub(in crate::sql) fn append_score_provenance_projections(
    projections: &mut Vec<PhysicalProjection>,
    schema: &[String],
) {
    for column in score_provenance_columns(schema) {
        if !projections.iter().any(|(name, _)| name == &column) {
            projections.push((column.clone(), ScalarExpr::Column(column)));
        }
    }
}

pub(in crate::sql) fn append_score_provenance_mappings(
    mappings: &mut Vec<OutputColumnMapping>,
    schema: &[String],
) {
    for column in score_provenance_columns(schema) {
        if !mappings.iter().any(|(name, _)| name == &column) {
            mappings.push((column.clone(), ScalarExpr::Column(column)));
        }
    }
}

pub(in crate::sql) fn visible_projection_source_column(column: &str) -> bool {
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
pub(in crate::sql) fn order_projection(
    projections: &[ProjectionPlan],
    input_schema: &uqa_execution::RowSchema,
) -> Result<(Vec<PhysicalProjection>, Vec<OutputColumnMapping>), SQLError> {
    let labels = projection_columns(projections);
    let mut physical = Vec::new();
    let mut output = Vec::new();
    let mut occupied: HashSet<String> = input_schema.columns().iter().cloned().collect();

    for (index, projection) in projections.iter().enumerate() {
        if matches!(projection.expr, ScalarExpr::Star) {
            for (position, column) in input_schema.columns().iter().enumerate() {
                if visible_projection_source_column(column) {
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
            for (star_position, (column, logical, _, _)) in columns.into_iter().enumerate() {
                if !visible_projection_source_column(&column) {
                    continue;
                }
                if let Some(logical) = logical {
                    output.push((column, ScalarExpr::Position(logical)));
                    continue;
                }
                let mut internal = format!("__uqa_projection_{index}_{star_position}");
                let mut suffix = 0usize;
                while occupied.contains(&internal) {
                    suffix += 1;
                    internal = format!("__uqa_projection_{index}_{star_position}_{suffix}");
                }
                occupied.insert(internal.clone());
                physical.push((
                    internal.clone(),
                    ScalarExpr::qualified_column(qualifier, &column),
                ));
                output.push((column, ScalarExpr::Column(internal)));
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

        let mut internal = format!("__uqa_projection_{index}");
        let mut suffix = 0usize;
        while occupied.contains(&internal) {
            suffix += 1;
            internal = format!("__uqa_projection_{index}_{suffix}");
        }
        occupied.insert(internal.clone());
        physical.push((internal.clone(), projection.expr.clone()));
        output.push((labels[index].clone(), ScalarExpr::Column(internal)));
    }
    Ok((physical, output))
}

fn split_locking_order_projections(
    statement: &QueryBlockPlan,
    output: &[OutputColumnMapping],
    physical: Vec<PhysicalProjection>,
) -> Result<
    (
        QueryBlockPlan,
        Vec<PhysicalProjection>,
        Vec<PhysicalProjection>,
    ),
    SQLError,
> {
    let mut sort_statement = statement.clone();
    let mut required = HashSet::new();
    for (index, order) in statement.order_by.iter().enumerate() {
        let expression = resolve_order_expression(&order.expr, output)?;
        if let ScalarExpr::Column(column) = &expression {
            if physical.iter().any(|(name, _)| name == column) {
                required.insert(column.clone());
                continue;
            }
        }
        if let Some((column, _)) = physical
            .iter()
            .find(|(_, projected)| crate::sql::aggregates::exprs_match(projected, &expression))
        {
            required.insert(column.clone());
            sort_statement.order_by[index].expr = ScalarExpr::Column(column.clone());
        }
    }
    let (before_sort, after_sort) = physical
        .into_iter()
        .partition(|(column, _)| required.contains(column));
    Ok((sort_statement, before_sort, after_sort))
}

fn append_row_at_time_projection<'a>(
    operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    projections: Vec<PhysicalProjection>,
    evaluator: SharedExpressionEvaluator<'a>,
) -> Box<dyn uqa_execution::PhysicalOperator + 'a> {
    if projections.is_empty() {
        return operator;
    }
    Box::new(uqa_execution::Project::appending_with_evaluator(
        Box::new(RowAtATime::new(operator)),
        projections,
        evaluator,
    ))
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
                ScalarExpr::Position(position) if position < schema.len() => Some(position),
                ScalarExpr::Column(column) => schema.position(&column),
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
                .ok_or_else(|| {
                    SQLError::TypeMismatch(format!(
                        "ORDER BY position {position} is not in the select list"
                    ))
                })?;
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

fn prepare_order_set_projections(
    engine: &Engine,
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    projections: &mut Vec<PhysicalProjection>,
    schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Result<Option<QueryBlockPlan>, SQLError> {
    let mut prepared: Option<QueryBlockPlan> = None;
    for (index, order) in statement.order_by.iter().enumerate() {
        let expression = resolve_order_expression(&order.expr, output_columns)?;
        if !expression_may_return_set(engine, &expression, schema, params)? {
            continue;
        }
        let column = if let Some((column, _)) = projections
            .iter()
            .find(|(_, projected)| crate::sql::aggregates::exprs_match(projected, &expression))
        {
            column.clone()
        } else {
            let column = format!("{ORDER_SET_COLUMN_PREFIX}{index}");
            projections.push((column.clone(), expression));
            column
        };
        prepared.get_or_insert_with(|| statement.clone()).order_by[index].expr =
            ScalarExpr::Column(column);
    }
    Ok(prepared)
}

fn append_distinct_set_projections(
    engine: &Engine,
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    projections: &mut Vec<PhysicalProjection>,
    schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Result<Vec<String>, SQLError> {
    let mut columns = Vec::new();
    for (index, expression) in statement.distinct_on.iter().enumerate() {
        let expression = resolve_order_expression(expression, output_columns)?;
        if expression_may_return_set(engine, &expression, schema, params)? {
            let column = format!("{DISTINCT_SET_COLUMN_PREFIX}{index}");
            projections.push((column.clone(), expression));
            columns.push(column);
        }
    }
    Ok(columns)
}

fn prepare_aggregate_set_key_statement(
    engine: &Engine,
    statement: &QueryBlockPlan,
    schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Result<Option<QueryBlockPlan>, SQLError> {
    let output = identity_order_columns(&projection_columns(&statement.projections));
    let mut prepared: Option<QueryBlockPlan> = None;
    for (index, order) in statement.order_by.iter().enumerate() {
        let expression = resolve_order_expression(&order.expr, &output)?;
        if !expression_may_return_set(engine, &expression, schema, params)? {
            continue;
        }
        let current = prepared.as_ref().unwrap_or(statement);
        let labels = projection_columns(&current.projections);
        let column = current
            .projections
            .iter()
            .zip(labels)
            .find(|(projection, _)| {
                crate::sql::aggregates::exprs_match(&projection.expr, &expression)
            })
            .map_or_else(
                || format!("{ORDER_SET_COLUMN_PREFIX}{index}"),
                |(_, label)| label,
            );
        let prepared = prepared.get_or_insert_with(|| statement.clone());
        if !prepared
            .projections
            .iter()
            .any(|projection| projection.alias.as_deref() == Some(&column))
            && column.starts_with(ORDER_SET_COLUMN_PREFIX)
        {
            prepared.projections.push(ProjectionPlan {
                expr: expression,
                alias: Some(column.clone()),
            });
        }
        prepared.order_by[index].expr = ScalarExpr::Column(column);
    }
    for (index, expression) in statement.distinct_on.iter().enumerate() {
        let expression = resolve_order_expression(expression, &output)?;
        if !expression_may_return_set(engine, &expression, schema, params)? {
            continue;
        }
        let column = format!("{DISTINCT_SET_COLUMN_PREFIX}{index}");
        prepared
            .get_or_insert_with(|| statement.clone())
            .projections
            .push(ProjectionPlan {
                expr: expression,
                alias: Some(column),
            });
    }
    Ok(prepared)
}

pub(in crate::sql) fn execute_filter_rows(
    engine: &Engine,
    schema: uqa_execution::RowSchema,
    rows: Vec<ResultRow>,
    predicate: ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ResultRow>, SQLError> {
    use uqa_execution::scan::TableScan;
    use uqa_execution::{physical::run_to_rows, Filter, PhysicalOperator};

    let scan: Box<dyn PhysicalOperator + '_> =
        Box::new(TableScan::from_rows_with_schema(schema, rows));
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    let mut filter = Filter::with_evaluator(scan, predicate, evaluator);
    let (_, rows) = run_to_rows(&mut filter).map_err(physical_exec_error)?;
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::sql) fn attach_order_limit<'a>(
    mut operator: Box<dyn uqa_execution::PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    output_columns: &[OutputColumnMapping],
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: &CteScope,
    evaluator: SharedExpressionEvaluator<'a>,
    recheck_source: Option<crate::sql::select::LockRowsRecheckSource>,
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
        let required_ordering = keys
            .iter()
            .map(|key| {
                uqa_execution::order_expression_position(operator.row_schema(), &key.expr).map(
                    |position| uqa_execution::PhysicalOrder {
                        position,
                        descending: key.descending,
                        nulls_first: Some(key.nulls_first.unwrap_or(key.descending)),
                        nullable: true,
                    },
                )
            })
            .collect::<Option<Vec<_>>>();
        let already_ordered = required_ordering.as_ref().is_some_and(|required| {
            uqa_execution::ordering_satisfies(operator.output_ordering(), required)
        });
        if !already_ordered {
            // A locking query must keep the complete sorted candidate
            // stream: SKIP LOCKED skips rows, and a tuple-local recheck can
            // drop a changed candidate, in which case PostgreSQL 18 surfaces
            // the next candidate in sort order instead of returning fewer
            // rows.
            operator = Box::new(ExternalSort::new(
                operator,
                keys,
                evaluator,
                keep.filter(|_| statement.locking.is_empty()),
                work_mem_bytes,
            ));
        }
    }
    if !statement.locking.is_empty() {
        let max_rows = limit
            .map(|limit| {
                offset
                    .unwrap_or(0)
                    .checked_add(limit)
                    .ok_or_else(|| SQLError::TypeMismatch("OFFSET + LIMIT overflow".into()))
            })
            .transpose()?;
        operator = crate::sql::select::attach_lock_rows(
            engine,
            operator,
            statement,
            params,
            ctes,
            max_rows,
            recheck_source,
        )?;
    }
    if offset.is_some() || limit.is_some() {
        operator = Box::new(Limit::new(operator, offset.unwrap_or(0), limit));
    }
    Ok(operator)
}

/// Rebuild the plan below one `LockRows` boundary for a tuple-local recheck.
/// The construction replays the same source, filter, and scalar target
/// projection below the original `LockRows` boundary, with the recheck pins
/// active in `ctes` so every lock-target base scan emits only the candidate's
/// tuples while unmarked relations rescan under the statement snapshot.
/// Sorting, locking, and `LIMIT` never run here: the candidate keeps its
/// original position in the outer stream.
pub(in crate::sql) fn build_row_lock_recheck_operator<'a>(
    engine: &'a Engine,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &mut CteScope,
    _ordered: bool,
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
    let (physical, _) = order_projection(&statement.projections, operator.row_schema())?;
    operator = if physical.is_empty() {
        operator
    } else {
        Box::new(Project::appending_with_evaluator(
            operator, physical, evaluator,
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
    validate_query_block_expression_types(engine, statement, operator.row_schema(), params)?;
    validate_query_set_contexts(engine, statement, operator.row_schema(), params)?;
    if matches!(output_mode, QueryOutputMode::ExistsKeySet)
        && matches!(statement.compute, ComputePlan::Project)
        && statement.order_by.is_empty()
        && statement.limit.is_none()
        && statement.offset.is_none()
        && !statement.distinct
        && statement.distinct_on.is_empty()
        && !projections_may_return_set(
            engine,
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
) -> Result<QueryOutput, SQLError> {
    use uqa_execution::{ColumnSelection, Distinct, Limit};

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
            let distinct_on = original
                .distinct_on
                .iter()
                .enumerate()
                .map(|(index, expression)| {
                    if operator
                        .schema()
                        .iter()
                        .any(|column| column == &format!("{DISTINCT_SET_COLUMN_PREFIX}{index}"))
                    {
                        ScalarExpr::Column(format!("{DISTINCT_SET_COLUMN_PREFIX}{index}"))
                    } else {
                        expression.clone()
                    }
                })
                .collect();
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
        let limit =
            resolve_limit_offset_with_ctes(original.limit.as_ref(), engine, params, "LIMIT", ctes)?;
        operator = Box::new(Limit::new(operator, offset.unwrap_or(0), limit));
    }
    if operator
        .schema()
        .iter()
        .any(|column| column.starts_with(DISTINCT_SET_COLUMN_PREFIX))
    {
        let visible = operator
            .schema()
            .iter()
            .filter(|column| !column.starts_with(DISTINCT_SET_COLUMN_PREFIX))
            .cloned()
            .collect();
        operator = Box::new(ColumnSelection::new(operator, visible));
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
) -> Result<Box<dyn uqa_execution::PhysicalOperator + 'a>, SQLError> {
    use uqa_execution::{ColumnSelection, HashAggregate, Project, Window};

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
    if let Some(clause) = statement.locking.first() {
        let projections = physical_projections(&statement.projections);
        let (_, output) = order_projection(&statement.projections, operator.row_schema())?;
        let order_returns_set = statement.order_by.iter().try_fold(false, |found, order| {
            if found {
                return Ok(true);
            }
            let expression = resolve_order_expression(&order.expr, &output)?;
            expression_may_return_set(engine, &expression, operator.row_schema(), params)
        })?;
        if projections_may_return_set(engine, &projections, operator.row_schema(), params)?
            || order_returns_set
        {
            return Err(SQLError::Unsupported(format!(
                "{} is not allowed with set-returning functions in the target list",
                clause.strength.sql_name()
            )));
        }
    }
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    operator = attach_relational_filter(engine, operator, predicate, params, ctes, &evaluator)?;
    let mut group_statement = None;
    if matches!(statement.compute, ComputePlan::Aggregate) {
        if let Some(plan) =
            prepare_group_set_projection(engine, statement, operator.row_schema(), params)?
        {
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
                append_distinct_set_projections(
                    engine,
                    statement,
                    &distinct_output,
                    &mut projections,
                    operator.row_schema(),
                    params,
                )?;
                append_score_provenance_projections(&mut projections, operator.schema());
                if !statement.locking.is_empty() {
                    let (physical, mut output) =
                        order_projection(&statement.projections, operator.row_schema())?;
                    append_score_provenance_mappings(&mut output, operator.schema());
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
                        Some(crate::sql::select::LockRowsRecheckSource::new(
                            statement, ctes, false,
                        )),
                    )?;
                    let output = output_selection_positions(operator.row_schema(), output)?;
                    operator = Box::new(ColumnSelection::with_positions(operator, output));
                } else if projections_may_return_set(
                    engine,
                    &projections,
                    operator.row_schema(),
                    params,
                )? {
                    // PostgreSQL evaluates set-returning target expressions
                    // above LockRows, so a rechecked tuple re-expands its
                    // rows from the substituted base tuple. Locks therefore
                    // attach below the set projection, and LIMIT is applied
                    // to the expanded output above.
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
                        projection_set_batch_size(statement),
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
                    operator = Box::new(Project::with_evaluator(operator, projections, evaluator));
                }
            } else {
                let (mut physical, mut output) =
                    order_projection(&statement.projections, operator.row_schema())?;
                // SQL ordinals and aliases are resolved only against the
                // visible SELECT list. Score provenance is carried through
                // the final column selection for parent query blocks, but it
                // is not itself a selectable output position.
                let order_output = output.clone();
                let order_statement = prepare_order_set_projections(
                    engine,
                    statement,
                    &order_output,
                    &mut physical,
                    operator.row_schema(),
                    params,
                )?;
                let distinct_columns = append_distinct_set_projections(
                    engine,
                    statement,
                    &order_output,
                    &mut physical,
                    operator.row_schema(),
                    params,
                )?;
                append_score_provenance_mappings(&mut output, operator.schema());
                output.extend(
                    distinct_columns
                        .into_iter()
                        .map(|column| (column.clone(), ScalarExpr::Column(column))),
                );
                if statement.locking.is_empty() {
                    operator = if projections_may_return_set(
                        engine,
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
                        Box::new(Project::appending_with_evaluator(
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
                } else {
                    let effective_order_statement = order_statement.as_ref().unwrap_or(statement);
                    let (mut sort_statement, before_sort, after_sort) =
                        split_locking_order_projections(
                            effective_order_statement,
                            &order_output,
                            physical,
                        )?;
                    if !before_sort.is_empty() {
                        operator = Box::new(Project::appending_with_evaluator(
                            operator,
                            before_sort,
                            Arc::clone(&evaluator),
                        ));
                    }
                    sort_statement.locking.clear();
                    sort_statement.limit = None;
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
                    lock_statement.order_by.clear();
                    operator = attach_order_limit(
                        operator,
                        &lock_statement,
                        &[],
                        engine,
                        params,
                        ctes,
                        Arc::clone(&evaluator),
                        Some(crate::sql::select::LockRowsRecheckSource::new(
                            statement, ctes, true,
                        )),
                    )?;
                }
                let output = output_selection_positions(operator.row_schema(), output)?;
                operator = Box::new(ColumnSelection::with_positions(operator, output));
            }
        }
        ComputePlan::Aggregate => {
            let set_key_statement = prepare_aggregate_set_key_statement(
                engine,
                statement,
                operator.row_schema(),
                params,
            )?;
            let statement = set_key_statement.as_ref().unwrap_or(statement);
            let schema = projection_columns(&statement.projections);
            let input_schema = operator.row_schema().clone();
            let work_mem_bytes = physical_work_mem_bytes(engine)?;
            if let Some(set_plan) =
                prepare_aggregate_set_projection(engine, statement, &input_schema, params)?
            {
                let aggregate_schema = projection_columns(&set_plan.statement.projections);
                let aggregate_types = set_plan
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
                    &set_plan.statement,
                    params,
                    ctes,
                    input_schema.clone(),
                    aggregate_row_schema,
                    work_mem_bytes,
                )?;
                operator = Box::new(HashAggregate::with_typed_executor(
                    operator,
                    aggregate_schema,
                    aggregate_types,
                    Box::new(aggregate_executor),
                ));
                operator = build_set_projection(
                    operator,
                    engine,
                    params,
                    ctes,
                    Arc::clone(&evaluator),
                    set_plan.projections,
                    false,
                    projection_set_batch_size(statement),
                )?;
            } else {
                let aggregate_types = statement
                    .projections
                    .iter()
                    .map(|projection| {
                        evaluator
                            .expression_type(&projection.expr, &input_schema)
                            .ok()
                            .flatten()
                    })
                    .collect::<Vec<_>>();
                let aggregate_row_schema =
                    uqa_execution::RowSchema::with_types(schema.clone(), aggregate_types.clone());
                operator = Box::new(HashAggregate::with_typed_executor(
                    operator,
                    schema.clone(),
                    aggregate_types,
                    Box::new(PhysicalAggregateExecutor::new(
                        engine,
                        statement,
                        params,
                        ctes,
                        input_schema,
                        aggregate_row_schema,
                        work_mem_bytes,
                    )?),
                ));
            }
            let output = identity_order_columns(&schema);
            operator = attach_order_limit(
                operator, statement, &output, engine, params, ctes, evaluator, None,
            )?;
            if set_key_statement.is_some() {
                let visible = operator
                    .schema()
                    .iter()
                    .filter(|column| !column.starts_with(ORDER_SET_COLUMN_PREFIX))
                    .cloned()
                    .collect();
                operator = Box::new(ColumnSelection::new(operator, visible));
            }
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
            let order_statement = prepare_order_set_projections(
                engine,
                statement,
                &output_columns,
                &mut projections,
                &schema,
                params,
            )?;
            append_distinct_set_projections(
                engine,
                statement,
                &output_columns,
                &mut projections,
                &schema,
                params,
            )?;
            operator = Box::new(Window::with_typed_executor(
                operator,
                schema.columns().to_vec(),
                schema.column_types().to_vec(),
                Box::new(PhysicalWindowExecutor::new(
                    engine,
                    window_plan,
                    params,
                    ctes,
                    source_row_schema,
                    work_mem_bytes,
                )),
            ));
            append_score_provenance_projections(&mut projections, operator.schema());
            operator =
                if projections_may_return_set(engine, &projections, operator.row_schema(), params)?
                {
                    build_set_projection(
                        operator,
                        engine,
                        params,
                        ctes,
                        Arc::clone(&evaluator),
                        projections,
                        false,
                        projection_set_batch_size(statement),
                    )?
                } else {
                    Box::new(Project::with_evaluator(
                        operator,
                        projections,
                        Arc::clone(&evaluator),
                    ))
                };
            operator = attach_order_limit(
                operator,
                order_statement.as_ref().unwrap_or(statement),
                &output_columns,
                engine,
                params,
                ctes,
                evaluator,
                None,
            )?;
            if order_statement.is_some() {
                let visible = operator
                    .schema()
                    .iter()
                    .filter(|column| !column.starts_with(ORDER_SET_COLUMN_PREFIX))
                    .cloned()
                    .collect();
                operator = Box::new(ColumnSelection::new(operator, visible));
            }
        }
    }

    Ok(operator)
}

pub(in crate::sql) fn walk_expr<F: FnMut(&ScalarExpr)>(expr: &ScalarExpr, f: &mut F) {
    f(expr);
    match expr {
        ScalarExpr::And(parts) | ScalarExpr::Or(parts) => {
            for p in parts {
                walk_expr(p, f);
            }
        }
        ScalarExpr::Not(inner) | ScalarExpr::UnaryMinus(inner) => walk_expr(inner, f),
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
