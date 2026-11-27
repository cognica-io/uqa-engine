//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Propagate admitted constants from VALUES and source-free subqueries, preserving empty-input grouping.

mod map;
mod production;

#[cfg(test)]
mod tests;

use super::{scalar::ScalarProduction, OptimizerConfig};
use crate::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan};
use map::Constants;
use uqa_core::Value;
use uqa_sql::{
    plan::{MutableQueryPlan, OwnedScalarExpr},
    routines::declaration::RoutineTypeCatalog,
    semantics::volatility::VolatilityCatalog,
    ColumnType, SQLError, ScalarExpr,
};

pub(super) fn propagate_caller(
    query: &mut QueryPlan,
    types: Option<&dyn RoutineTypeCatalog>,
    views: Option<&dyn VolatilityCatalog>,
    config: &OptimizerConfig<'_>,
) -> Result<(), SQLError> {
    query
        .edit_uncontrolled(|query| propagate(query, types, views, ScalarProduction::caller(config)))
}

pub(super) fn propagate(
    mut query: MutableQueryPlan,
    types: Option<&dyn RoutineTypeCatalog>,
    views: Option<&dyn VolatilityCatalog>,
    production: ScalarProduction<'_, '_>,
) -> Result<MutableQueryPlan, SQLError> {
    let workspace = &production.config.production;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(query);
    };
    let Some(source) = &block.from else {
        return Ok(query);
    };
    let Some(constants) = Constants::from_source(source, types, views, workspace)? else {
        return Ok(query);
    };
    let qualifier = source
        .visible_qualifier()
        .map(|name| workspace.copy_text(name))
        .transpose()?;
    let count = block.projections.len();
    for index in 0..count {
        workspace.check()?;
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        let projection = &block.projections[index];
        if projection.alias.is_none() {
            if let ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } =
                &projection.expr
            {
                let alias = production.copy_text(column)?;
                query = query.replace_projection_alias(index, Some(alias), &production.storage)?;
            }
        }
    }
    query.map_local_scalars(&production.storage, workspace, |expression| {
        substitute(
            expression,
            qualifier.as_deref().map(String::as_str),
            &constants,
            production,
        )
    })
}

fn substitute(
    expression: OwnedScalarExpr,
    qualifier: Option<&str>,
    constants: &Constants,
    production: ScalarProduction<'_, '_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let workspace = &production.config.production;
    workspace.check()?;
    let expression = expression.map_children(&production.storage, |_, child| {
        substitute(child, qualifier, constants, production)
    })?;
    let column = match &*expression {
        ScalarExpr::Column(column) => Some(column.as_str()),
        ScalarExpr::QualifiedColumn {
            qualifier: requested,
            column,
        } if qualifier == Some(requested.as_str()) => Some(column.as_str()),
        _ => None,
    };
    if let Some(value) = column.and_then(|column| constants.get(column)) {
        let value = value.copy_for_rewrite(workspace)?;
        let value =
            super::scalar::constants::metadata::retain_computed_integer(value, None, workspace)?;
        return production.adopt(value);
    }
    Ok(expression)
}

/// Retain one constant key so grouping an empty input still produces no group.
pub(super) fn simplify_group(
    query: MutableQueryPlan,
    types: Option<&dyn RoutineTypeCatalog>,
    production: ScalarProduction<'_, '_>,
) -> Result<MutableQueryPlan, SQLError> {
    let workspace = &production.config.production;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(query);
    };
    if !production::constant_group(block, types, workspace)? {
        return Ok(query);
    }
    let key = OwnedScalarExpr::literal(
        workspace.finish(Value::Bool(true), workspace.empty_reservation())?,
        workspace,
    )?;
    query.replace_group_by_with_workspace(std::iter::once(key), &production.storage, workspace)
}

#[cfg(test)]
fn constants_from_values<'a>(
    values: impl IntoIterator<Item = &'a ScalarExpr>,
    column_name: impl Fn(usize) -> String,
    types: Option<&dyn RoutineTypeCatalog>,
) -> Constants {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    let mut constants = Constants::new(&control);
    production::observe_values(
        values,
        |index| Ok(control.finish(column_name(index), None)?),
        types,
        &control,
        &mut |name, value| constants.insert(name, value, &control),
    )
    .expect("ordinary source constants");
    constants
}

#[cfg(test)]
fn constant_literal(expression: &ScalarExpr, types: Option<&dyn RoutineTypeCatalog>) -> bool {
    production::constant_literal(
        expression,
        types,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary source constant eligibility")
}
