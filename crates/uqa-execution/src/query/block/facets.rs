//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Facet output and score-prefix execution.

use super::{
    expect_column_name, physical_work_mem_bytes, ComputePlan, CteScope, ProjectionPlan,
    QueryBlockPlan, QueryOutput, QueryOutputMode, QueryRows, SQLError, SQLParam, ScalarExpr,
    ScoredDocumentSource, ScoredInput, SourceContext, Value, SCORE_COLUMN,
};

pub fn facet_projection_fields(
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

pub struct FacetExecution<'a, S: Clone> {
    pub(super) fields: &'a [String],
    pub(super) source_schema: Vec<String>,
    pub(super) params: &'a [SQLParam],
    pub(super) ctes: &'a CteScope<S>,
    pub(super) output_mode: QueryOutputMode<'a>,
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub fn build_facet_output<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    table: &str,
    scored: ScoredInput,
    predicate: Option<ScalarExpr>,
    execution: FacetExecution<'a, S>,
) -> Result<QueryOutput, SQLError> {
    use crate::{
        AggregateKind, AggregateSpec, ExternalSort, Filter, HashAggregate, PhysicalOperator,
        PhysicalProjectSet, RowProjectionValue, RowSchema, SortKey,
    };

    let include_field = execution.fields.len() > 1;
    let table_state = context.scans.tables.table(table)?;
    let table_columns = table_state
        .column_definitions()
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
    .with_table_oid(crate::catalog::projection::table_relation_oid(
        &context.catalog,
        table,
    )?);
    let mut source: Box<dyn PhysicalOperator + '_> =
        Box::new(crate::TableScan::new(Box::new(source)));
    if let Some(predicate) = predicate {
        source = Box::new(Filter::with_evaluator(
            source,
            predicate,
            context
                .relational
                .evaluator(execution.params, execution.ctes),
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
            Box::new(move |document: crate::OwnedPhysicalRow| {
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
                Ok(Box::new(rows) as crate::PhysicalProjectRows)
            }),
        ));

    // End the document/evaluator borrow phase in a bounded spill. The generic
    // external aggregate can then own a static scan while its group map and
    // final ordering independently obey work_mem.
    let facet_input = crate::query::collection::collect_query_operator(
        context.relational.runtime,
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
    let work_mem = physical_work_mem_bytes(context.relational.runtime)?;
    let aggregate: Box<dyn PhysicalOperator + '_> = Box::new(HashAggregate::new_with_work_mem(
        Box::new(crate::SharedSpillScan::new(facet_input)),
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
        context
            .relational
            .evaluator(execution.params, execution.ctes),
        None,
        work_mem,
    ));
    let mut columns = facet_columns;
    columns.push("facet_count".into());
    crate::query::collection::collect_query_operator(
        context.relational.runtime,
        columns,
        sorted,
        execution.output_mode,
    )
}

pub fn order_by_references_field(stmt: &QueryBlockPlan) -> bool {
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
pub fn score_limited_text_filter(expr: Option<&ScalarExpr>) -> bool {
    let Some(ScalarExpr::Func { name, .. }) = expr else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "text_match" | "bayesian_match"
    )
}

pub fn score_order_top_k<S: Clone + 'static>(
    stmt: &QueryBlockPlan,
    context: &SourceContext<'_, S>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Option<usize>, SQLError> {
    if !stmt.locking.is_empty()
        || stmt.distinct
        || !stmt.distinct_on.is_empty()
        || !matches!(stmt.compute, ComputePlan::Project)
        || stmt.order_by.is_empty()
        || order_by_references_field(stmt)
        || stmt.order_by.iter().any(|order| !order.descending)
        || uqa_sql::semantics::aggregates::has_aggregate(
            context.relational.catalog,
            &stmt.projections,
        )
        || !stmt.group_by.is_empty()
        || !stmt.grouping_sets.is_empty()
    {
        return Ok(None);
    }
    resolve_score_slice_top_k(stmt, context, params, ctes)
}

/// Return the score prefix required by a score-first SQL slice. Secondary
/// sort keys are allowed because the caller retains every row tied at the
/// boundary score and leaves exact tie ordering to the relational pipeline.
pub fn post_retrieval_score_top_k<S: Clone + 'static>(
    stmt: &QueryBlockPlan,
    context: &SourceContext<'_, S>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
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
        || stmt.order_by.iter().any(|order| {
            order.expr.contains_window()
                || uqa_sql::semantics::aggregates::contains_aggregate(
                    context.relational.catalog,
                    &order.expr,
                )
        })
    {
        return Ok(None);
    }
    resolve_score_slice_top_k(stmt, context, params, ctes)
}

fn resolve_score_slice_top_k<S: Clone + 'static>(
    stmt: &QueryBlockPlan,
    context: &SourceContext<'_, S>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Option<usize>, SQLError> {
    if stmt.with_ties {
        return Ok(None);
    }
    if stmt.limit.iter().chain(stmt.offset.iter()).any(|expr| {
        uqa_sql::semantics::volatility::expr_contains_volatile_function(context.volatility, expr)
    }) {
        return Ok(None);
    }
    let Some(limit) = crate::query::relational::row_count::resolve_limit_offset_with_ctes(
        stmt.limit.as_ref(),
        context.relational,
        params,
        "LIMIT",
        ctes,
    )?
    else {
        return Ok(None);
    };
    let offset = crate::query::relational::row_count::resolve_limit_offset_with_ctes(
        stmt.offset.as_ref(),
        context.relational,
        params,
        "OFFSET",
        ctes,
    )?
    .unwrap_or(0);
    let requested = limit.checked_add(offset).ok_or_else(|| {
        SQLError::TypeMismatch("LIMIT plus OFFSET exceeds the u64 execution range".into())
    })?;
    let top_k = usize::try_from(requested).map_err(|_| {
        SQLError::TypeMismatch("LIMIT plus OFFSET exceeds the platform usize range".into())
    })?;
    Ok(Some(top_k))
}
