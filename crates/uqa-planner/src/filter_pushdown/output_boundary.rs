//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicates may remove whole groups or partitions without changing their retained outputs.

use uqa_sql::{
    plan::{ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan},
    semantics::aggregates::exprs_match_with_control,
    SQLError, ScalarExpr,
};

pub(super) fn preserves_set_types_with_control(
    plan: &QueryPlan,
    qualifier: &str,
    predicate: &ScalarExpr,
    aliases: Option<&[String]>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if !matches!(plan.root, RelationalPlan::SetOp { .. }) {
        return Ok(true);
    }
    let Some(output) = plan.analyzed_output.as_ref() else {
        return Ok(false);
    };
    let mut positions = uqa_core::memory::ProductionMap::new(*control);
    predicate.try_visit(&mut |node| {
        control.check()?;
        let column = match node {
            ScalarExpr::Column(column) => column,
            ScalarExpr::QualifiedColumn {
                qualifier: owner,
                column,
                ..
            } if owner == qualifier => column,
            _ => return Ok::<_, SQLError>(true),
        };
        if let Some(index) = output
            .columns()
            .iter()
            .enumerate()
            .position(|(index, name)| {
                aliases.and_then(|names| names.get(index)).unwrap_or(name) == column
            })
        {
            positions.insert_copy(index, ())?;
        }
        Ok(true)
    })?;
    Ok(!positions.is_empty() && set_member_types_match(plan, output, &positions, control)?)
}

fn set_member_types_match(
    plan: &QueryPlan,
    output: &uqa_sql::RowSchema,
    positions: &uqa_core::memory::ProductionMap<'_, usize, ()>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if let RelationalPlan::SetOp { left, right, .. } = &plan.root {
        return Ok(set_member_types_match(left, output, positions, control)?
            && set_member_types_match(right, output, positions, control)?);
    }
    let Some(member) = plan.analyzed_output.as_ref() else {
        return Ok(false);
    };
    Ok(positions.iter().all(|(index, ())| {
        member
            .column_type(*index)
            .zip(output.column_type(*index))
            .is_some_and(|(input, output)| {
                uqa_sql::catalog::type_metadata::pg_type_oid(input)
                    == uqa_sql::catalog::type_metadata::pg_type_oid(output)
            })
    }))
}

pub(super) fn preserves_groups_with_control(
    block: &QueryBlockPlan,
    expression: &ScalarExpr,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    fn any_match<'a>(
        keys: impl IntoIterator<Item = &'a ScalarExpr>,
        expression: &ScalarExpr,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        for key in keys {
            control.check()?;
            if exprs_match_with_control(key, expression, control)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    control.check()?;
    if (matches!(block.compute, ComputePlan::Aggregate) || !block.group_by.is_empty())
        && !any_match(&block.group_by, expression, control)?
    {
        return Ok(false);
    }
    if !block.distinct_on.is_empty()
        && !any_match(
            block.distinct_on.iter().map(|key| match key {
                ScalarExpr::Position(index) => block
                    .projections
                    .get(*index)
                    .map(|p| &p.expr)
                    .unwrap_or(key),
                _ => key,
            }),
            expression,
            control,
        )?
    {
        return Ok(false);
    }
    if !matches!(block.compute, ComputePlan::Window) {
        return Ok(true);
    }
    let mut seen = false;
    let mut preserves = true;
    for root in block
        .projections
        .iter()
        .map(|projection| &projection.expr)
        .chain(block.order_by.iter().map(|order| &order.expr))
    {
        root.try_visit(&mut |node| {
            control.check()?;
            if let ScalarExpr::WindowCall { spec, .. } = node {
                seen = true;
                if preserves {
                    preserves = any_match(&spec.partition_by, expression, control)?;
                }
            }
            Ok::<_, SQLError>(true)
        })?;
    }
    Ok(seen && preserves)
}
