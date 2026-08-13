//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE materialization, EXPLAIN, VALUES, and SELECT-without-FROM execution.

use super::*;

pub(in crate::sql) fn materialize_plan_ctes(
    engine: &Engine,
    plans: &[CtePlan],
    params: &[SQLParam],
    ctes: &mut CteScope,
) -> Result<(), SQLError> {
    materialize_plan_ctes_with_filters(engine, plans, params, ctes, &BTreeMap::new())
}

pub(in crate::sql) fn materialize_plan_ctes_with_filters(
    engine: &Engine,
    plans: &[CtePlan],
    params: &[SQLParam],
    ctes: &mut CteScope,
    output_filters: &BTreeMap<String, (String, ScalarExpr)>,
) -> Result<(), SQLError> {
    for plan in plans {
        if plan.recursive {
            let rows = materialize_recursive_cte(
                engine,
                plan,
                params,
                ctes,
                output_filters.get(&plan.name),
            )?;
            ctes.insert_shared(plan.name.clone(), rows);
            continue;
        }

        let result = execute_query_plan_output(
            engine,
            &plan.query,
            params,
            ctes,
            QueryOutputMode::SharedSpill,
        )?;
        let mut columns = result.columns.clone();
        let source_columns = result.internal_columns.clone();
        let mut operator = result.into_operator();
        if !plan.columns.is_empty() {
            let renamed_columns = columns
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    plan.columns
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| source.clone())
                })
                .collect::<Vec<_>>();
            let mapping = source_columns
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    let output = if is_score_provenance_column(source) {
                        source.clone()
                    } else {
                        renamed_columns
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| source.clone())
                    };
                    (output, source.clone())
                })
                .collect();
            columns = renamed_columns;
            operator = Box::new(uqa_execution::ColumnSelection::with_mapping(
                operator, mapping,
            ));
        }
        let materialized =
            collect_query_operator(engine, columns, operator, QueryOutputMode::SharedSpill)?;
        let QueryRows::SharedSpill(materialized) = materialized.rows else {
            return Err(SQLError::Internal(
                "CTE spill collector returned in-memory rows".into(),
            ));
        };
        ctes.insert_shared(plan.name.clone(), materialized);
    }
    Ok(())
}

/// Render the inner statement as an EXPLAIN-style, single-column `plan`
/// result with one row per line.
pub(in crate::sql) struct ExplainAnalysis {
    pub(in crate::sql) elapsed: std::time::Duration,
    pub(in crate::sql) rows: u64,
    pub(in crate::sql) affected_rows: u64,
}

pub(in crate::sql) fn run_explain(
    body: &UnifiedPlan,
    verbose: bool,
    format: Option<&str>,
    analysis: Option<&ExplainAnalysis>,
) -> Result<SQLResult, SQLError> {
    let mut plan_text = match body {
        UnifiedPlan::Query(query) => format_query_plan(query),
        UnifiedPlan::Command(command) => format!("{}\n  {command:#?}", command.name()),
    };
    if verbose {
        plan_text.push_str("\n  verbose=true");
        write!(plan_text, "\n  physical_plan={body:#?}")
            .map_err(|error| SQLError::Internal(format!("format EXPLAIN plan: {error}")))?;
    }
    if let Some(analysis) = analysis {
        let _ = write!(
            plan_text,
            "\n  actual_rows={}\n  affected_rows={}\n  execution_time_ms={:.3}",
            analysis.rows,
            analysis.affected_rows,
            analysis.elapsed.as_secs_f64() * 1_000.0
        );
    }

    let format = format.unwrap_or("text").to_ascii_lowercase();
    if format == "json" {
        let payload = serde_json::json!({
            "Plan": plan_text.lines().collect::<Vec<_>>(),
            "Analyze": analysis.is_some(),
            "Actual Rows": analysis.map(|value| value.rows),
            "Affected Rows": analysis.map(|value| value.affected_rows),
            "Execution Time (ms)": analysis.map(|value| value.elapsed.as_secs_f64() * 1_000.0),
        });
        let mut row = ResultRow::new();
        row.insert("plan".to_string(), Value::Str(payload.to_string()));
        return Ok(SQLResult {
            columns: vec!["plan".to_string()],
            rows: vec![row],
            positional_rows: None,
            affected_rows: 0,
        });
    }
    if format != "text" {
        return Err(SQLError::Unsupported(format!(
            "EXPLAIN format `{format}` is not supported; expected TEXT or JSON"
        )));
    }
    let mut rows: Vec<ResultRow> = Vec::new();
    for line in plan_text.split('\n') {
        let mut r = ResultRow::new();
        r.insert("plan".to_string(), Value::Str(line.to_string()));
        rows.push(r);
    }
    Ok(SQLResult {
        columns: vec!["plan".to_string()],
        rows,
        positional_rows: None,
        affected_rows: 0,
    })
}

