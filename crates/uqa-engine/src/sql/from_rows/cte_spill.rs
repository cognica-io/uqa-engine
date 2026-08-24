//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE spill materialization and scoped cache restoration.

use super::{
    build_join_operator_with_ctes, eval_scalar, query_output_shared, BTreeSet, CteScope, Engine,
    PlanSubqueryArena, QueryOutputMode, QueryPlan, RelationalPlan, SQLError, SQLParam,
    ScalarEvalContext, ScalarExpr, SourceEvalContext, SourcePlan,
};
use uqa_planner::CtePlan;

/// Materialize a repeatable FROM input under the session work-memory budget.
/// DML statements may need to rescan their source for each target row; the
/// shared spill keeps that requirement without retaining the full source in a
/// cardinality-sized vector.
pub(in crate::sql) fn build_join_spill_with_ctes(
    engine: &Engine,
    from: &SourcePlan,
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<uqa_execution::SharedSpill, SQLError> {
    let mut bound = from.clone();
    crate::sql::select::bind_source_plan_schema_for_execution(
        engine, &mut bound, params, ctes, None,
    )?;
    let operator = build_join_operator_with_ctes(engine, &bound, params, ctes, None, None)?;
    let columns = operator.schema().to_vec();
    let output = crate::sql::select::collect_query_operator(
        engine,
        columns,
        operator,
        QueryOutputMode::SharedSpill,
    )?;
    query_output_shared(output, "DML FROM")
}

pub(in crate::sql) fn save_and_remove_cte_names(
    ctes: &mut CteScope,
    names: &BTreeSet<String>,
) -> Vec<SavedCteBinding> {
    names
        .iter()
        .map(|name| SavedCteBinding {
            name: name.clone(),
            rows: ctes.remove_materialized(name),
            deferred: ctes.remove_deferred(name),
        })
        .collect()
}

pub(in crate::sql) fn restore_cte_names(ctes: &mut CteScope, saved: Vec<SavedCteBinding>) {
    for binding in saved {
        ctes.remove_materialized(&binding.name);
        ctes.remove_deferred(&binding.name);
        if let Some(rows) = binding.rows {
            ctes.insert_shared(binding.name, rows);
        } else if let Some(plan) = binding.deferred {
            ctes.insert_deferred(plan);
        }
    }
}

pub(in crate::sql) struct SavedCteBinding {
    name: String,
    rows: Option<uqa_execution::SharedSpill>,
    deferred: Option<CtePlan>,
}

pub(in crate::sql) fn query_cte_names(plan: &QueryPlan) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_query_cte_names(plan, &mut names);
    names
}

pub(in crate::sql) fn collect_query_cte_names(plan: &QueryPlan, names: &mut BTreeSet<String>) {
    for cte in &plan.ctes {
        names.insert(cte.name.clone());
        collect_query_cte_names(&cte.query, names);
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_source_query_cte_names(source, names);
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            collect_query_cte_names(left, names);
            collect_query_cte_names(right, names);
        }
        RelationalPlan::Values { .. } => {}
    }
}

pub(in crate::sql) fn collect_source_query_cte_names(
    source: &SourcePlan,
    names: &mut BTreeSet<String>,
) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            collect_source_query_cte_names(left, names);
            collect_source_query_cte_names(right, names);
        }
        SourcePlan::Subquery { body, .. } => collect_query_cte_names(body, names),
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

pub(in crate::sql) fn build_values_physical_rows(
    context: &SourceEvalContext<'_>,
    rows: &[Vec<ScalarExpr>],
    column_types: &[Option<uqa_sql::ast::ColumnType>],
) -> Result<Vec<uqa_execution::PhysicalRow>, SQLError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let subquery_arena = PlanSubqueryArena::new(context.subqueries, Some(context.subquery_runner));
    let ctx = ScalarEvalContext::new(None, context.params)
        .with_function_hook(context.eval_hook)
        .with_subquery_runner(&subquery_arena);
    let empty_schema = uqa_execution::RowSchema::default();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut values = Vec::with_capacity(row.len());
        for (i, expr) in row.iter().enumerate() {
            let source_type = uqa_execution::common_context_expression_type(
                expr,
                &empty_schema,
                context.params,
                Some(context.engine),
            )?;
            let v = crate::sql::select::coerce_common_context_value(
                eval_scalar(expr, &ctx)?,
                source_type.as_ref(),
                column_types.get(i).and_then(Option::as_ref),
            )?;
            values.push(v);
        }
        out.push(uqa_execution::PhysicalRow::from_values(values));
    }
    Ok(out)
}
