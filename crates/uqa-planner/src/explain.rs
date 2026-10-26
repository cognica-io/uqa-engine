//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explain result rendering and query-block limit placement.

use std::fmt::Write as _;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionString, ProductionVec},
    Value,
};
use uqa_sql::{
    plan::{QueryBlockPlan, QueryPlan, RelationalPlan, UnifiedPlan},
    SQLError, SQLResult, ScalarExpr,
};

pub use uqa_sql::result::ExplainAnalysis;
use uqa_sql::result::ExplainPhysicalPlan;

mod costs;
mod estimates;
mod executions;
mod formats;
mod measurements;
pub use costs::estimate_plan;
mod physical;
mod properties;
mod serialization;
#[cfg(test)]
mod tests;
mod text;
mod vector;
pub use physical::{physical_plan, physical_plan_with_control, PhysicalExplainContext};

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
    run_explain_with_control(
        body,
        settings,
        analysis,
        physical,
        &ProductionControl::uncontrolled(),
    )
}

/// Formatting borrows captured diagnostics and retains every constructed text/container lease until the legacy SQL result handoff.
pub fn run_explain_with_control(
    body: &UnifiedPlan,
    settings: &uqa_sql::explain::ExplainSettings,
    analysis: Option<&ExplainAnalysis>,
    physical: &ExplainPhysicalPlan,
    control: &ProductionControl<'_>,
) -> Result<SQLResult, SQLError> {
    use uqa_sql::explain::ExplainFormat;
    control.check()?;
    let utility = utility_description(body);
    let plan_text = match utility {
        Some((_, text)) => control.copy_text(text)?,
        None => plan_text(body, settings, analysis, physical, *control)?,
    };
    let mut cells = ProductionVec::new(*control);
    let ty = if settings.format == ExplainFormat::Text {
        for line in plan_text.lines() {
            cells.push_produced(result_cell(control.copy_text(line)?, false, control)?)?;
        }
        uqa_sql::ColumnType::Text
    } else {
        let payload = match utility {
            Some((label, _)) => properties::Property::Text(label),
            None => structured_plan(body, &plan_text, settings, analysis, physical, *control)?,
        };
        cells.push_produced(result_cell(
            formats::render(
                &payload,
                settings.format,
                utility.map(|(label, _)| label),
                *control,
            )?,
            settings.format == ExplainFormat::Json,
            control,
        )?)?;
        match settings.format {
            ExplainFormat::Json => uqa_sql::ColumnType::Json,
            ExplainFormat::Xml => uqa_sql::ColumnType::Xml,
            _ => uqa_sql::ColumnType::Text,
        }
    };
    let cells = cells.finish()?;
    drop(plan_text);
    let result = SQLResult::from_column_with_control("QUERY PLAN", &ty, cells, control)?;
    // This is the existing ordinary SQL result handoff. All cell and metadata construction stays admitted through the move; the receiver owns subsequent result retention.
    let (result, memory) = result.into_parts();
    drop(memory);
    Ok(result)
}

fn result_cell(
    text: Produced<String>,
    json: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let (text, memory) = text.into_parts();
    Ok(control.finish(
        if json {
            Value::Json(text)
        } else {
            Value::Str(text)
        },
        memory,
    )?)
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
    control: ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let base = match body {
        UnifiedPlan::Query(query) => text::query(query, control)?,
        UnifiedPlan::Command(command) => {
            control.format(format_args!("{}\n  {command:#?}", command.name()))?
        }
    };
    let mut text = ProductionString::from_produced(base, control)?;
    if settings.verbose {
        let _ = write!(text, "\n  verbose=true\n  physical_plan={body:#?}");
    }
    if settings.format != uqa_sql::explain::ExplainFormat::Text {
        return Ok(text.finish()?);
    }
    if let Some(estimate) = physical.estimate.filter(|_| settings.costs) {
        let _ = write!(
            text,
            "\n  cost={:.2}..{:.2} rows={:.0}",
            estimate.startup, estimate.total, estimate.rows
        );
        if let Some(width) = estimate.width {
            let _ = write!(text, " width={width}");
        }
        estimates::append_text(&mut text, &physical.estimates, control)?;
    }
    for node in &physical.nodes {
        let rendered = formats::json(&properties::Property::Captured(node), control)?;
        for line in rendered.lines() {
            text.push_str("\n  ")?;
            text.push_str(line)?;
        }
    }
    if settings.settings && !physical.settings.is_empty() {
        text.push_str("\nSettings: ")?;
        for (index, (name, value)) in physical.settings.iter().enumerate() {
            if index != 0 {
                text.push_str(", ")?;
            }
            text.push_str(name)?;
            text.push_str(" = ")?;
            text.push_str(&uqa_sql::expr::quote_literal_with_control(value, &control)?)?;
        }
    }
    if let Some(elapsed) = physical.planning_elapsed.filter(|_| settings.summary) {
        let _ = write!(
            text,
            "\nPlanning Time: {:.3} ms",
            elapsed.as_secs_f64() * 1_000.0
        );
    }
    if let Some(analysis) = analysis {
        vector::append_text(&mut text, analysis, control)?;
        serialization::append_text(&mut text, settings, analysis)?;
        measurements::append_text(&mut text, analysis.root, settings.timing)?;
        let _ = write!(text, "\n  affected_rows={}", analysis.affected_rows);
        if settings.summary {
            let _ = write!(
                text,
                "\n  execution_time_ms={:.3}",
                analysis.elapsed.as_secs_f64() * 1_000.0
            );
        }
        executions::append_text(&mut text, &analysis.node_executions, settings.timing)?;
    }
    Ok(text.finish()?)
}

