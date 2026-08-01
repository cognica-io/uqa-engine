//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE spill materialization and scoped cache restoration.

use super::{
    build_join_operator_with_ctes, eval_scalar, prefix_row, query_output_shared, BTreeSet,
    CteScope, Engine, PhysicalSubqueryRunner, PlanSubqueryArena, QueryOutputMode, QueryPlan,
    RelationalPlan, ResultRow, SQLError, SQLParam, ScalarEvalContext, ScalarExpr, SourcePlan,
};

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
    let operator = build_join_operator_with_ctes(engine, from, params, ctes, None, None)?;
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
) -> Vec<(String, Option<uqa_execution::SharedSpill>)> {
    names
        .iter()
        .map(|name| (name.clone(), ctes.remove_materialized(name)))
        .collect()
}

pub(in crate::sql) fn restore_cte_names(
    ctes: &mut CteScope,
    saved: Vec<(String, Option<uqa_execution::SharedSpill>)>,
) {
    for (name, rows) in saved {
        match rows {
            Some(rows) => {
                ctes.rows.insert(name.clone(), rows);
            }
            None => {
                ctes.remove_materialized(&name);
            }
        }
    }
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
        SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => {}
    }
}

pub(in crate::sql) fn build_values_rows(
    rows: &[Vec<ScalarExpr>],
    alias: Option<&str>,
    column_aliases: &[String],
    params: &[SQLParam],
    eval_hook: &dyn uqa_sql::expr::EngineHook,
    subquery_runner: &dyn PhysicalSubqueryRunner,
    subqueries: &[QueryPlan],
) -> Result<Vec<ResultRow>, SQLError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let n_cols = rows[0].len();
    let columns: Vec<String> = (0..n_cols)
        .map(|i| {
            column_aliases
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("column{}", i + 1))
        })
        .collect();
    let subquery_arena = PlanSubqueryArena::new(subqueries, Some(subquery_runner));
    let ctx = ScalarEvalContext::new(None, params)
        .with_function_hook(eval_hook)
        .with_subquery_runner(&subquery_arena);
    let mut out: Vec<ResultRow> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut r = ResultRow::new();
        for (i, expr) in row.iter().enumerate() {
            let v = eval_scalar(expr, &ctx)?;
            r.insert(columns[i].clone(), v);
        }
        let r = match alias {
            Some(a) => prefix_row(a, &r),
            None => r,
        };
        out.push(r);
    }
    Ok(out)
}
