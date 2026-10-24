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

pub use uqa_sql::result::ExplainAnalysis;
use uqa_sql::result::ExplainPhysicalPlan;

mod formats;
mod physical;
#[cfg(test)]
mod tests;
mod vector;
pub use physical::{physical_plan, PhysicalExplainContext};

fn render_generation(generation: uqa_core::VectorGeneration) -> serde_json::Value {
    let mut database = String::with_capacity(32);
    for byte in generation.database {
        let _ = write!(database, "{byte:02x}");
    }
    serde_json::json!({"Database": database, "Table": generation.table, "Index": generation.index, "Generation": generation.generation})
}

pub fn run_explain(
    body: &UnifiedPlan,
    verbose: bool,
    format: Option<&str>,
    analysis: Option<&ExplainAnalysis>,
) -> Result<SQLResult, SQLError> {
    use uqa_sql::explain::{ExplainOption, ExplainOptionValue, ExplainOptions};
    let mut options = vec![
        ExplainOption {
            name: "verbose".into(),
            value: Some(ExplainOptionValue::Boolean(verbose)),
        },
        ExplainOption {
            name: "analyze".into(),
            value: Some(ExplainOptionValue::Boolean(analysis.is_some())),
        },
    ];
    if let Some(format) = format {
        options.push(ExplainOption {
            name: "format".into(),
            value: Some(ExplainOptionValue::String(format.into())),
        });
    }
    run_explain_with_physical(
        body,
        &ExplainOptions { options }.interpret()?,
        analysis,
        &ExplainPhysicalPlan::default(),
    )
}

pub fn run_explain_with_physical(
    body: &UnifiedPlan,
    settings: &uqa_sql::explain::ExplainSettings,
    analysis: Option<&ExplainAnalysis>,
    physical: &ExplainPhysicalPlan,
) -> Result<SQLResult, SQLError> {
    use uqa_sql::explain::ExplainFormat;
    let utility = utility_description(body);
    let plan_text = utility.map_or_else(
        || plan_text(body, settings, analysis, physical),
        |(_, text)| Ok(text.to_string()),
    )?;
    let (ty, values) = if settings.format == ExplainFormat::Text {
        (
            uqa_sql::ColumnType::Text,
            plan_text
                .lines()
                .map(|line| Value::Str(line.into()))
                .collect(),
        )
    } else {
        let payload = utility.map_or_else(
            || structured_plan(body, &plan_text, settings, analysis, physical),
            |(label, _)| serde_json::Value::String(label.into()),
        );
        let (ty, value) = match settings.format {
            ExplainFormat::Json => (
                uqa_sql::ColumnType::Json,
                Value::Json(
                    serde_json::to_string_pretty(&vec![&payload])
                        .map_err(|error| SQLError::Internal(error.to_string()))?,
                ),
            ),
            ExplainFormat::Xml => (
                uqa_sql::ColumnType::Xml,
                Value::Str(if let Some((label, _)) = utility {
                    formats::xml_utility(label)
                } else {
                    formats::xml(&payload)?
                }),
            ),
            ExplainFormat::Yaml => (
                uqa_sql::ColumnType::Text,
                Value::Str(formats::yaml(&payload)?),
            ),
            ExplainFormat::Text => unreachable!(),
        };
        (ty, vec![value])
    };
    let rows = values
        .into_iter()
        .map(|value| {
            let mut row = ResultRow::new();
            row.insert("QUERY PLAN".into(), value);
            row
        })
        .collect();
    Ok(SQLResult {
        kind: uqa_sql::SQLResultKind::Rows,
        command_tag: None,
        row_schema: None,
        columns: vec!["QUERY PLAN".into()],
        column_types: vec![Some(ty)],
        rows,
        positional_rows: None,
        affected_rows: 0,
    })
}

fn utility_description(body: &UnifiedPlan) -> Option<(&'static str, &'static str)> {
    use uqa_sql::plan::CommandPlan;
    let UnifiedPlan::Command(command) = body else {
        return None;
    };
    match command.as_ref() {
        CommandPlan::Insert(_)
        | CommandPlan::Update(_)
        | CommandPlan::Delete(_)
        | CommandPlan::Merge(_) => None,
        CommandPlan::CreateTableAs { .. } => Some(("CREATE TABLE AS", "")),
        CommandPlan::CreateMaterializedView { .. } => Some(("CREATE MATERIALIZED VIEW", "")),
        CommandPlan::Notify { .. } => Some(("Notify", "NOTIFY")),
        _ => Some((
            "Utility Statement",
            "Utility statements have no plan structure",
        )),
    }
}

fn plan_text(
    body: &UnifiedPlan,
    settings: &uqa_sql::explain::ExplainSettings,
    analysis: Option<&ExplainAnalysis>,
    physical: &ExplainPhysicalPlan,
) -> Result<String, SQLError> {
    let mut text = match body {
        UnifiedPlan::Query(query) => format_query_plan(query),
        UnifiedPlan::Command(command) => format!("{}\n  {command:#?}", command.name()),
    };
    if settings.verbose {
        write!(text, "\n  verbose=true\n  physical_plan={body:#?}")
            .map_err(|error| SQLError::Internal(error.to_string()))?;
    }
    if settings.format != uqa_sql::explain::ExplainFormat::Text {
        return Ok(text);
    }
    for node in &physical.nodes {
        let rendered = serde_json::to_string_pretty(node)
            .map_err(|error| SQLError::Internal(error.to_string()))?;
        for line in rendered.lines() {
            let _ = write!(text, "\n  {line}");
        }
    }
    if let Some(analysis) = analysis {
        vector::append_text(&mut text, analysis)?;
        let _ = write!(
            text,
            "\n  actual_rows={}\n  affected_rows={}",
            analysis.rows, analysis.affected_rows
        );
        if settings.summary {
            let _ = write!(
                text,
                "\n  execution_time_ms={:.3}",
                analysis.elapsed.as_secs_f64() * 1_000.0
            );
        }
    }
    Ok(text)
}

fn structured_plan(
    body: &UnifiedPlan,
    text: &str,
    settings: &uqa_sql::explain::ExplainSettings,
    analysis: Option<&ExplainAnalysis>,
    physical: &ExplainPhysicalPlan,
) -> serde_json::Value {
    let node = match body {
        UnifiedPlan::Query(query) => match &query.root {
            RelationalPlan::QueryBlock(_) => "Select",
            RelationalPlan::SetOp { .. } => "SetOp",
            RelationalPlan::Values { .. } => "Values",
        },
        UnifiedPlan::Command(command) => command.name(),
    };
    let mut plan =
        serde_json::json!({"Node Type": node, "UQA Plan": text.lines().collect::<Vec<_>>()});
    if let Some(analysis) = analysis {
        plan["Actual Rows"] = analysis.rows.into();
        plan["Affected Rows"] = analysis.affected_rows.into();
    }
    let mut payload = serde_json::json!({"Plan": plan});
    if let Some(analysis) = analysis {
        payload["Vector Searches"] = serde_json::Value::Array(
            analysis
                .vector_searches
                .iter()
                .map(|search| vector::record(search))
                .collect(),
        );
        if settings.summary {
            payload["Execution Time"] = (analysis.elapsed.as_secs_f64() * 1_000.0).into();
        }
    }
    if !physical.nodes.is_empty() {
        payload["Physical Plans"] = serde_json::Value::Array(physical.nodes.clone());
    }
    payload
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
