//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection, ordering, filtering, and relational physical-operator assembly.

use super::{
    collect_query_operator, is_score_provenance_column, prepare_window_plan, projection_columns,
    resolve_limit_offset_with_ctes, should_defer_distinct_limit, Arc, ComputePlan, CteScope,
    Engine, EngineExpressionEvaluator, HashSet, OutputColumnMapping, PhysicalAggregateExecutor,
    PhysicalProjection, PhysicalWindowExecutor, ProjectionPlan, QueryBlockPlan, QueryOutput,
    QueryOutputMode, ResultRow, SQLError, SQLParam, ScalarExpr, SharedExpressionEvaluator,
    SourcePlan, Value, DOC_ID_COLUMN, MERGE_ACTION_COLUMN, SCORE_COLUMN,
};

pub(in crate::sql) fn expand_from_star_columns(
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

pub(in crate::sql) fn from_clause_output_columns(
    engine: &Engine,
    from: &SourcePlan,
) -> Vec<String> {
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

pub(in crate::sql) fn qualify_output_columns(
    alias: Option<&str>,
    columns: Vec<String>,
) -> Vec<String> {
    match alias {
        Some(a) => columns
            .into_iter()
            .map(|column| format!("{a}.{column}"))
            .collect(),
        None => columns,
    }
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

pub(in crate::sql) fn source_columns(rows: &[ResultRow]) -> Vec<String> {
    rows.first()
        .map(|row| row.keys().cloned().collect())
        .unwrap_or_default()
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
            mappings.push((column.clone(), column));
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

pub(in crate::sql) fn identity_order_columns(columns: &[String]) -> Vec<OutputColumnMapping> {
    columns
        .iter()
        .map(|column| (column.clone(), column.clone()))
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

pub(in crate::sql) fn execute_filter_rows(
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

pub(in crate::sql) fn attach_order_limit<'a>(
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
pub(in crate::sql) fn finish_query_block_operator_output<'a>(
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

pub(in crate::sql) fn build_relational_operator<'a>(
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

pub(in crate::sql) fn walk_expr<F: FnMut(&ScalarExpr)>(expr: &ScalarExpr, f: &mut F) {
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
