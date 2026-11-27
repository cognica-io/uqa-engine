//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unused outputs retain positional metadata while retiring their scalar owners immediately.

use super::{OutputPruningContext, RelationalPlan, RowSchema, SQLError, ScalarExpr};
use crate::scratch::Buffer;
use uqa_core::memory::ProductionControl;
use uqa_sql::plan::{MutableQueryPlan, OwnedScalarExpr};

mod stars;
pub(crate) use stars::{expand as expand_stars, expand_caller as expand_local_stars};
#[cfg(test)]
mod tests;

pub fn prune_outputs(
    query: MutableQueryPlan,
    aliases: &[String],
    required: &dyn Fn(&str) -> bool,
    schema: Option<&RowSchema>,
    context: OutputPruningContext<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    super::prune_named(query, aliases, required, schema, context)
}

#[cfg(test)]
use super::target::prune_positions;

pub(in crate::optimizer) fn prune_plan_positions(
    query: MutableQueryPlan,
    required: &dyn Fn(usize) -> bool,
    schema: Option<&RowSchema>,
    context: OutputPruningContext<'_>,
    storage: ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    if storage.budget().is_none() && context.production.budget().is_some() {
        let mut syntax = query
            .finish(&storage)?
            .into_uncontrolled()
            .expect("caller-owned projections");
        super::target::prune_positions(&mut syntax, required, schema, context)?;
        Ok(MutableQueryPlan::uncontrolled(syntax))
    } else {
        super::target::prune_positions(query, required, schema, context)
    }
}

pub(in crate::optimizer) fn prune(
    query: MutableQueryPlan,
    context: OutputPruningContext<'_>,
    storage: ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(query);
    };
    let Some(demanded) = super::demand::consumer_outputs(block, context.production)? else {
        return Ok(query);
    };
    query.map_source(&storage, |source| {
        source.map_query(&storage, |body| {
            prune_plan_positions(
                body,
                &|position| demanded.get(position).copied().unwrap_or(true),
                None,
                context,
                storage,
            )
        })
    })
}
