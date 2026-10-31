//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Discard unused mutation outputs through their scalar owners while retaining effects and row shape.

use crate::scratch::Buffer;
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{
    plan::{ComputePlan, MutableQueryPlan, QueryScalarSlot, RelationalPlan},
    routines::RoutineResolution,
    semantics::{
        sets::static_setness::expression_may_return_set_statically_with_control,
        volatility::{scalar_in_scope_contains_volatile_function_with_control, VolatilityCatalog},
    },
    SQLError, ScalarExpr,
};

pub fn prune_unused_query_outputs_owned(
    volatility: &dyn VolatilityCatalog,
    routines: &dyn RoutineResolution,
    query: MutableQueryPlan,
    required: &dyn Fn(usize) -> bool,
    expected_width: usize,
    inherited_ctes: &dyn Fn(&str) -> bool,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    if !matches!(query.root, RelationalPlan::QueryBlock(_)) {
        return Ok(query);
    }
    let schema = query.analyzed_output.clone();
    let (mut query, expanded) =
        crate::optimizer::expand_local_stars_owned(query, schema.as_ref(), control)?;
    if !expanded {
        return Ok(query);
    }
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!("expanded query block");
    };
    if !matches!(block.compute, ComputePlan::Project)
        || block.distinct
        || !block.distinct_on.is_empty()
        || !block.order_by.is_empty()
        || block.projections.len() != expected_width
    {
        return Ok(query);
    }
    let visible =
        |name: &str| query.ctes.iter().any(|cte| cte.name == name) || inherited_ctes(name);
    let mut discarded = Buffer::new(*control);
    for (position, projection) in block.projections.iter().enumerate() {
        control.check()?;
        if !required(position)
            && !expression_may_return_set_statically_with_control(
                routines,
                &projection.expr,
                control,
            )?
            && !scalar_in_scope_contains_volatile_function_with_control(
                volatility,
                &projection.expr,
                &block.subqueries,
                &visible,
                control,
            )?
        {
            discarded.push(position)?;
        }
    }
    for position in discarded {
        query = query.map_scalar(QueryScalarSlot::Projection(position), control, |old| {
            drop(old);
            Ok(ScalarExpr::Literal(Value::Null).copy_for_rewrite(control)?)
        })?;
    }
    Ok(query)
}
