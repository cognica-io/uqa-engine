//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explain result rendering and query-block limit placement.

use std::fmt::Write as _;
use uqa_core::Value;
use uqa_sql::{
    plan::{QueryBlockPlan, QueryPlan, RelationalPlan, UnifiedPlan},
    ResultRow, SQLError, SQLResult, ScalarExpr,
};

pub struct ExplainAnalysis {
    pub elapsed: std::time::Duration,
    pub rows: u64,
    pub affected_rows: u64,
}

pub fn run_explain(
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
            kind: uqa_sql::SQLResultKind::Rows,
            command_tag: None,
            columns: vec!["plan".to_string()],
            column_types: vec![Some(uqa_sql::ColumnType::Text)],
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
        kind: uqa_sql::SQLResultKind::Rows,
        command_tag: None,
        columns: vec!["plan".to_string()],
        column_types: vec![Some(uqa_sql::ColumnType::Text)],
        rows,
        positional_rows: None,
        affected_rows: 0,
    })
}

pub fn format_query_plan(plan: &QueryPlan) -> String {
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

pub fn format_select_plan(stmt: &QueryBlockPlan) -> String {
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
    if !stmt.locking.is_empty() {
        let _ = writeln!(s, "  locking={}", stmt.locking.len());
    }
    s.trim_end().to_string()
}

pub use uqa_sql::semantics::{select_execution_stmt, should_defer_distinct_limit};

pub fn explain_int_expr(expr: &ScalarExpr) -> String {
    match expr {
        ScalarExpr::Literal(Value::Int(n)) => n.to_string(),
        _ => "<expr>".to_string(),
    }
}
