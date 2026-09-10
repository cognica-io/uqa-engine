//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-table access-path execution.

use super::{
    build_facet_output, combine_filter_parts, execute_mixed_where,
    execute_query_block_operator_output, expand_from_star_columns,
    expr_contains_jsonpath_fts_match, expr_is_jsonpath_fts_match, facet_projection_fields,
    flatten_and_filter_parts, post_retrieval_score_top_k, projection_columns,
    score_limited_text_filter, score_order_top_k, AccessPathPlan, CteScope, FacetExecution,
    QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError, SQLParam, ScalarExpr,
    ScoredDocumentSource, ScoredInput, SingleRelation, SourceContext, SourceProjection,
    TABLE_OID_COLUMN,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub fn run_single_table_select_output<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    relation: SingleRelation<'_>,
    block: &'a QueryBlockPlan,
    stmt: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
    output_mode: QueryOutputMode<'a>,
) -> Result<QueryOutput, SQLError> {
    let SingleRelation {
        reference_name,
        relation_name: table,
        qualifier,
    } = relation;
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    let table_snapshot = catalog
        .table_resolved(&resolution, table)?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let has_stored_score_column = table_snapshot
        .columns
        .iter()
        .any(|column| column.name == super::SCORE_COLUMN);
    let score_top_k = if !has_stored_score_column
        && matches!(
            block.access,
            AccessPathPlan::OperatorTree {
                score_limit_pushdown: true
            }
        ) {
        score_order_top_k(stmt, context, params, ctes)?
            .filter(|_| score_limited_text_filter(stmt.r#where.as_ref()))
    } else {
        None
    };
    let post_retrieval_top_k = if has_stored_score_column {
        None
    } else {
        post_retrieval_score_top_k(stmt, context, params, ctes)?
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
        Some(context.relation_retrieval.function(
            table,
            reference_name,
            name,
            args,
            params,
            Some(top_k),
        )?)
    } else {
        context.relation_retrieval.accelerated(
            table,
            reference_name,
            stmt.r#where.as_ref(),
            params,
        )?
    };
    let score_bearing_filter = stmt
        .r#where
        .as_ref()
        .is_some_and(uqa_sql::semantics::contains_retrieval);
    let (mut scored, mut physical_filter) = if let Some(rows) = optimised {
        (ScoredInput::entries(rows, score_bearing_filter), None)
    } else {
        match &block.access {
            AccessPathPlan::Row => (ScoredInput::All, stmt.r#where.clone()),
            AccessPathPlan::Hybrid => {
                let rows = match stmt.r#where.as_ref() {
                    Some(filter) => ScoredInput::entries(
                        execute_mixed_where(
                            context,
                            table,
                            reference_name,
                            qualifier,
                            filter,
                            params,
                            ctes,
                        )?,
                        uqa_sql::semantics::contains_retrieval(filter),
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
                            context.relation_retrieval.function(
                                table,
                                reference_name,
                                name,
                                args,
                                params,
                                None,
                            )?,
                            uqa_sql::semantics::contains_retrieval(filter_expr),
                        )
                    }
                    // The planner may optimistically choose the operator-tree
                    // access class for a predicate that the posting-list IR
                    // cannot represent (for example `IS NULL`, arithmetic, or
                    // a subquery). Keep it inside the same physical query
                    // pipeline as a relational Filter over the table scan.
                    Some(_) | None => ScoredInput::All,
                };
                let filter = matches!(rows, ScoredInput::All)
                    .then(|| stmt.r#where.clone())
                    .flatten();
                (rows, filter)
            }
        }
    };

    let source_projection = if let Some(source) = stmt.from.as_ref() {
        context
            .planning
            .column_prune_with_filter(stmt, source, physical_filter.as_ref(), ctes)?
            .and_then(|prune| prune.get(qualifier).cloned())
    } else {
        None
    };
    let metadata_projection = source_projection
        .as_ref()
        .map(SourceProjection::metadata)
        .unwrap_or_default();
    let bound_columns = match stmt.from.as_ref() {
        Some(uqa_sql::plan::SourcePlan::Table { bound_columns, .. }) => bound_columns.as_deref(),
        _ => None,
    };
    let table_columns = uqa_sql::semantics::bound_source_column_names(
        table_snapshot
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        bound_columns,
    )?;
    let source_schema: Vec<String> = source_projection
        .and_then(SourceProjection::explicit_columns)
        .map_or_else(|| table_columns, |columns| columns.into_iter().collect());

    if let Some(facet_fields) = facet_projection_fields(&stmt.projections)? {
        let execution = FacetExecution {
            fields: &facet_fields,
            source_schema,
            params,
            ctes,
            output_mode,
        };
        return build_facet_output(context, table, scored, physical_filter.take(), execution);
    }

    let table_state = context.scans.tables.table(table)?;
    let ordered_primary_key = table_snapshot
        .columns
        .iter()
        .find(|column| column.primary_key && column.ty.is_integer())
        .map(|column| column.name.clone());
    let predicate_schema = crate::RowSchema::with_qualified_types(
        qualifier,
        source_schema.clone(),
        source_schema
            .iter()
            .map(|name| {
                table_snapshot
                    .columns
                    .iter()
                    .find(|column| column.name == *name)
                    .map(|column| column.ty.clone())
            })
            .collect(),
    );
    let (pushed_predicate, residual_filter) =
        split_projected_filter(physical_filter.take(), &predicate_schema, params)?;
    physical_filter = residual_filter;
    if pushed_predicate.is_none() && physical_filter.is_none() {
        if let Some(top_k) = post_retrieval_top_k {
            scored.retain_top_scores_with_ties(top_k);
        }
    }
    let lock_origin = if ctes.lock_identities.emit {
        let storage_name = catalog
            .table_name_resolved(&resolution, table)?
            .unwrap_or_else(|| table.to_string());
        Some((
            std::sync::Arc::<str>::from(qualifier),
            std::sync::Arc::<str>::from(storage_name),
        ))
    } else {
        None
    };
    let recheck_pins = lock_origin
        .as_ref()
        .and_then(|(origin_qualifier, storage_name)| {
            ctes.recheck_docs_for_scan(origin_qualifier, storage_name)
        });
    let source = ScoredDocumentSource::new_with_metadata(
        table,
        table_state,
        scored,
        source_schema,
        ordered_primary_key,
        pushed_predicate,
        metadata_projection,
    )
    .with_table_oid(crate::catalog::projection::snapshot_table_relation_oid(
        &catalog,
        &resolution,
        table,
    )?)
    .with_qualifier(qualifier)
    .with_lock_origin(lock_origin)
    .with_recheck_pins(recheck_pins);
    let source: Box<dyn crate::PhysicalOperator + '_> =
        Box::new(crate::TableScan::new(Box::new(source)));
    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        &predicate_schema,
    )?;
    execute_query_block_operator_output(
        context.relational,
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

/// Compile every independently supported top-level conjunct into the storage
/// projection. A subquery or another unsupported residual must not force
/// otherwise positional predicates back through the row scalar evaluator.
fn split_projected_filter(
    predicate: Option<ScalarExpr>,
    source_schema: &crate::RowSchema,
    params: &[SQLParam],
) -> Result<(Option<crate::ProjectedPredicate>, Option<ScalarExpr>), SQLError> {
    let Some(predicate) = predicate else {
        return Ok((None, None));
    };
    if expression_references_tableoid(&predicate) {
        return Ok((None, Some(predicate)));
    }
    if let Some(compiled) =
        crate::ProjectedPredicate::compile_with_schema(&predicate, source_schema, params)?
    {
        return Ok((Some(compiled), None));
    }
    if !matches!(predicate, ScalarExpr::And(_)) {
        return Ok((None, Some(predicate)));
    }

    let mut projected = Vec::new();
    let mut residual = Vec::new();
    for conjunct in flatten_and_filter_parts(&predicate) {
        if !expression_references_tableoid(conjunct)
            && crate::ProjectedPredicate::compile_with_schema(conjunct, source_schema, params)?
                .is_some()
        {
            projected.push(conjunct.clone());
        } else {
            residual.push(conjunct.clone());
        }
    }
    let projected = match combine_filter_parts(projected) {
        Some(expression) => Some(
            crate::ProjectedPredicate::compile_with_schema(&expression, source_schema, params)?
                .ok_or_else(|| {
                    SQLError::Internal(
                        "individually compiled projected predicates could not be combined".into(),
                    )
                })?,
        ),
        None => None,
    };
    Ok((projected, combine_filter_parts(residual)))
}

fn expression_references_tableoid(expression: &ScalarExpr) -> bool {
    let mut columns = std::collections::BTreeSet::new();
    expression.collect_columns(&mut columns) && columns.contains(TABLE_OID_COLUMN)
}
