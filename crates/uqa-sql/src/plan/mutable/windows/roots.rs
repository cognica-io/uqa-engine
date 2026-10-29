//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    scalar, MutableQueryPlan, MutableSourcePlan, ProductionControl, QueryScalarSlot,
    RelationalPlan, SQLError, ScalarWindowSpec, SourceChildSlot, SourcePlan, SourceScalarSlot,
};

pub(super) fn normalize(
    mut query: MutableQueryPlan,
    resolved: &[ScalarWindowSpec],
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    query = query.map_source(control, |source| source_roots(source, resolved, control))?;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!("local window roots belong to a query block")
    };
    let counts = (
        block.projections.len(),
        block.group_by.len(),
        block.grouping_sets.len(),
        block.order_by.len(),
        block.distinct_on.len(),
    );
    for index in 0..counts.0 {
        query = slot(query, QueryScalarSlot::Projection(index), resolved, control)?;
    }
    query = slot(query, QueryScalarSlot::Where, resolved, control)?;
    for index in 0..counts.1 {
        query = slot(query, QueryScalarSlot::GroupBy(index), resolved, control)?;
    }
    for set in 0..counts.2 {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for expression in 0..block.grouping_sets[set].len() {
            query = slot(
                query,
                QueryScalarSlot::GroupingSet { set, expression },
                resolved,
                control,
            )?;
        }
    }
    query = slot(query, QueryScalarSlot::Having, resolved, control)?;
    for index in 0..counts.3 {
        query = slot(query, QueryScalarSlot::OrderBy(index), resolved, control)?;
    }
    query = slot(query, QueryScalarSlot::Limit, resolved, control)?;
    query = slot(query, QueryScalarSlot::Offset, resolved, control)?;
    for index in 0..counts.4 {
        query = slot(query, QueryScalarSlot::DistinctOn(index), resolved, control)?;
    }
    Ok(query)
}
fn slot(
    query: MutableQueryPlan,
    slot: QueryScalarSlot,
    resolved: &[ScalarWindowSpec],
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    query.map_scalar(slot, control, |expression| {
        scalar(expression, resolved, control)
    })
}
fn source_roots(
    mut source: MutableSourcePlan,
    resolved: &[ScalarWindowSpec],
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    control.check()?;
    match &*source {
        SourcePlan::Join { .. } => {
            source = source.map_child(SourceChildSlot::Left, control, |source| {
                source_roots(source, resolved, control)
            })?;
            source = source.map_child(SourceChildSlot::Right, control, |source| {
                source_roots(source, resolved, control)
            })?;
            source = source.map_scalar(SourceScalarSlot::JoinOn, control, |expression| {
                scalar(expression, resolved, control)
            })?;
        }
        SourcePlan::Values { rows, .. } => {
            let count = rows.len();
            for row in 0..count {
                let SourcePlan::Values { rows, .. } = &*source else {
                    unreachable!()
                };
                for column in 0..rows[row].len() {
                    source = source.map_scalar(
                        SourceScalarSlot::Value { row, column },
                        control,
                        |expression| scalar(expression, resolved, control),
                    )?;
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            let count = args.len();
            for index in 0..count {
                source = source.map_scalar(
                    SourceScalarSlot::Argument(index),
                    control,
                    |expression| scalar(expression, resolved, control),
                )?;
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            let count = functions.len();
            for function in 0..count {
                let SourcePlan::FunctionGroup { functions, .. } = &*source else {
                    unreachable!()
                };
                for argument in 0..functions[function].args.len() {
                    source = source.map_scalar(
                        SourceScalarSlot::GroupArgument { function, argument },
                        control,
                        |expression| scalar(expression, resolved, control),
                    )?;
                }
            }
        }
        SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } | SourcePlan::Subquery { .. } => {}
    }
    Ok(source)
}
