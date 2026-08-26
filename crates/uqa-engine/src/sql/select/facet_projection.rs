//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Facet output, star expansion, top-k, limit, and projection helpers.

use super::{
    collect_query_operator, contains_aggregate, eval_physical_scalar, expect_column_name,
    expr_contains_volatile_function, has_aggregate, physical_exec_error, physical_projections,
    physical_work_mem_bytes, projection_label_at, ComputePlan, CteScope, Engine,
    EngineExpressionEvaluator, PhysicalEvalContext, ProjectionPlan, QueryBlockPlan, QueryOutput,
    QueryOutputMode, QueryRows, SQLError, SQLParam, ScalarExpr, ScopedEngineHook,
    ScoredDocumentSource, ScoredInput, Value, SCORE_COLUMN,
};

pub(in crate::sql) fn facet_projection_fields(
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

pub(in crate::sql) struct FacetExecution<'a> {
    pub(super) fields: &'a [String],
    pub(super) source_schema: Vec<String>,
    pub(super) params: &'a [SQLParam],
    pub(super) ctes: &'a CteScope,
    pub(super) output_mode: QueryOutputMode,
}

pub(in crate::sql) fn build_facet_output(
    engine: &Engine,
    table: &str,
    scored: ScoredInput,
    predicate: Option<ScalarExpr>,
    execution: FacetExecution<'_>,
) -> Result<QueryOutput, SQLError> {
    use uqa_execution::{
        AggregateKind, AggregateSpec, ExternalSort, Filter, HashAggregate, PhysicalOperator,
        PhysicalProjectSet, RowProjectionValue, RowSchema, SortKey,
    };

    let include_field = execution.fields.len() > 1;
    let table_state = engine.require_table(table)?;
    let table_columns = table_state
        .columns
        .read()
        .iter()
        .map(|column| column.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(field) = execution
        .fields
        .iter()
        .find(|field| !table_columns.contains(field.as_str()))
    {
        return Err(SQLError::UnknownColumn(field.clone()));
    }
    let source = ScoredDocumentSource::new(
        table,
        table_state,
        scored,
        execution.source_schema,
        None,
        None,
    )
    .with_table_oid(crate::sql::catalog::table_relation_oid(engine, table)?);
    let mut source: Box<dyn PhysicalOperator + '_> =
        Box::new(uqa_execution::TableScan::new(Box::new(source)));
    if let Some(predicate) = predicate {
        source = Box::new(Filter::with_evaluator(
            source,
            predicate,
            EngineExpressionEvaluator::shared(engine, execution.params, execution.ctes),
        ));
    }

    let facet_columns = if include_field {
        vec!["facet_field".into(), "facet_value".into()]
    } else {
        vec!["facet_value".into()]
    };
    let facet_layout = execution
        .fields
        .iter()
        .filter_map(|field| {
            let logical = source.row_schema().position(field)?;
            let physical = source.row_schema().physical_slot(logical)?;
            Some((field.clone(), logical, physical))
        })
        .collect::<Vec<_>>();
    let facet_rows: Box<dyn PhysicalOperator + '_> =
        Box::new(PhysicalProjectSet::new(
            source,
            RowSchema::new(facet_columns.clone()),
            Box::new(move |document: uqa_execution::OwnedPhysicalRow| {
                let rows = facet_layout.clone().into_iter().filter_map(
                    move |(field, logical, physical)| {
                        if matches!(document.view().value_at(logical), None | Some(Value::Null)) {
                            return None;
                        }
                        let projected = if include_field {
                            document.row.project_with_values([
                                RowProjectionValue::Owned(Value::Str(field)),
                                RowProjectionValue::InputSlot(physical),
                            ])
                        } else {
                            document
                                .row
                                .project_with_values([RowProjectionValue::InputSlot(physical)])
                        };
                        Some(Ok(projected))
                    },
                );
                Ok(Box::new(rows) as uqa_execution::PhysicalProjectRows)
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

pub(in crate::sql) fn order_by_references_field(stmt: &QueryBlockPlan) -> bool {
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
pub(in crate::sql) fn score_limited_text_filter(expr: Option<&ScalarExpr>) -> bool {
    let Some(ScalarExpr::Func { name, .. }) = expr else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "text_match" | "bayesian_match"
    )
}

pub(in crate::sql) fn score_order_top_k(
    stmt: &QueryBlockPlan,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<usize>, SQLError> {
    if !stmt.locking.is_empty()
        || stmt.distinct
        || !stmt.distinct_on.is_empty()
        || !matches!(stmt.compute, ComputePlan::Project)
        || stmt.order_by.is_empty()
        || order_by_references_field(stmt)
        || stmt.order_by.iter().any(|order| !order.descending)
        || has_aggregate(engine, &stmt.projections)
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        return Ok(None);
    }
    resolve_score_slice_top_k(stmt, engine, params, ctes)
}

/// Return the score prefix required by a score-first SQL slice. Secondary
/// sort keys are allowed because the caller retains every row tied at the
/// boundary score and leaves exact tie ordering to the relational pipeline.
pub(in crate::sql) fn post_retrieval_score_top_k(
    stmt: &QueryBlockPlan,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<usize>, SQLError> {
    let Some(primary_order) = stmt.order_by.first() else {
        return Ok(None);
    };
    // A locking query must keep the complete ranked candidate stream: SKIP LOCKED skips rows, and a tuple-local recheck can drop a changed candidate, in which case PostgreSQL 18 surfaces the next candidate.
    if !stmt.locking.is_empty() {
        return Ok(None);
    }
    if stmt.distinct
        || !stmt.distinct_on.is_empty()
        || !matches!(stmt.compute, ComputePlan::Project)
        || !primary_order.descending
        || !matches!(
            &primary_order.expr,
            ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. }
                if name == SCORE_COLUMN
        )
        || stmt
            .order_by
            .iter()
            .any(|order| order.expr.contains_window() || contains_aggregate(engine, &order.expr))
    {
        return Ok(None);
    }
    resolve_score_slice_top_k(stmt, engine, params, ctes)
}

fn resolve_score_slice_top_k(
    stmt: &QueryBlockPlan,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Option<usize>, SQLError> {
    if stmt.with_ties {
        return Ok(None);
    }
    if stmt
        .limit
        .iter()
        .chain(stmt.offset.iter())
        .any(|expr| expr_contains_volatile_function(engine, expr))
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

pub(in crate::sql) fn explain_int_expr(expr: &ScalarExpr) -> String {
    match expr {
        ScalarExpr::Literal(Value::Int(n)) => n.to_string(),
        _ => "<expr>".to_string(),
    }
}

/// Evaluate a `LIMIT` / `OFFSET` expression to a non-negative `u64`.
/// Accepts integer constants, `$N` parameter references, and any expression
/// that the row evaluator
/// can fold to an integer at execute time. Returns `None` when the
/// clause was absent.
pub(in crate::sql) fn resolve_limit_offset_with_ctes(
    expr: Option<&ScalarExpr>,
    engine: &Engine,
    params: &[SQLParam],
    label: &str,
    ctes: &CteScope,
) -> Result<Option<u64>, SQLError> {
    let Some(expr) = expr else {
        return Ok(None);
    };
    let value = evaluate_limit_offset_with_ctes(expr, engine, params, ctes)?;
    coerce_limit_offset(value, expr, label)
}

/// Resolve the mandatory row count for `FETCH ... WITH TIES`. `PostgreSQL` uses `invalid_row_count_in_result_offset_clause` for both a NULL and a negative boundary, with a clause-specific diagnostic for NULL.
pub(in crate::sql) fn resolve_fetch_limit_with_ties(
    expr: Option<&ScalarExpr>,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<u64, SQLError> {
    let Some(expr) = expr else {
        return Err(SQLError::Internal(
            "FETCH ... WITH TIES is missing its row-count expression".into(),
        ));
    };
    let value = evaluate_limit_offset_with_ctes(expr, engine, params, ctes)?;
    match value {
        Value::Null => Err(SQLError::Routine {
            sqlstate: "2201W".into(),
            message: "row count cannot be null in FETCH FIRST ... WITH TIES clause".into(),
        }),
        value => coerce_limit_offset(value, expr, "LIMIT")?.ok_or_else(|| {
            SQLError::Internal("FETCH ... WITH TIES resolved without a row count".into())
        }),
    }
}

fn evaluate_limit_offset_with_ctes(
    expr: &ScalarExpr,
    engine: &Engine,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Value, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    let ctx = PhysicalEvalContext::new(None, params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    eval_physical_scalar(expr, &ctes.scalar_subqueries, &ctx)
}

fn coerce_limit_offset(
    value: Value,
    expression: &ScalarExpr,
    label: &str,
) -> Result<Option<u64>, SQLError> {
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    let allow_unknown_string = matches!(
        expression,
        ScalarExpr::Literal(Value::Str(_)) | ScalarExpr::Param(_)
    );
    let value = match &value {
        Value::Int(value) => Value::Int(*value),
        Value::Float(value) => Value::Int(
            i64::try_from(float_limit_offset(*value, label)?)
                .expect("PostgreSQL bigint row count fits i64"),
        ),
        Value::Decimal(decimal) if decimal.is_nan() => {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot convert NaN to bigint".into(),
            });
        }
        Value::Decimal(decimal) if decimal.is_infinite() => {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cannot convert infinity to bigint".into(),
            });
        }
        Value::Decimal(_) => uqa_sql::expr::cast_value(&value, "bigint")?,
        Value::Str(_) if allow_unknown_string => uqa_sql::expr::cast_value(&value, "bigint")?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "argument of {label} must be type bigint, got {other:?}"
            )));
        }
    };
    let Value::Int(value) = value else {
        return Err(SQLError::Internal(
            "row-count bigint coercion did not return an integer".into(),
        ));
    };
    if value < 0 {
        return Err(negative_row_count(label));
    }
    Ok(Some(u64::try_from(value).map_err(|_| {
        SQLError::Internal("non-negative bigint did not fit u64".into())
    })?))
}

fn negative_row_count(label: &str) -> SQLError {
    if label == "OFFSET" {
        SQLError::Routine {
            sqlstate: "2201X".into(),
            message: "OFFSET must not be negative".into(),
        }
    } else {
        SQLError::Routine {
            sqlstate: "2201W".into(),
            message: "LIMIT must not be negative".into(),
        }
    }
}

pub(in crate::sql) fn float_limit_offset(value: f64, label: &str) -> Result<u64, SQLError> {
    let Value::Int(value) = uqa_sql::expr::cast_value(&Value::Float(value), "bigint")? else {
        return Err(SQLError::Internal(
            "float row-count coercion did not return an integer".into(),
        ));
    };
    if value < 0 {
        return Err(negative_row_count(label));
    }
    Ok(u64::try_from(value).expect("non-negative bigint fits u64"))
}

pub(in crate::sql) fn projection_columns(projections: &[ProjectionPlan]) -> Vec<String> {
    projections.iter().map(projection_label_at).collect()
}

pub(in crate::sql) fn build_projection_physical_row_with_ctes(
    engine: &Engine,
    input: &uqa_execution::OwnedPhysicalRow,
    projections: &[ProjectionPlan],
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<uqa_execution::OwnedPhysicalRow, SQLError> {
    use uqa_execution::physical::run_to_batches;
    use uqa_execution::scan::TableScan;
    use uqa_execution::{PhysicalOperator, Project};

    let scan: Box<dyn PhysicalOperator + '_> = Box::new(TableScan::from_physical_rows(
        input.schema.clone(),
        vec![input.row.clone()],
    ));
    let evaluator = EngineExpressionEvaluator::shared(engine, params, ctes);
    let mut project =
        Project::with_target_evaluator(scan, physical_projections(projections), evaluator);
    let mut rows = run_to_batches(&mut project)
        .map_err(physical_exec_error)?
        .into_iter()
        .flat_map(uqa_execution::Batch::into_owned_rows);
    let row = rows.next().ok_or_else(|| {
        SQLError::Internal("physical projection produced no row for a single-row input".into())
    })?;
    if rows.next().is_some() {
        return Err(SQLError::Internal(
            "physical projection produced multiple rows for a single-row input".into(),
        ));
    }
    Ok(row)
}
