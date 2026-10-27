//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Render costs of native query/source boundaries separately from execution observations.

use super::properties::{array, Object, Property};
use std::fmt::Write as _;
use uqa_core::memory::{ProductionControl, ProductionString, ProductionVec};
use uqa_sql::{
    result::explain::{ExplainNodeEstimate, ExplainPlanEstimate},
    SQLError,
};

pub(super) fn fields(
    output: &mut Object<'_, '_>,
    estimate: ExplainPlanEstimate,
) -> Result<(), SQLError> {
    output.insert("Startup Cost", Property::Float(estimate.startup))?;
    output.insert("Total Cost", Property::Float(estimate.total))?;
    output.insert("Plan Rows", Property::Float(estimate.rows.round()))?;
    if let Some(width) = estimate.width {
        output.insert("Plan Width", Property::Unsigned(width))?;
    }
    Ok(())
}

pub(super) fn structured<'a>(
    nodes: &'a [ExplainNodeEstimate],
    control: ProductionControl<'_>,
) -> Result<Property<'a>, SQLError> {
    array(
        control,
        nodes.iter().enumerate().map(|(index, node)| {
            let mut output = Object::new(control);
            output.insert("Node ID", Property::Unsigned(index_number(index)?))?;
            output.insert("Node Type", Property::Text(node.kind))?;
            if let Some(parent) = node.parent {
                if parent >= index {
                    return Err(invalid_parent());
                }
                output.insert("Parent Node ID", Property::Unsigned(index_number(parent)?))?;
            }
            if let Some(relation) = &node.relation {
                output.insert("Relation Name", Property::Text(relation))?;
            }
            fields(&mut output, node.estimate)?;
            output.finish()
        }),
    )
}

pub(super) fn append_text(
    text: &mut ProductionString<'_>,
    nodes: &[ExplainNodeEstimate],
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    if nodes.len() <= 1 {
        return Ok(());
    }
    text.push_str("\nNode Estimates:")?;
    let mut depths = ProductionVec::new(control);
    for (index, node) in nodes.iter().enumerate() {
        let depth = node.parent.map_or(Ok(0_usize), |parent| {
            depths
                .get(parent)
                .copied()
                .filter(|_| parent < index)
                .and_then(|depth: usize| depth.checked_add(1))
                .ok_or_else(invalid_parent)
        })?;
        depths.push_copy(depth)?;
        text.push_str("\n  ")?;
        for _ in 0..depth {
            text.push_str("  ")?;
        }
        text.push_str(node.kind)?;
        if let Some(relation) = &node.relation {
            text.push_str(" ")?;
            text.push_str(&uqa_sql::expr::quote_ident_with_control(
                relation, &control,
            )?)?;
        }
        let estimate = node.estimate;
        let _ = write!(
            text,
            " cost={:.2}..{:.2} rows={:.0}",
            estimate.startup, estimate.total, estimate.rows
        );
        if let Some(width) = estimate.width {
            let _ = write!(text, " width={width}");
        }
    }
    Ok(())
}

fn index_number(index: usize) -> Result<u64, SQLError> {
    u64::try_from(index).map_err(|_| SQLError::Internal("EXPLAIN node index exceeds u64".into()))
}

fn invalid_parent() -> SQLError {
    SQLError::Internal("EXPLAIN estimate parent must precede its child".into())
}

#[cfg(test)]
mod tests;