pub(in crate::sql) fn format_query_plan(plan: &QueryPlan) -> String {
    match &plan.root {
        RelationalPlan::QueryBlock(block) => format_select_plan(block),
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => format!(
            "SetOp\n  kind={kind:?}\n  all={all}\n  left=({})\n  right=({})\n  order_by={}\n  limit={}\n  offset={}",
            format_query_plan(left).replace('\n', "\n    "),
            format_query_plan(right).replace('\n', "\n    "),
            order_by.len(),
            limit
                .as_deref()
                .map_or_else(|| "none".into(), explain_int_expr),
            offset
                .as_deref()
                .map_or_else(|| "none".into(), explain_int_expr),
        ),
        RelationalPlan::Values { rows, .. } => format!("Values\n  rows={}", rows.len()),
    }
}

pub(in crate::sql) fn format_select_plan(stmt: &QueryBlockPlan) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(s, "Select");
    if !stmt.projections.is_empty() {
        let _ = writeln!(s, "  projections={}", stmt.projections.len());
    }
    if let Some(from) = &stmt.from {
        let _ = writeln!(s, "  from={from:?}");
    }
    if stmt.r#where.is_some() {
        let _ = writeln!(s, "  where=<expr>");
    }
    if !stmt.group_by.is_empty() {
        let _ = writeln!(s, "  group_by={}", stmt.group_by.len());
    }
    if !stmt.grouping_sets.is_empty() {
        let _ = writeln!(s, "  grouping_sets={}", stmt.grouping_sets.len());
    }
    if !stmt.order_by.is_empty() {
        let _ = writeln!(s, "  order_by={}", stmt.order_by.len());
    }
    if let Some(expr) = stmt.limit.as_ref() {
        let _ = writeln!(s, "  limit={}", explain_int_expr(expr));
    }
    if let Some(expr) = stmt.offset.as_ref() {
        let _ = writeln!(s, "  offset={}", explain_int_expr(expr));
    }
    if stmt.distinct {
        let _ = writeln!(s, "  distinct=true");
    }
    s.trim_end().to_string()
}

pub(in crate::sql) fn should_defer_distinct_limit(stmt: &QueryBlockPlan) -> bool {
    stmt.distinct && (stmt.limit.is_some() || stmt.offset.is_some())
}

pub(in crate::sql) fn select_execution_stmt(
    stmt: &QueryBlockPlan,
    defer_distinct_limit: bool,
) -> QueryBlockPlan {
    if !defer_distinct_limit {
        return stmt.clone();
    }
    let mut exec_stmt = stmt.clone();
    exec_stmt.limit = None;
    exec_stmt.offset = None;
    exec_stmt
}

pub(in crate::sql) fn run_select_without_from_output(
    engine: &Engine,
    original: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let row = ResultRow::new();
    let columns = projection_columns(&stmt.projections);
    let operator: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::TableScan::from_rows(Vec::new(), vec![row]));
    execute_query_block_operator_output(
        engine,
        operator,
        stmt.r#where.clone(),
        stmt,
        original,
        params,
        ctes,
        columns,
        output_mode,
    )
}
