//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Append-member pruning retains each child owner and complete positional descriptor.

use super::{observation, OutputPruningContext, RelationalPlan, SQLError};
use uqa_sql::plan::{MutableQueryPlan, QueryChildSlot};

pub(in crate::optimizer) fn prune(
    query: MutableQueryPlan,
    required: &dyn Fn(usize) -> bool,
    context: OutputPruningContext<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let Some(output) = &query.analyzed_output else {
        return Ok(query);
    };
    if !super::simple_union(&query, output.column_types()) {
        return Ok(query);
    }
    members(query, required, context)
}

fn members(
    mut query: MutableQueryPlan,
    required: &dyn Fn(usize) -> bool,
    context: OutputPruningContext<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let control = &context.production;
    let pulled_up =
        observation::pulled_up_member(&query, context.routines, context.volatility, control)?;
    match &query.root {
        RelationalPlan::SetOp { .. } => {
            query = query.map_query_child(QueryChildSlot::Left, control, |child| {
                members(child, required, context)
            })?;
            query.map_query_child(QueryChildSlot::Right, control, |child| {
                members(child, required, context)
            })
        }
        RelationalPlan::QueryBlock(_) if pulled_up == Some(false) => {
            super::super::derived_projection::owned::prune_positions(query, required, None, context)
        }
        _ => Ok(query),
    }
}