fn structured_plan<'a>(
    body: &UnifiedPlan,
    text: &'a str,
    settings: &uqa_sql::explain::ExplainSettings,
    analysis: Option<&'a ExplainAnalysis>,
    physical: &'a ExplainPhysicalPlan,
    control: ProductionControl<'_>,
) -> Result<properties::Property<'a>, SQLError> {
    use properties::{array, object, Object, Property as P};
    let node = match body {
        UnifiedPlan::Query(query) => match &query.root {
            RelationalPlan::QueryBlock(_) => "Select",
            RelationalPlan::SetOp { .. } => "SetOp",
            RelationalPlan::Values { .. } => "Values",
        },
        UnifiedPlan::Command(command) => command.name(),
    };
    let mut plan = Object::new(control);
    plan.insert("Node Type", P::Text(node))?;
    plan.insert("UQA Plan", P::Lines(text))?;
    if let Some(estimate) = physical.estimate.filter(|_| settings.costs) {
        estimates::fields(&mut plan, estimate)?;
    }
    if let Some(analysis) = analysis {
        measurements::properties(&mut plan, analysis.root, settings.timing)?;
        plan.insert("Affected Rows", P::Unsigned(analysis.affected_rows))?;
    }
    let mut payload = Object::new(control);
    payload.insert("Plan", plan.finish()?)?;
    if settings.costs && !physical.estimates.is_empty() {
        payload.insert(
            "Node Estimates",
            estimates::structured(&physical.estimates, control)?,
        )?;
    }
    if settings.settings {
        payload.insert(
            "Settings",
            object(
                control,
                physical
                    .settings
                    .iter()
                    .map(|(key, value)| (*key, P::Text(value))),
            )?,
        )?;
    }
    if let Some(elapsed) = physical.planning_elapsed.filter(|_| settings.summary) {
        payload.insert("Planning Time", P::Float(elapsed.as_secs_f64() * 1_000.0))?;
    }
    if let Some(analysis) = analysis {
        payload.insert(
            "Node Executions",
            executions::structured(&analysis.node_executions, settings.timing, control)?,
        )?;
        if let Some(serialization) = serialization::properties(settings, analysis, control)? {
            payload.insert("Serialization", serialization)?;
        }
        payload.insert(
            "Vector Searches",
            array(
                control,
                analysis
                    .vector_searches
                    .iter()
                    .map(|search| vector::record(search, control)),
            )?,
        )?;
        if settings.summary {
            payload.insert(
                "Execution Time",
                P::Float(analysis.elapsed.as_secs_f64() * 1_000.0),
            )?;
        }
    }
    if !physical.nodes.is_empty() {
        payload.insert(
            "Physical Plans",
            array(
                control,
                physical.nodes.iter().map(|node| Ok(P::Captured(node))),
            )?,
        )?;
    }
    payload.finish()
}

pub fn format_query_plan(plan: &QueryPlan) -> String {
    text::query(plan, ProductionControl::uncontrolled())
        .expect("ordinary plan formatting")
        .into_uncontrolled()
        .expect("ordinary plan owner")
}

pub fn format_select_plan(plan: &QueryBlockPlan) -> String {
    text::select(plan, ProductionControl::uncontrolled())
        .expect("ordinary query block formatting")
        .into_uncontrolled()
        .expect("ordinary plan owner")
}

pub use uqa_sql::semantics::{select_execution_stmt, should_defer_distinct_limit};

pub fn explain_int_expr(expr: &ScalarExpr) -> String {
    match expr {
        ScalarExpr::Literal(Value::Int(n)) => n.to_string(),
        _ => "<expr>".to_string(),
    }
}
