//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted liveness scratch preserves lazy initialization and local scalar-query namespaces.

use uqa_core::memory::{ProductionControl, ProductionMap};
use uqa_sql::{
    plan::{QueryBlockPlan, SourcePlan},
    SQLError, ScalarExpr,
};

pub(super) use uqa_sql::plan::subqueries::{
    prune_command_with_control as prune_command, prune_query_with_control as prune_query,
};

/// Retain dead branches for static result typing without evaluating their child plans.
pub(super) fn live_slots<'a, 'control>(
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    control: ProductionControl<'control>,
) -> Result<ProductionMap<'control, usize, ()>, SQLError> {
    live_slots_with_source(expressions, None, control)
}

pub(super) fn live_slots_with_source<'a, 'control>(
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    source: Option<&'a SourcePlan>,
    control: ProductionControl<'control>,
) -> Result<ProductionMap<'control, usize, ()>, SQLError> {
    control.check()?;
    let mut slots = ProductionMap::new(control);
    let mut visit =
        |expression: &ScalarExpr| referenced_slots(expression, &mut slots, control).map(|()| true);
    for expression in expressions {
        visit(expression)?;
    }
    if let Some(source) = source {
        source.try_visit_expressions(&mut visit)?;
    }
    Ok(slots)
}

pub(super) fn live_slots_for_block<'control>(
    block: &QueryBlockPlan,
    control: ProductionControl<'control>,
) -> Result<ProductionMap<'control, usize, ()>, SQLError> {
    control.check()?;
    let mut slots = ProductionMap::new(control);
    block.try_visit_expressions(&mut |expression| {
        referenced_slots(expression, &mut slots, control).map(|()| true)
    })?;
    Ok(slots)
}

fn referenced_slots(
    expression: &ScalarExpr,
    slots: &mut ProductionMap<'_, usize, ()>,
    control: ProductionControl<'_>,
) -> Result<(), SQLError> {
    uqa_sql::catalog::security::builtin_routines::initialization::visit_with_control(
        expression,
        &mut |node| {
            match node {
                ScalarExpr::ScalarSubquery(index)
                | ScalarExpr::Exists {
                    subquery: index, ..
                }
                | ScalarExpr::InSubquery {
                    subquery: index, ..
                } => {
                    slots.insert_copy(*index, ())?;
                }
                _ => {}
            }
            Ok(())
        },
        &control,
    )
}

/// Plan only subquery slots reached by a surviving expression, in original slot order. Unreached slots retain their analyzed result types.
pub(super) fn optimize_live(
    queries: &mut [super::QueryPlan],
    slots: &ProductionMap<'_, usize, ()>,
    config: &super::OptimizerConfig,
    aggregates: &dyn super::AggregateClassifier,
) -> Result<(), SQLError> {
    config.production.check()?;
    for (index, ()) in slots {
        config.production.check()?;
        if let Some(query) = queries.get_mut(*index) {
            super::optimize_query(query, config, aggregates)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
