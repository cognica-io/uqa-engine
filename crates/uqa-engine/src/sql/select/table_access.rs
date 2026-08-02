//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-table access-path execution.

use super::{
    build_facet_output, column_prune_for_stmt, execute_function, execute_function_with_top_k,
    execute_mixed_where, execute_query_block_operator_output, expand_star_columns,
    expr_contains_jsonpath_fts_match, expr_is_jsonpath_fts_match, facet_projection_fields,
    projection_columns, score_limited_text_filter, score_order_top_k, AccessPathPlan, ComputePlan,
    CteScope, Engine, FacetExecution, QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError,
    SQLParam, ScalarExpr, ScoredDocumentSource, ScoredInput,
};

pub(in crate::sql) fn run_single_table_select_output(
    engine: &Engine,
    table: &str,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    if let Some(filter) = stmt.r#where.as_ref() {
        crate::sql::validate_expr_text_match_fields(engine, table, filter)?;
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
        crate::operator_tree_bridge::run_accelerated(engine, table, stmt.r#where.as_ref(), params)?
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
    let source_schema = stmt
        .from
        .as_ref()
        .and_then(|source| column_prune_for_stmt(engine, stmt, source))
        .and_then(|prune| prune.get(table).cloned())
        .map(|columns| columns.into_iter().collect())
        .map_or_else(
            || {
                engine.try_table_columns(table).map_err(|error| {
                    SQLError::Internal(format!("read table columns for `{table}`: {error}"))
                })
            },
            Ok,
        )?;
    let ordered_primary_key = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read table schema for `{table}`: {error}")))?
        .and_then(|columns| {
            columns
                .into_iter()
                .find(|column| {
                    column.primary_key && matches!(column.ty, uqa_sql::ast::ColumnType::Integer)
                })
                .map(|column| column.name)
        });
    let pushed_predicate = physical_filter
        .as_ref()
        .map(|predicate| {
            uqa_execution::ProjectedPredicate::compile(predicate, &source_schema, params)
        })
        .transpose()?
        .flatten();
    if pushed_predicate.is_some() {
        physical_filter = None;
    }
    let source = ScoredDocumentSource::new(
        table,
        table_state,
        scored,
        source_schema,
        ordered_primary_key,
        pushed_predicate,
    );
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
