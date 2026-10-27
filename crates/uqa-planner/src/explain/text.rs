//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native plan text uses the same admitted formatter for ordinary and controlled entry points.

use std::fmt::Write as _;
use uqa_core::memory::{Produced, ProductionControl, ProductionString};
use uqa_sql::{
    plan::{QueryBlockPlan, QueryPlan, RelationalPlan},
    SQLError, ScalarExpr,
};

pub(super) fn query(
    plan: &QueryPlan,
    control: ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    match &plan.root {
        RelationalPlan::QueryBlock(block) => select(block, control),
        RelationalPlan::Values { rows, .. } => {
            Ok(control.format(format_args!("Values\n  rows={}", rows.len()))?)
        }
        RelationalPlan::SetOp {
            kind,
            all,
            left,
            right,
            order_by,
            limit,
            offset,
            ..
        } => {
            let mut text = ProductionString::new(control);
            let _ = write!(text, "SetOp\n  kind={kind:?}\n  all={all}\n  left=(");
            child(&mut text, &query(left, control)?)?;
            text.push_str(")\n  right=(")?;
            child(&mut text, &query(right, control)?)?;
            let _ = write!(text, ")\n  order_by={}\n  limit=", order_by.len());
            integer(&mut text, limit.as_deref());
            text.push_str("\n  offset=")?;
            integer(&mut text, offset.as_deref());
            Ok(text.finish()?)
        }
    }
}

fn child(output: &mut ProductionString<'_>, text: &str) -> Result<(), SQLError> {
    for (index, line) in text.lines().enumerate() {
        if index != 0 {
            output.push_str("\n    ")?;
        }
        output.push_str(line)?;
    }
    Ok(())
}

fn integer(text: &mut ProductionString<'_>, expr: Option<&ScalarExpr>) {
    let _ = match expr {
        Some(ScalarExpr::Literal(uqa_core::Value::Int(value))) => write!(text, "{value}"),
        Some(_) => text.write_str("<expr>"),
        None => text.write_str("none"),
    };
}

pub(super) fn select(
    plan: &QueryBlockPlan,
    control: ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut text = ProductionString::new(control);
    text.push_str("Select")?;
    if !plan.projections.is_empty() {
        let _ = write!(text, "\n  projections={}", plan.projections.len());
    }
    if let Some(from) = &plan.from {
        let _ = write!(text, "\n  from={from:?}");
    }
    if plan.r#where.is_some() {
        text.push_str("\n  where=<expr>")?;
    }
    if !plan.group_by.is_empty() {
        let _ = write!(text, "\n  group_by={}", plan.group_by.len());
    }
    if !plan.grouping_sets.is_empty() {
        let _ = write!(text, "\n  grouping_sets={}", plan.grouping_sets.len());
    }
    if !plan.order_by.is_empty() {
        let _ = write!(text, "\n  order_by={}", plan.order_by.len());
    }
    if let Some(expr) = &plan.limit {
        text.push_str("\n  limit=")?;
        integer(&mut text, Some(expr));
    }
    if let Some(expr) = &plan.offset {
        text.push_str("\n  offset=")?;
        integer(&mut text, Some(expr));
    }
    if plan.distinct {
        text.push_str("\n  distinct=true")?;
    }
    if !plan.locking.is_empty() {
        let _ = write!(text, "\n  locking={}", plan.locking.len());
    }
    Ok(text.finish()?)
}
