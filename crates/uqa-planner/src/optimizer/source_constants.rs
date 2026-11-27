//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Propagate admitted constants from VALUES and source-free subqueries, preserving empty-input grouping.

mod map;
pub(super) mod owned;

use map::Constants;
use uqa_core::memory::ProductionControl;

use crate::{QueryBlockPlan, RelationalPlan, SourcePlan};
use uqa_sql::routines::declaration::RoutineTypeCatalog;
use uqa_sql::{ColumnType, SQLError, ScalarExpr};

mod production;

#[cfg(test)]
mod tests;

pub(super) fn propagate_source_constants(
    block: &mut QueryBlockPlan,
    types: Option<&dyn RoutineTypeCatalog>,
    views: Option<&dyn uqa_sql::semantics::volatility::VolatilityCatalog>,
) -> Result<(), SQLError> {
    let Some(source) = &block.from else {
        return Ok(());
    };
    let control = ProductionControl::uncontrolled();
    let Some(constants) = Constants::from_source(source, types, views, &control)? else {
        return Ok(());
    };
    let qualifier = source.visible_qualifier().map(str::to_string);
    let mut replace = |expression: &mut ScalarExpr| {
        let column = match expression {
            ScalarExpr::Column(column) => Some(column.as_str()),
            ScalarExpr::QualifiedColumn {
                qualifier: requested,
                column,
            } if qualifier.as_deref() == Some(requested.as_str()) => Some(column.as_str()),
            _ => None,
        };
        if let Some(value) = column.and_then(|column| constants.get(column)) {
            *expression = super::retain_computed_integer((**value).clone(), None);
        }
    };
    let rewrite = crate::unified_plan::rewrite_scalar_expression;
    for projection in &mut block.projections {
        if projection.alias.is_none() {
            if let ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } =
                &projection.expr
            {
                projection.alias = Some(column.to_string());
            }
        }
        rewrite(&mut projection.expr, &mut replace);
    }
    for expression in block
        .r#where
        .iter_mut()
        .chain(&mut block.group_by)
        .chain(block.grouping_sets.iter_mut().flatten())
        .chain(block.having.iter_mut())
        .chain(block.order_by.iter_mut().map(|order| &mut order.expr))
        .chain(block.limit.iter_mut())
        .chain(block.offset.iter_mut())
        .chain(&mut block.distinct_on)
        .chain(
            block
                .windows
                .iter_mut()
                .flat_map(|window| window.spec.expressions_mut()),
        )
    {
        rewrite(expression, &mut replace);
    }
    Ok(())
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

/// An admitted constant key defines a single equivalence class without invoking its type's equality or hash functions. Retain a key so an empty input still produces no group.
pub(super) fn simplify_constant_group(
    block: &mut QueryBlockPlan,
    types: Option<&dyn RoutineTypeCatalog>,
) {
    if production::constant_group(
        block,
        types,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary constant grouping")
    {
        block.group_by = vec![ScalarExpr::Literal(uqa_core::Value::Bool(true))];
    }
}
