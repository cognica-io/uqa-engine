//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Facet output, star expansion, top-k, limit, and projection helpers.

use super::{
    collect_query_operator, contains_aggregate, eval_physical_scalar, expect_column_name,
    expr_contains_volatile_function, has_aggregate, physical_exec_error, physical_projections,
    physical_work_mem_bytes, projection_label_at, ComputePlan, CteScope, Document, Engine,
    EngineExpressionEvaluator, PhysicalEvalContext, ProjectionPlan, QueryBlockPlan, QueryOutput,
    QueryOutputMode, QueryRows, ResultRow, SQLError, SQLParam, ScalarExpr, ScopedEngineHook,
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
        ProjectSet, SortKey,
    };

    let include_field = execution.fields.len() > 1;
    let table_state = engine.require_table(table)?;
    let source = ScoredDocumentSource::new(
        table,
        table_state,
        scored,
        execution.source_schema,
        None,
        None,
    );
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
pub(in crate::sql) fn expand_star_columns(
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
    if stmt.distinct
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

pub(in crate::sql) fn float_limit_offset(value: f64, label: &str) -> Result<u64, SQLError> {
    const U64_UPPER_EXCLUSIVE: f64 = 18_446_744_073_709_551_616.0;
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value >= U64_UPPER_EXCLUSIVE {
        return Err(SQLError::TypeMismatch(format!(
            "{label} must be a finite non-negative integer within the u64 execution range, got {value}"
        )));
    }
    Ok(value as u64)
}

pub(in crate::sql) fn projection_columns(projections: &[ProjectionPlan]) -> Vec<String> {
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

pub(in crate::sql) fn build_projection_row_with_ctes(
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
