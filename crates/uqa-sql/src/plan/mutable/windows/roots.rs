//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    checkpoint, Input, MutableQueryPlan, MutableSourcePlan, ProductionControl, QueryScalarSlot,
    RelationalPlan, SQLError, SourceChildSlot, SourcePlan, SourceScalarSlot, SourceScope, Windows,
};
use crate::plan::scalar::OwnedScalarExpr;

pub(super) async fn normalize<I: Input>(
    query: &mut MutableQueryPlan,
    resolved: Windows,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    if let Some(mut slot) = query.source_slot()? {
        let value = input
            .source(slot.take(), SourceScope::Roots(resolved.clone()), control)?
            .await?;
        slot.restore(value);
    }
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
        slot(
            query,
            QueryScalarSlot::Projection(index),
            &resolved,
            control,
            input,
        )
        .await?;
    }
    slot(query, QueryScalarSlot::Where, &resolved, control, input).await?;
    for index in 0..counts.1 {
        slot(
            query,
            QueryScalarSlot::GroupBy(index),
            &resolved,
            control,
            input,
        )
        .await?;
    }
    for set in 0..counts.2 {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for expression in 0..block.grouping_sets[set].len() {
            slot(
                query,
                QueryScalarSlot::GroupingSet { set, expression },
                &resolved,
                control,
                input,
            )
            .await?;
        }
    }
    slot(query, QueryScalarSlot::Having, &resolved, control, input).await?;
    for index in 0..counts.3 {
        slot(
            query,
            QueryScalarSlot::OrderBy(index),
            &resolved,
            control,
            input,
        )
        .await?;
    }
    slot(query, QueryScalarSlot::Limit, &resolved, control, input).await?;
    slot(query, QueryScalarSlot::Offset, &resolved, control, input).await?;
    for index in 0..counts.4 {
        slot(
            query,
            QueryScalarSlot::DistinctOn(index),
            &resolved,
            control,
            input,
        )
        .await?;
    }
    Ok(())
}
async fn slot<I: Input>(
    query: &mut MutableQueryPlan,
    selected: QueryScalarSlot,
    resolved: &Windows,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    if let Some(mut slot) = query.scalar_slot(selected) {
        let value = scalar(slot.take(), resolved, control, input).await?;
        slot.restore(value);
    }
    Ok(())
}

async fn scalar<I: Input>(
    expression: OwnedScalarExpr,
    resolved: &Windows,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<OwnedScalarExpr, SQLError> {
    input.scalar(expression, resolved.clone(), control)?.await
}

async fn source_slot<I: Input>(
    source: &mut MutableSourcePlan,
    selected: SourceScalarSlot,
    resolved: &Windows,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    if let Some(mut slot) = source.scalar_slot(selected) {
        let value = scalar(slot.take(), resolved, control, input).await?;
        slot.restore(value);
    }
    Ok(())
}

pub(super) async fn source<I: Input>(
    source: &mut MutableSourcePlan,
    resolved: Windows,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<(), SQLError> {
    checkpoint(control, input).await?;
    match &**source {
        SourcePlan::Join { .. } => {
            for child in [SourceChildSlot::Left, SourceChildSlot::Right] {
                let mut slot = source.child_slot(child);
                let value = input
                    .source(slot.take(), SourceScope::Roots(resolved.clone()), control)?
                    .await?;
                slot.restore(value);
            }
            source_slot(source, SourceScalarSlot::JoinOn, &resolved, control, input).await?;
        }
        SourcePlan::Values { rows, .. } => {
            let count = rows.len();
            for row in 0..count {
                let SourcePlan::Values { rows, .. } = &**source else {
                    unreachable!()
                };
                for column in 0..rows[row].len() {
                    source_slot(
                        source,
                        SourceScalarSlot::Value { row, column },
                        &resolved,
                        control,
                        input,
                    )
                    .await?;
                }
            }
        }
        SourcePlan::Function { args, .. } => {
            let count = args.len();
            for index in 0..count {
                source_slot(
                    source,
                    SourceScalarSlot::Argument(index),
                    &resolved,
                    control,
                    input,
                )
                .await?;
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            let count = functions.len();
            for function in 0..count {
                let SourcePlan::FunctionGroup { functions, .. } = &**source else {
                    unreachable!()
                };
                for argument in 0..functions[function].args.len() {
                    source_slot(
                        source,
                        SourceScalarSlot::GroupArgument { function, argument },
                        &resolved,
                        control,
                        input,
                    )
                    .await?;
                }
            }
        }
        SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } | SourcePlan::Subquery { .. } => {}
    }
    Ok(())
}
